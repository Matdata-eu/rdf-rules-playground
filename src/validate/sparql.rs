//! SHACL-SPARQL: SPARQL-based constraints (`sh:sparql`) and SPARQL-based
//! constraint components, evaluated with Oxigraph's SPARQL engine.

use super::vocab::sh;
use super::{Engine, Res, ResultPath, Shape, as_subject, is_true, object, objects};
use anyhow::{Context, Result, bail};
use oxrdf::vocab::{rdf, rdfs};
use oxrdf::{
    Dataset, Graph, GraphNameRef, Literal, NamedNode, NamedNodeRef, QuadRef, Term, Variable,
};
use regex::Regex;
use spareval::{QueryEvaluationError, QueryEvaluator, QueryResults};
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{NamedNodePattern, TermPattern};
use spargebra::{Query, SparqlParser};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Runs SHACL's SPARQL queries against the data graph.
pub(crate) struct SparqlRunner<'a> {
    data: &'a Graph,
    shapes: &'a Graph,
    /// The name `$shapesGraph` is bound to; the shapes are queryable as
    /// this named graph.
    pub shapes_graph: Term,
    /// Built from the data and shapes graphs the first time a query runs.
    dataset: OnceCell<Dataset>,
    queries: RefCell<HashMap<(Term, Term), Rc<Query>>>,
}

enum Outcome {
    Rows(Vec<Vec<(String, Term)>>),
    Bool(bool),
}

impl<'a> SparqlRunner<'a> {
    pub fn new(data: &'a Graph, shapes: &'a Graph) -> Self {
        Self {
            data,
            shapes,
            shapes_graph: NamedNode::new_unchecked("urn:x-shacl-report:shapes-graph").into(),
            dataset: OnceCell::new(),
            queries: RefCell::default(),
        }
    }

    fn dataset(&self) -> &Dataset {
        self.dataset.get_or_init(|| {
            let mut ds = Dataset::new();
            for t in self.data.iter() {
                ds.insert(QuadRef::new(
                    t.subject,
                    t.predicate,
                    t.object,
                    GraphNameRef::DefaultGraph,
                ));
            }
            let Term::NamedNode(name) = &self.shapes_graph else {
                unreachable!()
            };
            for t in self.shapes.iter() {
                ds.insert(QuadRef::new(t.subject, t.predicate, t.object, name));
            }
            ds
        })
    }

    /// Parse (once per shape) the query of `node`, substituting `$PATH` for
    /// property shapes.
    fn query(
        &self,
        node: &Term,
        shape: &Shape,
        text: &str,
        prefixes: &[(String, String)],
        bound: &[&str],
    ) -> Result<Rc<Query>> {
        let key = (node.clone(), shape.id.clone());
        if let Some(q) = self.queries.borrow().get(&key) {
            return Ok(q.clone());
        }
        let text = match &shape.path {
            Some(p) => text.replace("$PATH", &p.to_sparql()),
            None => text.to_string(),
        };
        let mut parser = SparqlParser::new();
        for (name, iri) in prefixes {
            parser = parser
                .with_prefix(name, iri)
                .with_context(|| format!("invalid namespace for prefix {name}:"))?;
        }
        let mut q = parser
            .parse_query(&text)
            .with_context(|| format!("invalid SPARQL query in {node}"))?;
        // The pre-bound variables this query actually mentions.
        let used: Vec<&str> = bound
            .iter()
            .copied()
            .filter(|n| {
                Regex::new(&format!(r"[?$]{}\b", regex::escape(n)))
                    .is_ok_and(|re| re.is_match(&text))
            })
            .collect();
        match &mut q {
            Query::Select { pattern, .. } => {
                check_restrictions(pattern, &used, true)?;
                // Pre-bound variables must be projected for the evaluator to
                // accept them; SHACL allows a SELECT that does not project them.
                project(pattern, bound);
            }
            Query::Ask { pattern, .. } => check_restrictions(pattern, &used, true)?,
            _ => bail!("SHACL queries must be SELECT or ASK queries"),
        }
        let q = Rc::new(q);
        self.queries.borrow_mut().insert(key, q.clone());
        Ok(q)
    }

