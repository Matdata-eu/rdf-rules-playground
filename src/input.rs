//! Reading RDF: files from the command line (any common syntax, optionally
//! gzipped) and text typed in the playground (Turtle), with error positions
//! the editor can point at.

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use flate2::read::MultiGzDecoder;
use oxrdf::{Graph, Triple};
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// RDF syntaxes the tool reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    /// N-Triples (.nt)
    Nt,
    /// Turtle (.ttl)
    Ttl,
    /// RDF/XML (.rdf, .owl, .xml)
    #[value(alias = "xml")]
    Rdf,
    /// TriG (.trig) — all graphs are merged
    Trig,
    /// N-Quads (.nq) — all graphs are merged
    Nq,
}

/// Detect the syntax of a file from its extension, ignoring a trailing `.gz`.
pub fn detect_format(path: &Path) -> Result<InputFormat> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("path has no filename: {}", path.display()))?
        .to_ascii_lowercase();
    let stem = name.strip_suffix(".gz").unwrap_or(&name);
    let ext = stem.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "nt" | "ntriples" => Ok(InputFormat::Nt),
        "ttl" | "turtle" | "shacl" => Ok(InputFormat::Ttl),
        "rdf" | "owl" | "xml" => Ok(InputFormat::Rdf),
        "trig" => Ok(InputFormat::Trig),
        "nq" | "nquads" => Ok(InputFormat::Nq),
        "" => bail!(
            "could not detect RDF format for {} (no extension); pass --format",
            path.display()
        ),
        other => bail!("unknown RDF extension '.{other}' for {}", path.display()),
    }
}

/// A parse error with the position the editor should highlight.
#[derive(Debug, Clone, Serialize)]
pub struct SyntaxError {
    pub message: String,
    /// 1-based line and column of the error.
    pub line: u64,
    pub column: u64,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for SyntaxError {}

fn turtle_error(e: &oxttl::TurtleSyntaxError) -> SyntaxError {
    let start = e.location().start;
    SyntaxError {
        message: e.message().to_string(),
        line: start.line + 1,
        column: start.column + 1,
    }
}

/// Parse `reader` and call `on_triple` for every statement. Quad formats are
/// flattened. Returns the prefixes declared in Turtle and TriG sources.
pub fn parse_triples<R: BufRead, F: FnMut(Triple)>(
    reader: R,
    format: InputFormat,
    mut on_triple: F,
) -> Result<Vec<(String, String)>> {
    let mut prefixes = Vec::new();
    let turtle = |e: oxttl::TurtleParseError| match e {
        oxttl::TurtleParseError::Syntax(s) => anyhow::Error::new(turtle_error(&s)),
        oxttl::TurtleParseError::Io(io) => anyhow::Error::new(io),
    };
    match format {
        InputFormat::Nt => {
            for t in oxttl::NTriplesParser::new().for_reader(reader) {
                on_triple(t.map_err(turtle)?);
            }
        }
        InputFormat::Ttl => {
            let mut parser = oxttl::TurtleParser::new().for_reader(reader);
            for t in parser.by_ref() {
                on_triple(t.map_err(turtle)?);
            }
            prefixes.extend(
                parser
                    .prefixes()
                    .map(|(k, v)| (k.to_string(), v.to_string())),
            );
        }
        InputFormat::Rdf => {
            for t in oxrdfxml::RdfXmlParser::new().for_reader(reader) {
                on_triple(t.context("RDF/XML parse error")?);
            }
        }
        InputFormat::Trig => {
            let mut parser = oxttl::TriGParser::new().for_reader(reader);
            for q in parser.by_ref() {
                let q = q.map_err(turtle)?;
                on_triple(Triple::new(q.subject, q.predicate, q.object));
            }
            prefixes.extend(
                parser
                    .prefixes()
                    .map(|(k, v)| (k.to_string(), v.to_string())),
            );
        }
        InputFormat::Nq => {
            for q in oxttl::NQuadsParser::new().for_reader(reader) {
                let q = q.map_err(turtle)?;
                on_triple(Triple::new(q.subject, q.predicate, q.object));
            }
        }
    }
    Ok(prefixes)
}

/// Add prefixes not declared yet; the first declaration of a name wins.
pub fn merge_prefixes(prefixes: &mut Vec<(String, String)>, found: Vec<(String, String)>) {
    for (name, iri) in found {
        if !prefixes.iter().any(|(n, _)| *n == name) {
            prefixes.push((name, iri));
        }
    }
}

/// Load files into `graph`. `-` reads standard input.
pub fn load_files<P: AsRef<Path>>(
    paths: &[P],
    format: Option<InputFormat>,
    graph: &mut Graph,
    prefixes: &mut Vec<(String, String)>,
) -> Result<()> {
    for path in paths {
        let path = path.as_ref();
        let (reader, fmt): (Box<dyn BufRead>, InputFormat) = if path == Path::new("-") {
            (
                Box::new(BufReader::new(std::io::stdin())),
                format.unwrap_or(InputFormat::Ttl),
            )
        } else {
            let fmt = match format {
                Some(f) => f,
                None => detect_format(path)?,
            };
            let file =
                File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
            let raw: Box<dyn Read> = if path.to_string_lossy().to_ascii_lowercase().ends_with(".gz")
            {
                Box::new(MultiGzDecoder::new(file))
            } else {
                Box::new(file)
            };
            (Box::new(BufReader::new(raw)), fmt)
        };
        let found = parse_triples(reader, fmt, |t| {
            graph.insert(&t);
        })
        .with_context(|| format!("failed to parse {}", path.display()))?;
        merge_prefixes(prefixes, found);
    }
    Ok(())
}

/// Parse Turtle typed in the playground. Fails with a [`SyntaxError`] when
/// the text does not parse, or when it holds more than `max_triples`.
pub fn parse_turtle(
    text: &str,
    max_triples: Option<usize>,
) -> std::result::Result<(Graph, Vec<(String, String)>), SyntaxError> {
    let mut graph = Graph::new();
    let mut parser = oxttl::TurtleParser::new().for_slice(text.as_bytes());
    for t in parser.by_ref() {
        let t = t.map_err(|e| turtle_error(&e))?;
        graph.insert(&t);
        if max_triples.is_some_and(|m| graph.len() > m) {
            return Err(SyntaxError {
                message: format!(
                    "more than {} triples, the most this server accepts",
                    max_triples.unwrap_or_default()
                ),
                line: 1,
                column: 1,
            });
        }
    }
    let prefixes = parser
        .prefixes()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Ok((graph, prefixes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_extensions() {
        assert_eq!(detect_format(Path::new("a.ttl")).unwrap(), InputFormat::Ttl);
        assert_eq!(
            detect_format(Path::new("a.nt.gz")).unwrap(),
            InputFormat::Nt
        );
        assert_eq!(detect_format(Path::new("a.owl")).unwrap(), InputFormat::Rdf);
        assert!(detect_format(Path::new("a.foo")).is_err());
    }

    #[test]
    fn reports_error_position() {
        let err = parse_turtle("@prefix ex: <http://ex/> .\nex:a ex:b .\n", None).unwrap_err();
        assert_eq!(err.line, 2);
    }
}
