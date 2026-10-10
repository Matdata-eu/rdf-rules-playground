//! A small Turtle writer for validation reports: results are nested in the
//! report as `[ … ]` blocks, paths as `( … )` lists, and IRIs use prefixes.

use super::vocab::sh;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Graph, NamedOrBlankNodeRef, Term, TermRef};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

pub fn write(g: &Graph, prefixes: &[(String, String)]) -> String {
    let mut w = Writer {
        g,
        prefixes: prefixes.to_vec(),
        refs: HashMap::new(),
        done: HashSet::new(),
    };
    for (name, iri) in [
        ("sh", "http://www.w3.org/ns/shacl#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ] {
        if !w.prefixes.iter().any(|(n, _)| n == name) {
            w.prefixes.push((name.to_string(), iri.to_string()));
        }
    }
    for t in g.iter() {
        if let TermRef::BlankNode(b) = t.object {
            *w.refs.entry(b.as_str().to_string()).or_default() += 1;
        }
    }
    let mut out = String::new();
    for (name, iri) in &w.prefixes {
        let _ = writeln!(out, "@prefix {name}: <{iri}> .");
    }
    out.push('\n');
    // The report first, then anything that could not be nested.
    let mut roots: Vec<NamedOrBlankNodeRef<'_>> = g
        .subjects_for_predicate_object(rdf::TYPE, sh::VALIDATION_REPORT)
        .collect();
    let mut rest: Vec<NamedOrBlankNodeRef<'_>> = Vec::new();
    let mut seen: HashSet<NamedOrBlankNodeRef<'_>> = roots.iter().copied().collect();
    for t in g.iter() {
        if seen.insert(t.subject) {
            rest.push(t.subject);
        }
    }
    // IRIs in order, then blank nodes.
    rest.sort_by_key(|s| (s.is_blank_node(), s.to_string()));
    roots.extend(rest);
    for s in roots {
        if w.done.contains(&s.to_string()) || w.inlinable(s) {
            continue;
        }
        let subject = match s {
            NamedOrBlankNodeRef::BlankNode(_) if !w.refs.contains_key(&key(s)) => "[]".to_string(),
            other => w.term(other.into()),
        };
        w.done.insert(s.to_string());
        let body = w.properties(s, 1);
        let _ = writeln!(out, "{subject}{body} .\n");
    }
    out
}

fn key(s: NamedOrBlankNodeRef<'_>) -> String {
    match s {
        NamedOrBlankNodeRef::BlankNode(b) => b.as_str().to_string(),
        NamedOrBlankNodeRef::NamedNode(n) => n.as_str().to_string(),
    }
}

/// Order of properties within a node: type, the result fields in reading
/// order, then the rest.
fn rank(p: &str) -> usize {
    const ORDER: &[&str] = &[
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/shacl#conforms",
        "http://www.w3.org/ns/shacl#focusNode",
        "http://www.w3.org/ns/shacl#resultPath",
        "http://www.w3.org/ns/shacl#value",
        "http://www.w3.org/ns/shacl#resultSeverity",
        "http://www.w3.org/ns/shacl#resultMessage",
        "http://www.w3.org/ns/shacl#sourceShape",
        "http://www.w3.org/ns/shacl#sourceConstraintComponent",
        "http://www.w3.org/ns/shacl#sourceConstraint",
        "http://www.w3.org/ns/shacl#result",
    ];
    ORDER
        .iter()
        .position(|o| *o == p)
        .unwrap_or(ORDER.len() - 1)
}

struct Writer<'a> {
    g: &'a Graph,
    prefixes: Vec<(String, String)>,
    refs: HashMap<String, usize>,
    done: HashSet<String>,
}

