//! Integration tests: Unicode-data string operations (design/symbol-strings.md
//! §6.6): `normalize` in all four forms and `localeCompare` in code-unit order.

mod common;
use common::{assert_bool, assert_number, assert_string};

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
