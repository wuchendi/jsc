//! Integration tests: `super` property assignment, update and `delete`
//! (DESIGN.md §7.6), `delete` of an optional chain, and the nullish-base
//! order of a computed `**=` / logical member assignment (DESIGN.md §7.2).

#![allow(clippy::expect_used)]

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string};

#[test]
fn super_store_calls_an_inherited_setter_with_this() {
    assert_output(
        "class A { set x(v) { this.seen = v; console.log(this === b, v); } }
         class B extends A { m() { super.x = 5; } }
         const b = new B(); b.m();
         console.log(b.seen, Object.getOwnPropertyNames(A.prototype).includes('seen'));",
        "true 5\n5 false\n",
    );
}

#[test]
fn super_store_on_an_inherited_data_property_defines_it_on_this() {
    assert_output(
        "class A {} A.prototype.x = 1;
         class B extends A { m() { return super.x = 2; } }
         const b = new B(); const r = b.m();
         console.log(r, b.x, A.prototype.x, Object.getOwnPropertyNames(b).join());",
        "2 2 1 x\n",
    );
    // A setter on the receiver's own class prototype is skipped: the lookup
    // starts at the home object's prototype.
    assert_output(
        "class A {}
         class B extends A { set y(v) { console.log('B setter'); } m() { super.y = 3; } }
         const b = new B(); b.m();
         console.log(Object.getOwnPropertyDescriptor(b, 'y').value);",
        "3\n",
    );
    // An own writable property of the receiver keeps its attributes.
    assert_output(
        "class A {}
         class B extends A { m() { super.z = 9; } }
         const b = new B(); Object.defineProperty(b, 'z', { value: 1, writable: true, enumerable: false, configurable: true });
         b.m(); const d = Object.getOwnPropertyDescriptor(b, 'z');
         console.log(d.value, d.enumerable);",
        "9 false\n",
    );
}

