//! Integration tests: CLI invocation modes: file input and `--emit wasm`.
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::Command;
mod common;
use common::{jsc, jsc_raw_stdout};

/// `jsc <file>` reads a multi-statement program from disk and runs it.
///
/// The recursive `fib(10)` covers the fourth milestone (55) and exercises the
/// file-input path end to end.
#[test]
fn milestone_via_file() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fib.js");
    std::fs::write(
        &path,
        "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)",
    )
    .expect("write temp source");

    let out = jsc(&[path.to_str().expect("utf-8 path")]);
    assert_eq!(out, "55");
}

/// `--emit wasm` writes a non-empty WebAssembly module to stdout.
#[test]
fn emit_wasm_writes_module_bytes() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "40 + 2", "--emit", "wasm"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.success(), "emit wasm should succeed");
    // Every wasm module starts with the `\0asm` magic and a version word.
    assert!(
        output.stdout.starts_with(b"\0asm"),
        "expected wasm magic header"
    );
}

/// `-e` accepts a value that starts with `-`; without `allow_hyphen_values` clap
/// treats `-1 * 2` as an unknown flag and the parse fails before evaluation.
#[test]
fn eval_accepts_leading_hyphen_expression() {
    assert_eq!(jsc(&["-e", "-1 * 2"]), "-2");
}

/// `-e '-0'` parses and runs without a clap error, and (N3) renders as `0`:
/// JS `String(-0)` is `"0"`. The stored f64 sign is preserved, so this pins both
/// that the hyphen-leading value is accepted and the normalized rendering.
#[test]
fn eval_accepts_leading_hyphen_negative_zero() {
    assert_eq!(jsc(&["-e", "-0"]), "0");
}

/// A non-hyphen `-e` invocation still works unchanged after enabling
/// `allow_hyphen_values`.
#[test]
fn eval_non_hyphen_still_works() {
    assert_eq!(jsc(&["-e", "40 + 2"]), "42");
}

/// S13.7 — a closure acceptance milestone also runs from a source *file*, so the
/// file-input path is exercised against the S13 feature set, not only `-e`.
#[test]
fn s13_closure_acceptance_via_file() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("counter.js");
    std::fs::write(
        &path,
        "function counter(){ let c=0; return function(){ c=c+1; return c } } \
         let n=counter(); n();n();n()",
    )
    .expect("write temp source");
    assert_eq!(jsc(&[path.to_str().expect("utf-8 path")]), "3");
}

/// S14.7 — a `console.log`-driven program runs from a source *file*, exercising the
/// file-input path against S14's print channel (not only `-e`). The program prints
/// several lines as it goes and ends on `undefined`.
#[test]
fn s14_console_log_acceptance_via_file() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("s14_log.js");
    std::fs::write(
        &path,
        "let a=[1,2]; a.push(3); console.log(a.length); console.log(a); \
         console.log([1,2,3].map(x=>x*2))",
    )
    .expect("write temp source");
    assert_eq!(
        jsc_raw_stdout(&[path.to_str().expect("utf-8 path")]),
        "3\n[ 1, 2, 3 ]\n[ 2, 4, 6 ]\nundefined\n"
    );
}
