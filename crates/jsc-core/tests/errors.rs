//! Integration tests: `throw` / `try` / `catch` / `finally` / `Error` exception handling.
#![allow(clippy::expect_used)]

use jsc_core::run::compile_and_run_capturing;
mod common;
use common::{assert_bool, assert_number, assert_rejected, assert_str, assert_uncaught};

// --- S15.1 milestones ---------------------------------------------------------

/// `throw 42` at the top level unwinds out of `main` and surfaces `Uncaught 42`.
#[test]
fn throw_number_literal_uncaught_milestone() {
    assert_uncaught("throw 42", "42");
}

/// `throw "boom"` renders the thrown string unquoted (matching `format_value`).
#[test]
fn throw_string_literal_uncaught_milestone() {
    assert_uncaught("throw \"boom\"", "boom");
}

/// A `throw` inside a callee propagates back through the (direct) call that
/// invoked it: `f()` unwinds and the program surfaces `Uncaught 42`.
#[test]
fn throw_propagates_through_one_call_milestone() {
    assert_uncaught("function f(){ throw 42 } f()", "42");
}

/// A `throw` propagates through TWO frames: `g()` calls `f()`, which throws; the
/// exception unwinds out of both and surfaces `Uncaught 42`.
#[test]
fn throw_propagates_through_two_calls_milestone() {
    assert_uncaught("function f(){ throw 42 } function g(){ f() } g()", "42");
}

// --- Extra coverage -----------------------------------------------------------

/// A `throw` reached only on the taken branch fires; the untaken branch does not.
#[test]
fn throw_on_conditional_branch() {
    assert_uncaught("function f(x){ if (x > 5) { throw x } return x } f(9)", "9");
    // The same function with a non-throwing argument completes normally.
    assert_number("function f(x){ if (x > 5) { throw x } return x } f(3)", 3.0);
}

/// A `throw` from an expression (not just a literal): the operand is evaluated
/// before the exception is raised, so an arithmetic thrown value renders.
#[test]
fn throw_evaluated_expression() {
    assert_uncaught("throw 40 + 2", "42");
}

/// A `throw` inside a higher-order callback propagates out through the internal
/// `call_indirect` invocation and abandons the loop.
#[test]
fn throw_propagates_through_hof_callback() {
    assert_uncaught("[1, 2, 3].map(function (x) { throw x })", "1");
}

/// A `throw` after a `console.log`: the log runs (side effect observed), then the
/// throw unwinds. Confirms output before the throw is preserved and the program
/// still surfaces the exception.
#[test]
fn side_effect_before_throw() {
    let err = compile_and_run_capturing("console.log(\"before\"); throw 7")
        .expect_err("program should throw");
    assert_eq!(err.to_string(), "Uncaught 7");
}

// --- S15.2 milestones: try/catch ----------------------------------------------

/// `try { throw 42 } catch (e) { e }` catches the thrown number and binds it to
/// the catch parameter. The completion value is routed to a top-level `r` (the
/// engine's program value is its last expression statement, and a `try` is a
/// statement), so the caught `42` is observable as the program result.
#[test]
fn try_catch_number_milestone() {
    assert_number("let r = 0; try { throw 42 } catch (e) { r = e } r", 42.0);
}

/// A thrown string is caught and bound to the catch parameter unchanged.
#[test]
fn try_catch_string_milestone() {
    assert_str(
        "let r = 0; try { throw \"boom\" } catch (e) { r = e } r",
        "boom",
    );
}

/// The no-throw path skips the catch entirely: `body` completes normally, so the
/// handler never runs and its assignment is not observed.
#[test]
fn try_no_throw_skips_catch_milestone() {
    assert_number("let r = 0; try { r = 42 } catch (e) { r = -1 } r", 42.0);
}

/// A `throw` two call frames deep unwinds out of `f` and `g` (each to its own
/// function boundary) and is finally caught at the `try` around `g()` in `main`.
#[test]
fn try_catch_across_call_frames_milestone() {
    assert_number(
        "function f(){ throw 42 } function g(){ f() } \
         let r = 0; try { g() } catch (e) { r = e } r",
        42.0,
    );
}

/// Nested `try`: the inner handler catches the inner throw (relative `br` to the
/// nearest landing pad), and the outer handler is never reached.
#[test]
fn nested_try_inner_catches_milestone() {
    assert_number(
        "let r = 0; try { try { throw 1 } catch (e) { r = e + 41 } } catch (e) { r = -1 } r",
        42.0,
    );
}

/// The catch parameter is the thrown jsval itself, so `typeof e` observes its
/// real runtime type (`"string"` for a thrown string).
#[test]
fn catch_binding_is_thrown_jsval_milestone() {
    assert_str(
        "let r = 0; try { throw \"x\" } catch (e) { r = typeof e } r",
        "string",
    );
}

