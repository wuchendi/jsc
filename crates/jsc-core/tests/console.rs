//! Integration tests: `console.log` host-import output.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_eval, assert_output, eval_text};

// --- console.log ------------------------------------------------------------

/// `console.log(40 + 2)` prints the number `42` on its own line.
#[test]
fn console_log_number() {
    assert_output("console.log(40 + 2)", "42\n");
}

/// `console.log("hi")` prints the string UNQUOTED.
#[test]
fn console_log_string_unquoted() {
    assert_output("console.log(\"hi\")", "hi\n");
}

/// `console.log(1, 2, 3)` prints its arguments single-space separated with one
/// trailing newline.
#[test]
fn console_log_multi_arg() {
    assert_output("console.log(1, 2, 3)", "1 2 3\n");
}

// --- Formatting parity with `format_value` ------------------------------------

/// Arrays and objects print Node-console style, exactly like `format_value`.
#[test]
fn console_log_arrays_and_objects() {
    assert_output("console.log([1, 2, 3])", "[ 1, 2, 3 ]\n");
    assert_output("console.log([])", "[]\n");
    assert_output("let o = {a: 1, b: 2}; console.log(o)", "{ a: 1, b: 2 }\n");
    // A nested array/object argument still formats recursively.
    assert_output("console.log([1, [2, 3]])", "[ 1, [ 2, 3 ] ]\n");
}

/// Booleans, `null`, and `undefined` print as their bare words.
#[test]
fn console_log_booleans_null_undefined() {
    assert_output("console.log(true)", "true\n");
    assert_output("console.log(false)", "false\n");
    assert_output("console.log(null)", "null\n");
    assert_output("console.log(undefined)", "undefined\n");
}

/// A function value prints Node-console style with its name.
#[test]
fn console_log_function_value() {
    assert_output(
        "function id(x){ return x } let g = id; console.log(g)",
        "[Function: id]\n",
    );
    assert_output("console.log([x => x])", "[ [Function (anonymous)] ]\n");
}

/// A zero-argument `console.log()` prints just a newline.
#[test]
fn console_log_zero_args_prints_newline() {
    assert_output("console.log()", "\n");
}

/// Mixed-type arguments each format independently, still space-separated.
#[test]
fn console_log_mixed_arguments() {
    assert_output(
        "console.log(1, \"two\", true, null, [3])",
        "1 two true null [ 3 ]\n",
    );
}

// --- Interleaving / semantics -------------------------------------------------

/// Multiple `console.log` statements print in source order, one line each, and
/// the program's completion value is independent of what was printed.
#[test]
fn console_log_prints_in_order_independent_of_final_value() {
    assert_output("console.log(1); console.log(2); 99", "1\n2\n");
    // The final expression `99` is the completion value, unaffected by the prints.
    assert_eval("console.log(1); console.log(2); 99", "99");
}

/// `console.log(..)` evaluates to `undefined` (its expression value), so it can
/// appear as the final expression.
#[test]
fn console_log_expression_value_is_undefined() {
    assert_output("console.log(7)", "7\n");
    assert_eval("console.log(7)", "undefined");
}

/// `console.log` inside a loop prints once per iteration, computed values and
/// all.
#[test]
fn console_log_inside_loop() {
    assert_output(
        "for (let i = 0; i < 3; i = i + 1) { console.log(i * 10) }",
        "0\n10\n20\n",
    );
}

// --- Regression: the import offset must not break existing programs -----------

/// The host import shifts every defined-function index by one; recursion,
/// closures, and indirect calls must still resolve.
#[test]
fn import_offset_regression_spot_check() {
    // Direct arithmetic and a direct recursive call.
    assert_eq!(eval_text("40 + 2"), "42");
    assert_eq!(
        eval_text("function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"),
        "55"
    );
    // Closures / indirect calls through the funcref table.
    assert_eq!(
        eval_text(
            "function counter(){ let c=0; return function(){ c=c+1; return c } } \
             let n=counter(); n(); n(); n()"
        ),
        "3"
    );
    assert_eq!(
        eval_text("function adder(x){ return y => x + y } let a5 = adder(5); a5(37)"),
        "42"
    );
}

/// A program that prints AND then uses a recursive call as its final value:
/// prints stream first, then the computed completion value is returned.
#[test]
fn console_log_alongside_recursion() {
    let src = "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } \
               console.log(\"start\"); fib(10)";
    assert_output(src, "start\n");
    assert_eval(src, "55");
}
