//! Integration tests: `String`/`Number`/`Boolean`/`parseInt`/`parseFloat` conversions.

mod common;
use common::{
    assert_bool, assert_nan, assert_number, assert_output, assert_string, assert_type_str,
};

/// `String(42)` yields the string `"42"` (same form as `format_number`), and its
/// runtime type is "string".
#[test]
fn string_conversion() {
    assert_string("String(42)", "42");
    assert_type_str("typeof String(42)", "string");
    assert_string("String(3.5)", "3.5");
    // Non-number operands stringify per `format_value`.
    assert_string("String(true)", "true");
    assert_string("String(false)", "false");
    assert_string("String(null)", "null");
    assert_string("String(undefined)", "undefined");
    // A string operand round-trips to the same content.
    assert_string("String(\"hi\")", "hi");
    // The result is a real heap string: it concatenates and compares by content.
    assert_string("String(42) + \"px\"", "42px");
    assert_bool("String(42) === \"42\"", true);
}

/// `Number(x)` coerces per the JS subset: strings are full-string parsed (trimmed;
/// empty => 0; non-numeric => NaN), booleans map to 1/0, null => 0, undefined =>
/// NaN, and numbers pass through.
#[test]
fn number_conversion() {
    assert_number("Number(\"42\")", 42.0);
    assert_number("Number(\"  3.5 \")", 3.5);
    assert_number("Number(\"\")", 0.0);
    assert_nan("Number(\"x\")");
    assert_number("Number(true)", 1.0);
    assert_number("Number(false)", 0.0);
    assert_number("Number(null)", 0.0);
    assert_nan("Number(undefined)");
    assert_number("Number(42)", 42.0);
    // The result is a real number: it flows into arithmetic.
    assert_number("Number(\"40\") + 2", 42.0);
    assert_type_str("typeof Number(\"1\")", "number");
}

/// `Boolean(x)` applies JS truthiness. Falsy: 0/-0/NaN, "", null, undefined,
/// false; everything else (non-empty strings, arrays, objects, nonzero numbers) is
/// true.
#[test]
fn boolean_conversion() {
    assert_bool("Boolean(0)", false);
    assert_bool("Boolean(\"\")", false);
    assert_bool("Boolean(\"x\")", true);
    assert_bool("Boolean(1)", true);
    assert_bool("Boolean(null)", false);
    assert_bool("Boolean(undefined)", false);
    assert_bool("Boolean(false)", false);
    assert_bool("Boolean(true)", true);
    assert_bool("Boolean(0 / 0)", false); // NaN
    // Arrays and objects are always truthy.
    assert_bool("Boolean([])", true);
    assert_bool("Boolean({})", true);
    assert_type_str("typeof Boolean(0)", "boolean");
}

/// `parseInt(s)` parses a leading base-10 integer, skipping whitespace and honoring
/// a sign, stopping at the first non-digit; no digits yields NaN.
#[test]
fn parse_int() {
    assert_number("parseInt(\"42px\")", 42.0);
    assert_nan("parseInt(\"abc\")");
    assert_number("parseInt(\"  -7z\")", -7.0);
    assert_number("parseInt(\"3.9\")", 3.0);
    assert_number("parseInt(\"1e3\")", 1.0);
    // `parseInt("-0")` is -0 (the sign applies to a zero value).
    assert_number("1 / parseInt(\"-0\")", f64::NEG_INFINITY);
    assert_type_str("typeof parseInt(\"1\")", "number");
}

/// `parseInt(s, radix)` honors the radix: `ToInt32(radix)` of 0 (or an omitted /
/// `undefined` radix) means 10 unless a `0x` prefix selects 16; 16 also strips an
/// optional `0x` prefix; a radix outside 2..=36 yields NaN; letters are
/// case-insensitive digits.
#[test]
fn parse_int_radix() {
    assert_number("parseInt(\"ff\", 16)", 255.0);
    assert_number("parseInt(\"0xff\")", 255.0);
    assert_number("parseInt(\"0XFF\")", 255.0);
    assert_number("parseInt(\"0xff\", 16)", 255.0);
    assert_number("parseInt(\"0xff\", 10)", 0.0);
    assert_number("parseInt(\"101\", 2)", 5.0);
    assert_number("parseInt(\"z\", 36)", 35.0);
    assert_number("parseInt(\"Z\", 36)", 35.0);
    assert_nan("parseInt(\"12\", 1)");
    assert_nan("parseInt(\"12\", 37)");
    assert_number("parseInt(\"12\", 0)", 12.0);
    assert_number("parseInt(\"12\", undefined)", 12.0);
    assert_number("parseInt(\"  -7f\", 16)", -127.0);
    assert_nan("parseInt(\"9\", 8)");
    assert_nan("parseInt(\"0x\")");
    // The radix goes through ToInt32: 2^32 + 16 wraps to 16, "16" coerces.
    assert_number("parseInt(\"ff\", 4294967312)", 255.0);
    assert_number("parseInt(\"ff\", \"16\")", 255.0);
    assert_number("parseInt(\"11\", 2.9)", 3.0);
}

