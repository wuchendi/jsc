//! Integration tests: `console.log` host-import output.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_eval, assert_output, assert_same_under_gc_stress, eval_text, exec};

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

// --- Argument evaluation before printing ------------------------------------

/// A later argument that throws leaves no partial output: every argument is
/// evaluated before anything is printed.
#[test]
fn console_log_throwing_later_argument_prints_nothing() {
    assert_output(
        "try { console.log('a', (() => { throw 1 })()) } catch (e) { console.log('c') }",
        "c\n",
    );
    assert_output(
        "try { console.log('a', 'b', (() => { throw 1 })(), 'd') } catch (e) { console.log('c') }",
        "c\n",
    );
}

/// An uncaught throw from a later argument prints nothing on stdout.
#[test]
fn console_log_uncaught_later_argument_prints_nothing() {
    let done = exec("console.log('a', (() => { throw 1 })())", false)
        .expect("program should compile and not trap");
    assert_eq!(
        (done.code, done.stdout.as_str(), done.stderr.as_str()),
        (1, "", "Uncaught 1\n")
    );
}

/// An object printed by an earlier argument renders in its state after a
/// later argument mutated it (the earlier argument is the same reference).
#[test]
fn console_log_renders_after_later_argument_side_effects() {
    assert_output(
        "const o = { a: 1 }; console.log(o, (o.a = 2, o.b = 3, 'x'))",
        "{ a: 2, b: 3 } x\n",
    );
    assert_output(
        "const xs = [1]; console.log(xs, xs.push(2))",
        "[ 1, 2 ] 2\n",
    );
}

/// A `console.log` nested in a later argument prints its own line first.
#[test]
fn console_log_nested_in_argument_prints_first() {
    assert_output(
        "function f() { console.log('inner'); return 2 } console.log('outer', 1, f())",
        "inner\nouter 1 2\n",
    );
}

/// Ordinary multi-argument calls print exactly as before.
#[test]
fn console_log_multi_arg_output_unchanged() {
    assert_output(
        "let i = 3; console.log('line', i, i * 2, i % 2 === 0)",
        "line 3 6 false\n",
    );
    assert_output(
        "console.log('done', null, undefined, [1, 2], { a: 1 })",
        "done null undefined [ 1, 2 ] { a: 1 }\n",
    );
    assert_output(
        "let s = 'v'; console.log(s, 1, true, null)",
        "v 1 true null\n",
    );
}

/// Earlier argument objects stay alive and render correctly while later
/// arguments allocate, under a collection at every allocation.
#[test]
fn console_log_allocating_later_arguments_gc_stress() {
    assert_same_under_gc_stress(
        "function mk(n) { const out = []; for (let i = 0; i < n; i++) out.push({ i, s: 'k' + i }); return out.length } \
         console.log({ a: [1, 2], b: 'x' + 1 }, mk(50), [{ c: 3 }], mk(50), 'end' + mk(5))",
        "{ a: [ 1, 2 ], b: x1 } 50 [ { c: 3 } ] 50 end5\n",
    );
}

/// A spread whose iterator throws midway prints nothing.
#[test]
fn console_log_spread_throwing_iterator_prints_nothing() {
    assert_output(
        "function* g() { yield 1; yield 2; throw 3 } \
         try { console.log('a', ...g()) } catch (e) { console.log('c', e) }",
        "c 3\n",
    );
}

/// The spread form under `gc_stress`: elements allocated mid-spread survive.
#[test]
fn console_log_spread_allocating_gc_stress() {
    assert_same_under_gc_stress(
        "function* g() { for (let i = 0; i < 3; i++) yield { i, s: [i, 'k' + i] } } \
         console.log({ z: 0 }, ...g(), [9])",
        "{ z: 0 } { i: 0, s: [ 0, k0 ] } { i: 1, s: [ 1, k1 ] } { i: 2, s: [ 2, k2 ] } [ 9 ]\n",
    );
}
