//! Integration tests: the `Promise` combinators `all` / `allSettled` / `any`
//! / `race` over the iteration protocol, `Promise.resolve` / `reject` with
//! `this` as the constructor, and `Math.sumPrecise`
//! (`design/symbol-strings.md` §5.7, Y3-T4).

mod common;
use common::{assert_bool, assert_nan, assert_number, assert_output, assert_string};

// --- Promise.all ------------------------------------------------------------------

#[test]
fn all_over_arrays_sets_and_generators() {
    assert_output(
        "Promise.all([1, Promise.resolve(2), new Promise((r) => r(3))])
           .then((v) => console.log(v.join(',')))",
        "1,2,3\n",
    );
    assert_output(
        "Promise.all(new Set([1, Promise.resolve(2)])).then((v) => console.log(v.join(',')))",
        "1,2\n",
    );
    assert_output(
        "function* g() { yield 1; yield Promise.resolve(2); yield 3; }
         Promise.all(g()).then((v) => console.log(v.join(',')))",
        "1,2,3\n",
    );
    assert_output(
        "Promise.all([]).then((v) => console.log(Array.isArray(v) + ' ' + v.length))",
        "true 0\n",
    );
}

#[test]
fn all_keeps_index_order_whatever_the_settlement_order() {
    assert_output(
        "let r1; const p1 = new Promise((r) => { r1 = r; });
         Promise.all([p1, 2, Promise.resolve(3)]).then((v) => console.log(v.join(',')));
         Promise.resolve().then(() => r1(1));",
        "1,2,3\n",
    );
}

#[test]
fn all_rejects_with_the_first_rejection() {
    assert_output(
        "Promise.all([1, Promise.reject('e1'), Promise.reject('e2')])
           .then(() => console.log('no'), (e) => console.log(e))",
        "e1\n",
    );
}

#[test]
fn all_awaited_at_top_level() {
    assert_output(
        "const v = await Promise.all([Promise.resolve('a'), 'b']); console.log(v.join(''))",
        "ab\n",
    );
}

// --- Promise.allSettled -----------------------------------------------------------

#[test]
fn all_settled_reports_every_outcome() {
    assert_output(
        "Promise.allSettled([1, Promise.reject(2), Promise.resolve(3)])
           .then((r) => console.log(JSON.stringify(r)))",
        "[{\"status\":\"fulfilled\",\"value\":1},{\"status\":\"rejected\",\"reason\":2},{\"status\":\"fulfilled\",\"value\":3}]\n",
    );
    assert_output(
        "function* g() { yield Promise.reject('x'); }
         Promise.allSettled(g()).then((r) => console.log(r[0].status + ' ' + r[0].reason))",
        "rejected x\n",
    );
}

// --- Promise.any --------------------------------------------------------------------

#[test]
fn any_takes_the_first_fulfillment() {
    assert_output(
        "Promise.any([Promise.reject(1), Promise.resolve(2), 3]).then((v) => console.log(v))",
        "2\n",
    );
    assert_output(
        "Promise.any(new Set([Promise.reject(1), 'ok'])).then((v) => console.log(v))",
        "ok\n",
    );
}

#[test]
fn any_of_all_rejected_is_an_aggregate_error() {
    assert_output(
        "Promise.any([Promise.reject(1), Promise.reject(2)]).catch((e) =>
           console.log((e instanceof AggregateError) + ' ' + e.errors.join(',') + ' ' +
             Object.getOwnPropertyDescriptor(e, 'errors').enumerable))",
        "true 1,2 false\n",
    );
    assert_output(
        "Promise.any([]).catch((e) =>
           console.log((e instanceof AggregateError) + ' ' + e.errors.length))",
        "true 0\n",
    );
}

// --- Promise.race -------------------------------------------------------------------

#[test]
fn race_settles_with_the_first_settlement() {
    assert_output(
        "Promise.race([new Promise(() => {}), Promise.resolve(3)]).then((v) => console.log(v))",
        "3\n",
    );
    assert_output(
        "function* g() { yield Promise.reject('r'); yield new Promise(() => {}); }
         Promise.race(g()).catch((e) => console.log(e))",
        "r\n",
    );
}

