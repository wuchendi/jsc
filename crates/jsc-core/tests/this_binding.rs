//! How `this` is bound: a parenthesized member callee keeps its receiver,
//! and arrows read the `this` of their enclosing non-arrow function.

mod common;

use common::assert_output;

#[test]
fn a_parenthesized_member_callee_keeps_its_receiver() {
    assert_output(
        "const o = { v: 7, m(){ return this === undefined ? 'none' : this.v; } };
         console.log((o.m)(), ((o.m))(), (o['m'])(), (0, o.m)());",
        "7 7 7 none\n",
    );
}

#[test]
fn an_arrow_ignores_the_this_of_call_apply_and_bind() {
    assert_output(
        "const o = { v: 7, m(){
           const f = () => this.v;
           return [f.call({ v: 1 }), f.apply({ v: 2 }, []), f.bind({ v: 3 })()];
         } };
         console.log(o.m());",
        "[ 7, 7, 7 ]\n",
    );
    assert_output(
        "console.log(({ v: 7, m(){ return (() => this.v).call({ v: 1 }); } }).m());",
        "7\n",
    );
}

#[test]
fn an_arrow_called_as_a_method_keeps_its_lexical_this() {
    assert_output(
        "function make(){ return { v: 1, get: () => this }; }
         const o = make.call({ tag: 'outer' });
         console.log(o.get().tag, o.get() === o);",
        "outer false\n",
    );
}

#[test]
fn nested_arrows_share_the_enclosing_function_this() {
    assert_output(
        "const o = { v: 5, m(){ return () => () => () => this.v; } };
         const f = o.m();
         console.log(f.call({ v: 0 })()());",
        "5\n",
    );
}

#[test]
fn a_top_level_arrow_sees_the_top_level_this() {
    assert_output(
        "const f = () => this;
         console.log(f() === globalThis, f.call({ v: 1 }) === globalThis, ({ f }).f() === globalThis);",
        "true true true\n",
    );
}

#[test]
fn an_arrow_in_a_class_method_and_constructor_reads_the_instance() {
    assert_output(
        "class C {
           constructor(){ this.v = 3; this.get = () => this.v; }
           m(){ return [1, 2].map(x => x + this.v); }
         }
         const c = new C();
         const g = c.get;
         console.log(g(), c.m());",
        "3 [ 4, 5 ]\n",
    );
}

#[test]
fn an_arrow_in_a_generator_reads_the_generator_this_across_yields() {
    assert_output(
        "const o = { v: 9, *g(){ const f = () => this.v; yield f(); yield f.call({ v: 0 }); } };
         console.log([...o.g()]);",
        "[ 9, 9 ]\n",
    );
}

#[test]
fn an_arrow_in_an_async_function_reads_its_this_after_await() {
    assert_output(
        "const o = { v: 4, async m(){
           const f = () => this.v;
           await null;
           const g = async () => { await null; return this.v; };
           return [f(), await g.call({ v: 0 })];
         } };
         o.m().then(r => console.log(r));",
        "[ 4, 4 ]\n",
    );
}
