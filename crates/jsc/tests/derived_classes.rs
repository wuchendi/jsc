//! Integration tests: derived classes (design 4.4-4.5) via the CLI — `super`
//! construction with `SELF_ALLOC`, the derived `this` TDZ, field order and
//! `CreateDataProperty` fields, `extends` of any expression, class expressions,
//! and static accessors.
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

const TDZ: &str = "ReferenceError: Must call super constructor in derived class before accessing 'this' or returning from derived constructor";

/// `super(args)` constructs through the parent; the derived instance is both a
/// `B` and an `A`.
#[test]
fn super_constructs_parent() {
    assert_eq!(
        run("class A { constructor(x){ this.x = x } }
             class B extends A { constructor(){ super(5); this.y = this.x + 1 } }
             let b = new B();
             console.log(b.x, b.y, b instanceof A, b instanceof B, b.constructor === B);"),
        "5 6 true true true\n"
    );
}

/// `this` before `super(..)`, a second `super(..)`, and a return without
/// `super(..)` are `ReferenceError`s.
#[test]
fn derived_this_tdz() {
    let stderr = uncaught(
        "class A { constructor(x){ this.x = x } }
         class B extends A { constructor(){ this.y = 1; super(1) } } new B()",
    );
    assert!(stderr.contains(TDZ), "{stderr}");
    let stderr = uncaught(
        "class A { m(){ return 1 } }
         class B extends A { constructor(){ super.m(); super() } } new B()",
    );
    assert!(stderr.contains(TDZ), "{stderr}");
    let stderr = uncaught("class A {} class B extends A { constructor(){ } } new B()");
    assert!(stderr.contains(TDZ), "{stderr}");
    let stderr =
        uncaught("class A {} class B extends A { constructor(){ super(); super() } } new B()");
    assert!(
        stderr.contains("ReferenceError: Super constructor may only be called once"),
        "{stderr}"
    );
    // Catchable, and `this` is usable after a guarded early access.
    assert_eq!(
        run("class A {} class B extends A { constructor(){
               try { this.x = 1 } catch (e) { console.log(e.name) }
               super(); this.x = 2 } }
             console.log(new B().x);"),
        "ReferenceError\n2\n"
    );
}

