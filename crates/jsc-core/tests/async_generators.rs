//! Async generators (`async function*`, `async *m(){}`): `next` / `return` /
//! `throw` return promises of `{ value, done }`, requests queue in order,
//! `await` works in the body and `yield v` awaits `v`.

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
fn next_returns_promises_of_iterator_results() {
    assert_output(
        "async function* g(){ const a = await Promise.resolve(1); yield a; yield await Promise.resolve(2); return 3; }
         const it = g();
         const p = it.next();
         console.log(p instanceof Promise);
         p.then(r => { console.log(JSON.stringify(r)); return it.next(); })
          .then(r => { console.log(JSON.stringify(r)); return it.next(); })
          .then(r => { console.log(JSON.stringify(r)); return it.next(); })
          .then(r => console.log(r.value, r.done));",
        "true\n{\"value\":1,\"done\":false}\n{\"value\":2,\"done\":false}\n{\"value\":3,\"done\":true}\nundefined true\n",
    );
}

#[test]
fn queued_next_calls_settle_in_request_order() {
    assert_output(
        "async function* g(){ yield 'a'; await null; yield 'b'; return 'c'; }
         const it = g();
         const out = [];
         const ps = [it.next(), it.next(), it.next(), it.next()];
         ps.forEach((p, i) => p.then(r => out.push(i + ':' + r.value + ':' + r.done)));
         Promise.resolve().then(() => null).then(() => null).then(() => null).then(() => null)
           .then(() => null).then(() => console.log(out.join(' ')));",
        "0:a:false 1:b:false 2:c:true 3:undefined:true\n",
    );
}

#[test]
fn the_body_runs_only_on_the_first_next() {
    assert_output(
        "const log = [];
         async function* g(){ log.push('start'); yield Promise.resolve('p'); log.push('after'); }
         const it = g();
         log.push('created');
         const p = it.next();
         log.push('called');
         p.then(r => { log.push(r.value, r.done); return it.next(); })
          .then(r => { log.push(r.value, r.done); console.log(log.join(' ')); });",
        "created start called p false after  true\n",
    );
}

#[test]
fn yield_awaits_a_promise_operand() {
    assert_output(
        "async function* g(){ const v = yield new Promise(r => r(41)); yield v + 1; }
         const it = g();
         it.next().then(r => { console.log(r.value); return it.next(1); })
          .then(r => console.log(r.value));",
        "41\n2\n",
    );
}

#[test]
fn a_rejected_yield_operand_throws_into_the_body() {
    assert_output(
        "async function* g(){ try { yield Promise.reject('bad'); } catch (e) { yield 'caught ' + e; } }
         g().next().then(r => console.log(r.value, r.done));",
        "caught bad false\n",
    );
}

#[test]
fn a_rejected_await_rejects_the_next_promise() {
    assert_output(
        "async function* g(){ await Promise.reject(new TypeError('no')); }
         const it = g();
         it.next().then(null, e => { console.log(e instanceof TypeError, e.message); return it.next(); })
          .then(r => console.log(r.value, r.done));",
        "true no\nundefined true\n",
    );
}

#[test]
fn throw_reaches_the_catch_around_the_suspended_yield() {
    assert_output(
        "async function* g(){ try { yield 1; } catch (e) { yield 'caught ' + e; } }
         const it = g();
         it.next().then(r => { console.log(r.value); return it.throw('boom'); })
          .then(r => { console.log(r.value, r.done); return it.next(); })
          .then(r => { console.log(r.value, r.done); return it.throw('again'); })
          .then(null, e => console.log('rejected', e));",
        "1\ncaught boom false\nundefined true\nrejected again\n",
    );
}

#[test]
fn return_awaits_its_value_and_completes() {
    assert_output(
        "async function* g(){ yield 1; yield 2; }
         const a = g();
         a.return(Promise.resolve(9)).then(r => { console.log('start', r.value, r.done); return a.next(); })
          .then(r => console.log('after', r.value, r.done));",
        "start 9 true\nafter undefined true\n",
    );
    assert_output(
        "async function* g(){ yield 1; yield 2; }
         const b = g();
         b.next().then(() => b.return(5)).then(r => { console.log('suspended', r.value, r.done); return b.next(); })
          .then(r => console.log('then', r.value, r.done));",
        "suspended 5 true\nthen undefined true\n",
    );
}

#[test]
fn return_of_a_rejected_promise_rejects() {
    assert_output(
        "async function* g(){ yield 1; }
         g().return(Promise.reject('r')).then(null, e => console.log('rejected', e));",
        "rejected r\n",
    );
}

#[test]
fn a_return_statement_awaits_its_operand() {
    assert_output(
        "async function* g(){ try { return Promise.reject('x'); } catch (e) { return 'caught ' + e; } }
         g().next().then(r => console.log(r.value, r.done));",
        "caught x true\n",
    );
}

