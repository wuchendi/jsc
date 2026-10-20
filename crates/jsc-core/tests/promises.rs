//! Integration tests: promises, microtasks, and top-level `await`.
//!
//! The covered surface is `Promise.resolve`/`Promise.reject`, `new Promise(executor)`,
//! `p.then`/`p.catch`/`p.finally`, a FIFO microtask queue drained by top-level
//! `await` and after `main`, and promise value rendering. `async function` bodies are
//! covered in the async/await section; `Promise.all`/`race` are out of scope.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_eval, assert_output, assert_rejected, assert_uncaught};
use jsc_core::run::compile_and_run;

// --- Microtask ordering -------------------------------------------------------

/// A `.then` callback runs as a MICROTASK — after the synchronous script
/// completes, not inline. So the two `console.log`s bracketing the `.then` both
/// print before the reaction.
#[test]
fn then_callback_runs_after_sync_code() {
    assert_output(
        "console.log(\"start\");\
         Promise.resolve(1).then(v => console.log(\"then\", v));\
         console.log(\"end\");",
        "start\nend\nthen 1\n",
    );
}

/// Two independent chains interleave in FIFO microtask order: each `.then` link
/// re-queues its successor at the back of the queue, so the second chain's first
/// link runs between the first chain's two links.
#[test]
fn microtask_queue_is_fifo() {
    assert_output(
        "Promise.resolve().then(() => console.log(\"a\")).then(() => console.log(\"c\"));\
         Promise.resolve().then(() => console.log(\"b\"));",
        "a\nb\nc\n",
    );
}

/// Reactions scheduled without any top-level `await` still run — the after-`main`
/// drain flushes the queue.
#[test]
fn unawaited_reactions_drain_after_main() {
    assert_output("Promise.resolve(7).then(v => console.log(v));", "7\n");
}

// --- Top-level await ----------------------------------------------------------

/// Top-level `await` drives the queue until the promise settles, then yields the
/// fulfillment value as the awaiting expression's value.
#[test]
fn await_yields_fulfillment_value() {
    assert_eval("await Promise.resolve(42)", "42");
}

/// `await` of a non-promise yields the value unchanged (no wrapping observable).
#[test]
fn await_non_promise_is_identity() {
    assert_eval("await 99", "99");
}

/// A `.then(f).then(g)` chain threads each callback's return into the next; the
/// whole chain is awaited to its final value.
#[test]
fn then_chain_threads_values() {
    assert_eval(
        "await Promise.resolve(1).then(v => v + 1).then(v => v * 10)",
        "20",
    );
}

/// A callback that returns a promise is ADOPTED: the chained promise settles with
/// the inner promise's eventual value, not with the promise itself.
#[test]
fn then_returning_promise_is_adopted() {
    assert_eval(
        "await Promise.resolve(3).then(v => Promise.resolve(v * 10))",
        "30",
    );
}

/// `await` YIELDS to the microtask queue: a `.then` queued before the `await`
/// runs before the code after it, even though the awaited promise is already
/// fulfilled. (Draining to empty at each `await` models top-level-await ordering
/// in this timer-less runtime.)
#[test]
fn await_yields_to_microtask_queue() {
    assert_output(
        "Promise.resolve().then(() => console.log(\"A\"));\
         const v = await Promise.resolve(42);\
         console.log(\"B\", v);",
        "A\nB 42\n",
    );
}

// --- Rejection paths ----------------------------------------------------------

/// `.catch(f)` handles a rejection and recovers the chain to a fulfilled value.
#[test]
fn catch_handles_rejection() {
    assert_output(
        "await Promise.reject(\"boom\").catch(e => console.log(\"caught\", e));",
        "caught boom\n",
    );
}

/// A rejection SKIPS a `.then` with no rejection handler and reaches the next
/// `.catch` — the fulfillment handler never runs.
#[test]
fn rejection_skips_then_reaches_catch() {
    assert_output(
        "await Promise.reject(\"e1\")\
           .then(v => console.log(\"unreached\"))\
           .catch(e => console.log(\"late\", e));",
        "late e1\n",
    );
}

