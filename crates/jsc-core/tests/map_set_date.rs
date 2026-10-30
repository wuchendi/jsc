//! Integration tests: `%Map.prototype%`, `%Set.prototype%` and
//! `%Date.prototype%` methods as real builtin function values (DESIGN.md
//! §6.4), their guarded call-site fast paths, the `size` getter,
//! the iterable `Map`/`Set` constructors and `Date()`.

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught,
};

/// A prefix that makes a program not intrinsics-sealed without writing any
/// prototype, so the call-site fast paths run behind their run-time guards.
const OPEN: &str = "let __open = 'prototype'; ";

/// Assert `src` evaluates to the string `expected`, both intrinsics-sealed
/// and behind run-time guards.
fn both_string(src: &str, expected: &str) {
    assert_string(src, expected);
    assert_string(&format!("{OPEN}{src}"), expected);
}

/// [`both_string`] for a number.
fn both_number(src: &str, expected: f64) {
    assert_number(src, expected);
    assert_number(&format!("{OPEN}{src}"), expected);
}

/// [`both_string`] for a boolean.
fn both_bool(src: &str, expected: bool) {
    assert_bool(src, expected);
    assert_bool(&format!("{OPEN}{src}"), expected);
}

// --- builtin function values ---------------------------------------------------

#[test]
fn map_set_methods_are_function_values() {
    for (name, length) in [
        ("get", 1.0),
        ("set", 2.0),
        ("has", 1.0),
        ("delete", 1.0),
        ("clear", 0.0),
        ("forEach", 1.0),
    ] {
        both_string(&format!("typeof new Map().{name}"), "function");
        both_bool(&format!("new Map().{name} === Map.prototype.{name}"), true);
        both_string(&format!("Map.prototype.{name}.name"), name);
        both_number(&format!("Map.prototype.{name}.length"), length);
    }
    for (name, length) in [
        ("add", 1.0),
        ("has", 1.0),
        ("delete", 1.0),
        ("clear", 0.0),
        ("forEach", 1.0),
    ] {
        both_string(&format!("typeof new Set().{name}"), "function");
        both_bool(&format!("new Set().{name} === Set.prototype.{name}"), true);
        both_string(&format!("Set.prototype.{name}.name"), name);
        both_number(&format!("Set.prototype.{name}.length"), length);
    }
    // Map and Set own distinct functions.
    both_bool("Map.prototype.has === Set.prototype.has", false);
}

#[test]
fn date_methods_are_function_values() {
    for name in [
        "getTime",
        "valueOf",
        "toString",
        "getFullYear",
        "getMonth",
        "getDate",
        "getDay",
        "getHours",
        "getMinutes",
        "getSeconds",
        "toISOString",
    ] {
        both_string(&format!("typeof new Date(0).{name}"), "function");
        both_bool(
            &format!("new Date(0).{name} === Date.prototype.{name}"),
            true,
        );
        both_string(&format!("Date.prototype.{name}.name"), name);
        both_number(&format!("Date.prototype.{name}.length"), 0.0);
    }
}

// --- size getter -----------------------------------------------------------------

#[test]
fn size_is_a_prototype_getter() {
    both_number("const m = new Map(); m.set(1, 2); m.set(3, 4); m.size", 2.0);
    both_number("const s = new Set(); s.add(1); s.add(1); s.size", 1.0);
    both_bool("new Map().hasOwnProperty('size')", false);
    both_bool("Map.prototype.hasOwnProperty('size')", true);
    both_bool("Set.prototype.hasOwnProperty('size')", true);
    // `Map.prototype.size` runs the getter on a non-Map receiver.
    assert_uncaught(
        "Map.prototype.size",
        "TypeError: Method get Map.prototype.size called on incompatible receiver",
    );
    assert_uncaught(
        "Set.prototype.size",
        "TypeError: Method get Set.prototype.size called on incompatible receiver",
    );
    // A subclass instance reaches the getter through its prototype chain.
    assert_number(
        "class M extends Map {} const m = new M(); m.set(1, 1); m.size",
        1.0,
    );
    // The getter has no setter: an assignment throws (strict code).
    assert_uncaught(
        "const m = new Map(); m.size = 9",
        "TypeError: Cannot set property size of object which has only a getter",
    );
}

