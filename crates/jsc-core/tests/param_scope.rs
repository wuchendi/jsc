//! Parameter scope: parameters bind left to right with a TDZ when the list
//! has parameter expressions, and a body `var` redeclaring a parameter keeps
//! its value — in every function kind (ordinary, arrow, method, generator,
//! async).

mod common;

use common::{assert_output, assert_rejected};

/// The JS helper every test uses: run `f`, printing its result or the name
/// of what it threw.
const RUN: &str =
    "function run(f){ try { console.log(f()); } catch (e) { console.log(e.name); } }\n";

fn check(src: &str, expected: &str) {
    assert_output(&format!("{RUN}{src}"), expected);
}

#[test]
fn a_default_reading_a_later_parameter_throws() {
    check(
        "function f(a = b, b){ return a; }
         const g = (a = b, b) => a;
         const o = { m(a = b, b){ return a; } };
         class C { m(a = b, b){ return a; } }
         run(() => f(undefined, 2));
         run(() => g(undefined, 2));
         run(() => o.m(undefined, 2));
         run(() => new C().m(undefined, 2));
         run(() => f(1, 2));",
        "ReferenceError\nReferenceError\nReferenceError\nReferenceError\n1\n",
    );
}

#[test]
fn a_default_reading_its_own_parameter_throws() {
    check(
        "function f(a = a){ return a; }
         const g = (a = a) => a;
         const o = { m(a = a + 1){ return a; } };
         run(() => f());
         run(() => g());
         run(() => o.m());
         run(() => f(3));",
        "ReferenceError\nReferenceError\nReferenceError\n3\n",
    );
}

#[test]
fn a_default_writing_a_later_parameter_throws() {
    check(
        "function f(a = (b = 1), b){ return a; }
         function t(a = typeof b, b){ return a; }
         run(() => f());
         run(() => t());",
        "ReferenceError\nReferenceError\n",
    );
}

#[test]
fn a_destructuring_default_reading_a_later_parameter_throws() {
    check(
        "function f({x = y}, y){ return x; }
         function g([x = y], y){ return x; }
         function h(x = y, {y}){ return x; }
         function k([x, z = x], w = z){ return [x, z, w]; }
         run(() => f({}, 1));
         run(() => g([], 1));
         run(() => h(undefined, {y: 1}));
         run(() => k([1]));",
        "ReferenceError\nReferenceError\nReferenceError\n[ 1, 1, 1 ]\n",
    );
}

#[test]
fn a_default_reads_an_earlier_destructured_parameter() {
    check(
        "function f([a], b = a){ return b; }
         const g = ({a}, b = a * 2) => b;
         run(() => f([5]));
         run(() => g({a: 4}));",
        "5\n8\n",
    );
}

#[test]
fn a_default_closure_sees_the_tdz_until_the_parameter_binds() {
    check(
        "function f(a = () => b, b){ return a(); }
         function g(a = (() => b)(), b){ return a; }
         function h(get = () => [x], [x]){ return get(); }
         run(() => f(undefined, 7));
         run(() => g(undefined, 7));
         run(() => h(undefined, [3]));",
        "7\nReferenceError\n[ 3 ]\n",
    );
}

#[test]
fn a_closure_captures_a_destructured_parameter() {
    check(
        "function f([a]){ return () => a; }
         function g({a}, b = 1){ return () => a + b; }
         run(() => f([4])());
         run(() => g({a: 4})());",
        "4\n5\n",
    );
}

#[test]
fn a_generator_parameter_tdz_throws_at_the_call() {
    check(
        "function* g(a = b, b){ yield a; }
         function* h(a = a){ yield a; }
         const o = { *m(a = b, b){ yield a; } };
         run(() => { g(undefined, 1); return 'created'; });
         run(() => { h(); return 'created'; });
         run(() => { o.m(undefined, 1); return 'created'; });
         run(() => [...g(1, 2)]);",
        "ReferenceError\nReferenceError\nReferenceError\n[ 1 ]\n",
    );
}

#[test]
fn an_async_parameter_tdz_rejects_the_promise() {
    assert_output(
        "async function f(a = b, b){ return a; }
         const g = async (a = a) => a;
         let p;
         try { p = f(undefined, 1); console.log('returned'); } catch (e) { console.log('threw'); }
         p.catch(e => console.log('f', e.name));
         g().catch(e => console.log('g', e.name));
         f(1, 2).then(v => console.log('ok', v));",
        "returned\nf ReferenceError\ng ReferenceError\nok 1\n",
    );
}

#[test]
fn var_redeclaring_a_simple_parameter_keeps_its_value() {
    check(
        "function f(a){ var a; return a; }
         function g(a){ var a = a + 1; return a; }
         function h(a){ const get = () => a; var a = 5; return get(); }
         const ar = (a) => { var a; return a; };
         const o = { m(a){ var a; return a; } };
         function v(){ var x = 1; var x; return x; }
         run(() => f(3));
         run(() => g(3));
         run(() => h(3));
         run(() => ar(3));
         run(() => o.m(3));
         run(() => v());",
        "3\n4\n5\n3\n3\n1\n",
    );
}

#[test]
fn var_redeclaring_a_parameter_with_expressions_copies_its_value() {
    check(
        "function f(a = 1){ var a; return a; }
         function g(a, get = () => a){ var a = 5; return [a, get()]; }
         function h([a], b = 0){ var a; return a; }
         const ar = (a = 1) => { var a; return a; };
         function k(a, get = () => a){ var a; a = 9; return [a, get()]; }
         run(() => f(3));
         run(() => f());
         run(() => g(3));
         run(() => h([4]));
         run(() => ar(3));
         run(() => k(2));",
        "3\n1\n[ 5, 3 ]\n4\n3\n[ 9, 2 ]\n",
    );
}

#[test]
fn var_redeclaring_a_generator_or_async_parameter_keeps_its_value() {
    assert_output(
        "function* g(a){ var a; yield a; }
         function* h(a = 1, get = () => a){ var a = 5; yield a; yield get(); }
         async function f(a){ var a; await null; return a; }
         async function k(a = 2){ var a; return a; }
         console.log([...g(3)], [...h(3)]);
         f(4).then(v => console.log('f', v));
         k().then(v => console.log('k', v));",
        "[ 3 ] [ 5, 3 ]\nk 2\nf 4\n",
    );
}

#[test]
fn a_body_function_declaration_replaces_the_parameter() {
    check(
        "function f(a){ function a(){} return typeof a; }
         function g(a = 1){ function a(){} return typeof a; }
         function h(a){ function a(){} var a; return typeof a; }
         run(() => f(3));
         run(() => g(3));
         run(() => h(3));",
        "function\nfunction\nfunction\n",
    );
}

#[test]
fn let_redeclaring_a_parameter_is_an_early_error() {
    assert_rejected("function f(a){ let a; }");
    assert_rejected("const f = (a = 1) => { const a = 2; };");
    assert_rejected("function* g([a]){ let a; }");
    assert_rejected("async function f(a){ class a {} }");
}
