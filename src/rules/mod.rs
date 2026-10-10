//! A SHACL Advanced Features rules engine: `sh:TripleRule` with node
//! expressions and `sh:SPARQLRule` with CONSTRUCT queries, honouring
//! `sh:condition`, `sh:order` and `sh:deactivated`. Rules run in order and
//! each one sees the triples inferred before it; the whole set repeats until
//! a round infers nothing new (or a limit stops it).

pub mod expr;
pub mod sparql;

use crate::validate::sparql::prefixes;
use crate::validate::values;
use crate::validate::vocab::sh;
use crate::validate::{Checker, as_subject, object, objects};
use anyhow::{Context as _, Result};
use expr::{Context, Expr, cartesian};
use oxrdf::vocab::{rdf, rdfs};
use oxrdf::{Graph, NamedNode, NamedOrBlankNode, Term, Triple};
use serde::Serialize;
use spargebra::Query;
use sparql::{Answer, Store};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::time::Instant;

/// Limits and switches for one run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Repeat the rules until nothing new is inferred. When false, every
    /// rule runs exactly once.
    pub iterate: bool,
    /// Most rounds before giving up on reaching a fixpoint.
    pub max_iterations: usize,
    /// Most inferred triples before stopping.
    pub max_inferred: usize,
    /// Stop with an error after this moment.
    pub deadline: Option<Instant>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            iterate: true,
            max_iterations: 100,
            max_inferred: 1_000_000,
            deadline: None,
        }
    }
}

/// What a rule is, as shown in the rules overview.
#[derive(Debug, Clone, Serialize)]
pub struct RuleInfo {
    /// The rule node (an IRI, or `_:b…` for a blank node).
    pub id: String,
    pub shape: String,
    /// `triple` or `sparql`.
    pub kind: &'static str,
    pub order: f64,
    pub label: Option<String>,
    pub comment: Option<String>,
    pub deactivated: bool,
    pub conditions: Vec<String>,
    /// For a triple rule: the subject, predicate and object expressions.
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
    /// For a SPARQL rule: the CONSTRUCT query.
    pub construct: Option<String>,
    /// Focus nodes the rule ran on in the last round, after its conditions.
    pub focus_nodes: usize,
    /// Focus nodes its conditions turned away in the last round.
    pub skipped: usize,
    /// Triples it produced that were new.
    pub inferred: usize,
    /// Distinct triples it produced that the data graph already stated.
    pub existing: usize,
    pub errors: Vec<String>,
}

/// One inferred triple, with where it came from.
#[derive(Debug, Clone)]
pub struct Inferred {
    pub triple: Triple,
    /// Index into [`Outcome::rules`].
    pub rule: usize,
    pub focus: Term,
    /// The round it was first inferred in, from 1.
    pub iteration: usize,
}

pub struct Outcome {
    pub rules: Vec<RuleInfo>,
    pub inferred: Vec<Inferred>,
    pub iterations: usize,
    /// Set when a limit cut the run short.
    pub stopped: Option<String>,
    /// Problems with the shapes graph that are not tied to one rule.
    pub warnings: Vec<String>,
    /// The data graph with every inferred triple added.
    pub graph: Graph,
    pub millis: u128,
}

enum Body {
    Triple {
        subject: Expr,
        predicate: Expr,
        object: Expr,
    },
    Sparql(Query),
}

struct Rule {
    shape: Term,
    body: Option<Body>,
    deactivated: bool,
    conditions: Vec<Term>,
}

