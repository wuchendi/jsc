//! Integration tests: string methods and receiver-type method dispatch.

mod common;
use common::{assert_bool, assert_eval, assert_nan, assert_number, assert_output, assert_string};

/// String `toUpperCase`/`toLowerCase` are ASCII case folds producing NEW strings.
#[test]
fn string_case_fold() {
    assert_string("\"abc\".toUpperCase()", "ABC");
    assert_string("\"ABC\".toLowerCase()", "abc");
    assert_string("\"Hi There\".toUpperCase()", "HI THERE");
    assert_string("\"Hi There\".toLowerCase()", "hi there");
}

/// `toLocaleUpperCase`/`toLocaleLowerCase` (no locale data) fold like
/// `toUpperCase`/`toLowerCase`, generic over a coercible `this`.
#[test]
fn string_locale_case_fold() {
    assert_string("\"Hi There\".toLocaleUpperCase()", "HI THERE");
    assert_string("\"Hi There\".toLocaleLowerCase()", "hi there");
    assert_string(
        "const u = String.prototype.toLocaleUpperCase; u.name + u.length + String.prototype.toLocaleLowerCase.name",
        "toLocaleUpperCase0toLocaleLowerCase",
    );
    assert_string(
        "String.prototype.toLocaleUpperCase.call({ toString() { return 'ab'; } })",
        "AB",
    );
    assert_bool(
        "let ok = false; try { String.prototype.toLocaleLowerCase.call(null); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
}

/// String `indexOf(sub)` is the first BYTE index of `sub` (empty `sub` => `0`),
/// or `-1` when absent.
#[test]
fn string_index_of() {
    assert_number("\"hello\".indexOf(\"ll\")", 2.0);
    assert_number("\"hello\".indexOf(\"z\")", -1.0);
    assert_number("\"hello\".indexOf(\"\")", 0.0);
    assert_number("\"hello\".indexOf(\"h\")", 0.0);
}

/// String `slice(a, b)` is the substring `[a, b)` by BYTE index (JS negative/clamp
/// semantics; `b` omitted => to end), a NEW string.
#[test]
fn string_slice() {
    assert_string("\"HELLO\".slice(1,3)", "EL");
    assert_string("\"hello\".slice(-3)", "llo");
    assert_string("\"hello\".slice(1)", "ello");
    assert_string("\"hello\".slice(0,0)", "");
}

/// String `charAt(i)` is a one-char string at byte index `i`, or `""` when out of
/// range (including negative / non-existent indices).
#[test]
fn string_char_at() {
    assert_string("\"abc\".charAt(1)", "b");
    assert_string("\"abc\".charAt(0)", "a");
    assert_string("\"abc\".charAt(9)", "");
    assert_string("\"abc\".charAt(-1)", "");
}

/// `indexOf`/`slice` exist on BOTH arrays and strings; dispatch is on the
/// receiver's runtime TYPE BYTE, so the same call name picks the right semantics.
#[test]
fn overloaded_methods_dispatch_on_receiver_type() {
    // slice: array yields an array, string yields a string.
    assert_number("[1,2,3,4].slice(1,3).length", 2.0);
    assert_string("\"abcd\".slice(1,3)", "bc");
    // indexOf: array is strict-equality index, string is substring index.
    assert_number("[7,8,9].indexOf(9)", 2.0);
    assert_number("\"abcabc\".indexOf(\"ca\")", 2.0);
    // A bound-variable receiver dispatches identically.
    assert_number("let a=[5,10,15]; a.indexOf(10)", 1.0);
    assert_string("let s=\"Hello\"; s.slice(0,2)", "He");
}

/// The new methods chain and compose with each other and with `.length`/indexing.
#[test]
fn methods_chain_and_compose() {
    assert_string("[1,2,3,4].slice(1,3).join(\"-\")", "2-3");
    assert_number("\"hello world\".slice(0,5).indexOf(\"llo\")", 2.0);
    assert_string("\"abc\".toUpperCase().charAt(1)", "B");
    assert_bool("[1,2,3,4].slice(1,3).includes(3)", true);
}

/// The new methods do not disturb ordinary element indexing or `.length`.
#[test]
fn methods_do_not_disturb_indexing() {
    assert_number("let a=[7,8,9]; a[1]", 8.0);
    assert_number("let a=[7,8,9]; a.length", 3.0);
    assert_string("\"abc\"[1]", "b");
    assert_number("\"abc\".length", 3.0);
}

/// String methods never reject an argument count at compile time: extra
/// arguments are evaluated and ignored, missing ones are `undefined`.
#[test]
fn method_arity_is_runtime() {
    assert_string("\"abc\".charAt()", "a");
    assert_string("\"abc\".toUpperCase(1)", "ABC");
    assert_string("\"x\".trim(1)", "x");
    assert_string("\"x\".repeat()", "");
    assert_bool("\"undefinedx\".startsWith()", true);
    assert_string("\"x\".padStart(3,\"_\",9)", "__x");
    assert_number("\"a,b\".split(\",\",1).length", 1.0);
    assert_output("let n = 0; \"a\".trim(n++, n++); console.log(n)", "2\n");
}

/// String `trim`/`trimStart`/`trimEnd` remove ASCII whitespace, producing NEW
/// strings; interior whitespace and non-whitespace are untouched.
#[test]
fn string_trim() {
    assert_string("\"  hi  \".trim()", "hi");
    assert_string("\"\\t\\nhi\\r\\n\".trim()", "hi");
    assert_string("\"  a b  \".trimStart()", "a b  ");
    assert_string("\"  a b  \".trimEnd()", "  a b");
    assert_string("\"nospace\".trim()", "nospace");
    assert_string("\"   \".trim()", "");
}

/// String `repeat(n)` concatenates `n` copies; `0` and a NaN count yield `""`.
#[test]
fn string_repeat() {
    assert_string("\"ab\".repeat(3)", "ababab");
    assert_string("\"x\".repeat(1)", "x");
    assert_string("\"ab\".repeat(0)", "");
    assert_string("\"ab\".repeat(NaN)", "");
    assert_string("\"\".repeat(5)", "");
}

/// String `startsWith`/`endsWith` are boolean prefix/suffix tests; the argument is
/// coerced to its string form.
#[test]
fn string_starts_ends_with() {
    assert_bool("\"hello\".startsWith(\"he\")", true);
    assert_bool("\"hello\".startsWith(\"lo\")", false);
    assert_bool("\"hello\".endsWith(\"lo\")", true);
    assert_bool("\"hello\".endsWith(\"he\")", false);
    assert_bool("\"hello\".startsWith(\"\")", true);
    // A number argument coerces to its string form.
    assert_bool("\"5abc\".startsWith(5)", true);
}

/// String `includes(sub)` is boolean substring containment (empty `sub` => true) —
/// the array/string overload dispatches on the receiver's TYPE BYTE.
#[test]
fn string_includes() {
    assert_bool("\"hello\".includes(\"ell\")", true);
    assert_bool("\"hello\".includes(\"z\")", false);
    assert_bool("\"hello\".includes(\"\")", true);
    // includes stays array-capable (strict-equality membership).
    assert_bool("[1,2,3].includes(2)", true);
    assert_bool("[1,2,3].includes(9)", false);
}

/// String `padStart`/`padEnd` pad to a code-unit length with a repeated pad string
/// (default `" "`); an already-long-enough receiver is returned unchanged.
#[test]
fn string_pad() {
    assert_string("\"5\".padStart(3,\"0\")", "005");
    assert_string("\"5\".padStart(3)", "  5");
    assert_string("\"5\".padEnd(3,\"*\")", "5**");
    assert_string("\"abcd\".padStart(2,\"x\")", "abcd");
    assert_string("\"ab\".padStart(5,\"xy\")", "xyxab");
    assert_string("\"ab\".padEnd(5,\"xy\")", "abxyx");
}

/// String `substring(a, b)` is the `[a, b)` BYTE slice with negatives/NaN clamped
/// to `0` and the endpoints swapped when `a > b`.
#[test]
fn string_substring() {
    assert_string("\"hello\".substring(1,3)", "el");
    assert_string("\"hello\".substring(3,1)", "el");
    assert_string("\"hello\".substring(-2,3)", "hel");
    assert_string("\"hello\".substring(2)", "llo");
    assert_string("\"hello\".substring(0,100)", "hello");
}

/// String `split(sep)` yields an array of substrings: omitted `sep` => the whole
/// string, `""` => one element per byte, otherwise split on each occurrence.
#[test]
fn string_split() {
    assert_number("\"a,b,c\".split(\",\").length", 3.0);
    assert_string("\"a,b,c\".split(\",\")[1]", "b");
    assert_string("\"a,,b\".split(\",\")[1]", "");
    assert_number("\"abc\".split(\"\").length", 3.0);
    assert_string("\"abc\".split(\"\")[2]", "c");
    assert_number("\"abc\".split().length", 1.0);
    assert_string("\"abc\".split()[0]", "abc");
    // A split result is an ordinary array (HOF-capable).
    assert_string("\"a-b-c\".split(\"-\").join(\"+\")", "a+b+c");
}

/// The string methods compose with bound-variable receivers and each other.
#[test]
fn string_methods_compose() {
    assert_string("let s=\"  Hello  \"; s.trim().toUpperCase()", "HELLO");
    assert_bool("\"  yes \".trim().startsWith(\"yes\")", true);
    assert_string("\"ab\".repeat(2).slice(1,3)", "ba");
}

/// `indexOf`/`includes`/`startsWith`/`endsWith` take a position, `split` a
/// limit; arguments are coerced with `ToString` / `ToNumber`.
#[test]
fn string_method_positions_and_coercion() {
    assert_number("\"abcabc\".indexOf(\"b\", 2)", 4.0);
    assert_number("\"abcabc\".indexOf(\"b\", -5)", 1.0);
    assert_number("\"abc\".indexOf(\"\", 9)", 3.0);
    assert_bool("\"abc\".includes(\"a\", 1)", false);
    assert_bool("\"abc\".startsWith(\"b\", 1)", true);
    assert_bool("\"abc\".endsWith(\"b\", 2)", true);
    assert_number("\"a1b\".indexOf(1)", 1.0);
    assert_number("\"xundefined\".indexOf()", 1.0);
    assert_string("\"abc\".slice({ valueOf() { return 1 } })", "bc");
    assert_string("\"a,b,c\".split(\",\", 2).join(\"|\")", "a|b");
    assert_number("\"a,b,c\".split(\",\", 0).length", 0.0);
}

/// `repeat` throws a `RangeError` for a negative or infinite count.
#[test]
fn string_repeat_range_error() {
    let caught = |body: &str| {
        format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name+\"|\"+e.message }} r")
    };
    assert_string(
        &caught("\"ab\".repeat(-1)"),
        "RangeError|Invalid count value: -1",
    );
    assert_string(
        &caught("\"ab\".repeat(Infinity)"),
        "RangeError|Invalid count value: Infinity",
    );
}

