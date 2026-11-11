//! Crate-level smoke test that the full pipeline is wired together.
//!
//! This exercises the `run` stage, so the whole module is gated on the
//! `native-run` feature; without it the crate has no runtime to drive.
#![cfg(feature = "native-run")]

use crate::emit::CompileOptions;
use crate::run::{Capture, Sinks, run};

#[test]
fn compile_and_run_evaluates_numeric_expression() {
    let program = crate::frontend::parse("1 + 2", crate::frontend::Dialect::Js).expect("parse");
    let ir = crate::codegen::lower(&program).expect("lower");
    let wasm = crate::emit::emit(
        &ir,
        CompileOptions {
            print_completion: true,
            gc_stress: false,
        },
    );
    let capture = Capture::new();
    let outcome = run(&wasm, Sinks::Capture(capture.clone()), None).expect("module should run");
    assert_eq!(outcome.code, 0);
    assert_eq!(capture.stdout(), "3\n");
}
