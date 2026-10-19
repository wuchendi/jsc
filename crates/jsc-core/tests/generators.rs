//! Generator integration tests (state-machine transform).
//!
//! Covers the user-approved v1 cut: `function*` declarations with `yield` in
//! statement / `x = yield` / `return yield` positions, `.next(v)` returning
//! `{ value, done }`, a persistent frame that survives across suspends, and the
//! diagnostics for the out-of-scope forms (CUT A: yield in try/catch/finally;
//! CUT B: yield inside control flow; nested-expression yield; `yield*`; a
//! non-generator `.next()` receiver).

mod common;

use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_rejected, assert_uncaught,
};

#[test]
fn yields_a_sequence_then_done() {
    assert_output(
        "function* g(){ yield 1; yield 2; }
         let it = g();
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(); console.log(b.value, b.done);
         let c = it.next(); console.log(c.value, c.done);",
        "1 false\n2 false\nundefined true\n",
    );
}

#[test]
fn calling_a_generator_does_not_run_its_body() {
    // The body's `console.log` must not fire until `.next()` is called.
    assert_output(
        "function* g(){ console.log(\"ran\"); yield 1; }
         let it = g();
         console.log(\"made\");
         it.next();",
        "made\nran\n",
    );
}

#[test]
fn a_return_value_completes_the_generator() {
    assert_output(
        "function* g(){ yield 1; return 9; }
         let it = g();
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(); console.log(b.value, b.done);
         let c = it.next(); console.log(c.value, c.done);",
        "1 false\n9 true\nundefined true\n",
    );
}

#[test]
fn an_empty_generator_is_immediately_done() {
    assert_output(
        "function* g(){}
         let r = g().next();
         console.log(r.value, r.done);",
        "undefined true\n",
    );
}

#[test]
fn next_stays_done_after_completion() {
    assert_output(
        "function* g(){ yield 7; }
         let it = g();
         it.next();
         console.log(it.next().done, it.next().done);",
        "true true\n",
    );
}

#[test]
fn a_sent_value_is_the_result_of_yield() {
    assert_output(
        "function* g(){ let s = yield 1; yield s + 100; }
         let it = g();
         console.log(it.next().value);
         console.log(it.next(5).value);",
        "1\n105\n",
    );
}

#[test]
fn assignment_of_a_sent_value_updates_a_local() {
    assert_output(
        "function* g(){ let x = 0; x = yield 1; yield x * 2; }
         let it = g();
         console.log(it.next().value);
         console.log(it.next(21).value);",
        "1\n42\n",
    );
}

#[test]
fn parameters_persist_across_a_yield() {
    assert_output(
        "function* g(a, b){ yield a; yield b; yield a + b; }
         let it = g(10, 20);
         console.log(it.next().value, it.next().value, it.next().value);",
        "10 20 30\n",
    );
}

#[test]
fn a_local_persists_across_a_yield() {
    assert_output(
        "function* g(x){ let y = x + 1; yield y; yield y * 2; }
         let it = g(10);
         console.log(it.next().value, it.next().value);",
        "11 22\n",
    );
}

#[test]
fn return_yield_returns_the_sent_value() {
    assert_output(
        "function* g(){ return yield 1; }
         let it = g();
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(99); console.log(b.value, b.done);",
        "1 false\n99 true\n",
    );
}

#[test]
fn instances_have_independent_state() {
    assert_output(
        "function* g(){ yield 1; yield 2; }
         let a = g();
         let b = g();
         console.log(a.next().value, b.next().value, a.next().value, b.next().value);",
        "1 1 2 2\n",
    );
}

#[test]
fn self_contained_control_flow_without_yield_is_allowed() {
    // An `if` that contains no `yield` stays inline within a segment.
    assert_output(
        "function* g(x){ let y = 0; if (x > 5) { y = 100; } else { y = 1; } yield y; }
         console.log(g(9).next().value, g(2).next().value);",
        "100 1\n",
    );
}

#[test]
fn yields_heap_strings() {
    assert_output(
        "function* g(){ yield \"a\"; yield \"b\"; }
         let it = g();
         console.log(it.next().value + it.next().value);",
        "ab\n",
    );
}

#[test]
fn a_generator_handle_renders_as_a_generator_object() {
    assert_eval(
        "function* g(){ yield 1; } let it = g(); it",
        "Object [Generator]",
    );
}

// --- control flow crossing a yield (CFG flattening) -------------------------

#[test]
fn yields_from_a_for_loop() {
    assert_output(
        "function* range(n){ for (let i = 0; i < n; i++) { yield i; } }
         let it = range(4);
         let out = \"\";
         let r = it.next();
         while (!r.done) { out = out + r.value; r = it.next(); }
         console.log(out);",
        "0123\n",
    );
}

#[test]
fn yields_from_a_while_loop_with_mutation() {
    assert_output(
        "function* g(){ let i = 0; while (i < 3) { yield i * 10; i++; } }
         let it = g();
         console.log(it.next().value, it.next().value, it.next().value, it.next().done);",
        "0 10 20 true\n",
    );
}

#[test]
fn yields_from_both_branches_of_an_if() {
    assert_output(
        "function* g(x){ if (x > 0) { yield 1; yield 2; } else { yield -1; } }
         let a = g(5);
         console.log(a.next().value, a.next().value, a.next().done);
         let b = g(-3);
         console.log(b.next().value, b.next().done);",
        "1 2 true\n-1 true\n",
    );
}

#[test]
fn a_return_inside_control_flow_completes_the_generator() {
    assert_output(
        "function* g(x){ if (x > 5) { return 100; } yield 1; yield 2; }
         let a = g(9).next(); console.log(a.value, a.done);
         let it = g(2); let b = it.next(); let c = it.next();
         console.log(b.value, b.done, c.value, c.done);",
        "100 true\n1 false 2 false\n",
    );
}

#[test]
fn break_exits_a_yielding_loop() {
    assert_output(
        "function* g(){ let i = 0; while (true) { if (i >= 3) { break; } yield i; i++; } yield 99; }
         let it = g();
         console.log(it.next().value, it.next().value, it.next().value, it.next().value, it.next().done);",
        "0 1 2 99 true\n",
    );
}

#[test]
fn continue_skips_an_iteration_of_a_yielding_loop() {
    assert_output(
        "function* g(){ for (let i = 0; i < 5; i++) { if (i === 2) { continue; } yield i; } }
         let it = g();
         let out = \"\";
         let r = it.next();
         while (!r.done) { out = out + r.value; r = it.next(); }
         console.log(out);",
        "0134\n",
    );
}

// --- consuming a generator with for-of --------------------------------------

#[test]
fn for_of_drives_a_generator() {
    assert_output(
        "function* g(){ yield 1; yield 2; yield 3; }
         let out = \"\";
         for (const x of g()) { out = out + x; }
         console.log(out);",
        "123\n",
    );
}

#[test]
fn for_of_over_a_generator_honors_break_and_continue() {
    assert_output(
        "function* g(){ yield 1; yield 2; yield 3; yield 4; }
         let out = \"\";
         for (const x of g()) { if (x === 2) { continue; } if (x === 4) { break; } out = out + x; }
         console.log(out);",
        "13\n",
    );
}

#[test]
fn for_of_over_a_looping_generator() {
    assert_number(
        "function* range(n){ let i = 0; while (i < n) { yield i; i++; } }
         let s = 0;
         for (const x of range(5)) { s = s + x; }
         s",
        10.0,
    );
}

#[test]
fn for_of_still_walks_arrays_and_strings() {
    // The generator branch must not disturb the ordinary index walk.
    assert_output(
        "let out = \"\";
         for (const x of [7, 8, 9]) { out = out + x; }
         for (const c of \"ab\") { out = out + c; }
         console.log(out);",
        "789ab\n",
    );
}

#[test]
fn spread_collects_a_generator_into_an_array() {
    assert_output(
        "function* g(){ yield 1; yield 2; yield 3; }
         let a = [...g()];
         console.log(a.length, a[0], a[1], a[2]);",
        "3 1 2 3\n",
    );
}

#[test]
fn spread_mixes_with_other_array_elements() {
    assert_output(
        "function* g(){ yield 2; yield 3; }
         console.log([1, ...g(), 4].join(\"\"));",
        "1234\n",
    );
}

#[test]
fn spread_forwards_a_generator_into_a_call() {
    assert_number(
        "function* g(){ yield 10; yield 20; }
         function add(a, b){ return a + b; }
         add(...g())",
        30.0,
    );
}

#[test]
fn spread_still_flattens_arrays() {
    // The generator drain must not disturb ordinary array spread.
    assert_output("console.log([...[5, 6], ...[7, 8]].join(\"\"));", "5678\n");
}

#[test]
fn a_for_of_without_a_yield_runs_inline() {
    // A `for-of` with no suspend in its body is emitted verbatim in the block.
    assert_output(
        "function* g(){ let a = [1, 2, 3]; let s = 0; for (const x of a) { s = s + x; } yield s; }
         console.log(g().next().value);",
        "6\n",
    );
}

#[test]
fn a_for_in_without_a_yield_runs_inline() {
    assert_output(
        "function* g(){ let o = { a: 1, b: 2 }; let out = \"\"; for (const k in o) { out = out + k; } yield out; }
         console.log(g().next().value);",
        "ab\n",
    );
}

// --- Diagnostics: the still-out-of-scope forms ------------------------------

#[test]
fn rejects_for_of_with_a_yield_body_inside_a_generator() {
    // The `for-of` hidden index/length temporaries do not persist across a suspend
    // (deferred); an indexed `for` over `.length` is the supported workaround.
    assert_rejected("function* g(){ let a = [1, 2]; for (const x of a) { yield x; } }");
    assert_rejected("function* g(){ let o = { a: 1 }; for (const k in o) { yield k; } }");
}

#[test]
fn cut_a_rejects_yield_inside_try() {
    // Exception-handler state across a suspend is out of scope.
    assert_rejected("function* g(){ try { yield 1; } catch (e) {} }");
    assert_rejected("function* g(){ try { doThing(); } finally { yield 1; } }");
}

#[test]
fn rejects_yield_inside_switch_or_in_a_condition() {
    assert_rejected("function* g(){ switch (1) { case 1: yield 1; } }");
    assert_rejected("function* g(){ let i = 0; while (yield i) { i++; } }");
}

#[test]
fn unsupported_yield_positions_name_the_construct() {
    let cases = [
        (
            "function* g(){ if (yield 1) {} }",
            "a `yield` in an `if` condition is not supported",
        ),
        (
            "function* g(){ let i = 0; while (yield i) { i++; } }",
            "a `yield` in a loop condition or update is not supported",
        ),
        (
            "function* g(){ switch (1) { case 1: yield 1; } }",
            "a `yield` inside a `switch` is not supported",
        ),
        (
            "function* g(){ yield (yield 1); }",
            "a `yield` operand may not itself contain a `yield`",
        ),
    ];
    for (src, expected) in cases {
        let msg = jsc_core::run::compile_and_run(src)
            .err()
            .map(|err| err.to_string())
            .unwrap_or_default();
        assert_eq!(msg, expected, "diagnostic for `{src}`");
    }
}

#[test]
fn rejects_a_yield_nested_in_a_larger_expression() {
    assert_rejected("function* g(){ let x = (yield 1) + 2; }");
    assert_rejected("function* g(){ foo(yield 1); }");
    assert_rejected("function* g(){ yield (yield 1); }");
}

#[test]
fn yield_delegate_forwards_an_inner_generator() {
    assert_output(
        "function* inner(){ yield 1; yield 2; }
         function* outer(){ yield 0; yield* inner(); yield 3; }
         let it = outer();
         let out = \"\";
         let r = it.next();
         while (!r.done) { out = out + r.value; r = it.next(); }
         console.log(out);",
        "0123\n",
    );
}

#[test]
fn yield_delegate_over_a_looping_generator() {
    assert_output(
        "function* g(n){ let i = 0; while (i < n) { yield i; i++; } }
         function* h(){ yield* g(3); yield* g(2); }
         let it = h();
         let out = \"\";
         let r = it.next();
         while (!r.done) { out = out + r.value; r = it.next(); }
         console.log(out);",
        "01201\n",
    );
}

#[test]
fn a_nested_arrow_in_a_generator_is_callable() {
    assert_output(
        "function* g(){ const f = () => 1; yield f() } console.log([...g()])",
        "[ 1 ]\n",
    );
}

#[test]
fn a_nested_function_declaration_in_a_generator_is_callable() {
    assert_output(
        "function* g(){ function h(){ return 2 } yield h(); } console.log([...g()])",
        "[ 2 ]\n",
    );
}

/// A closure and the generator body share the local's cell, so a write made by
/// the closure after one `yield` is observed by the body after the next.
#[test]
fn a_closure_mutating_a_generator_local_is_shared_across_yields() {
    assert_output(
        "function* g(){ let n = 1; const inc = () => { n = n + 10; }; yield n; inc(); yield n; inc(); yield n; }
         console.log([...g()])",
        "[ 1, 11, 21 ]\n",
    );
}

#[test]
fn a_closure_captures_a_generator_parameter() {
    assert_output(
        "function* g(p){ const get = () => p; yield get(); p = p * 2; yield get(); }
         console.log([...g(5)])",
        "[ 5, 10 ]\n",
    );
}

#[test]
fn array_map_with_a_closure_over_a_generator_local() {
    assert_output(
        "function* g(arr){ let k = 100; yield arr.map(x => x + k); k = 1; yield arr.map(x => x + k); }
         for (const v of g([1, 2])) console.log(v);",
        "[ 101, 102 ]\n[ 2, 3 ]\n",
    );
}

/// A closure yielded out of the generator keeps the frame's cell alive and
/// still observes it after the generator has finished.
#[test]
fn a_closure_escaping_a_generator_outlives_it() {
    assert_output(
        "function* g(){ let c = 0; const f = () => { c = c + 1; return c; }; yield f; c = 40; }
         const it = g();
         const f = it.next().value;
         console.log(it.next().done);
         console.log(f(), f());",
        "true\n41 42\n",
    );
}

#[test]
fn rejects_a_generator_expression() {
    // Only top-level `function*` DECLARATIONS are supported.
    assert_rejected("let g = function*(){ yield 1 }; g().next();");
}

#[test]
fn a_non_generator_next_throws_a_type_error() {
    // A `.next()` receiver that is not a generator throws a branded TypeError.
    assert_uncaught(
        "function* g(){ yield 1 } let o = {}; o.next();",
        "TypeError: called value is not a function",
    );
}

#[test]
fn a_non_generator_next_type_error_is_catchable() {
    assert_bool(
        "function* g(){ yield 1 }
         let caught = false;
         try { let n = 5; n.next(); } catch (e) { caught = e instanceof TypeError; }
         caught",
        true,
    );
}

/// A C-style `for (let ...)` loop with a `yield` in its body still produces the
/// right sequence (generator frames keep every binding in an env cell).
#[test]
fn for_let_loop_with_yield_in_generator() {
    assert_output(
        "function* g(){ for (let i=0;i<3;i++) { yield i; } }
         for (const v of g()) console.log(v);",
        "0\n1\n2\n",
    );
}
