//! Early (compile-time) errors of the static semantics: lexical
//! redeclarations, declarations in statement position, `await` as an
//! identifier in an async body, duplicate parameter / body names, the class
//! static block restrictions, and the private-name rules (undeclared and
//! duplicate names, `delete` of a private reference, a bare `#x`, a name not
//! starting with an `IdentifierStart`).

#![allow(clippy::expect_used)]

/// Assert `src` is rejected at compile time with a message containing
/// `message`.
fn assert_early_error(src: &str, message: &str) {
    let err = jsc_core::run::compile_and_run(src).expect_err("program should not compile");
    let text = err.to_string();
    assert!(
        text.contains(message),
        "expected an early error containing `{message}` for `{src}`, got `{text}`"
    );
}

/// Assert `src` compiles and runs.
fn assert_runs(src: &str) {
    let result = jsc_core::run::compile_and_run(src);
    assert!(result.is_ok(), "`{src}` should run, got {:?}", result.err());
}

#[test]
fn a_block_rejects_lexical_redeclarations() {
    for src in [
        "{ let f; let f; }",
        "{ function* f(){} let f; }",
        "{ async function f(){} class f {} }",
        "{ const f = 1; function* f(){} }",
        "{ function f(){} async function f(){} }",
        "{ function f(){} function f(){} }",
        "function g(){ { let f; let f; } }",
    ] {
        assert_early_error(src, "`f` has already been declared");
    }
}

#[test]
fn a_block_rejects_a_lexical_name_also_declared_by_var() {
    for src in [
        "{ function* f(){} var f; }",
        "{ var f; async function f(){} }",
        "{ function* f(){}; { var f; } }",
        "function x(){ { async function f(){}; var f; } }",
        "{ let f; for (var f of []) ; }",
    ] {
        assert_early_error(src, "`f` has already been declared");
    }
}

#[test]
fn a_switch_case_block_is_one_lexical_scope() {
    assert_early_error(
        "switch (0) { case 1: let f; default: function* f(){} }",
        "`f` has already been declared",
    );
    assert_early_error(
        "switch (0) { case 1: async function f(){} default: var f; }",
        "`f` has already been declared",
    );
}

#[test]
fn a_function_body_rejects_lexical_redeclarations() {
    assert_early_error(
        "function g(){ let a; var a; }",
        "`a` has already been declared",
    );
    assert_early_error("let a; var a;", "`a` has already been declared");
    assert_early_error("let a; function a(){}", "`a` has already been declared");
}

#[test]
fn valid_redeclarations_still_compile() {
    assert_runs("var a; var a; function f(){} var f;");
    assert_runs("function g(){ var a; function a(){} { let a; { let a; } } }");
    assert_runs("{ let a; } { let a; } switch (0) { case 1: { let b; } default: { let b; } }");
    assert_runs("function g(){ var a; { var a; } }");
    assert_runs("try {} catch (e) { var e; }");
    assert_runs("function g(){ { function* f(){} } var f; }");
}

#[test]
fn async_functions_are_not_allowed_in_statement_position() {
    for src in [
        "if (true) async function f(){}",
        "if (false) ; else async function f(){}",
        "while (false) async function f(){}",
        "do async function f(){} while (false)",
        "for (;false;) async function f(){}",
        "for (var x in {}) async function f(){}",
        "for (var x of []) async function f(){}",
    ] {
        assert_early_error(src, "declarations are not allowed in statement position");
    }
}

#[test]
fn await_is_not_an_identifier_in_an_async_body() {
    for src in [
        "async () => { var \\u0061wait; }",
        "var f = async function () { var \\u0061wait; };",
        "var f = async function f() { var \\u0061wait; };",
        "async function f() { var \\u0061wait; }",
    ] {
        assert_early_error(
            src,
            "`await` cannot be used as an identifier in an async context",
        );
    }
    // A nested ordinary function is not an async context.
    assert_runs(
        "async function f() { function g() { var \\u0061wait = 1; return await; } return g(); }",
    );
}

#[test]
fn duplicate_parameters_and_body_names_are_rejected() {
    assert_early_error("async (a, a) => {}", "duplicate parameter name `a`");
    assert_early_error("function* g(a, a) {}", "duplicate parameter name `a`");
    assert_early_error("async function f(a, a) {}", "duplicate parameter name `a`");
    for src in [
        "(a, a) => 1",
        "(a, a) => {}",
        "function f(a, a) {}",
        "(function (a, a) {});",
        "function f(a, a = 1) {}",
        "function f(a, [a]) {}",
        "function f(a, ...a) {}",
        "({ m(a, a) {} });",
        "({ m(a, a = 1) {} });",
        "({ *m(a, a) {} });",
        "class C { m(a, a) {} }",
        "class C { static m(a, a = 1) {} }",
        "class C { constructor(a, a) {} }",
        "(class { m(a, a) {} });",
        "({ set s([a, a]) {} });",
        "({ [function (a, a) {}]: 1 });",
    ] {
        assert_early_error(src, "duplicate parameter name `a`");
    }
    assert_early_error(
        "async (bar) => { let bar; }",
        "`bar` has already been declared",
    );
    assert_early_error(
        "(async function foo(bar) { let bar; });",
        "`bar` has already been declared",
    );
    assert_early_error(
        "function* g(x) { const x = 1; }",
        "`x` has already been declared",
    );
    assert_early_error(
        "try {} catch (e) { let e; }",
        "`e` has already been declared",
    );
    assert_runs("function* g(x) { var x; yield x; } g(1).next();");
}

