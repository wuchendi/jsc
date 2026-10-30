//! Integration tests: the builtin iterator intrinsics and iterator objects
//! (`design/symbol-strings.md` §5.4): `keys` / `values` / `entries` /
//! `[Symbol.iterator]` on Array, Map, Set and String, their `next`, and the
//! shared `%IteratorPrototype%`; and the iteration protocol behind every
//! consumer (§5.1-§5.3): user iterables, `next` read once, the result
//! checks, `IteratorClose` on abrupt exits (B3), and the guarded fast paths
//! (B1).

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

// --- the protocol (design/symbol-strings.md §5.1-§5.3) -----------------------------

/// A user iterable over `1, 2, 3` that logs `next` / `return` calls into
/// `log` (`return` logs its argument count).
const COUNTER: &str = "const log = [];
     const it = {
       [Symbol.iterator]() { log.push('iter'); return this },
       i: 0,
       next() { log.push('next'); this.i++; return { value: this.i, done: this.i > 3 } },
       return() { log.push('return:' + arguments.length); return {} },
     };";

#[test]
fn user_iterables_feed_every_consumer() {
    let cases = [
        (
            "const out = []; for (const x of it) out.push(x); out.join(',')",
            "1,2,3",
        ),
        ("[...it].join(',')", "1,2,3"),
        ("[0, ...it, 4].join(',')", "0,1,2,3,4"),
        (
            "function f(a, b, c) { return a + b + c } String(f(...it))",
            "6",
        ),
        ("Array.from(it).join(',')", "1,2,3"),
        ("Array.from(it, (x, i) => x * 10 + i).join(',')", "10,21,32"),
        ("new AggregateError(it).errors.join(',')", "1,2,3"),
        ("const [a, b] = it; a + ',' + b", "1,2"),
        ("function* g() { yield* it } [...g()].join(',')", "1,2,3"),
    ];
    for (consumer, expected) in cases {
        assert_string(&format!("{COUNTER} {consumer}"), expected);
    }
    assert_string(
        "const entries = { *[Symbol.iterator]() { yield ['a', 1]; yield ['b', 2] } };
         JSON.stringify(Object.fromEntries(entries))",
        "{\"a\":1,\"b\":2}",
    );
}

#[test]
fn a_completed_loop_does_not_close() {
    assert_string(
        &format!("{COUNTER} for (const x of it) {{}} log.join(',')"),
        "iter,next,next,next,next",
    );
    assert_string(
        &format!("{COUNTER} [...it]; log.join(',')"),
        "iter,next,next,next,next",
    );
}

#[test]
fn next_is_read_once() {
    assert_string(
        "let reads = 0; const out = [];
         const it = { i: 0, [Symbol.iterator]() { return this },
           get next() { reads++; return () => ({ value: ++this.i, done: this.i > 2 }) } };
         for (const x of it) out.push(x);
         [...it];
         reads + ':' + out.join(',')",
        "2:1,2",
    );
    // Replacing `next` mid-loop is unobservable.
    assert_string(
        "const out = [];
         const it = { i: 0, [Symbol.iterator]() { return this },
           next() { return { value: ++this.i, done: this.i > 3 } } };
         for (const x of it) { out.push(x); it.next = () => ({ done: true }) }
         out.join(',')",
        "1,2,3",
    );
}

#[test]
fn a_non_object_step_result_throws_type_error() {
    let source = "const it = { [Symbol.iterator]() { return { next() { return 1 } } } };";
    for consumer in ["for (const x of it) {}", "[...it]", "Array.from(it)"] {
        assert_uncaught(
            &format!("{source} {consumer}"),
            "TypeError: Iterator result 1 is not an object",
        );
    }
}

#[test]
fn an_iterator_method_returning_a_primitive_throws_type_error() {
    let source = "const it = { [Symbol.iterator]() { return 1 } };";
    for consumer in ["for (const x of it) {}", "[...it]", "new Set(it)"] {
        assert_uncaught(
            &format!("{source} {consumer}"),
            "TypeError: Result of the Symbol.iterator method is not an object",
        );
    }
}

#[test]
fn a_non_callable_next_throws_type_error() {
    assert_uncaught(
        "const it = { [Symbol.iterator]() { return { next: 1 } } }; for (const x of it) {}",
        "TypeError: 1 is not a function",
    );
}

#[test]
fn modified_builtin_iterators_are_honoured() {
    assert_string(
        "Array.prototype[Symbol.iterator] = function* () { yield 'x'; yield 'y' };
         const out = []; for (const v of [1, 2, 3]) out.push(v);
         out.join(',') + '|' + [...[1, 2, 3]].join(',')",
        "x,y|x,y",
    );
    assert_string(
        "const a = [1, 2]; a[Symbol.iterator] = function* () { yield 'own' }; [...a].join(',')",
        "own",
    );
    assert_number(
        "Object.getPrototypeOf([][Symbol.iterator]()).next = () => ({ done: true });
         [...[1, 2]].length",
        0.0,
    );
    assert_string(
        "String.prototype[Symbol.iterator] = function* () { yield 'S' };
         const out = []; for (const c of 'ab') out.push(c); out.join(',')",
        "S",
    );
}

