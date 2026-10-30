//! Integration tests: the builtin iterator intrinsics and iterator objects
//! (`design/symbol-strings.md` §5.4): `keys` / `values` / `entries` /
//! `[Symbol.iterator]` on Array, Map, Set and String, their `next`, and the
//! shared `%IteratorPrototype%`.

mod common;
use common::{assert_bool, assert_number, assert_string, assert_uncaught};

// --- iterator results ------------------------------------------------------------

#[test]
fn array_values_steps_the_elements() {
    assert_string(
        "const it = [1, 2].values(); JSON.stringify([it.next(), it.next(), it.next(), it.next()])",
        "[{\"value\":1,\"done\":false},{\"value\":2,\"done\":false},\
         {\"done\":true},{\"done\":true}]",
    );
}

#[test]
fn array_keys_and_entries_shapes() {
    assert_string(
        "const k = ['a', 'b'].keys(); JSON.stringify([k.next(), k.next(), k.next()])",
        "[{\"value\":0,\"done\":false},{\"value\":1,\"done\":false},{\"done\":true}]",
    );
    assert_string(
        "const e = ['a', 'b'].entries(); JSON.stringify([e.next(), e.next(), e.next()])",
        "[{\"value\":[0,\"a\"],\"done\":false},{\"value\":[1,\"b\"],\"done\":false},\
         {\"done\":true}]",
    );
}

#[test]
fn map_and_set_iterator_shapes() {
    assert_string(
        "const m = new Map([['a', 1], ['b', 2]]);
         JSON.stringify([m.keys().next(), m.values().next(), m.entries().next()])",
        "[{\"value\":\"a\",\"done\":false},{\"value\":1,\"done\":false},\
         {\"value\":[\"a\",1],\"done\":false}]",
    );
    assert_string(
        "const s = new Set(['x']); const it = s.entries();
         JSON.stringify([s.values().next(), it.next(), it.next()])",
        "[{\"value\":\"x\",\"done\":false},{\"value\":[\"x\",\"x\"],\"done\":false},\
         {\"done\":true}]",
    );
}

#[test]
fn string_iterator_yields_characters() {
    assert_string("\"ab\"[Symbol.iterator]().next().value", "a");
    assert_string(
        "const it = 'ab'[Symbol.iterator](); it.next(); const r = it.next(); r.value + r.done",
        "bfalse",
    );
    assert_bool(
        "const it = 'a'[Symbol.iterator](); it.next(); it.next().done",
        true,
    );
}

// --- method identities -------------------------------------------------------------

#[test]
fn iterator_method_aliases_are_identical() {
    assert_bool(
        "Array.prototype[Symbol.iterator] === Array.prototype.values",
        true,
    );
    assert_bool("Set.prototype.keys === Set.prototype.values", true);
    assert_bool(
        "Set.prototype[Symbol.iterator] === Set.prototype.values",
        true,
    );
    assert_bool(
        "Map.prototype[Symbol.iterator] === Map.prototype.entries",
        true,
    );
    assert_string("Array.prototype.values.name", "values");
    assert_string(
        "String.prototype[Symbol.iterator].name",
        "[Symbol.iterator]",
    );
    assert_number("Map.prototype.entries.length", 0.0);
}

// --- %IteratorPrototype% -------------------------------------------------------------

#[test]
fn one_iterator_prototype_above_every_builtin_iterator() {
    assert_bool(
        "const IP = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
         function* g() {}
         [new Map().keys(), new Set().values(), ''[Symbol.iterator](), g()]
           .every(it => Object.getPrototypeOf(Object.getPrototypeOf(it)) === IP
             || Object.getPrototypeOf(Object.getPrototypeOf(Object.getPrototypeOf(it))) === IP)
           && Object.getPrototypeOf(IP) === Object.prototype",
        true,
    );
    // A generator object inherits its function's `prototype`, which inherits
    // %GeneratorPrototype%, which inherits %IteratorPrototype%.
    assert_bool(
        "const IP = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
         function* g() {}
         Object.getPrototypeOf(Object.getPrototypeOf(g.prototype)) === IP",
        true,
    );
}

#[test]
fn iterator_prototype_iterator_returns_this() {
    assert_bool(
        "const IP = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
         const o = {}; IP[Symbol.iterator].call(o) === o",
        true,
    );
    assert_bool(
        "const it = [1].values(); it[Symbol.iterator]() === it",
        true,
    );
}

#[test]
fn iterator_prototypes_are_distinct_per_family() {
    assert_bool(
        "const P = it => Object.getPrototypeOf(it);
         P([].values()) === P([].keys()) && P([].values()) !== P(new Map().keys())
           && P(new Map().keys()) !== P(new Set().values())",
        true,
    );
}

// --- receivers -----------------------------------------------------------------------

#[test]
fn next_on_a_foreign_receiver_throws_type_error() {
    assert_uncaught(
        "const next = [].values().next; next.call({})",
        "TypeError: next method called on incompatible receiver",
    );
    assert_bool(
        "const next = [].values().next;
         try { next.call(new Map().keys()); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { new Map().keys().next.call(new Set().values()); false }
         catch (e) { e instanceof TypeError }",
        true,
    );
}

#[test]
fn creating_methods_check_their_receiver() {
    assert_bool(
        "try { Map.prototype.keys.call(new Set()); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_bool(
        "try { Array.prototype.values.call(undefined); false } catch (e) { e instanceof TypeError }",
        true,
    );
    // An array-like receiver is iterated by `length` and index.
    assert_string(
        "const it = Array.prototype.values.call({ length: 2, 0: 'x', 1: 'y' });
         it.next().value + it.next().value + it.next().done",
        "xytrue",
    );
}

// --- live iteration ------------------------------------------------------------------

#[test]
fn map_iterator_sees_entries_appended_during_iteration() {
    assert_string(
        "const m = new Map([[1, 'a']]); const it = m.keys(); const seen = [];
         let r = it.next();
         while (!r.done) { seen.push(r.value); if (r.value < 3) m.set(r.value + 1, 'x'); r = it.next(); }
         seen.join(',')",
        "1,2,3",
    );
}

#[test]
fn array_iterator_rereads_length() {
    assert_string(
        "const a = [1]; const it = a.values(); const seen = [];
         let r = it.next();
         while (!r.done) { seen.push(r.value); if (a.length < 3) a.push(a.length + 1); r = it.next(); }
         seen.join(',')",
        "1,2,3",
    );
}

#[test]
fn an_exhausted_iterator_stays_done() {
    assert_bool(
        "const a = []; const it = a.values(); it.next(); a.push(1); it.next().done",
        true,
    );
}