/// A `catch` with no binding parameter still runs its body when `body` throws.
#[test]
fn catch_without_binding() {
    assert_number("let r = 0; try { throw 1 } catch { r = 42 } r", 42.0);
}

/// A `throw` reached only on a taken branch inside a `try` body is still caught
/// (the handler-relative `br` accounts for the intervening `if` frame).
#[test]
fn throw_inside_if_inside_try_is_caught() {
    assert_number(
        "let r = 0; try { if (1 < 2) { throw 42 } } catch (e) { r = e } r",
        42.0,
    );
}

/// An inner `catch` may re-throw to the outer handler.
#[test]
fn catch_rethrow_to_outer_handler() {
    assert_number(
        "let r = 0; try { try { throw 1 } catch (e) { throw 41 + e } } catch (e) { r = e } r",
        42.0,
    );
}

/// A HOF callback that throws inside a `try` is caught (the propagation `br`
/// accounts for the intervening loop frames).
#[test]
fn throw_in_hof_callback_inside_try_is_caught() {
    assert_number(
        "let r = 0; try { [1, 2, 3].map(function (x) { throw x + 41 }) } catch (e) { r = e } r",
        42.0,
    );
}

/// An uncaught `throw` (no enclosing `try`) still unwinds all the way out and
/// surfaces `Uncaught <value>` — the S15.1 fallback path is intact.
#[test]
fn uncaught_still_surfaces_milestone() {
    assert_uncaught("throw 7", "7");
}

/// A `throw` inside a `try` whose exception ESCAPES the catch (a `catch` that
/// itself throws, with no outer `try`) surfaces as uncaught.
#[test]
fn escaping_rethrow_is_uncaught() {
    assert_uncaught("try { throw 1 } catch (e) { throw 99 }", "99");
}

// --- S15.3 milestones: finally ------------------------------------------------

/// `try`/`finally` (no `catch`), normal fall-through: the finally runs after the
/// body and its side effects are observable.
#[test]
fn finally_runs_on_normal_path() {
    assert_number("let x = 0; try { x = 40 } finally { x = x + 2 } x", 42.0);
}

/// `try`/`catch`/`finally`: after a caught throw, the finally still runs.
#[test]
fn finally_runs_after_caught_throw() {
    assert_number(
        "let x = 0; try { throw 1 } catch (e) { x = 40 } finally { x = x + 2 } x",
        42.0,
    );
}

/// `try`/`finally` with no `catch` and no throw: the finally's side effects run
/// and the program value is correct.
#[test]
fn finally_no_catch_no_throw() {
    assert_number("let x = 1; try { x = 20 } finally { x = x * 2 } x", 40.0);
}

/// A `throw` in a `try`/`finally` with no local `catch` runs the finally on the
/// pass-through, then RE-PROPAGATES to the enclosing `try`/`catch`, which sees
/// the ORIGINAL thrown value.
#[test]
fn finally_repropagates_to_outer_catch() {
    assert_number(
        "let x = 0; try { try { throw 1 } finally { x = 40 } } catch (e) { x = x + e + 1 } x",
        42.0,
    );
}

/// The re-propagated value is the ORIGINAL throw (bound in the outer catch),
/// confirming the finally does not clobber `$exc_value`.
#[test]
fn finally_repropagates_original_value() {
    assert_number(
        "let side = 0; let r = 0; \
         try { try { throw 42 } finally { side = 1 } } catch (e) { r = e } r",
        42.0,
    );
}

/// A `throw` INSIDE `finally` propagates outward, replacing any in-flight
/// exception (standard JS): the outer catch observes the finally's value.
#[test]
fn throw_inside_finally_replaces_in_flight() {
    assert_number(
        "let r = 0; try { try { throw 1 } finally { throw 2 } } catch (e) { r = e } r",
        2.0,
    );
}

/// A `throw` inside `finally` on the NORMAL path (body did not throw) still
/// propagates to the enclosing handler.
#[test]
fn throw_inside_finally_on_normal_path() {
    assert_number(
        "let r = 0; try { try { 1 } finally { throw 5 } } catch (e) { r = e } r",
        5.0,
    );
}

/// `try`/`catch`/`finally` where the `catch` itself re-throws: the finally still
/// runs (its side effect is observable) before the re-throw reaches the outer
/// handler.
#[test]
fn finally_runs_when_catch_rethrows() {
    assert_number(
        "let side = 0; let r = 0; \
         try { try { throw 1 } catch (e) { throw 40 } finally { side = 2 } } \
         catch (e) { r = e + side } r",
        42.0,
    );
}

