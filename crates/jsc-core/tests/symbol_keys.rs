//! Integration tests: symbol-keyed properties (`design/symbol-strings.md` §3):
//! `ToPropertyKey(symbol)`, the own-key modes (string walks skip symbols,
//! `getOwnPropertySymbols`, `OrdinaryOwnPropertyKeys` order), the copying
//! statics and object rest, descriptors and integrity on a symbol key, and
//! the run-time `name` of a function under a computed key.

mod common;
use common::{assert_bool, assert_number, assert_string};

// --- get / set / has / delete ----------------------------------------------------

#[test]
fn symbol_key_reads_back_what_was_written() {
    assert_number("let s = Symbol('k'); let o = {}; o[s] = 1; o[s]", 1.0);
    assert_number("let s = Symbol('k'); let o = { [s]: 2 }; o[s]", 2.0);
    assert_number(
        "let s = Symbol(); let o = {}; o[s] = 1; o[s] += 2; o[s]",
        3.0,
    );
}

#[test]
fn symbol_key_is_not_its_description_string() {
    assert_bool(
        "let s = Symbol('k'); let o = {}; o[s] = 1; o['k'] === undefined && o['Symbol(k)'] === undefined",
        true,
    );
    assert_bool(
        "let o = {}; o[Symbol('k')] = 1; o[Symbol('k')] === undefined",
        true,
    );
}

#[test]
fn symbol_wrapper_key_is_its_symbol() {
    assert_number(
        "let s = Symbol('k'); let o = {}; o[Object(s)] = 4; o[s]",
        4.0,
    );
}

#[test]
fn in_operator_sees_symbol_keys() {
    assert_bool("let s = Symbol('k'); let o = { [s]: 1 }; s in o", true);
    assert_bool("let s = Symbol('k'); let o = {}; s in o", false);
    assert_bool(
        "let s = Symbol('k'); let p = { [s]: 1 }; let o = Object.create(p); s in o",
        true,
    );
}

#[test]
fn delete_removes_a_symbol_key() {
    assert_bool(
        "let s = Symbol('k'); let o = { [s]: 1 }; delete o[s] && !(s in o) && o[s] === undefined",
        true,
    );
}

// --- string-key walks skip symbols ------------------------------------------------

#[test]
fn object_keys_skips_symbol_keys() {
    assert_string(
        "let o = { a: 1, [Symbol('s')]: 2, b: 3 }; Object.keys(o).join()",
        "a,b",
    );
    assert_number("Object.values({ [Symbol('s')]: 2 }).length", 0.0);
    assert_number("Object.entries({ [Symbol('s')]: 2 }).length", 0.0);
}

#[test]
fn for_in_skips_symbol_keys() {
    assert_string(
        "let o = { a: 1, [Symbol('s')]: 2 }; let ks = []; for (let k in o) ks.push(k); ks.join()",
        "a",
    );
}

#[test]
fn json_stringify_skips_symbol_keys() {
    assert_string("JSON.stringify({ a: 1, [Symbol('s')]: 2 })", "{\"a\":1}");
}

#[test]
fn get_own_property_names_skips_symbol_keys() {
    assert_string(
        "Object.getOwnPropertyNames({ a: 1, [Symbol('s')]: 2 }).join()",
        "a",
    );
    // `%Symbol.prototype%` owns `[@@toPrimitive]` and `[@@toStringTag]`.
    assert_bool(
        "Object.getOwnPropertyNames(Symbol.prototype).every(k => typeof k === 'string')",
        true,
    );
}

// --- getOwnPropertySymbols / own-key order ---------------------------------------

#[test]
fn get_own_property_symbols_lists_symbols_in_creation_order() {
    assert_bool(
        "let a = Symbol('a'), b = Symbol('b'); let o = { x: 1, [b]: 2 }; o[a] = 3; o.y = 4; \
         let ss = Object.getOwnPropertySymbols(o); ss.length === 2 && ss[0] === b && ss[1] === a",
        true,
    );
    assert_number("Object.getOwnPropertySymbols({ a: 1 }).length", 0.0);
    assert_number("Object.getOwnPropertySymbols('ab').length", 0.0);
    assert_number("Object.getOwnPropertySymbols.length", 1.0);
    assert_string("Object.getOwnPropertySymbols.name", "getOwnPropertySymbols");
}

#[test]
fn get_own_property_symbols_includes_non_enumerable_symbols() {
    assert_bool(
        "let s = Symbol('h'); let o = {}; Object.defineProperty(o, s, { value: 1 }); \
         Object.getOwnPropertySymbols(o)[0] === s",
        true,
    );
    assert_number("Object.getOwnPropertySymbols(Symbol.prototype).length", 2.0);
}

#[test]
fn get_own_property_descriptors_lists_strings_before_symbols() {
    assert_bool(
        "let s = Symbol('s'); let o = { [s]: 1, b: 2 }; o[1] = 3; \
         let d = Object.getOwnPropertyDescriptors(o); \
         Object.keys(d).join() === '1,b' && Object.getOwnPropertySymbols(d)[0] === s && d[s].value === 1",
        true,
    );
}

