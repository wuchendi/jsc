#![forbid(unsafe_code)]
//! `jsc` command-line entry point.
//!
//! A thin [`clap`] shell over [`jsc_core`] with node-like modes:
//!
//! - `jsc FILE` / `jsc -e SRC` — compile and run; only `console` output is
//!   printed.
//! - `jsc -p SRC` — compile and run, then print the completion value.
//! - `jsc build FILE -o OUT.wasm` — emit the WebAssembly module (`-o -` writes
//!   to stdout).
//!
//! FILE sources ending in `.ts`, `.mts` or `.cts` are compiled as TypeScript
//! (types are stripped); `.tsx` is rejected; every other FILE and all `-e` /
//! `-p` input is JavaScript.
//!
//! Exit codes: `0` success, `1` uncaught exception / runtime or I/O failure,
//! `2` compile error.

use std::io::Write as _;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jsc_core::frontend::Dialect;

/// Compile JavaScript/TypeScript to WebAssembly and run it, node-style.
#[derive(Parser)]
#[command(name = "jsc", version, about)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
#[command(group(
    clap::ArgGroup::new("source").required(true).args(["eval", "print", "file"])
))]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Run an inline source string, printing only console output.
    #[arg(short, long, value_name = "SRC", allow_hyphen_values = true)]
    eval: Option<String>,

    /// Run an inline source string and print its completion value.
    #[arg(short, long, value_name = "SRC", allow_hyphen_values = true)]
    print: Option<String>,

    /// Run a JavaScript or TypeScript source file, printing only console output.
    #[arg(value_name = "FILE")]
    file: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Compile FILE to a WebAssembly module.
    Build {
        /// JavaScript or TypeScript source file to compile.
        #[arg(value_name = "FILE")]
        file: String,

        /// Output path for the module bytes; `-` writes to stdout.
        #[arg(short, long, value_name = "OUT", allow_hyphen_values = true)]
        output: String,
    },
}

/// A failed CLI run, carrying the process exit code it maps to.
enum Failure {
    /// Parse or lower failed (exit 2).
    Compile(anyhow::Error),
    /// Uncaught exception, runtime trap, or I/O failure (exit 1).
    Runtime(anyhow::Error),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Build { file, output }) => build(&file, &output),
        None => run(cli.eval, cli.print, cli.file),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Compile(err)) => {
            eprintln!("{err:#}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(err)) => {
            eprintln!("{err:#}");
            ExitCode::from(1)
        }
    }
}

/// Run the `-e` / `-p` / FILE form. The `source` `ArgGroup` guarantees exactly one
/// source is present.
fn run(eval: Option<String>, print: Option<String>, file: Option<String>) -> Result<(), Failure> {
    // `label` names the source in diagnostics: `<eval>` for inline input, the
    // real path for a file.
    let (src, label, dialect, print_value) = match (eval, print, file) {
        (Some(src), None, None) => (src, "<eval>".to_owned(), Dialect::Js, false),
        (None, Some(src), None) => (src, "<eval>".to_owned(), Dialect::Js, true),
        (None, None, Some(path)) => {
            let dialect = file_dialect(&path)?;
            (read_source(&path)?, path, dialect, false)
        }
        _ => {
            return Err(Failure::Runtime(anyhow::anyhow!(
                "exactly one of --eval, --print or FILE must be provided"
            )));
        }
    };
    let wasm = compile(&src, dialect, &label)?;
    // `console.log` streams to the real stdout while the module runs.
    let (bits, memory) = jsc_core::run::run(&wasm).map_err(Failure::Runtime)?;
    if print_value {
        let value = jsc_core::run::decode(bits, &memory);
        println!("{}", jsc_core::run::format_value(&value));
    }
    Ok(())
}

/// `jsc build FILE -o OUT`: write the module bytes to OUT (`-` = stdout).
fn build(path: &str, output: &str) -> Result<(), Failure> {
    let dialect = file_dialect(path)?;
    let src = read_source(path)?;
    let wasm = compile(&src, dialect, path)?;
    let written = if output == "-" {
        let mut handle = std::io::stdout().lock();
        handle.write_all(&wasm).and_then(|()| handle.flush())
    } else {
        std::fs::write(output, &wasm)
    };
    written.map_err(|err| Failure::Runtime(anyhow::anyhow!("cannot write {output}: {err}")))
}

fn read_source(path: &str) -> Result<String, Failure> {
    std::fs::read_to_string(path)
        .map_err(|err| Failure::Runtime(anyhow::anyhow!("cannot read {path}: {err}")))
}

/// Pick the source dialect from FILE's extension: `.ts` / `.mts` / `.cts` are
/// TypeScript, `.tsx` is a compile error, anything else is JavaScript.
fn file_dialect(path: &str) -> Result<Dialect, Failure> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str);
    match ext {
        Some("ts" | "mts" | "cts") => Ok(Dialect::Ts),
        Some("tsx") => Err(Failure::Compile(anyhow::anyhow!(
            "{path}: JSX/TSX is not supported"
        ))),
        _ => Ok(Dialect::Js),
    }
}

/// Parse `src` as `dialect`, lower, and emit it. A spanned `CompileError` is replaced by its
/// rendered caret block (labelled `label`); a span-less one keeps its message.
fn compile(src: &str, dialect: Dialect, label: &str) -> Result<Vec<u8>, Failure> {
    let lowered = jsc_core::frontend::parse(src, dialect)
        .and_then(|program| jsc_core::codegen::lower(&program));
    match lowered {
        Ok(ir) => Ok(jsc_core::emit::emit(&ir)),
        Err(err) => {
            let rendered = err
                .downcast_ref::<jsc_core::error::CompileError>()
                .and_then(|ce| ce.render(src, label));
            Err(Failure::Compile(rendered.map_or(err, anyhow::Error::msg)))
        }
    }
}