    fn run(&self, query: &Query, bindings: &[(&str, &Term)]) -> Result<Outcome> {
        // IRIs and literals are written into the query as constants, so the
        // optimizer can plan around them. Blank nodes can only be substituted
        // by the evaluator, and the optimizer would treat those variables as
        // unbound where the pattern does not bind them (e.g. in a FILTER).
        let evaluator = if bindings
            .iter()
            .any(|(_, t)| matches!(t, Term::BlankNode(_)))
        {
            QueryEvaluator::new().without_optimizations()
        } else {
            QueryEvaluator::new()
        };
        let constants: HashMap<&str, Expression> = bindings
            .iter()
            .filter_map(|(n, t)| match t {
                Term::NamedNode(n2) => Some((*n, Expression::NamedNode(n2.clone()))),
                Term::Literal(l) => Some((*n, Expression::Literal(l.clone()))),
                _ => None,
            })
            .collect();
        let mut query = query.clone();
        match &mut query {
            Query::Select { pattern, .. } | Query::Ask { pattern, .. } => {
                substitute_pattern(pattern, &constants)
            }
            _ => {}
        }
        let query = &query;
        let mut bindings = bindings.to_vec();
        let results = loop {
            let mut prepared = evaluator.prepare(query);
            for (name, term) in &bindings {
                prepared =
                    prepared.substitute_variable(Variable::new_unchecked(*name), (*term).clone());
            }
            match prepared.execute(self.dataset()) {
                // A variable the query never uses: pre-binding it changes nothing.
                Err(QueryEvaluationError::NotExistingSubstitutedVariable(v)) => {
                    let before = bindings.len();
                    bindings.retain(|(n, _)| *n != v.as_str());
                    if bindings.len() == before {
                        bail!("cannot pre-bind ?{}", v.as_str());
                    }
                }
                other => break other?,
            }
        };
        match results {
            QueryResults::Boolean(b) => Ok(Outcome::Bool(b)),
            QueryResults::Solutions(rows) => {
                let mut out = Vec::new();
                for row in rows {
                    let row = row?;
                    out.push(
                        row.iter()
                            .map(|(v, t)| (v.as_str().to_string(), t.clone()))
                            .collect(),
                    );
                }
                Ok(Outcome::Rows(out))
            }
            QueryResults::Graph(_) => bail!("SHACL queries must be SELECT or ASK queries"),
        }
    }
}

/// Add `names` to the projection of a SELECT query's pattern.
pub(crate) fn project(pattern: &mut GraphPattern, names: &[&str]) {
    match pattern {
        GraphPattern::Project { variables, .. } => {
            for name in names {
                let v = Variable::new_unchecked(*name);
                if !variables.contains(&v) {
                    variables.push(v);
                }
            }
        }
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::OrderBy { inner, .. } => project(inner, names),
        _ => {}
    }
}

/// SHACL 5.3.1: queries with pre-bound variables must not use MINUS,
/// VALUES, SERVICE or nested SELECTs, nor assign a pre-bound variable.
fn check_restrictions(p: &GraphPattern, bound: &[&str], top: bool) -> Result<()> {
    let inner = |p: &GraphPattern| check_restrictions(p, bound, false);
    match p {
        GraphPattern::Minus { .. } => bail!("SHACL does not allow MINUS in these queries"),
        GraphPattern::Values { .. } => bail!("SHACL does not allow VALUES in these queries"),
        GraphPattern::Service { .. } => bail!("SHACL does not allow SERVICE in these queries"),
        GraphPattern::Project {
            inner: i,
            variables,
        } => {
            // A subquery must return every pre-bound variable the query uses.
            if !top
                && let Some(v) = bound
                    .iter()
                    .find(|b| !variables.iter().any(|v| v.as_str() == **b))
            {
                bail!("SHACL requires nested SELECT queries to return ${v}");
            }
            inner(i)
        }
        GraphPattern::Distinct { inner: i }
        | GraphPattern::Reduced { inner: i }
        | GraphPattern::Slice { inner: i, .. } => check_restrictions(i, bound, top),
        GraphPattern::OrderBy {
            inner: i,
            expression,
        } => {
            for e in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = e;
                check_expr(e, bound)?;
            }
            check_restrictions(i, bound, top)
        }
        GraphPattern::Extend {
            inner: i,
            variable,
            expression,
        } => {
            if bound.contains(&variable.as_str()) {
                bail!(
                    "SHACL does not allow assigning the pre-bound ?{}",
                    variable.as_str()
                );
            }
            check_expr(expression, bound)?;
            inner(i)
        }
        GraphPattern::Filter { expr, inner: i } => {
            check_expr(expr, bound)?;
            inner(i)
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            if let Some(e) = expression {
                check_expr(e, bound)?;
            }
            inner(left)?;
            inner(right)
        }
        GraphPattern::Join { left, right } | GraphPattern::Union { left, right } => {
            inner(left)?;
            inner(right)
        }
        GraphPattern::Graph { inner: i, .. } | GraphPattern::Group { inner: i, .. } => inner(i),
        _ => Ok(()),
    }
}

