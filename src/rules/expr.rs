//! SHACL-AF node expressions: the values of `sh:subject`, `sh:predicate`
//! and `sh:object` in a `sh:TripleRule`. Each one evaluates, for a focus
//! node, to a list of RDF terms.

use super::sparql::{self, Answer, Store};
use crate::validate::sparql::prefixes;
use crate::validate::values;
use crate::validate::vocab::sh;
use crate::validate::{Checker, ShaclPath, list, object, objects};
use anyhow::{Context as _, Result, bail};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Graph, Literal, Term};
use spargebra::Query;
use std::cmp::Ordering;
use std::collections::HashSet;
use oxsdatatypes::Decimal;
use std::rc::Rc;
use std::str::FromStr;

pub enum Expr {
    /// `sh:this`: the focus node.
    This,
    /// An IRI or literal, standing for itself.
    Constant(Term),
    /// `[ sh:path p ; sh:nodes e ]`: the values of `p` from each node of `e`
    /// (the focus node when `sh:nodes` is absent).
    Path {
        path: ShaclPath,
        nodes: Option<Box<Expr>>,
    },
    /// `[ sh:filterShape s ; sh:nodes e ]`: the nodes of `e` that conform to `s`.
    Filter { shape: Term, nodes: Box<Expr> },
    Intersection(Vec<Expr>),
    Union(Vec<Expr>),
    Count(Box<Expr>),
    Min(Box<Expr>),
    Max(Box<Expr>),
    Sum(Box<Expr>),
    /// `( fn arg1 arg2 … )`: a call of a `sh:SPARQLFunction`.
    Function { function: Rc<Function>, args: Vec<Expr> },
}

/// A `sh:SPARQLFunction` declared in the shapes graph.
pub struct Function {
    pub id: Term,
    /// Parameter variable names, in `sh:order`.
    params: Vec<String>,
    query: Query,
}

impl Function {
    fn parse(g: &Graph, id: &Term) -> Result<Self> {
        if !objects(g, id, rdf::TYPE).contains(&sh::SPARQL_FUNCTION.into()) {
            bail!("{id} is not declared as a sh:SPARQLFunction in the shapes graph");
        }
        let text = object(g, id, sh::SELECT)
            .or_else(|| object(g, id, sh::ASK))
            .and_then(|t| values::str_of(&t).map(str::to_string))
            .with_context(|| format!("the function {id} has no sh:select or sh:ask"))?;
        let mut params: Vec<(f64, String)> = Vec::new();
        for p in objects(g, id, sh::PARAMETER) {
            let Some(Term::NamedNode(path)) = object(g, &p, sh::PATH) else {
                bail!("a parameter of {id} has no sh:path IRI");
            };
            let order = object(g, &p, sh::ORDER)
                .and_then(|o| values::str_of(&o).and_then(|s| s.parse().ok()))
                .unwrap_or(0.0);
            params.push((order, local_name(path.as_str()).to_string()));
        }
        params.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        let query = sparql::parse(&text, &prefixes(g, id))
            .with_context(|| format!("invalid SPARQL in the function {id}"))?;
        Ok(Self {
            id: id.clone(),
            params: params.into_iter().map(|(_, n)| n).collect(),
            query,
        })
    }

    fn call(&self, store: &Store, args: &[Term]) -> Result<Option<Term>> {
        let bindings: Vec<(&str, Term)> = self
            .params
            .iter()
            .map(String::as_str)
            .zip(args.iter().cloned())
            .collect();
        Ok(match sparql::run(store, &self.query, &bindings)? {
            Answer::Bool(b) => Some(Literal::from(b).into()),
            Answer::Rows(rows) => {
                let first = sparql::projection(&self.query).into_iter().next();
                rows.into_iter().next().and_then(|row| {
                    row.into_iter()
                        .find(|(n, _)| first.as_deref().is_none_or(|f| f == n))
                        .map(|(_, t)| t)
                })
            }
            Answer::Triples(_) => bail!("a SPARQL function must be a SELECT or ASK query"),
        })
    }
}

/// The local name of an IRI: what a parameter path is called as a variable.
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/', ':']).next().unwrap_or(iri)
}

impl Expr {
    pub fn parse(g: &Graph, node: &Term) -> Result<Expr> {
        Self::parse_depth(g, node, 0)
    }