// --- receivers ------------------------------------------------------------------

#[test]
fn methods_reject_incompatible_receivers() {
    assert_uncaught(
        "Map.prototype.get.call({}, 1)",
        "TypeError: Method Map.prototype.get called on incompatible receiver",
    );
    assert_uncaught(
        "Map.prototype.get.call(new Set(), 1)",
        "TypeError: Method Map.prototype.get called on incompatible receiver",
    );
    assert_uncaught(
        "Set.prototype.add.call(new Map(), 1)",
        "TypeError: Method Set.prototype.add called on incompatible receiver",
    );
    assert_uncaught(
        "Date.prototype.getTime.call({})",
        "TypeError: this is not a Date object.",
    );
    assert_uncaught(
        "Date.prototype.getFullYear.call(5)",
        "TypeError: this is not a Date object.",
    );
}

#[test]
fn methods_called_through_values() {
    both_number(
        "const m = new Map(); const set = m.set; set.call(m, 1, 5); Map.prototype.get.call(m, 1)",
        5.0,
    );
    both_bool(
        "const s = new Set(); Set.prototype.add.call(s, 2); s.has(2)",
        true,
    );
    both_number("Date.prototype.getTime.call(new Date(7))", 7.0);
    // `set` and `add` return the receiver; `has`/`delete` booleans.
    both_bool("const m = new Map(); m.set(1, 1) === m", true);
    both_bool("const s = new Set(); s.add(1) === s", true);
    both_bool("const m = new Map(); m.set(1, 1); m.delete(1)", true);
    both_bool("const m = new Map(); m.delete(1)", false);
}

#[test]
fn clear_empties_the_collection() {
    both_number(
        "const m = new Map(); m.set(1, 1); m.set(2, 2); m.clear(); m.size",
        0.0,
    );
    both_bool(
        "const s = new Set(); s.add(1); s.clear(); s.add(2); s.has(1) || !s.has(2)",
        false,
    );
    both_string("typeof new Map().clear()", "undefined");
}

// --- arity ------------------------------------------------------------------------

#[test]
fn extra_and_missing_arguments_are_runtime_correct() {
    both_number("new Date(5).getTime(1, 2)", 5.0);
    both_number("new Date(0).getFullYear('x')", 1970.0);
    both_bool("const m = new Map(); m.set(); m.has(undefined)", true);
    both_string(
        "const m = new Map(); m.set(1); typeof m.get(1)",
        "undefined",
    );
    both_number("const m = new Map(); m.set(1, 2, 3); m.get(1, 9)", 2.0);
    // Extra arguments are still evaluated.
    both_number(
        "let n = 0; const d = new Date(3); d.getTime(n++, n++); n",
        2.0,
    );
}

#[test]
fn spread_calls_work() {
    both_number("new Date(4).getTime(...[1, 2])", 4.0);
    both_number("const m = new Map(); m.set(...[1, 8]); m.get(...[1])", 8.0);
}

// --- DF12: user members named like builtins ------------------------------------------

#[test]
fn user_members_named_like_builtins_are_called() {
    both_number(
        "const o = { getTime() { return 41 } }; o.getTime() + 1",
        42.0,
    );
    both_string(
        "class C { get(k) { return 'g' + k } set(k, v) { return 's' } has() { return 'h' } \
         add() { return 'a' } } const c = new C(); c.get(1) + c.set(1, 2) + c.has() + c.add()",
        "g1sha",
    );
    both_number("const o = { size: 3 }; o.size", 3.0);
    both_number("class S { get size() { return 7 } } new S().size", 7.0);
    // Own members on a collection instance shadow the prototype.
    both_number(
        "const m = new Map(); m.get = function () { return 9 }; m.get(1)",
        9.0,
    );
    both_number(
        "const d = new Date(1); d.getTime = () => 5; d.getTime()",
        5.0,
    );
    // A subclass override is honoured.
    assert_number(
        "class M extends Map { get(k) { return 100 } } const m = new M(); m.set(1, 1); m.get(1)",
        100.0,
    );
    // Calling a builtin name on a primitive/unrelated object without it throws.
    assert_uncaught(
        "const o = {}; o.getTime()",
        "TypeError: o.getTime is not a function",
    );
}