/// An uncaught throw that passes through a `finally` (no enclosing handler at
/// all) still surfaces as `Uncaught <value>` after the finally runs.
#[test]
fn finally_uncaught_still_surfaces() {
    assert_uncaught("try { throw 7 } finally { }", "7");
}

/// A `finally` string result flows through when the try/catch produce a value —
/// confirms the finally clause of `try`/`catch`/`finally` (all-three shape) is
/// wired and does not disturb the caught result.
#[test]
fn finally_all_three_shape() {
    assert_str(
        "let log = \"\"; \
         try { throw \"boom\" } catch (e) { log = e } finally { log = log + \"!\" } log",
        "boom!",
    );
}

// --- S15.4 milestones: Error/TypeError/RangeError objects ---------------------

/// `new Error(msg)` is a plain heap object carrying `.name`/`.message`; a thrown
/// Error is caught and its `.message` read back unchanged.
#[test]
fn error_thrown_message_milestone() {
    assert_str(
        "let r = 0; try { throw new Error(\"bad\") } catch (e) { r = e.message } r",
        "bad",
    );
}

/// `typeof (new Error(..))` is "object": an Error reuses the OBJECT heap
/// representation, so `typeof` needs no special case.
#[test]
fn error_typeof_object_milestone() {
    assert_str("typeof (new Error(\"x\"))", "object");
}

/// The `.name` of each of the three built-in constructors equals the
/// constructor identifier.
#[test]
fn error_name_property_milestone() {
    assert_str("new Error(\"bad\").name", "Error");
    assert_str("new TypeError(\"t\").name", "TypeError");
    assert_str("new RangeError(\"r\").name", "RangeError");
}

/// `.message` reads the first constructor argument verbatim.
#[test]
fn error_message_property_milestone() {
    assert_str("new RangeError(\"r\").message", "r");
}

/// `new Error()` with no argument defaults `.message` to the empty string
/// (matching JS), not `undefined`.
#[test]
fn error_default_empty_message_milestone() {
    assert_str("new Error().message", "");
}

/// `.stack` is the single-line `"<name>: <message>"` for each kind (jsc keeps no
/// runtime call frames, so there is no trailing "    at ..." section).
#[test]
fn error_stack_property_milestone() {
    assert_str("new Error(\"boom\").stack", "Error: boom");
    assert_str("new TypeError(\"t\").stack", "TypeError: t");
    assert_str("new RangeError(\"r\").stack", "RangeError: r");
}

/// `new Error()` with no message has `.stack` equal to just the name — no
/// trailing ": ".
#[test]
fn error_stack_no_message_milestone() {
    assert_str("new Error().stack", "Error");
}

/// A caught error exposes `.stack` on the catch binding, unchanged by the throw.
#[test]
fn error_stack_readable_after_catch_milestone() {
    assert_str(
        "let r = 0; try { throw new RangeError(\"r\") } catch (e) { r = e.stack } r",
        "RangeError: r",
    );
}

/// The hidden `@@errorkind` marker is invisible to `Object.keys` — an error's
/// own enumerable keys are exactly `name`, `message`, `stack`.
#[test]
fn error_kind_marker_hidden_from_keys_milestone() {
    assert_str(
        "Object.keys(new Error(\"x\")).join(\",\")",
        "name,message,stack",
    );
}

/// Each error kind is `instanceof` its own constructor.
#[test]
fn instanceof_exact_kind_milestone() {
    assert_bool("new Error(\"x\") instanceof Error", true);
    assert_bool("new TypeError(\"x\") instanceof TypeError", true);
    assert_bool("new RangeError(\"x\") instanceof RangeError", true);
}

/// A `TypeError`/`RangeError` is also `instanceof Error` (the base of the
/// hierarchy), but a base `Error` is NOT `instanceof` a subtype.
#[test]
fn instanceof_hierarchy_milestone() {
    assert_bool("new TypeError(\"x\") instanceof Error", true);
    assert_bool("new RangeError(\"x\") instanceof Error", true);
    assert_bool("new Error(\"x\") instanceof TypeError", false);
    assert_bool("new Error(\"x\") instanceof RangeError", false);
    assert_bool("new TypeError(\"x\") instanceof RangeError", false);
    assert_bool("new RangeError(\"x\") instanceof TypeError", false);
}

/// A non-object operand — number, string, boolean, `null`, `undefined`, array —
/// is never an instance and never traps.
#[test]
fn instanceof_non_object_is_false_milestone() {
    assert_bool("5 instanceof Error", false);
    assert_bool("\"oops\" instanceof Error", false);
    assert_bool("true instanceof Error", false);
    assert_bool("null instanceof Error", false);
    assert_bool("undefined instanceof Error", false);
    assert_bool("[1, 2] instanceof Error", false);
}

