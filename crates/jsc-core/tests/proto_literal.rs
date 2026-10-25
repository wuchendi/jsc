//! Integration tests: `__proto__: v` in an object literal sets the new
//! object's `[[Prototype]]` (ECMA-262 B.3.1) instead of defining an own
//! property (DESIGN.md §7.5).

mod common;
use common::{assert_bool, assert_eval, assert_string};

#[test]
fn proto_entry_sets_the_prototype_without_an_own_property() {
    assert_eval(
        "var o = {__proto__: Array.prototype}; \
         [Object.getPrototypeOf(o) === Array.prototype, o.hasOwnProperty(\"__proto__\")]",
        "[ true, false ]",
    );
    assert_bool(
        "var p = {x: 1}; var o = {\"__proto__\": p, y: 2}; \
         Object.getPrototypeOf(o) === p && o.x === 1 && Object.keys(o).join() === \"y\"",
        true,
    );
    assert_bool(
        "var o = {__proto__: null}; Object.getPrototypeOf(o) === null",
        true,
    );
}

#[test]
fn a_non_object_proto_value_is_ignored() {
    assert_bool(
        "var a = {__proto__: 1}, b = {__proto__: \"s\"}, c = {__proto__: undefined}; \
         [a, b, c].every(function (o) { \
           return Object.getPrototypeOf(o) === Object.prototype \
             && Object.getOwnPropertyNames(o).length === 0 })",
        true,
    );
}

#[test]
fn proto_entry_mid_literal_keeps_evaluation_order() {
    assert_string(
        "var log = []; function L(s, v) { log.push(s); return v } \
         var p = {}; \
         var o = {a: L(\"a\", 1), __proto__: L(\"p\", p), b: L(\"b\", 2), get c() { return 3 }}; \
         log.join() + \"|\" + (Object.getPrototypeOf(o) === p) + \"|\" + Object.keys(o).join() + \"|\" + o.c",
        "a,p,b|true|a,b,c|3",
    );
}

#[test]
fn computed_shorthand_and_method_proto_keys_stay_own_properties() {
    assert_bool(
        "var o = {[\"__proto__\"]: 1}; \
         o.hasOwnProperty(\"__proto__\") && Object.getPrototypeOf(o) === Object.prototype",
        true,
    );
    assert_bool(
        "var __proto__ = 5; var o = {__proto__}; \
         o.hasOwnProperty(\"__proto__\") && Object.getPrototypeOf(o) === Object.prototype",
        true,
    );
    assert_bool(
        "var o = {__proto__() { return 7 }}; \
         o.hasOwnProperty(\"__proto__\") && Object.getPrototypeOf(o) === Object.prototype",
        true,
    );
    assert_bool(
        "var o = {get __proto__() { return 8 }}; \
         o.hasOwnProperty(\"__proto__\") && Object.getPrototypeOf(o) === Object.prototype",
        true,
    );
}

#[test]
fn a_computed_proto_key_beside_a_proto_entry_is_not_a_duplicate() {
    assert_bool(
        "var p = {}; var o = {[\"__proto__\"]: 1, __proto__: p}; \
         o.hasOwnProperty(\"__proto__\") && Object.getPrototypeOf(o) === p",
        true,
    );
}