#[test]
fn prototype_overrides_are_honoured() {
    assert_number(
        "Map.prototype.get = function () { return 77 }; const m = new Map(); m.get(1)",
        77.0,
    );
    assert_number(
        "Date.prototype.getTime = function () { return 3 }; new Date(9).getTime()",
        3.0,
    );
    assert_number(
        "class M extends Map { get size() { return 12 } } new M().size",
        12.0,
    );
}

// --- forEach ----------------------------------------------------------------------

#[test]
fn map_for_each_visits_value_key_map() {
    assert_output(
        "const m = new Map(); m.set('a', 1); m.set('b', 2); \
         m.forEach(function (v, k, map) { console.log(k, v, map === m) })",
        "a 1 true\nb 2 true\n",
    );
    both_string("typeof new Map().forEach(() => 1)", "undefined");
}

#[test]
fn set_for_each_visits_value_value_set() {
    assert_output(
        "const s = new Set(); s.add('x'); s.add('y'); \
         s.forEach(function (v, k, set) { console.log(v, k, set === s) })",
        "x x true\ny y true\n",
    );
}

#[test]
fn for_each_this_arg_and_live_iteration() {
    both_number(
        "const m = new Map(); m.set(1, 1); let t = 0; \
         m.forEach(function () { t = this.n }, { n: 6 }); t",
        6.0,
    );
    // Entries added during the walk are visited.
    both_number(
        "const s = new Set(); s.add(1); let n = 0; \
         s.forEach(v => { n++; if (v < 4) s.add(v + 1) }); n",
        4.0,
    );
    // Strict callbacks see `undefined` as `this` without a thisArg.
    both_bool(
        "let ok; new Map([[1, 1]]).forEach(function () { 'use strict'; ok = this === undefined }); ok",
        true,
    );
}

#[test]
fn for_each_type_errors() {
    assert_uncaught(
        "new Map().forEach(1)",
        "TypeError: Map.prototype.forEach callback is not a function",
    );
    assert_uncaught(
        "new Set().forEach()",
        "TypeError: Set.prototype.forEach callback is not a function",
    );
    assert_uncaught(
        "Map.prototype.forEach.call(new Set(), () => 1)",
        "TypeError: Method Map.prototype.forEach called on incompatible receiver",
    );
}

// --- iterable constructors ------------------------------------------------------------

#[test]
fn iterable_constructors() {
    both_number("new Map([[1, 2], [3, 4]]).get(3)", 4.0);
    both_number("new Set([1, 2, 2, 3]).size", 3.0);
    both_number("new Set('abca').size", 3.0);
    both_number("new Map(undefined).size", 0.0);
    both_number("new Set(null).size", 0.0);
    // Through the constructor value.
    both_number("const M = Map; new M([[1, 2]]).get(1)", 2.0);
    both_number("const S = Set; new S([1, 2, 2]).size", 2.0);
    // Generators and other collections are iterables.
    both_number(
        "function* g() { yield [1, 'a']; yield [2, 'b'] } new Map(g()).size",
        2.0,
    );
    both_number(
        "function* g() { yield 1; yield 1; yield 2 } new Set(g()).size",
        2.0,
    );
    both_number(
        "const a = new Map([[1, 10]]); const b = new Map(a); b.set(2, 2); a.size * 10 + b.get(1)",
        20.0,
    );
    both_bool("const a = new Set([5]); new Set(a).has(5)", true);
    both_number("new Map(new Set([[1, 9]])).get(1)", 9.0);
    // Entries are read from any object.
    both_number("new Map([{ 0: 'k', 1: 3 }]).get('k')", 3.0);
}