/// A `throw` inside a `.then` callback rejects its chained promise, caught by a
/// downstream `.catch`.
#[test]
fn throw_in_then_is_caught() {
    assert_output(
        "await Promise.resolve(1)\
           .then(v => { throw \"bang\"; })\
           .catch(e => console.log(\"got\", e));",
        "got bang\n",
    );
}

/// Awaiting a promise that rejects (with no handler) surfaces the reason as an
/// uncaught exception, exactly like a synchronous `throw`.
#[test]
fn await_rejection_is_uncaught() {
    assert_uncaught("await Promise.reject(\"nope\")", "nope");
}

// --- finally ------------------------------------------------------------------

/// `.finally(f)` runs its callback on fulfillment and passes the original value
/// through to the chained promise.
#[test]
fn finally_runs_and_passes_value_through() {
    assert_output(
        "await Promise.resolve(5)\
           .finally(() => console.log(\"fin\"))\
           .then(v => console.log(\"val\", v));",
        "fin\nval 5\n",
    );
}

/// `.finally(f)` runs on rejection too, and the rejection still propagates.
#[test]
fn finally_runs_on_rejection() {
    assert_output(
        "await Promise.reject(\"x\")\
           .finally(() => console.log(\"cleanup\"))\
           .catch(e => console.log(\"then\", e));",
        "cleanup\nthen x\n",
    );
}

// --- Rendering ----------------------------------------------------------------

/// A settled promise renders Node-console style by settlement kind.
#[test]
fn promise_rendering() {
    assert_output("console.log(Promise.resolve(7));", "Promise { 7 }\n");
    assert_output(
        "const p = Promise.reject(\"e\"); console.log(p); p.catch(() => {});",
        "Promise { <rejected> e }\n",
    );
    // A `.then` result is still pending when logged (its reaction is a queued
    // microtask that has not run yet).
    assert_output(
        "console.log(Promise.resolve(1).then(v => v));",
        "Promise { <pending> }\n",
    );
}

/// `typeof` a promise is `"object"`.
#[test]
fn typeof_promise_is_object() {
    assert_eval("typeof Promise.resolve(1)", "object");
}

// --- new Promise(executor) ----------------------------------------------------

/// `new Promise((res) => res(v))` — a synchronously resolved promise, awaited to
/// its value.
#[test]
fn new_promise_synchronous_resolve() {
    assert_eval("await new Promise((res) => res(7))", "7");
}

/// `new Promise((res, rej) => rej(e))` — a synchronously rejected promise, caught.
#[test]
fn new_promise_synchronous_reject() {
    assert_output(
        "await new Promise((res, rej) => rej(\"boom\")).catch(e => console.log(e));",
        "boom\n",
    );
}

/// A THROWING executor rejects the promise (the throw does not propagate out of
/// `new Promise`), caught by a downstream `.catch`.
#[test]
fn new_promise_throwing_executor_rejects() {
    assert_output(
        "await new Promise((res, rej) => { throw \"kaboom\"; }).catch(e => console.log(e));",
        "kaboom\n",
    );
}

/// The executor may resolve ASYNCHRONOUSLY — store `resolve` and call it from a
/// later microtask. `await` drives the queue until that happens. This also
/// regression-guards capture analysis descending into an awaited expression: the
/// `() => res(v)` reaction is a closure over the executor's `res` parameter, which
/// must be boxed even though it is nested under `await new Promise(...)`.
#[test]
fn new_promise_asynchronous_resolve() {
    assert_eval(
        "await new Promise((res) => { Promise.resolve().then(() => res(99)); })",
        "99",
    );
}

/// A never-resolved promise renders as pending.
#[test]
fn new_promise_pending_renders() {
    assert_output(
        "console.log(new Promise(() => {}));",
        "Promise { <pending> }\n",
    );
}

