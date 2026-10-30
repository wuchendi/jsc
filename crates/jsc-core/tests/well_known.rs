//! Integration tests: the honoured well-known symbol hooks
//! (`design/symbol-strings.md` §4): `@@toPrimitive` in `ToPrimitive`
//! (hint strings, `GetMethod` errors, the `OrdinaryToPrimitive` fallback,
//! `Date.prototype[@@toPrimitive]`) and `@@toStringTag` in
//! `Object.prototype.toString` (builtin tags as real properties, user tags).

mod common;
use common::{assert_bool, assert_number, assert_string, assert_uncaught};

// --- @@toPrimitive ---------------------------------------------------------------

#[test]
fn to_primitive_receives_the_hint() {
    let hook = "let hints = []; \
        let o = { [Symbol.toPrimitive](h) { hints.push(h); return 1 } };";
    assert_string(
        &format!("{hook} +o; `${{o}}`; o + ''; o == 1; hints.join()"),
        "number,string,default,default",
    );
    assert_string(
        &format!("{hook} String(o); o < 2; hints.join()"),
        "string,number",
    );
    assert_bool(
        "let o = { [Symbol.toPrimitive]() { return this === o } }; `${o}` === 'true'",
        true,
    );
}

#[test]
fn to_primitive_result_is_used_at_every_conversion_site() {
    let obj = "let o = { [Symbol.toPrimitive]() { return 1 } };";
    assert_number(&format!("{obj} o + 1"), 2.0);
    assert_number(&format!("{obj} +o"), 1.0);
    assert_bool(&format!("{obj} o < 2"), true);
    assert_string(&format!("{obj} `${{o}}`"), "1");
    assert_string(&format!("{obj} String(o)"), "1");
    assert_number(&format!("{obj} ({{ 1: 5 }})[o]"), 5.0);
}

#[test]
fn non_callable_to_primitive_throws() {
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]: 1 }; o + 1",
        "TypeError: Symbol.toPrimitive is not a function",
    );
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]: {} }; `${o}`",
        "TypeError: Symbol.toPrimitive is not a function",
    );
}

#[test]
fn object_result_of_to_primitive_throws() {
    assert_uncaught(
        "let o = { [Symbol.toPrimitive]() { return {} } }; o + 1",
        "TypeError: Cannot convert object to primitive value",
    );
}

#[test]
fn nullish_to_primitive_falls_back_to_ordinary() {
    assert_number(
        "let o = { [Symbol.toPrimitive]: undefined, valueOf() { return 7 } }; o + 1",
        8.0,
    );
    assert_string(
        "let o = { [Symbol.toPrimitive]: null, toString() { return 's' } }; `${o}`",
        "s",
    );
}

#[test]
fn date_to_primitive_default_hint_is_string() {
    assert_bool("new Date(0) + '' === new Date(0).toString()", true);
    assert_string("typeof (new Date(0) + 1)", "string");
    assert_number("+new Date(5)", 5.0);
    assert_number("new Date(5) - 0", 5.0);
    assert_bool(
        "typeof Date.prototype[Symbol.toPrimitive] === 'function' && \
         Date.prototype[Symbol.toPrimitive].name === '[Symbol.toPrimitive]' && \
         Date.prototype[Symbol.toPrimitive].length === 1",
        true,
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Date.prototype, Symbol.toPrimitive); \
         !d.writable && !d.enumerable && d.configurable",
        true,
    );
}

#[test]
fn date_to_primitive_is_generic_and_checks_its_hint() {
    let o = "let o = { toString() { return 's' }, valueOf() { return 1 } };";
    let call = "Date.prototype[Symbol.toPrimitive].call";
    assert_number(&format!("{o} {call}(o, 'number')"), 1.0);
    assert_string(&format!("{o} {call}(o, 'default')"), "s");
    assert_string(&format!("{o} {call}(o, 'string')"), "s");
    assert_uncaught(&format!("{o} {call}(o, 'x')"), "TypeError: Invalid hint");
    assert_uncaught(&format!("{o} {call}(o)"), "TypeError: Invalid hint");
    assert_uncaught(
        &format!("{call}(1, 'number')"),
        "TypeError: Date.prototype [ @@toPrimitive ] called on non-object",
    );
}

// --- @@toStringTag ---------------------------------------------------------------

#[test]
fn builtin_to_string_tags() {
    let tag = |value: &str, expected: &str| {
        assert_string(
            &format!("Object.prototype.toString.call({value})"),
            expected,
        );
    };
    tag("Math", "[object Math]");
    tag("JSON", "[object JSON]");
    tag("new Map()", "[object Map]");
    tag("new Set()", "[object Set]");
    tag("Promise.resolve(1)", "[object Promise]");
    tag("Symbol()", "[object Symbol]");
    tag("Object(Symbol())", "[object Symbol]");
    tag("(function* () {})()", "[object Generator]");
    tag("(async function* () {})()", "[object AsyncGenerator]");
    tag("[].values()", "[object Array Iterator]");
    tag("'ab'[Symbol.iterator]()", "[object String Iterator]");
    tag("new Map().keys()", "[object Map Iterator]");
    tag("new Set().values()", "[object Set Iterator]");
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Map.prototype, Symbol.toStringTag); \
         d.value === 'Map' && !d.writable && !d.enumerable && d.configurable",
        true,
    );
}

#[test]
fn user_to_string_tag_wins() {
    assert_string(
        "let o = { [Symbol.toStringTag]: 'X' }; Object.prototype.toString.call(o)",
        "[object X]",
    );
    assert_string(
        "let o = { [Symbol.toStringTag]: 'X' }; String(o)",
        "[object X]",
    );
    assert_string(
        "class C { get [Symbol.toStringTag]() { return 'C' } } `${new C()}`",
        "[object C]",
    );
    assert_string(
        "class C {} Object.defineProperty(C.prototype, Symbol.toStringTag, { value: 'C' }); \
         `${new C()}`",
        "[object C]",
    );
    assert_string(
        "let a = [1]; a[Symbol.toStringTag] = 'L'; Object.prototype.toString.call(a)",
        "[object L]",
    );
}

#[test]
fn non_string_to_string_tag_is_ignored() {
    assert_string(
        "let o = { [Symbol.toStringTag]: 1 }; Object.prototype.toString.call(o)",
        "[object Object]",
    );
    assert_string(
        "let a = []; a[Symbol.toStringTag] = {}; Object.prototype.toString.call(a)",
        "[object Array]",
    );
    assert_string(
        "delete Map.prototype[Symbol.toStringTag]; Object.prototype.toString.call(new Map())",
        "[object Object]",
    );
}
