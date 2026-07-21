#![forbid(unsafe_code)]
//! `jsc` command-line entry point.
//!
//! A thin [`clap`] shell over [`jsc_core`]. It reads a JavaScript source string
//! either inline (`-e`) or from a file, then either runs it (`--emit run`,
//! the default) and prints the result value, or emits the raw WebAssembly
//! module bytes to stdout (`--emit wasm`).

use std::io::Write as _;

use anyhow::anyhow;
use clap::{Parser, ValueEnum};

/// Compile and run a numeric JavaScript expression.
#[derive(Parser)]
#[command(name = "jsc", version, about)]
#[command(group(
    clap::ArgGroup::new("source").required(true).args(["eval", "file"])
))]
struct Cli {
    /// Evaluate an inline expression string.
    #[arg(short, long, value_name = "EXPR", allow_hyphen_values = true)]
    eval: Option<String>,

    /// Read JavaScript source from a file.
    #[arg(value_name = "FILE")]
    file: Option<String>,

    /// What to produce: run the program or emit the WebAssembly module.
    #[arg(long, value_enum, default_value_t = Emit::Run)]
    emit: Emit,
}

/// Output mode selected by `--emit`.
#[derive(Clone, Copy, ValueEnum)]
enum Emit {
    /// Execute the program and print the numeric result.
    Run,
    /// Write the raw WebAssembly module bytes to stdout.
    Wasm,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // `label` names the source in diagnostics: `<eval>` for inline `-e` input,
    // the real path for a file.
    let (src, label) = match (cli.eval, cli.file) {
        (Some(expr), None) => (expr, "<eval>".to_owned()),
        (None, Some(path)) => (std::fs::read_to_string(&path)?, path),
        // The `source` ArgGroup guarantees exactly one is present.
        _ => return Err(anyhow!("exactly one of --eval or FILE must be provided")),
    };

    match cli.emit {
        Emit::Run => match jsc_core::run::compile_and_run(&src) {
            Ok(value) => println!("{}", jsc_core::run::format_value(&value)),
            Err(err) => {
                // A spanned CompileError renders a source-pointing caret block;
                // span-less or other errors fall through to anyhow's default
                // message-and-nonzero-exit behavior (unchanged).
                let rendered = err
                    .downcast_ref::<jsc_core::error::CompileError>()
                    .and_then(|ce| ce.render(&src, &label));
                if let Some(block) = rendered {
                    eprintln!("{block}");
                    std::process::exit(1);
                }
                return Err(err);
            }
        },
        Emit::Wasm => {
            let program = jsc_core::frontend::parse(&src)?;
            let ir = jsc_core::codegen::lower(&program)?;
            let wasm = jsc_core::emit::emit(&ir);
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(&wasm)?;
            handle.flush()?;
        }
    }

    Ok(())
}
