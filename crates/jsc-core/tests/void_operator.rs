//! The `void` unary operator: the operand is evaluated for its side effects
//! (calls, assignments, throws, `yield` / `await`) and the result is
//! `undefined`.

mod common;

use common::{assert_bool, assert_eval, assert_output, assert_str, assert_uncaught};

#[test]
fn void_literal_is_undefined() {
    assert_eval("void 0", "undefined");
    assert_eval("void 'x'", "undefined");
    assert_eval("void null", "undefined");
    assert_bool("void 0 === undefined", true);
    assert_str("typeof void 0", "undefined");
}

#[test]
fn void_runs_call_side_effects() {
    assert_output(
        "let n = 0; function f() { n++; return 42; } console.log(void f(), n); console.log((void f()), n)",
        "undefined 1\nundefined 2\n",
    );
}

#[test]
fn void_runs_assignment() {
    assert_output(
        "let a; let r = void (a = 1); console.log(r, a)",
        "undefined 1\n",
    );
}

#[test]
fn void_preserves_evaluation_order() {
    assert_output(
        "let log = []; function t(x) { log.push(x); return x; } let r = [t(1), void t(2), t(3)]; console.log(log.join(), r[1])",
        "1,2,3 undefined\n",
    );
}

#[test]
fn void_propagates_throws() {
    assert_uncaught("void missing", "ReferenceError: missing is not defined");
    assert_output(
        "try { void (() => { throw new Error('boom'); })(); } catch (e) { console.log(e.message) }",
        "boom\n",
    );
}

#[test]
fn void_in_default_parameter_and_destructuring() {
    assert_output(
        "function f(x = void 0, y = 7) { return [x, y]; } console.log(f()[0], f(1)[0], f(1, void 0)[1])",
        "undefined 1 7\n",
    );
    assert_output(
        "let { a = 5 } = { a: void 0 }; let [b = 6] = [void 0]; console.log(a, b)",
        "5 6\n",
    );
}

#[test]
fn void_in_generator_body() {
    assert_output(
        "function* g() { const r = void (yield 1); console.log(r); yield void 0; } \
         let it = g(); console.log(it.next().value); console.log(it.next(9).value)",
        "1\nundefined\nundefined\n",
    );
}

#[test]
fn void_in_async_body() {
    assert_output(
        "let n = 0; async function f() { const r = void (await Promise.resolve(n = 3)); return r; } \
         f().then((v) => console.log(v, n))",
        "undefined 3\n",
    );
}
