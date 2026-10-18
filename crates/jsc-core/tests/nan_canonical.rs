//! Integration tests: every NaN that f64 arithmetic produces (e.g. `0/0`, whose
//! raw bits may carry the sign bit, or `Math.sqrt(-1)`) behaves exactly like the
//! `NaN` literal — the number NaN is always reboxed as the canonical bit pattern.

mod common;
use common::{assert_bool, assert_nan, assert_number, assert_string, assert_type_str};

#[test]
fn computed_nan_includes_itself() {
    assert_bool("let a=0/0; [a].includes(a)", true);
}

#[test]
fn computed_nan_includes_nan_literal() {
    assert_bool("[0/0].includes(NaN)", true);
    assert_bool("[NaN].includes(0/0)", true);
    assert_bool("[Math.sqrt(-1)].includes(NaN)", true);
}

#[test]
fn nan_literal_includes_itself() {
    assert_bool("[NaN].includes(NaN)", true);
}

#[test]
fn computed_nan_index_of_stays_not_found() {
    assert_number("[0/0].indexOf(0/0)", -1.0);
    assert_number("[NaN].indexOf(NaN)", -1.0);
}

#[test]
fn computed_nan_is_falsy() {
    assert_number("(0/0) ? 1 : 2", 2.0);
    assert_bool("!(0/0)", true);
    assert_bool("!Math.sqrt(-1)", true);
}

#[test]
fn computed_nan_set_key_dedupes_with_nan_literal() {
    assert_number("let s=new Set(); s.add(0/0); s.add(NaN); s.size", 1.0);
    assert_bool("let s=new Set(); s.add(NaN); s.has(0/0)", true);
}

#[test]
fn computed_nan_map_key_matches_nan_literal() {
    assert_number("let m=new Map(); m.set(0/0, 1); m.get(NaN)", 1.0);
}

#[test]
fn map_keys_keep_signed_zero_equal() {
    assert_number("let m=new Map(); m.set(-0, 7); m.get(0)", 7.0);
}

#[test]
fn computed_nan_renders_and_types_as_number() {
    assert_string("String(0/0)", "NaN");
    assert_type_str("typeof (0/0)", "number");
    assert_string("JSON.stringify([0/0])", "[null]");
}

#[test]
fn nan_from_non_number_operand_is_a_number() {
    // `undefined - 0` must not leak the `undefined` box bits through arithmetic.
    assert_type_str("let u; typeof (u - 0)", "number");
    assert_bool("let u; let x = u - 0; x === x", false);
    assert_nan("let u; u * 1");
}

#[test]
fn computed_nan_from_math_and_increment() {
    assert_bool("[Math.max(1, 0/0)].includes(NaN)", true);
    assert_bool("[Math.pow(0/0, 2)].includes(NaN)", true);
    assert_bool("let a=0/0; a++; [a].includes(NaN)", true);
    assert_bool("[(0/0) % 2].includes(NaN)", true);
}

#[test]
fn negated_nan_map_key_matches_nan_literal() {
    // `-NaN` folds to a sign-bit NaN constant; `-(0/0)` negates at runtime.
    assert_number("let m=new Map(); m.set(-NaN, 1); m.get(NaN)", 1.0);
    assert_number("let a=0/0; let m=new Map(); m.set(-a, 2); m.get(NaN)", 2.0);
}
