//! Integration tests: the `super` base is the home object's CURRENT
//! `[[Prototype]]` at each reference (`GetSuperBase`), and `super(..)`
//! constructs the class's current prototype (`GetSuperConstructor`)
//! (DESIGN.md §7.6).

mod common;
use common::{assert_output, assert_rejected, assert_string};

#[test]
fn super_method_call_sees_a_moved_prototype() {
    assert_string(
        r#"class A { m(){ return "A" } }; class B extends A { m(){ return super.m() } };
           Object.setPrototypeOf(B.prototype, { m(){ return "X" } }); new B().m()"#,
        "X",
    );
}

#[test]
fn super_read_call_store_and_compound_follow_set_prototype_of() {
    assert_output(
        "class A { get x() { return 'A' } }
         class B extends A {
           read() { return super.x }
           call() { return super.who() }
           store(v) { super.y = v }
           bump() { return super.n += 1 }
         }
         const P = {
           x: 'P', n: 10,
           who() { return this === b },
           set y(v) { this.got = 'P:' + v },
         };
         const b = new B();
         console.log(b.read());
         Object.setPrototypeOf(B.prototype, P);
         console.log(b.read(), b.call());
         b.store(7); console.log(b.got, Object.getOwnPropertyNames(P).includes('got'));
         console.log(b.bump(), b.n, P.n);",
        "A\nP true\nP:7 false\n11 11 10\n",
    );
}

#[test]
fn static_super_follows_set_prototype_of_the_class() {
    assert_output(
        "class A { static who() { return 'A' } }
         class B extends A { static who() { return 'B>' + super.who() } }
         console.log(B.who());
         Object.setPrototypeOf(B, { who() { return 'X' } });
         console.log(B.who());",
        "B>A\nB>X\n",
    );
    // A base class's static home object is the class itself.
    assert_output(
        "class C { static s() { return super.tag } }
         console.log(C.s());
         Object.setPrototypeOf(C, { tag: 'moved' });
         console.log(C.s());",
        "undefined\nmoved\n",
    );
}

#[test]
fn super_in_constructor_field_initializer_and_arrow() {
    assert_output(
        "class A {}
         class B extends A {
           f = super.v;
           constructor() { super(); this.c = super.v; }
           arrow() { return () => super.v }
         }
         Object.setPrototypeOf(B.prototype, { v: 'new' });
         const b = new B();
         console.log(b.f, b.c, b.arrow()());",
        "new new new\n",
    );
    // A base class: the instance home object is its prototype.
    assert_output(
        "class C { m() { return super.k } }
         const c = new C();
         console.log(c.m());
         Object.setPrototypeOf(C.prototype, { k: 1 });
         console.log(c.m());",
        "undefined\n1\n",
    );
}

#[test]
fn super_in_static_field_and_static_block_reads_the_current_base() {
    assert_output(
        "class A { static v = 'A' }
         class B extends A {
           static f = super.v;
           static { B.g = super.v; }
           static h = () => super.v;
           static later() { return super.v }
         }
         console.log(B.f, B.g, B.later());
         Object.setPrototypeOf(B, { v: 'X' });
         console.log(B.later(), B.h());",
        "A A A\nX X\n",
    );
}

#[test]
fn null_super_base_throws_type_error() {
    assert_output(
        "class A { m() { return 1 } }
         class B extends A { m() { return super.m() } k() { return super['x'] } s() { super.x = 1 } }
         Object.setPrototypeOf(B.prototype, null);
         const b = new B();
         try { B.prototype.m.call(b) } catch (e) { console.log(e instanceof TypeError) }
         try { b.k() } catch (e) { console.log(e instanceof TypeError) }
         try { b.s() } catch (e) { console.log(e instanceof TypeError) }",
        "true\ntrue\ntrue\n",
    );
}