fn check_expr(e: &Expression, bound: &[&str]) -> Result<()> {
    let mut result = Ok(());
    visit_expr(e, &mut |e| {
        if let Expression::Exists(p) = e
            && result.is_ok()
        {
            result = check_restrictions(p, bound, false);
        }
    });
    result
}

fn visit_expr(e: &Expression, f: &mut dyn FnMut(&Expression)) {
    f(e);
    match e {
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            visit_expr(a, f);
            visit_expr(b, f);
        }
        Expression::In(a, items) => {
            visit_expr(a, f);
            items.iter().for_each(|i| visit_expr(i, f));
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            visit_expr(a, f)
        }
        Expression::If(a, b, c) => {
            visit_expr(a, f);
            visit_expr(b, f);
            visit_expr(c, f);
        }
        Expression::Coalesce(items) | Expression::FunctionCall(_, items) => {
            items.iter().for_each(|i| visit_expr(i, f))
        }
        _ => {}
    }
}

/// Replace a pre-bound variable in a triple pattern position by its value,
/// so the evaluator looks it up directly instead of scanning.
pub(crate) fn substitute_term(t: &mut TermPattern, b: &HashMap<&str, Expression>) {
    if let TermPattern::Variable(v) = t {
        match b.get(v.as_str()) {
            Some(Expression::NamedNode(n)) => *t = TermPattern::NamedNode(n.clone()),
            Some(Expression::Literal(l)) => *t = TermPattern::Literal(l.clone()),
            _ => {}
        }
    }
}

pub(crate) fn substitute_named(t: &mut NamedNodePattern, b: &HashMap<&str, Expression>) {
    if let NamedNodePattern::Variable(v) = t
        && let Some(Expression::NamedNode(n)) = b.get(v.as_str())
    {
        *t = NamedNodePattern::NamedNode(n.clone());
    }
}

/// Replace pre-bound variables (IRIs and literals) by their values in
/// triple patterns and in every expression (FILTER, BIND, ORDER BY,
/// aggregates), including inside EXISTS. Blank nodes are left to the
/// evaluator's own substitution.
pub(crate) fn substitute_pattern(p: &mut GraphPattern, b: &HashMap<&str, Expression>) {
    match p {
        GraphPattern::Bgp { patterns } => {
            for t in patterns {
                substitute_term(&mut t.subject, b);
                substitute_named(&mut t.predicate, b);
                substitute_term(&mut t.object, b);
            }
        }
        GraphPattern::Path {
            subject, object, ..
        } => {
            substitute_term(subject, b);
            substitute_term(object, b);
        }
        GraphPattern::Graph { name, inner } => {
            substitute_named(name, b);
            substitute_pattern(inner, b);
        }
        GraphPattern::Filter { expr, inner } => {
            substitute_expr(expr, b);
            substitute_pattern(inner, b);
        }
        GraphPattern::LeftJoin {
            left,
            right,
            expression,
        } => {
            if let Some(e) = expression {
                substitute_expr(e, b);
            }
            substitute_pattern(left, b);
            substitute_pattern(right, b);
        }
        GraphPattern::Extend {
            inner, expression, ..
        } => {
            substitute_expr(expression, b);
            substitute_pattern(inner, b);
        }
        GraphPattern::OrderBy { inner, expression } => {
            for e in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = e;
                substitute_expr(e, b);
            }
            substitute_pattern(inner, b);
        }
        GraphPattern::Group {
            inner, aggregates, ..
        } => {
            for (_, a) in aggregates {
                if let AggregateExpression::FunctionCall { expr, .. } = a {
                    substitute_expr(expr, b);
                }
            }
            substitute_pattern(inner, b);
        }
        GraphPattern::Join { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            substitute_pattern(left, b);
            substitute_pattern(right, b);
        }
        GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. }
        | GraphPattern::Service { inner, .. } => substitute_pattern(inner, b),
        _ => {}
    }
}