    fn parse_depth(g: &Graph, node: &Term, depth: usize) -> Result<Expr> {
        if depth > 64 {
            bail!("node expression nested too deeply");
        }
        let sub = |t: &Term| Self::parse_depth(g, t, depth + 1).map(Box::new);
        let many = |t: &Term| -> Result<Vec<Expr>> {
            list(g, t)
                .iter()
                .map(|m| Self::parse_depth(g, m, depth + 1))
                .collect()
        };
        match node {
            t if *t == Term::from(sh::THIS) => return Ok(Expr::This),
            Term::NamedNode(_) | Term::Literal(_) => return Ok(Expr::Constant(node.clone())),
            Term::BlankNode(_) => {}
        }
        if let Some(p) = object(g, node, sh::PATH) {
            return Ok(Expr::Path {
                path: ShaclPath::parse(g, &p)?,
                nodes: object(g, node, sh::NODES).map(|n| sub(&n)).transpose()?,
            });
        }
        if let Some(shape) = object(g, node, sh::FILTER_SHAPE) {
            let nodes = match object(g, node, sh::NODES) {
                Some(n) => sub(&n)?,
                None => Box::new(Expr::This),
            };
            return Ok(Expr::Filter { shape, nodes });
        }
        if let Some(l) = object(g, node, sh::INTERSECTION) {
            return Ok(Expr::Intersection(many(&l)?));
        }
        if let Some(l) = object(g, node, sh::UNION) {
            return Ok(Expr::Union(many(&l)?));
        }
        for (p, make) in [
            (sh::COUNT, Expr::Count as fn(Box<Expr>) -> Expr),
            (sh::MIN, Expr::Min),
            (sh::MAX, Expr::Max),
            (sh::SUM, Expr::Sum),
        ] {
            if let Some(e) = object(g, node, p) {
                return Ok(make(sub(&e)?));
            }
        }
        if object(g, node, rdf::FIRST).is_some() {
            let items = list(g, node);
            let Some((f, args)) = items.split_first() else {
                bail!("empty function expression");
            };
            return Ok(Expr::Function {
                function: Rc::new(Function::parse(g, f)?),
                args: args
                    .iter()
                    .map(|a| Self::parse_depth(g, a, depth + 1))
                    .collect::<Result<_>>()?,
            });
        }
        bail!(
            "{node} is not a node expression (expected sh:this, an IRI, a literal, \
             sh:path, sh:filterShape, sh:intersection, sh:union, sh:count, sh:min, \
             sh:max, sh:sum or a function call list)"
        )
    }

    /// The values of this expression for `focus`, without duplicates.
    pub fn eval(&self, cx: &Context<'_>, focus: &Term) -> Result<Vec<Term>> {
        let mut out = match self {
            Expr::This => vec![focus.clone()],
            Expr::Constant(t) => vec![t.clone()],
            Expr::Path { path, nodes } => {
                let starts = match nodes {
                    Some(n) => n.eval(cx, focus)?,
                    None => vec![focus.clone()],
                };
                let mut out = Vec::new();
                for s in starts {
                    out.extend(path.eval(cx.graph, &s));
                }
                out
            }
            Expr::Filter { shape, nodes } => {
                let mut out = Vec::new();
                for n in nodes.eval(cx, focus)? {
                    if cx.checker.conforms(shape, &n)? {
                        out.push(n);
                    }
                }
                out
            }
            Expr::Intersection(items) => {
                let mut iter = items.iter();
                let mut acc = match iter.next() {
                    Some(e) => e.eval(cx, focus)?,
                    None => Vec::new(),
                };
                for e in iter {
                    let other: HashSet<Term> = e.eval(cx, focus)?.into_iter().collect();
                    acc.retain(|t| other.contains(t));
                }
                acc
            }
            Expr::Union(items) => {
                let mut acc = Vec::new();
                for e in items {
                    acc.extend(e.eval(cx, focus)?);
                }
                acc
            }
            Expr::Count(e) => {
                let n = e.eval(cx, focus)?.len();
                vec![Literal::new_typed_literal(n.to_string(), xsd::INTEGER).into()]
            }
            Expr::Min(e) | Expr::Max(e) => {
                let want = if matches!(self, Expr::Min(_)) {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
                let mut best: Option<Term> = None;
                for t in e.eval(cx, focus)? {
                    best = match best {
                        Some(b) if values::compare(&t, &b) != Some(want) => Some(b),
                        _ => Some(t),
                    };
                }
                best.into_iter().collect()
            }
            Expr::Sum(e) => sum(&e.eval(cx, focus)?).into_iter().collect(),
            Expr::Function { function, args } => {
                let mut values = Vec::new();
                for a in args {
                    values.push(a.eval(cx, focus)?);
                }
                let mut out = Vec::new();
                for combo in cartesian(&values) {
                    if let Some(t) = function
                        .call(cx.store, &combo)
                        .with_context(|| format!("calling {}", function.id))?
                    {
                        out.push(t);
                    }
                }
                out
            }
        };
        let mut seen = HashSet::new();
        out.retain(|t| seen.insert(t.clone()));
        Ok(out)
    }

    /// A short, readable form for the rules overview.
    pub fn describe(&self) -> String {
        let join = |items: &[Expr], sep: &str| {
            items
                .iter()
                .map(Expr::describe)
                .collect::<Vec<_>>()
                .join(sep)
        };
        match self {
            Expr::This => "$this".to_string(),
            Expr::Constant(t) => t.to_string(),
            Expr::Path { path, nodes } => match nodes {
                Some(n) => format!("{}/{}", n.describe(), path.to_sparql()),
                None => format!("$this/{}", path.to_sparql()),
            },
            Expr::Filter { shape, nodes } => {
                format!("{} conforming to {shape}", nodes.describe())
            }
            Expr::Intersection(items) => format!("({})", join(items, " ∩ ")),
            Expr::Union(items) => format!("({})", join(items, " ∪ ")),
            Expr::Count(e) => format!("count({})", e.describe()),
            Expr::Min(e) => format!("min({})", e.describe()),
            Expr::Max(e) => format!("max({})", e.describe()),
            Expr::Sum(e) => format!("sum({})", e.describe()),
            Expr::Function { function, args } => {
                format!("{}({})", function.id, join(args, ", "))
            }
        }
    }
}

/// What a node expression can see while it is evaluated.
pub struct Context<'a> {
    pub graph: &'a Graph,
    pub checker: &'a Checker<'a>,
    pub store: &'a Store,
}

