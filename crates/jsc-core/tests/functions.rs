//! Integration tests: first-class function values, higher-order functions, and arrows.

mod common;
use common::assert_eval;

// ---------------------------------------------------------------------------
// Combined higher-order / closure scenarios (>= 6 mixed features).
// ---------------------------------------------------------------------------

/// 1. A higher-order function that **accepts** a closure argument and applies
///    it — the closure captures an enclosing binding by reference.
#[test]
fn higher_order_accepts_capturing_closure() {
    assert_eval(
        "function applyTwice(f, x) { return f(f(x)) }
         let base = 10;
         let addBase = n => n + base;
         applyTwice(addBase, 22)",
        "42",
    );
}

/// 2. A higher-order function that **returns** a closure (returned-function
///    escapes its defining frame and still reads the captured value).
#[test]
fn higher_order_returns_closure() {
    assert_eval(
        "function adder(x) { return y => x + y }
         let add5 = adder(5);
         add5(37)",
        "42",
    );
}

/// 3. An **array of functions** dispatched through the funcref table
///    (`call_indirect`) by index.
#[test]
fn array_of_functions_called_by_index() {
    assert_eval(
        "function dbl(n) { return n * 2 }
         let ops = [dbl, n => n + 1, n => n * n];
         ops[0](3) + ops[1](10) + ops[2](5)",
        // 6 + 11 + 25
        "42",
    );
}

/// 7. Functions stored in **object properties** and called through the member
///    expression (`o.f(a, b)`) — a plain function value field, no `this`.
#[test]
fn function_value_in_object_property_called() {
    assert_eval(
        "function add(a, b) { return a + b }
         let o = { f: add, g: (a, b) => a * b };
         o.f(40, 2)",
        "42",
    );
    assert_eval(
        "let o = { g: (a, b) => a * b };
         o.g(6, 7)",
        "42",
    );
}

/// 9. `typeof` a function value is `"function"` and a function value is always
///    truthy (used to branch).
#[test]
fn function_value_typeof_and_truthiness() {
    assert_eval("typeof (x => x)", "function");
    assert_eval("function f() {} typeof f", "function");
    assert_eval("let f = () => 0; f ? 42 : 0", "42");
}

// ---------------------------------------------------------------------------
// Function, closure, and member-assignment acceptance list, end to end.
// ---------------------------------------------------------------------------

#[test]
fn functions_and_closures_end_to_end() {
    // Member-compound assignment.
    assert_eval("let a = [10, 20]; a[0] += 5; a[0]", "15");
    assert_eval("let o = { x: 1 }; o.x += 41; o.x", "42");
    // First-class function values.
    assert_eval(
        "function add(a, b) { return a + b } let f = add; f(40, 2)",
        "42",
    );
    assert_eval(
        "function apply(f, x) { return f(x) } function dbl(n) { return n * 2 } apply(dbl, 21)",
        "42",
    );
    // Arrow functions.
    assert_eval("let add = (a, b) => a + b; add(40, 2)", "42");
    assert_eval("let sq = x => x * x; sq(7)", "49");
    // Return-fn closure + mutable counter.
    assert_eval(
        "function adder(x) { return y => x + y } let a5 = adder(5); a5(37)",
        "42",
    );
    assert_eval(
        "function counter() { let c = 0; return function() { c = c + 1; return c } }
         let n = counter(); n(); n(); n()",
        "3",
    );
    assert_eval("typeof (x => x)", "function");
}

// ---------------------------------------------------------------------------
// Uniform-ABI `K` scan: every function kind and expression position widens `K`
// (each program keeps the function and its only caller out of reach of any
// other widening, so a missed position would drop the argument).
// ---------------------------------------------------------------------------

/// An object-literal setter is the program's only function: `o.x = 5` passes
/// one argument.
#[test]
fn arity_scan_object_literal_setter_only_function() {
    assert_eval("var o = { set x(v) { this.y = v } }; o.x = 5; o.y", "5");
    assert_eval(
        "var o = { set x(v) { this.y = v }, get x() { return this.y } }; o.x = 5; o.x",
        "5",
    );
}

