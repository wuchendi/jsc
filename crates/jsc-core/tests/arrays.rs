//! Integration tests: array methods (push/pop, higher-order, indexOf/join/slice).
#![allow(clippy::expect_used)]

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_same_under_gc_stress, assert_string, eval_text,
};

// --- array growth: push / pop -------------------------------------------------

/// `push` grows the array and the SAME binding observes the new `.length`.
#[test]
fn array_push_grows_length() {
    assert_number("let a=[1,2]; a.push(3); a.length", 3.0);
}

/// `push` appends the value at the new last index, visible through the binding.
#[test]
fn array_push_appends_element() {
    assert_number("let a=[1,2]; a.push(3); a[2]", 3.0);
}

/// `pop` returns the removed last element.
#[test]
fn array_pop_returns_last() {
    assert_number("let a=[1,2,3]; a.pop()", 3.0);
}

/// After `pop`, the binding's `.length` shrinks by one.
#[test]
fn array_pop_shrinks_length() {
    assert_number("let a=[1,2,3]; a.pop(); a.length", 2.0);
}

/// `push` onto an empty literal array stores and reads back the element.
#[test]
fn array_push_onto_empty() {
    assert_number("let a=[]; a.push(7); a[0]", 7.0);
    assert_number("let a=[]; a.push(7); a.length", 1.0);
}

/// `push` returns the NEW length (JS semantics).
#[test]
fn array_push_returns_new_length() {
    assert_number("let a=[1]; a.push(2)", 2.0);
}

/// `pop` on an empty array yields `undefined`.
#[test]
fn array_pop_empty_is_undefined() {
    let result = eval_text("let a=[]; a.pop()");
    assert_eq!(result, "undefined");
}

/// Existing elements survive a grow: all indices read back correctly after `push`.
#[test]
fn array_push_preserves_existing_elements() {
    assert_number("let a=[1,2]; a.push(3); a[0] + a[1] + a[2]", 6.0);
    // Repeated pushes keep growing the same binding.
    assert_number("let a=[1,2]; a.push(3); a.push(4); a[3]", 4.0);
}

/// A grown array decodes as a full array with the appended element.
#[test]
fn array_push_decodes_grown_array() {
    let result = eval_text("let a=[1,2]; a.push(3); a");
    assert_eq!(result, "[ 1, 2, 3 ]");
}

/// `pop` mutates the block in place; the same binding decodes one element shorter.
#[test]
fn array_pop_decodes_shrunk_array() {
    let result = eval_text("let a=[10,20,30]; a.pop(); a");
    assert_eq!(result, "[ 10, 20 ]");
}

/// String elements round-trip through a grow (element bytes copied, not reparsed).
#[test]
fn array_push_preserves_string_elements() {
    assert_eq!(eval_text("let a=[\"x\"]; a.push(\"y\"); a[1]"), "y");
}

/// `pop` then `push` on the same binding tracks the length through both edits.
#[test]
fn array_pop_then_push_length() {
    assert_number("let a=[1,2,3]; a.pop(); a.push(9); a.length", 3.0);
}

// --- array higher-order methods (map/filter/reduce/forEach) --------------------

/// Each acceptance bullet: `map`/`filter`/`reduce`/`forEach` over a literal array.
#[test]
fn array_hof() {
    assert_number("[1,2,3].map(x => x*2)[2]", 6.0);
    assert_number("[1,2,3,4].filter(x => x > 2).length", 2.0);
    assert_number("[1,2,3,4].reduce((a,b) => a+b, 0)", 10.0);
    assert_number("let s=0; [1,2,3,4].forEach(x => { s = s + x }); s", 10.0);
    assert_number("[1,2,3].map(x => x+1).length", 3.0);
}

/// `map` preserves length and produces the transformed elements in order.
#[test]
fn array_map_elements_and_length() {
    assert_number("[1,2,3].map(x => x+1)[0]", 2.0);
    assert_number("[1,2,3].map(x => x+1)[1]", 3.0);
    assert_number("[1,2,3].map(x => x+1)[2]", 4.0);
    assert_number("[1,2,3].map(x => x*10).length", 3.0);
}

