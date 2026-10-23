//! Generator and async METHODS (object literal and class, instance and
//! static) and the generator `prototype` property: every generator function
//! owns a fresh `prototype` inheriting `%GeneratorPrototype%`, which the
//! generator objects it creates inherit.

mod common;

use common::{assert_output, assert_uncaught};

/// Compile `src` and assert it is rejected at compile time with a message
/// containing `message`.
#[allow(clippy::expect_used)]
fn assert_compile_error(src: &str, message: &str) {
    let err = jsc_core::run::compile_and_run(src).expect_err("program should not compile");
    let text = err.to_string();
    assert!(
        text.contains(message),
        "expected a compile error containing `{message}` for `{src}`, got `{text}`"
    );
}

#[test]
fn an_object_generator_method_iterates_in_for_of_and_spread() {
    assert_output(
        "const o = { n: 3, *count(){ for (let i = 1; i <= this.n; i++) yield i; } };
         const seen = []; for (const v of o.count()) seen.push(v);
         console.log(seen, [...o.count()], o.count.name, o.count.length);",
        "[ 1, 2, 3 ] [ 1, 2, 3 ] count 0\n",
    );
}

#[test]
fn object_generator_methods_take_string_and_numeric_keys() {
    assert_output(
        "const o = { *'a b'(x){ yield x; }, *2(){ yield 'two'; } };
         console.log([...o['a b'](1)], [...o[2]()], Object.keys(o));",
        "[ 1 ] [ two ] [ 2, a b ]\n",
    );
}

#[test]
fn an_object_async_method_awaits_with_this() {
    assert_output(
        "const o = { base: 40, async add(x){ const v = await Promise.resolve(x); return this.base + v; } };
         o.add(2).then(v => console.log(v, o.add.name));",
        "42 add\n",
    );
}

#[test]
fn class_instance_and_static_generator_methods_use_this() {
    assert_output(
        "class Bag {
           constructor(...xs){ this.xs = xs; }
           *items(){ for (const x of this.xs) yield x * 10; }
           static *range(n){ for (let i = 0; i < n; i++) yield this.name + i; }
         }
         console.log([...new Bag(1, 2).items()], [...Bag.range(2)]);
         console.log(Object.keys(Bag.prototype), Object.keys(Bag));",
        "[ 10, 20 ] [ Bag0, Bag1 ]\n[] []\n",
    );
}

#[test]
fn a_class_async_method_awaits_and_uses_this_and_super() {
    assert_output(
        "class A { m(){ return 'A' + this.k; } }
         class B extends A {
           constructor(){ super(); this.k = 1; }
           async m(){ const v = await Promise.resolve('!'); return super.m() + v + this.k; }
           static async s(){ await null; return this.name; }
         }
         new B().m().then(v => console.log(v));
         B.s().then(v => console.log(v));",
        "A1!1\nB\n",
    );
}

#[test]
fn super_property_access_survives_suspension_in_a_generator_method() {
    assert_output(
        "class A { get tag(){ return 'a'; } m(){ return 'A'; } }
         class B extends A { *g(){ yield super.tag; yield super.m(); yield super.tag + this.v; } }
         const b = new B(); b.v = 7;
         console.log([...b.g()]);",
        "[ a, A, a7 ]\n",
    );
}

#[test]
fn a_super_call_after_a_suspend_keeps_this() {
    assert_output(
        "class A { m(x){ return this.k + x; } }
         class B extends A { *g(){ const x = yield 'ask'; yield super.m(x); yield (yield 'again', super.m('?')); } }
         const b = new B(); b.k = 'k';
         const it = b.g();
         console.log(it.next().value, it.next('!').value, it.next().value, it.next().value);",
        "ask k! again k?\n",
    );
}

#[test]
fn a_super_call_with_a_suspending_argument_is_a_compile_error() {
    assert_compile_error(
        "class A { m(x){ return x; } } class B extends A { *g(){ return super.m(yield 1); } }",
        "would lose a compiler temporary across the suspend",
    );
}

