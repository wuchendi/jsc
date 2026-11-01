//! Integration tests: private class fields (DESIGN.md §7.6) — instance and
//! static `#x` fields, their initialization order, every access form, the
//! `TypeError`s, fresh names per class evaluation, and their invisibility to
//! every key walk and renderer. Expected texts are Node's output.

mod common;
use common::{assert_output, assert_uncaught};

/// `t(f)` runs `f` and renders its error (name and message).
const PRELUDE: &str =
    "function t(f) { try { f(); return 'ok' } catch (e) { return e.name + ': ' + e.message } }
";

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        assert_output(&format!("{PRELUDE}{body}"), expected);
    }
}

/// `#x = init` and `#x;` instance fields: read and written through methods,
/// an uninitialized field is `undefined`.
#[test]
fn instance_fields() {
    check(&[
        (
            "class C { #x = 1; get() { return this.#x } set(v) { this.#x = v } } \
             let c = new C(); console.log(c.get()); c.set(5); console.log(c.get())",
            "1\n5\n",
        ),
        (
            "class C { #x; read() { return this.#x } } console.log(new C().read())",
            "undefined\n",
        ),
        (
            "class C { #x = 1; #y = this.#x + 1; sum() { return this.#x + this.#y } } \
             let a = new C(), b = new C(); console.log(a.sum(), b.sum())",
            "3 3\n",
        ),
        (
            "class C { #s = 'a'; run() { return (this.#s = this.#s + 'b') + this.#s } } \
             console.log(new C().run())",
            "abab\n",
        ),
    ]);
}

/// Instance private fields are added interleaved with the public fields, in
/// source order; an initializer sees `this` and the fields before it, and an
/// anonymous function initializer is named after the field.
#[test]
fn initialization_order() {
    check(&[
        (
            "class C { a = 1; #b = this.a + 1; c = this.#b + 1; get b() { return this.#b } } \
             let o = new C(); console.log(o.b, o.c)",
            "2 3\n",
        ),
        (
            "let log = []; class C { a = log.push('a'); #b = log.push('b'); c = log.push('c'); \
             constructor() { log.push('ctor') } } new C(); console.log(log.join())",
            "a,b,c,ctor\n",
        ),
        (
            "class C { #y = this.#x; #x = 1; get y() { return this.#y } } console.log(t(() => new C()))",
            "TypeError: Cannot read private member #x from an object whose class did not declare it\n",
        ),
        (
            "class C { #f = function () {}; #g = () => 1; names() { return this.#f.name + ',' + this.#g.name } } \
             console.log(new C().names())",
            "#f,#g\n",
        ),
        (
            "class C { #x = 7; f = () => this.#x; } console.log(new C().f())",
            "7\n",
        ),
    ]);
}

/// `static #x = init` / `static #x;`: added to the class in source order with
/// the static public fields, `this` = the class; not inherited.
#[test]
fn static_fields() {
    check(&[
        (
            "class C { static #x = 10; static #y; static get() { return C.#x + String(C.#y) } } \
             console.log(C.get())",
            "10undefined\n",
        ),
        (
            "class C { static a = 1; static #b = this.a + 1; static c = this.#b + 1; \
             static b() { return this.#b } } console.log(C.b(), C.c)",
            "2 3\n",
        ),
        (
            "class C { static #n = 0; static inc() { return ++C.#n } } C.inc(); console.log(C.inc())",
            "2\n",
        ),
        (
            "class C { static #x = 1; static read(o) { return o.#x } } class D extends C {} \
             console.log(C.read(C), t(() => C.read(D)))",
            "1 TypeError: Cannot read private member #x from an object whose class did not declare it\n",
        ),
        (
            "class C { static #f = () => this; static same() { return C.#f() === C } } console.log(C.same())",
            "true\n",
        ),
    ]);
}

