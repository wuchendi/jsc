//! Integration tests: promises, microtasks, and top-level `await` (P4.1).
//!
//! The P4.1 surface is `Promise.resolve`/`Promise.reject`, `new Promise(executor)`,
//! `p.then`/`p.catch`/`p.finally`, a FIFO microtask queue drained by top-level
//! `await` and after `main`, and promise value rendering. `Promise.all`/`race` and
//! `async function` bodies remain out of scope (M2+).
#![allow(clippy::expect_used)]

mod common;
use common::{assert_eval, assert_output, assert_rejected, assert_uncaught};

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
        "console.log(Promise.reject(\"e\"));",
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

// --- P4.3 M4: async/await (generator + promise driven) ------------------------

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

/// `await` on a rejected promise propagates the rejection out as the async
/// function's own rejection (no `try`/`catch` around `await` is modeled).
#[test]
fn async_await_rejection_propagates() {
    assert_output(
        "async function f() { await Promise.reject(\"nope\"); return 1; } \
         f().catch(e => console.log(e));",
        "nope\n",
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