/// Run every rule in `shapes` over `data`.
pub fn infer(data: &Graph, shapes: &Graph, opts: &Options) -> Result<Outcome> {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let (rules, mut infos) = collect(shapes, &mut warnings);
    let mut graph = data.clone();
    let mut store = Store::new(data, shapes);
    let mut inferred: Vec<Inferred> = Vec::new();
    let mut stopped = None;
    let mut iterations = 0;
    let mut stated: Vec<HashSet<Triple>> = vec![HashSet::new(); rules.len()];
    'rounds: loop {
        iterations += 1;
        let before = inferred.len();
        for (i, rule) in rules.iter().enumerate() {
            let info = &mut infos[i];
            if rule.deactivated {
                continue;
            }
            let Some(body) = &rule.body else { continue };
            if opts.deadline.is_some_and(|d| Instant::now() > d) {
                anyhow::bail!("the rules took longer than this server allows");
            }
            let produced = {
                let checker = Checker::new(&graph, shapes)?;
                let cx = Context {
                    graph: &graph,
                    checker: &checker,
                    store: &store,
                };
                run_rule(rule, body, &cx, info)
            };
            for (focus, t) in produced {
                if graph.insert(&t) {
                    store.insert(&t);
                    info.inferred += 1;
                    inferred.push(Inferred {
                        triple: t,
                        rule: i,
                        focus,
                        iteration: iterations,
                    });
                    if inferred.len() >= opts.max_inferred {
                        stopped = Some(format!(
                            "stopped after {} inferred triples, the most this run allows",
                            opts.max_inferred
                        ));
                        break 'rounds;
                    }
                } else if data.contains(&t) && stated[i].insert(t) {
                    info.existing += 1;
                }
            }
        }
        if !opts.iterate || inferred.len() == before {
            break;
        }
        if iterations >= opts.max_iterations {
            stopped = Some(format!(
                "stopped after {iterations} rounds without reaching a fixpoint; rules that \
                 create blank nodes or new values each time never settle"
            ));
            break;
        }
    }
    Ok(Outcome {
        rules: infos,
        inferred,
        iterations,
        stopped,
        warnings,
        graph,
        millis: started.elapsed().as_millis(),
    })
}

/// Run one rule on all its focus nodes, returning (focus node, triple)
/// pairs. Errors are recorded on the rule rather than ending the run.
fn run_rule(
    rule: &Rule,
    body: &Body,
    cx: &Context<'_>,
    info: &mut RuleInfo,
) -> Vec<(Term, Triple)> {
    let mut out = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut focus_nodes = focus_nodes(&rule.shape, cx, &mut errors);
    let total = focus_nodes.len();
    if !rule.conditions.is_empty() {
        focus_nodes.retain(|f| {
            rule.conditions
                .iter()
                .all(|c| match cx.checker.conforms(c, f) {
                    Ok(ok) => ok,
                    Err(e) => {
                        errors.push(format!("condition {c}: {e:#}"));
                        false
                    }
                })
        });
    }
    info.focus_nodes = focus_nodes.len();
    info.skipped = total - focus_nodes.len();
    for focus in focus_nodes {
        let result = match body {
            Body::Triple {
                subject,
                predicate,
                object,
            } => triple_rule(cx, &focus, subject, predicate, object),
            Body::Sparql(q) => sparql_rule(cx, rule, &focus, q),
        };
        match result {
            Ok(ts) => out.extend(ts.into_iter().map(|t| (focus.clone(), t))),
            Err(e) => errors.push(format!("on {focus}: {e:#}")),
        }
    }
    for e in errors {
        if !info.errors.contains(&e) && info.errors.len() < 20 {
            info.errors.push(e);
        }
    }
    out
}

fn triple_rule(
    cx: &Context<'_>,
    focus: &Term,
    s: &Expr,
    p: &Expr,
    o: &Expr,
) -> Result<Vec<Triple>> {
    let parts = [s.eval(cx, focus)?, p.eval(cx, focus)?, o.eval(cx, focus)?];
    Ok(cartesian(&parts)
        .into_iter()
        .filter_map(|v| {
            let subject: NamedOrBlankNode = match &v[0] {
                Term::NamedNode(n) => n.clone().into(),
                Term::BlankNode(b) => b.clone().into(),
                Term::Literal(_) => return None,
            };
            let Term::NamedNode(predicate) = &v[1] else {
                return None;
            };
            Some(Triple::new(subject, predicate.clone(), v[2].clone()))
        })
        .collect())
}

