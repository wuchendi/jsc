//! Integration tests: the global object, global bindings resolved at run time
//! (design 4.3), and the `Math`/`JSON`/`console` namespace objects reached
//! through their values.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string, assert_uncaught};

#[test]
fn typeof_unresolvable_is_undefined() {
    assert_string("typeof notDeclaredAnywhere", "undefined");
    assert_string("function f() { return typeof nope; } f()", "undefined");
    assert_string("typeof Math", "object");
    assert_string("typeof console.log", "function");
}

#[test]
fn reading_unresolvable_throws_reference_error_at_run_time() {
    assert_bool(
        "let ok = false; try { nope; } catch (e) { ok = e instanceof ReferenceError && e.message === 'nope is not defined'; } ok",
        true,
    );
    // Only at run time: an unexecuted branch does not throw.
    assert_number("let x = 1; if (x === 2) { nope; } x", 1.0);
    assert_uncaught("nope + 1", "ReferenceError: nope is not defined");
    assert_uncaught("new Widget(1)", "ReferenceError: Widget is not defined");
    assert_uncaught(
        "function f() { return nope(); } f()",
        "ReferenceError: nope is not defined",
    );
}

#[test]
fn assigning_unresolvable_throws_reference_error() {
    assert_bool(
        "let ok = false; try { x = 1; } catch (e) { ok = e instanceof ReferenceError && e.message === 'x is not defined'; } ok",
        true,
    );
    assert_uncaught("y += 1", "ReferenceError: y is not defined");
    assert_uncaught("z++", "ReferenceError: z is not defined");
}

#[test]
fn global_object_identity_and_top_level_this() {
    assert_bool("globalThis.Math === Math", true);
    assert_bool("this === globalThis", true);
    assert_bool("globalThis.globalThis === globalThis", true);
    assert_bool(
        "globalThis.JSON === JSON && globalThis.console === console",
        true,
    );
    // Function-level `this` is unchanged.
    assert_bool("function f() { return this; } f() === undefined", true);
}

#[test]
fn global_value_properties() {
    assert_bool("globalThis.undefined === undefined", true);
    assert_bool("globalThis.NaN !== globalThis.NaN", true);
    assert_number("globalThis.Infinity", f64::INFINITY);
    assert_uncaught(
        "globalThis.NaN = 1",
        "TypeError: Cannot assign to read only property 'NaN' of object",
    );
}

#[test]
fn runtime_global_properties() {
    assert_number("globalThis.foo = 1; foo", 1.0);
    assert_number("globalThis.foo = 1; foo = 2; globalThis.foo", 2.0);
    assert_number("globalThis.n = 1; n++; n += 2; n", 4.0);
    assert_number("globalThis.f = function (a) { return a * 2; }; f(21)", 42.0);
}

#[test]
fn guarded_namespace_use() {
    assert_string(
        r#"const v = [1, "a"]; typeof JSON !== "undefined" ? JSON.stringify(v) : "x""#,
        r#"[1,"a"]"#,
    );
}

#[test]
fn namespace_through_value() {
    assert_number("let m = Math; m.max(1, 2)", 2.0);
    assert_number("let m = Math; m.min(4, 3, 5)", 3.0);
    assert_number("let m = Math; m.max()", f64::NEG_INFINITY);
    assert_number("let m = Math; m.floor(2.7) + m.ceil(2.1)", 5.0);
    assert_number("let m = Math; m.pow(2, 10)", 1024.0);
    assert_number(
        "let m = Math; m.abs(-3) + m.sqrt(16) + m.trunc(-1.5) + m.round(2.5)",
        9.0,
    );
    assert_number("let m = Math; m.PI", std::f64::consts::PI);
    assert_number("const f = Math.max; f(7, 9, 8)", 9.0);
    assert_string(
        "let j = JSON; j.stringify({ a: [1, true] })",
        r#"{"a":[1,true]}"#,
    );
    assert_number("let j = JSON; j.parse('[1, 2]')[1]", 2.0);
    assert_output("let c = console; c.log(1, 'two', [3])", "1 two [ 3 ]\n");
    assert_output("const log = console.log; log('x')", "x\n");
    assert_number("Math.max.length + Math.floor.length", 3.0);
    assert_string("Math.max.name", "max");
}

#[test]
fn namespace_through_global_object_matches_inline() {
    assert_number("globalThis.Math.max(1, 5, 3)", 5.0);
    assert_bool(
        "Math.floor(-2.5) === globalThis.Math.floor(-2.5) && Math.round(-2.5) === globalThis.Math.round(-2.5)",
        true,
    );
    assert_string(
        "globalThis.JSON.stringify({ k: 'v' }) + JSON.stringify({ k: 'v' })",
        r#"{"k":"v"}{"k":"v"}"#,
    );
    assert_output(
        "globalThis.console.log('a', 1); console.log('a', 1)",
        "a 1\na 1\n",
    );
    // A thrown parse error crosses the builtin call.
    assert_bool(
        "let ok = false; try { globalThis.JSON.parse('{'); } catch (e) { ok = e instanceof SyntaxError; } ok",
        true,
    );
}

#[test]
fn reassigned_namespace_updates_the_global() {
    assert_number("Math = { max() { return 7; } }; Math.max(1, 2)", 7.0);
    assert_number("Math = 3; globalThis.Math", 3.0);
}

#[test]
fn shadowed_namespace_uses_the_binding() {
    assert_number(
        "{ let Math = { max() { return 7; } }; Math.max(1, 2) }",
        7.0,
    );
    assert_number(
        "let r; { let Math = { max() { return 7; } }; r = Math.max(1, 2); } r + Math.max(1, 2)",
        9.0,
    );
}

#[test]
fn rendering_global_objects_is_sane() {
    assert_output("console.log(Math)", "{}\n");
    assert_output("console.log(typeof globalThis)", "object\n");
    assert_output("console.log(globalThis)", "{}\n");
    assert_output(
        "globalThis.self = globalThis; globalThis.k = 1; console.log(globalThis)",
        "<ref *1> { self: [Circular *1], k: 1 }\n",
    );
}
