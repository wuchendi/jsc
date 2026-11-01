//! Integration tests: array methods (push/pop, higher-order, indexOf/join/slice).
#![allow(clippy::expect_used)]

use jsc_core::run::{Value, compile_and_run, format_value};
mod common;
use common::{assert_bool, assert_number, assert_string};

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
    let result = compile_and_run("let a=[]; a.pop()").expect("pipeline should succeed");
    assert_eq!(result, Value::Undefined);
}

/// Existing elements survive a grow: all indices read back correctly after `push`.
#[test]
fn array_push_preserves_existing_elements() {
    assert_number("let a=[1,2]; a.push(3); a[0] + a[1] + a[2]", 6.0);
    // Repeated pushes keep growing the same binding.
    assert_number("let a=[1,2]; a.push(3); a.push(4); a[3]", 4.0);
}

/// A grown array decodes as a full [`Value::Array`] with the appended element.
#[test]
fn array_push_decodes_grown_array() {
    let result = compile_and_run("let a=[1,2]; a.push(3); a").expect("pipeline should succeed");
    assert_eq!(
        result,
        Value::Array(
            vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0),],
            Vec::new()
        )
    );
}

/// `pop` mutates the block in place; the same binding decodes one element shorter.
#[test]
fn array_pop_decodes_shrunk_array() {
    let result = compile_and_run("let a=[10,20,30]; a.pop(); a").expect("pipeline should succeed");
    assert_eq!(
        result,
        Value::Array(vec![Value::Number(10.0), Value::Number(20.0)], Vec::new())
    );
}

/// String elements round-trip through a grow (element bytes copied, not reparsed).
#[test]
fn array_push_preserves_string_elements() {
    assert_eq!(
        format_value(&compile_and_run("let a=[\"x\"]; a.push(\"y\"); a[1]").expect("runs")),
        "y"
    );
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
    assert_eq!(
        format_value(&compile_and_run("[1,2,3].forEach(x => x)").expect("runs")),
        "undefined"
    );
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