/// Every combination of one value per argument.
pub fn cartesian(values: &[Vec<Term>]) -> Vec<Vec<Term>> {
    let mut out: Vec<Vec<Term>> = vec![Vec::new()];
    for vs in values {
        let mut next = Vec::with_capacity(out.len() * vs.len());
        for prefix in &out {
            for v in vs {
                let mut row = prefix.clone();
                row.push(v.clone());
                next.push(row);
            }
        }
        out = next;
    }
    out
}

/// The numeric sum of `terms`, typed like SPARQL's `+`: integer while every
/// value is an integer, decimal once one is a decimal, double once one is a
/// float or double. Non-numeric values make the sum undefined.
fn sum(terms: &[Term]) -> Option<Term> {
    #[derive(PartialEq, PartialOrd)]
    enum Kind {
        Integer,
        Decimal,
        Double,
    }
    let mut kind = Kind::Integer;
    let mut exact = Decimal::from(0);
    let mut float = 0f64;
    for t in terms {
        let Term::Literal(l) = t else { return None };
        let dt = l.datatype();
        let v = l.value().trim();
        if dt == xsd::DOUBLE || dt == xsd::FLOAT {
            kind = Kind::Double;
            float += v.parse::<f64>().ok()?;
            continue;
        }
        let d = Decimal::from_str(v).ok()?;
        if dt == xsd::DECIMAL {
            if kind < Kind::Decimal {
                kind = Kind::Decimal;
            }
        } else if !INTEGER_TYPES.contains(&dt) {
            return None;
        }
        exact = exact.checked_add(d)?;
        float += v.parse::<f64>().ok()?;
    }
    Some(match kind {
        Kind::Integer => Literal::new_typed_literal(exact.to_string(), xsd::INTEGER).into(),
        Kind::Decimal => Literal::new_typed_literal(exact.to_string(), xsd::DECIMAL).into(),
        Kind::Double => Literal::new_typed_literal(format!("{float:E}"), xsd::DOUBLE).into(),
    })
}

const INTEGER_TYPES: [oxrdf::NamedNodeRef<'_>; 13] = [
    xsd::INTEGER,
    xsd::INT,
    xsd::LONG,
    xsd::SHORT,
    xsd::BYTE,
    xsd::NON_NEGATIVE_INTEGER,
    xsd::POSITIVE_INTEGER,
    xsd::NON_POSITIVE_INTEGER,
    xsd::NEGATIVE_INTEGER,
    xsd::UNSIGNED_INT,
    xsd::UNSIGNED_LONG,
    xsd::UNSIGNED_SHORT,
    xsd::UNSIGNED_BYTE,
];
