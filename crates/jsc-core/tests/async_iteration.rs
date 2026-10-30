//! Async iteration (design/symbol-strings.md §5.8): `GetIterator(async)`,
//! the `%AsyncFromSyncIteratorPrototype%` wrapper over a sync iterator and
//! `AsyncIteratorClose`. The wrapper is only reachable through `for await`
//! (Y5-T2) and async `yield*` (Y5-T3), so these are written as such
//! programs.

mod common;

use common::assert_output;

#[test]
fn async_from_sync_next_resolves_awaited_values() {
    assert_output(
        "async function main(){
           const out = [];
           for await (const x of [Promise.resolve(1), 2]) out.push(x);
           console.log(out.join(','));
         }
         main();",
        "1,2\n",
    );
}

#[test]
fn a_rejected_value_closes_the_sync_iterator() {
    assert_output(
        "let returns = 0;
         const it = { [Symbol.iterator]() {
           return {
             next() { return { value: Promise.reject('boom'), done: false }; },
             return() { returns++; return {}; },
           };
         } };
         async function main(){
           try { for await (const x of it) console.log('body', x); }
           catch (e) { console.log('caught', e, returns); }
         }
         main();",
        "caught boom 1\n",
    );
}

#[test]
fn a_sync_iterator_without_throw_is_closed_then_rejects() {
    assert_output(
        "let closed = 0;
         const sync = { [Symbol.iterator]() {
           return {
             next() { return { value: 1, done: false }; },
             return() { closed++; return {}; },
           };
         } };
         async function* g(){ yield* sync; }
         const it = g();
         it.next()
           .then(r => { console.log(r.value, r.done); return it.throw(new Error('x')); })
           .then(() => console.log('fulfilled'), e => console.log(e instanceof TypeError, closed));",
        "1 false\ntrue 1\n",
    );
}

#[test]
fn a_sync_iterator_without_return_completes_a_break() {
    assert_output(
        "const it = { [Symbol.iterator]() {
           let i = 0;
           return { next() { return { value: i++, done: false }; } };
         } };
         async function main(){
           for await (const x of it) { console.log(x); if (x === 1) break; }
           console.log('done');
         }
         main();",
        "0\n1\ndone\n",
    );
}

#[test]
fn symbol_async_iterator_is_preferred_over_symbol_iterator() {
    assert_output(
        "const both = {
           [Symbol.iterator]() { throw new Error('sync used'); },
           [Symbol.asyncIterator]() {
             let i = 0;
             return { next() { i++; return Promise.resolve({ value: i, done: i > 2 }); } };
           },
         };
         async function main(){
           const out = [];
           for await (const x of both) out.push(x);
           console.log(out.join(','));
         }
         main();",
        "1,2\n",
    );
}

#[test]
fn a_non_iterable_throws_a_type_error() {
    assert_output(
        "async function main(){
           for (const v of [5, {}]) {
             try { for await (const x of v) console.log(x); }
             catch (e) { console.log(e instanceof TypeError, e.message); }
           }
         }
         main();",
        "true 5 is not async iterable\ntrue object is not async iterable\n",
    );
}

// for await

#[test]
fn for_await_over_an_async_generator_yields_values_in_order() {
    assert_output(
        "async function* g(){ yield 1; yield Promise.resolve(2); yield 3; }
         async function main(){
           const out = [];
           for await (const x of g()) out.push(x);
           console.log(out.join(','));
         }
         main();",
        "1,2,3\n",
    );
}

#[test]
fn for_await_over_an_array_of_promises_awaits_each_value() {
    assert_output(
        "async function main(){
           for await (const x of [Promise.resolve('a'), 'b', new Promise(r => r('c'))]) console.log(x);
         }
         main();",
        "a\nb\nc\n",
    );
}

#[test]
fn for_await_over_a_user_async_iterator() {
    assert_output(
        "const src = { [Symbol.asyncIterator]() {
           let i = 0;
           return { next() { i++; return Promise.resolve({ value: i * 10, done: i > 3 }); } };
         } };
         async function main(){
           let sum = 0;
           for await (const x of src) sum += x;
           console.log(sum);
         }
         main();",
        "60\n",
    );
}