fn substitute_expr(e: &mut Expression, b: &HashMap<&str, Expression>) {
    match e {
        Expression::Variable(v) => {
            if let Some(c) = b.get(v.as_str()) {
                *e = c.clone();
            }
        }
        Expression::Bound(v) => {
            if b.contains_key(v.as_str()) {
                *e = Expression::Literal(Literal::from(true));
            }
        }
        Expression::Exists(p) => substitute_pattern(p, b),
        Expression::Or(a, c)
        | Expression::And(a, c)
        | Expression::Equal(a, c)
        | Expression::SameTerm(a, c)
        | Expression::Greater(a, c)
        | Expression::GreaterOrEqual(a, c)
        | Expression::Less(a, c)
        | Expression::LessOrEqual(a, c)
        | Expression::Add(a, c)
        | Expression::Subtract(a, c)
        | Expression::Multiply(a, c)
        | Expression::Divide(a, c) => {
            substitute_expr(a, b);
            substitute_expr(c, b);
        }
        Expression::In(a, items) => {
            substitute_expr(a, b);
            items.iter_mut().for_each(|i| substitute_expr(i, b));
        }
        Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
            substitute_expr(a, b)
        }
        Expression::If(a, c, d) => {
            substitute_expr(a, b);
            substitute_expr(c, b);
            substitute_expr(d, b);
        }
        Expression::Coalesce(items) | Expression::FunctionCall(_, items) => {
            items.iter_mut().for_each(|i| substitute_expr(i, b))
        }
        _ => {}
    }
}

fn text_of(g: &Graph, node: &Term, p: oxrdf::NamedNodeRef<'_>) -> Option<String> {
    match object(g, node, p)? {
        Term::Literal(l) => Some(l.value().to_string()),
        Term::NamedNode(n) => Some(n.into_string()),
        _ => None,
    }
}

/// Prefix declarations reachable through `sh:prefixes` / `sh:declare`,
/// following `owl:imports` between prefix holders.
pub(crate) fn prefixes(g: &Graph, node: &Term) -> Vec<(String, String)> {
    let imports = NamedNodeRef::new_unchecked("http://www.w3.org/2002/07/owl#imports");
    let mut out = Vec::new();
    let mut todo = objects(g, node, sh::PREFIXES);
    let mut seen = HashSet::new();
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        for d in objects(g, &p, sh::DECLARE) {
            if let (Some(prefix), Some(ns)) =
                (text_of(g, &d, sh::PREFIX), text_of(g, &d, sh::NAMESPACE))
            {
                out.push((prefix, ns));
            }
        }
        todo.extend(objects(g, &p, imports));
    }
    out
}

/// Fill `{?var}` and `{$var}` placeholders in a message.
fn fill(message: &Term, bindings: &[(String, Term)]) -> Term {
    let Term::Literal(l) = message else {
        return message.clone();
    };
    let mut text = l.value().to_string();
    for (name, term) in bindings {
        let value = match term {
            Term::NamedNode(n) => n.as_str().to_string(),
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        };
        text = text
            .replace(&format!("{{?{name}}}"), &value)
            .replace(&format!("{{${name}}}"), &value);
    }
    match l.language() {
        Some(lang) => Literal::new_language_tagged_literal_unchecked(text, lang).into(),
        None => Literal::new_simple_literal(text).into(),
    }
}

fn row_get<'r>(row: &'r [(String, Term)], name: &str) -> Option<&'r Term> {
    row.iter().find(|(n, _)| n == name).map(|(_, t)| t)
}

// ---------- sh:sparql ----------

pub(crate) struct SparqlConstraint {
    node: Term,
    select: String,
    prefixes: Vec<(String, String)>,
    messages: Vec<Term>,
    pub deactivated: bool,
}

impl SparqlConstraint {
    pub fn parse(g: &Graph, node: &Term) -> Result<Self> {
        let Some(select) = text_of(g, node, sh::SELECT) else {
            bail!("SPARQL constraint {node} has no sh:select");
        };
        Ok(Self {
            node: node.clone(),
            select,
            prefixes: prefixes(g, node),
            messages: objects(g, node, sh::MESSAGE),
            deactivated: is_true(object(g, node, sh::DEACTIVATED)),
        })
    }
}

