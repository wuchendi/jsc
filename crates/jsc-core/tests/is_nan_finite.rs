//! Integration tests: the global `isNaN` / `isFinite` functions (`ToNumber` of
//! the argument, then a NaN / finiteness test).

mod common;
use common::{assert_bool, assert_eval, assert_number, assert_string, assert_uncaught};

#[test]
fn is_nan_of_numbers() {
    assert_bool("isNaN(NaN)", true);
    assert_bool("isNaN(0)", false);
    assert_bool("isNaN(-0)", false);
    assert_bool("isNaN(Infinity)", false);
    assert_bool("isNaN(-Infinity)", false);
    assert_bool("isNaN(1e308)", false);
    assert_bool("isNaN(0 / 0)", true);
}

#[test]
fn is_finite_of_numbers() {
    assert_bool("isFinite(NaN)", false);
    assert_bool("isFinite(0)", true);
    assert_bool("isFinite(-0)", true);
    assert_bool("isFinite(Infinity)", false);
    assert_bool("isFinite(-Infinity)", false);
    assert_bool("isFinite(1e308)", true);
    assert_bool("isFinite(1e308 * 10)", false);
}

#[test]
fn strings_go_through_to_number() {
    assert_bool("isNaN('')", false);
    assert_bool("isNaN(' 12 ')", false);
    assert_bool("isNaN('abc')", true);
    assert_bool("isNaN('0x10')", false);
    assert_bool("isNaN('Infinity')", false);
    assert_bool("isFinite('')", true);
    assert_bool("isFinite(' 12 ')", true);
    assert_bool("isFinite('abc')", false);
    assert_bool("isFinite('0x10')", true);
    assert_bool("isFinite('Infinity')", false);
    assert_bool("isFinite('12')", true);
}

#[test]
fn other_primitives_and_missing_argument() {
    assert_bool("isNaN(true)", false);
    assert_bool("isNaN(false)", false);
    assert_bool("isNaN(null)", false);
    assert_bool("isNaN(undefined)", true);
    assert_bool("isNaN()", true);
    assert_bool("isFinite(true)", true);
    assert_bool("isFinite(null)", true);
    assert_bool("isFinite(undefined)", false);
    assert_bool("isFinite()", false);
}

#[test]
fn objects_use_value_of_and_to_string() {
    assert_bool("isNaN({ valueOf() { return NaN; } })", true);
    assert_bool("isNaN({ valueOf() { return 3; } })", false);
    assert_bool("isFinite({ valueOf() { return 3; } })", true);
    assert_bool("isFinite({ valueOf() { return 1 / 0; } })", false);
    assert_bool("isNaN({})", true);
    assert_bool("isNaN([])", false);
    assert_bool("isNaN([1, 2])", true);
    assert_bool("isFinite([5])", true);
}

#[test]
fn throwing_value_of_propagates() {
    assert_uncaught(
        "isNaN({ valueOf() { throw new TypeError('boom'); } })",
        "TypeError: boom",
    );
    assert_uncaught(
        "isFinite({ valueOf() { throw new RangeError('bang'); } })",
        "RangeError: bang",
    );
    assert_string(
        "let r = 'none'; try { isNaN({ valueOf() { throw 'x'; } }); r = 'no'; } catch (e) { r = e; } r",
        "x",
    );
}

#[test]
fn usable_as_values() {
    assert_eval(
        "[1, NaN, 'x', '5'].map(isNaN)",
        "[ false, true, true, false ]",
    );
    assert_eval(
        "[1, Infinity, '5', null].map(isFinite)",
        "[ true, false, true, true ]",
    );
    assert_bool("const f = isNaN; f('q')", true);
}

#[test]
fn function_facts_and_global_identity() {
    assert_string("typeof isNaN", "function");
    assert_string("typeof isFinite", "function");
    assert_number("isNaN.length", 1.0);
    assert_number("isFinite.length", 1.0);
    assert_string("isNaN.name", "isNaN");
    assert_string("isFinite.name", "isFinite");
    assert_bool("globalThis.isNaN === isNaN", true);
    assert_bool("globalThis.isFinite === isFinite", true);
    assert_bool("isNaN.call(null, 'a')", true);
    assert_uncaught("new isNaN()", "TypeError: isNaN is not a constructor");
}

#[test]
fn user_bindings_shadow() {
    assert_number("function isNaN() { return 7; } isNaN(NaN)", 7.0);
    assert_number("let isFinite = (x) => 8; isFinite(1)", 8.0);
}