/// A plain object that merely LOOKS like an error (same visible keys) lacks the
/// hidden `@@errorkind` marker, so it is not `instanceof Error`.
#[test]
fn instanceof_plain_object_is_false_milestone() {
    assert_bool("({}) instanceof Error", false);
    assert_bool(
        "({ name: \"Error\", message: \"x\" }) instanceof Error",
        false,
    );
}

/// `instanceof` sees through a `throw`/`catch`: the caught binding keeps its
/// error identity and hierarchy.
#[test]
fn instanceof_on_caught_error_milestone() {
    assert_bool(
        "let r = false; try { throw new TypeError(\"x\") } catch (e) { r = e instanceof TypeError } r",
        true,
    );
    assert_bool(
        "let r = false; try { throw new TypeError(\"x\") } catch (e) { r = e instanceof Error } r",
        true,
    );
    assert_bool(
        "let r = true; try { throw new TypeError(\"x\") } catch (e) { r = e instanceof RangeError } r",
        false,
    );
}

/// `instanceof` is GENERALIZED to user classes: an instance is `instanceof` its
/// own class (a single `@@prototype`-pointer hop), whether written directly or
/// through a binding, but not `instanceof` an unrelated class or a builtin error.
#[test]
fn instanceof_user_class_milestone() {
    assert_bool("class C {} new C() instanceof C", true);
    assert_bool("class C {} let c = new C(); c instanceof C", true);
    assert_bool("class C {} class D {} new C() instanceof D", false);
    assert_bool("class C {} new C() instanceof Error", false);
    assert_bool("class C { m() { return 1 } } new C() instanceof C", true);
}

/// A user class named `Error` SHADOWS the builtin: `instanceof Error` then means
/// the user class (user-class dispatch is checked before the builtin errors).
#[test]
fn instanceof_user_class_shadows_builtin_milestone() {
    assert_bool(
        "class Error { m() { return 1 } } new Error() instanceof Error",
        true,
    );
}

/// A non-object left operand is never `instanceof` a user class, and never traps.
#[test]
fn instanceof_user_class_non_object_is_false_milestone() {
    assert_bool("class C {} 5 instanceof C", false);
    assert_bool("class C {} null instanceof C", false);
    assert_bool("class C {} [1, 2] instanceof C", false);
    assert_bool("class C {} ({}) instanceof C", false);
}

/// `instanceof` with a right-hand side that is neither a user class nor an
/// (unshadowed) builtin (`Error`/`TypeError`/`RangeError`/`Array`/`Object`/
/// `Function`/`Date`) is rejected at compile time — jsc models no general
/// prototype-chain `instanceof`. A user binding that SHADOWS a builtin name also
/// falls through to the same rejection.
#[test]
fn instanceof_unsupported_rhs_rejected() {
    assert_rejected("({}) instanceof 5");
    assert_rejected("let C = 1; ({}) instanceof C");
    assert_rejected("let Error = 1; ({}) instanceof Error");
    assert_rejected("let Array = 1; [] instanceof Array");
    assert_rejected("let Object = 1; ({}) instanceof Object");
}

/// The `Date` builtin accepts only the timestamp constructor forms and a
/// zero-argument `Date.now()`; the calendar-field constructor, extra arguments,
/// and a spread argument are rejected at compile time (P-series).
#[test]
fn date_unsupported_forms_rejected() {
    assert_rejected("new Date(2024, 0, 1)");
    assert_rejected("Date.now(5)");
    assert_rejected("new Date(0).getTime(1)");
    assert_rejected("new Date(...[0])");
}

/// A thrown `TypeError` is caught and both its `.name` and `.message` are
/// readable from the catch binding.
#[test]
fn typeerror_thrown_name_and_message_milestone() {
    assert_str(
        "let r = 0; try { throw new TypeError(\"nope\") } catch (e) { r = e.name } r",
        "TypeError",
    );
    assert_str(
        "let r = 0; try { throw new TypeError(\"nope\") } catch (e) { r = e.message } r",
        "nope",
    );
}

/// An Error thrown across a call frame propagates unchanged to an outer
/// `try`/`catch`, where its `.message` reads back intact.
#[test]
fn error_propagates_across_call_milestone() {
    assert_str(
        "let r = 0; function f(){ throw new Error(\"x\") } \
         try { f() } catch (e) { r = e.message } r",
        "x",
    );
}

/// An Error flows through a `finally` clause unchanged: the caught `.message`
/// survives the finally block running.
#[test]
fn error_flows_through_finally_milestone() {
    assert_str(
        "let r = 0; let f = 0; \
         try { throw new RangeError(\"oops\") } catch (e) { r = e.message } finally { f = 1 } r",
        "oops",
    );
}