pub(crate) fn check_constraint(
    engine: &Engine<'_>,
    shape: &Shape,
    c: &SparqlConstraint,
    focus: &Term,
    out: &mut Vec<Res>,
) -> Result<()> {
    let query = engine.sparql.query(
        &c.node,
        shape,
        &c.select,
        &c.prefixes,
        &["this", "currentShape", "shapesGraph"],
    )?;
    let Outcome::Rows(rows) = engine.sparql.run(
        &query,
        &[
            ("this", focus),
            ("currentShape", &shape.id),
            ("shapesGraph", &engine.sparql.shapes_graph),
        ],
    )?
    else {
        bail!("SPARQL constraint {} must be a SELECT query", c.node);
    };
    for row in rows {
        if is_true(row_get(&row, "failure").cloned()) {
            bail!(
                "SPARQL constraint {} reported a failure for {focus}",
                c.node
            );
        }
        let value = row_get(&row, "value")
            .cloned()
            .or_else(|| (!shape.is_property()).then(|| focus.clone()));
        let mut r = engine.result(
            shape,
            focus,
            value.as_ref(),
            sh::component("SPARQL"),
            String::new,
        );
        if let Some(Term::NamedNode(p)) = row_get(&row, "path") {
            r.path = Some(ResultPath::Predicate(p.clone()));
        }
        let mut bindings = row.clone();
        bindings.push(("this".into(), focus.clone()));
        let messages = if c.messages.is_empty() {
            &shape.messages
        } else {
            &c.messages
        };
        r.messages = messages.iter().map(|m| fill(m, &bindings)).collect();
        r.constraint = Some(c.node.clone());
        out.push(r);
    }
    Ok(())
}

// ---------- SPARQL-based constraint components ----------

struct Param {
    path: NamedNode,
    name: String,
    optional: bool,
}

enum Kind {
    Select(String),
    Ask(String),
}

struct Validator {
    node: Term,
    kind: Kind,
    prefixes: Vec<(String, String)>,
    messages: Vec<Term>,
}

pub(crate) struct Component {
    iri: NamedNode,
    params: Vec<Param>,
    messages: Vec<Term>,
    validator: Option<Validator>,
    node_validator: Option<Validator>,
    property_validator: Option<Validator>,
}

fn read_validator(g: &Graph, owner: &Term, p: oxrdf::NamedNodeRef<'_>) -> Option<Validator> {
    let node = object(g, owner, p)?;
    let kind = if let Some(q) = text_of(g, &node, sh::SELECT) {
        Kind::Select(q)
    } else {
        Kind::Ask(text_of(g, &node, sh::ASK)?)
    };
    Some(Validator {
        prefixes: prefixes(g, &node),
        messages: objects(g, &node, sh::MESSAGE),
        node,
        kind,
    })
}

/// The SPARQL-based constraint components declared in the shapes graph.
pub(crate) fn components(g: &Graph) -> Result<Vec<Rc<Component>>> {
    let mut out = Vec::new();
    // Components may be typed with a subclass of sh:ConstraintComponent.
    let mut classes: Vec<Term> = vec![sh::CONSTRAINT_COMPONENT.into()];
    let mut i = 0;
    while i < classes.len() {
        for sub in g.subjects_for_predicate_object(rdfs::SUB_CLASS_OF, &classes[i]) {
            let sub: Term = sub.into_owned().into();
            if !classes.contains(&sub) {
                classes.push(sub);
            }
        }
        i += 1;
    }
    let mut ids: Vec<Term> = Vec::new();
    for class in &classes {
        for s in g.subjects_for_predicate_object(rdf::TYPE, class) {
            let s: Term = s.into_owned().into();
            if !ids.contains(&s) {
                ids.push(s);
            }
        }
    }
    for id in ids {
        let Term::NamedNode(iri) = &id else { continue };
        let validator = read_validator(g, &id, sh::VALIDATOR);
        let node_validator = read_validator(g, &id, sh::NODE_VALIDATOR);
        let property_validator = read_validator(g, &id, sh::PROPERTY_VALIDATOR);
        if validator.is_none() && node_validator.is_none() && property_validator.is_none() {
            continue;
        }
        let mut params = Vec::new();
        for p in objects(g, &id, sh::PARAMETER) {
            let Some(Term::NamedNode(path)) = object(g, &p, sh::PATH) else {
                bail!("parameter of {id} has no IRI sh:path");
            };
            let local = path
                .as_str()
                .rsplit(['#', '/'])
                .next()
                .unwrap_or("")
                .to_string();
            params.push(Param {
                name: local,
                optional: is_true(object(g, &p, sh::OPTIONAL)),
                path,
            });
        }
        out.push(Rc::new(Component {
            iri: iri.clone(),
            params,
            messages: objects(g, &id, sh::MESSAGE),
            validator,
            node_validator,
            property_validator,
        }));
    }
    Ok(out)
}