fn sparql_rule(cx: &Context<'_>, rule: &Rule, focus: &Term, q: &Query) -> Result<Vec<Triple>> {
    let bindings = [
        ("this", focus.clone()),
        ("currentShape", rule.shape.clone()),
        (
            "shapesGraph",
            NamedNode::new_unchecked(sparql::SHAPES_GRAPH).into(),
        ),
    ];
    match sparql::run(cx.store, q, &bindings)? {
        Answer::Triples(ts) => Ok(ts),
        _ => anyhow::bail!("sh:construct must be a CONSTRUCT query"),
    }
}

/// The focus nodes of a shape: its SHACL Core targets plus SPARQL-based
/// targets (`sh:target [ a sh:SPARQLTarget ; sh:select … ]`).
fn focus_nodes(shape: &Term, cx: &Context<'_>, errors: &mut Vec<String>) -> Vec<Term> {
    let mut out = cx.checker.focus_nodes(shape);
    let shapes = cx.checker.shapes();
    let targets = objects(shapes, shape, sh::TARGET);
    if targets.is_empty() {
        return out;
    }
    let mut seen: HashSet<Term> = out.iter().cloned().collect();
    for t in targets {
        let Some(text) = object(shapes, &t, sh::SELECT).and_then(|s| str_value(&s)) else {
            errors.push(format!(
                "target {t}: only SPARQL-based targets with sh:select are supported"
            ));
            continue;
        };
        let result = sparql::parse(&text, &prefixes(shapes, &t))
            .and_then(|q| sparql::run(cx.store, &q, &[]));
        match result {
            Ok(Answer::Rows(rows)) => {
                for row in rows {
                    if let Some((_, n)) = row.into_iter().find(|(v, _)| v == "this")
                        && seen.insert(n.clone())
                    {
                        out.push(n);
                    }
                }
            }
            Ok(_) => errors.push(format!("target {t}: sh:select must be a SELECT query")),
            Err(e) => errors.push(format!("target {t}: {e:#}")),
        }
    }
    out
}

fn str_value(t: &Term) -> Option<String> {
    values::str_of(t).map(str::to_string)
}

fn number(t: &Term) -> Option<f64> {
    values::str_of(t)?.trim().parse().ok()
}

/// Find every `sh:rule` in the shapes graph, in execution order.
fn collect(shapes: &Graph, warnings: &mut Vec<String>) -> (Vec<Rule>, Vec<RuleInfo>) {
    let mut rules: Vec<(Rule, RuleInfo)> = shapes
        .triples_for_predicate(sh::RULE)
        .map(|t| parse_rule(shapes, t.subject.into_owned().into(), t.object.into_owned()))
        .collect();
    // By sh:order, then by shape and by rule label (or name), so a run is
    // repeatable: blank node names change every time the shapes are parsed.
    rules.sort_by(|(_, a), (_, b)| {
        a.order
            .partial_cmp(&b.order)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.shape.cmp(&b.shape))
            .then_with(|| sort_name(a).cmp(&sort_name(b)))
    });
    let shape_count = rules
        .iter()
        .map(|r| &r.0.shape)
        .collect::<HashSet<_>>()
        .len();
    if rules.is_empty() {
        warnings.push("the shapes graph has no sh:rule, so there is nothing to infer".to_string());
    } else if shape_count > 0 {
        for (rule, info) in &rules {
            if !has_targets(shapes, &rule.shape) && info.errors.is_empty() {
                warnings.push(format!(
                    "{} has rules but no targets, so they never run; add sh:targetClass, \
                     sh:targetNode, sh:targetSubjectsOf, sh:targetObjectsOf or sh:target",
                    rule.shape
                ));
            }
        }
        warnings.dedup();
    }
    rules.into_iter().unzip()
}