#[test]
fn a_catch_pattern_rejects_duplicate_and_redeclared_names() {
    for src in [
        "try {} catch ([x, x]) {}",
        "try {} catch ({x, y: x}) {}",
        "try {} catch ([x, {a: [x]}]) {}",
        "try {} catch ({x, ...x}) {}",
        "try {} catch ({x}) { let x; }",
        "try {} catch ([x = 1]) { class x {} }",
        "try {} catch ({x}) { function x() {} }",
        // B.3.4 tolerates `var` only for an identifier parameter.
        "try {} catch ({x}) { var x; }",
        "try {} catch ([x]) { for (var x of []) {} }",
        "try {} catch ([x]) { { var x = 1; } }",
    ] {
        assert_early_error(src, "`x` has already been declared");
    }
    assert_runs("try {} catch ({x}) { { let x; } }");
    assert_runs("try {} catch ([x]) { function f() { var x; } }");
}

#[test]
fn a_duplicate_proto_entry_in_an_object_literal_is_an_early_error() {
    assert_early_error(
        "var o = {__proto__: null, \"__proto__\": null}",
        "duplicate __proto__",
    );
    assert_early_error(
        "if (false) { var o = {__proto__: {}, a: 1, __proto__: {}} }",
        "duplicate __proto__",
    );
    // Shorthand, computed, method and accessor keys do not count.
    assert_runs("var __proto__ = 1; var o = {__proto__: null, __proto__}");
    assert_runs("var o = {__proto__: null, [\"__proto__\"]: 1}");
    assert_runs("var o = {__proto__: null, __proto__() {}}");
    // A destructuring assignment pattern may repeat it.
    assert_runs("var a, b; ({__proto__: a, __proto__: b} = {})");
}

#[test]
fn class_static_block_early_errors() {
    // swc reports some of these itself, with its own messages, so only the
    // rejection is asserted where `early.rs` is not the sole checker.
    // `arguments`, outside a nested non-arrow function (computed keys of a
    // nested class and nested arrows included).
    for src in [
        "class C { static { arguments; } }",
        "class C { static { (class { [argument\\u0073]() {} }); } }",
        "class C { static { () => arguments; } }",
    ] {
        assert_early_error(src, "");
    }
    // `await` as a binding or reference, outside nested functions and arrows.
    for src in [
        "class C { static { class await {} } }",
        "class C { static { (class await {}); } }",
        "function f() { var x; class C { static { x = await; } } }",
    ] {
        assert_early_error(src, "");
    }
    assert_early_error("async function f() { class C { static { await 0; } } }", "");
    assert_early_error("function* g() { class C { static { yield; } } }", "");
    assert_early_error("class C { static { return; } }", "");
    assert_early_error("class C extends Object { static { super(); } }", "");
    // Jumps and labels do not cross the block.
    assert_early_error("x: while (false) { class C { static { break x; } } }", "");
    assert_early_error("while (false) { class C { static { continue; } } }", "");
    assert_early_error("class C { static { x: x: 0; } }", "");
    assert_early_error("class C { static { x: while (false) { break y; } } }", "");
    // The block is a function-like lexical scope.
    assert_early_error(
        "class C { static { let x; let x; } }",
        "`x` has already been declared",
    );
    assert_early_error(
        "class C { static { let x; var x; } }",
        "`x` has already been declared",
    );
    // Valid forms: nested functions, arrows and methods lift the restrictions
    // they cross; `var` and function declarations share the block's scope.
    assert_runs("class C { static { (function () { return arguments; })(); } }");
    assert_runs("class C { static { ({ m() { return arguments; } }).m(); } }");
    assert_runs("class C { static { (() => { class await {} }); } }");
    assert_runs("class C { static { (() => { return 1; })(); } }");
    assert_runs("class C { static { var f; function f() {} } }");
    assert_runs("class C { static { x: while (false) { break x; } } }");
}

