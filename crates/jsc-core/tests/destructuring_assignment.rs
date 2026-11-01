//! Integration tests: destructuring assignment in any expression position
//! (DESIGN.md §7.3) — nested patterns, defaults, holes, rest and member
//! targets, `IteratorClose` on an abrupt or early-finished pattern, and
//! generator / async bodies. The expression's value is the RHS.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string};

/// A user iterable logging `iter`/`next`/`return`, yielding 1, 2, 3.
const COUNTER: &str = "const log = [];
     const it = {
       [Symbol.iterator]() { log.push('iter'); return this },
       i: 0,
       next() { log.push('next'); this.i++; return { value: this.i, done: this.i > 3 } },
       return() { log.push('return'); return {} },
     };";

#[test]
fn the_value_is_the_rhs() {
    assert_string(
        "let a, b; const r = [a, b] = [1, 2]; JSON.stringify([r, a, b])",
        "[[1,2],1,2]",
    );
    assert_bool("let a; const src = { a: 1 }; ({ a } = src) === src", true);
    assert_string("let a, x; x = [a] = 'hi'; x + a", "hih");
    assert_number("let a; ({ a } = { a: 5 }).a", 5.0);
    assert_number("let a; [a] = [1]; a", 1.0);
}

#[test]
fn works_as_an_argument_and_in_operator_operands() {
    assert_number(
        "let a; function f(v) { return v.k + a } f(({ a } = { a: 2, k: 3 }))",
        5.0,
    );
    assert_number("let a, b; ([a] = [1], [b] = [2]); a + b", 3.0);
    assert_number(
        "let a, b; const c = [3, 4]; true ? [a, b] = c : 0; a + b",
        7.0,
    );
    assert_number("let a = 0; false ? [a] = [9] : 0; a", 0.0);
    assert_number("let a = 1; null ?? ([a] = [5]); a", 5.0);
    assert_number("let a = 1; 1 || ([a] = [5]); a", 1.0);
    assert_number(
        "let a, b; const n = ([a, b] = [1, 2]).length + a + b; n",
        5.0,
    );
}

#[test]
fn swaps_in_a_for_update_and_assigns_in_a_loop_test() {
    assert_string(
        "let a = 0, b = 1; const out = [];
         for (let i = 0; i < 5; i++, [a, b] = [b, a + b]) out.push(a);
         out.join(',')",
        "0,1,1,2,3",
    );
    assert_number(
        "let a, n = 0, i = 0; const xs = [[1], [2], []];
         while (([a] = xs[i++]) && a !== undefined) n += a;
         n",
        3.0,
    );
}

#[test]
fn works_as_an_arrow_body() {
    assert_string(
        "let a, b; const f = (o) => ({ a, b } = o); const r = f({ a: 1, b: 2 });
         JSON.stringify([a, b, r.a])",
        "[1,2,1]",
    );
    assert_number("let a; const g = (x) => [a] = x; g([7]); a", 7.0);
}

#[test]
fn nested_patterns_defaults_holes_rest_and_members() {
    assert_string(
        "let a, b, c; const o = {};
         const r = [a, [b, c = 3], , ...o.rest] = [1, [2], 9, 4, 5];
         JSON.stringify([a, b, c, o.rest, r.length])",
        "[1,2,3,[4,5],5]",
    );
    assert_string(
        "let x, y, rest;
         const v = ({ a: x, b: { c: y = 'd' }, ...rest } = { a: 1, b: {}, z: 2 });
         JSON.stringify([x, y, rest, v.z])",
        "[1,\"d\",{\"z\":2},2]",
    );
}

#[test]
fn closes_the_iterator_when_the_pattern_finishes_first() {
    assert_string(
        &format!("{COUNTER} let a; const v = ([a] = it); log.push(v === it, a); log.join(',')"),
        "iter,next,return,true,1",
    );
}

#[test]
fn closes_the_iterator_when_a_target_throws() {
    assert_string(
        &format!(
            "{COUNTER} const o = {{ set p(v) {{ throw 'boom' }} }}; let x;
             try {{ x = [o.p] = it }} catch (e) {{ log.push(e) }}
             log.push(x); log.join(',')"
        ),
        "iter,next,return,boom,",
    );
}

#[test]
fn works_in_a_generator() {
    assert_number(
        "function* g() { let a, b; const r = [a, b] = yield 1; yield a + b; return r.length }
         const it = g(); it.next();
         const s = it.next([3, 4]).value; const t = it.next().value;
         s * 10 + t",
        72.0,
    );
    // A `yield` inside the pattern (a default).
    assert_string(
        "function* g() { let a; const v = ([a = yield 'd'] = []); return a + v.length }
         const it = g(); it.next().value + ',' + it.next(5).value",
        "d,5",
    );
    // A `yield` in an earlier operand of the same expression.
    assert_number(
        "function f(x, y) { return x + y[0] }
         function* g() { let a; return f(yield 1, [a] = [2]) + a }
         const it = g(); it.next(); it.next(10).value",
        14.0,
    );
}

#[test]
fn works_in_an_async_function() {
    assert_output(
        "async function f() {
           let a, b; const r = [a, b] = await Promise.resolve([1, 2]);
           return a + b + r.length;
         }
         f().then(v => console.log(v));",
        "5\n",
    );
}

#[test]
fn private_member_targets() {
    // A private field as a nested pattern target, a `for-of` / `for-in`
    // assignment head, and a pattern default; a missing brand throws.
    assert_string(
        "class C { #a; #b; #k = '';
           run() {
             const r = [[this.#a], { b: this.#b = 7 }] = [[1], {}];
             for (this.#a of [2, 3]) {}
             for (this.#k in { p: 1, q: 2 }) {}
             return [this.#a, this.#b, this.#k, r.length].join() } }
         new C().run()",
        "3,7,q,2",
    );
    assert_string(
        "class C { #a; static f(o) { [o.#a] = [1] } }
         try { C.f({}) } catch (e) { e instanceof TypeError ? 'TypeError' : 'other' }",
        "TypeError",
    );
}