#[test]
fn iterable_constructor_errors() {
    assert_uncaught(
        "new Map([1])",
        "TypeError: Iterator value 1 is not an entry object",
    );
    assert_uncaught(
        "const M = Map; new M([[1, 2], 'x'])",
        "TypeError: Iterator value x is not an entry object",
    );
    assert_uncaught("new Set(5)", "TypeError: number 5 is not iterable");
    assert_uncaught("new Map({})", "TypeError: object is not iterable");
    assert_uncaught("Map([])", "TypeError: Constructor Map requires 'new'");
}

#[test]
fn iterable_constructors_call_the_adder() {
    assert_number(
        "let n = 0; const add = Set.prototype.add; \
         Set.prototype.add = function (v) { n += v; return add.call(this, v) }; \
         new Set([1, 2]); n",
        3.0,
    );
    assert_uncaught(
        "Map.prototype.set = 1; new Map([[1, 1]])",
        "TypeError: '1' returned for property 'set' of object '#<Map>' is not a function",
    );
}

/// `AddEntriesFromIterable`: the adder is read once, before the iterator
/// opens; a throwing adder closes a user iterator and the adder's error
/// propagates.
#[test]
fn iterable_constructors_close_on_a_throwing_adder() {
    both_string(
        "const log = []; \
         const it = { [Symbol.iterator]() { return this }, \
           next() { log.push('next'); return { value: [1, 2], done: false } }, \
           return() { log.push('return'); return {} } }; \
         class M extends Map { set(k, v) { log.push('set'); throw new Error('boom') } } \
         try { new M(it) } catch (e) { log.push(e.message) } log.join()",
        "next,set,return,boom",
    );
    both_string(
        "const log = []; \
         const it = { [Symbol.iterator]() { return this }, \
           next() { return { value: 1, done: false } }, \
           return() { log.push('return'); return {} } }; \
         class S extends Set { add(v) { throw new RangeError('no') } } \
         try { new S(it) } catch (e) { log.push(e.name) } log.join()",
        "return,RangeError",
    );
    // The adder is read once, before `@@iterator` is called.
    both_string(
        "const log = []; \
         class S extends Set { get add() { log.push('get add'); return Set.prototype.add } } \
         const it = { [Symbol.iterator]() { log.push('iterator'); return [1, 2][Symbol.iterator]() } }; \
         const s = new S(it); log.push(s.size); log.join()",
        "get add,iterator,2",
    );
}

/// A non-object `Map` entry closes the iterator, then throws `TypeError`; a
/// throw from `next` itself does not close.
#[test]
fn map_constructor_closes_on_a_non_object_entry() {
    both_string(
        "const log = []; \
         const it = { [Symbol.iterator]() { return this }, \
           next() { return { value: 7, done: false } }, \
           return() { log.push('return'); return {} } }; \
         try { new Map(it) } catch (e) { log.push(e instanceof TypeError, e.message) } log.join()",
        "return,true,Iterator value 7 is not an entry object",
    );
    both_string(
        "const log = []; \
         const it = { [Symbol.iterator]() { return this }, \
           next() { throw new Error('next') }, \
           return() { log.push('return'); return {} } }; \
         try { new Map(it) } catch (e) { log.push(e.message) } log.join()",
        "next",
    );
}

/// `new Set(arguments)` / `new Map(arguments)` iterate the `arguments`
/// object through its own `@@iterator` (`%Array.prototype.values%`).
#[test]
fn collections_from_arguments() {
    both_number(
        "function f() { return new Set(arguments) } f(1, 2, 2, 3).size",
        3.0,
    );
    both_number(
        "function f() { return new Map(arguments) } f([1, 'a'], [2, 'b']).get(2) === 'b' ? 1 : 0",
        1.0,
    );
    both_bool(
        "function f() { return arguments[Symbol.iterator] === Array.prototype.values } f()",
        true,
    );
    both_string(
        "function f() { const d = Object.getOwnPropertyDescriptor(arguments, Symbol.iterator); \
           return [d.writable, d.enumerable, d.configurable].join() } f()",
        "true,false,true",
    );
    // A replaced own `@@iterator` is honoured.
    both_number(
        "function f() { arguments[Symbol.iterator] = function* () { yield 9 }; \
           return [...new Set(arguments)][0] } f(1, 2)",
        9.0,
    );
}

