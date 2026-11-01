//! Integration tests: private class members (DESIGN.md §7.6) — instance and
//! static `#x` fields, methods and accessors (generator and async ones
//! included), their initialization order, every access form, brand checks,
//! `#x in o`, the `TypeError`s, fresh names per class evaluation, and their
//! invisibility to every key walk and renderer. Expected texts are Node's
//! output, except the method and accessor messages, which follow DESIGN.md
//! §7.6 (Node words the brand-check and not-writable errors differently).

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

/// `#m() {}`: one closure per class evaluation, shared by every instance,
/// named `"#m"`; `this` and `super.x` work in its body.
#[test]
fn instance_methods() {
    check(&[
        (
            "class C { #x = 2; #m() { return this.#x * 10 } run() { return this.#m() } } \
             console.log(new C().run())",
            "20\n",
        ),
        (
            "class C { #m() {} static f(o) { return o.#m } } let a = new C(), b = new C(); \
             console.log(C.f(a) === C.f(b), typeof C.f(a), C.f(a).name)",
            "true function #m\n",
        ),
        (
            "function make() { return class { #m() {} static f(o) { return o.#m } } } \
             let A = make(), B = make(); console.log(A.f(new A()) === B.f(new B()), t(() => A.f(new B())))",
            "false TypeError: Cannot read private member #m from an object whose class did not declare it\n",
        ),
        (
            "class A { hi() { return 'A' } } class B extends A { #m() { return super.hi() + 'B' } run() { return this.#m() } } \
             console.log(new B().run())",
            "AB\n",
        ),
        (
            "class C { #m(a, b) { return [this instanceof C, a, b].join() } run() { let f = this.#m; return this.#m(1, 2) + '|' + f.call(this, 3) } } \
             console.log(new C().run())",
            "true,1,2|true,3,\n",
        ),
        (
            "class C { #m() { return 1 } static f(o) { return o?.#m() } } console.log(C.f(new C()), C.f(null))",
            "1 undefined\n",
        ),
    ]);
}

/// `static #m()`: the class is its only holder (`this` = the class in a
/// static method); `super.x` reads the parent constructor; not inherited.
#[test]
fn static_methods() {
    check(&[
        (
            "class C { static #m() { return this === C } static run() { return C.#m() } } console.log(C.run())",
            "true\n",
        ),
        (
            "class A { static hi() { return 'sA' } } class B extends A { static #m() { return super.hi() } static run() { return B.#m() } } \
             console.log(B.run())",
            "sA\n",
        ),
        (
            "class C { static #m() { return 1 } static r(o) { return o.#m() } } class D extends C {} \
             console.log(C.r(C), t(() => C.r(D)), t(() => C.r(new C())))",
            "1 TypeError: Cannot read private member #m from an object whose class did not declare it \
             TypeError: Cannot read private member #m from an object whose class did not declare it\n",
        ),
    ]);
}

/// `get #x` / `set #x`: called with `this` = the receiver; a write yields the
/// assigned value; compound, logical and update forms call the getter then
/// the setter; destructuring targets call the setter; static accessors.
#[test]
fn accessors() {
    check(&[
        (
            "class C { #v = 1; get #x() { return this.#v } set #x(v) { this.#v = v * 2 } run() { this.#x = 5; return this.#x } } \
             console.log(new C().run())",
            "10\n",
        ),
        (
            "class C { set #x(v) {} run() { return (this.#x = 7) } } console.log(new C().run())",
            "7\n",
        ),
        (
            "let c; class C { get #x() { return this === c } run() { return this.#x } } c = new C(); console.log(c.run())",
            "true\n",
        ),
        (
            "class C { #v = 1; log = []; get #x() { this.log.push('get'); return this.#v } \
             set #x(v) { this.log.push('set ' + v); this.#v = v } \
             run() { this.#x += 2; this.#x++; ++this.#x; this.#x ??= 9; return this.log.join() + '|' + this.#v } } \
             console.log(new C().run())",
            "get,set 3,get,set 4,get,set 5,get|5\n",
        ),
        (
            "class C { #v = '4'; get #x() { return this.#v } set #x(v) { this.#v = v } \
             run() { let a = this.#x++; return [a, typeof a, this.#v].join() } } console.log(new C().run())",
            "4,number,5\n",
        ),
        (
            "class C { #v = 0; set #x(v) { this.#v = v } get #x() { return this.#v } \
             run() { [this.#x] = [3]; ({ a: this.#x } = { a: this.#x + 1 }); return this.#v } } console.log(new C().run())",
            "4\n",
        ),
        (
            "class C { static #v = 1; static get #x() { return C.#v } static set #x(v) { C.#v = v } \
             static run() { C.#x = 3; return this.#x } } console.log(C.run())",
            "3\n",
        ),
        (
            "class C { get #x() { return 'g' } static f(o) { return o?.#x } } console.log(C.f(new C()), C.f(undefined))",
            "g undefined\n",
        ),
    ]);
}

