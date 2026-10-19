//! Integration tests: `!==` / `!=` are exact negations of `===` / `==`, and a
//! computed NaN (e.g. `0/0`, whose sign bit may be set) compares as a number.

mod common;
use common::{assert_bool, assert_number};

#[test]
fn computed_nan_strict_not_equal_self() {
    assert_bool("let a=0/0; a !== a", true);
}

#[test]
fn computed_nan_strict_equal_self_is_false() {
    assert_bool("let a=0/0; a === a", false);
}

#[test]
fn computed_nan_loose_not_equal_self() {
    assert_bool("let a=0/0; a != a", true);
}

#[test]
fn computed_nan_loose_equal_self_is_false() {
    assert_bool("let a=0/0; a == a", false);
}

#[test]
fn negated_computed_nan_not_equal_self() {
    assert_bool("let a=-(0/0); a !== a", true);
}

#[test]
fn literal_nan_strict_not_equal() {
    assert_bool("NaN !== NaN", true);
}

#[test]
fn literal_nan_loose_not_equal() {
    assert_bool("NaN != NaN", true);
}

#[test]
fn signed_zeros_strict_equal() {
    assert_bool("0 !== -0", false);
}

#[test]
fn same_number_strict_equal() {
    assert_bool("1 !== 1", false);
}

#[test]
fn same_string_strict_equal() {
    assert_bool("'a' !== 'a'", false);
}

#[test]
fn null_loose_equals_undefined() {
    assert_bool("null != undefined", false);
}

#[test]
fn null_strict_differs_from_undefined() {
    assert_bool("null !== undefined", true);
}

#[test]
fn distinct_objects_strict_differ() {
    assert_bool("({}) !== ({})", true);
}

#[test]
fn same_object_strict_equal() {
    assert_bool("let o={}; o !== o", false);
}

#[test]
fn number_loose_equals_numeric_string() {
    assert_bool("1 != '1'", false);
}

#[test]
fn number_strict_differs_from_string() {
    assert_bool("1 !== '1'", true);
}

/// Operands of the negation matrix. `0/0` is a computed NaN, distinct in bits
/// from the `NaN` literal; the object and array appear once each so the
/// diagonal exercises identity and the off-diagonal distinct references.
const OPERANDS: [&str; 13] = [
    "NaN",
    "0/0",
    "0",
    "-0",
    "1",
    "'1'",
    "''",
    "true",
    "false",
    "null",
    "undefined",
    "{}",
    "[1]",
];

/// Build a program binding every operand to `v{i}` and counting the pairs for
/// which `negated` disagrees with `!(plain)`; the program's value is that count.
fn mismatch_program(plain: &str, negated: &str) -> String {
    let n = OPERANDS.len();
    let bindings = OPERANDS
        .iter()
        .enumerate()
        .map(|(i, operand)| format!("let v{i} = {operand};\n"));
    let checks = (0..n * n).map(|k| {
        let (i, j) = (k / n, k % n);
        format!("if ((v{i} {negated} v{j}) !== !(v{i} {plain} v{j})) {{ bad = bad + 1; }}\n")
    });
    bindings
        .chain(std::iter::once("let bad = 0;\n".to_owned()))
        .chain(checks)
        .chain(std::iter::once("bad".to_owned()))
        .collect()
}

#[test]
fn strict_not_equal_negates_strict_equal_matrix() {
    assert_number(&mismatch_program("===", "!=="), 0.0);
}

#[test]
fn loose_not_equal_negates_loose_equal_matrix() {
    assert_number(&mismatch_program("==", "!="), 0.0);
}