// --- deletion and live iteration (B2, design/symbol-strings.md §5.5) ------------

/// Deleting the visited entry during for-of or `forEach` does not skip the
/// next one (B2).
#[test]
fn delete_during_iteration_visits_every_entry() {
    both_string(
        "const m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]); const seen = []; \
         for (const [k] of m) { seen.push(k); if (k === 1) m.delete(1) } seen.join()",
        "1,2,3",
    );
    both_string(
        "const m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]); const seen = []; \
         m.forEach((v, k) => { seen.push(k); if (k === 1) m.delete(1) }); seen.join()",
        "1,2,3",
    );
    both_string(
        "const s = new Set([1, 2, 3]); const seen = []; \
         for (const v of s) { seen.push(v); s.delete(v) } seen.join() + '/' + s.size",
        "1,2,3/0",
    );
    both_string(
        "const s = new Set([1, 2, 3]); const seen = []; \
         s.forEach(v => { seen.push(v); s.delete(v) }); seen.join() + '/' + s.size",
        "1,2,3/0",
    );
    // Through an iterator object.
    both_string(
        "const m = new Map([[1, 1], [2, 2], [3, 3]]); const it = m.keys(); \
         const a = it.next().value; m.delete(a); m.delete(2); \
         a + ',' + it.next().value + ',' + it.next().done",
        "1,3,true",
    );
}

/// `clear()` during iteration, then `set`: the live iterator continues and
/// visits the new entry.
#[test]
fn clear_during_iteration_then_set_visits_the_new_entry() {
    both_string(
        "const m = new Map([[1, 1], [2, 2]]); const seen = []; \
         for (const [k] of m) { seen.push(k); if (k === 1) { m.clear(); m.set(9, 9) } } \
         seen.join() + '/' + m.size",
        "1,9/1",
    );
    both_string(
        "const s = new Set([1, 2]); const seen = []; \
         s.forEach(v => { seen.push(v); if (v === 1) { s.clear(); s.add(5) } }); seen.join()",
        "1,5",
    );
    both_number(
        "const s = new Set([1, 2]); const it = s.values(); it.next(); \
         s.clear(); s.add(3); it.next().value",
        3.0,
    );
}

/// `size` counts live entries; re-adding a deleted key appends it.
#[test]
fn size_and_re_adding_after_deletes() {
    both_number(
        "const m = new Map([[1, 1], [2, 2], [3, 3]]); m.delete(2); m.delete(2); m.delete(9); m.size",
        2.0,
    );
    both_number(
        "const s = new Set([1, 2, 3]); s.clear(); s.add(4); s.size",
        1.0,
    );
    both_string(
        "const m = new Map([['a', 1], ['b', 2], ['c', 3]]); m.delete('a'); m.set('a', 4); \
         [...m.keys()].join() + '/' + m.get('a') + '/' + m.size",
        "b,c,a/4/3",
    );
    both_string(
        "const s = new Set([1, 2, 3]); s.delete(1); s.add(1); [...s].join()",
        "2,3,1",
    );
    both_bool(
        "const m = new Map([[NaN, 1]]); m.delete(NaN); !m.has(NaN) && m.get(NaN) === undefined",
        true,
    );
}

// --- Date ------------------------------------------------------------------------

