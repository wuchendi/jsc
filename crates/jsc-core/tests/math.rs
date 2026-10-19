//! Integration tests: `Math.*` methods and constants.
#![allow(clippy::expect_used)]

use jsc_core::run::{Value, compile_and_run, format_value};
mod common;
use common::{assert_eval, assert_number, assert_output};

/// Each acceptance bullet: the primary result of every recognized `Math.*` method.
#[test]
fn math_methods() {
    assert_number("Math.floor(3.7)", 3.0);
    assert_number("Math.max(1, 42)", 42.0);
    assert_number("Math.sqrt(1764)", 42.0);
    assert_number("Math.abs(-42)", 42.0);
    assert_number("Math.min(5, 2, 9)", 2.0);
    assert_number("Math.ceil(3.2)", 4.0);
    assert_number("Math.round(2.5)", 3.0);
    assert_number("Math.trunc(-3.7)", -3.0);
    assert_number("Math.pow(2, 10)", 1024.0);
}

/// `Math.round` uses JS `floor(x + 0.5)` semantics, NOT wasm banker's rounding:
/// `round(2.5) == 3` and `round(-2.5) == -2`.
#[test]
fn math_round_js_semantics() {
    assert_number("Math.round(2.5)", 3.0);
    assert_number("Math.round(-2.5)", -2.0);
    assert_number("Math.round(0.4)", 0.0);
    assert_number("Math.round(-0.5)", -0.0);
}

/// `Math.PI` / `Math.E` are constant property READS (no call), lowering to their
/// `f64` literals; `typeof Math.PI` is `"number"`.
#[test]
fn math_constants() {
    assert_number("Math.PI", std::f64::consts::PI);
    assert_number("Math.E", std::f64::consts::E);
    assert_eq!(
        format_value(&compile_and_run("typeof Math.PI").expect("runs")),
        "number"
    );
    // Usable in arithmetic like any number.
    assert_number("Math.PI * 2", std::f64::consts::PI * 2.0);
}

/// `min`/`max` are variadic: empty args yield the identity infinity, and any NaN
/// argument propagates to NaN (matching wasm `f64.min`/`f64.max`).
#[test]
fn math_min_max_edges() {
    assert_number("Math.min()", f64::INFINITY);
    assert_number("Math.max()", f64::NEG_INFINITY);
    assert_number("Math.min(7)", 7.0);
    // A NaN operand (`0/0`) propagates through the fold.
    let nan = compile_and_run("Math.min(0 / 0, 1)").expect("runs");
    assert!(
        matches!(nan, Value::Number(n) if n.is_nan()),
        "min(NaN,1) is NaN"
    );
    let nan = compile_and_run("Math.max(1, 0 / 0)").expect("runs");
    assert!(
        matches!(nan, Value::Number(n) if n.is_nan()),
        "max(1,NaN) is NaN"
    );
}

/// N5: empty-arg `min`/`max` RENDER as the identity infinities (`Infinity` /
/// `-Infinity`) via the shared number-rendering path, the `Infinity` identifier
/// is usable as an argument, and the non-empty folds stay unchanged.
#[test]
fn math_min_max_empty_arg_identity() {
    assert_eval("Math.max()", "-Infinity");
    assert_eval("Math.min()", "Infinity");
    assert_eval("Math.max(1, Infinity)", "Infinity");
    // Regression: the ordinary variadic folds are untouched.
    assert_eval("Math.max(1, 2, 3)", "3");
    assert_eval("Math.min(1, 2, 3)", "1");
    // Rendered identically when routed through `console.log`.
    assert_output(
        "console.log(Math.max(), Math.min())",
        "-Infinity Infinity\n",
    );
}

/// `Math.pow` is exact for integer exponents (including zero and negatives) and
/// composes with itself.
#[test]
fn math_pow_integer_exponents() {
    assert_number("Math.pow(2, 10)", 1024.0);
    assert_number("Math.pow(5, 0)", 1.0);
    assert_number("Math.pow(2, -2)", 0.25);
    assert_number("Math.pow(3, 3)", 27.0);
    assert_number("Math.pow(Math.pow(2, 3), 2)", 64.0);
}

/// Huge-magnitude integer exponents terminate (square-and-multiply over a halving
/// counter, not one multiply per unit) and saturate to the exact IEEE result.
#[test]
fn math_pow_large_exponents_terminate() {
    assert_eval("2 ** -2147483648", "0");
    assert_eval("1 ** -2147483648", "1");
    assert_eval("(-1) ** 2147483648", "1");
    assert_eval("(-1) ** 2147483647", "-1");
    assert_eval("0.5 ** -2147483648", "Infinity");
    assert_eval("2 ** 2147483648", "Infinity");
    assert_eval("Math.pow(2, -2147483648)", "0");
    assert_eval("Math.pow(-2, 1e308)", "Infinity");
    assert_eval("Math.pow(0.5, 1e308)", "0");
    assert_eval("Math.pow(2, 1074) === Infinity", "false");
    assert_eval("2 ** -1074 > 0", "true");
    assert_eval("1.5 ** 3", "3.375");
    assert_eval("4 ** 2.5", "32");
}

/// The spec's `Number::exponentiate` special cases: `NaN` and infinite exponents,
/// `±1` with an infinite exponent, and signed zero / infinity bases under a
/// non-integer exponent.
#[test]
fn math_pow_special_values() {
    assert_eval("2 ** NaN", "NaN");
    assert_eval("NaN ** 0", "1");
    assert_eval("NaN ** -0", "1");
    assert_eval("NaN ** 1", "NaN");
    assert_eval("1 ** Infinity", "NaN");
    assert_eval("(-1) ** -Infinity", "NaN");
    assert_eval("2 ** Infinity", "Infinity");
    assert_eval("2 ** -Infinity", "0");
    assert_eval("0.5 ** Infinity", "0");
    assert_eval("(-0.5) ** -Infinity", "Infinity");
    assert_eval("(-8) ** (1 / 3)", "NaN");
    assert_eval("1 / ((-0) ** 0.5)", "Infinity");
    assert_eval("(-0) ** -0.5", "Infinity");
    assert_eval("1 / ((-0) ** 3)", "-Infinity");
    assert_eval("(-Infinity) ** 0.5", "Infinity");
    assert_eval("(-Infinity) ** 3", "-Infinity");
    assert_eval("1 / ((-Infinity) ** -3)", "-Infinity");
    assert_eval("1 / ((-Infinity) ** -0.5)", "Infinity");
}

/// `Math` is not a real binding, so a local named `Math` shadows the built-in: the
/// member call then resolves to the user object's function value.
#[test]
fn math_is_shadowable() {
    assert_number(
        "let Math = { floor: function (x) { return 99 } }; Math.floor(3)",
        99.0,
    );
}

/// An unknown `Math.<name>` or a wrong argument count is a compile error.
#[test]
fn math_unknown_method_and_arity_errors() {
    assert!(
        compile_and_run("Math.log(2)").is_err(),
        "unknown method errors"
    );
    assert!(
        compile_and_run("Math.floor(1, 2)").is_err(),
        "extra arg errors"
    );
    assert!(
        compile_and_run("Math.pow(2)").is_err(),
        "missing arg errors"
    );
}

/// `Math.*` results flow through `console.log` like any number.
#[test]
fn math_through_console_log() {
    assert_output("console.log(Math.max(1, 42), Math.floor(3.7))", "42 3\n");
}
