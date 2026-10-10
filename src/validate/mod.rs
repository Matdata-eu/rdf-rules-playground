//! A SHACL validation engine: all of SHACL Core, SPARQL-based constraints
//! (`sh:sparql`) and SPARQL-based constraint components. It comes from
//! [shacl-report](https://github.com/Matdata-eu/shacl-report), where it is
//! checked against the W3C SHACL test suite. The rules engine uses it to find
//! a shape's focus nodes and to check `sh:condition` and `sh:filterShape`.

mod path;
pub(crate) mod sparql;
mod turtle;
pub(crate) mod values;
pub(crate) mod vocab;

use anyhow::{Context, Result, bail};
use oxrdf::vocab::{rdf, rdfs, xsd};
use oxrdf::{
    BlankNode, Graph, Literal, NamedNode, NamedNodeRef, NamedOrBlankNodeRef, Term, TermRef,
    TripleRef,
};
use path::Path;
use regex::{Regex, RegexBuilder};
use sparql::{Component, SparqlConstraint, SparqlRunner};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use vocab::sh;

pub use path::Path as ShaclPath;

/// The outcome of a validation run.
pub struct Validation {
    /// The `sh:ValidationReport` graph.
    pub report: Graph,
    pub conforms: bool,
    pub results: usize,
}

/// Validate `data` against the shapes in `shapes`.
pub fn validate(data: &Graph, shapes: &Graph) -> Result<Validation> {
    validate_until(data, shapes, None)
}

/// [`validate`], giving up with an error once `deadline` has passed. The
/// check runs between focus nodes, so one slow node can overrun it.
pub fn validate_until(
    data: &Graph,
    shapes: &Graph,
    deadline: Option<std::time::Instant>,
) -> Result<Validation> {
    let engine = Engine::new(data, shapes)?;
    let mut results = Vec::new();
    for id in engine.shapes_with_targets() {
        let shape = engine.shape(&id)?;
        if shape.deactivated {
            continue;
        }
        for focus in engine.focus_nodes(&id) {
            if deadline.is_some_and(|d| std::time::Instant::now() > d) {
                anyhow::bail!("validation took longer than this server allows");
            }
            engine.validate_shape(&shape, &focus, &mut results)?;
        }
    }
    let report = report_graph(&results, shapes);
    Ok(Validation {
        conforms: results.is_empty(),
        results: results.len(),
        report,
    })
}

/// Answers the two questions the rules engine asks of SHACL Core: which
/// nodes a shape targets, and whether a node conforms to a shape.
pub struct Checker<'a> {
    engine: Engine<'a>,
}

impl<'a> Checker<'a> {
    pub fn new(data: &'a Graph, shapes: &'a Graph) -> Result<Self> {
        Ok(Self {
            engine: Engine::new(data, shapes)?,
        })
    }

    /// The focus nodes of `shape` in the data graph, from its targets
    /// (`sh:targetNode`, `sh:targetClass`, `sh:targetSubjectsOf`,
    /// `sh:targetObjectsOf`, and implicit class targets).
    pub fn focus_nodes(&self, shape: &Term) -> Vec<Term> {
        self.engine.focus_nodes(shape)
    }

    pub fn shapes(&self) -> &'a Graph {
        self.engine.shapes
    }

    /// True when `node` conforms to `shape`.
    pub fn conforms(&self, shape: &Term, node: &Term) -> Result<bool> {
        self.engine.conforms(shape, node)
    }

    /// The validation results `node` produces against `shape`, as messages.
    pub fn violations(&self, shape: &Term, node: &Term) -> Result<Vec<String>> {
        let s = self.engine.shape(shape)?;
        let mut out = Vec::new();
        self.engine.validate_shape(&s, node, &mut out)?;
        Ok(out
            .iter()
            .map(|r| match r.messages.first() {
                Some(Term::Literal(l)) => l.value().to_string(),
                _ => r.component.as_str().to_string(),
            })
            .collect())
    }
}

/// Serialize a report graph as readable Turtle with the given prefixes.
pub fn to_turtle(report: &Graph, prefixes: &[(String, String)]) -> String {
    turtle::write(report, prefixes)
}

// ---------- graph helpers ----------

pub(crate) fn as_subject(t: &Term) -> Option<NamedOrBlankNodeRef<'_>> {
    match t {
        Term::NamedNode(n) => Some(n.as_ref().into()),
        Term::BlankNode(b) => Some(b.as_ref().into()),
        _ => None,
    }
}