/// The instance brand is added before every field (on constructor entry, or
/// after `super()` returns); static methods exist before static fields run.
#[test]
fn install_order() {
    check(&[
        (
            "class C { #x = this.#m(); #m() { return 5 } get x() { return this.#x } } console.log(new C().x)",
            "5\n",
        ),
        (
            "class C { a = this.#g; get #g() { return 'g' } } console.log(new C().a)",
            "g\n",
        ),
        (
            "class A { constructor(f) { this.seen = f(this) } } \
             class B extends A { #m() { return 1 } constructor() { super((o) => t(() => B.has(o))) } static has(o) { return o.#m() } } \
             console.log(new B().seen, B.has(new B()))",
            "TypeError: Cannot read private member #m from an object whose class did not declare it 1\n",
        ),
        (
            "class C { static a = C.#m(); static #m() { return 'sm' } } console.log(C.a)",
            "sm\n",
        ),
        (
            "class C { static #m() { return this === C } static a = this.#m() } console.log(C.a)",
            "true\n",
        ),
        (
            "class C { #m() { return 1 } } console.log(Object.getOwnPropertyNames(new C()).length, \
             Object.getOwnPropertySymbols(new C()).length, Object.getOwnPropertyNames(C.prototype).join())",
            "0 0 constructor\n",
        ),
    ]);
}

/// The method and accessor `TypeError`s: writing a method, a getter-less
/// read, a setter-less write (also compound), an absent brand (the value is
/// evaluated first on a write), and a second brand add.
#[test]
fn method_type_errors() {
    check(&[
        (
            "class C { #m() {} static w(o) { o.#m = 1 } } console.log(t(() => C.w(new C()))); console.log(t(() => C.w({})))",
            "TypeError: Private method is not writable\n\
             TypeError: Cannot write private member #m to an object whose class did not declare it\n",
        ),
        (
            "class C { set #x(v) {} static r(o) { return o.#x } } console.log(t(() => C.r(new C())))",
            "TypeError: '#x' was defined without a getter\n",
        ),
        (
            "class C { get #x() { return 1 } static w(o) { o.#x = 1 } static c(o) { o.#x += 1 } } \
             console.log(t(() => C.w(new C()))); console.log(t(() => C.c(new C())))",
            "TypeError: '#x' was defined without a setter\nTypeError: '#x' was defined without a setter\n",
        ),
        (
            "class C { #m() {} get #x() { return 1 } static r(o) { return o.#m } static g(o) { return o.#x } } \
             console.log(t(() => C.r({}))); console.log(t(() => C.r(1))); console.log(t(() => C.g({})))",
            "TypeError: Cannot read private member #m from an object whose class did not declare it\n\
             TypeError: Cannot read private member #m from an object whose class did not declare it\n\
             TypeError: Cannot read private member #x from an object whose class did not declare it\n",
        ),
        (
            "class C { #m() {} set #x(v) {} static w(o) { let log = []; try { o.#m = log.push('m') } catch (e) { log.push(e.message) } \
             try { o.#x = log.push('x') } catch (e) { log.push(e.message) } return log.join('|') } } console.log(C.w({}))",
            "m|Cannot write private member #m to an object whose class did not declare it|\
             x|Cannot write private member #x to an object whose class did not declare it\n",
        ),
        (
            "class C { #m() {} static call(o) { let log = []; try { o.#m(log.push('arg')) } catch (e) { log.push(e.message) } return log.join('|') } } \
             console.log(C.call({}))",
            "Cannot read private member #m from an object whose class did not declare it\n",
        ),
        (
            "class Base { constructor(o) { return o } } class Stamp extends Base { #m() { return 1 } static f(o) { return o.#m() } } \
             let obj = {}; new Stamp(obj); console.log(Stamp.f(obj), t(() => new Stamp(obj)))",
            "1 TypeError: Cannot initialize #m twice on the same object\n",
        ),
    ]);
}

/// An uncaught private-method `TypeError` renders like any other.
#[test]
fn uncaught_method_type_error() {
    assert_uncaught(
        "class C { #m() {} static w(o) { o.#m = 1 } } C.w(new C())",
        "TypeError: Private method is not writable",
    );
}