fn sort_name(r: &RuleInfo) -> String {
    match (&r.label, r.id.starts_with("_:")) {
        (Some(l), _) => l.clone(),
        (None, false) => r.id.clone(),
        // Unlabelled blank nodes: by what they do.
        (None, true) => format!(
            "~{}{}{}{}",
            r.subject.as_deref().unwrap_or(""),
            r.predicate.as_deref().unwrap_or(""),
            r.object.as_deref().unwrap_or(""),
            r.construct.as_deref().unwrap_or("")
        ),
    }
}

fn has_targets(g: &Graph, shape: &Term) -> bool {
    [
        sh::TARGET_NODE,
        sh::TARGET_CLASS,
        sh::TARGET_SUBJECTS_OF,
        sh::TARGET_OBJECTS_OF,
        sh::TARGET,
    ]
    .iter()
    .any(|p| object(g, shape, *p).is_some())
        || objects(g, shape, rdf::TYPE).contains(&rdfs::CLASS.into())
}

fn parse_rule(g: &Graph, shape: Term, id: Term) -> (Rule, RuleInfo) {
    let types = objects(g, &id, rdf::TYPE);
    let text = |p| object(g, &id, p).and_then(|t| str_value(&t));
    let is_sparql = types.contains(&sh::SPARQL_RULE.into())
        || (!types.contains(&sh::TRIPLE_RULE.into()) && object(g, &id, sh::CONSTRUCT).is_some());
    let mut info = RuleInfo {
        id: id.to_string(),
        shape: shape.to_string(),
        kind: if is_sparql { "sparql" } else { "triple" },
        order: object(g, &id, sh::ORDER)
            .and_then(|o| number(&o))
            .unwrap_or(0.0),
        label: text(rdfs::LABEL),
        comment: text(rdfs::COMMENT),
        deactivated: object(g, &id, sh::DEACTIVATED)
            .is_some_and(|t| values::str_of(&t) == Some("true")),
        conditions: objects(g, &id, sh::CONDITION)
            .iter()
            .map(Term::to_string)
            .collect(),
        subject: None,
        predicate: None,
        object: None,
        construct: None,
        focus_nodes: 0,
        skipped: 0,
        inferred: 0,
        existing: 0,
        errors: Vec::new(),
    };
    let body = if as_subject(&id).is_none() {
        info.errors
            .push("a rule must be an IRI or a blank node".to_string());
        None
    } else if is_sparql {
        info.construct = text(sh::CONSTRUCT);
        match &info.construct {
            None => {
                info.errors
                    .push("a SPARQL rule needs sh:construct".to_string());
                None
            }
            Some(q) => match sparql::parse(q, &prefixes(g, &id))
                .context("invalid SPARQL in sh:construct")
            {
                Ok(q @ Query::Construct { .. }) => Some(Body::Sparql(q)),
                Ok(_) => {
                    info.errors
                        .push("sh:construct must be a CONSTRUCT query".to_string());
                    None
                }
                Err(e) => {
                    info.errors.push(format!("{e:#}"));
                    None
                }
            },
        }
    } else {
        let mut part = |p, what: &str| -> Option<Expr> {
            let Some(node) = object(g, &id, p) else {
                info.errors.push(format!("a triple rule needs {what}"));
                return None;
            };
            match Expr::parse(g, &node) {
                Ok(e) => Some(e),
                Err(e) => {
                    info.errors.push(format!("{what}: {e:#}"));
                    None
                }
            }
        };
        let s = part(sh::SUBJECT, "sh:subject");
        let p = part(sh::PREDICATE, "sh:predicate");
        let o = part(sh::OBJECT, "sh:object");
        info.subject = s.as_ref().map(Expr::describe);
        info.predicate = p.as_ref().map(Expr::describe);
        info.object = o.as_ref().map(Expr::describe);
        match (s, p, o) {
            (Some(subject), Some(predicate), Some(object)) => Some(Body::Triple {
                subject,
                predicate,
                object,
            }),
            _ => None,
        }
    };
    let rule = Rule {
        conditions: objects(g, &id, sh::CONDITION),
        shape,
        body,
        deactivated: info.deactivated,
    };
    (rule, info)
}