#[test]
fn private_name_early_errors() {
    // A `#x` no enclosing class body declares, in any position and even in
    // code that never runs.
    for src in [
        "this.#x;",
        "function f(o) { return o.#x; }",
        "function f(o) { return #x in o; }",
        "function f(o) { return o?.#x; }",
        "function f(o) { return o?.a.#x; }",
        "function f(o) { o.#x = 1; }",
        "function f(o) { o.#x(); }",
        "class C { #y; m(o) { return o.#x; } }",
        "class C { m() { class D { #x; } return this.#x; } }",
        "class C { #y = () => this.#x; }",
        "class C { static { this.#x; } }",
        "class C { [this.#x] = 1; }",
        "function f() { if (false) { class C { m(o) { o.#x; } } } }",
        // The heritage sees only the enclosing classes' names.
        "class C extends (o => o.#x) { #x; }",
        "class C extends class { y = this.#x; } { #x; }",
    ] {
        assert_early_error(src, "private name #x is not defined");
    }
    // Duplicates in one class body, except one getter plus one setter of the
    // same static-ness.
    for src in [
        "class C { #x; #x; }",
        "class C { #x; static #x; }",
        "class C { #x; #x() {} }",
        "class C { #x() {} static #x() {} }",
        "class C { get #x() {} get #x() {} }",
        "class C { set #x(v) {} set #x(v) {} }",
        "class C { get #x() {} set #x(v) {} get #x() {} }",
        "class C { get #x() {} set #x(v) {} #x; }",
        "class C { static get #x() {} set #x(v) {} }",
        "class C { *#x() {} async #x() {} }",
    ] {
        assert_early_error(src, "duplicate private name #x");
    }
    // `delete` of a private reference, plain, parenthesised or ending an
    // optional chain.
    for src in [
        "class C { #x; m() { delete this.#x; } }",
        "class C { #x; m() { delete (this.#x); } }",
        "class C { #x; m() { delete ((this.#x)); } }",
        "class C { #x() {} m() { delete this.#x; } }",
        "class C { #x; m() { delete this?.#x; } }",
        "class C { #x; m(o) { delete o?.a.#x; } }",
        "class C { #x; m(g) { delete g().#x; } }",
        "class C { #x; f = () => delete this.#x; }",
    ] {
        assert_early_error(src, "private fields cannot be deleted");
    }
    // A bare `#x` is an expression only on the left of `in`.
    for src in [
        "class C { #x; m() { #x; } }",
        "class C { #x; m(o) { return (#x) in o; } }",
        "class C { #x; m() { return #x + 1; } }",
    ] {
        assert_early_error(src, "unexpected private name #x");
    }
    // swc rejects these at parse time with its own messages.
    for src in [
        "class C { #constructor; }",
        "class C { #constructor() {} }",
        "class C { static #constructor() {} }",
        "class C extends Object { #x; m() { return super.#x; } }",
        "class C { #x; m() { return { #x: 1 }; } }",
        "class C { #x; m() { const { #x: x } = this; } }",
    ] {
        assert_early_error(src, "");
    }
    // Valid forms: an outer class's names inside a nested class, a getter /
    // setter pair, and a `delete` of a member of a private field.
    assert_runs("class C { #x = 1; m() { return class { f(o) { return o.#x; } }; } }");
    assert_runs("class C { get #x() { return 1; } set #x(v) {} }");
    assert_runs("class C { static set #x(v) {} static get #x() { return 1; } }");
    assert_runs("class C { #x = {}; m() { return delete this.#x.y; } } new C().m();");
    assert_runs("class C { #x; static has(o) { return #x in o; } } C.has({});");
}

#[test]
fn private_names_must_start_with_an_identifier_start() {
    // ZWJ / ZWNJ are IdentifierPart only, escaped or raw.
    for src in [
        r"class C { #‍_ZWJ; }",
        r"class C { #‌_ZWNJ; }",
        r"var C = class { #‍_ZWJ; };",
        r"class C { #\u{200C}x() {} }",
        "class C { #\u{200D}x; }",
        r"class C { #x; m() { return this.#‍x; } }",
        r"class C { #‍x; m() { return this.#‍x; } }",
        r"class C { #‌x; static has(o) { return #‌x in o; } }",
    ] {
        assert_early_error(src, "");
    }
    // swc rejects a raw ZWJ / ZWNJ itself; an escaped one reaches the early-error
    // pass.
    assert_early_error(r"class C { #\u200D_ZWJ; }", "invalid private name");
    assert_early_error(
        r"class C { #\u200Cx; m() { return this.#\u200Cx; } }",
        "invalid private name",
    );
    // A ZWJ / ZWNJ after the first character, `$`, `_`, an escaped
    // IdentifierStart and an Other_ID_Start character are valid.
    assert_runs(r"class C { #a‍‌b = 1; m() { return this.#a‍‌b; } } new C().m();");
    assert_runs("class C { #$ = 1; #_ = 2; m() { return this.#$ + this.#_; } } new C().m();");
    assert_runs(r"class C { #\u{6F} = 1; m() { return this.#o; } } new C().m();");
    assert_runs("class C { #\u{2118} = 1; m() { return this.#\u{2118}; } } new C().m();");
}
