//! Integration tests: class static blocks `static { .. }` (DESIGN.md §7.6) —
//! evaluation once per class evaluation, interleaved in source order with the
//! static fields (public and private), `this` = the class, `new.target`
//! `undefined`, `super.x` reading the parent constructor, the block's own
//! `var` scope, block-scoped lexical declarations, closures, the inner class
//! name, and exceptions escaping the class definition. Expected texts are
//! Node's output.

mod common;
use common::{assert_output, assert_uncaught};

/// A block runs once, when the class is defined, before any instance exists.
#[test]
fn runs_once_at_definition() {
    assert_output(
        "let n = 0; console.log('before'); class C { static { n++; console.log('block') } } \
         new C(); new C(); console.log(n)",
        "before\nblock\n1\n",
    );
    assert_output(
        "class C { static {} static { ; } } console.log(typeof C)",
        "function\n",
    );
}

/// Blocks and static fields (public and private) run in one source-order pass.
#[test]
fn interleaved_with_static_fields() {
    assert_output(
        "let seq = []; class C { static a = seq.push('first field'); static { seq.push('first block') } \
         static #b = seq.push('second field'); static { seq.push('second block') } } \
         console.log(seq.join())",
        "first field,first block,second field,second block\n",
    );
    assert_output(
        "class C { static a = 1; static { this.b = this.a + 1 } static c = this.b + 1; \
         static #d = 10; static { this.e = C.#d + this.c } } console.log(C.a, C.b, C.c, C.e)",
        "1 2 3 13\n",
    );
    // Instance fields do not run at definition.
    assert_output(
        "let seq = []; class C { x = seq.push('x'); static { seq.push('block') } } \
         console.log(seq.join()); new C(); console.log(seq.join())",
        "block\nblock,x\n",
    );
}

/// `this` is the class; `new.target` is `undefined`; arrows inherit both.
#[test]
fn this_and_new_target() {
    assert_output(
        "let self, nt = 0, arrowSelf; class C { static { self = this; nt = new.target; \
         arrowSelf = (() => this)() } } console.log(self === C, nt, arrowSelf === C)",
        "true undefined true\n",
    );
    assert_output(
        "let r; let D = class { static { r = this.name } }; console.log(r)",
        "D\n",
    );
    // A nested ordinary function gets its own `this`.
    assert_output(
        "let r; class C { static { r = (function () { return this })() } } console.log(r)",
        "undefined\n",
    );
}

/// The home object is the class: `super.x` reads the parent constructor, with
/// `this` = the class.
#[test]
fn super_property() {
    assert_output(
        "function Parent() {} Parent.test262 = 'test262'; let value; \
         class C extends Parent { static { value = super.test262 } } console.log(value)",
        "test262\n",
    );
    assert_output(
        "class A { static who() { return this.tag } } let r; \
         class B extends A { static tag = 'B'; static { r = super.who() } } console.log(r)",
        "B\n",
    );
}

/// `var` declarations are scoped to the block; `let`, `const`, `class` and
/// function declarations too, and nothing leaks out.
#[test]
fn block_scoping() {
    assert_output(
        "var test262 = 'outer scope'; var probe; \
         class C { static { var test262 = 'first block'; probe = test262 } } \
         console.log(probe, test262)",
        "first block outer scope\n",
    );
    assert_output(
        "var probe; class C { static { var x = 1 } static { probe = typeof x } } console.log(probe)",
        "undefined\n",
    );
    assert_output(
        "let x = 'outer'; let r; class C { static { let x = 'inner'; const y = x + '!'; \
         function f() { return y } class K {} r = f() + typeof K } } console.log(r, x)",
        "inner!function outer\n",
    );
    // A function declaration is hoisted within the block.
    assert_output(
        "let r; class C { static { r = g(); function g() { return 'hoisted' } } } console.log(r)",
        "hoisted\n",
    );
    // `var` and a function declaration of the same name may coexist.
    assert_output(
        "let r; class C { static { var f; function f() { return 1 } r = typeof f } } console.log(r)",
        "function\n",
    );
}

/// Closures created in a block capture its bindings and the enclosing ones.
#[test]
fn closures() {
    assert_output(
        "let get, inc; let outer = 10; class C { static { let n = 0; \
         inc = () => ++n + outer; get = function () { return n } } } \
         inc(); console.log(inc(), get())",
        "12 2\n",
    );
    assert_output(
        "let fs = []; class C { static { for (let i = 0; i < 3; i++) fs.push(() => i) } } \
         console.log(fs.map(f => f()).join())",
        "0,1,2\n",
    );
}

/// The inner class name and the class's private names are visible.
#[test]
fn inner_name_and_private_names() {
    assert_output(
        "let r; class C { static #count = 0; static { C.#count += 5; r = C.#count } } console.log(r)",
        "5\n",
    );
    assert_output(
        "let r; let D = class Named { static { r = Named === this } }; console.log(r)",
        "true\n",
    );
    assert_output(
        "let read; class C { #x = 42; static { read = o => o.#x } } console.log(read(new C()))",
        "42\n",
    );
}

/// One block closure per class evaluation: a class expression in a loop runs
/// its block every time.
#[test]
fn once_per_class_evaluation() {
    assert_output(
        "let ids = []; for (let i = 0; i < 3; i++) { let K = class { static { this.id = i; ids.push(i) } }; } \
         console.log(ids.join())",
        "0,1,2\n",
    );
}

/// An exception thrown in a block propagates out of the class definition;
/// later static elements do not run.
#[test]
fn exceptions_propagate() {
    assert_output(
        "let seq = []; try { class C { static { seq.push(1); throw new Error('boom') } \
         static x = seq.push(2) } } catch (e) { seq.push(e.message) } console.log(seq.join())",
        "1,boom\n",
    );
    assert_uncaught(
        "class C { static { throw new TypeError('t') } }",
        "TypeError: t",
    );
}

/// `arguments` and `await` are ordinary inside nested functions (and `await`
/// inside nested arrows) of a block.
#[test]
fn nested_functions_are_unrestricted() {
    assert_output(
        "let r; class C { static { r = (function () { return arguments.length })(1, 2) } } console.log(r)",
        "2\n",
    );
    assert_output(
        "function f() { var await = 0; let r; \
         class C { static { new (class { constructor(x = await) { r = x + await } }) } } return r } \
         console.log(f())",
        "0\n",
    );
    assert_output(
        "let r; class C { static { (() => { class await {} r = typeof await })() } } console.log(r)",
        "function\n",
    );
    assert_output(
        "let r; class C { static { r = (() => { return 'arrow return' })() } } console.log(r)",
        "arrow return\n",
    );
}

/// Labels, loops and `switch` inside a block work as usual.
#[test]
fn control_flow_inside() {
    assert_output(
        "let r = []; class C { static { outer: for (let i = 0; i < 3; i++) { for (;;) { \
         if (i === 1) continue outer; r.push(i); break } } switch (r.length) { case 2: r.push('s'); break } } } \
         console.log(r.join())",
        "0,2,s\n",
    );
}
