//! Integration tests: number pipeline milestones via the CLI.

mod common;
use common::jsc;

/// The S8 milestone programs, run inline with `-p`, print their numbers.
///
/// One program per language wave: a bare expression, a `while` loop with a
/// comparison, and a function call — all multi-statement except the first.
#[test]
fn milestones_via_eval() {
    assert_eq!(jsc(&["-p", "40 + 2"]), "42");
    assert_eq!(
        jsc(&["-p", "function add(a, b) { return a + b } add(40, 2)"]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let i=0; let s=0; while(i<10){ s = s + i; i = i + 1 } s"
        ]),
        "45",
    );
}

/// Prior-milestone number programs still render `42`/`45`/`55` on the binary.
///
/// S9 migrated the whole pipeline onto an i64 NaN-boxed jsval carrier; this
/// guards that the number path survived the carrier change unchanged.
#[test]
fn number_regression_survives_jsval() {
    assert_eq!(jsc(&["-p", "40+2"]), "42");
    assert_eq!(jsc(&["-p", "let x=40;x+2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "let i=0; let s=0; while(i<10){ s = s + i; i = i + 1 } s"
        ]),
        "45",
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55",
    );
}

/// Non-finite numbers render JS-style (`Infinity`/`-Infinity`/`NaN`), not
/// Rust's `inf`/`-inf`, while finite renders stay byte-for-byte unchanged.
#[test]
fn non_finite_renders_js_style() {
    assert_eq!(jsc(&["-p", "1/0"]), "Infinity");
    assert_eq!(jsc(&["-p", "-1/0"]), "-Infinity");
    assert_eq!(jsc(&["-p", "0/0"]), "NaN");
    // Finite regressions: integers and shortest fractionals are untouched.
    assert_eq!(jsc(&["-p", "42"]), "42");
    assert_eq!(jsc(&["-p", "4.5"]), "4.5");
    assert_eq!(jsc(&["-p", "0.5"]), "0.5");
}

/// Negative zero renders as `"0"` (JS `String(-0)`), while the stored f64 sign
/// is preserved so arithmetic still distinguishes `-0` from `+0`.
#[test]
fn negative_zero_renders_zero() {
    assert_eq!(jsc(&["-p", "-0"]), "0");
    assert_eq!(jsc(&["-p", "0 * -1"]), "0");
    // Sign survives in math: 1 / -0 is -Infinity, not Infinity.
    assert_eq!(jsc(&["-p", "1 / (0 * -1)"]), "-Infinity");
    // Regression: plain and nonzero renders are unchanged.
    assert_eq!(jsc(&["-p", "0"]), "0");
    assert_eq!(jsc(&["-p", "-5"]), "-5");
    assert_eq!(jsc(&["-p", "5"]), "5");
}

/// Large/small magnitudes render in exponential form per the ECMAScript
/// `Number::toString` thresholds (values `>= 1e21` or `0 < |x| < 1e-6`), with a
/// lowercase `e`, an explicit sign, and no leading zeros in the exponent, while
/// in-range magnitudes stay in fixed form.
#[test]
fn exponential_tostring_thresholds() {
    // Exponential above/below the fixed-form window.
    assert_eq!(jsc(&["-p", "1e21"]), "1e+21");
    assert_eq!(jsc(&["-p", "1e-7"]), "1e-7");
    assert_eq!(jsc(&["-p", "1e21 + 0"]), "1e+21");
    assert_eq!(jsc(&["-p", "1.5e22"]), "1.5e+22");
    assert_eq!(jsc(&["-p", "-1e21"]), "-1e+21");
    // The boundary just inside the window is still fixed.
    assert_eq!(jsc(&["-p", "1e-6"]), "0.000001");
    // Fixed-form regressions across the ordinary range.
    assert_eq!(jsc(&["-p", "123"]), "123");
    assert_eq!(jsc(&["-p", "100000"]), "100000");
    assert_eq!(jsc(&["-p", "0.5"]), "0.5");
    assert_eq!(jsc(&["-p", "9/2"]), "4.5");
}

/// `NaN` and `Infinity` resolve as read-only global number bindings, sharing
/// the same value path as their computed counterparts.
#[test]
fn nan_infinity_global_bindings() {
    assert_eq!(jsc(&["-p", "typeof NaN"]), "number");
    assert_eq!(jsc(&["-p", "typeof Infinity"]), "number");
    assert_eq!(jsc(&["-p", "Infinity > 1e300"]), "true");
    // NaN is never equal to itself, exactly like a computed `0/0`.
    assert_eq!(jsc(&["-p", "NaN === NaN"]), "false");
    assert_eq!(jsc(&["-p", "-Infinity < 0"]), "true");
    // Rendering of the bindings themselves is unchanged.
    assert_eq!(jsc(&["-p", "Infinity"]), "Infinity");
    assert_eq!(jsc(&["-p", "-Infinity"]), "-Infinity");
    assert_eq!(jsc(&["-p", "NaN"]), "NaN");
}