#[test]
fn date_to_string_format() {
    both_string(
        "new Date(0).toString()",
        "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)",
    );
    both_string(
        "String(new Date(1710012345678))",
        "Sat Mar 09 2024 19:25:45 GMT+0000 (Coordinated Universal Time)",
    );
    both_string(
        "new Date(-86400000).toString()",
        "Wed Dec 31 1969 00:00:00 GMT+0000 (Coordinated Universal Time)",
    );
    both_string("new Date(NaN).toString()", "Invalid Date");
}

#[test]
fn date_called_as_a_function_is_a_to_string_string() {
    both_string("typeof Date()", "string");
    both_bool(
        "Date().endsWith(' GMT+0000 (Coordinated Universal Time)')",
        true,
    );
    // Arguments are ignored.
    both_bool("Date(0).length === 62", true);
    both_bool("const D = Date; D().length === 62", true);
}

// --- TimeClip and invalid dates ---------------------------------------------------

/// The constructor path behind `new Date(...)` and an indirect `new D(...)`.
fn both_ctors(f: impl Fn(&str) -> String, check: impl Fn(&str)) {
    check(&f("Date"));
    check(&format!("const D = Date; {}", f("D")));
}

#[test]
fn date_time_clip_truncates_and_bounds() {
    for (arg, expected) in [
        ("8.64e15", 8.64e15),
        ("-8.64e15", -8.64e15),
        ("1.9", 1.0),
        ("-1.9", -1.0),
        ("'7'", 7.0),
    ] {
        both_ctors(
            |d| format!("new {d}({arg}).getTime()"),
            |src| both_number(src, expected),
        );
    }
    for arg in [
        "8.64e15 + 1",
        "-8.64e15 - 1",
        "1e20",
        "Infinity",
        "-Infinity",
        "NaN",
    ] {
        both_ctors(
            |d| format!("isNaN(new {d}({arg}).getTime())"),
            |src| both_bool(src, true),
        );
    }
    both_ctors(
        |d| format!("1 / new {d}(-0).getTime() === Infinity"),
        |src| both_bool(src, true),
    );
    both_ctors(
        |d| format!("1 / new {d}(-0.5).valueOf() === Infinity"),
        |src| both_bool(src, true),
    );
    // The host clock is clipped too: a whole, finite millisecond count.
    both_bool("const t = new Date().getTime(); t === Math.trunc(t)", true);
}

#[test]
fn date_from_a_date_takes_its_time_value() {
    // Not ToPrimitive: a Date's own `valueOf`/`toString` are not consulted.
    both_ctors(
        |d| format!("const a = new Date(5); a.valueOf = () => 9; new {d}(a).getTime()"),
        |src| both_number(src, 5.0),
    );
    both_ctors(
        |d| format!("new {d}(new Date(1e20)).toString()"),
        |src| both_string(src, "Invalid Date"),
    );
    // An ordinary object still goes through `valueOf`.
    both_number("new Date({ valueOf() { return 4.5 } }).getTime()", 4.0);
}

#[test]
fn invalid_date_behavior() {
    for getter in [
        "getTime",
        "valueOf",
        "getFullYear",
        "getMonth",
        "getDate",
        "getDay",
        "getHours",
        "getMinutes",
        "getSeconds",
    ] {
        both_bool(&format!("isNaN(new Date(1e20).{getter}())"), true);
    }
    both_string("new Date(1e20).toString()", "Invalid Date");
    both_string("String(new Date(8.64e15 + 1))", "Invalid Date");
    both_string("JSON.stringify(new Date(NaN))", "null");
    both_string("JSON.stringify({ d: new Date(1e20) })", "{\"d\":null}");
    both_bool("new Date(NaN).toJSON() === null", true);
    both_string(
        "try { new Date(1e20).toISOString(); 'none' } catch (e) { `${e instanceof RangeError} ${e.name}: ${e.message}` }",
        "true RangeError: Invalid time value",
    );
    assert_uncaught(
        "new Date(NaN).toISOString()",
        "RangeError: Invalid time value",
    );
    assert_output("console.log(new Date(1e20))", "Invalid Date\n");
    assert_eval("new Date(1e20)", "Invalid Date");
}