/// DF12: user methods named like string methods are called, and overriding a
/// `String.prototype` method is honoured by every call site.
#[test]
fn string_method_names_honour_user_methods() {
    assert_string(
        "let o = { indexOf() { return \"u1\" }, slice() { return \"u2\" }, toUpperCase() { return \"u3\" } }; o.indexOf(1) + o.slice() + o.toUpperCase()",
        "u1u2u3",
    );
    assert_string(
        "class C { trim() { return \"t\" } split(s) { return [s] } } let c = new C(); c.trim() + c.split(\"x\")[0]",
        "tx",
    );
    assert_string(
        "String.prototype.toUpperCase = function () { return \"P\" }; \"a\".toUpperCase()",
        "P",
    );
    assert_string(
        "String.prototype.indexOf = function (x) { return \"I\" + x }; \"a\".indexOf(\"q\")",
        "Iq",
    );
    assert_string(
        "String.prototype.foo = function () { return this + \"!\" }; \"a\".foo()",
        "a!",
    );
}

/// Spread calls on string methods.
#[test]
fn string_method_spread_calls() {
    assert_number("let a = [\"b\", 2]; \"abcab\".indexOf(...a)", 4.0);
    assert_string("let a = [1, 3]; \"hello\".slice(...a)", "el");
    assert_string("let a = []; \"Hi\".toUpperCase(...a)", "HI");
}

