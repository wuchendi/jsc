//! Integration tests: Unicode-aware string semantics on code units
//! (DESIGN.md §7.9): the full case mappings, `normalize` in all
//! four forms and `localeCompare` in code-unit order.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string};

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

/// The table-driven mapping: multi-unit `SpecialCasing`, supplementary
/// scalars, narrowing, and `Final_Sigma` over case-ignorable context.
#[test]
fn case_map_special_casing_and_final_sigma() {
    assert_string("\"ﬃ\".toUpperCase()", "FFI");
    assert_bool("\"\\u0149\".toUpperCase() === \"\\u02BCN\"", true);
    assert_bool(
        "\"\\u0390\".toUpperCase() === \"\\u0399\\u0308\\u0301\"",
        true,
    );
    assert_bool("\"x\\u0130\".toLowerCase() === \"xi\\u0307\"", true);
    assert_bool("\"\\u{10400}\".toLowerCase() === \"\\u{10428}\"", true);
    assert_bool("\"\\u{10428}\".toUpperCase() === \"\\u{10400}\"", true);
    assert_bool(
        "\"\\u0100\\u0102\".toLowerCase() === \"\\u0101\\u0103\"",
        true,
    );
    assert_bool("\"\\u0178\".toLowerCase() === \"ÿ\"", true);
    assert_string("\"Σ\".toLowerCase()", "σ");
    assert_string("\"ΑΣ\".toLowerCase()", "ας");
    assert_string("\"ΑΣΑ\".toLowerCase()", "ασα");
    assert_string("\"Α'Σ\".toLowerCase()", "α'ς");
    assert_string("\"ΑΣ'Α\".toLowerCase()", "ασ'α");
    assert_string("\"ΑΣ'\".toLowerCase()", "ας'");
    assert_string("\"1Σ\".toLowerCase()", "1σ");
}

/// A lone surrogate passes through and ends the `Final_Sigma` context; a
/// well-formed pair stays one scalar.
#[test]
fn case_map_lone_surrogate_passthrough() {
    assert_bool("\"\\uD800ß\".toUpperCase() === \"\\uD800SS\"", true);
    assert_bool(
        "\"\\uDC00\\u0101\".toUpperCase() === \"\\uDC00\\u0100\"",
        true,
    );
    assert_bool(
        "\"\\uD83D\\uDE00a\\uD83D\".toUpperCase() === \"\\uD83D\\uDE00A\\uD83D\"",
        true,
    );
    assert_bool("\"Α\\uD800Σ\".toLowerCase() === \"α\\uD800σ\"", true);
    assert_bool("\"ΑΣ\\uD800Α\".toLowerCase() === \"ας\\uD800α\"", true);
}

/// A key is bare only when every character is an identifier character; past
/// ASCII that is the `Alphanumeric` table, by code point.
#[test]
fn render_key_non_ascii_alphanumeric() {
    assert_output("console.log({ é: 1, 中: 2 })", "{ é: 1, 中: 2 }\n");
    assert_output("console.log({ \"\\u{1D400}\": 1 })", "{ 𝐀: 1 }\n");
    assert_output("console.log({ \"€\": 1 })", "{ '€': 1 }\n");
    assert_output("console.log({ \"a😀\": 1 })", "{ 'a😀': 1 }\n");
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

/// A field of the conformance file (space-separated hex) as a JS literal.
fn js_field(field: &str) -> String {
    let escapes: Vec<String> = field
        .split_whitespace()
        .map(|cp| format!("\\u{{{cp}}}"))
        .collect();
    format!("\"{}\"", escapes.concat())
}

/// The UAX #15 invariants of `NormalizationTest.txt` over the fixture sample:
/// each form, per column, maps to the expected column.
#[test]
fn normalize_all_forms_conformance_sample() {
    let rows: Vec<String> = include_str!("normalization_sample.txt")
        .lines()
        .filter(|line| !line.starts_with(['#', '@']))
        .map(|line| {
            let fields: Vec<String> = line.split(';').map(js_field).collect();
            format!("[{}]", fields.join(","))
        })
        .collect();
    assert!(rows.len() > 500, "fixture has {} rows", rows.len());
    let program = format!(
        "const rows = [{}];
         const forms = [
           [\"NFC\", [1, 1, 1, 3, 3]],
           [\"NFD\", [2, 2, 2, 4, 4]],
           [\"NFKC\", [3, 3, 3, 3, 3]],
           [\"NFKD\", [4, 4, 4, 4, 4]],
         ];
         let r = \"ok\";
         for (let i = 0; i < rows.length; i++) {{
           for (let f = 0; f < forms.length; f++) {{
             for (let c = 0; c < 5; c++) {{
               if (r === \"ok\" && rows[i][c].normalize(forms[f][0]) !== rows[i][forms[f][1][c]]) {{
                 r = \"row \" + i + \" \" + forms[f][0] + \" column \" + c;
               }}
             }}
           }}
         }}
         r",
        rows.join(",")
    );
    assert_string(&program, "ok");
}

/// A form that is not exactly one of the four names throws, wherever it came
/// from, and the form is coerced before it is checked.
#[test]
fn normalize_invalid_form_range_error() {
    let text = "RangeError|The normalization form should be one of NFC, NFD, NFKC, NFKD.";
    for form in [
        "\"\"", "\"NF\"", "\"NFK\"", "\"NFCC\"", "\"NFKE\"", "\"nfd\"", "\"NFC \"", "null", "1",
    ] {
        assert_string(&caught(&format!("\"a\".normalize({form})")), text);
        assert_string(
            &caught(&format!("const f = {form}; \"\\u0100\".normalize(f)")),
            text,
        );
    }
    assert_string(
        &caught("\"a\".normalize({ toString() { return \"NFKD\"; } })"),
        "none",
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

// string iterator

/// A valid surrogate pair is one step; any other unit (a lone surrogate
/// included) is its own step.
#[test]
fn string_iterator_steps_by_code_point() {
    assert_number("[...\"a😀b\"].length", 3.0);
    assert_bool("[...\"a😀b\"][1] === \"😀\"", true);
    assert_number("[...\"\\uD800\\uD800\"].length", 2.0);
    assert_number("let n = 0; for (const c of \"😀\") n++; n", 1.0);
    assert_number("Array.from(\"😀\").length", 1.0);
    assert_bool("const [a, b] = \"é😀\"; b === \"😀\"", true);
}

/// `%StringIteratorPrototype%.next` brand-checks its receiver and stays done
/// once exhausted.
#[test]
fn string_iterator_next() {
    assert_string(
        &caught("\"😀\"[Symbol.iterator]().next.call({})"),
        "TypeError|next method called on incompatible receiver",
    );
    assert_bool(
        "const it = \"a\"[Symbol.iterator](); it.next(); it.next().done",
        true,
    );
}
