//! Integration tests: the builtin constructors as real function values
//! (design 4.2): `typeof`/`name`/`length`/`prototype` back-links, statics read
//! as values, calling vs constructing, the Error family as `SELF_ALLOC`
//! builtins honouring `new.target` (`extends Error`), other builtin parents,
//! generic `Array.prototype.map`, and user bindings shadowing builtin names
//! (DF11).

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught,
};

/// Every global constructor with its `length`, as a JS array literal.
const CTORS: &str = "[[Object, 'Object', 1], [Function, 'Function', 1], [Array, 'Array', 1],
    [String, 'String', 1], [Number, 'Number', 1], [Boolean, 'Boolean', 1], [Error, 'Error', 1],
    [TypeError, 'TypeError', 1], [RangeError, 'RangeError', 1],
    [ReferenceError, 'ReferenceError', 1], [SyntaxError, 'SyntaxError', 1],
    [EvalError, 'EvalError', 1], [URIError, 'URIError', 1], [Map, 'Map', 0], [Set, 'Set', 0],
    [Promise, 'Promise', 1], [Date, 'Date', 7]]";

/// The Error-family constructor names, as a JS array literal.
const ERRORS: &str = "['Error', 'TypeError', 'RangeError', 'ReferenceError', 'SyntaxError',
    'EvalError', 'URIError']";

#[test]
fn constructors_are_function_values() {
    // Each check lists the constructors it fails for (`""` when none).
    let check = |test: &str| {
        assert_string(
            &format!(
                "let bad = ''; for (const e of {CTORS}) {{ const c = e[0], n = e[1], l = e[2]; if (!({test})) bad += n + ' ' }} bad"
            ),
            "",
        );
    };
    check("typeof c === 'function'");
    check("c.name === n && c.length === l");
    check("c.prototype.constructor === c");
    check("c instanceof Function && c instanceof Object");
    check("globalThis[n] === c");
    // `prototype` is not writable.
    check(
        "(() => { try { c.prototype = {}; return false } catch (e) { return e instanceof TypeError } })()",
    );
    assert_string("typeof Object", "function");
    assert_string("Map.name", "Map");
    assert_number("Date.length", 7.0);
    assert_bool("const c = Set; c === Set", true);
    assert_uncaught(
        "Array.prototype = {}",
        "TypeError: Cannot assign to read only property 'prototype' of object",
    );
}

#[test]
fn prototype_back_links_and_native_error_parents() {
    assert_bool("({}).constructor === Object", true);
    assert_bool("(function () {}).constructor === Function", true);
    assert_bool("[].constructor === Array", true);
    assert_bool("new Map().constructor === Map", true);
    assert_bool("new Set().constructor === Set", true);
    assert_bool("new Date(0).constructor === Date", true);
    assert_bool("Promise.resolve(1).constructor === Promise", true);
    assert_string("TypeError.prototype.name", "TypeError");
    assert_bool("TypeError.prototype instanceof Error", true);
    // The native error constructors inherit from `Error` (statics too).
    assert_bool("Error.x = 1; TypeError.x === 1 && RangeError.x === 1", true);
    assert_bool("Object.prototype.constructor === Object", true);
    assert_bool("Function.prototype.constructor === Function", true);
    assert_eval(
        "[TypeError, RangeError, Error].map(function (c) { return c.name })",
        "[ TypeError, RangeError, Error ]",
    );
}

#[test]
fn statics_are_function_properties() {
    assert_eval("const k = Object.keys; k({ a: 1, b: 2 })", "[ a, b ]");
    assert_eval("const v = Object.values; v({ a: 1 })", "[ 1 ]");
    assert_eval("const e = Object.entries; e({ a: 1 })", "[ [ a, 1 ] ]");
    assert_bool("const i = Array.isArray; i([]) && !i({})", true);
    assert_bool("Array.isArray([1]) && !Array.isArray('a')", true);
    assert_number("const n = Date.now; typeof n() === 'number' ? 1 : 0", 1.0);
    assert_string("Object.keys.name", "keys");
    assert_number("Promise.resolve.length", 1.0);
    assert_output(
        "const r = Promise.resolve; r(5).then(function (v) { console.log(v) })",
        "5\n",
    );
    assert_output(
        "const r = Promise.reject; r(6).catch(function (v) { console.log(v) })",
        "6\n",
    );
    // A replaced static is honoured.
    assert_number(
        "Object.keys = function () { return 7 }; Object.keys({})",
        7.0,
    );
}

