//! Integration tests: `instanceof` with any right-hand side
//! (`OrdinaryHasInstance`) and optional calls binding `this`, via the CLI.
#![allow(clippy::expect_used)]

mod common;
use common::jsc;

use std::process::{Command, Output};

/// Run the built `jsc` binary with `args`, returning the raw process output.
fn jsc_output(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(args)
        .output()
        .expect("failed to spawn jsc binary")
}

/// Assert `src` exits 1 with an uncaught `TypeError` carrying `message`.
fn assert_uncaught_type_error(src: &str, message: &str) {
    let output = jsc_output(&["-e", src]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "`{src}`: {stderr}");
    assert!(
        stderr.contains(&format!("TypeError: {message}")),
        "`{src}`: {stderr}"
    );
}

/// A class is matched by its BINDING: an alias reads the same constructor, a
/// derived instance is an instance of both classes, and a plain object or a
/// primitive is an instance of neither.
#[test]
fn instanceof_class_by_binding() {
    assert_eq!(
        jsc(&[
            "-e",
            "class A{}; class B extends A{}; let b = new B(); let K = A; \
             console.log(b instanceof K, b instanceof B, ({}) instanceof A, 5 instanceof A)"
        ]),
        "true true false false"
    );
}

/// Reassigning a class binding is observed by a later `instanceof`.
#[test]
fn instanceof_observes_reassigned_binding() {
    assert_eq!(
        jsc(&[
            "-e",
            "class A{}; class B{}; let a = new A(); let C = A; let r1 = a instanceof C; \
             C = B; console.log(r1, a instanceof C)"
        ]),
        "true false"
    );
}

/// Any expression is a valid right-hand side.
#[test]
fn instanceof_expression_rhs() {
    assert_eq!(
        jsc(&[
            "-e",
            "class A{}; class B{}; let a = new A(); let cond = true; \
             let o = { k: B }; \
             console.log(a instanceof (cond ? A : B), a instanceof (cond ? B : A), \
             a instanceof o.k, [1] instanceof (cond ? A : B))"
        ]),
        "true false false false"
    );
}

/// A shadowing binding of a builtin name goes through the generic path.
#[test]
fn instanceof_shadowed_builtin_name() {
    assert_eq!(
        jsc(&[
            "-e",
            "class Array {}; let x = new Array(); \
             console.log(x instanceof Array, [1] instanceof Array)"
        ]),
        "true false"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "class X {}; let Error = X; console.log(new X() instanceof Error, ({}) instanceof Error)"
        ]),
        "true false"
    );
}

/// A non-callable right-hand side throws a `TypeError`, even for a primitive
/// left operand; the error is catchable.
#[test]
fn instanceof_non_callable_rhs_throws() {
    let msg = "Right-hand side of 'instanceof' is not callable";
    assert_uncaught_type_error("let x = {}; x instanceof 5", msg);
    assert_uncaught_type_error("let x = {}; x instanceof {}", msg);
    assert_uncaught_type_error("5 instanceof null", msg);
    assert_eq!(
        jsc(&[
            "-e",
            "let C = 1; try { ({}) instanceof C } catch (e) { console.log(e instanceof TypeError, e.message) }"
        ]),
        format!("true {msg}")
    );
}

/// A callable right-hand side whose `prototype` is not an object throws for an
/// object left operand, and is simply `false` for a primitive one.
#[test]
fn instanceof_non_object_prototype() {
    assert_uncaught_type_error(
        "let f = () => 1; ({}) instanceof f",
        "Function has non-object prototype",
    );
    assert_eq!(
        jsc(&["-e", "let f = () => 1; console.log(5 instanceof f)"]),
        "false"
    );
}

/// `o?.m(..)`, `o.m?.(..)`, and `p?.o.m(..)` bind `this` to the member base
/// exactly like `o.m(..)`; a nullish base still short-circuits.
#[test]
fn optional_call_binds_this() {
    assert_eq!(
        jsc(&[
            "-e",
            "let o = { v: 7, m(){ return this.v } }; let p = {o}; \
             console.log(o?.m(), o.m?.(), p?.o.m(), p.o?.m(), o?.[\"m\"]())"
        ]),
        "7 7 7 7 7"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "let n = null; let o = { v: 1 }; console.log(n?.m(), o.m?.(), n?.a.m())"
        ]),
        "undefined undefined undefined"
    );
}

/// The receiver and method of an optional method call are evaluated once, and
/// arguments are passed through.
#[test]
fn optional_call_evaluates_once() {
    assert_eq!(
        jsc(&[
            "-e",
            "let n = 0; let o = { v: 2, m(a){ return this.v + a } }; \
             function get(){ n++; return o } \
             console.log(get()?.m(1), get().m?.(2), n)"
        ]),
        "3 4 2"
    );
}

/// A bare optional call keeps `this` undefined.
#[test]
fn optional_bare_call_this_undefined() {
    assert_eq!(
        jsc(&[
            "-e",
            "function f(){ return this === undefined } let g = f; console.log(g?.(), f?.())"
        ]),
        "true true"
    );
}

/// A plain `function` constructor (S3 construct) links its instances to
/// `F.prototype`, so `instanceof F` holds; an object returned by the
/// constructor replaces `this` and is not an instance.
#[test]
fn instanceof_plain_function_constructor() {
    assert_eq!(
        jsc(&[
            "-e",
            "function F(){ this.v = 1 } function G(){ return {} } \
             console.log(new F() instanceof F, new G() instanceof G, ({}) instanceof F)"
        ]),
        "true false false"
    );
}

/// Construct and optional-call `this` together.
#[test]
fn construct_with_optional_call_this() {
    assert_eq!(
        jsc(&[
            "-e",
            "function F(){ this.v = 1 } let f = new F(); \
             console.log(f instanceof F, ({m(){ return this }})?.m() !== undefined)"
        ]),
        "true true"
    );
}
