//! Shared harness helpers for the jsc-core integration suites.
//!
//! Brought into each theme file via `mod common;`. Helpers are plain fns (not
//! `#[test]`), so `allow-expect-in-tests` does not reach them — allow `expect`
//! explicitly here. Not every helper is used by every theme binary, so allow
//! `dead_code` too.
#![allow(clippy::expect_used)]
#![allow(dead_code)]

use jsc_core::run::{Value, compile_and_run, compile_and_run_capturing, format_value};

/// Compile + run `src` and assert its rendered value equals `expected`.
pub(crate) fn assert_eval(src: &str, expected: &str) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got error: {:?}",
        result.err()
    );
    let value = result.expect("checked Ok above");
    assert_eq!(format_value(&value), expected, "for source `{src}`");
}

/// Compile + run `src` and assert it fails (parse, lowering, or runtime trap) —
/// i.e. it does NOT silently produce a value.
pub(crate) fn assert_rejected(src: &str) {
    assert!(
        compile_and_run(src).is_err(),
        "`{src}` should be rejected as out of scope, but it produced a value",
    );
}

/// Compile + run `src`, asserting its captured `console.log` output equals
/// `expected` (the full stdout text, newlines included).
pub(crate) fn assert_output(src: &str, expected: &str) {
    let result = compile_and_run_capturing(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got error: {:?}",
        result.err()
    );
    let (_value, output) = result.expect("checked Ok above");
    assert_eq!(output, expected, "printed output for source `{src}`");
}

// --- S14.2: Math.* methods + PI / E ------------------------------------------

/// Compile + run `src`, asserting its completion value is the number `expected`.
pub(crate) fn assert_number(src: &str, expected: f64) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got: {:?}",
        result.err()
    );
    assert_eq!(
        result.expect("checked Ok above"),
        Value::Number(expected),
        "value of `{src}`"
    );
}

// --- S14.3: conversions String/Number/Boolean/parseInt/parseFloat -------------

/// Compile + run `src`, asserting its completion value is the string `expected`.
pub(crate) fn assert_string(src: &str, expected: &str) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got: {:?}",
        result.err()
    );
    assert_eq!(
        result.expect("checked Ok above"),
        Value::Str(expected.to_owned()),
        "value of `{src}`"
    );
}

/// Compile + run `src`, asserting its completion value is the boolean `expected`.
pub(crate) fn assert_bool(src: &str, expected: bool) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got: {:?}",
        result.err()
    );
    assert_eq!(
        result.expect("checked Ok above"),
        Value::Bool(expected),
        "value of `{src}`"
    );
}

/// Compile + run `src`, asserting its completion value is the `typeof`-result
/// type string `expected`. A `typeof` result is an ordinary heap string
/// (`Value::Str`) — its immediate encoding was retired.
pub(crate) fn assert_type_str(src: &str, expected: &str) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got: {:?}",
        result.err()
    );
    assert!(
        matches!(result.expect("checked Ok above"), Value::Str(s) if s == expected),
        "value of `{src}` should be typeof string {expected:?}",
    );
}

/// Compile + run `src`, asserting its completion value is a `NaN` number.
pub(crate) fn assert_nan(src: &str) {
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "`{src}` should run, got: {:?}",
        result.err()
    );
    assert!(
        matches!(result.expect("checked Ok above"), Value::Number(n) if n.is_nan()),
        "value of `{src}` should be NaN",
    );
}

/// Compile + run `src`, asserting its final value renders to `expected` — used
/// for caught string / `typeof` results routed to the program's last expression.
/// Rendering via `format_value` (a `Str` prints as its raw text, a `typeof`
/// immediate as the type name) keeps the assertion panic-free for clippy.
pub(crate) fn assert_str(src: &str, expected: &str) {
    let value = compile_and_run(src).expect("program should run to completion");
    assert_eq!(
        jsc_core::run::format_value(&value),
        expected,
        "string result for source `{src}`"
    );
}

/// Compile + run `src`, asserting it surfaces an uncaught exception whose
/// rendered message is exactly `Uncaught {expected}`.
pub(crate) fn assert_uncaught(src: &str, expected: &str) {
    let err = compile_and_run(src).expect_err("program should throw, but completed successfully");
    assert_eq!(
        err.to_string(),
        format!("Uncaught {expected}"),
        "uncaught-exception message for source `{src}`"
    );
}