#[test]
fn super_store_failures_throw_type_errors() {
    // A non-writable property on the prototype chain.
    assert_output(
        "class A {} Object.defineProperty(A.prototype, 'x', { value: 1, writable: false });
         class B extends A { m() { super.x = 2; } }
         try { new B().m(); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
    // A getter-only accessor.
    assert_output(
        "class A { get x() { return 1; } }
         class B extends A { m() { super.x = 2; } }
         try { new B().m(); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
    // A non-extensible receiver, and a read-only own property of the receiver.
    assert_output(
        "class A {}
         class B extends A { m() { super.x = 2; } }
         const b = Object.preventExtensions(new B());
         try { b.m(); } catch (e) { console.log(e instanceof TypeError, 'x' in b); }
         const c = new B(); Object.defineProperty(c, 'x', { value: 1 });
         try { c.m(); } catch (e) { console.log(e instanceof TypeError, c.x); }",
        "true false\ntrue 1\n",
    );
    // A base class stores through %Object.prototype%; a frozen receiver throws.
    assert_output(
        "class C { m() { super.x = 8; Object.freeze(C.prototype); try { super.y = 9; } catch (e) { console.log(e instanceof TypeError); } } }
         C.prototype.m(); console.log(C.prototype.x);",
        "true\n8\n",
    );
    // `class extends null`: the home object prototype is null.
    assert_output(
        "class C extends null { m() { super.x = 1; } }
         try { C.prototype.m(); } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
}

#[test]
fn super_reads_call_getters_with_this() {
    assert_output(
        "class P { get This() { return this; } }
         class C extends P { m() { return super.This; } }
         console.log(C.prototype.m() === C.prototype);",
        "true\n",
    );
}

#[test]
fn super_compound_and_logical_assignment() {
    assert_output(
        "const log = [];
         class A { get x() { log.push('get'); return this.v; } set x(w) { log.push('set ' + w); this.v = w; } }
         class B extends A {
           add() { return super.x += 2; }
           pow() { return super.x **= 2; }
           or() { return super.x ||= 7; }
           and() { return super.x &&= 0; }
           nul() { return super.x ??= 4; }
         }
         const b = new B(); b.v = 1;
         console.log(b.add(), b.pow(), b.or(), b.and(), b.or(), b.nul());
         console.log(log.join());",
        "3 9 9 0 7 7\nget,set 3,get,set 9,get,get,set 0,get,set 7,get\n",
    );
    // A short-circuited logical assignment skips the right-hand side.
    assert_number(
        "let n = 0; class A {} A.prototype.x = 5;
         class B extends A { m() { super.x ||= (n++, 1); super.x &&= (n++, 2); return n; } }
         new B().m()",
        1.0,
    );
}

#[test]
fn super_update_expressions() {
    assert_output(
        "class A {} A.prototype.n = '4';
         // The read starts at A.prototype (still '4'), the store goes to `b`.
         class B extends A { inc() { return super.n++; } dec() { return --super['n']; } }
         const b = new B();
         console.log(b.inc(), b.n, b.dec(), b.n, A.prototype.n);",
        "4 5 3 3 4\n",
    );
}

#[test]
fn super_computed_key_order() {
    // `this`, then the key expression, then the right-hand side, then
    // ToPropertyKey of the key (once), then the store.
    assert_string(
        "const log = [];
         const k = { toString() { log.push('key'); return 'p'; } };
         class A {}
         class B extends A { m() { super[(log.push('expr'), k)] = (log.push('rhs'), 1); return this.p; } }
         log.push(new B().m()); log.join()",
        "expr,rhs,key,1",
    );
    // A compound assignment converts the key once, before the right-hand side.
    assert_string(
        "const log = [];
         const k = { toString() { log.push('key'); return 'p'; } };
         class A {} A.prototype.p = 1;
         class B extends A { m() { return super[k] += (log.push('rhs'), 2); } }
         log.push(new B().m()); log.join()",
        "key,rhs,3",
    );
    // In a derived constructor `this` is read first: the key is not evaluated.
    assert_output(
        "class A { constructor() { console.log('A'); } }
         class B extends A { constructor() { super[(console.log('key'), 'x')] = 1; super(); } }
         try { new B(); } catch (e) { console.log(e instanceof ReferenceError); }",
        "true\n",
    );
}

#[test]
fn super_stores_in_static_methods_and_arrows() {
    // A static method's home object is the class: the lookup starts at the
    // parent constructor and the receiver is the class.
    assert_output(
        "class A { static set s(v) { this.got = v; } }
         class B extends A { static m() { super.s = 1; super.t = 2; } }
         B.m(); console.log(B.got, B.t, Object.getOwnPropertyNames(A).includes('t'));",
        "1 2 false\n",
    );
    // An arrow inside a method uses the method's `this`.
    assert_number(
        "class A {} class B extends A { m() { const f = () => { super.q = 6; }; f(); return this.q; } }
         new B().m()",
        6.0,
    );
}

#[test]
fn super_store_in_generator_and_async_methods() {
    assert_output(
        "class A { set x(v) { this.v = v; } } A.prototype.w = 1;
         class B extends A {
           *g() { super.x = yield 'a'; super.w += yield 'b'; return this.v + this.w; }
           async h() { super.z = await Promise.resolve(3); return this.z; }
         }
         const b = new B(); const it = b.g();
         console.log(it.next().value, it.next(5).value, it.next(10).value);
         b.h().then(v => console.log(v));",
        "a b 16\n3\n",
    );
}

#[test]
fn delete_super_property_throws_reference_error() {
    assert_output(
        "let n = 0;
         class A { m() { return 1; } }
         class B extends A {
           d() { delete super.m; }
           e() { delete super[(n++, 'm')]; }
         }
         const b = new B();
         try { b.d(); } catch (e) { console.log(e instanceof ReferenceError); }
         try { b.e(); } catch (e) { console.log(e instanceof ReferenceError, n, typeof A.prototype.m); }",
        "true\ntrue 1 function\n",
    );
    // The key is evaluated but not converted.
    assert_output(
        "const k = { toString() { console.log('converted'); return 'x'; } };
         class C { m() { delete super[k]; } }
         try { C.prototype.m(); } catch (e) { console.log(e instanceof ReferenceError); }",
        "true\n",
    );
}

#[test]
fn delete_of_an_optional_chain() {
    // A short-circuited chain deletes nothing and yields true.
    assert_bool("const o = null; delete o?.x", true);
    assert_bool("let o; delete o?.a.b", true);
    assert_number("let n = 0; const o = null; delete o?.[n++]; n", 0.0);
    // Otherwise the final reference is deleted.
    assert_output(
        "const o = { x: 1, a: { b: 2, c: 3 } };
         const k = 'c';
         console.log(delete o?.x, 'x' in o, delete o?.a.b, 'b' in o.a, delete o.a?.[k], 'c' in o.a);",
        "true false true false true false\n",
    );
    // A non-short-circuited intermediate nullish link still throws.
    assert_output(
        "const o = { a: undefined };
         try { delete o?.a.b; } catch (e) { console.log(e instanceof TypeError); }",
        "true\n",
    );
    // A call at the end of the chain is not a reference: evaluated, true.
    assert_bool("const o = { f() { return 1; } }; delete o?.f()", true);
}

#[test]
fn computed_pow_and_logical_assignment_on_a_nullish_base_throw_before_the_key_converts() {
    for op in ["**=", "??=", "||=", "&&="] {
        assert_output(
            &format!(
                "const log = [];
                 const k = {{ toString() {{ log.push('key'); return 'p'; }} }};
                 for (const base of [null, undefined]) {{
                   try {{ base[k] {op} (log.push('rhs'), 1); }} catch (e) {{ log.push(e instanceof TypeError); }}
                 }}
                 console.log(log.join());"
            ),
            "true,true\n",
        );
    }
    // The key expression itself still runs before the TypeError.
    assert_string(
        "const log = []; const base = null;
         try { base[(log.push('expr'), 'p')] **= 2; } catch (e) { log.push(e instanceof TypeError); }
         log.join()",
        "expr,true",
    );
    // A non-nullish base converts the key exactly once.
    assert_string(
        "const log = []; const o = { p: 2 };
         const k = { toString() { log.push('key'); return 'p'; } };
         o[k] **= 3; o[k] ??= 1; log.push(o.p); log.join()",
        "key,key,8",
    );
}