// --- abrupt completions -------------------------------------------------------------

#[test]
fn a_throwing_next_rejects_without_closing() {
    assert_output(
        "let closed = false;
         const it = { [Symbol.iterator]() { return {
           next() { throw new Error('boom'); },
           return() { closed = true; return {}; },
         }; } };
         Promise.all(it).catch((e) => console.log(e.message + ' ' + closed))",
        "boom false\n",
    );
}

#[test]
fn a_throwing_resolve_call_closes_the_iterator() {
    assert_output(
        "let closed = false;
         const it = { [Symbol.iterator]() { return {
           next() { return { value: 1, done: false }; },
           return() { closed = true; return {}; },
         }; } };
         Promise.resolve = function () { throw new Error('r'); };
         Promise.all(it).catch((e) => console.log(e.message + ' ' + closed))",
        "r true\n",
    );
}

#[test]
fn a_throwing_resolve_lookup_rejects_before_iterating() {
    // `GetPromiseResolve` runs before `GetIterator`: the iterator is never
    // opened, so there is nothing to close.
    assert_output(
        "let opened = false;
         const it = { [Symbol.iterator]() { opened = true; return [][Symbol.iterator](); } };
         Object.defineProperty(Promise, 'resolve', { get() { throw new Error('g'); } });
         Promise.race(it).catch((e) => console.log(e.message + ' ' + opened))",
        "g false\n",
    );
    assert_output(
        "Promise.resolve = 1;
         Promise.all([]).catch((e) => console.log(e instanceof TypeError))",
        "true\n",
    );
}

#[test]
fn a_non_iterable_rejects() {
    assert_output(
        "Promise.all(5).catch((e) => console.log(e instanceof TypeError))",
        "true\n",
    );
}

// --- `this` as the constructor ------------------------------------------------------

