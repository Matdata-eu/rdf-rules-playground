//! The playground's web server: static assets, the inference API, and a
//! SPARQL endpoint over each run's result for MatGUI.

pub mod api;
pub mod assets;

use crate::cli::ServeArgs;
use crate::input::load_files;
use crate::output;
use anyhow::{Context, Result};
use oxrdf::Graph;
use std::collections::VecDeque;
use std::net::SocketAddr;
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

/// The result of one run, kept for the SPARQL endpoint and downloads.
pub struct Run {
    pub id: String,
    /// The data graph plus everything inferred.
    pub all: Graph,
    pub inferred: Graph,
    pub shapes: Graph,
    pub prefixes: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct AppState {
    pub limits: Option<Limits>,
    /// Editor contents given on the command line.
    pub preload: Option<(String, String)>,
    runs: Arc<Mutex<VecDeque<Arc<Run>>>>,
    /// Runs allowed at the same time.
    jobs: Arc<Semaphore>,
}

/// How many recent runs stay queryable.
const KEEP_RUNS: usize = 32;

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
        run
    }

    pub fn run(&self, id: &str) -> Option<Arc<Run>> {
        let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.iter().find(|r| r.id == id).cloned()
    }
}

/// A random, unguessable run id.
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

/// Read a file for an editor: Turtle as written, anything else converted.
fn editor_text(path: &std::path::Path) -> Result<String> {
    let is_turtle = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttl"));
    if is_turtle {
        return std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()));
    }
    let mut g = Graph::new();
    let mut prefixes = Vec::new();
    load_files(&[path], None, &mut g, &mut prefixes)?;
    Ok(output::turtle(&g, &prefixes))
}

/// `shacl-af-playground serve`: start the server and block until ctrl-c.
pub fn serve(args: ServeArgs) -> Result<ExitCode> {
    let preload = match (&args.data, &args.shapes) {
        (None, None) => None,
        (d, s) => Some((
            d.as_deref()
                .map(editor_text)
                .transpose()?
                .unwrap_or_default(),
            s.as_deref()
                .map(editor_text)
                .transpose()?
                .unwrap_or_default(),
        )),
    };
    let limits = args.public.then(|| Limits {
        max_input_bytes: args.max_input_bytes,
        max_triples: args.max_triples,
        max_time: Duration::from_secs(args.max_seconds),
        max_iterations: 50,
    });
    let state = AppState::new(limits.clone(), preload);
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
        eprintln!("shacl-af-playground listening on {url}");
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