/// `new Promise` requires exactly one (executor) argument.
#[test]
fn new_promise_wrong_arity_is_rejected() {
    assert_rejected("new Promise()");
}

// --- Out-of-scope boundary ----------------------------------------------------

/// An unknown `Promise.<name>` static (e.g. `Promise.all`) is a compile error.
#[test]
fn unknown_promise_static_is_rejected() {
    assert_rejected("Promise.all([])");
}

// --- async/await (generator + promise driven) ---------------------------------

/// An `async function` returns a promise; with no `await`, the body runs to
/// completion synchronously and the promise is already fulfilled with the return
/// value.
#[test]
fn async_no_await_returns_fulfilled_promise() {
    assert_eval("async function f() { return 1; } f()", "Promise { 1 }");
}

/// A `.then` on an async function's result observes its return value.
#[test]
fn async_return_observed_via_then() {
    assert_output(
        "async function f() { return 42; } f().then(v => console.log(v));",
        "42\n",
    );
}

/// `await` on a non-promise resumes with the value itself (`Promise.resolve`
/// semantics) — but still yields to the microtask queue, so code after the async
/// call runs before the body resumes.
#[test]
fn async_await_non_promise_value() {
    assert_output(
        "async function f() { let x = await 5; return x + 1; } f().then(v => console.log(v));",
        "6\n",
    );
}

/// `await` on a promise resumes with its fulfillment value.
#[test]
fn async_await_promise_value() {
    assert_output(
        "async function f() { return await Promise.resolve(10); } \
         f().then(v => console.log(v));",
        "10\n",
    );
}

/// An `async function` suspends at its first `await` and returns control to the
/// caller: the synchronous code after the call runs before the body resumes, and
/// the two halves of the body (split by `await`) run as later microtasks.
#[test]
fn async_suspends_at_await() {
    assert_output(
        "async function f() { await 1; console.log(\"a\"); await 2; console.log(\"b\"); } \
         console.log(\"start\"); f(); console.log(\"end\");",
        "start\nend\na\nb\n",
    );
}

/// An `async function` can `await` another async function's result, threading the
/// resolved value through.
#[test]
fn async_awaits_async() {
    assert_eval(
        "async function inner() { return await 21; } \
         async function outer() { let r = await inner(); return r * 2; } \
         await outer()",
        "42",
    );
}

/// Multiple sequential `await`s in one body each resume with their settled value.
#[test]
fn async_sequential_awaits() {
    assert_output(
        "async function f() { let a = await 10; let b = await 20; return a + b; } \
         f().then(v => console.log(v));",
        "30\n",
    );
}

/// A `throw` in an async body (before any `await`) rejects the returned promise.
#[test]
fn async_throw_rejects_result() {
    assert_output(
        "async function f() { throw \"boom\"; } f().catch(e => console.log(\"caught\", e));",
        "caught boom\n",
    );
}

/// `await` on a rejected promise with no enclosing handler propagates the
/// rejection out as the async function's own rejection.
#[test]
fn async_await_rejection_propagates() {
    assert_output(
        "async function f() { await Promise.reject(\"nope\"); return 1; } \
         f().catch(e => console.log(e));",
        "nope\n",
    );
}

/// An uncaught rejection inside an async body rejects its result promise; with
/// no handler on the result, the run exits as an unhandled rejection.
#[test]
fn async_await_rejection_unhandled_is_uncaught() {
    assert_uncaught(
        "async function f() { await Promise.reject(\"nope\"); return 1; } f(); 0",
        "(in promise) nope",
    );
}

/// A rejected `await` THROWS into the body at the suspended `await`: the
/// enclosing `catch` binds the rejection reason.
#[test]
fn async_await_rejection_is_caught() {
    assert_output(
        "async function f(){ try { await Promise.reject(new Error(\"boom\")) } \
         catch (e) { return \"caught \" + e.message } } f().then(v => console.log(v))",
        "caught boom\n",
    );
}

