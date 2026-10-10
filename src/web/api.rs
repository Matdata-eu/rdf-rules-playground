//! HTTP routes.

use super::assets::{self, EXAMPLES};
use super::{AppState, Run, ServerData};
use crate::input::{SyntaxError, load_bytes, merge_prefixes, parse_turtle};
use crate::output;
use crate::rules::{self, Options, RuleInfo};
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use oxrdf::{Dataset, Graph};
use serde::{Deserialize, Serialize};
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use spareval::{QueryEvaluator, QueryResults};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Most inferred triples listed in a response; the rest are only counted.
/// Downloads and the SPARQL endpoint still have all of them.
const MAX_ROWS: usize = 20_000;

pub fn router(state: AppState) -> Router {
    let body_limit = state
        .limits
        .as_ref()
        .map_or(256 * 1024 * 1024, |l| l.max_input_bytes * 2 + 64 * 1024);
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(|| async { "ok" }))
        .route("/assets/*path", get(asset))
        .route("/api/meta", get(meta))
        .route("/api/examples", get(examples))
        .route(
            "/api/infer",
            post(infer).layer(DefaultBodyLimit::max(body_limit)),
        )
        .route(
            "/api/data",
            post(upload).layer(DefaultBodyLimit::max(body_limit)),
        )
        .route(
            "/api/data/:id/sparql",
            get(data_sparql_get).post(data_sparql_post),
        )
        .route("/api/runs/:id/sparql", get(sparql_get).post(sparql_post))
        .route("/licenses", get(licenses))
        .route("/api/runs/:id/download", get(download))
        .with_state(state)
}

