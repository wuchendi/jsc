//! Integration tests: out-of-scope constructs that must fail with a clear error, not a wrong value.

mod common;
use common::{assert_rejected, assert_uncaught};

// ---------------------------------------------------------------------------
// Out-of-scope boundary: each of these must be REJECTED, never mis-evaluated.
// (See DESIGN.md for the full catalogue + gaps.)
// ---------------------------------------------------------------------------

#[test]
fn rejects_spread() {
    // Call-argument and array-literal spread are supported; only
    // object spread remains out of scope.
    assert_rejected("let o = { a: 1 }; let p = { ...o }; p.a");
}

#[test]
fn rejects_unsupported_for_of_and_for_in() {
    // A bare-identifier `for-of` head and a statically non-enumerable
    // `for-in` head still trap (a non-iterable `for-of` source is a run-time
    // `TypeError`, see the iteration suite).
    assert_rejected("for (x of [1, 2, 3]) { x }");
    assert_rejected("for (const k in 5) { k }");
}

// ---------------------------------------------------------------------------
// An unknown / not-yet-implemented method is a run-time `TypeError` from the
// prototype lookup — never a compile error, and never invalid wasm.
// ---------------------------------------------------------------------------

#[test]
fn unknown_string_method_throws_typeerror() {
    // A primitive receiver reads its methods from its prototype at run time:
    // an unknown name is `undefined`, and calling it throws.
    assert_uncaught("`t`.zzz()", "TypeError: expression is not a function");
    for (src, name) in [
        ("\"x\".zzz()", "\"x\".zzz"),
        ("(1).zzz()", "1.zzz"),
        ("true.zzz()", "true.zzz"),
        ("\"x\"[\"zzz\"]()", "\"x\".zzz"),
        ("function f() { return 1 } \"x\".zzz()", "\"x\".zzz"),
    ] {
        assert_uncaught(src, &format!("TypeError: {name} is not a function"));
    }
}

#[test]
fn unknown_object_method_throws_at_runtime() {
    // Objects are dynamic: a method absent from the receiver reads `undefined`,
    // so calling it is an uncaught `TypeError` at run time, not a compile error.
    assert_uncaught("let o = {}; o.foo()", "TypeError: o.foo is not a function");
    assert_uncaught(
        "let o = { a: 1 }; o.bar()",
        "TypeError: o.bar is not a function",
    );
    assert_uncaught(
        "let o = {}; o[\"baz\"]()",
        "TypeError: o.baz is not a function",
    );
    assert_uncaught(
        "let o = { f() { return 1 } }; o.bar()",
        "TypeError: o.bar is not a function",
    );
    assert_uncaught(
        "({ f() { return 1 } }).bar()",
        "TypeError: expression is not a function",
    );
}

#[test]
fn method_added_after_literal_is_callable() {
    use jsc_core::run::{Value, compile_and_run};
    // A method added to an object after its literal is an ordinary property, so
    // calling it works (dot and computed forms, arrow and method values).
    assert_eq!(
        compile_and_run("let o = {}; o.f = () => 1; o.f()").expect("added method must run"),
        Value::Number(1.0),
    );
    assert_eq!(
        compile_and_run("const o = { a: 2 }; o[\"g\"] = function () { return this.a }; o.g()")
            .expect("added method must run"),
        Value::Number(2.0),
    );
}

#[test]
fn keeps_dynamic_shape_receivers_at_runtime() {
    use jsc_core::run::{Value, compile_and_run};
    // Soundness: a receiver whose shape is NOT statically fixed must keep runtime
    // dispatch — the static-shape guard must never reject a valid program. A `let`
    // reassigned to a different object holds an unknown shape at the call site.
    assert_eq!(
        compile_and_run("let o = { f() { return 1 } }; o = { bar() { return 2 } }; o.bar()")
            .expect("reassigned receiver must keep runtime dispatch"),
        Value::Number(2.0),
    );
    // Calling a method that IS in the known shape stays valid.
    assert_eq!(
        compile_and_run("let o = { f() { return 7 } }; o.f()").expect("known method must run"),
        Value::Number(7.0),
    );
    // A class instance receiver has no object-literal shape => runtime dispatch.
    assert_eq!(
        compile_and_run("class P { m() { return 9 } } let p = new P(); p.m()")
            .expect("class instance method must run"),
        Value::Number(9.0),
    );
}