/// Smoke test: a classic for-loop `FizzBuzz` that leans on `trim` to
/// strip per-line padding.
/// It composes for/if/`%`/`===`, string concatenation, `String(i)`, `.trim()`,
/// and `console.log`, and must print the canonical 1..15 `FizzBuzz` lines.
#[test]
fn fizzbuzz_with_trim_smoke() {
    let src = r#"
        for (let i = 1; i <= 15; i = i + 1) {
          let line = "";
          if (i % 3 === 0) line = line + "Fizz";
          if (i % 5 === 0) line = line + "Buzz";
          if (line === "") line = String(i);
          console.log(("  " + line + "  ").trim());
        }
    "#;
    assert_output(
        src,
        "1\n2\nFizz\n4\nBuzz\nFizz\n7\n8\nFizz\nBuzz\n11\nFizz\n13\n14\nFizzBuzz\n",
    );
}

// --- String statics and the remaining prototype methods --------------------------

/// Wrap `body` so the script yields `"<error name>|<message>"` for the error
/// it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name+\"|\"+e.message }} r")
}

/// Assert `body` throws a `TypeError` whose message contains `part`.
fn assert_type_error_with(body: &str, part: &str) {
    let src = format!(
        "let r=\"none\"; try {{ {body} }} catch(e) {{ r=(e instanceof TypeError)+\"|\"+e.message.includes({part:?}) }} r"
    );
    assert_string(&src, "true|true");
}