#[test]
fn break_awaits_return_before_the_code_after_the_loop() {
    assert_output(
        "const src = { [Symbol.asyncIterator]() {
           return {
             next() { return Promise.resolve({ value: 1, done: false }); },
             return() {
               return new Promise(r => r(1)).then(() => { console.log('return'); return {}; });
             },
           };
         } };
         async function main(){
           for await (const x of src) { console.log('body', x); break; }
           console.log('after');
         }
         main();",
        "body 1\nreturn\nafter\n",
    );
}

#[test]
fn break_rejects_when_return_yields_a_non_object() {
    assert_output(
        "const src = { [Symbol.asyncIterator]() {
           return {
             next() { return Promise.resolve({ value: 1, done: false }); },
             return() { return Promise.resolve(5); },
           };
         } };
         async function main(){
           for await (const x of src) break;
           console.log('unreachable');
         }
         main().catch(e => console.log(e instanceof TypeError));",
        "true\n",
    );
}

#[test]
fn a_body_throw_closes_and_propagates_the_original_error() {
    assert_output(
        "let returns = 0;
         const src = { [Symbol.asyncIterator]() {
           return {
             next() { return Promise.resolve({ value: 1, done: false }); },
             return() { returns++; throw new Error('from return'); },
           };
         } };
         async function main(){
           for await (const x of src) throw new Error('from body');
         }
         main().catch(e => console.log(e.message, returns));",
        "from body 1\n",
    );
}

#[test]
fn a_non_object_step_result_rejects_without_calling_return() {
    assert_output(
        "let returns = 0;
         const src = { [Symbol.asyncIterator]() {
           return {
             next() { return Promise.resolve(42); },
             return() { returns++; return {}; },
           };
         } };
         async function main(){
           for await (const x of src) console.log('body');
         }
         main().catch(e => console.log(e instanceof TypeError, returns));",
        "true 0\n",
    );
}

#[test]
fn for_await_at_the_top_level() {
    assert_output(
        "async function* g(){ yield 'x'; yield 'y'; }
         for await (const v of g()) console.log(v);
         console.log('end');",
        "x\ny\nend\n",
    );
}

#[test]
fn for_await_binds_destructuring_and_assignment_heads() {
    assert_output(
        "async function* g(){ yield [1, 2]; yield [3, 4]; }
         async function main(){
           for await (const [a, b] of g()) console.log(a + b);
           const o = {};
           let p, q;
           for await (o.x of ['m', 'n']) console.log(o.x);
           for await ([p, q] of g()) console.log(p * q);
         }
         main();",
        "3\n7\nm\nn\n2\n12\n",
    );
}

#[test]
fn continue_keeps_iterating() {
    assert_output(
        "async function main(){
           const out = [];
           for await (const x of [1, 2, 3, 4]) {
             if (x % 2) continue;
             out.push(x);
           }
           console.log(out.join(','));
         }
         main();",
        "2,4\n",
    );
}

#[test]
fn continue_outer_closes_the_inner_iterator() {
    assert_output(
        "function src(tag){ return { [Symbol.asyncIterator]() {
           let i = 0;
           return {
             next() { i++; return Promise.resolve({ value: i, done: i > 3 }); },
             return() { console.log('close', tag); return Promise.resolve({}); },
           };
         } }; }
         async function main(){
           outer: for (const k of ['a', 'b']) {
             for await (const x of src(k)) {
               console.log(k, x);
               continue outer;
             }
           }
           console.log('done');
         }
         main();",
        "a 1\nclose a\nb 1\nclose b\ndone\n",
    );
}

#[test]
fn for_await_inside_an_async_generator_that_yields() {
    assert_output(
        "async function* inner(){ yield 1; yield 2; }
         async function* outer(){
           for await (const x of inner()) yield x * 10;
           yield 'end';
         }
         async function main(){
           const out = [];
           for await (const v of outer()) out.push(v);
           console.log(out.join(','));
         }
         main();",
        "10,20,end\n",
    );
}
