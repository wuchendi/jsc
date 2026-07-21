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