/// Base fields run right after allocation, derived fields right after
/// `super(..)` returns.
#[test]
fn field_order() {
    assert_eq!(
        run("function log(s){ console.log(s); return s }
             class A { a = log('A.a') }
             class B extends A { b = log('B.b'); constructor(){ log('pre'); super(); log('post') } }
             let b = new B(); console.log(b.a, b.b);"),
        "pre\nA.a\nB.b\npost\nA.a B.b\n"
    );
}

/// Field initializers define own data properties: an inherited setter is not
/// invoked.
#[test]
fn fields_define_not_set() {
    assert_eq!(
        run("class P { set f(v){ console.log('setter') } }
             class Q extends P { f = 1 }
             console.log(new Q().f);
             class R { set g(v){ console.log('setter') } g = 2 }
             console.log(new R().g);"),
        "1\n2\n"
    );
}

/// A plain constructor function or a mixin result can be extended.
#[test]
fn extends_any_expression() {
    assert_eq!(
        run("function Base(){ this.k = 1 } class D extends Base {}
             let d = new D(); console.log(d.k, d instanceof Base, d instanceof D);
             class A { constructor(){ this.a = 7 } }
             let M = (S) => class extends S { m(){ return 2 } };
             class E extends M(A) {}
             let e = new E(); console.log(e.a, e.m(), e instanceof A);
             let flag = false;
             class C2 { who(){ return 'C2' } }
             class F extends (flag ? A : C2) {}
             console.log(new F().who());"),
        "1 true true\n7 2 true\nC2\n"
    );
}

/// `extends` of a non-constructor is a `TypeError` at class-definition time.
#[test]
fn extends_non_constructor() {
    let stderr = uncaught("let X = 1; class B extends X {}");
    assert!(
        stderr.contains("TypeError: Class extends value 1 is not a constructor or null"),
        "{stderr}"
    );
    let stderr = uncaught("let f = () => 1; class B extends f {}");
    assert!(stderr.contains("is not a constructor or null"), "{stderr}");
    // `extends null`: the class defines, but constructing it throws.
    let stderr = uncaught("class N extends null {} console.log('defined'); new N()");
    assert!(stderr.contains("TypeError"), "{stderr}");
}

/// `super.m()` / `super.x` resolve through the home object's prototype at run
/// time, not by the parent's binding name.
#[test]
fn super_by_home_object() {
    assert_eq!(
        run("class A { m(){ return 'A' } get g(){ return 'gA' } }
             class B extends A { m(){ return super.m() + 'B' } g2(){ return super.g } }
             let Saved = B; A = null;
             console.log(new Saved().m(), new Saved().g2());"),
        "AB gA\n"
    );
    assert_eq!(
        run("class S { static who(){ return 'S' } }
             class T extends S { static who(){ return super.who() + 'T' } }
             console.log(T.who());"),
        "ST\n"
    );
}

/// The default derived constructor forwards every argument.
#[test]
fn default_derived_constructor() {
    assert_eq!(
        run("class A { constructor(a, b, c){ this.s = a + b + c } }
             class B extends A {} class C extends B {}
             console.log(new C(1, 2, 3).s);"),
        "6\n"
    );
}

/// The derived constructor's return value: an object replaces `this`, a
/// primitive after `super(..)` keeps it (a `TypeError` when not undefined), and
/// `undefined` without `super(..)` throws.
#[test]
fn derived_return_override() {
    assert_eq!(
        run(
            "class A {} class B extends A { constructor(){ super(); return {o: 1} } }
             console.log(new B().o);
             class C extends A { constructor(){ return {p: 2} } }
             console.log(new C().p);
             class D extends A { constructor(){ super(); this.d = 3; return } }
             console.log(new D().d);"
        ),
        "1\n2\n3\n"
    );
    let stderr =
        uncaught("class A {} class B extends A { constructor(){ super(); return 5 } } new B()");
    assert!(
        stderr.contains("TypeError: Derived constructors may only return object or undefined"),
        "{stderr}"
    );
    // A base class constructor returning a primitive keeps `this`.
    assert_eq!(
        run("class A { constructor(){ this.a = 1; return 5 } } console.log(new A().a);"),
        "1\n"
    );
}

/// Return override in a parent applies to `super(..)`: the derived `this` is
/// the parent's returned object.
#[test]
fn parent_return_override_binds_this() {
    assert_eq!(
        run("let o = {tag: 'o'};
             class A { constructor(){ return o } }
             class B extends A { constructor(){ super(); this.b = 1 } }
             let b = new B(); console.log(b === o, o.b, b instanceof B);"),
        "true 1 false\n"
    );
}

/// `new.target` inside a parent constructor is the derived class.
#[test]
fn new_target_through_super() {
    assert_eq!(
        run("class A { constructor(){ this.t = new.target } }
             class B extends A {}
             console.log(new B().t === B, new A().t === A);"),
        "true true\n"
    );
}

/// Spread arguments in `super(...)`.
#[test]
fn super_spread() {
    assert_eq!(
        run("class A { constructor(a, b){ this.s = a * b } }
             class B extends A { constructor(xs){ super(...xs) } }
             console.log(new B([6, 7]).s);"),
        "42\n"
    );
}

/// Class expressions: anonymous ones take the binding's name, a named one binds
/// its name (immutably) only inside the class.
#[test]
fn class_expressions() {
    assert_eq!(
        run("let C = class { m(){ return 1 } };
             console.log(new C().m(), C.name);
             let D = class Named { m(){ return Named } };
             console.log(D.name, new D().m() === D);
             function make(){ return class { constructor(v){ this.v = v } } }
             let K = make(); console.log(new K(4).v);
             let o = { cls: class {} }; console.log(o.cls.name);
             console.log(new (class { constructor(){ this.z = 9 } })().z);"),
        "1 C\nNamed true\n4\ncls\n9\n"
    );
    // The inner name is immutable.
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args([
            "-e",
            "let D = class Named { m(){ Named = 1 } }; new D().m()",
        ])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(output.status.code(), Some(2));
}

/// Static accessors live on the class value and are inherited by subclasses
/// (`C.[[Prototype]]` is the parent); static `this` is the class.
#[test]
fn static_accessors_and_inheritance() {
    assert_eq!(
        run(
            "class S { static get v(){ return 3 } static set v(x){ console.log('set', x) }
                       static base = 10; static twice(){ return this.base * 2 } }
             console.log(S.v); S.v = 4;
             class T extends S { static base = 20 }
             console.log(T.v, T.twice());"
        ),
        "3\nset 4\n3 40\n"
    );
}

/// A class expression's setter parameter widens the uniform arity even when no
/// call site passes an argument.
#[test]
fn class_expression_setter_arity() {
    assert_eq!(
        jsc_raw_stdout(&[
            "-p",
            "let C = class { set v(x){ this.w = x } }; let c = new C(); c.v = 9; c.w"
        ]),
        "9\n"
    );
}

/// An arrow in a derived constructor shares its `this`: it may call
/// `super(..)` (binding the constructor's `this`), and reads `this` / `super.x`
/// with the same TDZ.
#[test]
fn arrow_super_and_this_in_derived_constructor() {
    assert_eq!(
        run("let count = 0;
             class A { constructor(){ count++; this.a = 1 } m(){ return 5 } }
             class B extends A { constructor(){ (() => super())(); this.b = this.a + 1 } }
             let b = new B(); console.log(count, b.a, b.b, b instanceof B);
             class C extends A { constructor(){
               const g = () => this;
               try { g() } catch (e) { console.log(e.name) }
               super(); console.log(g() === this, (() => super.m())()) } }
             new C();"),
        "1 1 2 true\nReferenceError\ntrue 5\n"
    );
    let stderr = uncaught(
        "class A {} class B extends A { constructor(){ let f = () => super(); f(); f() } } new B()",
    );
    assert!(
        stderr.contains("ReferenceError: Super constructor may only be called once"),
        "{stderr}"
    );
}

/// Class member name early errors are compile errors (exit 2).
#[test]
fn class_member_name_early_errors() {
    for src in [
        "var C = class { static prototype; }",
        "class C { static prototype(){} }",
        "class C { static get prototype(){ return 1 } }",
        "class C { static constructor = 1 }",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-e", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert_eq!(output.status.code(), Some(2), "{src}");
    }
    // `prototype` instance members and a static method named `constructor`
    // are allowed.
    assert_eq!(
        run("class C { prototype = 1; static constructor(){ return 2 } }
             console.log(new C().prototype, C.constructor());"),
        "1 2\n"
    );
}

/// Builtin parents allocate their own kind with `proto = new.target.prototype`
/// (design 4.7): an `extends Error` instance renders and throws under its
/// subclass name, and the class label names the builtin parent.
#[test]
fn builtin_parents() {
    let class = r#"class E extends Error { constructor(m) { super(m); this.name = "E" } }"#;
    assert_eq!(
        run(&format!(
            "{class} const e = new E('hi'); console.log(e instanceof E, e instanceof Error, e.message, String(e)); console.log(E)"
        )),
        "true true hi E: hi\n[class E extends Error]\n"
    );
    assert!(uncaught(&format!("{class} throw new E('boom')")).contains("E: boom"));
    assert_eq!(
        run("class S extends Set {} const s = new S(); s.add(1); console.log(s, s instanceof S)"),
        "Set(1) true\n"
    );
}
