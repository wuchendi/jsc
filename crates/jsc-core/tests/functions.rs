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
