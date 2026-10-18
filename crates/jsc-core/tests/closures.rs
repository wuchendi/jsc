//! Integration tests: closure capture semantics (boxed cells, block scope, loops).

mod common;
use common::{assert_eval, assert_output, assert_uncaught};

/// 4. A closure that **captures a block-scoped** binding declared inside a
///    nested block, read after the block via the returned function.
#[test]
fn closure_captures_block_scoped_binding() {
    assert_eval(
        "function make() {
           let f = () => 0;
           { let secret = 42; f = () => secret; }
           return f;
         }
         make()()",
        "42",
    );
}

/// 5. An **arrow inside a function inside an arrow** — three nesting levels,
///    with the innermost capturing bindings from every enclosing scope.
#[test]
fn arrow_inside_function_inside_arrow() {
    assert_eval(
        "let outer = a => {
           function middle(b) {
             let inner = c => a + b + c;
             return inner;
           }
           return middle;
         };
         outer(30)(10)(2)",
        "42",
    );
}

/// 6. A **closure counter** whose captured cell is mutated across repeated
///    calls driven by a `while` loop, proving shared mutable state persists.
#[test]
fn closure_counter_across_loop() {
    assert_eval(
        "function counter() { let c = 0; return function() { c = c + 1; return c } }
         let n = counter();
         let last = 0;
         let i = 0;
         while (i < 42) { last = n(); i = i + 1 }
         last",
        "42",
    );
}

/// 8. Two closures over the **same** cell (accumulator pattern) — mutation
///    through one call is visible to the next; a fresh accumulator is
///    independent.
#[test]
fn accumulator_shares_and_isolates_cells() {
    assert_eval(
        "function acc() { let s = 0; return x => { s += x; return s } }
         let a = acc();
         a(10);
         a(32)",
        "42",
    );
    // A second accumulator starts from its own independent cell.
    assert_eval(
        "function acc() { let s = 0; return x => { s += x; return s } }
         let a = acc();
         let b = acc();
         a(100);
         b(42)",
        "42",
    );
}

/// 9. Top-level bindings referenced by top-level functions live in module cells:
///    the functions stay hoisted, `let`/`const`/`class` keep their TDZ (an access
///    before the declaration throws a `ReferenceError`), a `var` reads
///    `undefined` before its declaration, and a declaration never becomes the
///    program's completion value.
#[test]
fn module_bindings_keep_tdz_and_hoisting() {
    assert_uncaught(
        "f(); let n = 41; function f() { return n }",
        "ReferenceError: Cannot access 'n' before initialization",
    );
    assert_uncaught(
        "f(); let n = 41; function f() { n = 1 }",
        "ReferenceError: Cannot access 'n' before initialization",
    );
    assert_eval(
        "function f() { n = 3 } \
         let r = 0; try { f() } catch (e) { r = e instanceof Error } \
         let n = 1; r",
        "true",
    );
    assert_eval("let n = 41; function f() { return n } f()", "41");
    assert_eval("f(); var n = 41; function f() { return n }", "undefined");
    assert_eval("let k = 5; function a() { return k }", "undefined");
    assert_eval("class Q {} function a() { return Q }", "undefined");
}

/// 10. Module bindings are reached from every function shape nested in a
///     top-level function: generators, async bodies, nested arrows, class
///     methods/static fields, and expression forms like template literals and
///     optional chains.
#[test]
fn module_bindings_reachable_from_nested_shapes() {
    assert_eval(
        "function* g() { yield k; yield k + 1 } let k = 10; \
         let it = g(); it.next().value + it.next().value",
        "21",
    );
    assert_output(
        "let k = 7; async function f() { await null; return k } \
         f().then(v => console.log(v))",
        "7\n",
    );
    assert_eval("let k = 5; function f() { return () => k } f()()", "5");
    assert_eval(
        "let k = 5; class A { m() { return k } } function f() { return new A().m() } f()",
        "5",
    );
    assert_eval(
        "let k = 5; function f() { class B { static s = k } return B.s } f()",
        "5",
    );
    assert_eval("let k = 1; function f() { return `v${k}` } f()", "v1");
    assert_eval(
        "let o = {a: {b: 2}}; function f() { return o?.a?.b } f()",
        "2",
    );
    assert_eval(
        "let i = 0; function f() { do { i++ } while (i < 3); return i } f()",
        "3",
    );
}