/// A derived class adds its private fields to the `this` `super()` returned.
#[test]
fn derived_class_after_super() {
    check(&[
        (
            "class A { constructor() { this.log = [] } } \
             class B extends A { #x = this.log.push('x'); constructor() { super(); this.log.push('ctor ' + this.#x) } } \
             console.log(new B().log.join())",
            "x,ctor 1\n",
        ),
        (
            "class A { #a = 'a'; getA() { return this.#a } } class B extends A { #b = 'b'; both() { return this.getA() + this.#b } } \
             console.log(new B().both())",
            "ab\n",
        ),
        (
            "class A { #a = 1; static has(o) { return t(() => o.#a) } } class B { #a = 2 } \
             console.log(A.has(new B()))",
            "TypeError: Cannot read private member #a from an object whose class did not declare it\n",
        ),
    ]);
}

/// Compound, logical (short-circuiting) and exponent assignment, `++` / `--`,
/// and destructuring targets.
#[test]
fn assignment_forms() {
    check(&[
        (
            "class C { #x = 1; run() { this.#x += 2; this.#x *= 3; this.#x **= 2; this.#x -= 1; return this.#x } } \
             console.log(new C().run())",
            "80\n",
        ),
        (
            "class C { #a = null; #b = 0; #c = 1; #d = 0; run() { let n = 0; this.#a ??= 'x'; this.#b ||= 'y'; this.#c &&= 'z'; \
             this.#c ??= (n++, 'no'); this.#b ||= (n++, 'no'); this.#d &&= (n++, 'no'); \
             return [this.#a, this.#b, this.#c, this.#d, n].join() } } console.log(new C().run())",
            "x,y,z,0,0\n",
        ),
        (
            "class C { #n = '4'; run() { let a = this.#n++; let b = ++this.#n; let c = this.#n--; let d = --this.#n; \
             return [a, typeof a, b, c, d, this.#n].join() } } console.log(new C().run())",
            "4,number,6,6,4,4\n",
        ),
        (
            "let calls = 0; class C { #x = 1; static get(o) { calls++; return o } static run(o) { C.get(o).#x += 10; C.get(o).#x++; return o.#x } } \
             console.log(C.run(new C()), calls)",
            "12 2\n",
        ),
        (
            "class C { #a; #b; run() { [this.#a, this.#b] = [1, 2]; ({ x: this.#a } = { x: 9 }); return this.#a + this.#b } } \
             console.log(new C().run())",
            "11\n",
        ),
    ]);
}

/// `o.#f(..)` calls with `this = o`, reading the field before the arguments
/// run; optional chains short-circuit.
#[test]
fn calls_and_optional_chains() {
    check(&[
        (
            "let c; class C { #f = function () { return this === c }; #g = (a, b) => a + b; run() { return [this.#f(), this.#g(1, 2)].join() } } \
             c = new C(); console.log(c.run())",
            "true,3\n",
        ),
        (
            "class C { #f = 1; static call(o) { let log = []; try { o.#f(log.push('arg')) } catch (e) { log.push(e.message) } return log.join('|') } } \
             console.log(C.call({})); console.log(C.call(new C()))",
            "Cannot read private member #f from an object whose class did not declare it\narg|o.#f is not a function\n",
        ),
        (
            "class C { #f = (...a) => a.join(); run(xs) { return this.#f(...xs) } } console.log(new C().run([1, 2, 3]))",
            "1,2,3\n",
        ),
        (
            "class C { #x = 5; static g(o) { return o?.#x } } console.log(C.g(new C()), C.g(null), C.g(undefined))",
            "5 undefined undefined\n",
        ),
        (
            "class C { #m = function () { return this.#x }; #x = 3; static g(o) { return o?.#m() } static h(o) { return o.#n?.() } #n; } \
             console.log(C.g(new C()), C.g(null), C.h(new C()))",
            "3 undefined undefined\n",
        ),
        (
            "class C { #inner = { v: 4 }; static g(o) { return o?.#inner.v } } console.log(C.g(new C()), C.g(undefined))",
            "4 undefined\n",
        ),
    ]);
}