async fn root() -> Response {
    match assets::lookup("/assets/app/index.html") {
        Some(a) => Html(std::str::from_utf8(a.bytes).unwrap_or("")).into_response(),
        None => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}

async fn asset(Path(path): Path<String>) -> Response {
    match assets::lookup(&format!("/assets/{path}")) {
        Some(a) => (
            [
                (header::CONTENT_TYPE, a.mime),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            a.bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn licenses() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        assets::THIRD_PARTY_NOTICES,
    )
        .into_response()
}

#[derive(Serialize)]
struct Meta {
    version: &'static str,
    public: bool,
    max_input_bytes: Option<usize>,
    max_iterations: Option<usize>,
    max_editor_triples: usize,
    preload: Option<Preload>,
    /// Data files given on the command line, too big for the editor.
    server_data: Option<DataInfo>,
}

/// A data graph the server holds, as the playground shows it.
#[derive(Serialize)]
struct DataInfo {
    id: String,
    files: Vec<String>,
    triples: usize,
    prefixes: Vec<(String, String)>,
}

impl From<&ServerData> for DataInfo {
    fn from(d: &ServerData) -> Self {
        Self {
            id: d.id.clone(),
            files: d.files.clone(),
            triples: d.graph.len(),
            prefixes: d.prefixes.clone(),
        }
    }
}

#[derive(Serialize)]
struct Preload {
    data: String,
    shapes: String,
}

async fn meta(State(s): State<AppState>) -> Json<Meta> {
    Json(Meta {
        version: env!("CARGO_PKG_VERSION"),
        public: s.limits.is_some(),
        max_input_bytes: s.limits.as_ref().map(|l| l.max_input_bytes),
        max_iterations: s.limits.as_ref().map(|l| l.max_iterations),
        max_editor_triples: s.max_editor_triples,
        server_data: s.startup_data.as_deref().map(DataInfo::from),
        preload: s.preload.as_ref().map(|(d, sh)| Preload {
            data: d.clone(),
            shapes: sh.clone(),
        }),
    })
}

#[derive(Serialize)]
struct ExampleDto {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    data: &'static str,
    shapes: &'static str,
}

async fn examples() -> Json<Vec<ExampleDto>> {
    Json(
        EXAMPLES
            .iter()
            .map(|e| ExampleDto {
                id: e.id,
                title: e.title,
                description: e.description,
                data: e.data,
                shapes: e.shapes,
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct UploadParams {
    /// The file name, which tells its syntax.
    name: String,
}

/// What an opened file becomes: editor text, or a graph the server keeps.
#[derive(Serialize)]
struct UploadResponse {
    text: Option<String>,
    data: Option<DataInfo>,
}

/// `POST /api/data?name=FILE`: parse a file opened in the browser. Small
/// graphs come back as Turtle for the editor; bigger ones stay on the server.
async fn upload(State(s): State<AppState>, Query(p): Query<UploadParams>, body: Bytes) -> Response {
    let Ok(_permit) = s.jobs.clone().acquire_owned().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    };
    let state = s.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut g = Graph::new();
        let mut prefixes = Vec::new();
        let max_triples = state.limits.as_ref().map(|l| l.max_triples);
        load_bytes(&p.name, &body, &mut g, &mut prefixes, max_triples)
            .map_err(|e| format!("{e:#}"))?;
        if g.len() <= state.max_editor_triples {
            return Ok::<_, String>(UploadResponse {
                text: Some(output::turtle(&g, &prefixes)),
                data: None,
            });
        }
        let data = state.keep_upload(ServerData::new(vec![p.name], g, prefixes));
        Ok(UploadResponse {
            text: None,
            data: Some(DataInfo::from(&*data)),
        })
    });
    match task.await {
        Ok(Ok(r)) => Json(r).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "reading the file crashed",
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InferRequest {
    /// The data editor's text; ignored when `data_id` is given.
    #[serde(default)]
    data: String,
    /// A data graph the server holds, instead of the editor's text.
    data_id: Option<String>,
    shapes: String,
    /// Also read rules from the data editor.
    #[serde(default)]
    shapes_in_data: bool,
    #[serde(default = "yes")]
    iterate: bool,
    max_iterations: Option<usize>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Default)]
struct SyntaxErrors {
    data: Option<SyntaxError>,
    shapes: Option<SyntaxError>,
}

#[derive(Serialize)]
struct InferredDto {
    s: String,
    p: String,
    o: String,
    rule: usize,
    focus: String,
    iteration: usize,
}

#[derive(Serialize, Default)]
struct InferResponse {
    run: Option<String>,
    syntax: SyntaxErrors,
    error: Option<String>,
    iterations: usize,
    stopped: Option<String>,
    warnings: Vec<String>,
    millis: u128,
    rules: Vec<RuleInfo>,
    /// The first [`MAX_ROWS`] inferred triples.
    inferred: Vec<InferredDto>,
    /// How many triples were inferred in all.
    inferred_total: usize,
    prefixes: Vec<(String, String)>,
    /// The inferred triples as Turtle, unless there are more than [`MAX_ROWS`].
    turtle: Option<String>,
    data_triples: usize,
    shapes_triples: usize,
}

async fn infer(State(s): State<AppState>, Json(req): Json<InferRequest>) -> Response {
    if let Some(l) = &s.limits
        && (req.data.len() > l.max_input_bytes || req.shapes.len() > l.max_input_bytes)
    {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "each editor may hold up to {} KB on this server",
                l.max_input_bytes / 1024
            ),
        )
            .into_response();
    }
    let Ok(_permit) = s.jobs.clone().acquire_owned().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    };
    let state = s.clone();
    match tokio::task::spawn_blocking(move || run_infer(&state, req)).await {
        Ok(r) => Json(r).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "the run crashed").into_response(),
    }
}

fn run_infer(s: &AppState, req: InferRequest) -> InferResponse {
    let max_triples = s.limits.as_ref().map(|l| l.max_triples);
    let mut out = InferResponse::default();
    let data = match &req.data_id {
        Some(id) => match s.data(id) {
            Some(d) => Ok((d.graph.clone(), d.prefixes.clone())),
            None => {
                out.error = Some(
                    "the server no longer holds this data graph; open the file again".to_string(),
                );
                return out;
            }
        },
        None => parse_turtle(&req.data, max_triples).map(|(g, p)| (Arc::new(g), p)),
    };
    let shapes = parse_turtle(&req.shapes, max_triples);
    let (data, shapes) = match (data, shapes) {
        (Ok(d), Ok(sh)) => (d, sh),
        (d, sh) => {
            out.syntax = SyntaxErrors {
                data: d.err(),
                shapes: sh.err(),
            };
            return out;
        }
    };
    let (data, mut prefixes) = data;
    let (mut shapes, shape_prefixes) = shapes;
    merge_prefixes(&mut prefixes, shape_prefixes);
    if req.shapes_in_data {
        shapes.extend(data.iter());
    }
    out.data_triples = data.len();
    out.shapes_triples = shapes.len();
    let mut opts = Options {
        iterate: req.iterate,
        max_iterations: req.max_iterations.unwrap_or(100).clamp(1, 1000),
        ..Options::default()
    };
    if let Some(l) = &s.limits {
        opts.max_iterations = opts.max_iterations.min(l.max_iterations);
        opts.max_inferred = l.max_triples;
        opts.deadline = Some(Instant::now() + l.max_time);
    }
    let outcome = match rules::infer(&data, &shapes, &opts) {
        Ok(o) => o,
        Err(e) => {
            out.error = Some(format!("{e:#}"));
            return out;
        }
    };
    let inferred: Graph = outcome.inferred.iter().map(|i| i.triple.clone()).collect();
    out.inferred_total = outcome.inferred.len();
    if inferred.len() <= MAX_ROWS {
        out.turtle = Some(output::turtle(&inferred, &prefixes));
    }
    out.inferred = outcome
        .inferred
        .iter()
        .take(MAX_ROWS)
        .map(|i| InferredDto {
            s: i.triple.subject.to_string(),
            p: i.triple.predicate.to_string(),
            o: i.triple.object.to_string(),
            rule: i.rule,
            focus: i.focus.to_string(),
            iteration: i.iteration,
        })
        .collect();
    out.iterations = outcome.iterations;
    out.stopped = outcome.stopped;
    out.warnings = outcome.warnings;
    out.millis = outcome.millis;
    out.rules = outcome.rules;
    drop(outcome.graph);
    let run = s.keep(Run::new(data, inferred, shapes, prefixes.clone()));
    out.run = Some(run.id.clone());
    out.prefixes = prefixes;
    out
}

#[derive(Deserialize)]
struct DownloadParams {
    /// `inferred` (default) or `all`.
    what: Option<String>,
    /// `ttl` (default) or `nt`.
    format: Option<String>,
}

async fn download(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(p): Query<DownloadParams>,
) -> Response {
    let Some(run) = s.run(&id) else {
        return (
            StatusCode::NOT_FOUND,
            "this run has expired; run the rules again",
        )
            .into_response();
    };
    let all = p.what.as_deref() == Some("all");
    let nt = p.format.as_deref() == Some("nt");
    let all_graph;
    let g = if all {
        all_graph = run.all();
        &all_graph
    } else {
        &run.inferred
    };
    let (body, mime, ext) = if nt {
        (output::ntriples(g), "application/n-triples", "nt")
    } else {
        (output::turtle(g, &run.prefixes), "text/turtle", "ttl")
    };
    let name = if all {
        "data-with-inferences"
    } else {
        "inferred"
    };
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}.{ext}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// What a SPARQL endpoint queries: a run's result, or a data graph.
enum Target {
    Run(String),
    Data(String),
}

async fn sparql_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    query_get(s, Target::Run(id), headers, params).await
}