/// The callback's second argument is the element index (a 1-param arrow ignores
/// it; a 2-param arrow observes it).
#[test]
fn array_map_index_argument() {
    assert_number("[10,20,30].map((x,i) => x+i)[0]", 10.0);
    assert_number("[10,20,30].map((x,i) => x+i)[2]", 32.0);
}

/// `filter` keeps only JS-truthy callback results and reports the new length.
#[test]
fn array_filter_length_and_elements() {
    assert_number("[1,2,3,4].filter(x => x > 2).length", 2.0);
    assert_number("[1,2,3,4].filter(x => x > 2)[0]", 3.0);
    assert_number("[1,2,3,4].filter(x => x > 2)[1]", 4.0);
    assert_number("[1,2,3].filter(x => x > 10).length", 0.0);
}

/// `reduce` folds from the supplied initial accumulator; the index is available
/// as the callback's third argument.
#[test]
fn array_reduce_fold_and_index() {
    assert_number("[1,2,3,4].reduce((a,b) => a+b, 0)", 10.0);
    assert_number("[1,2,3,4].reduce((a,b) => a+b, 100)", 110.0);
    assert_number("[1,2,3].reduce((acc,x,i) => acc + x*i, 0)", 8.0);
    assert_number("[].reduce((a,b) => a+b, 42)", 42.0);
}

/// `forEach` runs the callback for side effects and evaluates to `undefined`;
/// a closure-capturing callback persists its outer-binding mutation via cells.
#[test]
fn array_foreach_closure_mutation() {
    assert_number("let s=0; [1,2,3,4].forEach(x => { s = s + x }); s", 10.0);
    assert_number(
        "let total=0; let a=[5,10,15]; a.forEach(x => { total = total + x }); total",
        30.0,
    );
    assert_number("let n=0; [1,2,3].forEach((x,i) => { n = n + i }); n", 3.0);
    assert_eq!(eval_text("[1,2,3].forEach(x => x)"), "undefined");
}

/// The receiver may be a bound array variable (not only a literal); it is read,
/// never rebound.
#[test]
fn array_hof_over_bound_variable() {
    assert_number("let a = [1,2,3]; a.map(x => x*x)[2]", 9.0);
    assert_number("let a = [1,2,3]; a.map(x => x*x); a.length", 3.0);
    assert_number("let a = [2,4,6]; a.reduce((s,x) => s+x, 0)", 12.0);
}

/// HOF calls chain: the result array of one feeds the next as a receiver.
#[test]
fn array_hof_chaining() {
    assert_number("[1,2,3,4].map(x => x*2).filter(x => x > 4).length", 2.0);
    assert_number("[1,2,3,4].filter(x => x > 1).map(x => x+1)[0]", 3.0);
}

/// A callback that captures an outer binding (closure) works in `map`/`reduce`
/// too, not only `forEach` — the env-last ABI passes its environment unchanged.
#[test]
fn array_hof_closure_callback() {
    assert_number("let k = 10; [1,2,3].map(x => x + k)[2]", 13.0);
    assert_number(
        "let base = 100; [1,2,3].reduce((a,x) => a + x + base, 0)",
        306.0,
    );
}

/// The shared funcref signature is widened so a HOF callback invocation never
/// overflows the table type, even when the program's only function is the 1-param
/// callback itself (no `call_indirect` type mismatch trap).
#[test]
fn array_hof_arity_no_trap() {
    assert_number("[1,2,3].map(x => x*2)[2]", 6.0);
    assert_number("[5,6,7].forEach(x => x); [5,6,7].length", 3.0);
}

/// HOF method calls do not disturb ordinary element indexing / `.length`.
#[test]
fn array_indexing_still_works_alongside_hof() {
    assert_number("let a=[7,8,9]; a[1]", 8.0);
    assert_number("let a=[7,8,9]; a.length", 3.0);
    assert_number("[1,2,3].map(x => x)[1] + [4,5,6][0]", 6.0);
}