/// `#x` resolves lexically: nested functions, arrows and classes in the body
/// see it; a nested class declaring the same name shadows it.
#[test]
fn lexical_resolution() {
    check(&[
        (
            "class C { #x = 1; m() { function inner(o) { return o.#x } let arrow = () => this.#x; \
             class D { #y = 2; read(o) { return o.#x + this.#y } } return inner(this) + arrow() + new D().read(this) } } \
             console.log(new C().m())",
            "5\n",
        ),
        (
            "class C { #x = 'outer'; m() { class D { #x = 'inner'; static r(o) { return o.#x } } \
             return D.r(new D()) + ',' + t(() => D.r(this)) } } console.log(new C().m())",
            "inner,TypeError: Cannot read private member #x from an object whose class did not declare it\n",
        ),
        (
            "class C { #x = 1; static f() { return function (o) { return (() => o.#x)() } } } console.log(C.f()(new C()))",
            "1\n",
        ),
    ]);
}

/// Each evaluation of a class body mints fresh private names.
#[test]
fn fresh_names_per_evaluation() {
    check(&[
        (
            "function make() { return class { #x = 1; static has(o) { try { o.#x; return true } catch (e) { return false } } } } \
             let A = make(), B = make(); console.log(A.has(new A()), A.has(new B()), B.has(new B()))",
            "true false true\n",
        ),
        (
            "let cs = []; for (let i = 0; i < 2; i++) cs.push(class { #v = i; static v(o) { return o.#v } }); \
             console.log(cs[0].v(new cs[0]()), cs[1].v(new cs[1]()), t(() => cs[0].v(new cs[1]())))",
            "0 1 TypeError: Cannot read private member #v from an object whose class did not declare it\n",
        ),
    ]);
}

/// The `TypeError`s: a missing field (wrong object or primitive receiver) on
/// read and write, and a second initialization through a constructor return
/// override.
#[test]
fn type_errors() {
    check(&[
        (
            "class C { #x = 1; static r(o) { return o.#x } static w(o) { o.#x = 2 } } \
             console.log(t(() => C.r({}))); console.log(t(() => C.w({}))); \
             console.log(t(() => C.r(1))); console.log(t(() => C.r('s'))); console.log(t(() => C.r(undefined))); \
             console.log(t(() => C.w(true))); console.log(t(() => C.w(null)))",
            "TypeError: Cannot read private member #x from an object whose class did not declare it\n\
             TypeError: Cannot write private member #x to an object whose class did not declare it\n\
             TypeError: Cannot read private member #x from an object whose class did not declare it\n\
             TypeError: Cannot read private member #x from an object whose class did not declare it\n\
             TypeError: Cannot read private member #x from an object whose class did not declare it\n\
             TypeError: Cannot write private member #x to an object whose class did not declare it\n\
             TypeError: Cannot write private member #x to an object whose class did not declare it\n",
        ),
        (
            "class C { #x = 1; static w(o) { let log = []; try { o.#x = log.push('rhs') } catch (e) { log.push(e.name) } return log.join() } } \
             console.log(C.w({}))",
            "rhs,TypeError\n",
        ),
        (
            "class C { #x = 1; static inc(o) { o.#x++ } } console.log(t(() => C.inc({})))",
            "TypeError: Cannot read private member #x from an object whose class did not declare it\n",
        ),
        (
            "class Base { constructor(o) { return o } } class Stamp extends Base { #x = 42; static get(o) { return o.#x } } \
             let obj = {}; new Stamp(obj); console.log(Stamp.get(obj)); console.log(t(() => new Stamp(obj)))",
            "42\nTypeError: Cannot initialize #x twice on the same object\n",
        ),
        (
            "class Base { constructor(o) { return o } } \
             class Stamp extends Base { #y = 1; static has(o) { try { o.#y; return true } catch (e) { return false } } } \
             let f = function () {}; new Stamp(f); console.log(Stamp.has(f), Stamp.has(function () {}))",
            "true false\n",
        ),
    ]);
}

/// An uncaught private-field `TypeError` renders like any other.
#[test]
fn uncaught_type_error() {
    assert_uncaught(
        "class C { #x; static r(o) { return o.#x } } C.r({})",
        "TypeError: Cannot read private member #x from an object whose class did not declare it",
    );
}

