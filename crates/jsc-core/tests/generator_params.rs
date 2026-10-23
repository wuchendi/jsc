//! Non-simple parameters (defaults, destructuring, rest) and `arguments` in
//! generator and async functions: parameters bind when the function is
//! called, `arguments` is the call's arguments object and survives
//! suspension.

mod common;

use common::assert_output;

#[test]
fn a_generator_default_parameter_may_read_earlier_ones() {
    assert_output(
        "function* g(a, b = a + 1, c = () => a + b){ yield a; yield b; yield c(); }
         console.log([...g(1)], [...g(1, 5)], g.length);",
        "[ 1, 2, 3 ] [ 1, 5, 6 ] 1\n",
    );
}

#[test]
fn a_default_closure_shares_the_parameter_cell_with_the_body() {
    assert_output(
        "function* g(a, get = () => a){ a = a * 10; yield get(); a = 7; yield get(); }
         console.log([...g(2)]);",
        "[ 20, 7 ]\n",
    );
}

#[test]
fn a_generator_destructures_its_parameters() {
    assert_output(
        "function* g([a, [b = 2] = []], {x, y: {z = 9} = {}} = {x: 4}){ yield a; yield b; yield x; yield z; }
         console.log([...g([1])], [...g([1, [3]], {x: 5, y: {z: 6}})]);",
        "[ 1, 2, 4, 9 ] [ 1, 3, 5, 6 ]\n",
    );
}

#[test]
fn a_generator_rest_parameter_gathers_the_surplus() {
    assert_output(
        "function* g(a, ...rest){ yield a; yield rest; }
         console.log([...g(1, 2, 3)], [...g()], g.length);",
        "[ 1, [ 2, 3 ] ] [ undefined, [] ] 1\n",
    );
}

#[test]
fn generator_methods_and_expressions_take_non_simple_parameters() {
    assert_output(
        "const o = { *m({a} = {a: 1}, ...r){ yield a + r.length; } };
         class C { *m(x = 3){ yield x; } static *s([y]){ yield y; } }
         const e = function*(n = 4){ yield n; };
         console.log([...o.m()], [...new C().m()], [...C.s([5])], [...e()]);",
        "[ 1 ] [ 3 ] [ 5 ] [ 4 ]\n",
    );
}

#[test]
fn a_throwing_default_throws_from_the_generator_call() {
    assert_output(
        "let ran = false;
         function boom(){ throw new Error('boom'); }
         function* g(a = boom()){ ran = true; yield a; }
         try { g(); console.log('no throw'); } catch (e) { console.log(e.message, ran); }",
        "boom false\n",
    );
}

#[test]
fn parameter_binding_runs_before_the_first_next() {
    assert_output(
        "const log = [];
         function* g(a = log.push('default')){ log.push('body'); yield a; }
         const it = g();
         log.push('called');
         it.next();
         console.log(log);",
        "[ default, called, body ]\n",
    );
}

#[test]
fn a_failing_generator_destructuring_throws_a_type_error_on_call() {
    assert_output(
        "function* g([a]){ yield a; }
         try { g(null); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
}

#[test]
fn an_async_function_binds_defaults_and_destructuring() {
    assert_output(
        "async function f(a, b = a * 2, {c} = {c: a + 1}, ...r){ await null; return [a, b, c, r]; }
         const arrow = async ([x] = [8]) => (await x) + 1;
         f(1, undefined, undefined, 4).then(v => console.log(v));
         arrow().then(v => console.log(v));",
        "[ 1, 2, 2, [ 4 ] ]\n9\n",
    );
}

#[test]
fn a_failing_async_destructuring_rejects_the_promise() {
    assert_output(
        "async function f({a}){ return a; }
         let p;
         try { p = f(undefined); console.log('returned'); } catch (e) { console.log('threw'); }
         p.then(() => console.log('fulfilled'), e => console.log(e instanceof TypeError));",
        "returned\ntrue\n",
    );
}

#[test]
fn a_throwing_async_default_rejects_the_promise() {
    assert_output(
        "const f = async (a = (() => { throw new RangeError('r'); })()) => a;
         f().catch(e => console.log(e.name, e.message));
         console.log('sync');",
        "sync\nRangeError r\n",
    );
}

#[test]
fn arguments_in_a_generator_survives_a_yield() {
    assert_output(
        "function* g(a){ yield arguments.length; yield arguments[0]; const f = () => arguments[1]; yield f(); }
         console.log([...g(7, 8)]);",
        "[ 2, 7, 8 ]\n",
    );
}

#[test]
fn arguments_in_a_generator_is_unmapped_and_taken_at_call_time() {
    assert_output(
        "function* g(a){ a = 5; yield arguments[0]; yield arguments.length; }
         const it = g(1, 2, 3);
         console.log(it.next().value, it.next().value);",
        "1 3\n",
    );
}

#[test]
fn arguments_in_an_async_function_survives_an_await() {
    assert_output(
        "async function f(){ await null; const n = arguments.length; await null; return [n, arguments[0]]; }
         const o = { async m(){ await 0; return arguments.length; } };
         f('x', 'y').then(v => console.log(v));
         o.m(1, 2, 3).then(v => console.log(v));",
        "3\n[ 2, x ]\n",
    );
}

#[test]
fn an_async_arrow_reads_the_enclosing_arguments() {
    assert_output(
        "function outer(){ const f = async () => { await null; return arguments[0]; }; return f(9); }
         outer('o').then(v => console.log(v));",
        "o\n",
    );
}
