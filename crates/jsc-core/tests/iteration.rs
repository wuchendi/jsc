//! Integration tests: `GetIterator` of the builtin iterables (their guarded
//! fast paths and the protocol, design/symbol-strings.md §5.3) — for-of,
//! `yield*`, spread (array literal and call arguments) and array
//! destructuring over arrays, strings, generators, `Map` / `Set` handles,
//! `String` wrappers and `arguments`, and the `<x> is not iterable`
//! `TypeError` of every value without a callable `@@iterator`.

mod common;
use common::{assert_rejected, assert_string, assert_uncaught};

/// The JSON text of the array `out` after running `body`.
fn collected(body: &str) -> String {
    format!("let out = []; {body}; JSON.stringify(out)")
}

// --- Map / Set ---------------------------------------------------------------

#[test]
fn for_of_walks_map_entries_and_set_values_in_insertion_order() {
    assert_string(
        &collected("const m = new Map([[1, 'a'], [2, 'b']]); for (const e of m) out.push(e)"),
        "[[1,\"a\"],[2,\"b\"]]",
    );
    assert_string(
        &collected(
            "const m = new Map([[1, 'a'], [2, 'b']]); for (const [k, v] of m) out.push(k, v)",
        ),
        "[1,\"a\",2,\"b\"]",
    );
    assert_string(
        &collected("const s = new Set([3, 1, 3, 2]); for (const v of s) out.push(v)"),
        "[3,1,2]",
    );
}

#[test]
fn for_of_observes_entries_added_during_iteration() {
    assert_string(
        &collected(
            "const s = new Set([1]); for (const v of s) { out.push(v); if (v < 3) s.add(v + 1) }",
        ),
        "[1,2,3]",
    );
    assert_string(
        &collected(
            "const m = new Map([['a', 1]]); \
             for (const [k, v] of m) { out.push(k); if (v < 2) m.set('b', v + 1) }",
        ),
        "[\"a\",\"b\"]",
    );
}

#[test]
fn for_of_skips_entries_deleted_before_they_are_reached() {
    assert_string(
        &collected("const s = new Set([1, 2, 3]); for (const v of s) { out.push(v); s.delete(3) }"),
        "[1,2]",
    );
}

#[test]
fn deleting_the_current_entry_visits_the_next_one() {
    // Deletion tombstones the entry in place (design/symbol-strings.md §5.5).
    assert_string(
        &collected("const s = new Set([1, 2, 3]); for (const v of s) { out.push(v); s.delete(v) }"),
        "[1,2,3]",
    );
}

#[test]
fn spread_of_map_and_set() {
    assert_string(
        "JSON.stringify([0, ...new Map([[1, 2], [3, 4]]), 9])",
        "[0,[1,2],[3,4],9]",
    );
    assert_string("JSON.stringify([...new Set(['x', 'y'])])", "[\"x\",\"y\"]");
    assert_string(
        "function f(a, b, c) { return [a, b, c] } JSON.stringify(f(...new Set([1, 2]), 3))",
        "[1,2,3]",
    );
    assert_string("JSON.stringify(Math.max(...new Set([4, 9, 2])))", "9");
}

#[test]
fn array_destructuring_of_map_and_set() {
    assert_string(
        "const [a, b] = new Set([5, 6]); JSON.stringify([a, b])",
        "[5,6]",
    );
    assert_string(
        "const [[k, v], ...rest] = new Map([[1, 2], [3, 4]]); JSON.stringify([k, v, rest])",
        "[1,2,[[3,4]]]",
    );
    assert_string(
        "let a, b; [a, b] = new Set([7, 8]); JSON.stringify([a, b])",
        "[7,8]",
    );
    assert_string(
        "function f([x, y]) { return x + y } JSON.stringify(f(new Set([1, 2])))",
        "3",
    );
}

#[test]
fn yield_star_delegates_to_map_and_set() {
    assert_string(
        "function* g() { yield* new Map([[1, 2]]); yield* new Set([3, 4]) } \
         JSON.stringify([...g()])",
        "[[1,2],3,4]",
    );
    assert_string(
        "const s = new Set([1]); \
         function* g() { yield* s } \
         let out = []; for (const v of g()) { out.push(v); if (v < 3) s.add(v + 1) } \
         JSON.stringify(out)",
        "[1,2,3]",
    );
}

#[test]
fn map_set_iterator_methods_are_absent() {
    for (ctor, method) in [
        ("Map", "entries"),
        ("Map", "keys"),
        ("Map", "values"),
        ("Set", "values"),
        ("Set", "keys"),
        ("Set", "entries"),
    ] {
        assert_uncaught(
            &format!("const c = new {ctor}(); c.{method}()"),
            &format!("TypeError: c.{method} is not a function"),
        );
    }
}

