//! Integration tests: `Math.*` methods and constants.
#![allow(clippy::expect_used)]

use jsc_core::run::{Value, compile_and_run, format_value};
mod common;
use common::{assert_eval, assert_number, assert_output, assert_uncaught};

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
    assert_eval("Math.pow(2, 1023) === Infinity", "false");
    assert_eval("2 ** 1024", "Infinity");
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

/// An unknown `Math.<name>` is an ordinary property read (`undefined`), so
/// calling it throws a catchable `TypeError` at run time.
#[test]
fn math_unknown_method_is_a_type_error() {
    assert_uncaught("Math.nope(2)", "TypeError: Math.nope is not a function");
}

/// `Math.*` results flow through `console.log` like any number.
#[test]
fn math_through_console_log() {
    assert_output("console.log(Math.max(1, 42), Math.floor(3.7))", "42 3\n");
}

/// `1 / x`, so a signed zero renders as `Infinity` / `-Infinity`.
fn sign_of(expr: &str) -> String {
    format!("1 / ({expr})")
}

/// The transcendental and remaining ES2025 `Math` functions: primary results,
/// `ToNumber` of the argument, and the NaN / signed-zero / infinity cases.
#[test]
fn math_transcendentals() {
    for (src, expected) in [
        ("Math.log(Math.E)", "1"),
        ("Math.log(1)", "0"),
        ("Math.log(0)", "-Infinity"),
        ("Math.log(-1)", "NaN"),
        ("Math.log2(8)", "3"),
        ("Math.log10(1000)", "3"),
        ("Math.log1p(0)", "0"),
        ("Math.log1p(-1)", "-Infinity"),
        ("Math.exp(0)", "1"),
        ("Math.exp(-Infinity)", "0"),
        ("Math.expm1(0)", "0"),
        ("Math.expm1(-Infinity)", "-1"),
        ("Math.cbrt(27)", "3"),
        ("Math.cbrt(-8)", "-2"),
        ("Math.sin(0)", "0"),
        ("Math.cos(0)", "1"),
        ("Math.tan(0)", "0"),
        ("Math.asin(1) * 2 === Math.PI", "true"),
        ("Math.acos(1)", "0"),
        ("Math.atan(Infinity) * 2 === Math.PI", "true"),
        ("Math.atan2(1, 1) * 4 === Math.PI", "true"),
        ("Math.atan2(0, -1) === Math.PI", "true"),
        ("Math.sinh(0)", "0"),
        ("Math.cosh(0)", "1"),
        ("Math.tanh(Infinity)", "1"),
        ("Math.asinh(0)", "0"),
        ("Math.acosh(1)", "0"),
        ("Math.atanh(1)", "Infinity"),
        ("Math.sin('0')", "0"),
        ("Math.exp({ valueOf() { return 0 } })", "1"),
        ("Math.cos(NaN)", "NaN"),
        ("Math.sin(Infinity)", "NaN"),
        ("Math.acosh(0.5)", "NaN"),
        ("Math.cosh(-Infinity)", "Infinity"),
    ] {
        assert_eval(src, expected);
    }
    for expr in [
        "Math.sin(-0)",
        "Math.tan(-0)",
        "Math.asin(-0)",
        "Math.atan(-0)",
        "Math.sinh(-0)",
        "Math.tanh(-0)",
        "Math.asinh(-0)",
        "Math.atanh(-0)",
        "Math.cbrt(-0)",
        "Math.expm1(-0)",
        "Math.log1p(-0)",
        "Math.atan2(-0, 0)",
    ] {
        assert_eval(&sign_of(expr), "-Infinity");
    }
}

