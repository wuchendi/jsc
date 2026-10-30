//! Integration tests: the Symbol primitive (`design/symbol-strings.md` §2):
//! `typeof`, Node-style rendering of symbols, symbol-keyed properties and
//! uncaught symbols.

mod common;
use common::{assert_eval, assert_string, assert_uncaught};

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
