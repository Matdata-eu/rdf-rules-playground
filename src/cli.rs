//! Command line interface.

use crate::input::InputFormat;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "rdf-rules-playground",
    version,
    about = "Run SHACL Advanced Features rules (sh:TripleRule, sh:SPARQLRule) on RDF data",
    long_about = "Run SHACL Advanced Features rules (sh:TripleRule, sh:SPARQLRule) on RDF data.\n\n\
                  Without a command, the playground opens in your browser: type or paste data \
                  and rules, run them, and see which rule inferred which triple. `infer` runs \
                  the same engine from the command line.",
    after_help = "Questions, bug reports and feature requests: https://github.com/Matdata-eu/rdf-rules-playground/issues"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the rules over the data and print the inferred triples.
    Infer(InferArgs),
    /// Open the playground in the browser (the default without a command).
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct InferArgs {
    /// Data graph file(s); `-` reads Turtle from standard input.
    #[arg(short, long = "data", value_name = "FILE", required = true, num_args = 1..)]
    pub data: Vec<PathBuf>,
    /// Shapes graph file(s) holding the rules. Without it, the rules are read
    /// from the data graph.
    #[arg(short, long = "shapes", value_name = "FILE", num_args = 1..)]
    pub shapes: Vec<PathBuf>,
    /// Syntax of every input file; detected from the extension by default.
    #[arg(long, value_enum)]
    pub format: Option<InputFormat>,
    /// Write the result to FILE instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Syntax of the result.
    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Turtle)]
    pub output_format: OutputFormat,
    /// Write the data graph together with the inferred triples.
    #[arg(long)]
    pub all: bool,
    /// Run every rule once instead of repeating until nothing new is inferred.
    #[arg(long = "single-pass")]
    pub single_pass: bool,
    /// Most rounds before giving up on reaching a fixpoint.
    #[arg(long = "max-iterations", value_name = "N", default_value_t = 100)]
    pub max_iterations: usize,
    /// Print for every rule how many focus nodes it ran on and what it inferred.
    #[arg(long)]
    pub explain: bool,
    /// Print nothing on standard error except errors.
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Turtle,
    Ntriples,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Data graph file(s) to open with: Turtle, N-Triples, RDF/XML, TriG or
    /// N-Quads, also gzipped. Data with more triples than
    /// --max-editor-triples stays on the server and is shown as a query view.
    #[arg(short, long = "data", value_name = "FILE", num_args = 1..)]
    pub data: Vec<PathBuf>,
    /// Shapes file to open in the playground's rules editor.
    #[arg(short, long = "shapes", value_name = "FILE")]
    pub shapes: Option<PathBuf>,
    /// Syntax of the data files; detected from the extension by default.
    #[arg(long, value_enum)]
    pub format: Option<InputFormat>,
    /// Most triples the data editor is given. Bigger data graphs, from the
    /// command line or opened in the browser, are queried with MatGUI instead.
    #[arg(
        long = "max-editor-triples",
        value_name = "N",
        default_value_t = 20_000,
        env = "RDF_RULES_PLAYGROUND_MAX_EDITOR_TRIPLES"
    )]
    pub max_editor_triples: usize,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:0", env = "RDF_RULES_PLAYGROUND_BIND")]
    pub bind: String,
    /// Do not open the browser.
    #[arg(long = "no-open")]
    pub no_open: bool,
    /// Serve the playground as a public website: caps input size, run time
    /// and rounds per request, and never opens a browser.
    #[arg(long, env = "RDF_RULES_PLAYGROUND_PUBLIC")]
    pub public: bool,
    /// In public mode: most bytes of Turtle per editor.
    #[arg(long = "max-input-bytes", value_name = "BYTES", default_value_t = 2 * 1024 * 1024, env = "RDF_RULES_PLAYGROUND_MAX_INPUT_BYTES")]
    pub max_input_bytes: usize,
    /// In public mode: most triples per editor, and most inferred triples.
    #[arg(
        long = "max-triples",
        value_name = "N",
        default_value_t = 50_000,
        env = "RDF_RULES_PLAYGROUND_MAX_TRIPLES"
    )]
    pub max_triples: usize,
    /// In public mode: most seconds one run may take.
    #[arg(
        long = "max-seconds",
        value_name = "S",
        default_value_t = 20,
        env = "RDF_RULES_PLAYGROUND_MAX_SECONDS"
    )]
    pub max_seconds: u64,
}
