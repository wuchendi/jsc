//! Integration tests: Unicode-aware string semantics on code units
//! (design/symbol-strings.md §6.6): the full case mappings, `normalize` in all
//! four forms and `localeCompare` in code-unit order.

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

/// Wrap `body` so the script yields `"<error name>|<message>"` for the error
/// it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name+\"|\"+e.message }} r")
}

// normalize / localeCompare

#[test]
fn normalize_all_forms() {
    assert_bool(
        "\"\\u0041\\u030A\".normalize(\"NFC\") === \"\\u00C5\"",
        true,
    );
    assert_bool(
        "\"\\u00C5\".normalize(\"NFD\") === \"\\u0041\\u030A\"",
        true,
    );
    assert_string("\"\\uFB01\".normalize(\"NFKC\")", "fi");
    assert_bool(
        "\"\\u1E9B\\u0323\".normalize(\"NFKD\") === \"\\u0073\\u0323\\u0307\"",
        true,
    );
    assert_bool(
        "\"\\u1E9B\\u0323\".normalize(\"NFKC\") === \"\\u1E69\"",
        true,
    );
}

#[test]
fn normalize_defaults_to_nfc() {
    assert_bool("\"e\\u0301\".normalize() === \"\\u00E9\"", true);
    assert_bool("\"e\\u0301\".normalize(undefined) === \"\\u00E9\"", true);
    assert_bool(
        "const f = \"NFC\"; \"e\\u0301\".normalize(f) === \"\\u00E9\"",
        true,
    );
}

#[test]
fn normalize_invalid_form_throws_range_error() {
    assert_string(
        &caught("\"abc\".normalize(\"nfc\")"),
        "RangeError|The normalization form should be one of NFC, NFD, NFKC, NFKD.",
    );
    assert_string(
        &caught("const f = \"x\"; \"\\u00E9\".normalize(f)"),
        "RangeError|The normalization form should be one of NFC, NFD, NFKC, NFKD.",
    );
}

#[test]
fn normalize_preserves_lone_surrogates() {
    assert_bool("\"a\\uD800\".normalize(\"NFC\") === \"a\\uD800\"", true);
    assert_number("\"a\\uD800\".normalize(\"NFC\").length", 2.0);
    assert_bool(
        "\"\\uDC00e\\u0301\\uD800\".normalize(\"NFD\") === \"\\uDC00e\\u0301\\uD800\"",
        true,
    );
    assert_bool(
        "\"e\\u0301\\uDC00e\\u0301\".normalize() === \"\\u00E9\\uDC00\\u00E9\"",
        true,
    );
}

#[test]
fn locale_compare_code_unit_order() {
    assert_bool("\"a\".localeCompare(\"b\") < 0", true);
    assert_bool("\"b\".localeCompare(\"a\") > 0", true);
    assert_number("\"a\".localeCompare(\"a\")", 0.0);
    // U+FFFF is one unit 0xFFFF; U+10000 starts with the lead 0xD800.
    assert_bool("\"\\uFFFF\".localeCompare(\"\\u{10000}\") > 0", true);
    // Lone surrogates compare as their units, not as U+FFFD.
    assert_bool("\"\\uD800\".localeCompare(\"\\uDC00\") < 0", true);
    assert_bool(
        "const s = \"\\uDC00\"; s.localeCompare(\"\\uD800\") > 0",
        true,
    );
}
