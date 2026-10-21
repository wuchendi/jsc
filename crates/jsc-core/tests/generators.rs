//! Generator integration tests (state-machine transform).
//!
//! Covers the user-approved v1 cut: `function*` declarations with `yield` in
//! statement / `x = yield` / `return yield` positions, `.next(v)` returning
//! `{ value, done }`, a persistent frame that survives across suspends, and the
//! diagnostics for the out-of-scope forms (a non-generator `.next()` receiver).
//! A `yield` may sit in a `try`, `catch` or `finally` clause. A `yield` nested in an expression is
//! linearized with JS evaluation order; a `yield` inside `for-of`/`for-in`/
//! `switch` crosses the loop/switch temporaries through frame cells.

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

// --- A `yield` inside for-of / for-in / switch -----------------------------

#[test]
fn yield_in_a_for_of_body_over_an_array() {
    assert_output(
        "function* g(xs){ for (const x of xs) { yield x * 2 } }
         console.log([...g([1, 2, 3])].join(\",\"));",
        "2,4,6\n",
    );
}

#[test]
fn yield_in_a_for_of_body_over_a_string() {
    assert_output(
        "function* g(s){ for (const c of s) { yield c + c } }
         console.log([...g(\"abc\")].join(\"\"));",
        "aabbcc\n",
    );
}

#[test]
fn yield_in_a_for_of_body_over_another_generator() {
    assert_output(
        "function* inner(){ yield 1; yield 2; yield 3; }
         function* outer(){ for (const x of inner()) { yield x * 10; } yield 0; }
         console.log([...outer()].join(\",\"));",
        "10,20,30,0\n",
    );
}

#[test]
fn yield_star_in_a_for_of_body() {
    assert_output(
        "function* inner(n){ for (let i = 0; i < n; i++) yield n; }
         function* g(xs){ for (const x of xs) { yield* inner(x); yield \"|\"; } }
         console.log([...g([1, 2, 3])].join(\"\"));",
        "1|22|333|\n",
    );
}

#[test]
fn yield_in_a_for_in_body() {
    assert_output(
        "function* keys(o){ for (const k in o) { yield k + \"=\" + o[k]; } }
         console.log([...keys({ a: 1, b: 2, c: 3 })].join(\" \"));",
        "a=1 b=2 c=3\n",
    );
}

#[test]
fn yield_in_a_for_of_body_with_break_continue_and_return() {
    assert_output(
        "function* g(xs){ for (const x of xs) { if (x === 2) continue; if (x === 5) break; yield x; } yield 99; }
         function* h(xs){ for (const x of xs) { yield x; if (x === 2) return 7; } yield 99; }
         console.log([...g([1, 2, 3, 4, 5, 6])].join(\",\"));
         let it = h([1, 2, 3]);
         let a = it.next(); let b = it.next(); let c = it.next(); let d = it.next();
         console.log(a.value, b.value, c.value, c.done, d.done);",
        "1,3,4,99\n1 2 7 true true\n",
    );
}

#[test]
fn yield_in_nested_for_of_loops_with_labels() {
    assert_output(
        "function* g(){
           outer: for (const a of [1, 2, 3]) {
             for (const b of \"xyz\") {
               if (b === \"z\") continue outer;
               if (a === 3) break outer;
               yield a + b;
             }
           }
           yield \"end\";
         }
         console.log([...g()].join(\" \"));",
        "1x 1y 2x 2y end\n",
    );
}

#[test]
fn yield_in_a_for_of_iterable_expression() {
    assert_output(
        "function* g(){ let s = 0; for (const x of (yield \"need\")) { s = s + x; yield s; } return s; }
         let it = g();
         console.log(it.next().value);
         console.log(it.next([4, 5]).value, it.next().value, it.next().value);",
        "need\n4 9 9\n",
    );
}

#[test]
fn yield_in_a_for_of_body_receives_sent_values() {
    assert_output(
        "function* g(xs){ let out = \"\"; for (const x of xs) { out = out + x + (yield x); } return out; }
         let it = g([\"a\", \"b\"]);
         it.next(); it.next(1);
         console.log(it.next(2).value);",
        "a1b2\n",
    );
}

#[test]
fn yield_in_switch_cases_with_fallthrough_and_break() {
    assert_output(
        "function* g(x){
           switch (x) {
             case 1: yield \"one\";
             case 2: yield \"two\"; break;
             case 3: yield \"three\";
           }
           yield \"after\";
         }
         console.log([...g(1)].join(\",\"), [...g(2)].join(\",\"), [...g(3)].join(\",\"), [...g(9)].join(\",\"));",
        "one,two,after two,after three,after after\n",
    );
}

#[test]
fn yield_in_switch_with_default_in_the_middle() {
    assert_output(
        "function* g(x){
           switch (x) {
             case 1: yield 1; break;
             default: yield \"d\";
             case 2: yield 2; break;
             case 3: yield 3;
           }
         }
         console.log([...g(1)].join(\",\"), [...g(2)].join(\",\"), [...g(7)].join(\",\"), [...g(3)].join(\",\"));",
        "1 2 d,2 3\n",
    );
}

#[test]
fn yield_in_switch_discriminant_and_case_tests() {
    assert_output(
        "function* g(){
           switch (yield \"disc\") {
             case (yield \"t1\"): yield \"matched1\"; break;
             case (yield \"t2\"): yield \"matched2\"; break;
           }
           return \"done\";
         }
         let it = g();
         let out = [it.next().value, it.next(5).value, it.next(4).value, it.next(5).value, it.next().value];
         console.log(out.join(\",\"));",
        "disc,t1,t2,matched2,done\n",
    );
}

#[test]
fn switch_in_a_loop_continue_and_break_targets() {
    assert_output(
        "function* g(xs){
           for (const x of xs) {
             switch (x) {
               case \"skip\": yield \"s\"; continue;
               case \"stop\": yield \"t\"; break;
               default: yield x;
             }
             yield \"|\";
             if (x === \"stop\") break;
           }
           yield \"end\";
         }
         console.log([...g([\"a\", \"skip\", \"b\", \"stop\", \"c\"])].join(\"\"));",
        "a|sb|t|end\n",
    );
}

#[test]
fn a_switch_with_break_but_no_yield_inside_a_generator() {
    assert_output(
        "function* g(x){ let r = 0; switch (x) { case 1: r = 10; break; default: r = 20; } yield r; }
         console.log(g(1).next().value, g(2).next().value);",
        "10 20\n",
    );
}

// --- yield/await inside try/catch/finally ------------------------------------

#[test]
fn yield_in_try_completes_normally() {
    assert_output(
        "function* g(){ try { yield 1; yield 2; } catch (e) { yield 99; } yield 3; }
         console.log([...g()].join());",
        "1,2,3\n",
    );
}

#[test]
fn a_throw_after_resume_is_caught_by_the_catch() {
    assert_output(
        "function* g(){ try { yield 1; throw new Error(\"x\") } catch (e) { yield e.message } }
         console.log(JSON.stringify([...g()]));",
        "[1,\"x\"]\n",
    );
}

#[test]
fn yield_inside_catch() {
    assert_output(
        "function* g(){ try { throw 5; } catch (e) { yield e; yield e + 1; } yield 0; }
         console.log([...g()].join());",
        "5,6,0\n",
    );
}

#[test]
fn yield_inside_finally_on_normal_completion() {
    assert_output(
        "function* g(){ try { yield 1; } finally { yield 2; console.log(\"fin\"); } yield 3; }
         console.log([...g()].join());",
        "fin\n1,2,3\n",
    );
}

#[test]
fn finally_runs_on_throw_then_rethrows_to_an_outer_try() {
    assert_output(
        "function* g(){
           try { try { yield 1; throw new Error(\"boom\"); } finally { yield 2; } }
           catch (e) { yield e.message; }
         }
         console.log([...g()].join());",
        "1,2,boom\n",
    );
}

#[test]
fn finally_rethrow_escapes_the_generator_uncaught() {
    assert_uncaught(
        "function* g(){ try { yield 1; throw new Error(\"out\"); } finally { console.log(\"fin\"); } }
         let it = g(); it.next(); it.next();",
        "Error: out",
    );
}

#[test]
fn a_throw_out_of_the_generator_completes_it() {
    assert_output(
        "function* g(){ yield 1; throw new Error(\"x\"); yield 2; }
         let it = g(); it.next();
         try { it.next(); } catch (e) { console.log(e.message); }
         let r = it.next(); console.log(r.value, r.done);",
        "x\nundefined true\n",
    );
}

#[test]
fn return_inside_try_runs_finally_and_keeps_the_value() {
    assert_output(
        "function* g(){ try { yield 1; return 7; } finally { yield 2; console.log(\"fin\"); } }
         let it = g();
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(); console.log(b.value, b.done);
         let c = it.next(); console.log(c.value, c.done);
         let d = it.next(); console.log(d.value, d.done);",
        "1 false\n2 false\nfin\n7 true\nundefined true\n",
    );
}

#[test]
fn break_and_continue_out_of_a_try_in_a_loop_run_finally() {
    assert_output(
        "function* g(){
           for (let i = 0; i < 5; i++) {
             try {
               if (i === 1) continue;
               if (i === 3) break;
               yield i;
             } finally { yield \"f\" + i; }
           }
           yield \"end\";
         }
         console.log([...g()].join());",
        "0,f0,f1,2,f2,f3,end\n",
    );
}

#[test]
fn nested_try_finally_runs_inner_then_outer() {
    assert_output(
        "function* g(){
           try {
             try { yield 1; return 9; } finally { yield \"inner\"; }
           } finally { yield \"outer\"; }
         }
         let it = g(); let out = [];
         for (let r = it.next(); ; r = it.next()) { out.push(r.value); if (r.done) break; }
         console.log(out.join());",
        "1,inner,outer,9\n",
    );
}

#[test]
fn a_callee_exception_after_resume_lands_in_the_right_handler() {
    assert_output(
        "function boom(){ throw new Error(\"callee\"); }
         function* g(){
           try {
             try { yield 1; } catch (e) { yield \"wrong\"; }
             yield 2;
             boom();
           } catch (e) { yield e.message; }
         }
         console.log([...g()].join());",
        "1,2,callee\n",
    );
}

#[test]
fn an_exception_from_an_inner_generator_is_caught_across_resumes() {
    assert_output(
        "function* inner(){ yield 1; throw new Error(\"inner\"); }
         function* outer(){
           let it = inner();
           try { yield it.next().value; yield it.next().value; } catch (e) { yield e.message; }
         }
         console.log([...outer()].join());",
        "1,inner\n",
    );
}

#[test]
fn async_await_in_try_then_catch_resolves() {
    assert_output(
        "async function f(){ try { await 1; throw new Error(\"e\") } catch (x) { return x.message } }
         f().then(v => console.log(v));",
        "e\n",
    );
}

// --- Diagnostics: the still-out-of-scope forms ------------------------------

#[test]
fn a_nested_yield_preserves_evaluation_order() {
    // `t("a")` runs before the suspend, `t("c")` after it; the driver's `|` lands
    // in between.
    assert_output(
        "let log = \"\";
         function t(s, v){ log = log + s; return v; }
         function* g(){ let r = t(\"a\", 1) + (yield t(\"b\", 2)) + t(\"c\", 3); log = log + \"d\"; return r; }
         let it = g();
         let a = it.next(); log = log + \"|\";
         let b = it.next(10);
         console.log(a.value, b.value, b.done, log);",
        "2 14 true ab|cd\n",
    );
}

#[test]
fn a_nested_yield_call_evaluates_arguments_in_order() {
    assert_output(
        "let log = \"\";
         function t(s, v){ log = log + s; return v; }
         let o = {f: function(a, b, c){ return a * 100 + b * 10 + c; }};
         function* g(){ return o.f(t(\"a\", 1), yield 2, t(\"c\", 3)); }
         let it = g();
         let a = it.next(); log = log + \"|\";
         let b = it.next(5);
         console.log(a.value, b.value, log);",
        "2 153 a|c\n",
    );
}

#[test]
fn a_store_after_a_suspend_targets_the_receiver_captured_before_it() {
    // The receiver and the computed key are evaluated before the `yield`; the
    // driver rebinds both during the suspend, which must not redirect the store.
    assert_output(
        "let box = {o: {x: 0}};
         let first = box.o;
         let key = \"x\";
         function* g(){ box.o[key] = yield 1; }
         let it = g(); it.next();
         box.o = {x: 0}; key = \"y\";
         it.next(5);
         console.log(first.x, box.o.x);",
        "5 0\n",
    );
}

#[test]
fn a_compound_member_update_reads_the_old_value_before_the_suspend() {
    assert_output(
        "let a = [1];
         function* g(){ a[0] += yield 1; }
         let it = g(); it.next();
         a[0] = 100;
         it.next(5);
         console.log(a[0]);",
        "6\n",
    );
}

#[test]
fn a_short_circuited_yield_does_not_suspend() {
    assert_output(
        "function* g(){ let r = 1 || (yield 9); yield r; let s = null ?? (yield 8); yield s; }
         let it = g();
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(); console.log(b.value, b.done);
         let c = it.next(3); console.log(c.value, c.done);",
        "1 false\n8 false\n3 false\n",
    );
}

#[test]
fn a_conditional_yields_only_on_the_taken_branch() {
    assert_output(
        "function* g(c){ return c ? (yield 1) : (yield 2); }
         let it = g(false);
         let a = it.next(); console.log(a.value, a.done);
         let b = it.next(5); console.log(b.value, b.done);",
        "2 false\n5 true\n",
    );
}

#[test]
fn a_yield_of_a_yield_resumes_twice() {
    assert_output(
        "function* g(){ let r = yield (yield 1); return r; }
         let it = g();
         console.log(it.next().value, it.next(2).value, it.next(3).value);",
        "1 2 3\n",
    );
}

#[test]
fn a_yield_in_a_loop_or_if_condition() {
    assert_output(
        "function* g(){ let i = 0; while (yield i) { i++; } if (yield 9) { return 1; } return 2; }
         let it = g();
         console.log(it.next().value, it.next(true).value, it.next(true).value);
         console.log(it.next(false).value, it.next(0).value);",
        "0 1 2\n9 2\n",
    );
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
fn yield_delegate_over_an_array_and_a_string() {
    assert_output(
        "function* g(){ yield* [1,2]; yield* \"ab\" } console.log([...g()].join(\",\"))",
        "1,2,a,b\n",
    );
}

#[test]
fn yield_delegate_spread_of_an_inner_generator() {
    assert_output(
        "function* inner(){ yield 1; yield 2 }
         function* g(){ yield* inner(); yield 3 }
         console.log([...g()])",
        "[ 1, 2, 3 ]\n",
    );
}

#[test]
fn yield_delegate_dispatches_on_the_runtime_operand() {
    assert_output(
        "function* inner(){ yield 5 }
         function* g(x){ yield* x }
         console.log([...g([7,8])], [...g(\"hi\")].join(\",\"), [...g(inner())])",
        "[ 7, 8 ] h,i [ 5 ]\n",
    );
}

#[test]
fn yield_delegate_over_an_array_reads_a_live_length() {
    assert_output(
        "function* g(x){ yield* x }
         let a = [1, 2, 3];
         let out = \"\";
         for (const v of g(a)) { if (v === 1) { a.pop(); } out = out + v; }
         console.log(out);",
        "12\n",
    );
}

#[test]
fn for_of_over_a_delegating_generator() {
    assert_output(
        "function* g(){ yield* [1,2]; yield* \"c\" }
         let out = \"\";
         for (const v of g()) { out = out + v; }
         console.log(out);",
        "12c\n",
    );
}

#[test]
fn yield_delegate_over_a_non_iterable_throws_a_type_error() {
    assert_uncaught(
        "function* g(){ yield* 5 } [...g()]",
        "TypeError: yield* operand is not iterable",
    );
}

#[test]
fn yield_delegate_non_iterable_type_error_is_catchable() {
    assert_bool(
        "function* g(x){ yield* x }
         let caught = false;
         try { [...g({ a: 1 })]; } catch (e) { caught = e instanceof TypeError; }
         caught",
        true,
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
        "TypeError: o.next is not a function",
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

/// `.next` is only the generator step for a generator receiver: an object or
/// class instance with its own `next` method gets an ordinary method call
/// (`this` bound, every argument passed).
#[test]
fn a_user_next_method_is_called_on_a_non_generator() {
    assert_number("const o = { next() { return 7; } }; o.next()", 7.0);
    assert_number(
        "const o = { next(a, b, c) { return a + b + c; } }; o.next(1, 2, 3)",
        6.0,
    );
    assert_number(
        "class C { constructor() { this.n = 10; } next(x) { return this.n + x; } }
         const c = new C(); c.next(5)",
        15.0,
    );
    assert_number(
        "const it = { i: 0, next() { this.i = this.i + 1; return { value: this.i, done: this.i > 2 }; } };
         it.next(); it.next().value",
        2.0,
    );
}

/// A user iterator object and a generator mix in one program: each `.next()`
/// dispatches on its own receiver.
#[test]
fn user_next_and_generator_next_coexist() {
    assert_number(
        "function* g(){ yield 40 }
         const o = { next() { return 2; } };
         g().next().value + o.next()",
        42.0,
    );
}

/// A nullish `.next()` receiver throws the ordinary member-read `TypeError`.
#[test]
fn a_nullish_next_receiver_throws_a_type_error() {
    assert_uncaught(
        "let u; u.next();",
        "TypeError: Cannot read properties of undefined (reading 'next')",
    );
    assert_uncaught(
        "let n = null; n.next();",
        "TypeError: Cannot read properties of null (reading 'next')",
    );
}

// --- %GeneratorPrototype% -------------------------------------------------------

#[test]
fn generator_objects_inherit_the_generator_prototype() {
    assert_eval("function* g() {} typeof g().next", "function");
    assert_bool(
        "function* g() {} function* h() {} g().next === h().next",
        true,
    );
    // %GeneratorPrototype% inherits from %Object.prototype%.
    assert_bool(
        "function* g() {} g().hasOwnProperty === Object.prototype.hasOwnProperty",
        true,
    );
    assert_eval("function* g() {} g().toString()", "[object Generator]");
    assert_bool("function* g() {} g().hasOwnProperty('next')", false);
    for (name, length) in [("next", 1.0), ("return", 1.0), ("throw", 1.0)] {
        assert_eval(&format!("function* g() {{}} g().{name}.name"), name);
        assert_number(&format!("function* g() {{}} g().{name}.length"), length);
    }
}

#[test]
fn next_through_a_value_and_on_a_wrong_receiver() {
    assert_number(
        "function* g() { yield 4 } const it = g(); const next = it.next; next.call(it).value",
        4.0,
    );
    assert_uncaught(
        "function* g() {} g().next.call({})",
        "TypeError: next method called on incompatible receiver",
    );
    // DF12: a user iterator's own `next`.
    assert_number(
        "const it = { next() { return { value: 3, done: false } } }; it.next().value",
        3.0,
    );
    // An own `next` on a generator object shadows the prototype's.
    assert_number(
        "function* g() { yield 1 } const it = g(); it.next = () => 8; it.next()",
        8.0,
    );
}

#[test]
fn generator_return_completes_the_generator() {
    assert_output(
        "function* g() { yield 1; yield 2 } const it = g(); \
         const r = it.return(5); console.log(r.value, r.done); \
         const n = it.next(); console.log(n.value, n.done)",
        "5 true\nundefined true\n",
    );
    assert_output(
        "function* g() { yield 1; yield 2 } const it = g(); it.next(); \
         const r = it.return(6); console.log(r.value, r.done, it.next().done)",
        "6 true true\n",
    );
    // A `try`/`catch` (no `finally`) does not see a return completion.
    assert_output(
        "function* g() { try { yield 1 } catch (e) { console.log('no') } yield 2 } \
         const it = g(); it.next(); console.log(it.return(7).value, it.next().done)",
        "7 true\n",
    );
    assert_output(
        "function* g() {} const it = g(); it.next(); console.log(it.return().value)",
        "undefined\n",
    );
}

#[test]
fn generator_return_through_finally_is_a_named_limitation() {
    assert_uncaught(
        "function* g() { try { yield 1 } finally { console.log('f') } } \
         const it = g(); it.next(); it.return(1)",
        "TypeError: generator return() at a yield inside try/finally or yield* is not supported",
    );
    // Before the first `next` (or after completion) no `finally` is active.
    assert_bool(
        "function* g() { try { yield 1 } finally {} } g().return(1).done",
        true,
    );
}

#[test]
fn generator_throw_throws_at_the_suspended_yield() {
    assert_output(
        "function* g() { try { yield 1 } catch (e) { console.log('caught', e); yield 2 } } \
         const it = g(); it.next(); const r = it.throw('x'); console.log(r.value, r.done)",
        "caught x\n2 false\n",
    );
    assert_uncaught(
        "function* g() { yield 1 } const it = g(); it.next(); it.throw(new Error('boom'))",
        "Error: boom",
    );
    // Not started: the generator completes without running its body.
    assert_output(
        "function* g() { console.log('ran'); yield 1 } const it = g(); \
         try { it.throw(3) } catch (e) { console.log('t', e) } console.log(it.next().done)",
        "t 3\ntrue\n",
    );
    // An uncaught throw completes the generator.
    assert_output(
        "function* g() { yield 1; yield 2 } const it = g(); it.next(); \
         try { it.throw(4) } catch (e) {} console.log(it.next().done)",
        "true\n",
    );
}

#[test]
fn generator_throw_through_yield_star_is_a_named_limitation() {
    assert_uncaught(
        "function* inner() { yield 1 } function* g() { yield* inner() } \
         const it = g(); it.next(); it.throw(1)",
        "TypeError: generator throw() at a yield* is not supported",
    );
}

#[test]
fn resuming_a_running_generator_throws() {
    for method in ["next()", "return(1)", "throw(1)"] {
        assert_output(
            &format!(
                "let it; function* g() {{ it.{method}; }} it = g(); \
                 try {{ it.next() }} catch (e) {{ console.log(e.message) }} \
                 console.log(it.next().done)"
            ),
            "Generator is already running\ntrue\n",
        );
    }
}