pub(crate) fn object(g: &Graph, s: &Term, p: NamedNodeRef<'_>) -> Option<Term> {
    g.object_for_subject_predicate(as_subject(s)?, p)
        .map(TermRef::into_owned)
}

pub(crate) fn objects(g: &Graph, s: &Term, p: NamedNodeRef<'_>) -> Vec<Term> {
    match as_subject(s) {
        Some(s) => g
            .objects_for_subject_predicate(s, p)
            .map(TermRef::into_owned)
            .collect(),
        None => Vec::new(),
    }
}

/// The members of an RDF list.
pub(crate) fn list(g: &Graph, head: &Term) -> Vec<Term> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut node = head.clone();
    while node != Term::from(rdf::NIL) && seen.insert(node.clone()) {
        let Some(first) = object(g, &node, rdf::FIRST) else {
            break;
        };
        out.push(first);
        match object(g, &node, rdf::REST) {
            Some(rest) => node = rest,
            None => break,
        }
    }
    out
}

fn is_true(t: Option<Term>) -> bool {
    // Only "true" counts: "1"^^xsd:boolean does not activate a constraint.
    matches!(t, Some(Term::Literal(l)) if l.value() == "true")
}

fn count(t: &Term) -> Result<usize> {
    match t {
        Term::Literal(l) => l
            .value()
            .parse()
            .with_context(|| format!("{t} is not a valid count")),
        _ => bail!("{t} is not a valid count"),
    }
}

/// How a term is named in default messages: `<iri>` (shortened by the viewer)
/// or the N-Triples form of a literal.
fn show(t: &Term) -> String {
    match t {
        Term::BlankNode(_) => "a nested shape".to_string(),
        Term::Literal(l) if l.language().is_none() => l.value().to_string(),
        other => other.to_string(),
    }
}

// ---------- shapes ----------

#[derive(Debug, Clone, Copy)]
enum Bound {
    MinExclusive,
    MinInclusive,
    MaxExclusive,
    MaxInclusive,
}

#[derive(Debug, Clone, Copy)]
enum Pair {
    Equals,
    Disjoint,
    LessThan,
    LessThanOrEquals,
}

enum Constraint {
    Class(Term),
    Datatype(NamedNode),
    NodeKind(NamedNode),
    MinCount(usize),
    MaxCount(usize),
    Range(Bound, Term),
    MinLength(usize),
    MaxLength(usize),
    Pattern(Regex, String),
    LanguageIn(Vec<String>),
    UniqueLang,
    Pair(Pair, NamedNode),
    Not(Term),
    And(Vec<Term>),
    Or(Vec<Term>),
    Xone(Vec<Term>),
    Node(Term),
    Property(Term),
    Qualified {
        shape: Term,
        min: Option<usize>,
        max: Option<usize>,
        siblings: Vec<Term>,
    },
    Closed(HashSet<NamedNode>),
    HasValue(Term),
    In(Vec<Term>),
    Sparql(Rc<SparqlConstraint>),
    Component(Rc<Component>, Vec<(String, Term)>),
}

pub(crate) struct Shape {
    pub id: Term,
    pub path: Option<Path>,
    path_node: Option<Term>,
    severity: Term,
    pub messages: Vec<Term>,
    deactivated: bool,
    constraints: Vec<Constraint>,
}

impl Shape {
    pub fn is_property(&self) -> bool {
        self.path.is_some()
    }
}

/// One validation result before it is written to the report graph.
pub(crate) struct Res {
    focus: Term,
    path: Option<ResultPath>,
    value: Option<Term>,
    shape: Term,
    component: NamedNode,
    severity: Term,
    messages: Vec<Term>,
    constraint: Option<Term>,
}

pub(crate) enum ResultPath {
    /// A path node in the shapes graph.
    Shape(Term),
    Predicate(NamedNode),
}

pub(crate) struct Engine<'a> {
    pub data: &'a Graph,
    pub shapes: &'a Graph,
    cache: RefCell<HashMap<Term, Rc<Shape>>>,
    stack: RefCell<Vec<(Term, Term)>>,
    superclasses: RefCell<HashMap<Term, Rc<HashSet<Term>>>>,
    components: Vec<Rc<Component>>,
    pub sparql: SparqlRunner<'a>,
}

