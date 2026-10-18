//! `jsc-test262` command-line entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("usage: jsc-test262 <test262-dir> [FILTER...]");
    ExitCode::from(2)
}
