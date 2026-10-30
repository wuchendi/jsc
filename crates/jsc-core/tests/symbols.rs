//! Integration tests: the Symbol primitive (`design/symbol-strings.md` §2):
//! `typeof`, Node-style rendering of symbols, symbol-keyed properties and
//! uncaught symbols.

mod common;
use common::{assert_bool, assert_eval, assert_number, assert_string, assert_uncaught};

// --- typeof ----------------------------------------------------------------------

#[test]
fn typeof_symbol_is_symbol() {
    assert_string("typeof Symbol()", "symbol");
    assert_string("typeof Symbol(\"a\")", "symbol");
    assert_string("let s = Symbol(\"a\"); typeof s", "symbol");
}

// --- rendering -------------------------------------------------------------------

#[test]
fn symbol_renders_with_its_description() {
    assert_eval("Symbol(\"a\")", "Symbol(a)");
}

#[test]
fn symbol_without_description_renders_empty() {
    assert_eval("Symbol()", "Symbol()");
}

#[test]
fn symbol_inside_array_renders_node_style() {
    assert_eval("[Symbol(\"a\")]", "[ Symbol(a) ]");
}

/// Symbol keys render after the string keys, whatever their insertion order.
#[test]
fn symbol_keys_render_last() {
    assert_eval("({ a: 1, [Symbol(\"b\")]: 2 })", "{ a: 1, [Symbol(b)]: 2 }");
    assert_eval(
        "let o = {}; o[Symbol(\"b\")] = 2; o.a = 1; o",
        "{ a: 1, [Symbol(b)]: 2 }",
    );
}

// --- uncaught --------------------------------------------------------------------

#[test]
fn uncaught_symbol_renders_its_description() {
    assert_uncaught("throw Symbol(\"a\")", "Symbol(a)");
}

// --- Symbol(), Symbol.for, Symbol.keyFor ------------------------------------------

#[test]
fn every_symbol_call_is_a_fresh_identity() {
    assert_bool("Symbol(\"a\") !== Symbol(\"a\")", true);
    assert_bool("const s = Symbol(\"a\"); s === s", true);
    assert_bool("Symbol() !== Symbol()", true);
}

#[test]
fn description_getter() {
    assert_string("Symbol(\"a\").description", "a");
    assert_bool("Symbol().description === undefined", true);
    assert_bool("Symbol(undefined).description === undefined", true);
    // `ToString` of a non-string description.
    assert_string("Symbol(12).description", "12");
    assert_string("Symbol(\"\").description", "");
}

