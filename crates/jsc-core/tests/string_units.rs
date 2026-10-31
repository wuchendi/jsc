//! Integration tests: strings as UTF-16 code units (DESIGN.md
//! §5.2): `.length` and `s[i]` count code units, equality and `<` / `sort`
//! compare code units, concatenation mixes widths, string iteration steps by
//! code point, JSON escapes and gaps work on code units, and a String
//! wrapper's index properties are virtual (DESIGN.md §5.3).

mod common;
use common::{assert_bool, assert_number, assert_string};

/// Wrap `body` so the script yields `"<error name>|<message>"` for the error
/// it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name+\"|\"+e.message }} r")
}

#[test]
fn length_counts_code_units() {
    assert_number("\"é\".length", 1.0);
    assert_number("\"😀\".length", 2.0);
    assert_number("\"aé😀\".length", 4.0);
    assert_number("\"\\uD800\".length", 1.0);
}

#[test]
fn index_reads_one_code_unit() {
    assert_bool("\"😀\"[0] === \"\\uD83D\"", true);
    assert_bool("\"😀\"[1] === \"\\uDE00\"", true);
    assert_bool("\"aé\"[1] === \"é\"", true);
    assert_bool("\"aé\"[1] === \"\\u00e9\"", true);
    assert_number("\"😀\"[0].length", 1.0);
}

#[test]
fn lone_surrogates_stay_distinct() {
    assert_bool("\"\\uD800\" !== \"\\uDBFF\"", true);
    assert_bool("\"\\uD800\" === \"\\uD800\"", true);
    assert_bool("\"\\uD83D\\uDE00\" === \"😀\"", true);
}

#[test]
fn comparison_is_code_unit_order() {
    assert_bool("\"\\uFFFF\" < \"\\u{10000}\"", false);
    assert_bool("\"\\u{10000}\" < \"\\uFFFF\"", true);
    assert_bool("\"é\" < \"ê\"", true);
    assert_bool("\"é\" < \"Ā\"", true);
    assert_bool("\"a\" < \"é\"", true);
    assert_bool("\"Ā\" > \"é\"", true);
    // Code-unit order puts U+10000 (lead D800) before U+FFFF.
    assert_bool(
        "[\"\\uFFFF\", \"\\u{10000}\"].sort()[0] === \"\\u{10000}\"",
        true,
    );
    assert_bool(
        "[\"\\u{10000}\", \"\\uFFFF\"].sort()[1] === \"\\uFFFF\"",
        true,
    );
}

#[test]
fn concatenation_mixes_widths() {
    assert_number("(\"é\" + \"😀\").length", 3.0);
    assert_bool("\"é\" + \"😀\" === \"é😀\"", true);
    assert_bool("\"a\" + \"é\" === \"aé\"", true);
    assert_bool("\"Ā\" + \"b\" === \"Āb\"", true);
    assert_bool("(\"\" + \"😀\") === \"😀\"", true);
    assert_bool("(\"😀\" + \"\") === \"😀\"", true);
    assert_bool("(\"\\uD83D\" + \"\\uDE00\") === \"😀\"", true);
}

#[test]
fn iteration_steps_by_code_point() {
    assert_number("[...\"a😀b\"].length", 3.0);
    assert_bool("[...\"a😀b\"][1] === \"😀\"", true);
    assert_number("[...\"\\uD800\\uD800\"].length", 2.0);
    assert_number("[...\"\\uDE00\\uD83D\"].length", 2.0);
    assert_number("[...\"é\"].length", 1.0);
    assert_number("let n = 0; for (const c of \"a😀b\") n++; n", 3.0);
    assert_number("let n = 0; for (const c of \"\\uD800\\uD800\") n++; n", 2.0);
    assert_number("Array.from(\"😀\").length", 1.0);
}

#[test]
fn json_parse_unicode_escapes() {
    assert_number("JSON.parse('\"\\\\u0041\\\\uD800\"').length", 2.0);
    assert_bool(
        "JSON.parse('\"\\\\u0041\\\\uD800\"') === \"A\\uD800\"",
        true,
    );
    assert_string("JSON.parse('\"\\\\u0041\"')", "A");
    assert_bool("JSON.parse('\"\\\\u00e9\"') === \"é\"", true);
    assert_bool("JSON.parse('\"\\\\u00E9\"') === \"é\"", true);
    assert_bool("JSON.parse('\"\\\\uD83D\\\\uDE00\"') === \"😀\"", true);
    assert_bool("JSON.parse('\"é😀\"') === \"é😀\"", true);
    assert_bool("JSON.parse('{\"é\": \"😀\"}')[\"é\"] === \"😀\"", true);
    assert_string(
        &caught("JSON.parse('\"\\\\u00g0\"')"),
        "SyntaxError|JSON.parse: invalid or unsupported JSON input",
    );
    assert_string(
        &caught("JSON.parse('\"\\\\u00\"')"),
        "SyntaxError|JSON.parse: invalid or unsupported JSON input",
    );
}