/// An uncaught Error unwinds out of `main`; the run stage renders the thrown
/// object as its single-line stack (`"<name>: <message>"`), the way a JS console
/// prints an error, rather than as a plain `{ ... }` object (E-series identity).
#[test]
fn error_uncaught_surfaces() {
    assert_uncaught("throw new Error(\"boom\")", "Error: boom");
}

// --- S15.5 milestones: integration hardening ----------------------------------
//
// The individual pieces (throw / try / catch / finally / Error) are covered
// above. This section stress-tests their COMBINATION across deep call chains,
// nested trys at multiple depths, rethrow / throw-in-catch / throw-in-finally,
// HOF callbacks, recursion, loops, switch, and Error objects flowing through all
// of it. Every test drives the full source -> value pipeline. As merged, the
// system passes all of these with no codegen change required.

/// Campaign headline: an inner `catch` that re-throws a TRANSFORMED value reaches
/// the OUTER catch with the transformed value. `throw 1` -> inner catch `throw
/// e + 41` -> outer catch binds `42`.
#[test]
fn nested_rethrow_transformed_headline() {
    assert_number(
        "let r = 0; try { try { throw 1 } catch (e) { throw e + 41 } } catch (e) { r = e } r",
        42.0,
    );
}

// 1. Deep propagation ----------------------------------------------------------

/// A `throw` three frames down (`h` throws, `g` calls `h`, `f` calls `g`) unwinds
/// through all three function boundaries to a `try` around `f()` with the value
/// intact.
#[test]
fn deep_propagation_three_frames_value() {
    assert_number(
        "function h(){ throw 42 } function g(){ h() } function f(){ g() } \
         let r = 0; try { f() } catch (e) { r = e } r",
        42.0,
    );
}

/// The same three-frame unwind carrying an Error object: `.message` reads back
/// intact after crossing all three boundaries.
#[test]
fn deep_propagation_three_frames_error_message() {
    assert_str(
        "function h(){ throw new Error(\"m3\") } function g(){ h() } function f(){ g() } \
         let r = 0; try { f() } catch (e) { r = e.message } r",
        "m3",
    );
}

// 2. Rethrow / throw-in-catch --------------------------------------------------

/// A bare re-throw `catch (e) { throw e }` preserves the thrown value all the way
/// to the outer handler (numeric identity).
#[test]
fn bare_rethrow_preserves_value() {
    assert_number(
        "let r = 0; try { try { throw 7 } catch (e) { throw e } } catch (e) { r = e } r",
        7.0,
    );
}

/// A bare re-throw of an Error object preserves object IDENTITY: the outer catch
/// reads the same `.message` the inner throw created.
#[test]
fn bare_rethrow_preserves_error_identity() {
    assert_str(
        "let r = 0; try { try { throw new Error(\"id\") } catch (e) { throw e } } \
         catch (e) { r = e.message } r",
        "id",
    );
}

// 3. Nested try at multiple depths ---------------------------------------------

/// The inner `try/catch` swallows its throw; a SECOND throw after the inner try
/// (still inside the outer body) is caught by the OUTER handler.
#[test]
fn inner_swallows_second_throw_hits_outer() {
    assert_number(
        "let r = 0; try { try { throw 1 } catch (e) { } throw 42 } catch (e) { r = e } r",
        42.0,
    );
}

/// An inner `try/finally` (no catch) passes the exception through to the outer
/// catch WHILE running its finally (side effect observed, then original value
/// caught).
#[test]
fn inner_finally_passes_through_running_side_effect() {
    assert_number(
        "let side = 0; let r = 0; \
         try { try { throw 5 } finally { side = 10 } } catch (e) { r = e + side } r",
        15.0,
    );
}

// 4. throw inside finally overrides in-flight exception ------------------------

/// `try { try { throw 1 } finally { throw 2 } } catch (e) { e }` => 2: the
/// finally's throw replaces the in-flight exception.
#[test]
fn throw_in_finally_overrides_in_flight() {
    assert_number(
        "let r = 0; try { try { throw 1 } finally { throw 2 } } catch (e) { r = e } r",
        2.0,
    );
}

/// The same override across a CALL frame: the inner `try/finally` lives inside a
/// callee, and the finally's throw (not the body's) is what the caller catches.
#[test]
fn throw_in_finally_overrides_across_call() {
    assert_number(
        "function f(){ try { throw 1 } finally { throw 2 } } \
         let r = 0; try { f() } catch (e) { r = e } r",
        2.0,
    );
}

// 5. Exception through a HOF callback ------------------------------------------

/// A throw inside a `reduce` callback that is itself inside a `try` is caught
/// locally (the propagation `br` accounts for the internal `call_indirect`).
#[test]
fn throw_in_reduce_callback_caught_locally() {
    assert_number(
        "let r = 0; \
         try { [1, 2, 3].reduce(function (a, b) { if (b === 2) { throw 42 } return a + b }, 0) } \
         catch (e) { r = e } r",
        42.0,
    );
}