#[test]
fn date_iso_string_extended_years() {
    for (ms, iso) in [
        ("-8.64e15", "-271821-04-20T00:00:00.000Z"),
        ("8.64e15", "+275760-09-13T00:00:00.000Z"),
        ("-62198755200000", "-000001-01-01T00:00:00.000Z"),
        ("-62167219200000", "0000-01-01T00:00:00.000Z"),
        ("253402300799999", "9999-12-31T23:59:59.999Z"),
        ("253402300800000", "+010000-01-01T00:00:00.000Z"),
        ("-1", "1969-12-31T23:59:59.999Z"),
    ] {
        both_string(&format!("new Date({ms}).toISOString()"), iso);
        both_string(&format!("new Date({ms}).toJSON()"), iso);
    }
    assert_output(
        "console.log(new Date(8.64e15), new Date(-8.64e15))",
        "+275760-09-13T00:00:00.000Z -271821-04-20T00:00:00.000Z\n",
    );
}

// --- the calendar-field constructor ----------------------------------------------

/// `new Date(year, month[, date, hours, minutes, seconds, ms])` builds the
/// UTC time value (`MakeDay` / `MakeTime`, two-digit years in the 1900s,
/// month overflow carried into the year, `TimeClip`).
#[test]
fn date_calendar_fields_build_the_utc_time_value() {
    both_number(
        "new Date(2020, 0, 2, 3, 4, 5, 6).getTime()",
        1_577_934_245_006.0,
    );
    both_number("new Date(2020, 1).getTime()", 1_580_515_200_000.0);
    both_number("new Date(2019, 13, 1).getTime()", 1_580_515_200_000.0);
    both_number("new Date(2020, -1, 1).getTime()", 1_575_158_400_000.0);
    both_number("new Date(99, 0).getFullYear()", 1999.0);
    both_number("new Date(-1, 0).getTime()", -62_198_755_200_000.0);
    both_number("new Date(1970, 0, 1, 0, 0, 0, -1).getTime()", -1.0);
    both_number("new Date(2020.7, 0.9, 1.5).getTime()", 1_577_836_800_000.0);
    both_number("new Date(275760, 8, 13).getTime()", 8.64e15);
    both_bool("isNaN(new Date(275760, 8, 13, 0, 0, 0, 1).getTime())", true);
    both_bool("isNaN(new Date(2020, NaN).getTime())", true);
    both_bool("isNaN(new Date(Infinity, 0).getTime())", true);
    both_number("new Date('2020', '1').getTime()", 1_580_515_200_000.0);
    // Every argument is converted, in order, even after a NaN.
    assert_string(
        "let log = ''; const f = (v) => ({ valueOf() { log += v; return v } }); \
         new Date(f(1), f(NaN), f(3), f(4), f(5), f(6), f(7)); log",
        "1NaN34567",
    );
    // A subclass gets its own prototype.
    assert_bool(
        "class D extends Date {} const d = new D(2020, 0); d instanceof D && d.getTime() === 1577836800000",
        true,
    );
}

/// A spread construct passing more arguments than the `K` slots (these
/// programs keep `K = 1`) hands `Date` every calendar field, read before a
/// field's `valueOf` can publish another spread argument array.
#[test]
fn date_reads_spread_arguments_past_the_slots() {
    assert_number(
        "new Date(...[2020, 0, 2, 3, 4, 5, 6]).getTime() % 1000",
        6.0,
    );
    assert_number(
        "new Date(...[2020, 0, 2, 3, 4, 5, 6]).getTime()",
        1_577_934_245_006.0,
    );
    assert_number(
        "const y = { valueOf() { return Number(...[2020]) } }; \
         new Date(...[y, 0, 2, 3, 4, 5, 6]).getTime()",
        1_577_934_245_006.0,
    );
    assert_number(
        "class D extends Date { constructor(...a) { super(...a) } } \
         new D(...[2020, 0, 2, 3, 4, 5, 6]).getTime()",
        1_577_934_245_006.0,
    );
}