/// `#x in o`: fields, methods, accessors and statics; no prototype walk and
/// no getter call; a non-object right-hand side throws.
#[test]
fn private_in() {
    check(&[
        (
            "class C { #f = 1; #m() {} get #a() { return 1 } static #s = 1; static #sm() {} \
             static test(o) { return [#f in o, #m in o, #a in o, #s in o, #sm in o].join() } } \
             console.log(C.test(new C())); console.log(C.test(C)); console.log(C.test({}))",
            "true,true,true,false,false\nfalse,false,false,true,true\nfalse,false,false,false,false\n",
        ),
        (
            "class C { #x; static f(o) { return #x in o } } \
             console.log(t(() => C.f(1))); console.log(t(() => C.f(undefined))); console.log(t(() => C.f('s')))",
            "TypeError: Cannot use 'in' operator to search for '#x' in 1\n\
             TypeError: Cannot use 'in' operator to search for '#x' in undefined\n\
             TypeError: Cannot use 'in' operator to search for '#x' in s\n",
        ),
        (
            "class C { #x; static f(o) { return #x in o } } console.log(C.f(Object.create(new C())), C.f(function () {}))",
            "false false\n",
        ),
        (
            "let n = 0; class C { get #g() { n++; return 1 } static f(o) { return #g in o } } console.log(C.f(new C()), n)",
            "true 0\n",
        ),
        (
            "class C { static #m() {} static f(o) { return #m in o } } class D extends C {} console.log(C.f(C), C.f(D))",
            "true false\n",
        ),
        (
            "class C { #x = 1; static f(o) { if (#x in o) return o.#x; return 'no' } } console.log(C.f(new C()), C.f({}))",
            "1 no\n",
        ),
    ]);
}

/// Generator, async and async generator private methods (instance and
/// static), and suspends inside private accessor and method accesses.
#[test]
fn generator_and_async_private_methods() {
    check(&[
        (
            "class C { #v = 3; *#g() { yield this.#v; yield this.#v + 1 } run() { return [...this.#g()].join() } } \
             console.log(new C().run())",
            "3,4\n",
        ),
        (
            "class C { static *#g() { yield 1; yield 2 } static run() { return [...C.#g()].join() } } console.log(C.run())",
            "1,2\n",
        ),
        (
            "class C { #m() {} *#g() {} async #a() {} async *#ag() {} \
             static names(o) { return [o.#m.name, o.#g.name, o.#a.name, o.#ag.name].join() } } console.log(C.names(new C()))",
            "#m,#g,#a,#ag\n",
        ),
        (
            "class C { #v = 2; async #a() { await null; return this.#v * 5 } run() { return this.#a() } } \
             new C().run().then((v) => console.log(v))",
            "10\n",
        ),
        (
            "class C { #k = 1; async *#ag() { yield this.#k; yield await 2 } \
             async run() { let it = this.#ag(); let a = await it.next(); let b = await it.next(); return a.value + b.value } } \
             new C().run().then((v) => console.log(v))",
            "3\n",
        ),
        (
            "class C { static async #a() { return 'sa' } static run() { return C.#a() } } C.run().then((v) => console.log(v))",
            "sa\n",
        ),
        (
            "class C { #v = 0; get #x() { return this.#v } set #x(v) { this.#v = v } \
             *g() { this.#x = yield 'a'; this.#x += yield 'b'; return this.#x } } \
             let it = new C().g(); console.log(it.next().value, it.next(2).value, it.next(5).value)",
            "a b 7\n",
        ),
        (
            "class C { #m(a) { return a + 1 } *g() { return this.#m(yield 0) } } let it = new C().g(); it.next(); \
             console.log(it.next(41).value)",
            "42\n",
        ),
        (
            "class C { get #x() { return 'got' } *g() { return (yield 0).#x } } let c = new C(); let it = c.g(); it.next(); \
             console.log(it.next(c).value)",
            "got\n",
        ),
    ]);
}

/// A repeated private name is a spanned compile error, except one getter
/// plus one setter of the same static-ness.
#[test]
fn duplicate_private_names() {
    for src in [
        "class C { #a; #a() {} }",
        "class C { #a() {} #a() {} }",
        "class C { get #a() {} get #a() {} }",
        "class C { get #a() {} static set #a(v) {} }",
        "class C { #a() {} set #a(v) {} }",
    ] {
        let err = jsc_core::run::compile_and_run(src).expect_err("duplicate names are rejected");
        assert!(
            err.to_string().contains("duplicate private name #a"),
            "unexpected error for `{src}`: {err}"
        );
    }
    check(&[(
        "class C { static get #a() { return 1 } static set #a(v) {} static f() { return C.#a } } console.log(C.f())",
        "1\n",
    )]);
}

/// Private method bodies and private field initializers capture enclosing
/// bindings, and their parameter counts widen the call arity.
#[test]
fn captures_and_arity() {
    check(&[
        (
            "function f() { let k = 5; class C { #m() { return k } run() { return this.#m() } } return new C().run() } \
             console.log(f())",
            "5\n",
        ),
        (
            "function g() { let z = 'z'; class C { #v = () => z; static #s = z + '!'; run() { return this.#v() + C.#s } } \
             return new C().run() } console.log(g())",
            "zz!\n",
        ),
        (
            "class C { #m(a, b, c, d, e, f, g) { return [a, g].join() } run() { return this.#m(1) } } console.log(new C().run())",
            "1,\n",
        ),
    ]);
}