// --- B1: an own `next` on a generator object ----------------------------------------

#[test]
fn an_own_next_on_a_generator_is_called() {
    let generator = "let n = 0; function* g() { yield 1; yield 2 }
         const it = g(); it.next = function () { n++; return { done: true } };";
    assert_number(&format!("{generator} for (const x of it) {{}} n"), 1.0);
    assert_number(&format!("{generator} [...it].length"), 0.0);
    assert_number(&format!("{generator} [...it]; n"), 1.0);
    assert_number(&format!("{generator} Array.from(it).length"), 0.0);
    assert_number(&format!("{generator} Array.from(it); n"), 1.0);
}

// --- B3: IteratorClose on abrupt exits -----------------------------------------------

#[test]
fn break_return_and_throw_close_a_user_iterator_once_without_arguments() {
    assert_string(
        &format!("{COUNTER} for (const x of it) break; log.join(',')"),
        "iter,next,return:0",
    );
    assert_string(
        &format!("{COUNTER} function f() {{ for (const x of it) return x }} f(); log.join(',')"),
        "iter,next,return:0",
    );
    assert_string(
        &format!("{COUNTER} try {{ for (const x of it) throw 1 }} catch (e) {{}} log.join(',')"),
        "iter,next,return:0",
    );
    assert_string(
        &format!(
            "{COUNTER} outer: for (const y of [1]) {{ for (const x of it) continue outer }} \
             log.join(',')"
        ),
        "iter,next,return:0",
    );
    // A throw from the head's destructuring closes too.
    assert_string(
        &format!(
            "{COUNTER} try {{ for (const [a] of {{ [Symbol.iterator]() {{ return it }} }}) {{}} }} \
             catch (e) {{}} log.join(',')"
        ),
        "next,return:0",
    );
}

#[test]
fn a_throw_from_the_iterator_does_not_close() {
    assert_string(
        "const log = [];
         const it = { [Symbol.iterator]() { return this },
           next() { throw 'boom' }, return() { log.push('return'); return {} } };
         try { for (const x of it) {} } catch (e) { log.push(e) }
         log.join(',')",
        "boom",
    );
    assert_string(
        "const log = [];
         const it = { [Symbol.iterator]() { return this },
           next() { return { get value() { throw 'value' }, done: false } },
           return() { log.push('return'); return {} } };
         try { for (const x of it) {} } catch (e) { log.push(e) }
         log.join(',')",
        "value",
    );
}

#[test]
fn a_non_object_return_result_throws_only_on_non_throw_exits() {
    let source = "const it = { [Symbol.iterator]() { return this },
           next() { return { value: 1, done: false } }, return() { return 5 } };";
    assert_uncaught(
        &format!("{source} for (const x of it) break"),
        "TypeError: Iterator result 5 is not an object",
    );
    // A throw completion wins over the close's own failure.
    assert_string(
        &format!(
            "{source} let e; try {{ for (const x of it) throw 'body' }} catch (x) {{ e = x }} e"
        ),
        "body",
    );
    // A `return` method that throws replaces a break, but not a throw.
    assert_string(
        "const it = { [Symbol.iterator]() { return this },
           next() { return { value: 1, done: false } }, return() { throw 'close' } };
         let a, b;
         try { for (const x of it) break } catch (e) { a = e }
         try { for (const x of it) throw 'body' } catch (e) { b = e }
         a + ',' + b",
        "close,body",
    );
}

#[test]
fn break_return_and_throw_run_a_generators_finally() {
    let generator = "const log = [];
         function* g() { try { yield 1; yield 2 } finally { log.push('f') } }";
    assert_string(
        &format!("{generator} for (const x of g()) break; log.join(',')"),
        "f",
    );
    assert_string(
        &format!("{generator} function h() {{ for (const x of g()) return x }} h(); log.join(',')"),
        "f",
    );
    assert_string(
        &format!("{generator} try {{ for (const x of g()) throw 0 }} catch (e) {{}} log.join(',')"),
        "f",
    );
}

#[test]
fn closing_a_done_or_builtin_iterator_is_harmless() {
    assert_string(
        "const out = [];
         for (const x of [1, 2, 3]) { if (x === 2) break; out.push(x) }
         for (const c of 'abc') { if (c === 'b') break; out.push(c) }
         for (const [k] of new Map([[7, 0], [8, 0]])) { out.push(k); break }
         function* g() { yield 'g'; yield 'h' }
         for (const v of g()) { out.push(v); break }
         out.join(',')",
        "1,a,7,g",
    );
}
