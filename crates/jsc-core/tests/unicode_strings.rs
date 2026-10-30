//! Integration tests: Unicode-aware string semantics on code units.

mod common;
use common::{assert_bool, assert_number, assert_string};

// case mapping

/// The one-byte (Latin-1) fast path: lowercasing stays Latin-1; `×` / `÷`
/// have no case pair.
#[test]
fn case_mapping_latin1() {
    assert_string("\"ÀÉ\".toLowerCase()", "àé");
    assert_string("\"àé\".toUpperCase()", "ÀÉ");
    assert_string("\"×÷\".toUpperCase()", "×÷");
    assert_string("\"×÷\".toLowerCase()", "×÷");
    assert_string("\"Hello, Wörld\".toUpperCase()", "HELLO, WÖRLD");
}

/// Uppercasing `ß`, `ÿ` and `µ` leaves Latin-1 or changes the length.
#[test]
fn case_mapping_latin1_leaves_one_byte() {
    assert_string("\"ß\".toUpperCase()", "SS");
    assert_bool("\"ÿ\".toUpperCase() === \"\\u0178\"", true);
    assert_bool("\"µ\".toUpperCase() === \"\\u039C\"", true);
    assert_string("\"aßc\".toUpperCase()", "ASSC");
}

/// The full mapping on two-byte strings: `SpecialCasing` and `Final_Sigma`.
#[test]
fn case_mapping_full() {
    assert_string("\"ΑΣ\".toLowerCase()", "ας");
    assert_string("\"ΑΣ Α\".toLowerCase()", "ας α");
    assert_number("\"İ\".toLowerCase().length", 2.0);
    assert_bool("\"İ\".toLowerCase() === \"i\\u0307\"", true);
    assert_string("\"ﬀ\".toUpperCase()", "FF");
}

/// A lone surrogate passes through unchanged as its unit.
#[test]
fn case_mapping_lone_surrogates() {
    assert_bool("\"\\uD800\".toUpperCase() === \"\\uD800\"", true);
    assert_number("\"\\uD800\".toUpperCase().length", 1.0);
    assert_bool("\"a\\uD800b\".toUpperCase() === \"A\\uD800B\"", true);
    assert_bool("\"A\\uDC00B\".toLowerCase() === \"a\\uDC00b\"", true);
}

/// `toLocaleUpperCase` / `toLocaleLowerCase` match the default mappings.
#[test]
fn case_mapping_locale() {
    assert_string("\"ß\".toLocaleUpperCase()", "SS");
    assert_bool("\"ÿ\".toLocaleUpperCase() === \"\\u0178\"", true);
    assert_string("\"ÀÉ\".toLocaleLowerCase()", "àé");
    assert_string("\"ΑΣ\".toLocaleLowerCase()", "ας");
    assert_bool("\"a\\uD800b\".toLocaleUpperCase() === \"A\\uD800B\"", true);
    assert_string("\"ﬀ\".toLocaleUpperCase()", "FF");
}
