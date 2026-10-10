//! Writing inferred triples as Turtle or N-Triples.

use oxrdf::Graph;

/// Readable Turtle using `prefixes`, declaring only the ones it uses.
pub fn turtle(g: &Graph, prefixes: &[(String, String)]) -> String {
    let text = crate::validate::to_turtle(g, prefixes);
    let (head, body) = text.split_once("\n\n").unwrap_or(("", &text));
    let used: Vec<&str> = head
        .lines()
        .filter(|line| {
            line.strip_prefix("@prefix ")
                .and_then(|r| r.split_once(':'))
                .is_some_and(|(name, _)| uses_prefix(body, name))
        })
        .collect();
    if used.is_empty() {
        body.to_string()
    } else {
        format!("{}\n\n{body}", used.join("\n"))
    }
}

fn uses_prefix(body: &str, name: &str) -> bool {
    let needle = format!("{name}:");
    body.match_indices(&needle).any(|(i, _)| {
        // A prefix starts a token: not inside another name or an <iri>.
        i == 0
            || body[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || "([,;^".contains(c))
    })
}

/// N-Triples, one triple per line, sorted.
pub fn ntriples(g: &Graph) -> String {
    let mut lines: Vec<String> = g.iter().map(|t| format!("{t} .")).collect();
    lines.sort();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}