/// A caught rejection is not reported as an unhandled rejection.
#[test]
fn async_caught_rejection_is_not_unhandled() {
    assert_eval(
        "async function f(){ try { await Promise.reject(1) } catch (e) { return e + 1 } } await f()",
        "2",
    );
}

/// A non-Error rejection value is bound to the catch parameter unchanged.
#[test]
fn async_await_rejection_non_error_value() {
    assert_output(
        "async function f(){ try { await Promise.reject(42) } catch (e) { console.log(e, typeof e) } } f()",
        "42 number\n",
    );
}

/// A rejected `await` inside `try`/`finally` runs the finalizer, then the
/// rejection still rejects the result promise.
#[test]
fn async_await_rejection_runs_finally() {
    assert_output(
        "async function f(){ try { await Promise.reject(\"r\") } finally { console.log(\"fin\") } } \
         f().catch(e => console.log(\"rejected\", e))",
        "fin\nrejected r\n",
    );
}

/// After a caught rejection, later awaits in the same body (inside a `try`
/// again) continue normally.
#[test]
fn async_awaits_continue_after_caught_rejection() {
    assert_output(
        "async function f(){ let log = \"\"; \
         try { await Promise.reject(\"a\") } catch (e) { log = log + e } \
         try { const v = await Promise.resolve(\"b\"); log = log + v; const w = await 3; log = log + w } \
         catch (e) { log = log + \"!\" } \
         return log } f().then(v => console.log(v))",
        "ab3\n",
    );
}

/// Control flow (an `if`) may cross an `await`: the state machine resumes into the
/// correct branch.
#[test]
fn async_control_flow_across_await() {
    assert_output(
        "async function f() { let x = await 5; if (x > 3) { return \"big\"; } return \"small\"; } \
         f().then(v => console.log(v));",
        "big\n",
    );
}

/// A loop body may `await` each iteration (via `x = await e`, the supported yield
/// position); the accumulator persists across every suspend.
#[test]
fn async_loop_with_await() {
    assert_output(
        "async function f() { let s = 0; for (let i = 1; i <= 3; i++) { let t = await i; s = s + t; } return s; } \
         f().then(v => console.log(v));",
        "6\n",
    );
}

/// A top-level `await` drives an async function to completion and yields its value.
#[test]
fn top_level_await_of_async_result() {
    assert_eval("async function f(x) { return x * 2; } await f(21)", "42");
}

/// Returning a promise from an async body ADOPTS it: the result promise settles
/// with the inner promise's value.
#[test]
fn async_return_promise_adopts() {
    assert_output(
        "async function f() { return Promise.resolve(5); } f().then(v => console.log(v));",
        "5\n",
    );
}

/// A `.then` chain on an async result runs in microtask order.
#[test]
fn async_result_then_chain() {
    assert_output(
        "async function f() { return 7; } \
         f().then(v => v + 1).then(v => console.log(v));",
        "8\n",
    );
}

/// An async function expression, an async arrow, and an async generator remain
/// out of scope (only top-level `async function` DECLARATIONS are supported).
#[test]
fn async_non_declaration_forms_rejected() {
    assert_rejected("let f = async function () { return 1 }; f()");
    assert_rejected("let f = async () => 1; f()");
    assert_rejected("async function* f() { yield 1 } f().next()");
}

/// A C-style `for (let ...)` loop with an `await` in its body inside an async
/// function still runs each iteration in order.
#[test]
fn for_let_loop_with_await_in_async_function() {
    assert_output(
        "async function f(){ let s = \"\"; for (let i=0;i<3;i++) { const v = await Promise.resolve(i); s = s + v; } return s; }
         f().then(v => console.log(v));",
        "012\n",
    );
}