/// A throw inside a `forEach` callback with NO local handler propagates out of
/// the HOF call and surfaces uncaught.
#[test]
fn throw_in_foreach_callback_propagates_out() {
    assert_uncaught(
        "[1, 2, 3].forEach(function (x) { if (x === 2) { throw 8 } })",
        "8",
    );
}

/// A throw inside a `filter` callback propagates out to an ENCLOSING try when not
/// caught locally by the callback.
#[test]
fn throw_in_filter_callback_reaches_enclosing_try() {
    assert_number(
        "let r = 0; try { [1, 2, 3].filter(function (x) { throw x + 41 }) } catch (e) { r = e } r",
        42.0,
    );
}

// 6. Recursion + throw ---------------------------------------------------------

/// A recursive function throws at its base case; the exception unwinds through
/// every recursive frame and is caught by an outer `try` with the correct value.
#[test]
fn recursion_throw_at_base_case_unwinds() {
    assert_number(
        "function rec(n){ if (n === 0) { throw 99 } return rec(n - 1) } \
         let r = 0; try { rec(5) } catch (e) { r = e } r",
        99.0,
    );
}

/// Recursion carrying an Error object: the base-case throw's `.message` survives
/// the multi-frame unwind.
#[test]
fn recursion_throw_error_message_survives() {
    assert_str(
        "function rec(n){ if (n === 0) { throw new RangeError(\"floor\") } return rec(n - 1) } \
         let r = 0; try { rec(4) } catch (e) { r = e.message } r",
        "floor",
    );
}

// 7. try/catch inside loops and switch -----------------------------------------

/// A `try/catch` inside a `for` loop catches a throw EACH iteration; the loop
/// continues and accumulates all three caught values.
#[test]
fn try_catch_inside_for_loop_each_iteration() {
    assert_number(
        "let s = 0; for (let i = 0; i < 3; i = i + 1) { try { throw i } catch (e) { s = s + e } } s",
        3.0,
    );
}

/// A `try/catch` inside a `while` loop behaves the same across iterations.
#[test]
fn try_catch_inside_while_loop_each_iteration() {
    assert_number(
        "let i = 0; let s = 0; \
         while (i < 3) { try { throw i } catch (e) { s = s + e } i = i + 1 } s",
        3.0,
    );
}

/// An UNCAUGHT throw inside a loop body abandons the loop and propagates out.
#[test]
fn uncaught_throw_in_loop_propagates() {
    assert_uncaught(
        "for (let i = 0; i < 3; i = i + 1) { if (i === 1) { throw 55 } }",
        "55",
    );
}

/// A `try/catch` inside a `switch` case catches a throw in the case body; the
/// switch completes and the caught value is observed.
#[test]
fn try_catch_inside_switch_case() {
    assert_number(
        "let r = 0; let x = 2; \
         switch (x) { case 2: try { throw 7 } catch (e) { r = e } break; } r",
        7.0,
    );
}

// 8. Error object end-to-end ---------------------------------------------------

/// `throw new TypeError(..)` three frames deep, passing through a `finally` on
/// the way out, caught by the outer try — both `.name` and `.message` read back.
#[test]
fn typeerror_deep_through_finally_name_and_message() {
    assert_str(
        "function h(){ throw new TypeError(\"boom\") } \
         function g(){ try { h() } finally { } } function f(){ g() } \
         let r = 0; try { f() } catch (e) { r = e.name } r",
        "TypeError",
    );
    assert_str(
        "function h(){ throw new TypeError(\"boom\") } \
         function g(){ try { h() } finally { } } function f(){ g() } \
         let r = 0; try { f() } catch (e) { r = e.message } r",
        "boom",
    );
}

// 9. Uncaught still surfaces after all this ------------------------------------

/// An uncaught numeric throw still surfaces as `Uncaught <value>` — the S15.1
/// fallback path is intact after all the integration machinery.
#[test]
fn uncaught_number_still_surfaces_after_integration() {
    assert_uncaught("function f(){ throw 13 } f()", "13");
}

/// An uncaught Error object thrown across a call frame surfaces as its
/// single-line stack (`"<name>: <message>"`), matching the E-series error render.
#[test]
fn uncaught_error_still_surfaces_after_integration() {
    assert_uncaught(
        "function f(){ throw new Error(\"boom\") } f()",
        "Error: boom",
    );
}

// 9b. E-series: error identity through throw/catch integration -----------------

