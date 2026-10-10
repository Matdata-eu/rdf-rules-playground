//! The playground's web server: static assets, the inference API, data
//! graphs too big for the editor, and SPARQL endpoints for MatGUI.

pub mod api;
pub mod assets;

use crate::cli::ServeArgs;
use crate::input::{InputFormat, load_files};
use crate::output;
use crate::rules::sparql::SHAPES_GRAPH;
use anyhow::{Context, Result};
use oxrdf::{Dataset, Graph, GraphNameRef, NamedNode, QuadRef};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

/// Caps for a public server. A local playground has none.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_triples: usize,
    pub max_time: Duration,
    pub max_iterations: usize,
}

/// A data graph held by the server instead of the editor: files given on
/// the command line, or a file opened in the browser that is too big to edit.
pub struct ServerData {
    pub id: String,
    /// File names, for display.
    pub files: Vec<String>,
    pub graph: Arc<Graph>,
    pub prefixes: Vec<(String, String)>,
    dataset: Mutex<Option<Arc<Dataset>>>,
}

impl ServerData {
    pub fn new(files: Vec<String>, graph: Graph, prefixes: Vec<(String, String)>) -> Self {
        Self {
            id: new_id(),
            files,
            graph: Arc::new(graph),
            prefixes,
            dataset: Mutex::default(),
        }
    }

    /// The graph as a queryable dataset, built on first use.
    pub fn dataset(&self) -> Arc<Dataset> {
        let mut slot = self.dataset.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert_with(|| Arc::new(dataset(&[&self.graph], None)))
            .clone()
    }
}

/// The result of one run, kept for the SPARQL endpoint and downloads.
pub struct Run {
    pub id: String,
    /// The data graph the rules ran on.
    pub data: Arc<Graph>,
    pub inferred: Graph,
    pub shapes: Graph,
    pub prefixes: Vec<(String, String)>,
    /// Data, inferences and shapes as one dataset, built when first queried.
    dataset: Mutex<Option<Arc<Dataset>>>,
}

impl Run {
    pub fn new(
        data: Arc<Graph>,
        inferred: Graph,
        shapes: Graph,
        prefixes: Vec<(String, String)>,
    ) -> Self {
        Self {
            id: new_id(),
            data,
            inferred,
            shapes,
            prefixes,
            dataset: Mutex::default(),
        }
    }

    /// Data plus inferences in the default graph, shapes as a named graph.
    pub fn dataset(&self) -> Arc<Dataset> {
        let mut slot = self.dataset.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert_with(|| {
            Arc::new(dataset(&[&self.data, &self.inferred], Some(&self.shapes)))
        })
        .clone()
    }

    fn forget_dataset(&self) {
        *self.dataset.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The data graph with the inferred triples added.
    pub fn all(&self) -> Graph {
        let mut g = (*self.data).clone();
        g.extend(self.inferred.iter());
        g
    }
}

fn dataset(defaults: &[&Graph], shapes: Option<&Graph>) -> Dataset {
    let mut ds = Dataset::new();
    for g in defaults {
        for t in g.iter() {
            ds.insert(QuadRef::new(
                t.subject,
                t.predicate,
                t.object,
                GraphNameRef::DefaultGraph,
            ));
        }
    }
    if let Some(shapes) = shapes {
        let name = NamedNode::new_unchecked(SHAPES_GRAPH);
        for t in shapes.iter() {
            ds.insert(QuadRef::new(t.subject, t.predicate, t.object, &name));
        }
    }
    ds
}

#[derive(Clone)]
pub struct AppState {
    pub limits: Option<Limits>,
    /// Editor contents given on the command line.
    pub preload: Option<(String, String)>,
    /// Data files given on the command line that are too big for the editor.
    pub startup_data: Option<Arc<ServerData>>,
    /// Most triples the data editor is given; bigger data stays on the server.
    pub max_editor_triples: usize,
    uploads: Arc<Mutex<VecDeque<Arc<ServerData>>>>,
    runs: Arc<Mutex<VecDeque<Arc<Run>>>>,
    /// Runs allowed at the same time.
    jobs: Arc<Semaphore>,
}

/// How many recent runs stay queryable.
const KEEP_RUNS: usize = 32;
/// How many of those keep their query dataset in memory.
const KEEP_DATASETS: usize = 3;
/// How many data graphs opened in the browser the server keeps.
const KEEP_UPLOADS: usize = 4;

impl AppState {
    pub fn new(limits: Option<Limits>, preload: Option<(String, String)>) -> Self {
        let jobs = if limits.is_some() {
            std::thread::available_parallelism().map_or(2, |n| n.get())
        } else {
            Semaphore::MAX_PERMITS
        };
        Self {
            limits,
            preload,
            startup_data: None,
            max_editor_triples: 20_000,
            uploads: Arc::default(),
            runs: Arc::default(),
            jobs: Arc::new(Semaphore::new(jobs)),
        }
    }