#[test]
fn all_uses_this_as_the_constructor() {
    assert_bool(
        "class P extends Promise {}
         P.all([1]) instanceof P && P.race([1]) instanceof P &&
           P.allSettled([]) instanceof P && P.any([1]) instanceof P",
        true,
    );
    // A non-promise constructor with a synchronous `resolve` / `then`.
    assert_output(
        "function C(executor) {
           executor((v) => console.log('resolved ' + v.join(',')), () => {});
         }
         C.resolve = function (v) { return { then(f) { f(v * 10); } }; };
         Promise.all.call(C, [1, 2]);",
        "resolved 10,20\n",
    );
    assert_bool(
        "try { Promise.all.call({}, []); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Promise.any.call(1, []); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

#[test]
fn combinator_function_properties() {
    assert_string(
        "[Promise.all, Promise.allSettled, Promise.any, Promise.race]
           .map((f) => f.name + f.length).join(',')",
        "all1,allSettled1,any1,race1",
    );
}

// --- Promise.resolve / Promise.reject -------------------------------------------------

#[test]
fn resolve_and_reject_construct_from_this() {
    assert_bool("class P extends Promise {} P.resolve(1) instanceof P", true);
    assert_bool(
        "class P extends Promise {} const p = P.reject(1); p.catch(() => {}); p instanceof P",
        true,
    );
    assert_bool(
        "class P extends Promise {} const p = new P((r) => r(1)); P.resolve(p) === p",
        true,
    );
    // `p.constructor` is `P`, not `%Promise%`: a fresh promise adopts it.
    assert_bool(
        "class P extends Promise {} const p = new P((r) => r(1));
         const q = Promise.resolve(p); q !== p && !(q instanceof P)",
        true,
    );
    assert_output(
        "class P extends Promise {} Promise.resolve(new P((r) => r(7))).then((v) => console.log(v))",
        "7\n",
    );
    assert_bool(
        "const p = Promise.resolve(1); Promise.resolve(p) === p",
        true,
    );
}

#[test]
fn resolve_and_reject_reject_a_non_constructor_this() {
    assert_bool(
        "try { Promise.resolve.call(1); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Promise.reject.call(undefined, 1); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Promise.resolve.call({}, 1); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

// --- one-argument programs ---------------------------------------------------------
//
// The widest call in each of these sources takes one argument (`K = 1`), so the
// executor's `reject` and a two-handler `then` overflow the positional slots.

#[test]
fn combinators_in_one_argument_programs() {
    assert_output(
        "Promise.all([1, 2]).then((v) => console.log(v.length));",
        "2\n",
    );
    assert_output("Promise.race([1]).then((v) => console.log(v));", "1\n");
    assert_output(
        "Promise.allSettled([1]).then((v) => console.log(v.length));",
        "1\n",
    );
    assert_output("Promise.any([1]).then((v) => console.log(v));", "1\n");
    assert_output("Promise.all([]).then((v) => console.log(v.length));", "0\n");
}

#[test]
fn capability_executor_gets_both_settlers_in_one_argument_programs() {
    assert_output(
        "new Promise(function () { console.log(typeof arguments[1]); });",
        "function\n",
    );
    assert_output(
        "class P extends Promise {} P.resolve(1).then((v) => console.log(v));",
        "1\n",
    );
    assert_output(
        "class P extends Promise {} P.reject(2).catch((v) => console.log(v));",
        "2\n",
    );
    assert_output(
        "class P extends Promise {} P.reject(3).finally(() => console.log(0)).catch((v) => console.log(v));",
        "0\n3\n",
    );
    assert_output(
        "class P extends Promise {} P.all([P.resolve(4)]).then((v) => console.log(v[0]));",
        "4\n",
    );
    assert_output(
        "class P extends Promise {} P.race([P.reject(5)]).catch((v) => console.log(v));",
        "5\n",
    );
}

// --- Math.sumPrecise ----------------------------------------------------------------

#[test]
fn sum_precise_is_exact() {
    assert_number("Math.sumPrecise([0.1, 0.2, 0.3])", 0.6);
    assert_number("Math.sumPrecise([1e20, 0.1, -1e20])", 0.1);
    assert_number("Math.sumPrecise([1, 1e100, 1, -1e100])", 2.0);
    // An exact tie rounds to even.
    assert_number("Math.sumPrecise([-0.1, -0.2])", -0.300_000_000_000_000_04);
    // No intermediate overflow.
    assert_number("Math.sumPrecise([1e308, 1e308, -1e308])", 1e308);
    assert_number(
        "Math.sumPrecise([Number.MAX_VALUE, Number.MAX_VALUE])",
        f64::INFINITY,
    );
    assert_number("Math.sumPrecise([5e-324, 5e-324])", 1e-323);
    assert_number("Math.sumPrecise(new Set([1, 2]))", 3.0);
    assert_number(
        "function* g() { yield 0.5; yield 0.25; } Math.sumPrecise(g())",
        0.75,
    );
}

#[test]
fn sum_precise_special_values() {
    assert_bool("Object.is(Math.sumPrecise([]), -0)", true);
    assert_bool("Object.is(Math.sumPrecise([-0, -0]), -0)", true);
    assert_bool("Object.is(Math.sumPrecise([-0, 0]), 0)", true);
    assert_bool("Object.is(Math.sumPrecise([1, -1]), 0)", true);
    assert_nan("Math.sumPrecise([Infinity, -Infinity])");
    assert_nan("Math.sumPrecise([NaN, 1])");
    assert_number("Math.sumPrecise([Infinity, 1, 1e308])", f64::INFINITY);
    assert_number("Math.sumPrecise([-Infinity, -1])", f64::NEG_INFINITY);
    assert_string(
        "Math.sumPrecise.name + Math.sumPrecise.length",
        "sumPrecise1",
    );
}

#[test]
fn sum_precise_rejects_non_numbers_and_closes() {
    assert_bool(
        "try { Math.sumPrecise([1, '2']); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "let closed = false;
         const it = { [Symbol.iterator]() { let i = 0; return {
           next() { return { value: i++ ? 'x' : 1, done: false }; },
           return() { closed = true; return {}; },
         }; } };
         try { Math.sumPrecise(it); false } catch (e) { e instanceof TypeError && closed }",
        true,
    );
    assert_bool(
        "try { Math.sumPrecise(5); false } catch (e) { e instanceof TypeError }",
        true,
    );
}