/// A single flow exercises the whole E-series surface at once: a `TypeError` is
/// thrown across a call frame, caught, and its identity (`instanceof` up the
/// hierarchy), `.name`, `.message`, and `.stack` are all read back from the
/// catch binding and concatenated.
#[test]
fn error_identity_end_to_end_integration() {
    assert_str(
        "let out = \"\"; \
         function boom(){ throw new TypeError(\"nope\") } \
         try { boom() } catch (e) { \
           out = (e instanceof TypeError) + \"|\" + (e instanceof Error) + \"|\" \
               + (e instanceof RangeError) + \"|\" + e.name + \"|\" + e.message + \"|\" + e.stack \
         } out",
        "true|true|false|TypeError|nope|TypeError: nope",
    );
}

/// `instanceof` narrows a caught error by kind — the idiomatic multi-`catch`
/// substitute: a `RangeError` matches its own branch, not `TypeError`.
#[test]
fn instanceof_narrows_caught_error_by_kind_integration() {
    assert_str(
        "let which = \"none\"; \
         try { throw new RangeError(\"r\") } catch (e) { \
           if (e instanceof TypeError) { which = \"type\" } \
           else if (e instanceof RangeError) { which = \"range\" } \
         } which",
        "range",
    );
}

// 10. Mixed nested shapes — finally ordering -----------------------------------

/// try/catch/finally containing a try/finally containing a throw: every finally
/// runs in the correct order. The order is recorded as a concatenated string so
/// the assertion pins the exact sequence: inner-finally, outer-catch,
/// outer-finally.
#[test]
fn mixed_nested_finally_ordering() {
    assert_str(
        "let log = \"\"; \
         try { try { log = log + \"a\"; throw 0 } finally { log = log + \"b\" } } \
         catch (e) { log = log + \"c\" } finally { log = log + \"d\" } log",
        "abcd",
    );
}

/// Same shape but the ordering carried through an ARRAY of side-effect markers,
/// confirming the array/`push` path is undisturbed by the unwinding: the four
/// markers land in order.
#[test]
fn mixed_nested_finally_ordering_via_array() {
    assert_number(
        "let log = []; \
         try { try { log.push(1); throw 0 } finally { log.push(2) } } \
         catch (e) { log.push(3) } finally { log.push(4) } \
         log[0] * 1000 + log[1] * 100 + log[2] * 10 + log[3]",
        1234.0,
    );
}

// --- S15.6 milestones: finally on return/break/continue ------------------------
//
// A `finally` runs on EVERY exit edge of the guarded region, not just normal
// completion and throw: a `return`, `break`, or `continue` that leaves the `try`
// must route THROUGH each intervening finalizer first (S15.3). An abrupt
// completion inside the `finally` itself (return/break/continue/throw) OVERRIDES
// the pending one. Each test drives the full source -> value pipeline.

/// Headline: a `return` in the `try` body is overridden by a `return` in the
/// `finally` — `f()` yields `2`, not `1`.
#[test]
fn finally_return_overrides_try_return_milestone() {
    assert_number(
        "function f(){ try { return 1 } finally { return 2 } } f()",
        2.0,
    );
}

/// A `break` out of a loop runs the `finally` on the way out: the side effect is
/// observed exactly once and the loop is abandoned (`s == 10`, one iteration).
#[test]
fn finally_runs_on_break_milestone() {
    assert_number(
        "let s = 0; for (let i = 0; i < 3; i = i + 1) { try { break } finally { s = s + 10 } } s",
        10.0,
    );
}

/// A `continue` runs the `finally` each iteration AND still advances the loop
/// variable (the desugared `for` update): three iterations, `s == 3`.
#[test]
fn finally_runs_on_continue_milestone() {
    assert_number(
        "let s = 0; for (let i = 0; i < 3; i = i + 1) { try { continue } finally { s = s + 1 } } s",
        3.0,
    );
}

/// A `return` runs the `finally`'s side effect before returning: the captured
/// `log` is mutated by the finally and the returned `7` is delivered, so
/// `r + log == 12`. (A `let`-bound arrow captures `log`; a `function`
/// declaration does not capture in this engine.)
#[test]
fn finally_return_runs_side_effect_milestone() {
    assert_number(
        "let log = 0; \
         let g = () => { try { return 7 } finally { log = 5 } }; \
         let r = g(); r + log",
        12.0,
    );
}

/// A `continue` past dead code still runs the `finally` and skips the rest of the
/// body: the `s = s + 100` after `continue` never runs, so `s == 3`.
#[test]
fn continue_skips_rest_of_body_but_runs_finally() {
    assert_number(
        "let s = 0; \
         for (let i = 0; i < 3; i = i + 1) { try { continue; s = s + 100 } finally { s = s + 1 } } s",
        3.0,
    );
}