impl Component {
    /// Parameter bindings for each constraint this component has on `shape`
    /// (one per combination of parameter values).
    pub fn bindings(&self, g: &Graph, shape: &Term) -> Vec<Vec<(String, Term)>> {
        if as_subject(shape).is_none() {
            return Vec::new();
        }
        let mut combos: Vec<Vec<(String, Term)>> = vec![Vec::new()];
        let mut any_required = false;
        for p in &self.params {
            let values = objects(g, shape, p.path.as_ref());
            if values.is_empty() {
                if p.optional {
                    continue;
                }
                return Vec::new();
            }
            any_required |= !p.optional;
            combos = combos
                .into_iter()
                .flat_map(|c| {
                    values.iter().map(move |v| {
                        let mut c = c.clone();
                        c.push((p.name.clone(), v.clone()));
                        c
                    })
                })
                .collect();
        }
        if !any_required {
            return Vec::new();
        }
        combos
    }
}

pub(crate) fn check_component(
    engine: &Engine<'_>,
    shape: &Shape,
    component: &Component,
    bindings: &[(String, Term)],
    focus: &Term,
    values: &[Term],
    out: &mut Vec<Res>,
) -> Result<()> {
    let specific = if shape.is_property() {
        component.property_validator.as_ref()
    } else {
        component.node_validator.as_ref()
    };
    let Some(v) = specific.or(component.validator.as_ref()) else {
        return Ok(());
    };
    let messages = if !shape.messages.is_empty() {
        &shape.messages
    } else if !v.messages.is_empty() {
        &v.messages
    } else {
        &component.messages
    };
    let mut push = |value: Option<&Term>, extra: &[(String, Term)]| {
        let mut r = engine.result(shape, focus, value, component.iri.clone(), String::new);
        let mut all: Vec<(String, Term)> = bindings.to_vec();
        all.extend_from_slice(extra);
        all.push(("this".into(), focus.clone()));
        if let Some(value) = value {
            all.push(("value".into(), value.clone()));
        }
        r.messages = messages.iter().map(|m| fill(m, &all)).collect();
        if let Some(Term::NamedNode(p)) = row_get(extra, "path") {
            r.path = Some(ResultPath::Predicate(p.clone()));
        }
        out.push(r);
    };
    let mut pre: Vec<(&str, &Term)> = bindings.iter().map(|(n, t)| (n.as_str(), t)).collect();
    pre.push(("this", focus));
    pre.push(("currentShape", &shape.id));
    pre.push(("shapesGraph", &engine.sparql.shapes_graph));
    let mut names: Vec<&str> = pre.iter().map(|(n, _)| *n).collect();
    let value_bound = matches!(v.kind, Kind::Ask(_)) || !shape.is_property();
    if value_bound {
        names.push("value");
    }
    match &v.kind {
        Kind::Ask(text) => {
            let query = engine
                .sparql
                .query(&v.node, shape, text, &v.prefixes, &names)?;
            for value in values {
                let mut b = pre.clone();
                b.push(("value", value));
                match engine.sparql.run(&query, &b)? {
                    Outcome::Bool(true) => {}
                    Outcome::Bool(false) => push(Some(value), &[]),
                    Outcome::Rows(_) => bail!("validator {} must be an ASK query", v.node),
                }
            }
        }
        Kind::Select(text) => {
            let query = engine
                .sparql
                .query(&v.node, shape, text, &v.prefixes, &names)?;
            if !shape.is_property() {
                pre.push(("value", focus));
            }
            let Outcome::Rows(rows) = engine.sparql.run(&query, &pre)? else {
                bail!("validator {} must be a SELECT query", v.node);
            };
            for row in rows {
                if is_true(row_get(&row, "failure").cloned()) {
                    bail!("validator {} reported a failure for {focus}", v.node);
                }
                let value = row_get(&row, "value")
                    .cloned()
                    .or_else(|| (!shape.is_property()).then(|| focus.clone()));
                push(value.as_ref(), &row);
            }
        }
    }
    Ok(())
}
