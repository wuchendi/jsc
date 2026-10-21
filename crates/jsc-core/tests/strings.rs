//! Integration tests: string methods and receiver-type method dispatch.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string};

/// String `toUpperCase`/`toLowerCase` are ASCII case folds producing NEW strings.
#[test]
fn string_case_fold() {
    assert_string("\"abc\".toUpperCase()", "ABC");
    assert_string("\"ABC\".toLowerCase()", "abc");
    assert_string("\"Hi There\".toUpperCase()", "HI THERE");
    assert_string("\"Hi There\".toLowerCase()", "hi there");
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

/// String `padStart`/`padEnd` pad to a BYTE length with a repeated pad string
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
