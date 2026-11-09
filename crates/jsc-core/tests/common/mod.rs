//! Shared harness helpers for the jsc-core integration suites, on the WASI
//! runner: a program's completion value is rendered by the guest
//! (`print_completion`) and compared as text.
//!
//! Brought into each theme file via `mod common;`. Helpers are plain fns (not
//! `#[test]`), so `allow-expect-in-tests` does not reach them — allow `expect`
//! explicitly here. Not every helper is used by every theme binary, so allow
//! `dead_code` too.
#![allow(clippy::expect_used, clippy::panic)]
#![allow(dead_code)]

use jsc_core::emit::CompileOptions;
use jsc_core::run::{Capture, Sinks};

/// The observable result of running a program.
pub(crate) struct Finished {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Parse, lower and emit `src`, failing the test on an invalid module.
pub(crate) fn compile(src: &str, print_completion: bool) -> anyhow::Result<Vec<u8>> {
    let program = jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js)?;
    let ir = jsc_core::codegen::lower(&program)?;
    let wasm = jsc_core::emit::emit(&ir, CompileOptions { print_completion });
    wasmparser::Validator::new()
        .validate_all(&wasm)
        .unwrap_or_else(|err| panic!("`{src}` compiled to invalid wasm: {err}"));
    Ok(wasm)
}

/// Compile and run `src`, capturing its output. A compile error or a trap is
/// an `Err`.
pub(crate) fn exec(src: &str, print_completion: bool) -> anyhow::Result<Finished> {
    let wasm = compile(src, print_completion)?;
    let capture = Capture::new();
    let outcome = jsc_core::run::run(&wasm, Sinks::Capture(capture.clone()), None)?;
    Ok(Finished {
        code: outcome.code,
        stdout: capture.stdout(),
        stderr: capture.stderr(),
    })
}

/// `src`'s compile error text; fails the test if it compiles.
pub(crate) fn compile_error(src: &str) -> String {
    compile(src, false)
        .expect_err("program should not compile")
        .to_string()
}

/// Whether `src` is refused: a compile error, a trap, or a non-zero exit.
pub(crate) fn fails(src: &str) -> bool {
    exec(src, false).map_or(true, |done| done.code != 0)
}

/// Assert `src` runs to a zero exit.
pub(crate) fn assert_runs(src: &str) {
    let done = exec(src, false).expect("program should compile and not trap");
    assert_eq!(done.code, 0, "`{src}` failed: {}", done.stderr);
}

/// `src`'s completion value as rendered text.
pub(crate) fn eval_text(src: &str) -> String {
    let done = exec(src, true).expect("program should compile and not trap");
    assert_eq!(done.code, 0, "`{src}` failed: {}", done.stderr);
    // The completion follows anything the program printed itself.
    let printed = if src.contains("console") {
        exec(src, false).expect("runs").stdout
    } else {
        String::new()
    };
    done.stdout
        .strip_prefix(printed.as_str())
        .and_then(|rest| rest.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("`{src}`: no completion line in {:?}", done.stdout))
        .to_owned()
}

/// Render `x` the way ECMAScript `Number::toString` does, from Rust's shortest
/// round-trip digits (`-0` renders as `0`).
pub(crate) fn js_number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if x.classify() == std::num::FpCategory::Zero {
        return "0".to_owned();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("LowerExp has an exponent");
    let digits = mantissa.replace('.', "");
    let k = i32::try_from(digits.len()).expect("few digits");
    let n = exponent.parse::<i32>().expect("integer exponent") + 1;
    let zeros = |count: i32| "0".repeat(usize::try_from(count).expect("non-negative"));
    let split = |at: i32| digits.split_at(usize::try_from(at).expect("non-negative"));
    let body = if (k..=21).contains(&n) {
        format!("{digits}{}", zeros(n - k))
    } else if (1..=21).contains(&n) {
        let (int, frac) = split(n);
        format!("{int}.{frac}")
    } else if (-5..=0).contains(&n) {
        format!("0.{}{digits}", zeros(-n))
    } else {
        let sign = if n > 0 { '+' } else { '-' };
        let (first, rest) = split(1);
        let dot = if rest.is_empty() { "" } else { "." };
        format!("{first}{dot}{rest}e{sign}{}", (n - 1).abs())
    };
    if x < 0.0 { format!("-{body}") } else { body }
}

/// Assert `src`'s rendered completion value equals `expected`.
pub(crate) fn assert_eval(src: &str, expected: &str) {
    assert_eq!(eval_text(src), expected, "for source `{src}`");
}

/// Assert `src` is refused (see [`fails`]) — it does NOT silently produce a
/// value.
pub(crate) fn assert_rejected(src: &str) {
    assert!(
        fails(src),
        "`{src}` should be rejected as out of scope, but it produced a value",
    );
}

/// Assert `src` exits 1 reporting the unhandled rejection `expected` on stderr.
pub(crate) fn assert_unhandled_rejection(src: &str, expected: &str) {
    let done = exec(src, false).expect("program should compile and not trap");
    assert_eq!(
        (done.code, done.stderr.as_str()),
        (1, format!("Uncaught (in promise) {expected}\n").as_str()),
        "unhandled-rejection report for source `{src}`"
    );
}

/// Assert `src` exits 0 and its stdout equals `expected`, newlines included.
pub(crate) fn assert_output(src: &str, expected: &str) {
    let done = exec(src, false).expect("program should compile and not trap");
    assert_eq!(done.code, 0, "`{src}` failed: {}", done.stderr);
    assert_eq!(done.stdout, expected, "printed output for source `{src}`");
}

/// Assert `src`'s completion value renders as the number `expected`.
pub(crate) fn assert_number(src: &str, expected: f64) {
    assert_eq!(eval_text(src), js_number(expected), "value of `{src}`");
}

/// Assert `src`'s completion value renders as the string `expected`.
pub(crate) fn assert_string(src: &str, expected: &str) {
    assert_eq!(eval_text(src), expected, "value of `{src}`");
}

/// Assert `src`'s completion value renders as the boolean `expected`.
pub(crate) fn assert_bool(src: &str, expected: bool) {
    assert_eq!(eval_text(src), expected.to_string(), "value of `{src}`");
}

/// Assert `src`'s completion value renders as the `typeof` result `expected`.
pub(crate) fn assert_type_str(src: &str, expected: &str) {
    assert_eq!(eval_text(src), expected, "value of `{src}`");
}

/// Assert `src`'s completion value is `NaN`.
pub(crate) fn assert_nan(src: &str) {
    assert_eq!(eval_text(src), "NaN", "value of `{src}`");
}

/// Assert `src`'s final value renders to `expected`.
pub(crate) fn assert_str(src: &str, expected: &str) {
    assert_eq!(eval_text(src), expected, "string result for source `{src}`");
}

/// Assert `src` exits 1 reporting exactly `Uncaught {expected}` on stderr.
pub(crate) fn assert_uncaught(src: &str, expected: &str) {
    let done = exec(src, false).expect("program should compile and not trap");
    assert_eq!(
        (done.code, done.stderr.as_str()),
        (1, format!("Uncaught {expected}\n").as_str()),
        "uncaught-exception report for source `{src}`"
    );
}
