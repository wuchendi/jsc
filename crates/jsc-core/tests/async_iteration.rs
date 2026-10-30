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