async fn sparql_post(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    query_post(s, Target::Run(id), headers, body).await
}

async fn data_sparql_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    query_get(s, Target::Data(id), headers, params).await
}

async fn data_sparql_post(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    query_post(s, Target::Data(id), headers, body).await
}

async fn query_get(
    s: AppState,
    target: Target,
    headers: HeaderMap,
    params: HashMap<String, String>,
) -> Response {
    match params.get("query") {
        Some(q) => sparql(s, target, q.clone(), headers).await,
        None => (StatusCode::BAD_REQUEST, "missing query parameter").into_response(),
    }
}

async fn query_post(s: AppState, target: Target, headers: HeaderMap, body: Bytes) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let query = if content_type.starts_with("application/sparql-query") {
        String::from_utf8_lossy(&body).into_owned()
    } else {
        let form: HashMap<String, String> = form_pairs(&body).into_iter().collect();
        match form.get("query") {
            Some(q) => q.clone(),
            None => {
                return (StatusCode::BAD_REQUEST, "missing query parameter").into_response();
            }
        }
    };
    sparql(s, target, query, headers).await
}

/// Decode an `application/x-www-form-urlencoded` body.
fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                },
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

async fn sparql(s: AppState, target: Target, query: String, headers: HeaderMap) -> Response {
    let found = match &target {
        Target::Run(id) => s.run(id).map(|r| {
            let prefixes = r.prefixes.clone();
            (
                Box::new(move || r.dataset()) as Box<dyn FnOnce() -> Arc<Dataset> + Send>,
                prefixes,
            )
        }),
        Target::Data(id) => s.data(id).map(|d| {
            let prefixes = d.prefixes.clone();
            (
                Box::new(move || d.dataset()) as Box<dyn FnOnce() -> Arc<Dataset> + Send>,
                prefixes,
            )
        }),
    };
    let Some((dataset, prefixes)) = found else {
        let msg = match target {
            Target::Run(_) => "this run has expired; run the rules again to query the result",
            Target::Data(_) => "the server no longer holds this data graph; open the file again",
        };
        return (StatusCode::NOT_FOUND, msg).into_response();
    };
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let deadline = s.limits.as_ref().map(|l| l.max_time);
    let Ok(_permit) = s.jobs.clone().acquire_owned().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
    };
    let task =
        tokio::task::spawn_blocking(move || evaluate(&dataset(), &prefixes, &query, &accept));
    let result = match deadline {
        Some(d) => match tokio::time::timeout(d, task).await {
            Ok(r) => r,
            Err(_) => {
                return (StatusCode::SERVICE_UNAVAILABLE, "the query took too long")
                    .into_response();
            }
        },
        None => task.await,
    };
    match result {
        Ok(Ok((mime, body))) => ([(header::CONTENT_TYPE, mime)], body).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "the query crashed").into_response(),
    }
}