#[test]
fn conversion_functions_as_values() {
    assert_string(
        r#"JSON.stringify(["1", 2, true].map(String))"#,
        r#"["1","2","true"]"#,
    );
    assert_eval(r#"["1", "x"].map(Number)"#, "[ 1, NaN ]");
    assert_eval("[0, 1].map(Boolean)", "[ false, true ]");
    assert_string("const s = String; s()", "");
    assert_number("const n = Number; n()", 0.0);
    assert_bool("const b = Boolean; b()", false);
    assert_uncaught(
        "const S = String; new S('a')",
        "TypeError: String wrapper objects are not supported",
    );
    assert_uncaught(
        "new Number(1)",
        "TypeError: Number wrapper objects are not supported",
    );
}

#[test]
fn object_function_array_called_and_constructed() {
    assert_bool("const o = {}; Object(o) === o && new Object(o) === o", true);
    assert_string("typeof Object()", "object");
    assert_eval("new Object()", "{}");
    assert_eval("Object(null)", "{}");
    assert_uncaught(
        "Object(1)",
        "TypeError: Object(primitive) wrapper objects are not supported",
    );
    assert_uncaught(
        "Function('return 1')",
        "TypeError: the Function constructor is not supported",
    );
    assert_uncaught(
        "new Function()",
        "TypeError: the Function constructor is not supported",
    );
    assert_eval("Array(3)", "[ <3 empty items> ]");
    assert_eval("new Array(2)", "[ <2 empty items> ]");
    assert_eval("Array('a')", "[ a ]");
    assert_eval("new Array(1, 2, 3)", "[ 1, 2, 3 ]");
    assert_eval("Array()", "[]");
    assert_eval("const xs = [4, 5, 6]; Array(...xs)", "[ 4, 5, 6 ]");
    assert_number("new Array(5).length", 5.0);
    assert_uncaught("new Array(-1)", "RangeError: Invalid array length");
}

#[test]
fn collections_require_new() {
    assert_uncaught(
        "const M = Map; M()",
        "TypeError: Constructor Map requires 'new'",
    );
    assert_uncaught("Set()", "TypeError: Constructor Set requires 'new'");
    assert_uncaught(
        "Promise(function () {})",
        "TypeError: Constructor Promise requires 'new'",
    );
    assert_number(
        "const M = Map; const m = new M(); m.set(1, 2); m.get(1)",
        2.0,
    );
    assert_number("const S = Set; const s = new S(); s.add(1); s.size", 1.0);
    assert_string("typeof Date()", "string");
    assert_number("const D = Date; new D(5).getTime()", 5.0);
}

#[test]
fn error_family_call_and_new() {
    let check = |test: &str| {
        assert_string(
            &format!(
                "let bad = ''; for (const n of {ERRORS}) {{ const C = globalThis[n]; if (!({test})) bad += n + ' ' }} bad"
            ),
            "",
        );
    };
    check("C('m') instanceof C && new C('m') instanceof C && new C('m') instanceof Error");
    check("C('m').message === 'm' && new C(12).message === '12' && C('m').name === n");
    check("!new C().hasOwnProperty('message') && new C('x').hasOwnProperty('message')");
    check("String(new C(12)) === n + ': 12' && new C('q').stack === n + ': q'");
    check("new C().stack === n && String(C()) === n");
    // The syntactic forms agree with the values.
    assert_bool("RangeError('m') instanceof RangeError", true);
    assert_string("new SyntaxError('s').stack", "SyntaxError: s");
    assert_bool("!new URIError().hasOwnProperty('message')", true);
}

#[test]
fn engine_errors_have_their_constructor() {
    assert_bool(
        "try { null.x } catch (e) { e.constructor === TypeError }",
        true,
    );
    assert_bool(
        "try { new Array(-1) } catch (e) { e.constructor === RangeError }",
        true,
    );
    assert_bool(
        "try { notDefinedAnywhere } catch (e) { e.constructor === ReferenceError }",
        true,
    );
    assert_string(
        "try { null.x } catch (e) { e.constructor.name }",
        "TypeError",
    );
}

#[test]
fn assert_throws_style_comparison() {
    let harness = r#"
        function throws(expected, fn) {
            try { fn() } catch (thrown) {
                if (thrown.constructor !== expected) {
                    return "wrong: " + thrown.constructor.name + " vs " + expected.name;
                }
                return "ok " + expected.name;
            }
            return "no throw";
        }
    "#;
    assert_string(
        &format!("{harness} throws(TypeError, function () {{ null.x }})"),
        "ok TypeError",
    );
    assert_string(
        &format!("{harness} throws(RangeError, function () {{ null.x }})"),
        "wrong: TypeError vs RangeError",
    );
    assert_string(
        &format!("{harness} throws(Error, function () {{ throw new Error('e') }})"),
        "ok Error",
    );
}

#[test]
fn class_extends_error() {
    let class = r#"class E extends Error { constructor(m) { super(m); this.name = "E" } }"#;
    assert_bool(
        &format!("{class} const e = new E('hi'); e instanceof E"),
        true,
    );
    assert_bool(
        &format!("{class} const e = new E('hi'); e instanceof Error"),
        true,
    );
    assert_string(&format!("{class} new E('hi').message"), "hi");
    assert_string(&format!("{class} new E('hi').name"), "E");
    assert_string(&format!("{class} String(new E('hi'))"), "E: hi");
    assert_string(&format!("{class} new E('hi').toString()"), "E: hi");
    assert_string(&format!("{class} new E('hi').stack"), "Error: hi");
    assert_bool(&format!("{class} new E('hi').constructor === E"), true);
    assert_uncaught(&format!("{class} throw new E('boom')"), "E: boom");
    // Without an explicit constructor; a subclass of a native error kind.
    assert_string("class F extends TypeError {} new F('x').name", "TypeError");
    assert_bool(
        "class F extends TypeError {} const f = new F('x'); f instanceof F && f instanceof TypeError && f instanceof Error",
        true,
    );
    assert_bool(
        "class F extends RangeError { m() { return 3 } } new F().m() === 3",
        true,
    );
}

#[test]
fn builtin_parents_honour_new_target() {
    assert_string(
        "class M extends Map { twice(k) { return this.get(k) * 2 } } const m = new M(); m.set('a', 4); m.twice('a') + ':' + (m instanceof M) + ':' + (m instanceof Map)",
        "8:true:true",
    );
    assert_string(
        "class S extends Set { has2(v) { return this.has(v) } } const s = new S(); s.add(1); s.has2(1) + ':' + (s instanceof S) + ':' + (s instanceof Set) + ':' + s.size",
        "true:true:true:1",
    );
    assert_string(
        "class D extends Date { y() { return this.getTime() + 1 } } const d = new D(41); d.y() + ':' + (d instanceof D) + ':' + (d instanceof Date)",
        "42:true:true",
    );
    assert_string(
        "class A extends Array { first() { return this[0] } } const a = new A(1, 2); a.push(3); a.first() + ':' + a.length + ':' + (a instanceof A) + ':' + (a instanceof Array) + ':' + Array.isArray(a)",
        "1:3:true:true:true",
    );
    assert_output(
        "class P extends Promise { tag() { return 'p' } } const p = new P(function (r) { r(9) }); console.log(p.tag(), p instanceof P, p instanceof Promise); p.then(function (v) { console.log(v) })",
        "p true true\n9\n",
    );
}

#[test]
fn generic_array_map_over_array_likes() {
    assert_string(
        r#"JSON.stringify(Array.prototype.map.call({ length: 2, 0: "a", 1: 2 }, String))"#,
        r#"["a","2"]"#,
    );
    assert_eval(
        "Array.prototype.map.call({ length: 3, 0: 1, 2: 3 }, function (x) { return x * 2 })",
        "[ 2, <1 empty item>, 6 ]",
    );
    assert_eval(
        r#"Array.prototype.map.call({ length: "2", 0: 1, 1: 2 }, function (x, i, o) { return x + i + (o.length === "2") })"#,
        "[ 2, 4 ]",
    );
    assert_eval(
        "Array.prototype.map.call({ length: 1, 0: 1 }, function (x, i, o) { return this.k + x + i + o.length }, { k: 10 })",
        "[ 12 ]",
    );
    assert_eval("Array.prototype.map.call({}, String)", "[]");
    assert_eval("[1, 2].map === Array.prototype.map", "true");
    assert_uncaught(
        "Array.prototype.map.call({ length: 1 }, 5)",
        "TypeError: Array.prototype.map callback is not a function",
    );
    assert_uncaught(
        "Array.prototype.map.call(null, String)",
        "TypeError: Array.prototype.map called on null or undefined",
    );
}

#[test]
fn user_bindings_shadow_builtin_names() {
    // `new`
    assert_number(
        "class Map { constructor() { this.x = 3 } } new Map().x",
        3.0,
    );
    assert_number("function Error(m) { this.m = m } new Error(4).m", 4.0);
    assert_number(
        "function f() { let Date = function () { this.z = 5 }; return new Date().z } f()",
        5.0,
    );
    // `instanceof`
    assert_bool(
        "function TypeError() {} try { null.x } catch (e) { e instanceof TypeError }",
        false,
    );
    assert_bool(
        "class Error {} const e = new Error(); e instanceof Error",
        true,
    );
    assert_bool("let Array = function () {}; [] instanceof Array", false);
    // calls
    assert_string("function String(x) { return 'mine' } String(1)", "mine");
    assert_string("const Number = function () { return 'n' }; Number(1)", "n");
    assert_number("function Error() { return 8 } Error()", 8.0);
}

#[test]
fn renderer_class_extends_label() {
    assert_output(
        "class A {} class B extends A {} class E extends Error {} console.log(A, B, E)",
        "[class A] [class B extends A] [class E extends Error]\n",
    );
    assert_output(
        "console.log(Object, Map)",
        "[Function: Object] [Function: Map]\n",
    );
}