#[test]
fn object_and_class_async_generator_methods_bind_this() {
    assert_output(
        "const o = { v: 1, async *m(x){ yield this.v + x; } };
         class C {
           constructor(){ this.k = 'K'; }
           async *items(){ yield this.k; yield await 2; }
           static async *s(){ yield this.name; }
         }
         o.m(1).next().then(r => console.log(r.value));
         const it = new C().items();
         it.next().then(r => { console.log(r.value); return it.next(); })
           .then(r => { console.log(r.value); return it.next(); })
           .then(r => console.log(r.done));
         C.s().next().then(r => console.log(r.value));
         console.log(o.m.name, C.prototype.items.name, C.s.length);",
        "m items 0\n2\nK\nC\n2\ntrue\n",
    );
}

#[test]
fn async_generator_expressions_and_arguments() {
    assert_output(
        "const g = async function* named(a, b = 2){ yield arguments.length; yield a + b; yield typeof named; };
         const it = g(1);
         const out = [];
         it.next().then(r => { out.push(r.value); return it.next(); })
           .then(r => { out.push(r.value); return it.next(); })
           .then(r => { out.push(r.value); console.log(out.join(' '), g.name); });",
        "1 3 function named\n",
    );
}

#[test]
fn a_parameter_binding_error_throws_from_the_call() {
    assert_output(
        "async function* g([a]){ yield a; }
         try { g(null); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
}

#[test]
fn async_generator_prototypes_mirror_generators() {
    assert_output(
        "async function* ag(){}
         const it = ag();
         console.log(typeof ag, typeof ag.prototype, ag.prototype.hasOwnProperty('constructor'));
         console.log(it instanceof ag, Object.keys(ag.prototype).length);
         ag.prototype.extra = 7;
         console.log(ag().extra, typeof it.next, it.next.length, it.next.name, it.return.name, it.throw.name);
         function* sg(){}
         console.log(it.next === sg().next);",
        "function object false\ntrue 0\n7 function 1 next return throw\nfalse\n",
    );
    assert_uncaught(
        "async function* ag(){} new ag();",
        "TypeError: ag is not a constructor",
    );
}

#[test]
fn async_generator_methods_reject_a_bad_receiver() {
    assert_output(
        "async function* ag(){}
         const next = ag().next;
         function* sg(){ yield 1; }
         next.call(sg()).then(null, e => console.log(e instanceof TypeError, e.message));
         next.call({}).then(null, e => console.log(e instanceof TypeError));",
        "true next method called on incompatible receiver\ntrue\n",
    );
}

#[test]
fn async_generators_are_not_sync_iterables() {
    assert_uncaught(
        "async function* ag(){ yield 1; } for (const x of ag()) {}",
        "TypeError: object is not iterable",
    );
    assert_uncaught(
        "async function* ag(){ yield 1; } [...ag()];",
        "TypeError: object is not iterable",
    );
    assert_output(
        "async function* ag(){ yield 1; }
         function* sg(){ yield 1; }
         try { sg().next.call(ag()); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
}

#[test]
fn a_reentrant_next_from_the_body_is_queued() {
    assert_output(
        "let it, p;
         async function* g(){ p = it.next(); yield 'first'; yield 'second'; }
         it = g();
         it.next().then(r => { console.log(r.value); return p; }).then(r => console.log(r.value, r.done));",
        "first\nsecond false\n",
    );
}

#[test]
fn yield_star_in_an_async_generator_is_a_compile_error() {
    assert_compile_error(
        "async function* g(){ yield* [1]; }",
        "`yield*` in an async generator is unsupported",
    );
}

#[test]
fn return_inside_try_finally_settles_after_the_finally() {
    assert_output(
        "async function* g(){ try { yield 1; } finally { console.log('cleanup'); } }
         const it = g();
         it.next().then(() => it.return(2)).then(r => console.log(r.value, r.done));",
        "cleanup\n2 true\n",
    );
    // The returned value is awaited at the `yield` first.
    assert_output(
        "async function* g(){ try { yield 1; } finally { console.log('cleanup'); } }
         const it = g();
         it.next().then(() => it.return(Promise.resolve(5))).then(r => console.log(r.value, r.done));",
        "cleanup\n5 true\n",
    );
}

#[test]
fn async_generator_prototype_inherits_async_iterator_prototype() {
    assert_output(
        "const AIP = Object.getPrototypeOf(Object.getPrototypeOf(async function*(){}.prototype));
         console.log(typeof AIP[Symbol.asyncIterator]);
         const o = {};
         console.log(AIP[Symbol.asyncIterator].call(o) === o);
         console.log(Object.getPrototypeOf(AIP) === Object.prototype);",
        "function\ntrue\ntrue\n",
    );
}
