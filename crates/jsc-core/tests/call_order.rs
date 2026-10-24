//! Integration tests: the evaluation order of calls (ECMA-262 13.3.6.1):
//! the callee (member base, key, then `[[Get]]`), then the arguments left
//! to right, then `IsCallable` and the call (DESIGN.md §7.4).

mod common;
use common::assert_string;

/// `log` and `L(s)` (push `s`, return it) prepended to `body`.
fn logged(body: &str) -> String {
    format!("var log = []; function L(s) {{ log.push(s); return s }} {body}; log.join()")
}

#[test]
fn a_computed_callee_runs_before_the_arguments() {
    assert_string(
        &logged("function q() { log.push(\"q\"); return function () {} } q()(L(\"d\"))"),
        "q,d",
    );
    assert_string(
        &logged("var f = function () {}; (L(\"c\"), f)(L(\"a\"))"),
        "c,a",
    );
}

#[test]
fn a_binding_callee_is_read_before_an_argument_reassigns_it() {
    assert_string(
        &logged(
            "var f = function () { log.push(\"old\") }; \
             f((f = function () { log.push(\"new\") }, L(\"a\")))",
        ),
        "a,old",
    );
}

#[test]
fn a_method_getter_runs_before_the_arguments() {
    assert_string(
        &logged("var o = {get m() { log.push(\"get\"); return function () {} }}; o.m(L(\"a\"))"),
        "get,a",
    );
    assert_string(
        &logged(
            "var o = {get m() { log.push(\"get\"); return function () {} }}; o[L(\"k\") && \"m\"](L(\"a\"))",
        ),
        "k,get,a",
    );
    // A builtin method name on a receiver that does not take the fast path.
    assert_string(
        &logged(
            "var o = {get push() { log.push(\"get\"); return function () {} }}; o.push(L(\"a\"))",
        ),
        "get,a",
    );
    assert_string(
        &logged("var o = {get m() { log.push(\"get\"); return function () {} }}; o?.m(L(\"a\"))"),
        "get,a",
    );
    assert_string(
        &logged("var o = {get m() { log.push(\"get\"); return function () {} }}; o.m?.(L(\"a\"))"),
        "get,a",
    );
}

#[test]
fn the_method_read_before_the_arguments_is_the_one_called() {
    assert_string(
        &logged(
            "var o = {m: function () { log.push(\"old\") }}; \
             o.m((o.m = function () { log.push(\"new\") }, L(\"a\")))",
        ),
        "a,old",
    );
    // A builtin read before the arguments stays the callee even when an
    // argument shadows it with an own property.
    assert_string(
        "var a = []; a.push((a.push = function () { return -1 }, 5)) + \",\" + a.length",
        "1,1",
    );
    // An own method read before the arguments stays the callee even when an
    // argument makes the receiver take the builtin fast path.
    assert_string(
        "var a = []; Object.setPrototypeOf(a, {push: function () { return \"own\" }}); \
         a.push((Object.setPrototypeOf(a, Array.prototype), 1)) + \",\" + a.length",
        "own,0",
    );
}

#[test]
fn a_namespace_member_getter_runs_before_the_arguments() {
    assert_string(
        &logged(
            "var M = Math; Object.defineProperty(M, \"max\", {get: function () { \
               log.push(\"get\"); return function () { return 0 } }}); M.max(L(\"a\"))",
        ),
        "get,a",
    );
}

#[test]
fn a_spread_call_reads_its_callee_before_the_arguments() {
    assert_string(
        &logged("function q() { log.push(\"q\"); return function () {} } q()(...[L(\"d\")])"),
        "q,d",
    );
    assert_string(
        &logged(
            "var o = {get m() { log.push(\"get\"); return function () {} }}; o.m(...[L(\"a\")])",
        ),
        "get,a",
    );
}

#[test]
fn a_super_method_is_read_before_the_arguments() {
    assert_string(
        &logged(
            "class A { get m() { log.push(\"get\"); return function () {} } } \
             class B extends A { n() { super.m(L(\"a\")) } } new B().n()",
        ),
        "get,a",
    );
}

#[test]
fn new_evaluates_the_constructor_before_the_arguments() {
    assert_string(
        &logged("function C() { log.push(\"C\"); return function () {} } new (C())(L(\"a\"))"),
        "C,a",
    );
    assert_string(
        &logged(
            "var o = {get C() { log.push(\"get\"); return function () {} }}; new o.C(L(\"a\"))",
        ),
        "get,a",
    );
}

#[test]
fn a_non_callable_callee_throws_after_the_arguments() {
    assert_string(
        &logged("var o = {}; try { o.m(L(\"a\")) } catch (e) { log.push(e.constructor.name) }"),
        "a,TypeError",
    );
}

#[test]
fn a_suspending_argument_runs_after_the_method_read() {
    let getter = "var o = {get m() { log.push(\"get\"); return function (x) { return x } }};";
    assert_string(
        &logged(&format!(
            "{getter} function* g() {{ return o.m(L(\"a\"), yield 1) }} \
             var it = g(); it.next(); it.next(5)"
        )),
        "get,a",
    );
    assert_string(
        &logged(&format!(
            "{getter} function* g() {{ return o.push(L(\"a\"), yield 1) }} \
             var it = g(); it.next(); it.next(5)"
        ))
        .replace("get m", "get push"),
        "get,a",
    );
    assert_string(
        &logged(
            "var M = Math; Object.defineProperty(M, \"max\", {get: function () { \
               log.push(\"get\"); return function () { return 0 } }}); \
             function* g() { return M.max(L(\"a\"), yield 1) } var it = g(); it.next(); it.next(5)",
        ),
        "get,a",
    );
    assert_string(
        "var o = {m: function (a, b) { return this === o ? a + b : -1 }}; \
         function* g() { return o.m(1, yield 0) } var it = g(); it.next(); String(it.next(2).value)",
        "3",
    );
}
