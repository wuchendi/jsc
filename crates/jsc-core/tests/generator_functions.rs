//! Generator and async functions beyond top-level declarations: function
//! expressions (anonymous and named), nested declarations, async arrows, and
//! their closure environments (captured bindings surviving suspension).

mod common;

use common::{assert_output, assert_uncaught};

/// Compile `src` and assert it is rejected at compile time with a message
/// containing `message`.
#[allow(clippy::expect_used)]
fn assert_compile_error(src: &str, message: &str) {
    let err = jsc_core::run::compile_and_run(src).expect_err("program should not compile");
    let text = err.to_string();
    assert!(
        text.contains(message),
        "expected a compile error containing `{message}` for `{src}`, got `{text}`"
    );
}

#[test]
fn an_anonymous_generator_expression_yields() {
    assert_output(
        "const g = function*(){ yield 1; yield 2; };
         console.log([...g()], g.name, g.length);",
        "[ 1, 2 ] g 0\n",
    );
}

#[test]
fn a_named_generator_expression_references_itself() {
    assert_output(
        "const g = function* count(n){ if (n > 0) { yield n; yield* count(n - 1); } };
         console.log([...g(3)], g.name, g.length);",
        "[ 3, 2, 1 ] count 1\n",
    );
}

#[test]
fn a_named_generator_expression_name_is_read_only() {
    assert_compile_error(
        "const g = function* h(){ h = 1; yield 0; }; g().next();",
        "cannot assign to constant binding: h",
    );
}

#[test]
fn a_nested_generator_declaration_in_a_function() {
    assert_output(
        "function outer(k){ const r = [...inner()]; function* inner(){ yield k; yield k * 2; } return r; }
         console.log(outer(4));",
        "[ 4, 8 ]\n",
    );
}

#[test]
fn a_nested_generator_declaration_in_a_block() {
    assert_output(
        "{ function* g(){ yield 9; } console.log([...g()]); }",
        "[ 9 ]\n",
    );
}

#[test]
fn a_generator_closure_mutates_an_outer_local_across_yields() {
    assert_output(
        "function outer(){ let n = 0; const g = function*(){ while (true) yield n++; }; return [g, () => n]; }
         const [g, peek] = outer();
         const it = g();
         console.log(it.next().value, it.next().value, it.next().value, peek());",
        "0 1 2 3\n",
    );
}

#[test]
fn generator_captures_outlive_the_creating_call() {
    assert_output(
        "function counter(){ let n = 0; return function*(){ while (true) yield n++; }; }
         const a = counter()(); const b = counter()();
         console.log(a.next().value, a.next().value, b.next().value, a.next().value);",
        "0 1 0 2\n",
    );
}

#[test]
fn a_generator_expression_drives_for_of_and_spread() {
    assert_output(
        "const xs = [10, 20];
         const g = function*(){ for (const x of xs) yield x + 1; };
         for (const v of g()) console.log(v);
         console.log([...g()]);",
        "11\n21\n[ 11, 21 ]\n",
    );
}

#[test]
fn a_generator_expression_passed_and_returned_as_a_value() {
    assert_output(
        "function take(gen, n){ const out = []; for (const v of gen()) { if (out.length === n) break; out.push(v); } return out; }
         console.log(take(function*(){ let i = 0; while (true) yield i++; }, 3));",
        "[ 0, 1, 2 ]\n",
    );
}

#[test]
fn a_generator_method_value_reads_its_receiver() {
    assert_output(
        "const o = { x: 7 }; o.g = function*(){ yield this.x; yield this.x + 1; };
         console.log([...o.g()]);",
        "[ 7, 8 ]\n",
    );
}

#[test]
fn new_on_a_generator_expression_throws_a_type_error() {
    assert_uncaught(
        "const g = function*(){ yield 1; }; new g();",
        "TypeError: g is not a constructor",
    );
}

#[test]
fn an_async_function_expression_awaits_a_promise() {
    assert_output(
        "const f = async function(x){ const v = await Promise.resolve(x); return v * 2; };
         f(21).then(v => console.log(v, f.name, f.length));",
        "42 f 1\n",
    );
}

#[test]
fn a_named_async_function_expression_references_itself() {
    assert_output(
        "const f = async function down(n){ if (n === 0) return 'done'; return await down(n - 1); };
         f(3).then(v => console.log(v));",
        "done\n",
    );
}

#[test]
fn a_nested_async_declaration_captures_outer_bindings() {
    assert_output(
        "function outer(){ let log = []; async function step(v){ const x = await v; log.push(x); return log; } return step; }
         const step = outer();
         step(1).then(() => step(Promise.resolve(2))).then(l => console.log(l));",
        "[ 1, 2 ]\n",
    );
}

#[test]
fn an_async_arrow_with_an_expression_body_awaits() {
    assert_output(
        "const k = 5; const f = async x => (await x) + k;
         f(Promise.resolve(1)).then(v => console.log(v, f.name));",
        "6 f\n",
    );
}

#[test]
fn an_async_arrow_with_a_block_body_awaits() {
    assert_output(
        "function outer(){ let total = 0; const add = async (a, b) => { total += await a; total += await b; return total; }; return add; }
         outer()(Promise.resolve(3), 4).then(v => console.log(v));",
        "7\n",
    );
}

#[test]
fn an_async_arrow_reads_the_enclosing_arguments() {
    assert_output(
        "function f(){ const g = async () => { await null; return arguments.length; }; return g(); }
         f(1, 2, 3).then(v => console.log(v));",
        "3\n",
    );
}

#[test]
fn an_async_arrow_returns_a_promise_and_is_not_a_constructor() {
    assert_output(
        "const f = async () => 1;
         console.log(f() instanceof Promise);
         try { new f(); } catch (e) { console.log(e instanceof TypeError); }",
        "true\ntrue\n",
    );
}

#[test]
fn async_generators_stay_a_compile_error() {
    assert_compile_error(
        "async function* f(){ yield 1 } f().next()",
        "async generator functions are unsupported",
    );
    assert_compile_error(
        "const f = async function*(){ yield 1 }; f().next()",
        "async generator functions are unsupported",
    );
    assert_compile_error(
        "function o(){ async function* f(){ yield 1 } return f }",
        "async generator functions are unsupported",
    );
}
