//! Integration tests: `var` hoisting to the function / script var scope
//! (DESIGN.md §7.1) and top-level `var` / function declarations as
//! global-object properties (DESIGN.md §6.4).

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught,
};

#[test]
fn function_var_is_hoisted_to_function_scope() {
    assert_eval("function h(){ return v; var v = 1 } h()", "undefined");
    assert_string(
        "function g(){ var x = typeof y; { var y = 1 } return x } g()",
        "undefined",
    );
    assert_number("function f(){ { var x = 1 } return x } f()", 1.0);
    assert_number(
        "function f(){ if (true) { var x = 2 } else { var x = 3 } return x } f()",
        2.0,
    );
    assert_number(
        "function f(){ for (var i = 0; i < 3; i++) { var r = i } return r + i } f()",
        5.0,
    );
    assert_number(
        "function f(){ try { var a = 1 } finally { var b = 2 } return a + b } f()",
        3.0,
    );
    assert_number(
        "function f(){ try { throw 0 } catch (e) { var c = 4 } return c } f()",
        4.0,
    );
    assert_number(
        "function f(x){ switch (x) { case 1: var s = 10; break; default: var s = 20 } return s } f(1) + f(2)",
        30.0,
    );
    assert_number(
        "function f(){ l: for (;;) { var q = 6; break l } return q } f()",
        6.0,
    );
    assert_number(
        "function f(){ var n = 0; while (n < 2) { var w = n; n++ } return w } f()",
        1.0,
    );
    // A `var` without an initializer never resets the binding.
    assert_number(
        "function f(){ var t = 0; for (let i = 0; i < 3; i++) { var t; t = t + 1 } return t } f()",
        3.0,
    );
    // `let` in a block still shadows.
    assert_number("function f(){ var x = 1; { let x = 2 } return x } f()", 1.0);
}

#[test]
fn for_in_of_var_heads_bind_the_function_var() {
    assert_number("function f(a){ for (var a of [1,2]); return a } f(0)", 2.0);
    assert_string(
        "function f(){ for (var k in {p: 1, q: 2}); return k } f()",
        "q",
    );
    assert_number(
        "function f(){ for (var [x, y] of [[1, 2], [3, 4]]); return x + y } f()",
        7.0,
    );
    assert_eval(
        "function f(){ var r = k; for (var k of [1]); return r } f()",
        "undefined",
    );
}

#[test]
fn var_redeclaring_a_parameter_keeps_its_value() {
    assert_number("function f(a){ var a; return a } f(5)", 5.0);
    assert_number("function f(a){ { var a } return a } f(5)", 5.0);
    assert_number("function f(a){ var a = a + 1; return a } f(5)", 6.0);
}

#[test]
fn catch_parameter_var_assigns_the_catch_binding() {
    assert_number(
        "function f(){ var r; try { throw 1 } catch (e) { var e = 2; r = e } return r } f()",
        2.0,
    );
    // The function-level `e` exists and stays undefined.
    assert_eval(
        "function f(){ try { throw 1 } catch (e) { var e = 2 } return e } f()",
        "undefined",
    );
}

#[test]
fn closures_share_one_hoisted_var() {
    assert_number(
        "function f(){ var fs = []; for (var i = 0; i < 3; i++) { fs.push(() => i) } return fs[0]() + fs[2]() } f()",
        6.0,
    );
    assert_number(
        "function f(){ var g = () => late; var late = 7; return g() } f()",
        7.0,
    );
    assert_number(
        "function f(){ { var x = 1 } var get = () => x; x = 9; return get() } f()",
        9.0,
    );
}

#[test]
fn generators_and_async_hoist_vars() {
    assert_eval(
        "function* g(){ var a = b; yield a; { var b = 2 } yield b } var it = g(); [it.next().value, it.next().value]",
        "[ undefined, 2 ]",
    );
    assert_number(
        "function* g(p){ for (var p of [3, 4]) yield p; return p } var it = g(1); it.next(); it.next(); it.next().value",
        4.0,
    );
    assert_output(
        "async function f(){ var a = await 1; { var b = a + 1 } return b } f().then(v => console.log(v))",
        "2\n",
    );
}

#[test]
fn top_level_var_is_hoisted() {
    assert_number("for (var i = 0; i < 2; i++) { var r = i } r", 1.0);
    assert_eval("var u0 = z0; var z0 = 1; u0", "undefined");
    assert_string("var b0 = typeof q0; { var q0 = 1 } b0", "undefined");
    assert_number("try { var t0 = 3 } catch (e) {} t0", 3.0);
    assert_number("switch (1) { case 1: var s0 = 4 } s0", 4.0);
    assert_eval(
        "function f(){ return late } var r = f(); var late = 1; r",
        "undefined",
    );
    assert_eval(
        "const g = () => late; var r = g(); var late = 1; r",
        "undefined",
    );
}

#[test]
fn top_level_declarations_are_global_properties() {
    assert_bool("var q = 3; this.q === 3", true);
    assert_bool(
        "function ff(){} Object.prototype.hasOwnProperty.call(globalThis, 'ff')",
        true,
    );
    assert_bool("function ff(){} globalThis.ff === ff", true);
    assert_number("var q = 1; globalThis.q = 2; q", 2.0);
    assert_number("var q; function s(){ q = 5 } s(); globalThis.q", 5.0);
    assert_number("var q; function s(){ globalThis.q = 6 } s(); q", 6.0);
    assert_bool(
        "globalThis.hasOwnProperty('later'); var later; 'later' in globalThis",
        true,
    );
    assert_bool(
        "var before = globalThis.hasOwnProperty('later'); var later = 1; before",
        true,
    );
    assert_eval("var v; globalThis.v", "undefined");
    assert_string(
        "var v = 1; var d = Object.getOwnPropertyDescriptor(globalThis, 'v'); [d.value, d.writable, d.enumerable, d.configurable].join()",
        "1,true,true,false",
    );
    assert_string(
        "function fn(){} var d = Object.getOwnPropertyDescriptor(globalThis, 'fn'); [d.value === fn, d.writable, d.enumerable, d.configurable].join()",
        "true,true,true,false",
    );
    // A non-configurable global property cannot be deleted.
    assert_uncaught(
        "var v = 1; delete globalThis.v",
        "TypeError: Cannot delete property 'v' of object",
    );
    // An existing global property is not redefined by `var`.
    assert_bool(
        "var Math; Math === globalThis.Math && typeof Math.abs === 'function'",
        true,
    );
    // A function reassigned through the global object is called through it.
    assert_number("function f(){ return 1 } globalThis.f = () => 2; f()", 2.0);
    assert_number("function f(){ return 1 } f = () => 3; globalThis.f()", 3.0);
}

#[test]
fn global_declarations_shadowing_intrinsics() {
    assert_number(
        "var parseInt = (s) => 42; globalThis.x = 0; parseInt('7')",
        42.0,
    );
    assert_number(
        "function parseInt(){ return 43 } globalThis.parseInt('7')",
        43.0,
    );
    assert_bool("var Array = 1; globalThis.Array === 1", true);
    assert_number("var String = (v) => 5; this; String(1)", 5.0);
    assert_number("var f = 1; function f(){} f", 1.0);
    assert_string("function f(){} var f; typeof f", "function");
    assert_uncaught(
        "function NaN(){} this",
        "TypeError: Cannot redefine property: NaN",
    );
}

#[test]
fn var_statement_has_no_completion_value() {
    assert_number("1; var a = 2", 1.0);
    assert_number("var g = globalThis; 1; var a = 2", 1.0);
    assert_number("1; for (var k in {p: 1});", 1.0);
}
