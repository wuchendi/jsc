//! Integration tests: `new X(args)` construct semantics (design 4.5) via the CLI —
//! plain constructor functions, return override, `new.target`, class
//! constructors called without `new`, non-constructors, and spread arguments.
#![allow(clippy::expect_used)]

mod common;
use common::jsc_raw_stdout;

use std::process::Command;

/// Run `jsc -e src`, assert it exits with code 1 (uncaught), and return stderr.
fn uncaught(src: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", src])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(
        output.status.code(),
        Some(1),
        "jsc -e {src:?}: stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stderr).expect("stderr should be UTF-8")
}

fn run(src: &str) -> String {
    jsc_raw_stdout(&["-e", src])
}

/// A plain `function` is a constructor: `this` is a fresh object linked to
/// `F.prototype`, whose `constructor` is `F`.
#[test]
fn plain_function_constructs() {
    assert_eq!(
        run("function P(x){ this.x = x }
             P.prototype.get = function(){ return this.x };
             console.log(new P(3).get());
             console.log(new P(1).constructor === P);"),
        "3\ntrue\n"
    );
    // Function expressions and nested declarations construct too.
    assert_eq!(
        run("let Q = function(a){ this.a = a }; console.log(new Q(7).a);
             function outer(){ function In(){ this.k = 1 } return new In().k }
             console.log(outer());"),
        "7\n1\n"
    );
}

/// The constructor's return value replaces `this` only when it is an object.
#[test]
fn return_override() {
    assert_eq!(
        run("function A(){ this.a = 1; return {b: 2} }
             let x = new A(); console.log(x.a, x.b);
             function B(){ this.a = 1; return 5 }
             console.log(new B().a);
             function R(){ return [1, 2] } console.log(new R().length);
             function G(){ return () => 9 } console.log(new G()());
             class C { constructor(){ this.c = 1; return {d: 4} } }
             let c = new C(); console.log(c.c, c.d);
             class D { constructor(){ this.c = 1; return 'no' } }
             console.log(new D().c);"),
        "undefined 2\n1\n2\n9\nundefined 4\n1\n"
    );
}

/// `new.target` is the constructor under `new` and `undefined` on a plain call;
/// an arrow sees its enclosing function's value.
#[test]
fn new_target() {
    assert_eq!(
        run("function F(){ console.log(new.target === F) } new F(); F();"),
        "true\nfalse\n"
    );
    assert_eq!(
        run(
            "function F(){ let g = () => new.target; let h = () => () => new.target;
                           console.log(g() === F, h()() === F) }
             new F(); F();"
        ),
        "true true\nfalse false\n"
    );
    assert_eq!(
        run(
            "function F(){ return new.target } console.log(F() === undefined);
             let o = { m(){ return new.target } }; console.log(o.m());
             class K { constructor(){ this.t = new.target } } console.log(new K().t === K);"
        ),
        "true\nundefined\ntrue\n"
    );
}

/// `new.target` outside any function is a compile error (a `SyntaxError` in JS).
#[test]
fn new_target_at_top_level_is_compile_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "new.target"])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(output.status.code(), Some(2));
}

/// A class constructor called without `new` throws; class identity is by
/// binding, so a second binding constructs it.
#[test]
fn class_ctor_requires_new() {
    let err = uncaught("class C {}; C()");
    assert!(
        err.contains("TypeError: Class constructor C cannot be invoked without 'new'"),
        "{err}"
    );
    assert_eq!(
        run(
            "class C { constructor(v){ this.v = v } } let D = C; console.log(new D(5).v);
             try { D() } catch (e) { console.log(e instanceof TypeError, e.message) }"
        ),
        "5\ntrue Class constructor C cannot be invoked without 'new'\n"
    );
}

/// Arrows and methods are not constructors.
#[test]
fn non_constructors_throw() {
    let err = uncaught("let f = () => 1; new f()");
    assert!(err.contains("TypeError: f is not a constructor"), "{err}");
    let err = uncaught("let o = {m(){}}; new o.m()");
    assert!(err.contains("TypeError: o.m is not a constructor"), "{err}");
    let err = uncaught("class A { m(){} } new (new A().m)()");
    assert!(err.contains("is not a constructor"), "{err}");
    let err = uncaught("function g(){ return 1 } let x = 5; new x()");
    assert!(err.contains("TypeError: x is not a constructor"), "{err}");
}

/// Any callee expression constructs: a member, a call result, a parenthesized
/// function expression.
#[test]
fn any_callee_expression() {
    assert_eq!(
        run(
            "let ns = { P: function(x){ this.x = x } }; console.log(new ns.P(4).x);
             function mk(){ return function(){ this.y = 2 } } console.log(new (mk())().y);
             console.log(new (function(a, b){ this.s = a + b })(1, 2).s);"
        ),
        "4\n2\n3\n"
    );
}

/// The rest-parameter prologue decodes the construct marker, with or without
/// spread arguments at the `new` site.
#[test]
fn rest_and_spread_args() {
    assert_eq!(
        run("function V(...xs){ this.n = xs.length }
             console.log(new V(1, 2).n, new V(...[1, 2, 3]).n, new V().n);
             class W { constructor(a, ...r){ this.a = a; this.r = r.length } }
             let w = new W(...[1, 2, 3]); console.log(w.a, w.r);"),
        "2 3 0\n1 2\n"
    );
}

/// Existing class semantics keep working through the construct path: field
/// initializers before the body, base and derived classes, `super(...)`.
#[test]
fn classes_through_construct() {
    assert_eq!(
        run("class A { f = 1; constructor(x){ this.g = this.f + x } }
             class B extends A { h = 3; constructor(){ super(10); this.i = this.h } }
             let b = new B(); console.log(b.f, b.g, b.h, b.i);
             class E extends A {} console.log(new E(5).g);"),
        "1 11 3 3\n6\n"
    );
}