/// `parseFloat(s)` parses a leading decimal float, including a fraction or
/// exponent; no numeric prefix yields NaN.
// `3.14` is an acceptance-criteria input, not an approximation of PI.
#[allow(clippy::approx_constant)]
#[test]
fn parse_float() {
    assert_number("parseFloat(\"3.14xyz\")", 3.14);
    assert_number("parseFloat(\"  .5\")", 0.5);
    assert_number("parseFloat(\"1e3\")", 1000.0);
    assert_nan("parseFloat(\"abc\")");
}

/// `Number(str)` and `parseFloat(str)` skip every `StrWhiteSpaceChar` (the
/// Unicode `WhiteSpace` and `LineTerminator` code points, U+000B and U+FEFF).
#[test]
fn string_to_number_trims_unicode_whitespace() {
    let ws = "\\u0009\\u000B\\u000C\\u0020\\u00A0\\u000A\\u000D\\u2028\\u2029\\u1680\\u2000\\u200A\\u202F\\u205F\\u3000\\uFEFF";
    assert_number(&format!("Number(\"{ws}\")"), 0.0);
    assert_number(&format!("Number(\"{ws}12{ws}\")"), 12.0);
    assert_number(&format!("+\"{ws}-Infinity{ws}\""), f64::NEG_INFINITY);
    assert_number(&format!("parseFloat(\"{ws}1.5e1x\")"), 15.0);
    assert_nan("Number(\"\\u0085 1\")");
}

/// The conversions are bare global names, not real bindings, so a user binding of
/// the same name shadows the built-in (mirrors `Math`): the call then resolves to
/// the user value (here a function value invoked indirectly).
#[test]
fn conversions_are_shadowable() {
    // `let String = ..` shadows the built-in `String`.
    assert_number("let String = function (x) { return 99 }; String(1)", 99.0);
    // `let Number = ..` shadows the built-in `Number`.
    assert_number(
        "let Number = function (x) { return 7 }; Number(\"42\")",
        7.0,
    );
    // `let Boolean = ..` shadows the built-in `Boolean`.
    assert_number("let Boolean = function (x) { return 5 }; Boolean(0)", 5.0);
    // A user `function parseInt(){}` declaration also shadows the built-in.
    assert_number("function parseInt(s) { return 1 } parseInt(\"42px\")", 1.0);
}

/// Conversion results flow through `console.log` like any value.
#[test]
fn conversions_through_console_log() {
    assert_output(
        "console.log(String(42), Number(\"8\"), Boolean(1))",
        "42 8 true\n",
    );
}

/// `String(v)`, `'' + v`, template literals, and `join` use JS `ToString`
/// text (not the console rendering): arrays join their elements, plain objects
/// are `[object Object]`, errors use `Error.prototype.toString`, and a cyclic
/// array joins its self-reference as the empty string.
#[test]
fn to_string_uses_js_text() {
    assert_output(
        "console.log(String([1, 2])); console.log([1, [2, 3]].join('-')); \
         console.log('' + [1, 'a', null, undefined]); console.log(String({}), `${{ a: 1 }}`)",
        "1,2\n1-2,3\n1,a,,\n[object Object] [object Object]\n",
    );
    assert_output(
        "console.log(String(new TypeError('x')), String(new Error()), '' + new RangeError('r'))",
        "TypeError: x Error RangeError: r\n",
    );
    assert_output(
        "let e = new Error('m'); e.name = ''; console.log(String(e)); \
         let a = [1, 0]; a[1] = a; console.log(String(a), a.join('-'))",
        "m\n1, 1-\n",
    );
    assert_output(
        "console.log('x'.padStart(4, [1, 2]), 'a1,2b'.split([1, 2]), parseFloat([2.5]))",
        "1,2x [ a, b ] 2.5\n",
    );
}