#[test]
fn a_generator_method_is_not_a_constructor() {
    assert_uncaught(
        "const o = { *g(){ yield 1; } }; new o.g();",
        "TypeError: o.g is not a constructor",
    );
}

#[test]
fn an_async_method_is_not_a_constructor() {
    assert_uncaught(
        "class C { async m(){} } const c = new C(); new c.m();",
        "TypeError: c.m is not a constructor",
    );
}

#[test]
fn a_generator_function_has_its_own_prototype() {
    assert_output(
        "function* g(){ yield 1; }
         const p = g.prototype;
         console.log(typeof p, g.hasOwnProperty('prototype'), Object.keys(g), p.hasOwnProperty('constructor'));
         console.log(Object.keys(p), typeof p.next, p.next === function*(){}.prototype.next);
         const it = g();
         p.extra = 'x';
         console.log(it instanceof g, it.extra, it.next().value);
         try { delete g.prototype; } catch (e) { console.log(e.name, g.prototype === p); }",
        "object true [] false\n[] function true\ntrue x 1\nTypeError true\n",
    );
}

#[test]
fn every_generator_kind_gets_a_fresh_prototype() {
    assert_output(
        "function outer(){ return function*(){ yield 0; }; }
         const a = outer(), b = outer();
         const o = { *m(){ yield 1; } };
         class C { *m(){ yield 2; } static *s(){ yield 3; } }
         console.log(a.prototype !== b.prototype, a() instanceof a, a() instanceof b);
         console.log(o.m() instanceof o.m, new C().m() instanceof C.prototype.m, C.s() instanceof C.s);
         function f(){ function* inner(){ yield 4; } return inner; }
         const i = f();
         console.log(i() instanceof i, i.prototype !== f().prototype);",
        "true true false\ntrue true true\ntrue true\n",
    );
}

#[test]
fn a_generator_object_inherits_a_replaced_prototype() {
    assert_output(
        "function* g(){ yield 1; }
         const old = g.prototype;
         const P = { tag: 'P' };
         g.prototype = P;
         const it = g();
         console.log(it instanceof g, it.tag, typeof it.next, g.prototype === P, old !== P);",
        "true P undefined true true\n",
    );
}

#[test]
fn a_non_object_prototype_falls_back_to_generator_prototype() {
    assert_output(
        "function* g(){ yield 1; }
         g.prototype.tag = 'own';
         g.prototype = 5;
         const it = g();
         console.log(it.tag, it.next().value, it instanceof Object);
         const o = { *m(){ yield 2; } };
         o.m.prototype = null;
         const jt = o.m();
         console.log(jt.next().value, typeof jt.return);",
        "undefined 1 true\n2 function\n",
    );
}

#[test]
fn async_functions_and_methods_have_no_prototype() {
    assert_output(
        "async function f(){} const e = async function(){}; const a = async () => 1;
         const o = { async m(){} }; class C { async m(){} }
         console.log(f.hasOwnProperty('prototype'), e.hasOwnProperty('prototype'),
           a.hasOwnProperty('prototype'), o.m.hasOwnProperty('prototype'),
           C.prototype.m.hasOwnProperty('prototype'), f.prototype);",
        "false false false false false undefined\n",
    );
}

#[test]
fn async_generator_methods_stay_a_compile_error() {
    assert_compile_error(
        "const o = { async *m(){ yield 1; } };",
        "async generator functions are unsupported",
    );
    assert_compile_error(
        "class C { async *m(){ yield 1; } }",
        "async generator functions are unsupported",
    );
}

#[test]
fn computed_generator_class_methods_stay_rejected() {
    assert_compile_error(
        "const k = 'g'; class C { *[k](){ yield 1; } }",
        "computed class method names are not supported",
    );
}