#[test]
fn json_parse_two_byte_text() {
    assert_number("JSON.parse(\"[1, \\\"😀\\\", 2]\").length", 3.0);
    assert_number("JSON.parse(\"[1, \\\"😀\\\", 2]\")[2]", 2.0);
    assert_bool("JSON.parse(\"[\\\"😀\\\", \\\"a\\\"]\")[1] === \"a\"", true);
    assert_number(
        "JSON.parse(\"{\\\"a\\\": 12.5, \\\"b\\\": \\\"Ā\\\"}\").a",
        12.5,
    );
}

#[test]
fn json_stringify_escapes_lone_surrogates() {
    assert_bool(
        "JSON.stringify(\"\\uD800\") === \"\\\"\\\\ud800\\\"\"",
        true,
    );
    assert_bool(
        "JSON.stringify(\"\\uDE00\") === \"\\\"\\\\ude00\\\"\"",
        true,
    );
    assert_bool(
        "JSON.stringify(\"\\uDE00\\uD83D\") === \"\\\"\\\\ude00\\\\ud83d\\\"\"",
        true,
    );
    assert_bool("JSON.stringify(\"😀\") === \"\\\"😀\\\"\"", true);
    assert_bool(
        "JSON.stringify(\"a\\uD83D\\uDE00b\") === \"\\\"a😀b\\\"\"",
        true,
    );
    assert_bool("JSON.stringify(\"é\") === \"\\\"é\\\"\"", true);
    assert_bool("JSON.stringify(\"Ā\\n\") === \"\\\"Ā\\\\n\\\"\"", true);
    assert_number("JSON.stringify(\"\\uD800\").length", 8.0);
}

#[test]
fn json_stringify_gap_takes_ten_code_units() {
    assert_bool(
        "JSON.stringify({a: 1}, null, \"ééééééééééé\") === \"{\\néééééééééé\\\"a\\\": 1\\n}\"",
        true,
    );
    assert_bool(
        "JSON.stringify([1], null, \"😀😀😀😀😀😀\") === \"[\\n😀😀😀😀😀1\\n]\"",
        true,
    );
    assert_bool(
        "JSON.stringify([\"Ā\"], null, \" \") === \"[\\n \\\"Ā\\\"\\n]\"",
        true,
    );
}

#[test]
fn string_wrapper_index_descriptor() {
    let desc = "let d = Object.getOwnPropertyDescriptor(new String(\"é\"), 0);";
    assert_bool(
        &format!(
            "{desc} d.value === \"é\" && d.writable === false && d.enumerable === true && d.configurable === false"
        ),
        true,
    );
    assert_string(
        &format!("{desc} Object.keys(d).join()"),
        "value,writable,enumerable,configurable",
    );
    assert_bool(
        "Object.getOwnPropertyDescriptor(new String(\"é\"), 1) === undefined",
        true,
    );
}

#[test]
fn string_wrapper_keys_are_code_unit_indices() {
    assert_string("Object.keys(new String(\"aé\")).join()", "0,1");
    assert_string("Object.keys(new String(\"😀\")).join()", "0,1");
    assert_string(
        "Object.getOwnPropertyNames(new String(\"aé\")).join()",
        "0,1,length",
    );
    assert_number("new String(\"aé😀\").length", 4.0);
    assert_bool("new String(\"😀\")[1] === \"\\uDE00\"", true);
    assert_bool("3 in new String(\"aé😀\")", true);
    assert_bool("4 in new String(\"aé😀\")", false);
}

#[test]
fn string_wrapper_index_is_read_only() {
    assert_string(
        &caught("new String(\"a\")[0] = \"b\""),
        "TypeError|Cannot assign to read only property '0' of object",
    );
    assert_string(
        &caught("Object.defineProperty(new String(\"ab\"), \"0\", {value: \"z\"})"),
        "TypeError|Cannot redefine property: 0",
    );
    assert_string(
        &caught("Object.defineProperty(new String(\"ab\"), \"0\", {value: \"a\"})"),
        "none",
    );
    assert_bool("Object.isFrozen(Object.freeze(new String(\"ab\")))", true);
    assert_bool("Object.isSealed(Object.seal(new String(\"ab\")))", true);
}