/// `PrivateFieldAdd` works on non-extensible, sealed and frozen objects, and
/// `freeze` / `isFrozen` / `isSealed` leave private entries alone.
#[test]
fn integrity_levels() {
    check(&[
        (
            "class Base { constructor(o) { return o } } \
             class Stamp extends Base { #x = 1; static get(o) { return o.#x } static set(o, v) { o.#x = v } } \
             let f = Object.freeze({}); new Stamp(f); console.log(Stamp.get(f)); Stamp.set(f, 2); \
             console.log(Stamp.get(f), Object.isFrozen(f), Object.isExtensible(f), Object.keys(f).length); \
             let s = Object.seal({ a: 1 }); new Stamp(s); console.log(Stamp.get(s), Object.isSealed(s)); \
             let p = Object.preventExtensions({}); new Stamp(p); console.log(Stamp.get(p), Object.isExtensible(p))",
            "1\n2 true false 0\n1 true\n1 false\n",
        ),
        (
            "class C { #x = 1; inc() { return ++this.#x } } let c = Object.freeze(new C()); \
             console.log(c.inc(), c.inc(), Object.isFrozen(c), Object.isSealed(c))",
            "2 3 true true\n",
        ),
        (
            "class C { static #x = 1; static inc() { return ++C.#x } } Object.freeze(C); console.log(C.inc(), Object.isFrozen(C))",
            "2 true\n",
        ),
    ]);
}

/// Private entries are invisible to every key walk, `JSON.stringify` and
/// the renderer.
#[test]
fn invisibility() {
    check(&[
        (
            "class C { #x = 1; a = 3; [Symbol.iterator] = 0 } let c = new C(); \
             console.log(Object.getOwnPropertyNames(c).join(), Object.getOwnPropertySymbols(c).length, Object.keys(c).join(), \
             Object.values(c).join(), Object.entries(c).join())",
            "a 1 a 3 a,3\n",
        ),
        (
            "class C { #x = 1; a = 3 } let c = new C(); let ks = []; for (let k in c) ks.push(k); \
             let { ...r } = c; \
             console.log(ks.join(), Object.keys(r).join(), Object.getOwnPropertySymbols(r).length, \
             Object.getOwnPropertySymbols(Object.assign({}, c)).length, \
             Object.keys(Object.getOwnPropertyDescriptors(c)).join(), Object.getOwnPropertySymbols(Object.getOwnPropertyDescriptors(c)).length)",
            "a a 0 0 a 0\n",
        ),
        (
            "class C { #x = 1; a = 3; static #s = 2; static b = 4 } let c = new C(); \
             console.log(JSON.stringify(c), Object.keys(C).join(), Object.getOwnPropertySymbols(C).length); console.log(c)",
            "{\"a\":3} b 0\nC { a: 3 }\n",
        ),
        (
            "class C { #x = 1 } let c = new C(); console.log(c, JSON.stringify(c), Object.getOwnPropertyNames(c).length)",
            "C {} {} 0\n",
        ),
    ]);
}

/// Private fields in generator methods, with a suspend inside the access.
#[test]
fn generator_methods() {
    check(&[
        (
            "class C { #f = (a) => a * 2; *g() { return this.#f(yield 1) } } let it = new C().g(); it.next(); \
             console.log(it.next(21).value)",
            "42\n",
        ),
        (
            "class C { #n = 1; *g() { this.#n += yield 'w'; return this.#n } } let it = new C().g(); \
             console.log(it.next().value, it.next(4).value)",
            "w 5\n",
        ),
        (
            "class C { #n = null; *g() { this.#n ??= yield 'w'; return this.#n } } let it = new C().g(); \
             console.log(it.next().value, it.next(7).value)",
            "w 7\n",
        ),
    ]);
}

/// Private methods and accessors stay a spanned compile error until they land.
#[test]
fn private_methods_rejected() {
    for src in [
        "class C { #m() {} }",
        "class C { get #a() { return 1 } }",
        "class C { static #m() {} }",
    ] {
        let err = jsc_core::run::compile_and_run(src).expect_err("private methods are rejected");
        assert!(
            err.to_string()
                .contains("private class methods are not supported"),
            "unexpected error for `{src}`: {err}"
        );
    }
}
