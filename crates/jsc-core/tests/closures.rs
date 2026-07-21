//! Integration tests: closure capture semantics (boxed cells, block scope, loops).

mod common;
use common::assert_eval;

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