// --- other sources -----------------------------------------------------------

#[test]
fn strings_generators_wrappers_and_arguments_iterate() {
    assert_string(
        &collected("for (const c of 'ab') out.push(c)"),
        "[\"a\",\"b\"]",
    );
    assert_string(
        &collected("for (const c of new String('ab')) out.push(c)"),
        "[\"a\",\"b\"]",
    );
    assert_string("JSON.stringify([...new String('ab')])", "[\"a\",\"b\"]");
    assert_string(
        "function* g() { yield 1; yield 2 } const [a, b] = g(); JSON.stringify([a, b])",
        "[1,2]",
    );
    assert_string(
        "function f() { return [...arguments] } JSON.stringify(f(1, 2))",
        "[1,2]",
    );
    assert_string(
        "function f() { let out = []; for (const x of arguments) out.push(x); return out } \
         JSON.stringify(f(3, 4))",
        "[3,4]",
    );
    assert_string(
        "function* g() { yield* new String('xy') } JSON.stringify([...g()])",
        "[\"x\",\"y\"]",
    );
}

#[test]
fn for_of_over_an_array_visits_elements_pushed_by_the_body() {
    assert_string(
        &collected(
            "const a = [1]; for (const x of a) { out.push(x); if (a.length < 3) a.push(x + 1) }",
        ),
        "[1,2,3]",
    );
}

// --- not iterable ------------------------------------------------------------

/// Each non-iterable source, with its message (`GetMethod(v, @@iterator)`
/// is absent or not callable).
const NOT_ITERABLE: [(&str, &str); 11] = [
    ("Promise.resolve(1)", "object is not iterable"),
    ("new Date(0)", "object is not iterable"),
    ("({ a: 1 })", "object is not iterable"),
    ("function () {}", "function is not iterable"),
    ("5", "number 5 is not iterable"),
    ("true", "boolean true is not iterable"),
    ("new Number(1)", "object is not iterable"),
    ("new Boolean(false)", "object is not iterable"),
    ("undefined", "undefined is not iterable"),
    ("({ [Symbol.iterator]: 1 })", "object is not iterable"),
    ("({ [Symbol.iterator]: null })", "object is not iterable"),
];

#[test]
fn for_of_over_a_non_iterable_throws_type_error() {
    for (source, message) in NOT_ITERABLE {
        assert_uncaught(
            &format!("const v = {source}; for (const x of v) {{}}"),
            &format!("TypeError: {message}"),
        );
    }
    assert_uncaught(
        "for (const x of 5) {}",
        "TypeError: number 5 is not iterable",
    );
    assert_uncaught(
        "for (const x of { a: 1 }) {}",
        "TypeError: object is not iterable",
    );
}

#[test]
fn spread_of_a_non_iterable_throws_type_error() {
    for (source, message) in NOT_ITERABLE {
        assert_uncaught(
            &format!("const v = {source}; [...v]"),
            &format!("TypeError: {message}"),
        );
        assert_uncaught(
            &format!("function f() {{}} const v = {source}; f(...v)"),
            &format!("TypeError: {message}"),
        );
    }
    assert_uncaught("[...null]", "TypeError: null is not iterable");
}

#[test]
fn destructuring_a_non_iterable_throws_type_error() {
    for (source, message) in NOT_ITERABLE {
        assert_uncaught(
            &format!("const v = {source}; const [a] = v"),
            &format!("TypeError: {message}"),
        );
        assert_uncaught(
            &format!("const v = {source}; let a; [a] = v"),
            &format!("TypeError: {message}"),
        );
    }
}

#[test]
fn yield_star_of_a_non_iterable_throws_type_error() {
    for (source, message) in NOT_ITERABLE {
        assert_uncaught(
            &format!("const v = {source}; function* g() {{ yield* v }} [...g()]"),
            &format!("TypeError: {message}"),
        );
    }
}

#[test]
fn a_not_iterable_throw_is_catchable_and_stops_evaluation() {
    assert_string(
        "let out = []; function f() { out.push('f') } \
         try { [...5, f()] } catch (e) { out.push(e instanceof TypeError) } \
         JSON.stringify(out)",
        "[true]",
    );
}

#[test]
fn for_of_destructuring_head_rejects_duplicate_bindings() {
    assert_rejected("for (const [x, x] of []) {}");
    assert_rejected("for (let { a, b: [a] } of []) {}");
}