impl Writer<'_> {
    /// A blank node used exactly once as an object is written in place.
    /// Blank nodes without triples here (shapes, data nodes) keep their label.
    fn inlinable(&self, s: NamedOrBlankNodeRef<'_>) -> bool {
        matches!(s, NamedOrBlankNodeRef::BlankNode(_))
            && self.refs.get(&key(s)) == Some(&1)
            && self.g.triples_for_subject(s).next().is_some()
    }

    fn properties(&mut self, s: NamedOrBlankNodeRef<'_>, depth: usize) -> String {
        let mut triples: Vec<_> = self.g.triples_for_subject(s).collect();
        triples.sort_by(|a, b| {
            rank(a.predicate.as_str())
                .cmp(&rank(b.predicate.as_str()))
                .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
                .then_with(|| self.sort_key(a.object).cmp(&self.sort_key(b.object)))
        });
        let indent = "    ".repeat(depth);
        let mut out = String::new();
        let mut last_p: Option<String> = None;
        for t in triples {
            let p = t.predicate.as_str().to_string();
            let object = self.object(t.object, depth);
            if last_p.as_deref() == Some(p.as_str()) {
                if object.starts_with('[') {
                    let _ = write!(out, ", {object}");
                } else {
                    let _ = write!(out, " ,\n{indent}    {object}");
                }
            } else {
                let pred = if t.predicate == rdf::TYPE {
                    "a".to_string()
                } else {
                    self.term(t.predicate.into())
                };
                let sep = if last_p.is_some() { " ;" } else { "" };
                let _ = write!(out, "{sep}\n{indent}{pred} {object}");
                last_p = Some(p);
            }
        }
        out
    }

    /// Sort results by focus node, then path, so the file reads in order.
    fn sort_key(&self, o: TermRef<'_>) -> String {
        let TermRef::BlankNode(b) = o else {
            return o.to_string();
        };
        let get = |p| {
            self.g
                .object_for_subject_predicate(b, p)
                .map(|t| t.to_string())
                .unwrap_or_default()
        };
        format!("{}\u{0}{}", get(sh::FOCUS_NODE), get(sh::RESULT_PATH))
    }

    fn object(&mut self, o: TermRef<'_>, depth: usize) -> String {
        if let TermRef::BlankNode(b) = o {
            let s: NamedOrBlankNodeRef<'_> = b.into();
            if self.inlinable(s) && self.done.insert(s.to_string()) {
                if let Some(items) = self.list(o) {
                    let items: Vec<String> = items
                        .into_iter()
                        .map(|t| self.object(t.as_ref(), depth + 1))
                        .collect();
                    return format!("( {} )", items.join(" "));
                }
                let body = self.properties(s, depth + 1);
                if body.is_empty() {
                    return "[]".to_string();
                }
                return format!("[{body}\n{}]", "    ".repeat(depth));
            }
        }
        self.term(o)
    }

    /// The members of a well-formed RDF list starting at `head`.
    fn list(&mut self, head: TermRef<'_>) -> Option<Vec<Term>> {
        let mut items = Vec::new();
        let mut node: Term = head.into_owned();
        let mut nodes = Vec::new();
        while node != Term::from(rdf::NIL) {
            let Term::BlankNode(b) = &node else {
                return None;
            };
            let s: NamedOrBlankNodeRef<'_> = b.as_ref().into();
            if self.g.triples_for_subject(s).count() != 2
                || (!nodes.is_empty() && !self.inlinable(s))
            {
                return None;
            }
            items.push(
                self.g
                    .object_for_subject_predicate(s, rdf::FIRST)?
                    .into_owned(),
            );
            let rest = self
                .g
                .object_for_subject_predicate(s, rdf::REST)?
                .into_owned();
            nodes.push(s.to_string());
            node = rest;
        }
        self.done.extend(nodes);
        Some(items)
    }

    fn term(&self, t: TermRef<'_>) -> String {
        match t {
            TermRef::NamedNode(n) => self.iri(n.as_str()),
            TermRef::BlankNode(b) => format!("_:{}", b.as_str()),
            TermRef::Literal(l) => {
                let dt = l.datatype();
                if dt == xsd::BOOLEAN && (l.value() == "true" || l.value() == "false") {
                    return l.value().to_string();
                }
                if dt == xsd::INTEGER
                    && l.value().bytes().all(|b| b.is_ascii_digit())
                    && !l.value().is_empty()
                {
                    return l.value().to_string();
                }
                if l.direction().is_some() {
                    // "text"@lang--dir: written out as is.
                    return l.to_string();
                }
                let quoted = oxrdf::LiteralRef::new_simple_literal(l.value()).to_string();
                if let Some(lang) = l.language() {
                    format!("{quoted}@{lang}")
                } else if dt == xsd::STRING {
                    quoted
                } else {
                    format!("{quoted}^^{}", self.iri(dt.as_str()))
                }
            }
            TermRef::Triple(t) => format!(
                "<<( {} {} {} )>>",
                self.term(t.subject.as_ref().into()),
                self.iri(t.predicate.as_str()),
                self.term(t.object.as_ref())
            ),
        }
    }

    fn iri(&self, iri: &str) -> String {
        let best = self
            .prefixes
            .iter()
            .filter(|(_, base)| iri.starts_with(base.as_str()))
            .max_by_key(|(_, base)| base.len());
        if let Some((name, base)) = best {
            let local = &iri[base.len()..];
            let safe = local
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                && !local.starts_with('-');
            if safe {
                return format!("{name}:{local}");
            }
        }
        format!("<{iri}>")
    }
}