/// `sign`, `clz32`, `fround`, `imul`, `hypot` and `random`.
#[test]
fn math_integer_and_misc() {
    for (src, expected) in [
        ("Math.sign(-3)", "-1"),
        ("Math.sign(5)", "1"),
        ("Math.sign(NaN)", "NaN"),
        ("Math.sign('-2')", "-1"),
        ("Math.clz32(1)", "31"),
        ("Math.clz32(0)", "32"),
        ("Math.clz32(-1)", "0"),
        ("Math.clz32(2 ** 32)", "32"),
        ("Math.clz32(NaN)", "32"),
        ("Math.clz32(0.5)", "32"),
        ("Math.fround(5.5)", "5.5"),
        ("Math.fround(5.05)", "5.050000190734863"),
        ("Math.fround(2 ** 128)", "Infinity"),
        ("Math.fround(NaN)", "NaN"),
        ("Math.imul(2, 4)", "8"),
        ("Math.imul(-1, 8)", "-8"),
        ("Math.imul(0xffffffff, 5)", "-5"),
        ("Math.imul(0xfffffffe, 5)", "-10"),
        ("Math.imul('3', { valueOf() { return 4 } })", "12"),
        ("Math.hypot()", "0"),
        ("Math.hypot(3, 4)", "5"),
        ("Math.hypot(-3)", "3"),
        ("Math.hypot(NaN, 1)", "NaN"),
        ("Math.hypot(NaN, Infinity)", "Infinity"),
        ("Math.hypot(-Infinity, NaN)", "Infinity"),
        ("Math.hypot(1e200, 1e200) > 1e200", "true"),
        ("Math.hypot(1, 2, 2)", "3"),
        ("Math.hypot(...[1, 2, 2])", "3"),
        ("Math.hypot.length", "2"),
        ("Math.max.length + Math.atan2.length + Math.imul.length", "6"),
        ("Math.sin.name", "sin"),
        ("typeof Math.random()", "number"),
        (
            "let ok = true; for (let i = 0; i < 100; i++) { const r = Math.random(); if (!(r >= 0 && r < 1)) ok = false } ok",
            "true",
        ),
        ("Math.random() !== Math.random()", "true"),
    ] {
        assert_eval(src, expected);
    }
    for expr in [
        "Math.sign(-0)",
        "Math.fround(-0)",
        "Math.imul(-0, 1) * -1",
    ] {
        assert_eval(&sign_of(expr), "-Infinity");
    }
    assert_eval(&sign_of("Math.hypot(-0)"), "Infinity");
    assert_eval(&sign_of("Math.hypot(-0, -0)"), "Infinity");
}

/// Every argument of a multi-argument `Math` function is converted with
/// `ToNumber` in order, before the `Infinity` / `NaN` rules apply.
#[test]
fn math_converts_every_argument_in_order() {
    let prelude = "let s = ''; const v = (c, n) => ({ valueOf() { s += c; return n } });";
    for src in [
        "Math.hypot(v('a', Infinity), v('b', NaN), v('c', 1)); s",
        "Math.atan2(v('a', 1), v('b', 2)); s",
        "Math.imul(v('a', 1), v('b', 2)); s",
        "Math.hypot(...[v('a', NaN), v('b', 1), v('c', 2)]); s",
    ] {
        let expected = if src.contains("'c'") { "abc" } else { "ab" };
        assert_eval(&format!("{prelude} {src}"), expected);
    }
}

/// `Math.round` rounds half up without the `x + 0.5` error, keeps `-0` for
/// `[-0.5, -0]`, and leaves large integers alone.
#[test]
fn math_round_edges() {
    assert_eval("Math.round(0.49999999999999994)", "0");
    assert_eval("Math.round(4503599627370497)", "4503599627370497");
    assert_eval("Math.round(-4503599627370497)", "-4503599627370497");
    assert_eval(&sign_of("Math.round(-0.4)"), "-Infinity");
    assert_eval(&sign_of("Math.round(-0.5)"), "-Infinity");
    assert_eval(&sign_of("Math.round(-0)"), "-Infinity");
    assert_eval("Math.round(-0.6)", "-1");
    assert_eval("Math.round(Infinity)", "Infinity");
}