/// `.then`/`.catch`/`.finally` are only the promise methods for a promise
/// receiver: an object or class instance with a same-named method gets an
/// ordinary method call (`this` bound, every argument passed).
#[test]
fn user_then_catch_finally_methods_are_called_on_non_promises() {
    assert_eval("const t = { then(a) { return a + 1; } }; t.then(1)", "2");
    assert_eval(
        "const t = { then(a, b, c) { return a + b + c; } }; t.then(1, 2, 3)",
        "6",
    );
    assert_eval(
        "const o = { catch(e) { return e * 2; }, finally() { return \"f\"; } }; o.catch(4) + o.finally()",
        "8f",
    );
    assert_eval(
        "class T { constructor() { this.k = 3; } then(f) { return f(this.k); } }
         new T().then(v => v * 2)",
        "6",
    );
}

/// A nullish `.then`/`.catch`/`.finally` receiver throws the ordinary
/// member-read `TypeError`.
#[test]
fn nullish_promise_method_receiver_throws_a_type_error() {
    assert_uncaught(
        "let n = null; n.then();",
        "TypeError: Cannot read properties of null (reading 'then')",
    );
    assert_uncaught(
        "let u; u.catch(() => 1);",
        "TypeError: Cannot read properties of undefined (reading 'catch')",
    );
    assert_uncaught(
        "let u; u.finally();",
        "TypeError: Cannot read properties of undefined (reading 'finally')",
    );
}

/// A non-promise primitive without a `then` method throws "not a function".
#[test]
fn then_on_a_primitive_throws_a_type_error() {
    assert_uncaught(
        "let x = 5; x.then();",
        "TypeError: x.then is not a function",
    );
}

// --- Unhandled rejections -----------------------------------------------------

/// Run `src` and assert it fails with `Uncaught (in promise) <expected>`.
fn assert_unhandled(src: &str, expected: &str) {
    let err = compile_and_run(src).expect_err("program should report an unhandled rejection");
    assert_eq!(
        err.to_string(),
        format!("Uncaught (in promise) {expected}"),
        "unhandled-rejection message for source `{src}`"
    );
}

/// A rejection that never gets a handler fails the run, rendered like an
/// uncaught exception (Error objects as `Name: message`, primitives as-is).
#[test]
fn unhandled_rejection_is_reported() {
    assert_unhandled("Promise.reject(new Error(\"x\")); 0", "Error: x");
    assert_unhandled("Promise.reject(new TypeError(\"t\")); 0", "TypeError: t");
    assert_unhandled("Promise.reject(1); 0", "1");
    assert_unhandled("new Promise((res, rej) => rej(\"s\")); 0", "s");
}

/// A `.then` without a rejection handler forwards the rejection to its result
/// promise, which is itself unhandled.
#[test]
fn unhandled_rejection_through_then_passthrough() {
    assert_unhandled("Promise.reject(2).then(v => v); 0", "2");
}

/// A throwing `.then` callback rejects the chained promise; unhandled, it fails.
#[test]
fn unhandled_rejection_from_throwing_reaction() {
    assert_unhandled(
        "Promise.resolve(1).then(v => { throw \"bang\"; }); 0",
        "bang",
    );
}

/// An async function that throws and is never awaited fails the run.
#[test]
fn unhandled_async_throw_is_reported() {
    assert_unhandled(
        "async function f() { throw new Error(\"a\"); } f(); 0",
        "Error: a",
    );
    assert_unhandled(
        "async function f() { await Promise.reject(\"r\"); } f(); 0",
        "r",
    );
}

/// The FIRST unhandled rejection (creation order) is the one reported.
#[test]
fn first_unhandled_rejection_wins() {
    assert_unhandled("Promise.reject(\"a\"); Promise.reject(\"b\"); 0", "a");
}

/// A handler (catch / then / finally+catch / await / adoption) marks the
/// rejection handled, including one attached later in the same run.
#[test]
fn handled_rejections_complete_normally() {
    assert_eval("Promise.reject(1).catch(() => {}); 0", "0");
    assert_eval("Promise.reject(1).then(v => v, e => e); 0", "0");
    assert_eval(
        "Promise.reject(1).finally(() => {}).catch(() => {}); 0",
        "0",
    );
    assert_eval(
        "const p = Promise.reject(1); Promise.resolve(0).then(() => p.catch(() => {})); 0",
        "0",
    );
    assert_eval(
        "async function f() { throw 1; } async function g() { await f(); } \
         g().catch(() => {}); 0",
        "0",
    );
    assert_eval(
        "async function f() { throw 1; } f().catch(() => {}); 0",
        "0",
    );
    assert_eval(
        "new Promise((res) => res(Promise.reject(1))).catch(() => {}); 0",
        "0",
    );
}

