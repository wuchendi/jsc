//! Integration tests: the numeric operators (`- * / % **`, unary `-`/`+`,
//! `++`/`--`, compound assignment, and relational comparison of non-string
//! pairs) apply `ToPrimitive` + `ToNumber` to non-number operands.

mod common;
use common::{assert_bool, assert_nan, assert_number, assert_string, assert_type_str};

#[test]
fn binary_arithmetic_coerces_strings() {
    assert_number(r#""4" * "2""#, 8.0);
    assert_number(r#""4" - 0"#, 4.0);
    assert_type_str(r#"typeof ("4" - 0)"#, "number");
    assert_number(r#""9" / "3""#, 3.0);
    assert_number(r#""7" % "4""#, 3.0);
    assert_number(r#""2" ** "3""#, 8.0);
}

#[test]
fn unary_minus_and_plus_coerce() {
    assert_number(r#"-"3""#, -3.0);
    assert_number(r#"+"3""#, 3.0);
    assert_number(r#"+"""#, 0.0);
    assert_number(r#"+" 12 ""#, 12.0);
    assert_number(r#"+"0x10""#, 16.0);
    assert_nan(r#"+"abc""#);
    assert_type_str(r#"typeof +"3""#, "number");
}

#[test]
fn string_to_number_grammar() {
    assert_number(r#"+"Infinity""#, f64::INFINITY);
    assert_number(r#"+"-Infinity""#, f64::NEG_INFINITY);
    assert_nan(r#"+"inf""#);
    assert_nan(r#"+"infinity""#);
    assert_number(r#"+"0b101""#, 5.0);
    assert_number(r#"+"0o17""#, 15.0);
    assert_nan(r#"+"-0x10""#);
    assert_nan(r#"+"0x""#);
    assert_number(r#"Number("0x10")"#, 16.0);
}

#[test]
fn primitives_coerce() {
    assert_number("true * 3", 3.0);
    assert_number("null - 1", -1.0);
    assert_nan("undefined * 1");
}

#[test]
fn objects_coerce_via_to_primitive() {
    assert_number("[5] * 2", 10.0);
    assert_number("[] - 0", 0.0);
    assert_nan("({}) * 1");
    assert_nan("[1, 2] * 1");
}

#[test]
fn update_and_compound_assignment_coerce() {
    assert_number(r#"let s = "5"; s++; s"#, 6.0);
    assert_number(r#"let p = "5"; p++"#, 5.0);
    assert_number(r#"let q = "5"; ++q"#, 6.0);
    assert_type_str(r#"let t = "5"; t--; typeof t"#, "number");
    assert_number(r#"let u = "2"; u *= "3"; u"#, 6.0);
    assert_number(r#"let v = "8"; v -= "3"; v"#, 5.0);
    assert_number(r#"let w = "8"; w /= "2"; w"#, 4.0);
    assert_number(r#"let x = "8"; x %= "3"; x"#, 2.0);
    assert_number(r#"let y = "2"; y **= "3"; y"#, 8.0);
}

#[test]
fn relational_coerces_unless_both_strings() {
    assert_bool(r#""10" < 9"#, false);
    assert_bool(r#""10" > 9"#, true);
    assert_bool(r#""a" < 1"#, false);
    assert_bool("null >= 0", true);
    assert_bool(r#""10" < "9""#, true);
    assert_bool("[2] > 1", true);
}

#[test]
fn coerced_nan_is_canonical() {
    assert_bool(r#"[("abc" * 1)].includes(NaN)"#, true);
    assert_bool("[(undefined - 1)].includes(NaN)", true);
}

#[test]
fn plus_concatenation_unchanged() {
    assert_string(r#""4" + 2"#, "42");
    assert_string(r#"1 + "2""#, "12");
    assert_number("1 + 2", 3.0);
}

#[test]
fn captured_update_coerces() {
    assert_number(
        r#"function f() { let c = "5"; const g = () => c++; g(); return c; } f()"#,
        6.0,
    );
    assert_number(
        r#"function f() { let c = "5"; const g = () => c++; return g(); } f()"#,
        5.0,
    );
}

#[test]
fn number_builtin_uses_shared_to_number() {
    assert_number("Number([7])", 7.0);
    assert_number("Number([])", 0.0);
}
