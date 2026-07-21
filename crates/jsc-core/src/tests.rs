//! Crate-level smoke test that the full pipeline is wired together.
//!
//! This exercises the `run` stage, so the whole module is gated on the
//! `native-run` feature; without it the crate has no runtime to drive.
#![cfg(feature = "native-run")]

use crate::run::{compile_and_run, format_value};

#[test]
fn compile_and_run_evaluates_numeric_expression() {
    let value = compile_and_run("1 + 2").expect("pipeline should succeed");
    assert_eq!(format_value(&value), "3");
}