/// Evaluate a query: over a run, the data with its inferences is the default
/// graph and the shapes graph a named graph.
fn evaluate(
    ds: &Dataset,
    prefixes: &[(String, String)],
    query: &str,
    accept: &str,
) -> Result<(String, Vec<u8>), String> {
    let q = spargebra::SparqlParser::new()
        .parse_query(query)
        .map_err(|e| e.to_string())?;
    let results = QueryEvaluator::new()
        .prepare(&q)
        .execute(ds)
        .map_err(|e| e.to_string())?;
    let err = |e: std::io::Error| e.to_string();
    let (format, mime) = if accept.contains("sparql-results+xml") {
        (QueryResultsFormat::Xml, "application/sparql-results+xml")
    } else if accept.contains("text/csv") {
        (QueryResultsFormat::Csv, "text/csv")
    } else if accept.contains("tab-separated-values") {
        (QueryResultsFormat::Tsv, "text/tab-separated-values")
    } else {
        (QueryResultsFormat::Json, "application/sparql-results+json")
    };
    match results {
        QueryResults::Boolean(b) => {
            let out = QueryResultsSerializer::from_format(format)
                .serialize_boolean_to_writer(Vec::new(), b)
                .map_err(err)?;
            Ok((mime.to_string(), out))
        }
        QueryResults::Solutions(rows) => {
            let mut w = QueryResultsSerializer::from_format(format)
                .serialize_solutions_to_writer(Vec::new(), rows.variables().to_vec())
                .map_err(err)?;
            for row in rows {
                let row = row.map_err(|e| e.to_string())?;
                w.serialize(row.iter()).map_err(err)?;
            }
            Ok((mime.to_string(), w.finish().map_err(err)?))
        }
        QueryResults::Graph(triples) => {
            let mut g = Graph::new();
            for t in triples {
                g.insert(&t.map_err(|e| e.to_string())?);
            }
            if accept.contains("n-triples") {
                Ok((
                    "application/n-triples".to_string(),
                    output::ntriples(&g).into_bytes(),
                ))
            } else {
                Ok((
                    "text/turtle".to_string(),
                    output::turtle(&g, prefixes).into_bytes(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::form_pairs;

    #[test]
    fn decodes_forms() {
        let pairs = form_pairs(b"query=SELECT+*+%7B%3Fs+%3Fp+%3Fo%7D&x=%");
        assert_eq!(pairs[0].1, "SELECT * {?s ?p ?o}");
        assert_eq!(pairs[1].1, "%");
    }
}
