//! Integration tests: `console.log` host-import output.
#![allow(clippy::expect_used)]

use jsc_core::run::{Value, compile_and_run, compile_and_run_capturing, format_value};
mod common;
use common::assert_output;

// --- S14.1 milestones ---------------------------------------------------------

/// `console.log(40 + 2)` prints the number `42` on its own line.
#[test]
fn console_log_number_milestone() {
    assert_output("console.log(40 + 2)", "42\n");
}

/// `console.log("hi")` prints the string UNQUOTED.
#[test]
fn console_log_string_unquoted_milestone() {
    assert_output("console.log(\"hi\")", "hi\n");
}

/// `console.log(1, 2, 3)` prints its arguments single-space separated with one
/// trailing newline.
#[test]
fn console_log_multi_arg_milestone() {
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

/// A function value prints Node-console style `[Function (anonymous)]`.
#[test]
fn console_log_function_value() {
    assert_output(
        "function id(x){ return x } let g = id; console.log(g)",
        "[Function (anonymous)]\n",
    );
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
    let (value, output) = compile_and_run_capturing("console.log(1); console.log(2); 99")
        .expect("pipeline should succeed");
    assert_eq!(output, "1\n2\n");
    // The final expression `99` is the completion value, unaffected by the prints.
    assert_eq!(value, Value::Number(99.0));
    assert_eq!(format_value(&value), "99");
}

/// `console.log(..)` evaluates to `undefined` (its expression value), so it can
/// appear as the final expression.
#[test]
fn console_log_expression_value_is_undefined() {
    let (value, output) =
        compile_and_run_capturing("console.log(7)").expect("pipeline should succeed");
    assert_eq!(output, "7\n");
    assert_eq!(value, Value::Undefined);
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
/// closures, and indirect calls must still resolve. `compile_and_run` (no
/// capture) drives the same pipeline the CLI uses.
#[test]
fn import_offset_regression_spot_check() {
    // Direct arithmetic and a direct recursive call.
    assert_eq!(
        compile_and_run("40 + 2").expect("pipeline should succeed"),
        Value::Number(42.0)
    );
    assert_eq!(
        compile_and_run("function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)")
            .expect("pipeline should succeed"),
        Value::Number(55.0)
    );
    // Closures / indirect calls through the funcref table.
    assert_eq!(
        compile_and_run(
            "function counter(){ let c=0; return function(){ c=c+1; return c } } \
             let n=counter(); n(); n(); n()"
        )
        .expect("pipeline should succeed"),
        Value::Number(3.0)
    );
    assert_eq!(
        compile_and_run("function adder(x){ return y => x + y } let a5 = adder(5); a5(37)")
            .expect("pipeline should succeed"),
        Value::Number(42.0)
    );
}

/// A program that prints AND then uses a recursive call as its final value:
/// prints stream first, then the computed completion value is returned.
#[test]
fn console_log_alongside_recursion() {
    let (value, output) = compile_and_run_capturing(
        "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } \
         console.log(\"start\"); fib(10)",
    )
    .expect("pipeline should succeed");
    assert_eq!(output, "start\n");
    assert_eq!(value, Value::Number(55.0));
}