#[test]
fn registry_for_and_key_for() {
    assert_bool("Symbol.for(\"k\") === Symbol.for(\"k\")", true);
    assert_bool("Symbol.for(\"k\") !== Symbol(\"k\")", true);
    assert_bool("Symbol.for(\"k\") !== Symbol.for(\"j\")", true);
    assert_string("Symbol.keyFor(Symbol.for(\"k\"))", "k");
    assert_string("Symbol.for(\"k\").description", "k");
    // The key is `ToString(key)`.
    assert_bool("Symbol.for(1) === Symbol.for(\"1\")", true);
    assert_bool("Symbol.keyFor(Symbol()) === undefined", true);
    assert_bool("Symbol.keyFor(Symbol(\"k\")) === undefined", true);
    // Well-known symbols are not registered.
    assert_bool("Symbol.keyFor(Symbol.iterator) === undefined", true);
    assert_uncaught(
        "Symbol.keyFor(\"k\")",
        "TypeError: Symbol.keyFor argument is not a symbol",
    );
    assert_bool(
        "let ok = false; try { Symbol.keyFor({}); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
}

#[test]
fn symbol_is_not_a_constructor() {
    assert_uncaught("new Symbol()", "TypeError: Symbol is not a constructor");
    assert_bool(
        "let ok = false; try { new Symbol(\"a\"); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
}

#[test]
fn symbol_function_facts() {
    assert_number("Symbol.length", 0.0);
    assert_string("Symbol.name", "Symbol");
    assert_string("typeof Symbol", "function");
    assert_bool("Symbol.prototype.constructor === Symbol", true);
    assert_bool(
        "Object.getPrototypeOf(Symbol.prototype) === Object.prototype",
        true,
    );
    assert_number("Symbol.for.length", 1.0);
    assert_number("Symbol.keyFor.length", 1.0);
    assert_string("Symbol.for.name", "for");
    assert_string("Symbol.keyFor.name", "keyFor");
}

// --- well-known symbols ----------------------------------------------------------

/// The 13 well-known symbols, in `Symbol`'s own-property order.
const WELL_KNOWN: [&str; 13] = [
    "asyncIterator",
    "hasInstance",
    "isConcatSpreadable",
    "iterator",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];

#[test]
fn symbol_iterator_descriptor_is_all_false() {
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(Symbol, \"iterator\"); \
         String(d.writable) + String(d.enumerable) + String(d.configurable)",
        "falsefalsefalse",
    );
    assert_bool(
        "Object.getOwnPropertyDescriptor(Symbol, \"iterator\").value === Symbol.iterator",
        true,
    );
}

#[test]
fn every_well_known_symbol_is_a_constant_symbol() {
    for name in WELL_KNOWN {
        assert_string(&format!("typeof Symbol.{name}"), "symbol");
        assert_string(
            &format!("Symbol.{name}.description"),
            &format!("Symbol.{name}"),
        );
        assert_bool(&format!("Symbol.{name} === Symbol.{name}"), true);
        assert_bool(
            &format!(
                "const d = Object.getOwnPropertyDescriptor(Symbol, \"{name}\"); \
                 !d.writable && !d.enumerable && !d.configurable && d.value === Symbol.{name}"
            ),
            true,
        );
    }
    assert_bool("Symbol.iterator !== Symbol.asyncIterator", true);
    assert_bool("Symbol.iterator !== Symbol.for(\"Symbol.iterator\")", true);
}

// --- %Symbol.prototype% ----------------------------------------------------------

#[test]
fn symbol_prototype_to_string_and_value_of() {
    assert_string("Symbol(\"a\").toString()", "Symbol(a)");
    assert_string("Symbol().toString()", "Symbol()");
    assert_string("Symbol.iterator.toString()", "Symbol(Symbol.iterator)");
    assert_bool("const s = Symbol(); s.valueOf() === s", true);
    // `thisSymbolValue` unwraps a Symbol wrapper.
    assert_string(
        "Symbol.prototype.toString.call(Object(Symbol(\"w\")))",
        "Symbol(w)",
    );
    assert_bool("const s = Symbol(); Object(s).valueOf() === s", true);
}

#[test]
fn symbol_prototype_methods_require_a_symbol_receiver() {
    assert_uncaught(
        "Symbol.prototype.toString.call(1)",
        "TypeError: Symbol.prototype.toString requires that 'this' be a Symbol",
    );
    assert_uncaught(
        "Symbol.prototype.valueOf.call(\"s\")",
        "TypeError: Symbol.prototype.valueOf requires that 'this' be a Symbol",
    );
    assert_uncaught(
        "Symbol.prototype.description",
        "TypeError: Symbol.prototype.description requires that 'this' be a Symbol",
    );
    assert_bool(
        "let ok = false; try { Symbol.prototype.toString.call({}); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
}

#[test]
fn symbol_prototype_to_primitive_returns_the_primitive() {
    assert_bool(
        "const s = Symbol(); Symbol.prototype[Symbol.toPrimitive].call(s) === s",
        true,
    );
    assert_bool(
        "const s = Symbol(); Symbol.prototype[Symbol.toPrimitive].call(Object(s), \"string\") === s",
        true,
    );
    assert_string(
        "Symbol.prototype[Symbol.toPrimitive].name",
        "[Symbol.toPrimitive]",
    );
    assert_number("Symbol.prototype[Symbol.toPrimitive].length", 1.0);
    // Configurable only.
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(Symbol.prototype, Symbol.toPrimitive); \
         String(d.writable) + String(d.enumerable) + String(d.configurable)",
        "falsefalsetrue",
    );
    assert_uncaught(
        "Symbol.prototype[Symbol.toPrimitive].call(1)",
        "TypeError: Symbol.prototype [ @@toPrimitive ] requires that 'this' be a Symbol",
    );
}

#[test]
fn symbol_prototype_to_string_tag_and_description_accessor() {
    assert_string("Symbol.prototype[Symbol.toStringTag]", "Symbol");
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(Symbol.prototype, Symbol.toStringTag); \
         String(d.writable) + String(d.enumerable) + String(d.configurable)",
        "falsefalsetrue",
    );
    assert_bool(
        "const d = Object.getOwnPropertyDescriptor(Symbol.prototype, \"description\"); \
         typeof d.get === \"function\" && d.set === undefined && !d.enumerable && d.configurable",
        true,
    );
    assert_string(
        "Object.getOwnPropertyDescriptor(Symbol.prototype, \"description\").get.name",
        "get description",
    );
}