/// A synchronous uncaught exception takes precedence over a pending unhandled
/// rejection.
#[test]
fn sync_uncaught_beats_unhandled_rejection() {
    assert_uncaught("Promise.reject(1); throw 2;", "2");
}

/// A closure created before an `await` captures the async frame's parameter
/// cell and reads it after resumption.
#[test]
fn closure_created_before_await_captures_param() {
    assert_output(
        "async function a(n){ const f = () => n; await 0; return f() } a(3).then(v=>console.log(v))",
        "3\n",
    );
}

#[test]
fn closure_created_after_await_captures_param() {
    assert_output(
        "async function a(n){ await 0; const f = () => n; return f() } a(3).then(v=>console.log(v))",
        "3\n",
    );
}

/// A closure and the async body share a local's cell across an `await`.
#[test]
fn closure_shares_async_local_across_await() {
    assert_output(
        "async function a(){ let s = 1; const bump = () => { s = s * 3; }; bump(); await 0; bump(); await 0; return s; }
         a().then(v => console.log(v));",
        "9\n",
    );
}

/// A closure passed as a `.then` reaction inside an async body captures its
/// local and runs after the function has already returned.
#[test]
fn closure_over_async_local_runs_as_reaction() {
    assert_output(
        "async function a(){ let tag = \"x\"; await 0; Promise.resolve(1).then(v => console.log(tag, v)); tag = \"y\"; }
         a();",
        "y 1\n",
    );
}

/// An `await` nested in an expression keeps JS evaluation order: the operands
/// before it run synchronously, the rest after the microtask resumes the body.
#[test]
fn nested_await_preserves_evaluation_order() {
    assert_output(
        "let log = \"\";
         function t(s, v){ log = log + s; return v; }
         async function f(){ let r = t(\"a\", 1) + (await t(\"b\", 2)) + t(\"c\", 3); console.log(r, log); }
         f(); log = log + \"|\";",
        "6 ab|c\n",
    );
}

/// A store after a nested `await` targets the receiver evaluated before it.
#[test]
fn nested_await_store_targets_the_receiver_captured_before_it() {
    assert_output(
        "let box = {o: {x: 0}};
         let first = box.o;
         async function f(){ box.o.x = await 5; console.log(first.x, box.o.x); }
         f(); box.o = {x: 0};",
        "5 0\n",
    );
}

/// An `await` in a `for-of` body over an array keeps the loop's cursor across
/// each resume.
#[test]
fn await_in_a_for_of_body() {
    assert_output(
        "async function f(xs){ let s = 0; for (const x of xs) { s += await x } return s }
         f([1, 2, 3]).then(v => console.log(v));",
        "6\n",
    );
}

/// An `await` in a `for-in` body walks every key of the snapshot.
#[test]
fn await_in_a_for_in_body() {
    assert_output(
        "async function f(o){ let out = \"\"; for (const k in o) { out = out + k + (await o[k]); } return out }
         f({ a: 1, b: 2 }).then(v => console.log(v));",
        "a1b2\n",
    );
}

/// An `await` inside a `switch` case resumes into the selected clause, and
/// `break` after the resume leaves the switch.
#[test]
fn await_in_a_switch_case() {
    assert_output(
        "async function f(x){
           let r = \"\";
           switch (await x) {
             case 1: r = r + (await \"one\"); break;
             case 2: r = r + \"two\";
             default: r = r + (await \"dflt\");
           }
           return r;
         }
         f(1).then(v => console.log(v));
         f(2).then(v => console.log(v));",
        "one\ntwodflt\n",
    );
}