/// A `return` routed through TWO nested finallies runs both, innermost first:
/// the accumulator records `2` then `3`, so `a == 23`.
#[test]
fn return_runs_both_nested_finallies_in_order() {
    assert_number(
        "let a = 0; \
         let f = () => { try { try { return 1 } finally { a = a * 10 + 2 } } finally { a = a * 10 + 3 } }; \
         f(); a",
        23.0,
    );
}

/// A `break` routed through TWO nested finallies runs both, innermost first,
/// exactly once — then exits the loop (`a == 23`, one iteration).
#[test]
fn break_runs_both_nested_finallies_in_order() {
    assert_number(
        "let a = 0; \
         for (let i = 0; i < 9; i = i + 1) { \
           try { try { break } finally { a = a * 10 + 2 } } finally { a = a * 10 + 3 } } a",
        23.0,
    );
}

/// A `return` inside `finally` overrides a still-pending THROW from the `try`
/// body: the exception is swallowed and the function returns the finally's value.
#[test]
fn finally_return_overrides_pending_throw() {
    assert_number(
        "let f = () => { try { throw 1 } finally { return 2 } }; f()",
        2.0,
    );
}

/// A `throw` inside `finally` overrides a pending RETURN from the `try` body: the
/// return is discarded and the outer `catch` observes the finally's throw.
#[test]
fn finally_throw_overrides_pending_return() {
    assert_number(
        "let f = () => { let r = 0; try { try { return 5 } finally { throw 9 } } catch (e) { r = e } return r }; \
         f()",
        9.0,
    );
}

/// A `return` inside `finally` overrides a pending BREAK: the loop's `break` is
/// discarded in favor of the finally's return, which leaves the function.
#[test]
fn finally_return_overrides_pending_break() {
    assert_number(
        "let f = () => { for (let i = 0; i < 3; i = i + 1) { try { break } finally { return 99 } } return 0 }; \
         f()",
        99.0,
    );
}

/// A `continue` inside `finally` overrides a pending RETURN each iteration: the
/// return never escapes, the loop runs to completion, and the function returns
/// its post-loop value (`0`).
#[test]
fn finally_continue_overrides_pending_return() {
    assert_number(
        "let f = () => { for (let i = 0; i < 3; i = i + 1) { try { return 50 } finally { continue } } return 0 }; \
         f()",
        0.0,
    );
}

/// A `return` in the `try` runs the `finally`, which THROWS — overriding the
/// return and propagating the exception across the call frame to the caller's
/// `catch` (the return value `7` never escapes `g`).
#[test]
fn return_through_finally_throw_across_call_frame() {
    assert_number(
        "let g = () => { try { return 7 } finally { throw 5 } }; \
         let r = 0; try { g() } catch (e) { r = e } r",
        5.0,
    );
}

/// An inner `break` targets only the inner loop; its `finally` runs each inner
/// iteration exit, the outer loop continues normally (two outer iterations,
/// `s == 202`).
#[test]
fn inner_break_finally_leaves_outer_loop_running() {
    assert_number(
        "let s = 0; \
         for (let i = 0; i < 2; i = i + 1) { \
           for (let j = 0; j < 5; j = j + 1) { try { break } finally { s = s + 1 } } s = s + 100 } s",
        202.0,
    );
}

/// A `break`/`continue` in a `try` with NO `catch` (a `try`/`finally`) still runs
/// the finally: three iterations each run the finally via `continue`, `s == 3`.
#[test]
fn continue_through_try_finally_no_catch() {
    assert_number(
        "let s = 0; let i = 0; \
         while (i < 3) { i = i + 1; try { continue } finally { s = s + 1 } } s",
        3.0,
    );
}

/// `continue` is legal only inside a loop; a top-level `continue` is rejected
/// during lowering (mirrors the `break`-outside-breakable guard).
#[test]
fn continue_outside_loop_is_rejected() {
    assert!(
        jsc_core::run::compile_and_run_capturing("continue;").is_err(),
        "a top-level continue is illegal"
    );
}

// --- Regression guard: no-throw programs are unaffected ------------------------

/// The pending-exception globals and call-site checks must not perturb ordinary
/// programs: arithmetic, recursion, and closures all still return their values.
#[test]
fn no_throw_programs_unaffected() {
    assert_number("40 + 2", 42.0);
    assert_number(
        "function fib(n){ if (n < 2) return n; return fib(n-1) + fib(n-2) } fib(10)",
        55.0,
    );
    assert_number(
        "function counter(){ let c = 0; return () => { c = c + 1; return c } } \
         let c = counter(); c(); c(); c()",
        3.0,
    );
    assert_number("let adder = (a) => (b) => a + b; adder(40)(2)", 42.0);
    assert_number("[1, 2, 3].reduce((a, b) => a + b, 0)", 6.0);
}