#[test]
fn computed_super_reads_the_base_before_the_key_converts() {
    // test262 prop-expr-getsuperbase-before-topropertykey-*: the base is read
    // after the key expression is evaluated, before ToPropertyKey runs.
    assert_output(
        "const proto = { p: 'ok', n: 1, set w(v) { log.push('ok ' + v) } };
         const proto2 = { p: 'bad', n: -1, set w(v) { log.push('bad ' + v) } };
         const log = [];
         class C {
           get_() { return super[key] }
           compound() { return super[key2] += 1 }
           inc() { return ++super[key2] }
           put() { super[key3] = 10 }
         }
         const reset = () => Object.setPrototypeOf(C.prototype, proto);
         const flip = (name) => ({ toString() { Object.setPrototypeOf(C.prototype, proto2); return name } });
         const key = flip('p'), key2 = flip('n'), key3 = flip('w');
         const c = new C();
         reset(); console.log(c.get_());
         reset(); console.log(c.compound());
         reset(); console.log(c.inc());
         reset(); c.put(); console.log(log.join());",
        "ok\n2\n2\nok 10\n",
    );
}

#[test]
fn compound_super_store_uses_the_base_its_read_used() {
    assert_output(
        "const P1 = { set x(v) { log.push('P1 ' + v) }, get x() { return 1 } };
         const P2 = { set x(v) { log.push('P2 ' + v) }, get x() { return 2 } };
         const log = [];
         class C { m() { super.x += (Object.setPrototypeOf(C.prototype, P2), 10) } }
         Object.setPrototypeOf(C.prototype, P1);
         new C().m();
         console.log(log.join());",
        "P1 11\n",
    );
}

#[test]
fn super_call_constructs_the_current_prototype() {
    assert_output(
        "class A { constructor() { this.from = 'A' } }
         class C { constructor() { this.from = 'C'; this.nt = new.target } }
         class B extends A {}
         Object.setPrototypeOf(B, C);
         const b = new B();
         console.log(b.from, b.nt === B, Object.getPrototypeOf(b) === B.prototype, b instanceof B);",
        "C true true true\n",
    );
    // An explicit constructor passes its arguments.
    assert_output(
        "class A { constructor(v) { this.v = 'A' + v } }
         class C { constructor(v) { this.v = 'C' + v } }
         class B extends A { constructor() { super(1) } }
         console.log(new B().v);
         Object.setPrototypeOf(B, C);
         console.log(new B().v);",
        "A1\nC1\n",
    );
}

#[test]
fn super_call_to_a_non_constructor_prototype_throws_after_the_arguments() {
    // test262 call-proto-not-ctor.
    assert_output(
        "let evaluated = false, caught;
         class C extends Object {
           constructor() {
             try { super(evaluated = true) } catch (e) { caught = e }
           }
         }
         Object.setPrototypeOf(C, parseInt);
         try { new C() } catch (_) {}
         console.log(caught instanceof TypeError, caught.message, evaluated);",
        "true Super constructor is not a constructor true\n",
    );
}

#[test]
fn object_literal_super_inside_a_class_method_stays_rejected() {
    // An object-literal method's home object is the literal, not the class.
    assert_rejected(
        "class A { x() { return 'A' } }
         class B extends A { m() { return { f() { return super.x() } }.f() } }
         new B().m()",
    );
    assert_rejected(
        "class B { m() { return { get g() { return super.x } }.g } }
         new B().m()",
    );
}

#[test]
fn generator_and_async_methods_read_the_current_base() {
    assert_output(
        "class A { get t() { return 'A' } }
         class B extends A {
           *g() { yield super.t; yield super.t; }
           async a() { await null; return super.t; }
         }
         const it = new B().g();
         console.log(it.next().value);
         Object.setPrototypeOf(B.prototype, { t: 'X' });
         console.log(it.next().value);
         new B().a().then(v => console.log(v));",
        "A\nX\nX\n",
    );
    // A compound store across a suspension writes the base its read used.
    assert_output(
        "const P1 = { get w() { return 1 }, set w(v) { log.push('P1 ' + v) } };
         const P2 = { get w() { return 2 }, set w(v) { log.push('P2 ' + v) } };
         const log = [];
         class C { *g() { super.w += yield 'k'; } }
         Object.setPrototypeOf(C.prototype, P1);
         const it = new C().g();
         it.next();
         Object.setPrototypeOf(C.prototype, P2);
         it.next(5);
         console.log(log.join());",
        "P1 6\n",
    );
}