/// A getter body is scanned: the arrow inside it and its call widen `K`.
#[test]
fn arity_scan_object_literal_getter_body() {
    assert_eval(
        "var o = { get x() { var f = (a) => a; return f(7) } }; o.x",
        "7",
    );
    assert_eval(
        "var o = { get x() { return { m() { var f = (a) => a; return f(7) } } } }; o.x.m()",
        "7",
    );
}

/// A class whose only function is an (instance or static) setter.
#[test]
fn arity_scan_class_setter_only_function() {
    assert_eval(
        "class C { set x(v) { this.y = v } }; var c = new C(); c.x = 5; c.y",
        "5",
    );
    assert_eval(
        "class C { static set x(v) { this.y = v } }; C.x = 5; C.y",
        "5",
    );
}

/// Class field initializers (instance and static) containing arrows.
#[test]
fn arity_scan_class_field_initializers() {
    assert_eval(
        "class C { static s = (() => { var f = (a) => a; return f(7) })() }; C.s",
        "7",
    );
    assert_eval(
        "class C { s = (() => { var f = (a) => a; return f(7) })() }; new C().s",
        "7",
    );
}

/// Setters nested in function, method, and getter bodies.
#[test]
fn arity_scan_nested_setters() {
    assert_eval(
        "function mk() { return { set x(v) { this.y = v } } }; var o = mk(); o.x = 5; o.y",
        "5",
    );
    assert_eval(
        "var o = { m() { return { set x(v) { this.y = v } } } }; var p = o.m(); p.x = 5; p.y",
        "5",
    );
}

/// Computed property names: object-literal keys and assignment targets.
#[test]
fn arity_scan_computed_keys() {
    assert_eval("var o = { [((a) => a)(\"k\")]: 1 }; o.k", "1");
    assert_eval("var o = {}; o[((a) => a)(\"k\")] = 1; o.k", "1");
}

/// Default parameter values of declarations, expressions, arrows, and
/// constructors.
#[test]
fn arity_scan_param_defaults() {
    assert_eval("function g(x = ((a, b) => b)(1, 2)) { return x }; g()", "2");
    assert_eval(
        "var g = function (x = ((a, b) => b)(1, 2)) { return x }; g()",
        "2",
    );
    assert_eval("var g = (x = ((a, b) => b)(1, 2)) => x; g()", "2");
    assert_eval(
        "class C { constructor(x = ((a, b) => b)(1, 2)) { this.x = x } }; new C().x",
        "2",
    );
}

/// Destructuring defaults in declarations and assignments.
#[test]
fn arity_scan_destructuring_defaults() {
    assert_eval("var { r = ((a) => a)(7) } = {}; r", "7");
    assert_eval("var [r = ((a) => a)(7)] = []; r", "7");
    assert_eval("var r; ({ r = ((a) => a)(7) } = {}); r", "7");
}

/// do-while, for-in, for-of (head and body), and labeled statements.
#[test]
fn arity_scan_loop_statements() {
    assert_eval(
        "var r; do { var f = (a) => a; r = f(7) } while (false); r",
        "7",
    );
    assert_eval(
        "var r; for (var k in {a:1}) { var f = (a) => a; r = f(7) }; r",
        "7",
    );
    assert_eval(
        "var r; for (var k of [1]) { var f = (a) => a; r = f(7) }; r",
        "7",
    );
    assert_eval("var r; for (var k of [((a) => a)(7)]) { r = k }; r", "7");
    assert_eval(
        "var r; l: while (true) { var f = (a) => a; r = f(7); break l }; r",
        "7",
    );
}

/// Template-literal substitutions.
#[test]
fn arity_scan_template_literal() {
    assert_eval("var r = +`${((a) => a)(7)}`; r", "7");
}

/// Optional calls pad to `K` like plain calls, and optional chains are scanned.
#[test]
fn arity_scan_optional_chain() {
    assert_eval("var f = (...r) => r.length; f?.(1, 2, 3)", "3");
    assert_eval("var r = [1].map(x => x)?.[((a) => a)(0)]; r", "1");
}

/// `yield` operands are scanned.
#[test]
fn arity_scan_yield_operand() {
    assert_eval(
        "function* g() { yield ((a) => a)(7) }; g().next().value",
        "7",
    );
}
