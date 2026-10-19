//! Integration tests: JS `+` string coercion — when EITHER operand is a string
//! the OTHER is coerced to its string form and the two are concatenated. Number +
//! number stays numeric addition; string + string is unchanged.

use jsc_core::run::compile_and_run;
mod common;
use common::{assert_number, assert_string};

/// A string on either side coerces a numeric operand to its string form.
#[test]
fn number_operand_coerces_to_string() {
    assert_string("\"x\" + 8", "x8");
    assert_string("8 + \"x\"", "8x");
    assert_string("\"n=\" + 42", "n=42");
}

/// Non-numeric primitives (`true`/`false`/`null`/`undefined`) coerce to their
/// string forms when concatenated with a string.
#[test]
fn primitive_operands_coerce_to_string() {
    assert_string("\"x\" + true", "xtrue");
    assert_string("\"x\" + false", "xfalse");
    assert_string("\"x\" + null", "xnull");
    assert_string("\"x\" + undefined", "xundefined");
}

/// Number + number stays numeric addition (no concat path).
#[test]
fn number_plus_number_stays_numeric() {
    assert_number("40 + 2", 42.0);
}

/// String + string is unchanged plain concatenation.
#[test]
fn string_plus_string_unchanged() {
    assert_string("\"a\" + \"b\"", "ab");
}

/// The coerced operand may itself be produced by an expression, and both sides
/// may need coercion order preserved (left operand first).
#[test]
fn coercion_composes_and_preserves_order() {
    assert_string("\"n=\" + (40 + 2)", "n=42");
    assert_string("(1 + 2) + \"!\"", "3!");
    // A program can run these to completion without error.
    assert!(compile_and_run("\"x\" + 8").is_ok());
}

/// With no string operand, `+` applies `ToNumber` to both sides: `null` is `0`,
/// booleans are `0`/`1`, and `undefined` is `NaN`.
#[test]
fn non_string_primitives_add_numerically() {
    assert_number("1 + null", 1.0);
    assert_number("null + 1", 1.0);
    assert_number("null + null", 0.0);
    assert_number("true + 1", 2.0);
    assert_number("true + true", 2.0);
    assert_number("false + 1", 1.0);
    let result = compile_and_run("undefined + 1");
    assert!(
        matches!(result, Ok(jsc_core::run::Value::Number(n)) if n.is_nan()),
        "`undefined + 1` should be NaN, got {result:?}"
    );
}

/// A string on either side still concatenates, after the other side's `ToString`.
#[test]
fn string_side_still_concatenates() {
    assert_string("\"a\" + true", "atrue");
    assert_string("\"a\" + undefined", "aundefined");
    assert_string("\"x\" + null", "xnull");
    assert_string("1 + \"2\"", "12");
}

/// A heap operand takes the concat path (`ToPrimitive` yields a string): an
/// array becomes its `join()` string, even when the other side is not a string.
#[test]
fn array_operand_concatenates() {
    assert_string("[1] + 1", "11");
    assert_string("1 + [2,3]", "12,3");
    assert_string("[] + []", "");
    assert_string("null + []", "null");
    assert_string("true + [1]", "true1");
}
