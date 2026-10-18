//! Integration tests: string methods and receiver-type method dispatch.

use jsc_core::run::compile_and_run;
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

/// Arity mismatches on the fixed-arity methods are compile errors.
#[test]
fn method_arity_errors() {
    assert!(
        compile_and_run("[1,2,3].indexOf()").is_err(),
        "indexOf needs one argument"
    );
    assert!(
        compile_and_run("\"abc\".charAt()").is_err(),
        "charAt needs one argument"
    );
    assert!(
        compile_and_run("\"abc\".toUpperCase(1)").is_err(),
        "toUpperCase takes no arguments"
    );
    assert!(
        compile_and_run("[1,2,3].slice(0,1,2)").is_err(),
        "slice takes at most two arguments"
    );
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

/// String `repeat(n)` concatenates `n` copies; `0` and a negative/NaN count both
/// yield `""` (JS throws a `RangeError` for negatives — a documented limitation).
#[test]
fn string_repeat() {
    assert_string("\"ab\".repeat(3)", "ababab");
    assert_string("\"x\".repeat(1)", "x");
    assert_string("\"ab\".repeat(0)", "");
    assert_string("\"ab\".repeat(-1)", "");
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

/// Arity mismatches on the string methods are compile errors.
#[test]
fn string_method_arity_errors() {
    assert!(
        compile_and_run("\"x\".trim(1)").is_err(),
        "trim takes no arguments"
    );
    assert!(
        compile_and_run("\"x\".repeat()").is_err(),
        "repeat needs one argument"
    );
    assert!(
        compile_and_run("\"x\".startsWith()").is_err(),
        "startsWith needs one argument"
    );
    assert!(
        compile_and_run("\"x\".padStart(1,\"_\",9)").is_err(),
        "padStart takes at most two arguments"
    );
    assert!(
        compile_and_run("\"x\".split(\",\",\"y\")").is_err(),
        "split takes at most one argument"
    );
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
