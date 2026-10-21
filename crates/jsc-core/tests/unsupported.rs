//! Integration tests: out-of-scope constructs that must fail with a clear error, not a wrong value.

mod common;
use common::{assert_rejected, assert_uncaught};

/// Assert `src` is rejected at COMPILE time with a clean unknown-method
/// diagnostic: the error names the unsupported method and, critically, is
/// NEVER the wasm-validator failure `failed to compile: wasm...` that a malformed
/// `call_indirect` would produce.
fn assert_unknown_method(src: &str, method: &str) {
    let result = jsc_core::run::compile_and_run(src);
    let msg = match &result {
        Ok(_) => String::new(),
        Err(err) => err.to_string(),
    };
    assert!(
        result.is_err(),
        "`{src}`: unknown method should be rejected at compile time"
    );
    assert_eq!(
        msg,
        format!("method '{method}' is not supported"),
        "diagnostic for `{src}`"
    );
    assert!(
        !msg.contains("failed to compile"),
        "`{src}` must never emit invalid wasm, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Out-of-scope boundary: each of these must be REJECTED, never mis-evaluated.
// (See DESIGN.md for the full catalogue + gaps.)
// ---------------------------------------------------------------------------

#[test]
fn rejects_async_functions() {
    // Top-level `async function` DECLARATIONS are supported (see the
    // promises suite); an async FUNCTION EXPRESSION, an async ARROW, and an async
    // GENERATOR remain out of scope.
    assert_rejected("let f = async function () { return 1 }; f()");
    assert_rejected("let f = async () => 1; f()");
    assert_rejected("async function* f() { yield 1 } f().next()");
}

#[test]
fn rejects_generator_functions() {
    // Top-level `function*` DECLARATIONS are supported (see the
    // generators suite); a generator EXPRESSION remains rejected.
    assert_rejected("let g = function* () { return 1 }; g()");
}

#[test]
fn rejects_spread() {
    // Call-argument and array-literal spread are supported; only
    // object spread remains out of scope.
    assert_rejected("let o = { a: 1 }; let p = { ...o }; p.a");
}

#[test]
fn rejects_top_level_this() {
    // Object methods and shorthand methods are supported (see the
    // objects suite), but a top-level `this` — outside any function, so with no
    // receiver binding — remains rejected.
    assert_rejected("this");
    // Async/generator methods stay out of scope.
    assert_rejected("let o = { async m() { return 1 } }; o.m()");
}

#[test]
fn rejects_computed_class_fields() {
    // Class declarations and expressions, `extends` of any expression, and
    // plain/static members are supported (see the classes suites); computed
    // field forms remain out of scope.
    assert_rejected("class A { [k] = 1 } new A()");
    assert_rejected("let C = class { [k] = 1 }; new C()");
}

#[test]
fn rejects_unsupported_for_of_and_for_in() {
    // `for-of` over arrays/strings with a `const`/`let` head is supported
    // and `for-in` over objects/arrays is supported, but these unsupported
    // forms still trap: an assignment-target head, a statically non-iterable
    // `for-of` head, and a statically non-enumerable `for-in` head.
    assert_rejected("for (x of [1, 2, 3]) { x }");
    assert_rejected("for (const x of 5) { x }");
    assert_rejected("for (const x of { a: 1 }) { x }");
    assert_rejected("for (const k in 5) { k }");
}

#[test]
fn rejects_function_prototype_bind() {
    // `bind` is phase 3: it throws rather than mis-dispatching.
    assert_rejected("function f(a) { return a } let g = f.bind(null); g(1)");
}

// ---------------------------------------------------------------------------
// An unknown / not-yet-implemented method must be rejected at COMPILE time
// with a clean span diagnostic — NEVER a `call_indirect` that the wasm validator
// rejects with `failed to compile: wasm[0]::function[..]`.
// ---------------------------------------------------------------------------

#[test]
fn rejects_unknown_string_method() {
    // A string-literal receiver can carry only the built-in string methods; any
    // other name is a statically-provable unknown method. (Names here are
    // genuinely unknown; trim/repeat/padStart/split are built in.)
    assert_unknown_method("\"x\".zzz()", "zzz");
    assert_unknown_method("\"hi\".charCodeAt(0)", "charCodeAt");
    assert_unknown_method("\"x\".normalize()", "normalize");
    // Reached through a template literal and a computed string-literal key.
    assert_unknown_method("`t`.zzz()", "zzz");
    assert_unknown_method("\"x\"[\"zzz\"]()", "zzz");
}

#[test]
fn rejects_unknown_array_method() {
    assert_unknown_method("[1, 2].flat()", "flat");
    assert_unknown_method("[1, 2, 3].reverse()", "reverse");
    assert_unknown_method("[].concat([1])", "concat");
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
fn rejects_unknown_method_even_with_functions_present() {
    // A primitive-literal receiver is rejected regardless of whether the program
    // also defines functions (which would otherwise emit a funcref table).
    assert_unknown_method("function f() { return 1 } \"x\".zzz()", "zzz");
    assert_unknown_method("let id = (x) => x; [1].flat()", "flat");
}

#[test]
fn unknown_method_in_function_free_program_throws_typeerror() {
    // Genuine Map/Set methods work (see the collections suite); an unrecognized
    // one is a runtime TypeError even when the program declares no function (no
    // funcref table), never a compile-time rejection or invalid wasm.
    assert_uncaught(
        "let m = new Map(); m.clear()",
        "TypeError: m.clear is not a function",
    );
    assert_uncaught(
        "let o = { a: 1 }; o = { b: 2 }; o.zzz()",
        "TypeError: o.zzz is not a function",
    );
    // A builtin-named method's user-method fallback throws too.
    assert_uncaught("let o = {}; o.get(1)", "TypeError: o.get is not a function");
    assert_uncaught(
        "let o = {}; o.getFullYear()",
        "TypeError: o.getFullYear is not a function",
    );
}
