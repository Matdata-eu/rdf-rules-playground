use clap::Parser;
use oxrdf::Graph;
use shacl_af_playground::cli::{Cli, Command, InferArgs, OutputFormat, ServeArgs};
use shacl_af_playground::input::{load_files, merge_prefixes};
use shacl_af_playground::rules::{self, Options};
use shacl_af_playground::{output, web};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Infer(args)) => infer(args),
        Some(Command::Serve(args)) => web::serve(args),
        None => web::serve(ServeArgs {
            bind: "127.0.0.1:0".to_string(),
            ..ServeArgs::default()
        }),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("shacl-af-playground: error: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn infer(args: InferArgs) -> anyhow::Result<ExitCode> {
    let mut data = Graph::new();
    let mut prefixes = Vec::new();
    load_files(&args.data, args.format, &mut data, &mut prefixes)?;
    let shapes = if args.shapes.is_empty() {
        data.clone()
    } else {
        let mut shapes = Graph::new();
        let mut found = Vec::new();
        load_files(&args.shapes, args.format, &mut shapes, &mut found)?;
        merge_prefixes(&mut prefixes, found);
        shapes
    };
    let opts = Options {
        iterate: !args.single_pass,
        max_iterations: args.max_iterations.max(1),
        ..Options::default()
    };
    let outcome = rules::infer(&data, &shapes, &opts)?;

    let result = if args.all {
        outcome.graph.clone()
    } else {
        outcome.inferred.iter().map(|i| i.triple.clone()).collect()
    };
    let text = match args.output_format {
        OutputFormat::Turtle => output::turtle(&result, &prefixes),
        OutputFormat::Ntriples => output::ntriples(&result),
    };
    match &args.output {
        Some(path) => std::fs::write(path, text)
            .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?,
        None => print!("{text}"),
    }

    let mut failed = false;
    if !args.quiet {
        for w in &outcome.warnings {
            eprintln!("warning: {w}");
        }
        if args.explain {
            for r in &outcome.rules {
                let name = r.label.clone().unwrap_or_else(|| r.id.clone());
                let state = if r.deactivated { "  (deactivated)" } else { "" };
                eprintln!(
                    "rule {name} [{} on {}, order {}]{state}: focus-nodes={} skipped-by-condition={} inferred={} already-present={}",
                    r.kind, r.shape, r.order, r.focus_nodes, r.skipped, r.inferred, r.existing
                );
            }
        }
        eprintln!(
            "rules={}  rounds={}  inferred={}  in {} ms",
            outcome.rules.len(),
            outcome.iterations,
            outcome.inferred.len(),
            outcome.millis
        );
    }
    for r in &outcome.rules {
        for e in &r.errors {
            failed = true;
            eprintln!(
                "error: rule {}: {e}",
                r.label.clone().unwrap_or_else(|| r.id.clone())
            );
        }
    }
    if let Some(s) = &outcome.stopped {
        failed = true;
        eprintln!("error: {s}");
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
