//! Integration tests: out-of-scope constructs that must fail with a clear error, not a wrong value.

mod common;
use common::{assert_uncaught, eval_text};

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
    // A method added to an object after its literal is an ordinary property, so
    // calling it works (dot and computed forms, arrow and method values).
    assert_eq!(eval_text("let o = {}; o.f = () => 1; o.f()"), "1",);
    assert_eq!(
        eval_text("const o = { a: 2 }; o[\"g\"] = function () { return this.a }; o.g()"),
        "2",
    );
}

#[test]
fn keeps_dynamic_shape_receivers_at_runtime() {
    // Soundness: a receiver whose shape is NOT statically fixed must keep runtime
    // dispatch — the static-shape guard must never reject a valid program. A `let`
    // reassigned to a different object holds an unknown shape at the call site.
    assert_eq!(
        eval_text("let o = { f() { return 1 } }; o = { bar() { return 2 } }; o.bar()"),
        "2",
    );
    // Calling a method that IS in the known shape stays valid.
    assert_eq!(eval_text("let o = { f() { return 7 } }; o.f()"), "7",);
    // A class instance receiver has no object-literal shape => runtime dispatch.
    assert_eq!(
        eval_text("class P { m() { return 9 } } let p = new P(); p.m()"),
        "9",
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
        eval_text(
            "class A { f() { return 1 } } let a = new A(); let caught = false; \
             try { a.bar() } catch (e) { caught = e instanceof TypeError } caught"
        ),
        "true",
    );
    // It is also `instanceof Error`, and its `.message` is the bare reason (the
    // `TypeError:` prefix belongs to `.stack` / the uncaught render).
    assert_eq!(
        eval_text(
            "class A { f() { return 1 } } let a = new A(); let caught = false; \
             try { a.bar() } catch (e) { caught = e instanceof Error } caught"
        ),
        "true",
    );
    assert_eq!(
        eval_text(
            "class A { f() { return 1 } } let a = new A(); let m = 0; \
             try { a.bar() } catch (e) { m = e.message } m"
        ),
        "a.bar is not a function",
    );
}

#[test]
fn unknown_method_in_function_free_program_throws_typeerror() {
    // Genuine Map/Set methods work (see the collections suite); an unrecognized
    // one is a runtime TypeError even when the program declares no function (no
    // funcref table), never a compile-time rejection or invalid wasm.
    assert_uncaught(
        "let m = new Map(); m.zzz()",
        "TypeError: m.zzz is not a function",
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
