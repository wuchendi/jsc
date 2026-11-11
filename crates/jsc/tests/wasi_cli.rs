//! `jsc -p` against an in-process `print_completion` run.
#![allow(clippy::expect_used)]

use std::process::Command;

use jsc_core::emit::CompileOptions;
use jsc_core::run::{Capture, Sinks};

#[test]
fn print_completion_matches_cli_p() {
    for src in ["40 + 2", "'a'", "[1, { b: 2 }]", "console.log(1); 2"] {
        let program =
            jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js).expect("parses");
        let ir = jsc_core::codegen::lower(&program).expect("lowers");
        let wasm = jsc_core::emit::emit(
            &ir,
            CompileOptions {
                print_completion: true,
                gc_stress: false,
            },
        );
        let capture = Capture::new();
        let outcome =
            jsc_core::run::run(&wasm, Sinks::Capture(capture.clone()), None).expect("runs");
        let cli = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-p", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert_eq!(
            cli.status.code(),
            Some(outcome.code),
            "exit code of `{src}`"
        );
        assert_eq!(
            String::from_utf8_lossy(&cli.stdout),
            capture.stdout(),
            "stdout of `{src}`"
        );
    }
}