#[test]
fn dynamic_shape_unknown_method_throws_typeerror() {
    // Runtime guard: a dynamic-shape receiver whose method is genuinely absent
    // can only be caught at runtime (the compile-time guards can't prove it).
    // Instead of reading `undefined` and dispatching it as a bogus funcref index (a
    // wrong value or a cryptic wasm trap), the call throws a clean TypeError.
    // A `new`-instance receiver has no statically-known object-literal shape —
    // both when bound to a variable and as a direct `new C().bar()` expression
    // (the static-shape guard explicitly defers class instances to this runtime guard).
    assert_uncaught(
        "class A { f() { return 1 } } let a = new A(); a.bar()",
        "TypeError: a.bar is not a function",
    );
    assert_uncaught(
        "class C { f() { return 1 } } new C().bar()",
        "TypeError: expression is not a function",
    );
    // A function parameter's shape is unknown at the call site.
    assert_uncaught(
        "function g(o) { return o.bar() } class C { f() { return 1 } } g(new C())",
        "TypeError: o.bar is not a function",
    );
    // An array element receiver is a runtime read.
    assert_uncaught(
        "class C { f() { return 1 } } let arr = [new C()]; arr[0].bar()",
        "TypeError: expression is not a function",
    );
    // A dynamic `this.m()` inside a method whose target method is absent.
    assert_uncaught(
        "class C { run() { return this.nope() } f() { return 1 } } new C().run()",
        "TypeError: this.nope is not a function",
    );
    // A reassigned binding drops its static shape => guarded runtime dispatch.
    assert_uncaught(
        "let o = { f() { return 1 } }; o = { g() { return 2 } }; o.zzz()",
        "TypeError: o.zzz is not a function",
    );
    // A computed key is unknown until runtime.
    assert_uncaught(
        "let o = { f() { return 1 } }; o[\"z\" + \"z\"]()",
        "TypeError: expression is not a function",
    );
    // The thrown value is a real branded TypeError object, so
    // `try`/`catch` can observe its identity and read its fields.
    assert_eq!(
        jsc_core::run::compile_and_run(
            "class A { f() { return 1 } } let a = new A(); let caught = false; \
             try { a.bar() } catch (e) { caught = e instanceof TypeError } caught"
        )
        .expect("the guard's TypeError must be catchable"),
        jsc_core::run::Value::Bool(true),
    );
    // It is also `instanceof Error`, and its `.message` is the bare reason (the
    // `TypeError:` prefix belongs to `.stack` / the uncaught render).
    assert_eq!(
        jsc_core::run::compile_and_run(
            "class A { f() { return 1 } } let a = new A(); let caught = false; \
             try { a.bar() } catch (e) { caught = e instanceof Error } caught"
        )
        .expect("the guard's TypeError must be an Error"),
        jsc_core::run::Value::Bool(true),
    );
    assert_eq!(
        jsc_core::run::compile_and_run(
            "class A { f() { return 1 } } let a = new A(); let m = 0; \
             try { a.bar() } catch (e) { m = e.message } m"
        )
        .expect("the guard's TypeError message must be readable"),
        jsc_core::run::Value::Str("a.bar is not a function".to_owned()),
    );
}

#[test]
fn unknown_method_in_function_free_program_throws_typeerror() {
    // Genuine Map/Set methods work (see the collections suite); an unrecognized
    // one is a runtime TypeError even when the program declares no function (no
    // funcref table), never a compile-time rejection or invalid wasm.
    assert_uncaught(
        "let m = new Map(); m.entries()",
        "TypeError: m.entries is not a function",
    );
    assert_uncaught(
        "let o = { a: 1 }; o = { b: 2 }; o.zzz()",
        "TypeError: o.zzz is not a function",
    );
    // An array literal reads its methods from `%Array.prototype%` at run time.
    assert_uncaught("[1, 2].group()", "TypeError: expression is not a function");
    assert_uncaught("[].groupBy([1])", "TypeError: expression is not a function");
    // A builtin-named method's user-method fallback throws too.
    assert_uncaught("let o = {}; o.get(1)", "TypeError: o.get is not a function");
    assert_uncaught(
        "let o = {}; o.getFullYear()",
        "TypeError: o.getFullYear is not a function",
    );
}

// ---------------------------------------------------------------------------
// Interim guards (design/symbol-strings.md §9.4): a user well-known symbol
// hook a consult site does not honour yet throws a named `TypeError`, never a
// silently wrong result. Each test is deleted by the stage that honours it.
// ---------------------------------------------------------------------------

#[test]
fn user_species_throws_until_y4() {
    let text = "TypeError: Symbol.species is not supported yet";
    let setup =
        "let C = function () {}; C[Symbol.species] = Array; let a = [1]; a.constructor = C;";
    for method in [
        "a.map(x => x)",
        "a.filter(x => x)",
        "a.slice()",
        "a.concat()",
    ] {
        assert_uncaught(&format!("{setup} {method}"), text);
    }
}

#[test]
fn is_concat_spreadable_throws_until_y4() {
    let text = "TypeError: Symbol.isConcatSpreadable is not supported yet";
    assert_uncaught(
        "let o = { [Symbol.isConcatSpreadable]: true, length: 1, 0: 'x' }; [].concat(o)",
        text,
    );
    assert_uncaught(
        "let a = [1]; a[Symbol.isConcatSpreadable] = false; a.concat([2])",
        text,
    );
}
