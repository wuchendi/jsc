//! Integration tests: the Error `cause` option (`InstallErrorCause`) and the
//! syntactic builtin calls (`parseInt`, `String`, `TypeError`, ..) honouring a
//! reassigned or shadowed global.

mod common;
use common::{assert_number, assert_string};

/// The native error constructors.
const KINDS: [&str; 7] = [
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
];

/// `[own, enumerable keys, value identity]` of the `cause` of `error`, where
/// `error` may read the object `c`.
fn cause_probe(error: &str) -> String {
    format!(
        "var c = {{}}; var e = {error}; \
         [e.hasOwnProperty('cause'), Object.keys(e).length, e.cause === c].join()"
    )
}

/// `new K(m, {cause})` and `K(m, {cause})` install an own non-enumerable
/// `cause` holding the given value, for every native error constructor.
#[test]
fn cause_is_installed_for_every_native_error() {
    for kind in KINDS {
        assert_string(
            &cause_probe(&format!("new {kind}('m', {{ cause: c }})")),
            "true,0,true",
        );
        assert_string(
            &cause_probe(&format!("{kind}('m', {{ cause: c }})")),
            "true,0,true",
        );
    }
}

/// The message keeps its `ToString` semantics alongside the cause.
#[test]
fn cause_keeps_the_message() {
    assert_string(
        "var e = new TypeError(12, { cause: 1 }); e.message + '|' + e.cause + '|' + e",
        "12|1|TypeError: 12",
    );
}

/// No `cause` property when the options lack one, are a primitive, are
/// `null`, or are omitted.
#[test]
fn cause_is_absent_without_a_cause_property() {
    for args in ["'m', {}", "'m', 5", "'m', 'cause'", "'m', null", "'m'", ""] {
        for kind in ["Error", "RangeError"] {
            assert_string(
                &format!("var e = new {kind}({args}); String(e.hasOwnProperty('cause'))"),
                "false",
            );
            assert_string(
                &format!("var e = {kind}({args}); String(e.hasOwnProperty('cause'))"),
                "false",
            );
        }
    }
}

/// `cause: undefined` still installs the property.
#[test]
fn undefined_cause_is_installed() {
    assert_string(
        "var e = new Error('m', { cause: undefined }); \
         [e.hasOwnProperty('cause'), e.cause === undefined].join()",
        "true,true",
    );
}

/// `HasProperty` sees an inherited `cause`, and `Get` runs its getter after
/// the message's `ToString`.
#[test]
fn inherited_cause_getter_runs_after_the_message() {
    assert_string(
        "var log = []; \
         class O { get cause() { log.push('cause'); return 7 } } \
         var m = { toString() { log.push('message'); return 'x' } }; \
         var e = new Error(m, new O()); \
         log.join() + '|' + e.cause + '|' + e.hasOwnProperty('cause')",
        "message,cause|7|true",
    );
}

/// Arguments are evaluated left to right before the cause is read; surplus
/// arguments are still evaluated.
#[test]
fn surplus_arguments_are_evaluated() {
    assert_string(
        "var log = []; function t(v) { log.push(v); return v } \
         var e = new Error(t('m'), t({ cause: 3 }), t('extra')); \
         log.length + '|' + e.message + '|' + e.cause",
        "3|m|3",
    );
}

/// A user subclass forwarding the options to `super(m, opts)`, and one with
/// the default derived constructor.
#[test]
fn subclasses_install_the_cause() {
    assert_string(
        &cause_probe(
            "new (class E extends Error { constructor(m, o) { super(m, o) } })('m', { cause: c })",
        ),
        "true,0,true",
    );
    assert_string(
        &cause_probe("new (class F extends TypeError {})('m', { cause: c })"),
        "true,0,true",
    );
}

/// The builtin constructor reached as a value (an alias, a spread) installs
/// the cause too.
#[test]
fn constructor_values_install_the_cause() {
    assert_string(
        &cause_probe("(function (K) { return new K('m', { cause: c }) })(RangeError)"),
        "true,0,true",
    );
    assert_string(
        &cause_probe("(function (K) { return K('m', { cause: c }) })(EvalError)"),
        "true,0,true",
    );
    assert_string(
        &cause_probe("new Error(...['m', { cause: c }])"),
        "true,0,true",
    );
    assert_string(
        "var K = Error; String(new K('m', 5).hasOwnProperty('cause'))",
        "false",
    );
}

/// `parseInt` / `parseFloat` replaced on the global object are called, not
/// the builtin conversion; shadowing declarations are honoured too.
#[test]
fn replaced_parse_functions_are_honoured() {
    assert_number(
        "globalThis.parseInt = function () { return 42 }; parseInt('1')",
        42.0,
    );
    assert_number(
        "globalThis.parseFloat = function (s) { return s + 1 }; parseFloat(1)",
        2.0,
    );
    assert_number(
        "globalThis.parseInt = function () { return 42 }; parseInt(...['1'])",
        42.0,
    );
    assert_number(
        "let parseInt = function () { return 7 }; parseInt('1')",
        7.0,
    );
    assert_number("function parseFloat() { return 8 } parseFloat('1')", 8.0);
}

/// Reassigned `String` / `Number` / `Boolean` / error constructors are
/// called, not lowered syntactically.
#[test]
fn reassigned_conversions_and_errors_are_honoured() {
    assert_string("String = function () { return 's' }; String(1)", "s");
    assert_number("Number = function () { return 9 }; Number('1')", 9.0);
    assert_string("Boolean = function () { return 'b' }; Boolean(0)", "b");
    assert_number(
        "TypeError = function () { this.x = 1 }; new TypeError('x').x",
        1.0,
    );
    assert_string(
        "globalThis.RangeError = function () { return { k: 'user' } }; RangeError('x').k",
        "user",
    );
}

/// Sealed programs keep the builtin fast paths.
#[test]
fn sealed_conversions_keep_builtin_results() {
    assert_number("parseInt('12px') + parseFloat('0.5')", 12.5);
    assert_string("String(1) + Number('2') + Boolean(0)", "12false");
}