// --- copying: Object.assign / object rest / defineProperties -----------------------

#[test]
fn object_assign_copies_enumerable_symbol_keys() {
    assert_bool(
        "let s = Symbol('s'), h = Symbol('h'); let src = { [s]: 1 }; \
         Object.defineProperty(src, h, { value: 2, enumerable: false }); \
         let t = Object.assign({}, src); t[s] === 1 && !(h in t)",
        true,
    );
}

#[test]
fn object_rest_copies_enumerable_symbol_keys() {
    assert_bool(
        "let s = Symbol('s'), h = Symbol('h'); let o = { a: 1, [s]: 2 }; \
         Object.defineProperty(o, h, { value: 3, enumerable: false }); \
         let { a, ...r } = o; a === 1 && r[s] === 2 && !(h in r) && !('a' in r)",
        true,
    );
}

#[test]
fn define_properties_takes_symbol_keys() {
    assert_bool(
        "let s = Symbol('s'); let o = Object.defineProperties({}, { [s]: { value: 5, enumerable: true } }); \
         o[s] === 5",
        true,
    );
    assert_bool(
        "let s = Symbol('s'); let o = Object.create({}, { [s]: { value: 6 } }); o[s] === 6",
        true,
    );
}

// --- descriptors and integrity ---------------------------------------------------

#[test]
fn define_property_with_a_symbol_key() {
    assert_bool(
        "let s = Symbol('s'); let o = {}; Object.defineProperty(o, s, { value: 1 }); \
         let d = Object.getOwnPropertyDescriptor(o, s); \
         d.value === 1 && !d.writable && !d.enumerable && !d.configurable",
        true,
    );
    assert_bool(
        "let s = Symbol('s'); let o = { [s]: 1 }; o.hasOwnProperty(s) && Object.hasOwn(o, s) && o.propertyIsEnumerable(s)",
        true,
    );
}

#[test]
fn freeze_covers_symbol_keys() {
    assert_bool(
        "let s = Symbol('s'); let o = { [s]: 1 }; Object.freeze(o); \
         let d = Object.getOwnPropertyDescriptor(o, s); Object.isFrozen(o) && !d.writable && !d.configurable",
        true,
    );
    assert_bool(
        "'use strict'; let s = Symbol('s'); let o = Object.freeze({ [s]: 1 }); \
         try { o[s] = 2; false } catch (e) { e instanceof TypeError && o[s] === 1 }",
        true,
    );
}

// --- run-time SetFunctionName for computed keys -----------------------------------

#[test]
fn computed_symbol_method_is_named_by_its_description() {
    assert_string("let s = Symbol('d'); ({ [s]() {} })[s].name", "[d]");
    assert_string(
        "let s = Symbol('d'); ({ [s]: function () {} })[s].name",
        "[d]",
    );
    assert_string("let s = Symbol('d'); ({ [s]: () => 1 })[s].name", "[d]");
    assert_string("let s = Symbol('d'); ({ *[s]() {} })[s].name", "[d]");
}

#[test]
fn computed_symbol_accessors_are_prefixed() {
    assert_string(
        "let s = Symbol('d'); Object.getOwnPropertyDescriptor({ get [s]() { return 1 } }, s).get.name",
        "get [d]",
    );
    assert_string(
        "let s = Symbol('d'); Object.getOwnPropertyDescriptor({ set [s](v) {} }, s).set.name",
        "set [d]",
    );
}

#[test]
fn symbol_without_description_names_empty() {
    assert_string("let s = Symbol(); ({ [s]() {} })[s].name", "");
    assert_string(
        "let s = Symbol(); Object.getOwnPropertyDescriptor({ get [s]() { return 1 } }, s).get.name",
        "get ",
    );
}

#[test]
fn computed_string_key_names_its_function() {
    assert_string("let k = 'm'; ({ [k + 'x']() {} }).mx.name", "mx");
    assert_string("({ [1 + 1]: function () {} })[2].name", "2");
    assert_string("({ 3() {} })[3].name", "3");
    assert_string(
        "let k = 'g'; Object.getOwnPropertyDescriptor({ get [k]() { return 1 } }, 'g').get.name",
        "get g",
    );
    // A named function expression keeps its own name.
    assert_string("let k = 'm'; ({ [k]: function own() {} }).m.name", "own");
}

#[test]
fn computed_key_is_converted_once_before_the_value() {
    assert_string(
        "let log = []; let k = { toString() { log.push('k'); return 'm' } }; \
         let o = { [k]: (log.push('v'), function () {}) }; \
         let f = { [k]() {} }; log.join() + ':' + f.m.name",
        "k,v,k:m",
    );
}

// --- JSON.parse ToString -----------------------------------------------------------

#[test]
fn json_parse_takes_to_string_of_its_text() {
    assert_bool(
        "try { JSON.parse(Symbol('x')); false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_number("JSON.parse({ toString() { return '[7]' } })[0]", 7.0);
}
