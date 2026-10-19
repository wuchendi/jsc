//! Member access / call / assignment on a `null` or `undefined` receiver throws
//! a branded `TypeError` with V8's message text; optional chaining still
//! short-circuits to `undefined`.

mod common;

use common::{assert_bool, assert_eval, assert_number, assert_str, assert_uncaught};

#[test]
fn literal_receiver_read_throws() {
    assert_uncaught(
        "null.x",
        "TypeError: Cannot read properties of null (reading 'x')",
    );
    assert_uncaught(
        "undefined.x",
        "TypeError: Cannot read properties of undefined (reading 'x')",
    );
}

#[test]
fn variable_receiver_read_throws() {
    assert_uncaught(
        "let n = null; n.x",
        "TypeError: Cannot read properties of null (reading 'x')",
    );
}

#[test]
fn chained_read_throws() {
    assert_uncaught(
        "let x = {}; x.y.z",
        "TypeError: Cannot read properties of undefined (reading 'z')",
    );
}

#[test]
fn computed_read_uses_key_string() {
    assert_uncaught(
        "let u; u[0]",
        "TypeError: Cannot read properties of undefined (reading '0')",
    );
    assert_uncaught(
        "let n = null; let k = \"ab\"; n[k]",
        "TypeError: Cannot read properties of null (reading 'ab')",
    );
}

#[test]
fn method_call_on_nullish_throws() {
    assert_uncaught(
        "let n = null; n.f()",
        "TypeError: Cannot read properties of null (reading 'f')",
    );
    assert_uncaught(
        "null.f()",
        "TypeError: Cannot read properties of null (reading 'f')",
    );
    assert_uncaught(
        "undefined.f()",
        "TypeError: Cannot read properties of undefined (reading 'f')",
    );
    assert_uncaught(
        "let x = {}; x.y.f()",
        "TypeError: Cannot read properties of undefined (reading 'f')",
    );
}

#[test]
fn method_call_receiver_checked_before_arguments() {
    assert_str(
        "let log = \"\"; function g() { log = log + \"g\"; return 1; } \
         let n = null; try { n.f(g()) } catch (e) { log = log + \"c\" } log",
        "c",
    );
}

#[test]
fn assignment_on_nullish_throws() {
    assert_uncaught(
        "let n = null; n.x = 1",
        "TypeError: Cannot set properties of null (setting 'x')",
    );
    assert_uncaught(
        "let x = {}; x.y.z = 1",
        "TypeError: Cannot set properties of undefined (setting 'z')",
    );
}

#[test]
fn compound_assignment_on_nullish_throws() {
    assert_uncaught(
        "let x = {}; x.y.z += 1",
        "TypeError: Cannot read properties of undefined (reading 'z')",
    );
}

#[test]
fn thrown_error_is_catchable_type_error() {
    assert_bool(
        "let r = false; try { null.x } catch (e) { r = e instanceof TypeError } r",
        true,
    );
    assert_str(
        "let r = \"\"; try { let x = {}; x.y.z } catch (e) { r = e.message } r",
        "Cannot read properties of undefined (reading 'z')",
    );
    assert_str(
        "let r = \"\"; try { null.x } catch (e) { r = e.name } r",
        "TypeError",
    );
    assert_str(
        "let r = \"\"; try { let n = null; n.x = 1 } catch (e) { r = e.stack } r",
        "TypeError: Cannot set properties of null (setting 'x')",
    );
}

#[test]
fn throw_propagates_through_call_frames() {
    assert_uncaught(
        "function f(o) { return o.a; } function g() { return f(undefined); } g()",
        "TypeError: Cannot read properties of undefined (reading 'a')",
    );
    assert_str(
        "function f(o) { return o.a; } let r = \"\"; try { f(null) } catch (e) { r = e.message } r",
        "Cannot read properties of null (reading 'a')",
    );
}

#[test]
fn optional_chaining_short_circuits() {
    assert_eval("null?.x", "undefined");
    assert_eval("let x = {}; x.y?.z", "undefined");
    assert_eval("let x = {}; x.y?.z.w", "undefined");
    assert_eval("let n = null; n?.f()", "undefined");
    assert_eval("let u; u?.[0]", "undefined");
}

#[test]
fn non_nullish_receivers_unchanged() {
    assert_eval("let o = {}; o.missing", "undefined");
    assert_number("let a = [1, 2]; a[1]", 2.0);
    assert_str("let s = \"hi\"; s[0]", "h");
    assert_number("let m = new Map(); m.set(1, 2); m.size", 1.0);
    assert_number("let o = { a: 1 }; o.a += 2; o.a", 3.0);
}

#[test]
fn member_update_expressions() {
    assert_number("let o = { p: 1 }; o.p++", 1.0);
    assert_number("let o = { p: 1 }; o.p++; o.p", 2.0);
    assert_number("let o = { p: 2 }; ++o.p", 3.0);
    assert_number("let o = { p: 2 }; ++o.p; o.p", 3.0);
    assert_number("let o = { p: 5 }; o['p']--", 5.0);
    assert_number("let o = { p: 5 }; o['p']--; o.p", 4.0);
    assert_number("let o = { p: 5 }; --o.p", 4.0);
    assert_number("let a = [7, 8]; a[0]++; a[0]", 8.0);
    assert_number("let a = [7, 8]; let k = 1; a[k]--", 8.0);
    assert_number("let a = [7, 8]; let k = 1; a[k]--; a[1]", 7.0);
    assert_number("let o = { p: \"4\" }; o.p++; o.p", 5.0);
    assert_number("let o = { p: \"4\" }; o.p++", 4.0);
    assert_number(
        "let i = 0; let o = { p: 1 }; function k() { i = i + 1; return \"p\"; } o[k()]++; i",
        1.0,
    );
}

#[test]
fn member_update_on_nullish_throws_read_error() {
    assert_uncaught(
        "let x = {}; x.y.z++",
        "TypeError: Cannot read properties of undefined (reading 'z')",
    );
    assert_uncaught(
        "let n = null; --n.p",
        "TypeError: Cannot read properties of null (reading 'p')",
    );
}

#[test]
fn length_and_builtin_methods_on_nullish_throw() {
    assert_uncaught(
        "let n = null; n.length",
        "TypeError: Cannot read properties of null (reading 'length')",
    );
    assert_uncaught(
        "let x = {}; x.y.length",
        "TypeError: Cannot read properties of undefined (reading 'length')",
    );
    assert_uncaught(
        "let u; u.push(1)",
        "TypeError: Cannot read properties of undefined (reading 'push')",
    );
    assert_uncaught(
        "let n = null; n.toUpperCase()",
        "TypeError: Cannot read properties of null (reading 'toUpperCase')",
    );
    assert_uncaught(
        "let n = null; n.map((v) => v)",
        "TypeError: Cannot read properties of null (reading 'map')",
    );
    assert_uncaught(
        "let n = null; n.get(1)",
        "TypeError: Cannot read properties of null (reading 'get')",
    );
    assert_uncaught(
        "let n = null; n.getTime()",
        "TypeError: Cannot read properties of null (reading 'getTime')",
    );
    assert_bool(
        "let r = false; try { let n = null; n.length } catch (e) { r = e instanceof TypeError } r",
        true,
    );
}
