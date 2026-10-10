//! SPARQL for the rules engine: `sh:SPARQLRule` CONSTRUCT queries,
//! `sh:SPARQLFunction` bodies and `sh:SPARQLTarget` selections, evaluated
//! with Oxigraph's SPARQL engine against the data graph as it grows.

use crate::validate::sparql::{project, substitute_named, substitute_pattern, substitute_term};
use anyhow::{Context, Result, bail};
use oxrdf::{Dataset, GraphNameRef, NamedNode, QuadRef, Term, Triple, Variable};
use spareval::{QueryEvaluationError, QueryEvaluator, QueryResults};
use spargebra::algebra::Expression;
use spargebra::{Query, SparqlParser};
use std::collections::HashMap;

/// The name `$shapesGraph` is bound to. The shapes are queryable as this
/// named graph; the data graph is the default graph.
pub const SHAPES_GRAPH: &str = "urn:x-shacl-af-playground:shapes-graph";

/// The data graph (default graph) plus the shapes graph (named graph),
/// kept in sync with the working graph as rules add triples.
pub struct Store {
    pub dataset: Dataset,
}

impl Store {
    pub fn new(data: &oxrdf::Graph, shapes: &oxrdf::Graph) -> Self {
        let mut dataset = Dataset::new();
        for t in data.iter() {
            dataset.insert(QuadRef::new(
                t.subject,
                t.predicate,
                t.object,
                GraphNameRef::DefaultGraph,
            ));
        }
        let name = NamedNode::new_unchecked(SHAPES_GRAPH);
        for t in shapes.iter() {
            dataset.insert(QuadRef::new(t.subject, t.predicate, t.object, &name));
        }
        Self { dataset }
    }

    pub fn insert(&mut self, t: &Triple) {
        self.dataset.insert(QuadRef::new(
            &t.subject,
            &t.predicate,
            &t.object,
            GraphNameRef::DefaultGraph,
        ));
    }
}

/// Parse a query with the prefixes declared through `sh:prefixes`.
pub fn parse(text: &str, prefixes: &[(String, String)]) -> Result<Query> {
    let mut parser = SparqlParser::new();
    for (name, iri) in prefixes {
        parser = parser
            .with_prefix(name, iri)
            .with_context(|| format!("invalid namespace for prefix {name}:"))?;
    }
    parser.parse_query(text).map_err(|e| anyhow::anyhow!("{e}"))
}

pub enum Answer {
    Triples(Vec<Triple>),
    Rows(Vec<Vec<(String, Term)>>),
    Bool(bool),
}

/// Run `query` with the given variables pre-bound. A pre-bound variable the
/// query does not mention is ignored.
pub fn run(store: &Store, query: &Query, bindings: &[(&str, Term)]) -> Result<Answer> {
    // Blank nodes, and triple terms that may hold them, can only be pre-bound
    // by the evaluator itself, and the optimizer then treats those variables
    // as unbound in places the pattern does not bind them (e.g. a FILTER);
    // run without it.
    let evaluator = if bindings
        .iter()
        .any(|(_, t)| matches!(t, Term::BlankNode(_) | Term::Triple(_)))
    {
        QueryEvaluator::new().without_optimizations()
    } else {
        QueryEvaluator::new()
    };
    // IRIs and literals are written into the query as constants, which is
    // what the optimizer plans best around and works in every position
    // (BIND, FILTER, sub-queries, the CONSTRUCT template).
    let constants: HashMap<&str, Expression> = bindings
        .iter()
        .filter_map(|(n, t)| match t {
            Term::NamedNode(n2) => Some((*n, Expression::NamedNode(n2.clone()))),
            Term::Literal(l) => Some((*n, Expression::Literal(l.clone()))),
            Term::BlankNode(_) | Term::Triple(_) => None,
        })
        .collect();
    let mut query = query.clone();
    let names: Vec<&str> = bindings.iter().map(|(n, _)| *n).collect();
    match &mut query {
        Query::Select { pattern, .. } => {
            substitute_pattern(pattern, &constants);
            // Pre-bound blank nodes must be projected for the evaluator to
            // accept them.
            project(pattern, &names);
        }
        Query::Ask { pattern, .. } | Query::Describe { pattern, .. } => {
            substitute_pattern(pattern, &constants)
        }
        Query::Construct {
            template, pattern, ..
        } => {
            substitute_pattern(pattern, &constants);
            for t in template {
                substitute_term(&mut t.subject, &constants);
                substitute_named(&mut t.predicate, &constants);
                substitute_term(&mut t.object, &constants);
            }
        }
    }
    let query = &query;
    let mut bindings: Vec<(&str, Term)> = bindings
        .iter()
        .filter(|(_, t)| matches!(t, Term::BlankNode(_) | Term::Triple(_)))
        .cloned()
        .collect();
    let results = loop {
        let mut prepared = evaluator.prepare(query);
        for (name, term) in &bindings {
            prepared = prepared.substitute_variable(Variable::new_unchecked(*name), term.clone());
        }
        match prepared.execute(&store.dataset) {
            Err(QueryEvaluationError::NotExistingSubstitutedVariable(v)) => {
                let before = bindings.len();
                bindings.retain(|(n, _)| *n != v.as_str());
                if bindings.len() == before {
                    bail!("cannot pre-bind ?{}", v.as_str());
                }
            }
            other => break other.map_err(|e| anyhow::anyhow!("{e}"))?,
        }
    };
    Ok(match results {
        QueryResults::Boolean(b) => Answer::Bool(b),
        QueryResults::Solutions(rows) => {
            let mut out = Vec::new();
            for row in rows {
                let row = row.map_err(|e| anyhow::anyhow!("{e}"))?;
                out.push(
                    row.iter()
                        .map(|(v, t)| (v.as_str().to_string(), t.clone()))
                        .collect(),
                );
            }
            Answer::Rows(out)
        }
        QueryResults::Graph(triples) => {
            let mut out = Vec::new();
            for t in triples {
                out.push(t.map_err(|e| anyhow::anyhow!("{e}"))?);
            }
            Answer::Triples(out)
        }
    })
}

/// The variables a SELECT query projects, in order.
pub fn projection(query: &Query) -> Vec<String> {
    fn find(p: &spargebra::algebra::GraphPattern) -> Vec<String> {
        use spargebra::algebra::GraphPattern as G;
        match p {
            G::Project { variables, .. } => {
                variables.iter().map(|v| v.as_str().to_string()).collect()
            }
            G::Distinct { inner }
            | G::Reduced { inner }
            | G::Slice { inner, .. }
            | G::OrderBy { inner, .. } => find(inner),
            _ => Vec::new(),
        }
    }
    match query {
        Query::Select { pattern, .. } => find(pattern),
        _ => Vec::new(),
    }
}