impl<'a> Engine<'a> {
    fn new(data: &'a Graph, shapes: &'a Graph) -> Result<Self> {
        Ok(Self {
            data,
            shapes,
            cache: RefCell::default(),
            stack: RefCell::default(),
            superclasses: RefCell::default(),
            components: sparql::components(shapes)?,
            sparql: SparqlRunner::new(data, shapes),
        })
    }

    /// Shapes with explicit targets, and shapes that are also classes.
    fn shapes_with_targets(&self) -> Vec<Term> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let g = self.shapes;
        for p in [
            sh::TARGET_NODE,
            sh::TARGET_CLASS,
            sh::TARGET_SUBJECTS_OF,
            sh::TARGET_OBJECTS_OF,
        ] {
            for t in g.triples_for_predicate(p) {
                let s: Term = t.subject.into_owned().into();
                if seen.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
        for t in g.subjects_for_predicate_object(rdf::TYPE, rdfs::CLASS) {
            let s: Term = t.into_owned().into();
            let types = objects(g, &s, rdf::TYPE);
            let is_shape = types
                .iter()
                .any(|t| *t == sh::NODE_SHAPE.into() || *t == sh::PROPERTY_SHAPE.into());
            if is_shape && seen.insert(s.clone()) {
                out.push(s);
            }
        }
        out
    }

    fn focus_nodes(&self, shape: &Term) -> Vec<Term> {
        let g = self.shapes;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut add = |t: Term| {
            if seen.insert(t.clone()) {
                out.push(t);
            }
        };
        for n in objects(g, shape, sh::TARGET_NODE) {
            add(n);
        }
        let mut classes = objects(g, shape, sh::TARGET_CLASS);
        if objects(g, shape, rdf::TYPE).contains(&rdfs::CLASS.into()) {
            classes.push(shape.clone());
        }
        for c in classes {
            for n in self.instances(&c) {
                add(n);
            }
        }
        for p in objects(g, shape, sh::TARGET_SUBJECTS_OF) {
            if let Term::NamedNode(p) = p {
                for t in self.data.triples_for_predicate(&p) {
                    add(t.subject.into_owned().into());
                }
            }
        }
        for p in objects(g, shape, sh::TARGET_OBJECTS_OF) {
            if let Term::NamedNode(p) = p {
                for t in self.data.triples_for_predicate(&p) {
                    add(t.object.into_owned());
                }
            }
        }
        out
    }

    /// SHACL instances of `class`: nodes typed with it or one of its subclasses.
    fn instances(&self, class: &Term) -> Vec<Term> {
        let mut classes = vec![class.clone()];
        let mut seen: HashSet<Term> = classes.iter().cloned().collect();
        let mut i = 0;
        while i < classes.len() {
            let c = classes[i].clone();
            for s in self
                .data
                .subjects_for_predicate_object(rdfs::SUB_CLASS_OF, &c)
            {
                let s: Term = s.into_owned().into();
                if seen.insert(s.clone()) {
                    classes.push(s);
                }
            }
            i += 1;
        }
        let mut out = Vec::new();
        let mut found = HashSet::new();
        for c in &classes {
            for s in self.data.subjects_for_predicate_object(rdf::TYPE, c) {
                let s: Term = s.into_owned().into();
                if found.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
        out
    }

    fn is_instance(&self, node: &Term, class: &Term) -> bool {
        objects(self.data, node, rdf::TYPE)
            .iter()
            .any(|t| t == class || self.superclasses_of(t).contains(class))
    }

    fn superclasses_of(&self, class: &Term) -> Rc<HashSet<Term>> {
        if let Some(s) = self.superclasses.borrow().get(class) {
            return s.clone();
        }
        let mut seen = HashSet::new();
        let mut todo = vec![class.clone()];
        while let Some(c) = todo.pop() {
            for s in objects(self.data, &c, rdfs::SUB_CLASS_OF) {
                if seen.insert(s.clone()) {
                    todo.push(s);
                }
            }
        }
        let rc = Rc::new(seen);
        self.superclasses
            .borrow_mut()
            .insert(class.clone(), rc.clone());
        rc
    }

    pub fn shape(&self, id: &Term) -> Result<Rc<Shape>> {
        if let Some(s) = self.cache.borrow().get(id) {
            return Ok(s.clone());
        }
        let shape = Rc::new(
            self.parse_shape(id)
                .with_context(|| format!("invalid shape {id}"))?,
        );
        self.cache.borrow_mut().insert(id.clone(), shape.clone());
        Ok(shape)
    }

    fn parse_shape(&self, id: &Term) -> Result<Shape> {
        let g = self.shapes;
        let path_node = object(g, id, sh::PATH);
        let path = path_node.as_ref().map(|p| Path::parse(g, p)).transpose()?;
        let mut constraints = Vec::new();
        let one = |p| object(g, id, p);
        let iri = |t: Term, what: &str| match t {
            Term::NamedNode(n) => Ok(n),
            other => bail!("{what} must be an IRI, not {other}"),
        };
        let Some(subject) = as_subject(id) else {
            bail!("a shape must be an IRI or a blank node");
        };
        for t in g.triples_for_subject(subject) {
            let o = t.object.into_owned();
            let p = t.predicate;
            let c = if p == sh::CLASS {
                Constraint::Class(o)
            } else if p == sh::DATATYPE {
                Constraint::Datatype(iri(o, "sh:datatype")?)
            } else if p == sh::NODE_KIND {
                Constraint::NodeKind(iri(o, "sh:nodeKind")?)
            } else if p == sh::MIN_COUNT {
                Constraint::MinCount(count(&o)?)
            } else if p == sh::MAX_COUNT {
                Constraint::MaxCount(count(&o)?)
            } else if p == sh::MIN_EXCLUSIVE {
                Constraint::Range(Bound::MinExclusive, o)
            } else if p == sh::MIN_INCLUSIVE {
                Constraint::Range(Bound::MinInclusive, o)
            } else if p == sh::MAX_EXCLUSIVE {
                Constraint::Range(Bound::MaxExclusive, o)
            } else if p == sh::MAX_INCLUSIVE {
                Constraint::Range(Bound::MaxInclusive, o)
            } else if p == sh::MIN_LENGTH {
                Constraint::MinLength(count(&o)?)
            } else if p == sh::MAX_LENGTH {
                Constraint::MaxLength(count(&o)?)
            } else if p == sh::PATTERN {
                let flags = one(sh::FLAGS)
                    .and_then(|f| values::str_of(&f).map(str::to_string))
                    .unwrap_or_default();
                let pattern = values::str_of(&o).unwrap_or_default().to_string();
                Constraint::Pattern(build_regex(&pattern, &flags)?, pattern)
            } else if p == sh::LANGUAGE_IN {
                Constraint::LanguageIn(
                    list(g, &o)
                        .iter()
                        .filter_map(|t| values::str_of(t).map(str::to_string))
                        .collect(),
                )
            } else if p == sh::UNIQUE_LANG {
                if !is_true(Some(o)) {
                    continue;
                }
                Constraint::UniqueLang
            } else if p == sh::EQUALS {
                Constraint::Pair(Pair::Equals, iri(o, "sh:equals")?)
            } else if p == sh::DISJOINT {
                Constraint::Pair(Pair::Disjoint, iri(o, "sh:disjoint")?)
            } else if p == sh::LESS_THAN {
                Constraint::Pair(Pair::LessThan, iri(o, "sh:lessThan")?)
            } else if p == sh::LESS_THAN_OR_EQUALS {
                Constraint::Pair(Pair::LessThanOrEquals, iri(o, "sh:lessThanOrEquals")?)
            } else if p == sh::NOT {
                Constraint::Not(o)
            } else if p == sh::AND {
                Constraint::And(list(g, &o))
            } else if p == sh::OR {
                Constraint::Or(list(g, &o))
            } else if p == sh::XONE {
                Constraint::Xone(list(g, &o))
            } else if p == sh::NODE {
                Constraint::Node(o)
            } else if p == sh::PROPERTY {
                Constraint::Property(o)
            } else if p == sh::QUALIFIED_VALUE_SHAPE {
                let min = one(sh::QUALIFIED_MIN_COUNT)
                    .map(|t| count(&t))
                    .transpose()?;
                let max = one(sh::QUALIFIED_MAX_COUNT)
                    .map(|t| count(&t))
                    .transpose()?;
                let siblings = if is_true(one(sh::QUALIFIED_VALUE_SHAPES_DISJOINT)) {
                    self.sibling_shapes(id, &o)
                } else {
                    Vec::new()
                };
                Constraint::Qualified {
                    shape: o,
                    min,
                    max,
                    siblings,
                }
            } else if p == sh::CLOSED {
                if !is_true(Some(o)) {
                    continue;
                }
                let mut allowed = HashSet::new();
                for ps in objects(g, id, sh::PROPERTY) {
                    if let Some(Term::NamedNode(p)) = object(g, &ps, sh::PATH) {
                        allowed.insert(p);
                    }
                }
                for ignored in objects(g, id, sh::IGNORED_PROPERTIES) {
                    for t in list(g, &ignored) {
                        if let Term::NamedNode(p) = t {
                            allowed.insert(p);
                        }
                    }
                }
                Constraint::Closed(allowed)
            } else if p == sh::HAS_VALUE {
                Constraint::HasValue(o)
            } else if p == sh::IN {
                Constraint::In(list(g, &o))
            } else if p == sh::SPARQL {
                let c = SparqlConstraint::parse(g, &o)?;
                if c.deactivated {
                    continue;
                }
                Constraint::Sparql(Rc::new(c))
            } else {
                continue;
            };
            constraints.push(c);
        }
        for component in &self.components {
            for bindings in component.bindings(g, id) {
                constraints.push(Constraint::Component(component.clone(), bindings));
            }
        }
        Ok(Shape {
            id: id.clone(),
            path,
            path_node,
            severity: one(sh::SEVERITY).unwrap_or_else(|| sh::VIOLATION.into()),
            messages: objects(g, id, sh::MESSAGE),
            deactivated: is_true(one(sh::DEACTIVATED)),
            constraints,
        })
    }

    /// The other qualified value shapes next to `qvs`, for
    /// `sh:qualifiedValueShapesDisjoint`.
    fn sibling_shapes(&self, shape: &Term, qvs: &Term) -> Vec<Term> {
        let g = self.shapes;
        let mut out = Vec::new();
        for parent in g.subjects_for_predicate_object(sh::PROPERTY, shape) {
            let parent: Term = parent.into_owned().into();
            for ps in objects(g, &parent, sh::PROPERTY) {
                for other in objects(g, &ps, sh::QUALIFIED_VALUE_SHAPE) {
                    if other != *qvs && !out.contains(&other) {
                        out.push(other);
                    }
                }
            }
        }
        out
    }

    /// True when `node` conforms to the shape `id` (no results at all).
    pub fn conforms(&self, id: &Term, node: &Term) -> Result<bool> {
        let shape = self.shape(id)?;
        let mut out = Vec::new();
        self.validate_shape(&shape, node, &mut out)?;
        Ok(out.is_empty())
    }

    pub fn validate_shape(&self, shape: &Shape, focus: &Term, out: &mut Vec<Res>) -> Result<()> {
        if shape.deactivated {
            return Ok(());
        }
        // Recursive shapes: a node that is already being checked against
        // this shape is assumed to conform.
        let key = (shape.id.clone(), focus.clone());
        if self.stack.borrow().contains(&key) {
            return Ok(());
        }
        self.stack.borrow_mut().push(key);
        let values = match &shape.path {
            Some(p) => p.eval(self.data, focus),
            None => vec![focus.clone()],
        };
        let mut result = Ok(());
        for c in &shape.constraints {
            result = self.check(shape, c, focus, &values, out);
            if result.is_err() {
                break;
            }
        }
        self.stack.borrow_mut().pop();
        result
    }

    pub fn result(
        &self,
        shape: &Shape,
        focus: &Term,
        value: Option<&Term>,
        component: NamedNode,
        message: impl FnOnce() -> String,
    ) -> Res {
        let messages = if shape.messages.is_empty() {
            vec![Literal::new_simple_literal(message()).into()]
        } else {
            shape.messages.clone()
        };
        Res {
            focus: focus.clone(),
            path: shape.path_node.clone().map(ResultPath::Shape),
            value: value.cloned(),
            shape: shape.id.clone(),
            component,
            severity: shape.severity.clone(),
            messages,
            constraint: None,
        }
    }

    fn check(
        &self,
        shape: &Shape,
        c: &Constraint,
        focus: &Term,
        values: &[Term],
        out: &mut Vec<Res>,
    ) -> Result<()> {
        let each = |out: &mut Vec<Res>,
                    name: &str,
                    fails: &dyn Fn(&Term) -> Result<bool>,
                    msg: &dyn Fn(&Term) -> String|
         -> Result<()> {
            for v in values {
                if fails(v)? {
                    out.push(self.result(shape, focus, Some(v), sh::component(name), || msg(v)));
                }
            }
            Ok(())
        };
        match c {
            Constraint::Class(class) => {
                each(out, "Class", &|v| Ok(!self.is_instance(v, class)), &|_| {
                    format!("Value is not an instance of {}", show(class))
                })?
            }
            Constraint::Datatype(dt) => each(
                out,
                "Datatype",
                &|v| {
                    Ok(!matches!(v, Term::Literal(l)
                        if l.datatype() == dt.as_ref() && values::well_formed(l)))
                },
                &|_| format!("Value is not a valid literal of datatype <{}>", dt.as_str()),
            )?,
            Constraint::NodeKind(kind) => each(
                out,
                "NodeKind",
                &|v| Ok(!node_kind_matches(kind.as_ref(), v)),
                &|_| format!("Value is not of node kind <{}>", kind.as_str()),
            )?,
            Constraint::MinCount(n) => {
                if values.len() < *n {
                    out.push(
                        self.result(shape, focus, None, sh::component("MinCount"), || {
                            format!("Fewer than {n} values (found {})", values.len())
                        }),
                    );
                }
            }
            Constraint::MaxCount(n) => {
                if values.len() > *n {
                    out.push(
                        self.result(shape, focus, None, sh::component("MaxCount"), || {
                            format!("More than {n} values (found {})", values.len())
                        }),
                    );
                }
            }
            Constraint::Range(bound, limit) => {
                let (name, op) = match bound {
                    Bound::MinExclusive => ("MinExclusive", ">"),
                    Bound::MinInclusive => ("MinInclusive", ">="),
                    Bound::MaxExclusive => ("MaxExclusive", "<"),
                    Bound::MaxInclusive => ("MaxInclusive", "<="),
                };
                each(
                    out,
                    name,
                    &|v| {
                        use std::cmp::Ordering::*;
                        let ok = match (bound, values::compare(v, limit)) {
                            (_, None) => false,
                            (Bound::MinExclusive, Some(o)) => o == Greater,
                            (Bound::MinInclusive, Some(o)) => o != Less,
                            (Bound::MaxExclusive, Some(o)) => o == Less,
                            (Bound::MaxInclusive, Some(o)) => o != Greater,
                        };
                        Ok(!ok)
                    },
                    &|_| format!("Value is not {op} {}", show(limit)),
                )?
            }
            Constraint::MinLength(n) => each(
                out,
                "MinLength",
                &|v| Ok(values::str_of(v).is_none_or(|s| s.chars().count() < *n)),
                &|_| format!("Value is shorter than {n} characters"),
            )?,
            Constraint::MaxLength(n) => each(
                out,
                "MaxLength",
                &|v| Ok(values::str_of(v).is_none_or(|s| s.chars().count() > *n)),
                &|_| format!("Value is longer than {n} characters"),
            )?,
            Constraint::Pattern(re, pattern) => each(
                out,
                "Pattern",
                &|v| Ok(values::str_of(v).is_none_or(|s| !re.is_match(s))),
                &|_| format!("Value does not match the pattern \"{pattern}\""),
            )?,
            Constraint::LanguageIn(langs) => each(
                out,
                "LanguageIn",
                &|v| {
                    Ok(!matches!(v, Term::Literal(l)
                        if l.language().is_some_and(|tag| langs.iter().any(|r| values::lang_matches(tag, r)))))
                },
                &|_| format!("Language tag is not one of {}", langs.join(", ")),
            )?,
            Constraint::UniqueLang => {
                let mut counts: Vec<(String, usize)> = Vec::new();
                for v in values {
                    if let Term::Literal(l) = v
                        && let Some(tag) = l.language()
                    {
                        let tag = tag.to_ascii_lowercase();
                        match counts.iter_mut().find(|(t, _)| *t == tag) {
                            Some((_, n)) => *n += 1,
                            None => counts.push((tag, 1)),
                        }
                    }
                }
                for (tag, n) in counts {
                    if n > 1 {
                        out.push(self.result(
                            shape,
                            focus,
                            None,
                            sh::component("UniqueLang"),
                            || format!("Language \"{tag}\" is used by more than one value"),
                        ));
                    }
                }
            }
            Constraint::Pair(kind, p) => self.check_pair(shape, *kind, p, focus, values, out),
            Constraint::Not(s) => each(out, "Not", &|v| self.conforms(s, v), &|_| {
                format!("Value conforms to {} but must not", show(s))
            })?,
            Constraint::And(shapes) => each(
                out,
                "And",
                &|v| {
                    for s in shapes {
                        if !self.conforms(s, v)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                },
                &|_| "Value does not conform to all of the shapes in sh:and".to_string(),
            )?,
            Constraint::Or(shapes) => each(
                out,
                "Or",
                &|v| {
                    for s in shapes {
                        if self.conforms(s, v)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                },
                &|_| "Value does not conform to any of the shapes in sh:or".to_string(),
            )?,
            Constraint::Xone(shapes) => each(
                out,
                "Xone",
                &|v| {
                    let mut n = 0;
                    for s in shapes {
                        if self.conforms(s, v)? {
                            n += 1;
                        }
                    }
                    Ok(n != 1)
                },
                &|_| "Value does not conform to exactly one of the shapes in sh:xone".to_string(),
            )?,
            Constraint::Node(s) => each(out, "Node", &|v| Ok(!self.conforms(s, v)?), &|_| {
                format!("Value does not conform to {}", show(s))
            })?,
            Constraint::Property(ps) => {
                let ps = self.shape(ps)?;
                for v in values {
                    self.validate_shape(&ps, v, out)?;
                }
            }
            Constraint::Qualified {
                shape: qvs,
                min,
                max,
                siblings,
            } => {
                let mut n = 0;
                for v in values {
                    if self.conforms(qvs, v)? {
                        let mut in_sibling = false;
                        for s in siblings {
                            if self.conforms(s, v)? {
                                in_sibling = true;
                                break;
                            }
                        }
                        if !in_sibling {
                            n += 1;
                        }
                    }
                }
                if let Some(min) = min
                    && n < *min
                {
                    out.push(self.result(
                        shape,
                        focus,
                        None,
                        sh::component("QualifiedMinCount"),
                        || {
                            format!(
                                "Fewer than {min} values conform to {} (found {n})",
                                show(qvs)
                            )
                        },
                    ));
                }
                if let Some(max) = max
                    && n > *max
                {
                    out.push(self.result(
                        shape,
                        focus,
                        None,
                        sh::component("QualifiedMaxCount"),
                        || {
                            format!(
                                "More than {max} values conform to {} (found {n})",
                                show(qvs)
                            )
                        },
                    ));
                }
            }
            Constraint::Closed(allowed) => {
                for v in values {
                    let Some(s) = as_subject(v) else { continue };
                    for t in self.data.triples_for_subject(s) {
                        if allowed.contains(&t.predicate.into_owned()) {
                            continue;
                        }
                        let p = t.predicate.into_owned();
                        let mut r = self.result(
                            shape,
                            focus,
                            Some(&t.object.into_owned()),
                            sh::component("Closed"),
                            || {
                                format!(
                                    "Property <{}> is not allowed by the closed shape",
                                    p.as_str()
                                )
                            },
                        );
                        r.path = Some(ResultPath::Predicate(p));
                        out.push(r);
                    }
                }
            }
            Constraint::HasValue(expected) => {
                if !values.contains(expected) {
                    out.push(
                        self.result(shape, focus, None, sh::component("HasValue"), || {
                            format!("Missing the value {}", show(expected))
                        }),
                    );
                }
            }
            Constraint::In(allowed) => each(out, "In", &|v| Ok(!allowed.contains(v)), &|_| {
                let items: Vec<String> = allowed.iter().map(show).collect();
                format!("Value is not one of {}", items.join(", "))
            })?,
            Constraint::Sparql(c) => sparql::check_constraint(self, shape, c, focus, out)?,
            Constraint::Component(component, bindings) => {
                sparql::check_component(self, shape, component, bindings, focus, values, out)?
            }
        }
        Ok(())
    }

    fn check_pair(
        &self,
        shape: &Shape,
        kind: Pair,
        p: &NamedNode,
        focus: &Term,
        values: &[Term],
        out: &mut Vec<Res>,
    ) {
        let others = objects(self.data, focus, p.as_ref());
        let name = match kind {
            Pair::Equals => "Equals",
            Pair::Disjoint => "Disjoint",
            Pair::LessThan => "LessThan",
            Pair::LessThanOrEquals => "LessThanOrEquals",
        };
        let mut push = |v: &Term, msg: String| {
            out.push(self.result(shape, focus, Some(v), sh::component(name), || msg));
        };
        let prop = format!("<{}>", p.as_str());
        match kind {
            Pair::Equals => {
                for v in values.iter().filter(|v| !others.contains(v)) {
                    push(v, format!("Value is not also a value of {prop}"));
                }
                for o in others.iter().filter(|o| !values.contains(o)) {
                    push(o, format!("Value of {prop} is missing here"));
                }
            }
            Pair::Disjoint => {
                for v in values.iter().filter(|v| others.contains(v)) {
                    push(v, format!("Value is also a value of {prop}"));
                }
            }
            Pair::LessThan | Pair::LessThanOrEquals => {
                let strict = matches!(kind, Pair::LessThan);
                for v in values {
                    for o in &others {
                        let ok = match values::compare(v, o) {
                            Some(std::cmp::Ordering::Less) => true,
                            Some(std::cmp::Ordering::Equal) => !strict,
                            _ => false,
                        };
                        if !ok {
                            let op = if strict { "<" } else { "<=" };
                            push(
                                v,
                                format!("Value is not {op} {} (a value of {prop})", show(o)),
                            );
                        }
                    }
                }
            }
        }
    }
}

fn node_kind_matches(kind: NamedNodeRef<'_>, v: &Term) -> bool {
    let (iri, blank, literal) = match v {
        Term::NamedNode(_) => (true, false, false),
        Term::BlankNode(_) => (false, true, false),
        Term::Literal(_) => (false, false, true),
        #[allow(unreachable_patterns)]
        _ => (false, false, false),
    };
    if kind == sh::IRI {
        iri
    } else if kind == sh::BLANK_NODE {
        blank
    } else if kind == sh::LITERAL {
        literal
    } else if kind == sh::BLANK_NODE_OR_IRI {
        blank || iri
    } else if kind == sh::BLANK_NODE_OR_LITERAL {
        blank || literal
    } else if kind == sh::IRI_OR_LITERAL {
        iri || literal
    } else {
        false
    }
}

/// Build a regex with SPARQL `REGEX` flags (`i`, `m`, `s`, `x`, `q`).
fn build_regex(pattern: &str, flags: &str) -> Result<Regex> {
    let pattern = if flags.contains('q') {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .ignore_whitespace(flags.contains('x'))
        .build()
        .with_context(|| format!("invalid sh:pattern \"{pattern}\""))
}

// ---------- report ----------

fn report_graph(results: &[Res], shapes: &Graph) -> Graph {
    let mut g = Graph::new();
    let report = BlankNode::default();
    g.insert(TripleRef::new(&report, rdf::TYPE, sh::VALIDATION_REPORT));
    let conforms = Literal::new_typed_literal(
        if results.is_empty() { "true" } else { "false" },
        xsd::BOOLEAN,
    );
    g.insert(TripleRef::new(&report, sh::CONFORMS, &conforms));
    for r in results {
        let node = BlankNode::default();
        g.insert(TripleRef::new(&report, sh::RESULT, &node));
        g.insert(TripleRef::new(&node, rdf::TYPE, sh::VALIDATION_RESULT));
        g.insert(TripleRef::new(&node, sh::FOCUS_NODE, &r.focus));
        match &r.path {
            Some(ResultPath::Shape(p)) => {
                g.insert(TripleRef::new(&node, sh::RESULT_PATH, p));
                path::copy_path(shapes, p, &mut g);
            }
            Some(ResultPath::Predicate(p)) => {
                g.insert(TripleRef::new(&node, sh::RESULT_PATH, p));
            }
            None => {}
        }
        if let Some(v) = &r.value {
            g.insert(TripleRef::new(&node, sh::VALUE, v));
        }
        g.insert(TripleRef::new(&node, sh::RESULT_SEVERITY, &r.severity));
        g.insert(TripleRef::new(
            &node,
            sh::SOURCE_CONSTRAINT_COMPONENT,
            &r.component,
        ));
        g.insert(TripleRef::new(&node, sh::SOURCE_SHAPE, &r.shape));
        if let Some(c) = &r.constraint {
            g.insert(TripleRef::new(&node, sh::SOURCE_CONSTRAINT, c));
        }
        for m in &r.messages {
            g.insert(TripleRef::new(&node, sh::RESULT_MESSAGE, m));
        }
    }
    g
}