#[test]
fn from_char_code() {
    assert_string("String.fromCharCode(104, 105)", "hi");
    assert_string("String.fromCharCode()", "");
    assert_string("String.fromCharCode(65 + 65536)", "A");
    assert_string(
        "String.fromCharCode(\"66\", { valueOf() { return 67 } })",
        "BC",
    );
    assert_string("String.fromCharCode(233)", "\u{e9}");
    assert_string("String.fromCharCode(0xD83D, 0xDE00)", "\u{1F600}");
    assert_number("String.fromCharCode.length", 1.0);
    assert_string("String.fromCharCode.name", "fromCharCode");
    assert_string(
        "let f = String.fromCharCode; f(97, 98, 99, 100, 101, 102)",
        "abcdef",
    );
    assert_bool("String.fromCharCode(0xD800) === \"\\uD800\"", true);
}

#[test]
fn from_code_point() {
    assert_string("String.fromCodePoint(104, 105)", "hi");
    assert_string("String.fromCodePoint(0x1F600)", "\u{1F600}");
    assert_string("String.fromCodePoint()", "");
    assert_number("String.fromCodePoint.length", 1.0);
    for (bad, shown) in [
        ("-1", "-1"),
        ("1.5", "1.5"),
        ("0x110000", "1114112"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("undefined", "NaN"),
        ("\"x\"", "NaN"),
    ] {
        assert_string(
            &caught(&format!("String.fromCodePoint({bad})")),
            &format!("RangeError|Invalid code point {shown}"),
        );
    }
    assert_bool("String.fromCodePoint(0xDC00) === \"\\uDC00\"", true);
    assert_bool(
        "String.fromCodePoint(0xD83D, 0xDE00) === String.fromCodePoint(0x1F600)",
        true,
    );
}

#[test]
fn string_raw() {
    assert_string("String.raw({ raw: [\"a\", \"b\", \"c\"] }, 1, 2)", "a1b2c");
    assert_string("String.raw({ raw: [\"a\", \"b\", \"c\"] }, 1)", "a1bc");
    assert_string("String.raw({ raw: [\"x\"] }, 1, 2)", "x");
    assert_string("String.raw({ raw: { length: 0 } })", "");
    assert_string("String.raw({ raw: \"abc\" }, \"-\", \"+\")", "a-b+c");
    assert_string(
        "String.raw({ raw: { length: 2, 0: 5, 1: true } }, null)",
        "5nulltrue",
    );
    assert_number("String.raw.length", 1.0);
    assert_string(
        &caught("String.raw()"),
        "TypeError|Cannot convert undefined or null to object",
    );
    assert_string(
        &caught("String.raw({})"),
        "TypeError|Cannot convert undefined or null to object",
    );
}

#[test]
fn at_char_code_at_code_point_at() {
    assert_string("\"abc\".at(0)", "a");
    assert_string("\"abc\".at(-1)", "c");
    assert_eval("\"abc\".at(3)", "undefined");
    assert_eval("\"abc\".at(-4)", "undefined");
    assert_string("\"abc\".at()", "a");
    assert_number("\"abc\".charCodeAt(1)", 98.0);
    assert_number("\"abc\".charCodeAt()", 97.0);
    assert_nan("\"abc\".charCodeAt(3)");
    assert_nan("\"abc\".charCodeAt(-1)");
    assert_number("\"abc\".codePointAt(2)", 99.0);
    assert_eval("\"abc\".codePointAt(3)", "undefined");
    assert_number("\"abc\".charCodeAt(\"1\")", 98.0);
    assert_number("\"abc\".charCodeAt(1.9)", 98.0);
    assert_number("\"\u{e9}t\u{e9}\".charCodeAt(0)", 233.0);
    assert_string("\"\u{e9}\".at(0)", "\u{e9}");
    assert_number("\"\u{e9}\".codePointAt(0)", 233.0);
}

#[test]
fn concat_last_index_of_locale_compare() {
    assert_string(
        "\"a\".concat(\"b\", 1, null, undefined, [2, 3])",
        "ab1nullundefined2,3",
    );
    assert_string("\"a\".concat()", "a");
    assert_string("\"a\".concat({ toString() { return \"T\" } })", "aT");
    assert_number("String.prototype.concat.length", 1.0);
    assert_string("\"x\".concat(1, 2, 3, 4, 5, 6, 7, 8)", "x12345678");
    assert_number("\"canal\".lastIndexOf(\"a\")", 3.0);
    assert_number("\"canal\".lastIndexOf(\"a\", 2)", 1.0);
    assert_number("\"canal\".lastIndexOf(\"a\", 0)", -1.0);
    assert_number("\"canal\".lastIndexOf(\"x\")", -1.0);
    assert_number("\"canal\".lastIndexOf(\"\")", 5.0);
    assert_number("\"canal\".lastIndexOf(\"\", 2)", 2.0);
    assert_number("\"canal\".lastIndexOf(\"a\", NaN)", 3.0);
    assert_number("\"abab\".lastIndexOf(\"ab\", -5)", 0.0);
    assert_number("String.prototype.lastIndexOf.length", 1.0);
    assert_number("\"a\".localeCompare(\"b\")", -1.0);
    assert_number("\"b\".localeCompare(\"a\")", 1.0);
    assert_number("\"a\".localeCompare(\"a\")", 0.0);
    assert_number("\"a\".localeCompare()", -1.0);
    assert_number("\"undefined\".localeCompare()", 0.0);
}

#[test]
fn normalize_forms() {
    assert_string("\"abc\".normalize()", "abc");
    for form in ["NFC", "NFD", "NFKC", "NFKD"] {
        assert_string(&format!("\"abc\".normalize(\"{form}\")"), "abc");
    }
    assert_string("\"abc\".normalize(undefined)", "abc");
    assert_string(
        &caught("\"abc\".normalize(\"nfc\")"),
        "RangeError|The normalization form should be one of NFC, NFD, NFKC, NFKD.",
    );
    assert_number("String.prototype.normalize.length", 0.0);
}

#[test]
fn search_and_match_with_string_patterns() {
    assert_number("\"hello\".search(\"ll\")", 2.0);
    assert_number("\"hello\".search(\"x\")", -1.0);
    assert_number("\"hello\".search()", 0.0);
    assert_string(
        "let m = \"hello\".match(\"ll\"); m.length + \",\" + m[0] + \",\" + m.index + \",\" + m.input + \",\" + m.groups",
        "1,ll,2,hello,undefined",
    );
    assert_eval("\"hello\".match(\"x\")", "null");
    assert_string("\"hello\".match()[0]", "");
    assert_type_error_with("\"a.b\".search(\".\")", "RegExp");
    assert_type_error_with("\"a.b\".match(\"a+\")", "RegExp");
    assert_type_error_with("\"aa\".matchAll(\"a\")", "RegExp");
    assert_number("String.prototype.matchAll.length", 1.0);
}

#[test]
fn replace_with_string_patterns() {
    assert_string("\"aXbX\".replace(\"X\", \"-\")", "a-bX");
    assert_string("\"aXbX\".replaceAll(\"X\", \"-\")", "a-b-");
    assert_string("\"abc\".replace(\"z\", \"-\")", "abc");
    assert_string("\"abc\".replace(\"\", \"-\")", "-abc");
    assert_string("\"abc\".replaceAll(\"\", \"-\")", "-a-b-c-");
    assert_string("\"abc\".replace(\"b\", \"[$&]\")", "a[b]c");
    assert_string("\"abc\".replace(\"b\", \"[$$]\")", "a[$]c");
    assert_string("\"abc\".replace(\"b\", \"[$`]\")", "a[a]c");
    assert_string("\"abc\".replace(\"b\", \"[$']\")", "a[c]c");
    assert_string("\"abc\".replace(\"b\", \"[$1$<n>$]\")", "a[$1$<n>$]c");
    assert_string("\"abcb\".replaceAll(\"b\", \"<$`>\")", "a<a>c<abc>");
    assert_string("\"abc\".replace(\"b\", 5)", "a5c");
    assert_string(
        "\"xaya\".replace(\"a\", function (m, p, s) { return m + p + s.length })",
        "xa14ya",
    );
    assert_string(
        "\"xaya\".replaceAll(\"a\", (m, p, s) => \"(\" + p + \")\")",
        "x(1)y(3)",
    );
    assert_string("\"ab\".replaceAll(\"\", (m, p) => p)", "0a1b2");
    assert_string("\"aaa\".replaceAll(\"aa\", \"b\")", "ba");
    assert_string("\"a1\".replace(1, \"one\")", "aone");
    assert_string("\"a\".replace({ toString() { return \"a\" } }, \"b\")", "b");
    assert_number("String.prototype.replace.length", 2.0);
    assert_number("String.prototype.replaceAll.length", 2.0);
    assert_string(
        &caught("\"a\".replace(\"a\", function () { throw new RangeError(\"r\") })"),
        "RangeError|r",
    );
    assert_string("\"\u{e9}a\".replace(\"a\", (m, p) => p)", "\u{e9}1");
    assert_string(
        "\"\u{e9}a\u{e8}a\".replaceAll(\"a\", (m, p, s) => \"(\" + p + s + \")\")",
        "\u{e9}(1\u{e9}a\u{e8}a)\u{e8}(3\u{e9}a\u{e8}a)",
    );
    assert_string("\"\u{1F600}\".replaceAll(\"\", (m) => m)", "\u{1F600}");
    assert_string("\"\u{e9}a\".replace(\"a\", \"b\")", "\u{e9}b");
}

#[test]
fn split_coerces_limit_before_separator() {
    assert_string(
        "let log = \"\"; \"a,b\".split({ toString() { log += \"S\"; return \",\" } }, { valueOf() { log += \"L\"; return 1 } }); log",
        "LS",
    );
    assert_string("JSON.stringify(\"a,b\".split(\",\", 0))", "[]");
    assert_string("JSON.stringify(\"a,b\".split(undefined, 0))", "[]");
}

#[test]
fn repeat_and_pad_audit() {
    assert_string(
        &caught("\"a\".repeat(-1)"),
        "RangeError|Invalid count value: -1",
    );
    assert_string(
        &caught("\"a\".repeat(Infinity)"),
        "RangeError|Invalid count value: Infinity",
    );
    assert_string("\"ab\".repeat(\"2\")", "abab");
    assert_string("\"\".repeat(5)", "");
    assert_string("\"a\".padStart(4, 12)", "121a");
    assert_string("\"a\".padEnd(3, { toString() { return \"z\" } })", "azz");
    assert_string("\"a\".padStart(3, \"\")", "a");
}

/// `padStart`/`padEnd` follow `StringPad` order: `fillString` is not coerced
/// when `maxLength` does not exceed the length, and coerced once otherwise.
#[test]
fn pad_fill_string_coercion_order() {
    for name in ["padStart", "padEnd"] {
        assert_number(
            &format!("let n = 0; \"abc\".{name}(2, {{ toString() {{ n++; return \"x\"; }} }}); n"),
            0.0,
        );
    }
    assert_string(
        "let n = 0; const r = \"abc\".padStart(5, { toString() { n++; return \"x\"; } }); r + n",
        "xxabc1",
    );
    assert_string(
        "let n = 0; const r = \"abc\".padEnd(5, { toString() { n++; return \"x\"; } }); r + n",
        "abcxx1",
    );
}

#[test]
fn unicode_case_and_trim() {
    assert_string("\"\u{e9}t\u{e9}\".toUpperCase()", "\u{c9}T\u{c9}");
    assert_string("\"\u{c9}T\u{c9}\".toLowerCase()", "\u{e9}t\u{e9}");
    assert_string("\"stra\u{df}e\".toUpperCase()", "STRASSE");
    assert_string("\"\u{a0}\u{feff}a\u{2028}\u{3000}\".trim()", "a");
    assert_string("\"\u{a0}a\u{a0}\".trimStart()", "a\u{a0}");
    assert_string("\"\u{a0}a\u{a0}\".trimEnd()", "\u{a0}a");
}

#[test]
fn code_unit_positions_over_non_ascii() {
    assert_string("\"\u{e9}a\".slice(1)", "a");
    assert_string("\"\u{e9}a\".substring(1)", "a");
    assert_string("\"\u{e9}a\".charAt(1)", "a");
    assert_string("\"\u{e9}a\".padStart(5)", "   \u{e9}a");
    assert_string("\"a\".padEnd(5, \"\u{e9}\")", "a\u{e9}\u{e9}\u{e9}\u{e9}");
    assert_string("\"\u{e9}a\".split(\"\").join(\"|\")", "\u{e9}|a");
    // Search positions are code-unit offsets.
    assert_number("\"\u{e9}a\".indexOf(\"a\")", 1.0);
    assert_number("\"\u{e9}a\u{e9}a\".indexOf(\"a\", 2)", 3.0);
    assert_number("\"\u{e9}a\u{e9}a\".lastIndexOf(\"a\", 2)", 1.0);
    assert_number("\"\u{1F600}a\".indexOf(\"a\", 1)", 2.0);
    assert_number("\"\u{1F600}a\".lastIndexOf(\"\u{1F600}\", 1)", 0.0);
    assert_number("\"\u{e9}a\".search(\"a\")", 1.0);
    assert_bool("\"\u{e9}a\".includes(\"a\", 1)", true);
    assert_bool("\"\u{e9}a\".startsWith(\"a\", 1)", true);
    assert_bool("\"\u{e9}a\".endsWith(\"\u{e9}\", 1)", true);
    assert_bool("\"\u{1F600}a\".startsWith(\"a\", 1)", false);
    assert_number("\"\u{ab}0\u{bb}\".match(\"0\").index", 1.0);
    // Positions over an ASCII prefix stay exact.
    assert_number("\"a\u{ab}b\u{bb}\".indexOf(\"a\")", 0.0);
    assert_number("\"\u{ab}x\".indexOf(\"z\")", -1.0);
    assert_number("\"ab\u{ab}\".lastIndexOf(\"b\", 1)", 1.0);
    assert_string("\"ab\u{ab}\".slice(0, 2)", "ab");
    assert_string("\"ab\u{ab}\".substring(2, 0)", "ab");
    assert_string("\"x\u{ab}\".charAt(0) + \"x\u{ab}\".at(0)", "xx");
    assert_number("\"x\u{ab}\".charCodeAt(0)", 120.0);
    assert_bool("\"ab\u{ab}\".startsWith(\"b\", 1)", true);
    assert_eval("\"x\u{ab}\".at(5)", "undefined");
    assert_string("\"ab\u{ab}\".at(-1)", "\u{ab}");
    assert_string("\"ab\u{ab}\".slice(-1)", "\u{ab}");
    // Position-free results stay correct.
    assert_bool("\"\u{e9}a\".includes(\"a\")", true);
    assert_bool("\"\u{e9}a\".startsWith(\"\u{e9}\")", true);
    assert_string("\"\u{e9},a\".split(\",\").join(\"|\")", "\u{e9}|a");
    assert_string("\"\u{e9}\".concat(\"\u{e8}\")", "\u{e9}\u{e8}");
    assert_string("\"\u{e9}\".repeat(2)", "\u{e9}\u{e9}");
}

#[test]
fn generic_receivers() {
    for name in [
        "at",
        "charCodeAt",
        "codePointAt",
        "concat",
        "lastIndexOf",
        "localeCompare",
        "normalize",
        "search",
        "match",
        "matchAll",
        "replace",
        "replaceAll",
        "isWellFormed",
        "toWellFormed",
    ] {
        assert_string(
            &caught(&format!("String.prototype.{name}.call(undefined)")),
            &format!("TypeError|String.prototype.{name} called on null or undefined"),
        );
        assert_string(
            &caught(&format!("String.prototype.{name}.call(null)")),
            &format!("TypeError|String.prototype.{name} called on null or undefined"),
        );
        assert_string(&format!("String.prototype.{name}.name"), name);
        assert_bool(&format!("\"a\".{name} === String.prototype.{name}"), true);
    }
    assert_string("String.prototype.at.call(123, -1)", "3");
    assert_number("String.prototype.charCodeAt.call(true, 0)", 116.0);
    assert_string(
        "String.prototype.concat.call({ toString() { return \"o\" } }, \"k\")",
        "ok",
    );
    assert_number("String.prototype.lastIndexOf.call(1001, 1)", 3.0);
    assert_string("String.prototype.replace.call(1234, 2, \"x\")", "1x34");
    assert_string(
        "class S extends String {} new S(\"abc\").at(-1) + new S(\"ab\").concat(\"c\")",
        "cabc",
    );
}

/// Every former non-ASCII / lone-surrogate refusal returns the spec result:
/// the methods work on code units at each string's width.
#[test]
fn former_refusals_return_spec_results() {
    assert_string("\"a\u{e9}\".charAt(1)", "\u{e9}");
    assert_bool("\"\u{1F600}\".at(-1) === \"\\uDE00\"", true);
    assert_bool(
        "\"a\u{e9}\u{1F600}\".slice(1, 3) === \"\u{e9}\\uD83D\"",
        true,
    );
    assert_string("\"\u{e9}\".padStart(3, \"\u{e9}\")", "\u{e9}\u{e9}\u{e9}");
    assert_string(
        "JSON.stringify(\"a\u{e9}\".split(\"\"))",
        "[\"a\",\"\u{e9}\"]",
    );
    assert_string(
        "let p = \"\u{1F600}\".split(\"\"); p.length + \",\" + (p[0] === \"\\uD83D\") + \",\" + (p[1] === \"\\uDE00\")",
        "2,true,true",
    );
    assert_string("\"\u{1F600}\".replace(\"\", \"-\")", "-\u{1F600}");
    assert_number("String.fromCharCode(0xD800).length", 1.0);
    assert_bool("String.fromCodePoint(0xDC00) === \"\\uDC00\"", true);
    assert_bool("\"\\uD800\".isWellFormed()", false);
    assert_bool("\"a\u{1F600}\".isWellFormed()", true);
    assert_string("\"a\\uD800\".toWellFormed()", "a\u{FFFD}");
    assert_string("\"\u{3000}a\u{3000}\".trim()", "a");
    assert_string("\"\u{feff}a\".trimStart()", "a");
    assert_number("\"\u{1F600}\".codePointAt(0)", 128_512.0);
    assert_number("\"\u{1F600}\".codePointAt(1)", 56_832.0);
    assert_number("\"\u{e9}\u{1F600}\u{e9}\".lastIndexOf(\"\u{e9}\")", 3.0);
    assert_number("\"a\u{e9}\".indexOf(\"\u{e9}\")", 1.0);
    assert_number("\"x\u{e9}\".search(\"\u{e9}\")", 1.0);
    assert_number(
        "\"\u{e9}\u{e9}\".replaceAll(\"\u{e9}\", \"$&$&\").length",
        4.0,
    );
    assert_number("\"\u{e9}\".repeat(3).length", 3.0);
    assert_bool("\"a\u{e9}\".startsWith(\"\u{e9}\", 1)", true);
    assert_bool("\"\u{e9}\u{1F600}\".endsWith(\"\u{1F600}\")", true);
    // A `RegExp`-syntax string pattern still throws (correct-or-throw).
    assert_type_error_with("\"\u{e9}.\".search(\".\")", "RegExp");
    assert_type_error_with("\"\u{e9}+\".match(\"\u{e9}+\")", "RegExp");
}

/// `isWellFormed` / `toWellFormed` over pairs and lone surrogates; results
/// narrow to one-byte when every unit fits.
#[test]
fn well_formed_and_narrowing() {
    assert_number("String.prototype.isWellFormed.length", 0.0);
    assert_number("String.prototype.toWellFormed.length", 0.0);
    assert_bool("\"\\uDC00\\uD800\".isWellFormed()", false);
    assert_bool("\"\".isWellFormed() && \"abc\".isWellFormed()", true);
    assert_bool(
        "\"\\uDC00a\\uD800\".toWellFormed() === \"\\uFFFDa\\uFFFD\"",
        true,
    );
    assert_string("\"a\u{1F600}\".toWellFormed()", "a\u{1F600}");
    assert_string("\"\u{e9}\".toWellFormed()", "\u{e9}");
    // A slice of two-byte text that fits Latin-1 equals the one-byte literal.
    assert_bool("\"\u{e9}\u{1F600}\".slice(0, 1) === \"\u{e9}\"", true);
    assert_bool("\"a\u{3000}\".trim() === \"a\"", true);
    assert_bool("\"\u{1F600}ab\".split(\"\u{1F600}\")[1] === \"ab\"", true);
    assert_bool("String.fromCharCode(0xE9) === \"\u{e9}\"", true);
    assert_bool("\"a\".padEnd(3, \"b\u{100}\") === \"ab\u{100}\"", true);
    assert_bool("\"a\".padEnd(2, \"b\u{100}\") === \"ab\"", true);
    assert_string(
        "\"\u{e9}a\u{e9}\".replaceAll(\"a\", \"$\\u0060|$'\")",
        "\u{e9}\u{e9}|\u{e9}\u{e9}",
    );
}
