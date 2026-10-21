//! Integration tests: the realm's static intrinsics (`%Object.prototype%`,
//! `%Function.prototype%`, the Error prototypes) and their builtin methods,
//! reached through ordinary property lookup on the `proto` chain.

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught,
};

#[test]
fn has_own_property_own_inherited_missing() {
    assert_bool(r#"({ a: 1 }).hasOwnProperty("a")"#, true);
    assert_bool(r#"({ a: 1 }).hasOwnProperty("b")"#, false);
    // Inherited from %Object.prototype%, not own.
    assert_bool(r#"({ a: 1 }).hasOwnProperty("toString")"#, false);
    assert_bool(
        r#"class A { m() {} } const a = new A(); a.x = 1; a.hasOwnProperty("x") && !a.hasOwnProperty("m")"#,
        true,
    );
    // ToPropertyKey: a number key matches its canonical string.
    assert_bool(r#"({ 1: "x" }).hasOwnProperty(1)"#, true);
    // A deleted key is gone.
    assert_bool(
        r#"const o = { a: 1 }; delete o.a; o.hasOwnProperty("a")"#,
        false,
    );
    // Function objects: materialized own `length`/`name`.
    assert_bool(r#"function f(a) {} f.hasOwnProperty("length")"#, true);
    // Reached through `call` on any receiver.
    assert_bool(r#"({}).hasOwnProperty.call([1, 2], "length")"#, true);
    assert_bool("({}).hasOwnProperty.call([1, 2], 1)", true);
    assert_bool("({}).hasOwnProperty.call([1, 2], 2)", false);
    assert_bool(r#"({}).hasOwnProperty.call("ab", "length")"#, true);
    assert_bool(r#"({}).hasOwnProperty.call(5, "x")"#, false);
}

#[test]
fn has_own_property_on_nullish_receiver_throws() {
    assert_uncaught(
        r#"({}).hasOwnProperty.call(null, "a")"#,
        "TypeError: Cannot convert undefined or null to object",
    );
    assert_uncaught(
        r#"({}).hasOwnProperty.call(undefined, "a")"#,
        "TypeError: Cannot convert undefined or null to object",
    );
}

#[test]
fn object_to_string_tags() {
    let tag = |value: &str, expected: &str| {
        assert_string(&format!("({{}}).toString.call({value})"), expected);
    };
    tag("undefined", "[object Undefined]");
    tag("null", "[object Null]");
    tag("true", "[object Boolean]");
    tag("1.5", "[object Number]");
    tag(r#""s""#, "[object String]");
    tag("[1]", "[object Array]");
    tag("function () {}", "[object Function]");
    tag("new Error('e')", "[object Error]");
    tag("new RangeError('e')", "[object Error]");
    tag("new Date(0)", "[object Date]");
    tag("{ a: 1 }", "[object Object]");
    // The builtin `@@toStringTag`s of the Map/Set/Promise prototypes.
    tag("new Map()", "[object Map]");
    tag("new Set()", "[object Set]");
    tag("Promise.resolve(1)", "[object Promise]");
    assert_string(
        "function f() { return ({}).toString.call(arguments); } f(1)",
        "[object Arguments]",
    );
    assert_string("({}).toString()", "[object Object]");
    assert_string("class A {} new A().toString()", "[object Object]");
}

#[test]
fn value_of_returns_receiver() {
    assert_bool("const o = {}; o.valueOf() === o", true);
    assert_number("({}).valueOf.call(7)", 7.0);
    assert_uncaught(
        "({}).valueOf.call(null)",
        "TypeError: Cannot convert undefined or null to object",
    );
}

#[test]
fn own_property_shadows_inherited_method() {
    assert_string(
        r#"const o = { toString() { return "mine"; } }; o.toString()"#,
        "mine",
    );
    assert_string(
        r#"const o = {}; o.toString = function () { return "set"; }; o.toString()"#,
        "set",
    );
    // The intrinsic itself is unchanged for other objects.
    assert_string(
        r#"const o = {}; o.toString = function () { return "set"; }; ({}).toString()"#,
        "[object Object]",
    );
    assert_string(
        r#"class A { toString() { return "A!"; } } new A().toString()"#,
        "A!",
    );
}

#[test]
fn function_call_binds_this_and_arguments() {
    assert_number(
        "function f(a, b) { return this.x + a + b; } f.call({ x: 1 }, 2, 3)",
        6.0,
    );
    assert_bool(
        "function f() { return this; } f.call(undefined) === undefined",
        true,
    );
    assert_number(
        "function f() { return arguments.length; } f.call(null, 1, 2, 3)",
        3.0,
    );
    assert_number("function f() { return arguments.length; } f.call()", 0.0);
    // Rest parameters see the shifted arguments.
    assert_number(
        "function f(...r) { return r.length; } f.call(null, 1, 2)",
        2.0,
    );
    // A spread call into `call` past the positional slots.
    assert_string(
        r#"function f(...r) { return r.join(","); } f.call(...[null, 1, 2, 3, 4, 5, 6])"#,
        "1,2,3,4,5,6",
    );
    // Methods and arrows; a method reached through `call` on another receiver.
    assert_number(
        "const o = { x: 5, m() { return this.x; } }; o.m.call({ x: 9 })",
        9.0,
    );
    assert_number("((a) => a * 2).call(null, 21)", 42.0);
    // `call` on a builtin.
    assert_string(
        "({}).toString.call.call(({}).toString, [])",
        "[object Array]",
    );
}

#[test]
fn function_apply_spreads_arg_list() {
    assert_number(
        "function f(a, b) { return this.x + a + b; } f.apply({ x: 1 }, [2, 3])",
        6.0,
    );
    assert_number(
        "function f() { return arguments.length; } f.apply(null, [1, 2, 3, 4, 5])",
        5.0,
    );
    assert_number(
        "function f() { return arguments.length; } f.apply(null)",
        0.0,
    );
    assert_number(
        "function f() { return arguments.length; } f.apply(null, null)",
        0.0,
    );
    assert_number(
        "function f() { return arguments.length; } f.apply(null, undefined)",
        0.0,
    );
    // Holes read as `undefined`.
    assert_bool(
        "function f(a, b) { return b === undefined; } f.apply(null, [1, , 3])",
        true,
    );
    // Array-likes go through `length` and indexed reads.
    assert_string(
        r#"function f(...r) { return r.join("-"); } f.apply(null, { length: 3, 0: "a", 2: "c" })"#,
        "a--c",
    );
    assert_number(
        "function f() { return arguments.length; } f.apply(null, { length: 2.7 })",
        2.0,
    );
    // An `arguments` object forwards.
    assert_number(
        "function g(a, b) { return a + b; } function f() { return g.apply(null, arguments); } f(4, 5)",
        9.0,
    );
}

#[test]
fn function_apply_and_call_type_errors() {
    assert_uncaught(
        "function f() {} f.apply(null, 1)",
        "TypeError: CreateListFromArrayLike called on non-object",
    );
    assert_uncaught(
        r#"function f() {} f.apply(null, "ab")"#,
        "TypeError: CreateListFromArrayLike called on non-object",
    );
    assert_uncaught(
        "const c = (function () {}).call; c.call(1)",
        "TypeError: Function.prototype.call called on a value that is not a function",
    );
    assert_uncaught(
        "const a = (function () {}).apply; a.call({}, null, [])",
        "TypeError: Function.prototype.apply called on a value that is not a function",
    );
    // A throw inside the target propagates out of `call` / `apply`.
    assert_uncaught(
        r#"function f() { throw new RangeError("in"); } f.call(null)"#,
        "RangeError: in",
    );
    assert_string(
        r#"function f() { throw new Error("x"); } let m = ""; try { f.apply(null, []); } catch (e) { m = e.message; } m"#,
        "x",
    );
}

#[test]
fn error_prototype_to_string_for_every_kind() {
    for kind in [
        "Error",
        "TypeError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "EvalError",
        "URIError",
    ] {
        assert_string(
            &format!(r#"new {kind}("boom").toString()"#),
            &format!("{kind}: boom"),
        );
        assert_string(&format!("new {kind}().toString()"), kind);
        assert_bool(&format!(r#"new {kind}("m") instanceof Error"#), true);
        assert_bool(&format!(r#"new {kind}("m") instanceof {kind}"#), true);
        assert_string(&format!(r#"new {kind}("m").name"#), kind);
    }
    // An empty name yields the message alone; a missing one defaults.
    assert_string(
        r#"const e = new Error("msg"); e.name = ""; e.toString()"#,
        "msg",
    );
    assert_string(
        r#"({}).toString.call(1) + "|" + new Error("m").toString.call({ message: "only" })"#,
        "[object Number]|Error: only",
    );
    assert_string(
        r#"new Error("m").toString.call({ name: "N", message: "" })"#,
        "N",
    );
    assert_uncaught(
        r#"new Error("m").toString.call(1)"#,
        "TypeError: Error.prototype.toString called on a non-object",
    );
}

#[test]
fn engine_errors_inherit_the_static_prototypes() {
    assert_bool(
        r#"function f() { return y; } let ok = false; try { f(); } catch (e) { ok = e instanceof ReferenceError && !e.hasOwnProperty("name"); } let y = 1; ok"#,
        true,
    );
    assert_string(
        r#"let s = ""; try { null.x; } catch (e) { s = e.toString(); } s"#,
        "TypeError: Cannot read properties of null (reading 'x')",
    );
    assert_bool(
        r#"let ok = false; try { JSON.parse("{"); } catch (e) { ok = e instanceof SyntaxError && e instanceof Error && !e.hasOwnProperty("name"); } ok"#,
        true,
    );
}

#[test]
fn rendering_lists_own_properties_only() {
    assert_eval("({})", "{}");
    assert_eval("({ a: 1 })", "{ a: 1 }");
    assert_output("console.log({}, { a: 1 })", "{} { a: 1 }\n");
    assert_output("class A { m() {} } console.log(new A())", "A {}\n");
    // A prototype object's own `constructor` back-link does not name it.
    assert_output(
        "class E {} class B extends E {} function F() {} console.log(E.prototype, B.prototype, F.prototype, new F())",
        "{} E {} {} F {}\n",
    );
}