/// Builtin arities are runtime behaviour: a missing callback throws a
/// `TypeError`, `reduce` without an initial value folds from the first
/// element, and `filter`'s second argument is the callback's `this`.
#[test]
fn array_hof_arities_follow_the_spec() {
    assert_bool(
        "let ok = false; try { [1,2,3].map(); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
    assert_number("[1,2,3].reduce((a,b) => a+b)", 6.0);
    assert_number(
        "[1,2,3].filter(function (x) { return x > this.m; }, { m: 1 }).length",
        2.0,
    );
}

// --- non-callback array/string methods ---------------------------------------

/// Array `indexOf`/`includes`: `indexOf` yields the first strict-equal index (or
/// `-1`), `includes` is `indexOf(x) !== -1` as a boolean.
#[test]
fn array_index_of_and_includes() {
    assert_number("[10,20,30].indexOf(20)", 1.0);
    assert_number("[10,20,30].indexOf(99)", -1.0);
    assert_bool("[1,2,3].includes(2)", true);
    assert_bool("[1,2,3].includes(9)", false);
    // Strict equality: strings compare by content, and a type mismatch never matches.
    assert_number("[\"a\",\"b\",\"c\"].indexOf(\"b\")", 1.0);
    assert_number("[1,2,3].indexOf(\"2\")", -1.0);
}

/// `join(sep)` concatenates `String(element)` separated by `sep` (`","` when
/// omitted); an empty array is `""` and `null`/`undefined` elements render `""`.
#[test]
fn array_join() {
    assert_string("[1,2,3].join(\"-\")", "1-2-3");
    assert_string("[1,2,3].join()", "1,2,3");
    assert_string("[].join(\"-\")", "");
    assert_string("[\"a\",\"b\"].join(\"/\")", "a/b");
    assert_string("[1,null,3].join(\"-\")", "1--3");
}

/// Array `slice(a, b)` builds a NEW array of `[a, b)` with JS negative/clamp
/// semantics; `b` omitted slices to the end.
#[test]
fn array_slice() {
    assert_number("[1,2,3,4].slice(1,3).length", 2.0);
    assert_number("[1,2,3,4].slice(1,3)[0]", 2.0);
    assert_number("[1,2,3,4].slice(1,3)[1]", 3.0);
    assert_number("[1,2,3,4,5].slice(-2).length", 2.0);
    assert_number("[1,2,3,4,5].slice(-2)[0]", 4.0);
    assert_number("[1,2,3,4].slice(1).length", 3.0);
    assert_number("[1,2,3].slice(0,0).length", 0.0);
}

/// `%Array.prototype%` is an Array exotic object: `IsArray` is true, its own
/// `length` is 0 and tracks element writes, and an element written to it is
/// seen through the holes of every array, inline fast paths included.
#[test]
fn array_prototype_is_an_array() {
    assert_bool("Array.isArray(Array.prototype)", true);
    assert_number("Array.prototype.length", 0.0);
    assert_string(
        "Object.prototype.toString.call(Array.prototype)",
        "[object Array]",
    );
    assert_string(
        "JSON.stringify(Object.getOwnPropertyDescriptor(Array.prototype, 'length'))",
        "{\"value\":0,\"writable\":true,\"enumerable\":false,\"configurable\":false}",
    );
    assert_string(
        "Array.prototype.push(5); Array.prototype.length + ',' + Array.prototype[0]",
        "1,5",
    );
    assert_string("Array.prototype[1] = 'x'; [0, , 2][1]", "x");
    assert_string(
        "Array.prototype[1] = 'x'; [0, , 2].map(v => v).join()",
        "0,x,2",
    );
}

// --- length growth and re-densify (DESIGN.md §5.4) ---------------------------

/// The wasm instructions `src` executes (wasmtime fuel), as the opcount gate
/// measures them.
fn fuel(src: &str) -> u64 {
    let program = jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js).expect("parse");
    let ir = jsc_core::codegen::lower(&program).expect("lower");
    let wasm = jsc_core::emit::emit(&ir, jsc_core::emit::CompileOptions::default());
    let capture = jsc_core::run::Capture::new();
    let outcome = jsc_core::run::run(
        &wasm,
        jsc_core::run::Sinks::Capture(capture.clone()),
        Some(1 << 40),
    )
    .expect("program runs cleanly");
    assert_eq!(outcome.code, 0, "{}", capture.stderr());
    outcome.fuel
}