    pub fn keep(&self, run: Run) -> Arc<Run> {
        let run = Arc::new(run);
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.push_back(run.clone());
        while runs.len() > KEEP_RUNS {
            runs.pop_front();
        }
        // A query dataset copies the whole data graph; only recent runs keep one.
        let old = runs.len().saturating_sub(KEEP_DATASETS);
        for r in runs.iter().take(old) {
            r.forget_dataset();
        }
        run
    }

    pub fn run(&self, id: &str) -> Option<Arc<Run>> {
        let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.iter().find(|r| r.id == id).cloned()
    }

    pub fn keep_upload(&self, data: ServerData) -> Arc<ServerData> {
        let data = Arc::new(data);
        let mut uploads = self.uploads.lock().unwrap_or_else(|e| e.into_inner());
        uploads.push_back(data.clone());
        while uploads.len() > KEEP_UPLOADS {
            uploads.pop_front();
        }
        data
    }

    /// A data graph held by the server, by id.
    pub fn data(&self, id: &str) -> Option<Arc<ServerData>> {
        if let Some(d) = &self.startup_data
            && d.id == id
        {
            return Some(d.clone());
        }
        let uploads = self.uploads.lock().unwrap_or_else(|e| e.into_inner());
        uploads.iter().find(|d| d.id == id).cloned()
    }
}

/// A random, unguessable id.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_turtle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttl"))
}

/// Read a shapes file for the editor: Turtle as written, anything else converted.
fn shapes_text(path: &Path) -> Result<String> {
    if is_turtle(path) {
        return std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()));
    }
    let mut g = Graph::new();
    let mut prefixes = Vec::new();
    load_files(&[path], None, &mut g, &mut prefixes)?;
    Ok(output::turtle(&g, &prefixes))
}

/// Load the data files: their text for the editor when they are small (one
/// Turtle file as written, anything else converted), or the graph to keep on
/// the server when they hold more than `max_editor_triples`.
fn startup_data(
    paths: &[PathBuf],
    format: Option<InputFormat>,
    max_editor_triples: usize,
) -> Result<(String, Option<ServerData>)> {
    if paths.is_empty() {
        return Ok((String::new(), None));
    }
    let mut g = Graph::new();
    let mut prefixes = Vec::new();
    load_files(paths, format, &mut g, &mut prefixes)?;
    if g.len() > max_editor_triples {
        let files = paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
            })
            .collect();
        return Ok((String::new(), Some(ServerData::new(files, g, prefixes))));
    }
    if let [path] = paths
        && is_turtle(path)
        && format.is_none_or(|f| f == InputFormat::Ttl)
    {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        return Ok((text, None));
    }
    Ok((output::turtle(&g, &prefixes), None))
}

/// `rdf-rules-playground serve`: start the server and block until ctrl-c.
pub fn serve(args: ServeArgs) -> Result<ExitCode> {
    let (data_text, big_data) = startup_data(&args.data, args.format, args.max_editor_triples)?;
    let shapes = args.shapes.as_deref().map(shapes_text).transpose()?;
    let preload = (!args.data.is_empty() || shapes.is_some())
        .then(|| (data_text, shapes.unwrap_or_default()));
    let limits = args.public.then(|| Limits {
        max_input_bytes: args.max_input_bytes,
        max_triples: args.max_triples,
        max_time: Duration::from_secs(args.max_seconds),
        max_iterations: 50,
    });
    let mut state = AppState::new(limits.clone(), preload);
    state.max_editor_triples = args.max_editor_triples;
    if let Some(d) = big_data {
        eprintln!(
            "data graph: {} triples, more than the editor takes (--max-editor-triples {}); \
             the playground queries it with MatGUI instead",
            d.graph.len(),
            args.max_editor_triples
        );
        state.startup_data = Some(Arc::new(d));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&args.bind)
            .await
            .with_context(|| format!("failed to bind {}", args.bind))?;
        let addr = listener.local_addr().context("local_addr")?;
        let url = format!("http://{addr}/");
        if let Some(l) = &limits {
            eprintln!(
                "public mode: inputs up to {} KB / {} triples, runs up to {} s and {} rounds",
                l.max_input_bytes / 1024,
                l.max_triples,
                l.max_time.as_secs(),
                l.max_iterations
            );
        }
        eprintln!("rdf-rules-playground listening on {url}");
        if !args.no_open
            && !args.public
            && let Err(e) = webbrowser::open(&url)
        {
            eprintln!("could not open browser: {e}");
        }
        let app = api::router(state).into_make_service_with_connect_info::<SocketAddr>();
        tokio::select! {
            r = axum::serve(listener, app) => r.context("web server failed")?,
            r = shutdown_signal() => {
                r?;
                eprintln!("shutting down");
            }
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// Resolves on ctrl-c, or on SIGTERM where there is one.
async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("SIGTERM handler failed")?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => r.context("ctrl-c handler failed"),
            _ = term.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("ctrl-c handler failed")
    }
}
