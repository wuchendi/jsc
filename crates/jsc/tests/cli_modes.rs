//! Integration tests: CLI invocation modes — `jsc FILE`, `-e`, `-p`,
//! `jsc build`, and the exit-code contract (0 success, 1 uncaught/runtime,
//! 2 compile error).
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
mod common;
use common::{jsc, jsc_raw_stdout};

/// Spawn the built `jsc` binary with `args`, returning the raw process output.
fn jsc_output(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(args)
        .output()
        .expect("failed to spawn jsc binary")
}

/// Write `src` to `name` under the cargo test temp dir and return its path.
fn temp_source(name: &str, src: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, src).expect("write temp source");
    path
}

/// `jsc FILE` runs the program and prints only its console output — the
/// completion value (`fib(10)` = 55) is not printed.
#[test]
fn file_run_prints_console_output_only() {
    let path = temp_source(
        "fib.js",
        "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } \
         console.log(fib(10)); fib(10)",
    );
    assert_eq!(
        jsc_raw_stdout(&[path.to_str().expect("utf-8 path")]),
        "55\n"
    );
}

/// `jsc FILE` with no console output prints nothing at all.
#[test]
fn file_run_without_console_prints_nothing() {
    let path = temp_source(
        "counter.js",
        "function counter(){ let c=0; return function(){ c=c+1; return c } } \
         let n=counter(); n();n();n()",
    );
    assert_eq!(jsc_raw_stdout(&[path.to_str().expect("utf-8 path")]), "");
}

/// A `console.log`-driven program runs from a source *file*; only the
/// printed lines reach stdout (no trailing completion value).
#[test]
fn console_log_program_via_file() {
    let path = temp_source(
        "s14_log.js",
        "let a=[1,2]; a.push(3); console.log(a.length); console.log(a); \
         console.log([1,2,3].map(x=>x*2))",
    );
    assert_eq!(
        jsc_raw_stdout(&[path.to_str().expect("utf-8 path")]),
        "3\n[ 1, 2, 3 ]\n[ 2, 4, 6 ]\n"
    );
}

/// `-e` runs the source and prints only console output, never the value.
#[test]
fn eval_prints_console_output_only() {
    assert_eq!(jsc_raw_stdout(&["-e", "console.log(1); 2"]), "1\n");
    assert_eq!(jsc_raw_stdout(&["-e", "40 + 2"]), "");
}

/// `-p` prints the completion value after any console output (node -p).
#[test]
fn print_prints_completion_value() {
    assert_eq!(jsc(&["-p", "40 + 2"]), "42");
    assert_eq!(jsc(&["--print", "1 + 1"]), "2");
    assert_eq!(jsc_raw_stdout(&["-p", "console.log(1); 2"]), "1\n2\n");
}

/// `-p` accepts a value that starts with `-`; without `allow_hyphen_values` clap
/// treats `-1 * 2` as an unknown flag and the parse fails before evaluation.
#[test]
fn print_accepts_leading_hyphen_expression() {
    assert_eq!(jsc(&["-p", "-1 * 2"]), "-2");
}

/// `-p '-0'` parses and runs without a clap error, and (N3) renders as `0`:
/// JS `String(-0)` is `"0"`.
#[test]
fn print_accepts_leading_hyphen_negative_zero() {
    assert_eq!(jsc(&["-p", "-0"]), "0");
}

/// `-e` also accepts a leading-hyphen source.
#[test]
fn eval_accepts_leading_hyphen_source() {
    assert_eq!(jsc_raw_stdout(&["-e", "-1; console.log(3)"]), "3\n");
}

/// `-e`, `-p`, and FILE are mutually exclusive.
#[test]
fn source_forms_are_mutually_exclusive() {
    let output = jsc_output(&["-e", "1", "-p", "2"]);
    assert!(!output.status.success(), "-e with -p must be rejected");
}

/// `jsc build FILE -o OUT.wasm` writes the WebAssembly module to OUT.wasm.
#[test]
fn build_writes_module_to_file() {
    let src = temp_source("build_src.js", "40 + 2");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("build_out.wasm");
    let _ = std::fs::remove_file(&out);
    let output = jsc_output(&[
        "build",
        src.to_str().expect("utf-8 path"),
        "-o",
        out.to_str().expect("utf-8 path"),
    ]);
    assert_eq!(output.status.code(), Some(0), "build should succeed");
    assert!(output.stdout.is_empty(), "build -o FILE prints nothing");
    let bytes = std::fs::read(&out).expect("read built module");
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic header");
}

/// `jsc build FILE -o -` writes the module bytes to stdout.
#[test]
fn build_writes_module_to_stdout() {
    let src = temp_source("build_stdout.js", "40 + 2");
    let output = jsc_output(&["build", src.to_str().expect("utf-8 path"), "-o", "-"]);
    assert_eq!(output.status.code(), Some(0), "build should succeed");
    assert!(
        output.stdout.starts_with(b"\0asm"),
        "expected wasm magic header"
    );
}

/// `jsc build` requires `-o`.
#[test]
fn build_requires_output() {
    let src = temp_source("build_no_o.js", "1");
    let output = jsc_output(&["build", src.to_str().expect("utf-8 path")]);
    assert!(!output.status.success(), "build without -o must fail");
}

/// A successful run exits 0.
#[test]
fn success_exits_zero() {
    assert_eq!(jsc_output(&["-e", "1 + 1"]).status.code(), Some(0));
    assert_eq!(jsc_output(&["-p", "1 + 1"]).status.code(), Some(0));
}

/// A compile error exits 2 and renders the caret diagnostic on stderr.
#[test]
fn compile_error_exits_two() {
    for flag in ["-e", "-p"] {
        let output = jsc_output(&[flag, "let x = ;"]);
        assert_eq!(output.status.code(), Some(2), "{flag}: compile error");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--> <eval>:1:"), "got: {stderr}");
        assert!(stderr.contains('^'), "got: {stderr}");
    }
}

/// A compile error in a FILE exits 2 and labels the diagnostic with the path.
#[test]
fn file_compile_error_exits_two() {
    let path = temp_source("bad.js", "let x = ;");
    let path = path.to_str().expect("utf-8 path");
    let output = jsc_output(&[path]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("--> {path}:1:")), "got: {stderr}");
}

/// `jsc build` on a source with a compile error exits 2.
#[test]
fn build_compile_error_exits_two() {
    let src = temp_source("build_bad.js", "let x = ;");
    let output = jsc_output(&["build", src.to_str().expect("utf-8 path"), "-o", "-"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "no module bytes on a compile error"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains('^'));
}

/// An uncaught exception exits 1 with `Uncaught ...` on stderr.
#[test]
fn uncaught_exception_exits_one() {
    for flag in ["-e", "-p"] {
        let output = jsc_output(&[flag, "throw 1"]);
        assert_eq!(output.status.code(), Some(1), "{flag}: uncaught throw");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("Uncaught 1"), "got: {stderr}");
        assert!(output.stdout.is_empty(), "no value printed on a throw");
    }
}

/// An unreadable FILE fails with a message on stderr.
#[test]
fn missing_file_fails() {
    let output = jsc_output(&["/nonexistent/jsc-missing.js"]);
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty(), "expected an I/O error message");
}