/// A pure `length` growth then a `fill` from 0 (the test262
/// `copyWithin/coerced-values-*-detached*` setup) writes every element and
/// lists every key.
#[test]
fn length_growth_then_fill_writes_every_element() {
    assert_string(
        "let a = []; a.length = 10000; a.fill(7, 0);
         let ok = true;
         for (let i = 0; i < 10000; i++) if (a[i] !== 7) ok = false;
         ok + ',' + a.length + ',' + Object.keys(a).length + ',' + a.reduce((s, v) => s + v, 0)",
        "true,10000,10000,70000",
    );
}

/// The fill after a pure `length` growth re-densifies the store, so fill plus
/// a full sequential read is linear: fuel(1e4) stays well under 2.5x
/// fuel(5e3) (index keys in the linear-scan table would be ~4x).
#[test]
fn length_growth_then_fill_is_linear() {
    let program = |n: u32| {
        format!(
            "const a = []; a.length = {n}; a.fill(7, 0);
             let s = 0; for (let i = 0; i < {n}; i++) s += a[i];
             console.log(s)"
        )
    };
    let half = fuel(&program(5_000));
    let full = fuel(&program(10_000));
    assert!(
        full * 2 < half * 5,
        "grown-then-filled array is not dense: fuel(5e3) = {half}, fuel(1e4) = {full}"
    );
}

/// The tail a pure `length` growth adds is holes: absent own properties, read
/// through the prototype chain.
#[test]
fn length_growth_tail_is_holes() {
    assert_string(
        "Array.prototype[5000] = 'p';
         let a = []; a.length = 10000;
         [a[5000], 5000 in a, a.hasOwnProperty(5000), a.hasOwnProperty(4999),
          Object.keys(a).length, a.length].join()",
        "p,false,false,false,0,10000",
    );
}

/// JSON, spread, `for-in`, search and rendering of a grown array see the
/// written prefix and holes for the tail.
#[test]
fn length_growth_observers() {
    assert_string(
        "let a = [1, 2]; a.length = 3000;
         let keys = []; for (const k in a) keys.push(k);
         [JSON.stringify(a).length, [...a].length, [...a][2] === undefined, keys.join(' '),
          a.indexOf(undefined), a.includes(undefined), a[1]].join()",
        "14995,3000,true,0 1,-1,true,2",
    );
    assert_eval(
        "let a = [1]; a.length = 5000; a",
        "[ 1, <4999 empty items> ]",
    );
}

/// A far element store on a small array still goes `SPARSE`: keys, length and
/// the gap stay correct.
#[test]
fn far_write_still_sparse() {
    assert_string(
        "let a = [1, 2]; a[100000] = 3;
         [a.length, Object.keys(a).join(' '), a[100000], a[50000] === undefined, 50000 in a].join()",
        "100001,0 1 100000,3,true,false",
    );
}

/// Contiguous stores from the store's end after a far write grow the store
/// back, folding the far element in when the store reaches it: values, keys
/// and key order stay correct throughout.
#[test]
fn contiguous_writes_after_sparsify() {
    assert_string(
        "let a = [0]; a[5000] = 'far';
         for (let i = 1; i < 3000; i++) a[i] = i;
         let k1 = Object.keys(a);
         let first = [a.length, k1.length, k1[2999], k1[3000], a[2999], a[5000]].join();
         for (let i = 3000; i < 6000; i++) if (i !== 5000) a[i] = i;
         let k2 = Object.keys(a);
         [first, a.length, k2.length, k2.every((k, i) => k === String(i)), a[5000], a[5999],
          4000 in a].join('|')",
        "5001,3001,2999,5000,2999,far|6000|6000|true|far|5999|true",
    );
    assert_string(
        "let b = []; b[3000] = 'x';
         for (let i = 0; i < 3000; i++) b[i] = i;
         b.push('y');
         [b.length, b[3000], b[3001], Object.keys(b).length, b.indexOf('x')].join()",
        "3002,x,y,3002,3000",
    );
}

