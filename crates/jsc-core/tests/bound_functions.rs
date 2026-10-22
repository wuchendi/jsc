//! Integration tests: `Function.prototype.call` / `apply` / `bind` /
//! `toString`, and bound function exotic objects.

mod common;
use common::{assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught};

#[test]
fn call_apply_bind_to_string_are_builtin_methods() {
    for (name, length) in [("call", 1.0), ("apply", 2.0), ("bind", 1.0), ("toString", 0.0)] {
        assert_number(&format!("Function.prototype.{name}.length"), length);
        assert_string(&format!("Function.prototype.{name}.name"), name);
        assert_bool(
            &format!(r#"Function.prototype.hasOwnProperty("{name}")"#),
            true,
        );
    }
}

#[test]
fn reassigned_call_and_apply_are_honoured() {
    assert_string(
        r#"Function.prototype.call = function () { return "mine"; };
           function f() { return "theirs"; } f.call(null)"#,
        "mine",
    );
    assert_string(
        r#"Function.prototype.apply = function (t, a) { return "mine" + a.length; };
           function f() { return "theirs"; } f.apply(null, [1, 2])"#,
        "mine2",
    );
    // An own `call` shadows the prototype's.
    assert_number(
        "function f() { return 1; } f.call = function () { return 2; }; f.call()",
        2.0,
    );
}

#[test]
fn apply_reads_array_likes_and_arguments() {
    assert_string(
        r#"function f(...r) { return r.join(","); }
           function g() { return f.apply(null, arguments); } g(1, 2, 3, 4, 5, 6, 7)"#,
        "1,2,3,4,5,6,7",
    );
    assert_string(
        r#"function f(...r) { return r.join(","); }
           f.apply(null, { length: "2", 0: "x", 1: "y", 2: "z" })"#,
        "x,y",
    );
    // `length` coercion runs user code and a throw propagates.
    assert_uncaught(
        r#"function f() {} f.apply(null, { get length() { throw new RangeError("len"); } })"#,
        "RangeError: len",
    );
    assert_uncaught(
        r#"function f() {} f.apply(null, { length: { valueOf() { throw new RangeError("v"); } } })"#,
        "RangeError: v",
    );
    // `call` / `apply` on a subclass-instance receiver.
    assert_number(
        "class A { m(a, b) { return this.k + a + b; } } class B extends A {}
         const b = new B(); b.k = 1; A.prototype.m.apply(b, [2, 3])",
        6.0,
    );
}

#[test]
fn bind_partially_applies() {
    assert_number(
        "function f(a, b, c) { return this.x + a + b + c; }
         const g = f.bind({ x: 1 }, 2); g(3, 4)",
        10.0,
    );
    assert_string(
        r#"function f(...r) { return r.join(","); }
           const g = f.bind(null, 1, 2, 3, 4); g(5, 6, 7, 8)"#,
        "1,2,3,4,5,6,7,8",
    );
    assert_string(
        r#"function f(...r) { return r.join(","); }
           const g = f.bind(null, 1); g(...[2, 3, 4, 5, 6, 7, 8, 9])"#,
        "1,2,3,4,5,6,7,8,9",
    );
    assert_string(
        r#"function f(...r) { return r.join(","); }
           const g = f.bind(...[null, 1, 2, 3, 4, 5, 6, 7, 8]); g(9)"#,
        "1,2,3,4,5,6,7,8,9",
    );
    // The bound `this` ignores the call-site receiver.
    assert_number(
        "function f() { return this.v; } const o = { v: 2, m: f.bind({ v: 1 }) }; o.m()",
        1.0,
    );
    assert_number(
        "function f() { return arguments.length; } f.bind(null, 1, 2)(3)",
        3.0,
    );
    // Arrows keep their lexical `this`.
    assert_number(
        "const o = { v: 7, m() { return (() => this.v).bind({ v: 1 })(); } }; o.m()",
        7.0,
    );
    // Builtins bind too.
    assert_string(
        r#"const join = Array.prototype.join.bind([1, 2, 3]); join("-")"#,
        "1-2-3",
    );
    assert_number("const max = Math.max.bind(null, 5); max(1, 9)", 9.0);
}

#[test]
fn bind_of_bind_chains() {
    assert_string(
        r#"function f(...r) { return this.t + r.join(""); }
           const g = f.bind({ t: "a" }, 1).bind({ t: "b" }, 2); g(3)"#,
        "a123",
    );
    assert_string(
        "function f() {} f.bind().bind().name",
        "bound bound f",
    );
}

#[test]
fn bind_requires_a_callable_receiver() {
    assert_uncaught(
        "Function.prototype.bind.call({})",
        "TypeError: Bind must be called on a function",
    );
    assert_uncaught(
        "const b = (function () {}).bind; b(1)",
        "TypeError: Bind must be called on a function",
    );
}

#[test]
fn bound_length() {
    assert_number("function f(a, b, c) {} f.bind(null).length", 3.0);
    assert_number("function f(a, b, c) {} f.bind(null, 1).length", 2.0);
    assert_number("function f(a, b, c) {} f.bind(null, 1, 2, 3, 4).length", 0.0);
    assert_number("((a, ...r) => 0).bind(null).length", 1.0);
    // A target `length` redefined as an own data property.
    for (value, bound_args, expected) in [
        ("NaN", "", "0"),
        ("-0", "", "0"),
        ("Infinity", "", "Infinity"),
        ("Infinity", ", 0", "Infinity"),
        ("-Infinity", ", 0", "0"),
        ("3.66", "", "3"),
        ("-0.77", "", "0"),
        ("2147483648", "", "2147483648"),
        ("9007199254740991", ", 1", "9007199254740990"),
        (r#""3""#, "", "0"),
        ("undefined", "", "0"),
    ] {
        assert_eval(
            &format!(
                "function f() {{}} delete f.length; f.length = {value}; f.bind(null{bound_args}).length"
            ),
            expected,
        );
    }
    // No own `length` on the target: 0, even when one is inherited.
    assert_number("function f(a) {} delete f.length; f.bind(null).length", 0.0);
    assert_number(
        "class A { static get length() { return 4; } } class B extends A {}
         delete B.length; B.bind(null).length",
        0.0,
    );
    // An own `length` getter runs.
    assert_number(
        "class A { static get length() { return 4; } } A.bind(null, 1).length",
        3.0,
    );
}

#[test]
fn bound_name() {
    assert_string("function foo() {} foo.bind(null).name", "bound foo");
    assert_string("(() => 0).bind(null).name", "bound ");
    assert_string("class A {} A.bind(null).name", "bound A");
    assert_string(
        "function f() {} delete f.name; f.name = 7; f.bind().name",
        "bound ",
    );
    assert_string(
        "class A { static get name() { return \"got\"; } } A.bind().name",
        "bound got",
    );
    assert_uncaught(
        r#"class A { static get name() { throw new RangeError("name"); } } A.bind()"#,
        "RangeError: name",
    );
}

#[test]
fn bound_own_properties() {
    // `length` / `name` are non-writable (a strict write throws),
    // non-enumerable and configurable.
    assert_uncaught(
        "function f(a) {} const g = f.bind(null); g.length = 5",
        "TypeError: Cannot assign to read only property 'length' of function '[Function: bound f]'",
    );
    assert_uncaught(
        "function f(a) {} const g = f.bind(null); g.name = 5",
        "TypeError: Cannot assign to read only property 'name' of function '[Function: bound f]'",
    );
    assert_number("function f(a) {} Object.keys(f.bind(null)).length", 0.0);
    assert_bool(
        r#"function f(a) {} const g = f.bind(null); delete g.length; delete g.name;
           g.hasOwnProperty("length") || g.hasOwnProperty("name")"#,
        false,
    );
    // No own `prototype`, `caller` or `arguments`.
    assert_bool(
        r#"function f() {} f.bind(null).hasOwnProperty("prototype")"#,
        false,
    );
    assert_bool(
        r#"function f() {} const g = f.bind(null); g.hasOwnProperty("caller") || g.hasOwnProperty("arguments")"#,
        false,
    );
    assert_uncaught(
        "function f() {} f.bind(null).caller",
        "TypeError: 'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them",
    );
    // Ordinary properties can be added.
    assert_number("function f() {} const g = f.bind(); g.p = 12; g.p", 12.0);
    assert_string("function f() {} typeof f.bind()", "function");
}

#[test]
fn bound_prototype_is_target_prototype() {
    assert_bool(
        "function f() {} f.bind().call === Function.prototype.call",
        true,
    );
    assert_number(
        "class A { static s() { return 3; } } class B extends A {} B.bind().s()",
        3.0,
    );
}

#[test]
fn new_on_bound_function_constructs_target() {
    assert_number(
        "function P(x, y) { this.x = x; this.y = y; }
         const B = P.bind({ ignored: 1 }, 1); const p = new B(2); p.x + p.y",
        3.0,
    );
    assert_bool(
        "function P() {} const B = P.bind(null); new B() instanceof P",
        true,
    );
    assert_bool(
        "function P() {} const B = P.bind(null); new B().constructor === P",
        true,
    );
    // new.target is the target when the bound function was new.target.
    assert_bool(
        "let nt; function A() { nt = new.target; } const C = A.bind().bind(); new C(); nt === A",
        true,
    );
    // A derived class target.
    assert_string(
        r#"class A { constructor(a) { this.a = a; } }
           class B extends A { constructor(a, b) { super(a); this.b = b; } }
           const Bb = B.bind(null, "x"); const o = new Bb("y");
           [o.a, o.b, o instanceof B, o instanceof A].join()"#,
        "x,y,true,true",
    );
    // A builtin constructor target.
    assert_number("const D = Date.bind(null, 5); new D().getTime()", 5.0);
    assert_bool("const M = Map.bind(); new M() instanceof Map", true);
    // A subclass of a bound function constructs with the subclass's new.target.
    assert_bool(
        "function P() { this.p = 1; } const B = P.bind(null);
         class C extends B {} const c = new C(); c.p === 1 && c instanceof C",
        true,
    );
    // Calling a bound class without `new` still throws.
    assert_uncaught(
        "class A {} const B = A.bind(); B()",
        "TypeError: Class constructor A cannot be invoked without 'new'",
    );
}

#[test]
fn new_on_bound_non_constructor_throws() {
    assert_uncaught(
        "const B = (() => 0).bind(null); new B()",
        "TypeError: B is not a constructor",
    );
    assert_uncaught(
        "const o = { m() {} }; const B = o.m.bind(o); new B()",
        "TypeError: B is not a constructor",
    );
}

#[test]
fn instanceof_bound_uses_target() {
    assert_bool(
        "function P() {} const B = P.bind(); new P() instanceof B",
        true,
    );
    assert_bool(
        "function P() {} function Q() {} const B = P.bind().bind(); new Q() instanceof B",
        false,
    );
    assert_bool("function P() {} const B = P.bind(); 1 instanceof B", false);
}

#[test]
fn function_to_string_is_native_function_syntax() {
    assert_string(
        "function foo(a) { return a; } foo.toString()",
        "function foo() { [native code] }",
    );
    assert_string(
        "Function.prototype.call.toString()",
        "function call() { [native code] }",
    );
    assert_string(
        "function foo() {} foo.bind().toString()",
        "function () { [native code] }",
    );
    assert_string("(() => 0).toString()", "function () { [native code] }");
    assert_string(
        "[].push.toString()",
        "function push() { [native code] }",
    );
    assert_string(
        r#"const o = { "a b"() {} }; o["a b"].toString()"#,
        "function () { [native code] }",
    );
    // ToString of a function goes through Function.prototype.toString.
    assert_string(
        r#"function foo() {} "" + foo"#,
        "function foo() { [native code] }",
    );
    assert_uncaught(
        "Function.prototype.toString.call({})",
        "TypeError: Function.prototype.toString requires that 'this' be a Function",
    );
}

#[test]
fn bound_function_renders_like_node() {
    assert_eval("function f() {} f.bind(null)", "[Function: bound f]");
    assert_eval("class A {} A.bind(null)", "[Function: bound A]");
    assert_eval("(() => 0).bind(null)", "[Function: bound ]");
    assert_output(
        "function f() {} console.log(f.bind(null), f.bind().bind())",
        "[Function: bound f] [Function: bound bound f]\n",
    );
}