/// A store that re-densifies a grown array still offers an absent element to
/// the prototype chain first: an inherited setter consumes it.
#[test]
fn regrow_consults_prototype_setter() {
    assert_string(
        "let log = '';
         Object.defineProperty(Array.prototype, '0', { set(v) { log += v; }, configurable: true });
         let a = []; a.length = 5000; a[0] = 1; a[1] = 2;
         [log, a.hasOwnProperty(0), a[1], a.length].join()",
        "1,false,2,5000",
    );
}

/// A `length` shrink after a growth drops the tail; later pushes append at the
/// new end.
#[test]
fn length_shrink_after_growth() {
    assert_string(
        "let a = [1, 2, 3]; a.length = 5000; a.length = 2; a.push(9);
         [a.length, a.join(), Object.keys(a).join()].join('|')",
        "3|1,2,9|0,1,2",
    );
    assert_string(
        "let a = []; a.length = 5000; a.fill(1); a.length = 10;
         [a.length, Object.keys(a).length, a[9], 10 in a].join()",
        "10,10,1,false",
    );
}

/// `delete` inside a grown tail: a hole before the fill, an absent element
/// after it.
#[test]
fn delete_in_grown_tail() {
    assert_string(
        "let a = [1]; a.length = 5000;
         [delete a[3000], a.length, Object.keys(a).join()].join()",
        "true,5000,0",
    );
    assert_string(
        "let a = []; a.length = 5000; a.fill(1); delete a[4000]; delete a[10];
         [a.length, 4000 in a, 10 in a, Object.keys(a).length, a[4000] === undefined].join()",
        "5000,false,false,4998,true",
    );
}

/// Growth + fill and the sparse -> dense fold survive a collection at every
/// allocation (the folded table elements move into the regrown store).
#[test]
fn length_growth_and_regrow_gc_stress() {
    assert_same_under_gc_stress(
        "let a = []; a.length = 5000; a.fill('s' + 1, 0);
         let b = [{ v: 1 }]; b[4000] = { v: 2 };
         for (let i = 1; i < 4000; i++) b[i] = { v: i };
         let s = 0; for (const o of b) s += o.v;
         console.log(a.length, a[4999], s, Object.keys(b).length)",
        "5000 s1 7998003 4001\n",
    );
}

// --- length write order (ArraySetLength) ----------------------------------------

/// `a.length = v` converts a non-number `v` twice (`ToUint32`, then
/// `ToNumber`), and a conversion that makes `length` read-only makes the
/// write throw the `TypeError` (and `Reflect.set` return `false`); an
/// already read-only `length` throws before any conversion; a mismatch is
/// the `RangeError`.
#[test]
fn length_write_conversion_order() {
    assert_string(
        "let a = [1, 2, 3];
         let hints = [];
         const len = { [Symbol.toPrimitive](hint) {
           hints.push(hint);
           Object.defineProperty(a, 'length', { writable: false });
           return 0;
         } };
         let r1;
         try { a.length = len; r1 = 'no throw'; } catch (e) { r1 = e.constructor.name; }
         const out = [r1, hints.join('/'), a.length];
         a = [1, 2, 3];
         hints = [];
         out.push(Reflect.set(a, 'length', len), hints.join('/'), a.length);
         hints = [];
         let r2;
         try { a.length = len; r2 = 'no throw'; } catch (e) { r2 = e.constructor.name; }
         out.push(r2, hints.length);
         const b = [1, 2];
         let n = 0;
         const odd = { valueOf() { n += 1; return n === 1 ? 1 : 2; } };
         let r3;
         try { b.length = odd; r3 = 'no throw'; } catch (e) { r3 = e.constructor.name; }
         const c = [1, 2, 3];
         c.length = { valueOf() { return 1; } };
         out.push(r3, n, b.length, c.length);
         out.join()",
        "TypeError,number/number,3,false,number/number,3,TypeError,0,RangeError,2,2,1",
    );
}
