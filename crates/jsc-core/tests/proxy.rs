//! Integration tests: Proxy exotic objects (DESIGN.md §6.11, stages P2a and
//! P2b) — `new Proxy`, `Proxy.revocable`, the eleven object internal-method
//! traps with their invariant checks, proxies on prototype chains and as
//! receivers, the generic algorithms over proxies (`Object.keys`, spread,
//! `for-in`, integrity levels), the renderer, callable and constructor
//! proxies from every call-site kind, and the integration paths (`IsArray`,
//! `instanceof`, `Object.prototype.toString`, `JSON`, `concat`,
//! `Function.prototype.toString` / `bind`).

#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_same_under_gc_stress, assert_string, assert_uncaught};

/// `src` (a function body run inside `try`) throws a `TypeError`.
fn type_error(body: &str) -> String {
    format!(
        "(function () {{ try {{ {body}; return 'no throw'; }} catch (e) {{ \
         return e instanceof TypeError ? 'TypeError' : 'other: ' + e; }} }})()"
    )
}

/// Assert every body in `bodies` throws a `TypeError`.
fn assert_type_errors(prelude: &str, bodies: &[&str]) {
    for body in bodies {
        assert_string(&format!("{prelude}\n{}", type_error(body)), "TypeError");
    }
}

// --- construction ------------------------------------------------------------

/// `Proxy` is a constructor of `length` 2 named `Proxy` without a
/// `prototype`; `new Proxy` makes an object proxy whose `typeof` is
/// `"object"`; calling it without `new` and non-object arguments throw.
#[test]
fn constructor_shape_and_errors() {
    assert_string(
        "[typeof Proxy, Proxy.length, Proxy.name, 'prototype' in Proxy,
          Object.getOwnPropertyNames(Proxy).join('/'),
          typeof new Proxy({}, {}), typeof Proxy.revocable, Proxy.revocable.length].join()",
        "function,2,Proxy,false,length/name/revocable,object,function,2",
    );
    assert_type_errors(
        "",
        &[
            "Proxy({}, {})",
            "new Proxy(1, {})",
            "new Proxy({}, null)",
            "new Proxy('s', {})",
            "new Proxy({})",
        ],
    );
    assert_uncaught(
        "Proxy({}, {})",
        "TypeError: Constructor Proxy requires 'new'",
    );
    assert_uncaught(
        "new Proxy({}, 1)",
        "TypeError: Cannot create proxy with a non-object as target or handler",
    );
}

/// A revoked proxy is still an acceptable target or handler.
#[test]
fn revoked_proxy_as_target_or_handler() {
    assert_string(
        "const r = Proxy.revocable({}, {}); r.revoke();
         const a = new Proxy(r.proxy, {}); const b = new Proxy({}, r.proxy);
         [typeof a, typeof b].join()",
        "object,object",
    );
}

// --- get ------------------------------------------------------------------------

/// Without a trap `[[Get]]` forwards to the target; the trap receives the
/// target, the key (a string or a symbol) and the receiver, with `this` the
/// handler.
#[test]
fn get_trap_and_forwarding() {
    assert_string(
        "const t = { a: 1, get g() { return this === p; } };
         const p = new Proxy(t, {});
         [p.a, p.b, p.g, p['a']].join()",
        "1,,true,1",
    );
    assert_string(
        "const t = { a: 1 }; const log = [];
         const h = { get(target, key, recv) {
           log.push(this === h, target === t, typeof key, String(key), recv === p);
           return 42;
         } };
         const p = new Proxy(t, h); const s = Symbol('s');
         [p.a, p[s], p[7], log.join()].join('|')",
        "42|42|42|true,true,string,a,true,true,true,symbol,Symbol(s),true,true,true,string,7,true",
    );
}

/// A computed key no table ever used still reaches the trap intact.
#[test]
fn never_interned_keys_reach_the_trap() {
    assert_string(
        "const p = new Proxy({}, { get(t, k) { return k; }, has(t, k) { return k === 'zq' + 'x'; } });
         const k = 'zq' + 'x';
         [p[k], k in p, p['nope' + 1]].join()",
        "zqx,true,nope1",
    );
}

/// `get` invariants: a non-writable, non-configurable data property must
/// report its value, an accessor without a getter `undefined`.
#[test]
fn get_invariants() {
    let prelude = "const t = {};
        Object.defineProperty(t, 'c', { value: 1 });
        Object.defineProperty(t, 's', { set(v) {} });
        Object.defineProperty(t, 'w', { value: 1, writable: true });
        const p = new Proxy(t, { get() { return 2; } });";
    assert_type_errors(prelude, &["p.c", "p.s"]);
    assert_string(&format!("{prelude} String(p.w)"), "2");
    assert_string(
        "const t = {}; Object.defineProperty(t, 'c', { value: NaN });
         String(new Proxy(t, { get() { return NaN; } }).c)",
        "NaN",
    );
}

// --- set ------------------------------------------------------------------------

/// Without a trap `[[Set]]` forwards; the trap gets `(target, key, value,
/// receiver)`; a falsish result throws in strict code.
#[test]
fn set_trap_and_forwarding() {
    assert_string(
        "const t = {}; const p = new Proxy(t, {}); p.a = 1; [t.a, p.a].join()",
        "1,1",
    );
    assert_string(
        "const t = {}; let seen;
         const p = new Proxy(t, { set(target, key, v, recv) {
           seen = [target === t, key, v, recv === p].join(); target[key] = v * 2; return true;
         } });
         p.x = 3; [seen, t.x].join('|')",
        "true,x,3,true|6",
    );
    assert_type_errors(
        "const p = new Proxy({}, { set() { return false; } });",
        &["p.x = 1"],
    );
}

/// `set` invariants: a non-writable, non-configurable data property only
/// "accepts" its own value; an accessor without a setter refuses.
#[test]
fn set_invariants() {
    let prelude = "const t = {};
        Object.defineProperty(t, 'c', { value: 1 });
        Object.defineProperty(t, 'g', { get() { return 1; } });
        const p = new Proxy(t, { set() { return true; } });";
    assert_type_errors(prelude, &["p.c = 2", "p.g = 2"]);
    assert_string(&format!("{prelude} p.c = 1; 'ok'"), "ok");
}

/// A trap-less proxy forwarding `[[Set]]` to an ordinary target defines the
/// property on the receiver, the proxy itself, through its
/// `getOwnPropertyDescriptor` and `defineProperty` traps.
#[test]
fn set_receiver_step_goes_through_proxy_traps() {
    assert_string(
        "const t = {}; const log = [];
         const p = new Proxy(t, {
           getOwnPropertyDescriptor(target, k) { log.push('gopd:' + k); return Reflect_gopd(target, k); },
           defineProperty(target, k, d) {
             log.push('define:' + k + ':' + d.value + ':' + Object.keys(d).join('/'));
             return Object.defineProperty(target, k, d), true;
           },
         });
         function Reflect_gopd(o, k) { return Object.getOwnPropertyDescriptor(o, k); }
         p.a = 1; p.a = 2;
         [log.join(' '), t.a].join('|')",
        "gopd:a define:a:1:value/writable/enumerable/configurable gopd:a define:a:2:value|2",
    );
}

// --- has --------------------------------------------------------------------------

/// `in` forwards or asks the trap; a falsish result may not hide a
/// non-configurable property, nor any property of a non-extensible target.
#[test]
fn has_trap_and_invariants() {
    assert_string(
        "const p = new Proxy({ a: 1 }, {}); ['a' in p, 'b' in p].join()",
        "true,false",
    );
    assert_string(
        "const p = new Proxy({}, { has(t, k) { return k.length > 1; } }); ['ab' in p, 'a' in p].join()",
        "true,false",
    );
    assert_type_errors(
        "const t = {}; Object.defineProperty(t, 'c', { value: 1 });
         const p = new Proxy(t, { has() { return false; } });
         const n = Object.preventExtensions({ x: 1 });
         const q = new Proxy(n, { has() { return false; } });",
        &["'c' in p", "'x' in q"],
    );
}

// --- deleteProperty -----------------------------------------------------------------

/// `delete` forwards or asks the trap; a falsish result throws in strict
/// code; reporting a non-configurable property, or a property of a
/// non-extensible target, as deleted throws.
#[test]
fn delete_trap_and_invariants() {
    assert_string(
        "const t = { a: 1 }; const p = new Proxy(t, {}); delete p.a; String('a' in t)",
        "false",
    );
    assert_string(
        "let k; const p = new Proxy({}, { deleteProperty(t, key) { k = key; return true; } });
         [delete p.q, k].join()",
        "true,q",
    );
    assert_type_errors(
        "const t = {}; Object.defineProperty(t, 'c', { value: 1 });
         const p = new Proxy(t, { deleteProperty() { return true; } });
         const n = Object.preventExtensions({ x: 1 });
         const q = new Proxy(n, { deleteProperty() { return true; } });
         const f = new Proxy({ a: 1 }, { deleteProperty() { return false; } });",
        &["delete p.c", "delete q.x", "delete f.a"],
    );
}

// --- defineProperty -----------------------------------------------------------------

/// The trap receives a fresh descriptor object holding only the present
/// fields; a falsish result makes `Object.defineProperty` throw.
#[test]
fn define_property_trap() {
    assert_string(
        "let seen;
         const p = new Proxy({}, { defineProperty(t, k, d) {
           seen = k + ':' + Object.keys(d).join('/') + ':' + d.value + ':' + d.enumerable;
           return Object.defineProperty(t, k, d), true;
         } });
         Object.defineProperty(p, 'x', { value: 1, enumerable: false });
         [seen, p.x].join('|')",
        "x:value/enumerable:1:false|1",
    );
    assert_type_errors(
        "const p = new Proxy({}, { defineProperty() { return false; } });",
        &["Object.defineProperty(p, 'x', { value: 1 })"],
    );
}

/// `defineProperty` invariants: adding to a non-extensible target, a
/// non-configurable property the target lacks or has configurable, an
/// incompatible descriptor, and a non-writable report of a writable
/// non-configurable target property.
#[test]
fn define_property_invariants() {
    assert_type_errors(
        "const yes = { defineProperty() { return true; } };
         const n = new Proxy(Object.preventExtensions({}), yes);
         const e = new Proxy({}, yes);
         const c = new Proxy({ a: 1 }, yes);
         const t = {}; Object.defineProperty(t, 'f', { value: 1 });
         const i = new Proxy(t, yes);
         const w = {}; Object.defineProperty(w, 'k', { value: 1, writable: true });
         const m = new Proxy(w, yes);",
        &[
            "Object.defineProperty(n, 'x', { value: 1 })",
            "Object.defineProperty(e, 'x', { value: 1, configurable: false })",
            "Object.defineProperty(c, 'a', { value: 1, configurable: false })",
            "Object.defineProperty(i, 'f', { value: 2 })",
            "Object.defineProperty(m, 'k', { value: 1, writable: false, configurable: false })",
        ],
    );
}

// --- getOwnPropertyDescriptor -------------------------------------------------------

/// The trap result is completed (absent fields false / undefined) and
/// becomes the descriptor object; `undefined` reports absence.
#[test]
fn get_own_property_descriptor_trap() {
    assert_string(
        "const p = new Proxy({}, { getOwnPropertyDescriptor(t, k) {
           return k === 'a' ? { value: 5, configurable: true } : undefined;
         } });
         const d = Object.getOwnPropertyDescriptor(p, 'a');
         [d.value, d.writable, d.enumerable, d.configurable,
          Object.getOwnPropertyDescriptor(p, 'b'), p.hasOwnProperty('a'),
          Object.prototype.propertyIsEnumerable.call(p, 'a')].join()",
        "5,false,false,true,,true,false",
    );
    assert_string(
        "const t = { a: 1 }; const d = Object.getOwnPropertyDescriptor(new Proxy(t, {}), 'a');
         [d.value, d.writable, d.enumerable, d.configurable].join()",
        "1,true,true,true",
    );
}

/// `getOwnPropertyDescriptor` invariants: a non-object result, hiding a
/// non-configurable property or any property of a non-extensible target,
/// inventing a property on a non-extensible target, an incompatible or
/// non-configurable report of a configurable property, and a non-writable
/// report of a writable non-configurable property.
#[test]
fn get_own_property_descriptor_invariants() {
    assert_type_errors(
        "const t = {}; Object.defineProperty(t, 'c', { value: 1, writable: true });
         const n = Object.preventExtensions({ x: 1 });
         const gopd = (r) => ({ getOwnPropertyDescriptor() { return r; } });
         const bad = new Proxy({}, gopd(1));
         const hide = new Proxy(t, gopd(undefined));
         const hideN = new Proxy(n, gopd(undefined));
         const invent = new Proxy(Object.preventExtensions({}), gopd({ value: 1, configurable: true }));
         const nonc = new Proxy({ a: 1 }, gopd({ value: 1, configurable: false }));
         const ro = new Proxy(t, gopd({ value: 1, writable: false, configurable: false }));",
        &[
            "Object.getOwnPropertyDescriptor(bad, 'a')",
            "Object.getOwnPropertyDescriptor(hide, 'c')",
            "Object.getOwnPropertyDescriptor(hideN, 'x')",
            "Object.getOwnPropertyDescriptor(invent, 'z')",
            "Object.getOwnPropertyDescriptor(nonc, 'a')",
            "Object.getOwnPropertyDescriptor(ro, 'c')",
        ],
    );
}

// --- ownKeys --------------------------------------------------------------------------

/// The trap's array-like result lists the keys in its order; `Object.keys`
/// keeps the enumerable string keys after one `getOwnPropertyDescriptor`
/// per key; `getOwnPropertyNames` / `getOwnPropertySymbols` split by type.
#[test]
fn own_keys_trap_and_filters() {
    assert_string(
        "const s = Symbol('s'); const log = [];
         const p = new Proxy({ b: 1, a: 2 }, {
           ownKeys() { return ['b', s, 'a', 'z']; },
           getOwnPropertyDescriptor(t, k) {
             log.push(String(k));
             return k === 'z' ? undefined : Object.getOwnPropertyDescriptor(t, k);
           },
         });
         [Object.keys(p).join('/'), log.join('/'), Object.getOwnPropertyNames(p).join('/'),
          Object.getOwnPropertySymbols(p).length, Object.values(p).join('/'),
          Object.entries(p).join('/')].join('|')",
        "b/a|b/a/z|b/a/z|1|1/2|b,1/a,2",
    );
    assert_string("Object.keys(new Proxy({ x: 1, y: 2 }, {})).join()", "x,y");
}

/// `ownKeys` result checks: a non-object, an element that is neither a
/// string nor a symbol, a duplicate, a missing non-configurable key, and a
/// non-extensible target's keys exactly.
#[test]
fn own_keys_invariants() {
    assert_type_errors(
        "const keys = (r) => ({ ownKeys() { return r; } });
         const t = {}; Object.defineProperty(t, 'c', { value: 1 });
         const n = Object.preventExtensions({ x: 1 });",
        &[
            "Object.keys(new Proxy({}, keys(1)))",
            "Object.keys(new Proxy({}, keys([1])))",
            "Object.keys(new Proxy({}, keys(['a', 'a'])))",
            "Object.keys(new Proxy(t, keys([])))",
            "Object.keys(new Proxy(n, keys([])))",
            "Object.keys(new Proxy(n, keys(['x', 'y'])))",
        ],
    );
    assert_string(
        "const n = Object.preventExtensions({ x: 1 });
         Object.getOwnPropertyNames(new Proxy(n, { ownKeys() { return ['x']; } })).join()",
        "x",
    );
}

/// A trap-less proxy forwards the target's full `[[OwnPropertyKeys]]`: an
/// array target's `"length"` after its indices, a not-yet-materialized
/// function target's `length` / `name` / `prototype`, and the same through
/// a proxy target.
#[test]
fn own_keys_forward_full_target_list() {
    assert_string(
        "const s = Symbol('s'); const a = [1, 2]; a.x = 3; a[s] = 4;
         function f(a, b) {}
         [Reflect.ownKeys(new Proxy([], {})).join(),
          Reflect.ownKeys(new Proxy(a, {})).map(String).join(),
          Object.getOwnPropertyNames(new Proxy(f, {})).join(),
          Object.getOwnPropertyNames(new Proxy(() => 1, {})).join(),
          Reflect.ownKeys(new Proxy(new Proxy([7], {}), {})).join(),
          Object.keys(new Proxy([5, 6], {})).join()].join('|')",
        "length|0,1,length,x,Symbol(s)|length,name,prototype|length,name|0,length|0,1",
    );
}

/// The `ownKeys` invariant sees the target's keys that `__own_keys` does
/// not list: an array's non-configurable `length` and a lazy function's
/// non-configurable `prototype` must be reported; listing them passes.
#[test]
fn own_keys_invariant_sees_hidden_target_keys() {
    assert_type_errors(
        "const keys = (r) => ({ ownKeys() { return r; } });
         function f() {}",
        &[
            "Reflect.ownKeys(new Proxy([], keys([])))",
            "Reflect.ownKeys(new Proxy([1], keys(['0'])))",
            "Reflect.ownKeys(new Proxy(f, keys(['length', 'name'])))",
            "Reflect.ownKeys(new Proxy(new Proxy([], {}), keys([])))",
            "Reflect.ownKeys(new Proxy(Object.preventExtensions([]), keys(['length', 'x'])))",
        ],
    );
    assert_string(
        "function g() {}
         [Reflect.ownKeys(new Proxy([1], { ownKeys() { return ['length', '0', 'extra']; } })).join(),
          Reflect.ownKeys(new Proxy(g, { ownKeys() { return ['prototype']; } })).join(),
          Reflect.ownKeys(new Proxy(Object.preventExtensions([]),
            { ownKeys() { return ['length']; } })).join()].join('|')",
        "length,0,extra|prototype|length",
    );
}

// --- prototypes and extensibility -----------------------------------------------------

/// `getPrototypeOf` / `setPrototypeOf` traps and their invariants.
#[test]
fn prototype_traps() {
    assert_string(
        "const proto = { k: 1 };
         const p = new Proxy({}, { getPrototypeOf() { return proto; } });
         const q = new Proxy({}, {});
         Object.setPrototypeOf(q, proto);
         [Object.getPrototypeOf(p) === proto, p.__proto__ === proto,
          Object.getPrototypeOf(q) === proto, q.k].join()",
        "true,true,true,1",
    );
    assert_type_errors(
        "const n = Object.preventExtensions({});
         const a = new Proxy({}, { getPrototypeOf() { return 1; } });
         const b = new Proxy(n, { getPrototypeOf() { return {}; } });
         const c = new Proxy({}, { setPrototypeOf() { return false; } });
         const d = new Proxy(n, { setPrototypeOf() { return true; } });",
        &[
            "Object.getPrototypeOf(a)",
            "Object.getPrototypeOf(b)",
            "Object.setPrototypeOf(c, {})",
            "Object.setPrototypeOf(d, {})",
        ],
    );
}

/// `isExtensible` / `preventExtensions` traps, their invariants, and the
/// generic `seal` / `freeze` / `isFrozen` over traps.
#[test]
fn extensibility_and_integrity() {
    assert_string(
        "const t = { a: 1 }; const p = new Proxy(t, {});
         const before = Object.isExtensible(p);
         Object.freeze(p);
         [before, Object.isExtensible(p), Object.isFrozen(p), Object.isFrozen(t),
          Object.isSealed(p)].join()",
        "true,false,true,true,true",
    );
    assert_type_errors(
        "const a = new Proxy({}, { isExtensible() { return false; } });
         const b = new Proxy({}, { preventExtensions() { return true; } });
         const c = new Proxy({}, { preventExtensions() { return false; } });",
        &[
            "Object.isExtensible(a)",
            "Object.preventExtensions(b)",
            "Object.preventExtensions(c)",
            "Object.freeze(c)",
        ],
    );
    assert_string(
        "const log = [];
         const p = new Proxy({ a: 1 }, {
           defineProperty(t, k, d) { log.push(k + ':' + Object.keys(d).join('/')); return Object.defineProperty(t, k, d), true; },
         });
         Object.seal(p); log.join()",
        "a:configurable",
    );
}

// --- prototype chains -------------------------------------------------------------------

/// A proxy on a prototype chain takes `get`, `set` (with the original
/// receiver), `has`, an array hole and `super.x` through its traps.
#[test]
fn proxy_on_prototype_chain() {
    assert_string(
        "const log = [];
         const proto = new Proxy({ x: 1 }, {
           get(t, k, r) { log.push('get:' + String(k) + ':' + (r === o)); return t[k]; },
           has(t, k) { log.push('has:' + String(k)); return k in t; },
           set(t, k, v, r) { log.push('set:' + k + ':' + (r === o)); return false; },
         });
         const o = Object.create(proto);
         const x = o.x; const h = 'x' in o; let threw = false;
         try { o.y = 2; } catch (e) { threw = e instanceof TypeError; }
         [x, h, threw, log.join(' ')].join('|')",
        "1|true|true|get:x:true has:x set:y:true",
    );
    assert_string(
        "const arr = [0, , 2];
         Object.setPrototypeOf(arr, new Proxy([], { get(t, k) { return k === '1' ? 'hole' : t[k]; },
           has(t, k) { return k === '1' || k in t; } }));
         String(arr[1])",
        "hole",
    );
    assert_string(
        "const base = new Proxy({}, { get(t, k, r) { return 'super:' + String(k) + ':' + (r === b); } });
         class B { m() { return super.x; } }
         Object.setPrototypeOf(B.prototype, base);
         const b = new B();
         b.m()",
        "super:x:true",
    );
}

/// `for-in` enumerates a proxy level through its `ownKeys` and
/// `getOwnPropertyDescriptor` traps, then its `getPrototypeOf` result.
#[test]
fn for_in_over_proxies() {
    assert_string(
        "const p = new Proxy({ a: 1, b: 2 }, {});
         const ks = []; for (const k in p) ks.push(k); ks.join()",
        "a,b",
    );
    assert_string(
        "const proto = { inherited: 1 };
         const p = new Proxy({ own: 1 }, { getPrototypeOf() { return proto; },
           ownKeys() { return ['own', 'hidden']; },
           getOwnPropertyDescriptor(t, k) {
             return k === 'own' ? { value: 1, enumerable: true, configurable: true }
                                : { value: 1, enumerable: false, configurable: true };
           } });
         const ks = []; for (const k in p) ks.push(k); ks.join()",
        "own,inherited",
    );
    assert_string(
        "const o = Object.create(new Proxy({ x: 1 }, {})); o.y = 2;
         const ks = []; for (const k in o) ks.push(k); ks.join()",
        "y,x",
    );
}

// --- copying ---------------------------------------------------------------------------

/// Object spread, object rest and `Object.assign` read a proxy source
/// through `ownKeys`, `getOwnPropertyDescriptor` and `get` in spec order;
/// an excluded key is never described.
#[test]
fn spread_rest_assign_trap_order() {
    let src = "const log = [];
        const p = new Proxy({ a: 1, b: 2 }, {
          ownKeys(t) { log.push('keys'); return ['a', 'b']; },
          getOwnPropertyDescriptor(t, k) { log.push('gopd:' + k); return Object.getOwnPropertyDescriptor(t, k); },
          get(t, k) { log.push('get:' + String(k)); return t[k]; },
        });";
    assert_string(
        &format!("{src} const c = {{ ...p }}; [c.a, c.b, log.join(' ')].join('|')"),
        "1|2|keys gopd:a get:a gopd:b get:b",
    );
    assert_string(
        &format!("{src} const {{ a, ...rest }} = p; [a, rest.b, log.join(' ')].join('|')"),
        "1|2|get:a keys gopd:b get:b",
    );
    assert_string(
        &format!("{src} const o = Object.assign({{}}, p); [o.a, o.b, log.join(' ')].join('|')"),
        "1|2|keys gopd:a get:a gopd:b get:b",
    );
}

// --- revocation -----------------------------------------------------------------------

/// `Proxy.revocable` returns `{ proxy, revoke }`; after `revoke()` (which
/// may be called again) every trap throws.
#[test]
fn revocation() {
    assert_string(
        "const r = Proxy.revocable({ a: 1 }, {});
         const before = r.proxy.a;
         [Object.keys(r).join('/'), typeof r.revoke, r.revoke.length, r.revoke.name === '',
          before, r.revoke(), r.revoke()].join()",
        "proxy/revoke,function,0,true,1,,",
    );
    assert_type_errors(
        "const r = Proxy.revocable({ a: 1 }, {}); r.revoke(); const p = r.proxy;",
        &[
            "p.a",
            "p.a = 1",
            "'a' in p",
            "delete p.a",
            "Object.keys(p)",
            "Object.getOwnPropertyDescriptor(p, 'a')",
            "Object.defineProperty(p, 'a', { value: 1 })",
            "Object.getPrototypeOf(p)",
            "Object.setPrototypeOf(p, null)",
            "Object.isExtensible(p)",
            "Object.preventExtensions(p)",
            "for (const k in p) {}",
        ],
    );
    assert_uncaught(
        "const r = Proxy.revocable({}, {}); r.revoke(); r.proxy.x",
        "TypeError: Cannot perform 'get' on a proxy that has been revoked",
    );
}

/// A trap that is neither callable nor `undefined` / `null` throws; `null`
/// means "no trap".
#[test]
fn non_callable_trap() {
    assert_string("String(new Proxy({ a: 1 }, { get: null }).a)", "1");
    assert_uncaught(
        "new Proxy({}, { get: 1 }).a",
        "TypeError: proxy trap 'get' is not a function",
    );
}

// --- private names ----------------------------------------------------------------------

/// Private fields and methods act on the proxy itself and never trap: a base
/// constructor returning a proxy lets a derived class install them.
#[test]
fn private_names_on_a_proxy() {
    assert_string(
        "const log = [];
         const handler = { get(t, k) { log.push('get'); return t[k]; },
           defineProperty(t, k, d) { log.push('define:' + k); return Object.defineProperty(t, k, d), true; } };
         class Base { constructor() { return new Proxy({}, handler); } }
         class D extends Base {
           #x = 1; pub = 2; #m() { return this.#x; }
           static read(o) { return o.#m() + (#x in o ? 10 : 0); }
         }
         const d = new D();
         [D.read(d), log.join(' ')].join('|')",
        "11|define:pub",
    );
}

// --- rendering -----------------------------------------------------------------------------

/// `console.log` renders a proxy as its target without running a trap, a
/// revoked one as `<Revoked Proxy>`.
#[test]
fn console_log_of_proxies() {
    assert_output(
        "const p = new Proxy({ a: 1 }, { get() { throw new Error('no'); },
           ownKeys() { throw new Error('no'); } });
         const r = Proxy.revocable([1, 2], {}); const q = r.proxy; r.revoke();
         console.log(p); console.log([p, new Proxy(p, {})]); console.log(q);",
        "{ a: 1 }\n[ { a: 1 }, { a: 1 } ]\n<Revoked Proxy>\n",
    );
}

// --- gc_stress -------------------------------------------------------------------------------

/// The allocating trap paths (descriptor objects, record lists, key lists,
/// trap argument spills, for-in snapshots) stay correct with a collection at
/// every allocation.
#[test]
fn traps_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const t = { a: 1, b: 2 };
         const p = new Proxy(t, {
           get(t, k, r) { return typeof k === 'string' ? t[k] + '!' : undefined; },
           set(t, k, v, r) { t[k] = [v]; return true; },
           has(t, k) { return k in t; },
           ownKeys(t) { return Object.keys(t).concat(['z']); },
           getOwnPropertyDescriptor(t, k) { return Object.getOwnPropertyDescriptor(t, k); },
           defineProperty(t, k, d) { return Object.defineProperty(t, k, d), true; },
           deleteProperty(t, k) { return delete t[k]; },
         });
         p.c = 3; Object.defineProperty(p, 'd', { value: 4, enumerable: true, configurable: true });
         const ks = []; for (const k in p) ks.push(k);
         const copy = { ...p }; delete p.d;
         console.log(p.a, 'c' in p, ks.join(), Object.keys(copy).join(), JSON.stringify(t.c),
           Object.entries(p).join('/'));",
        "1! true a,b,c,d a,b,c,d [3] a,1!/b,2!/c,3!\n",
    );
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 20; i++) {
           const r = Proxy.revocable({ i }, {});
           keep.push(r.proxy.i);
           Object.freeze(r.proxy);
           r.revoke();
         }
         console.log(keep.join());",
        "0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19\n",
    );
    assert_same_under_gc_stress(
        "function f(a) {} const arr = [1, 2, 3]; arr.k = 0;
         const strict = new Proxy(arr, { ownKeys(t) { return Reflect.ownKeys(t); } });
         console.log(Reflect.ownKeys(new Proxy(arr, {})).join(),
           Object.getOwnPropertyNames(new Proxy(f, {})).join(),
           Reflect.ownKeys(strict).join());",
        "0,1,2,length,k length,name,prototype 0,1,2,length,k\n",
    );
}

// --- callable proxies (P2b) ---------------------------------------------------------------

/// A proxy over a function is callable (`typeof` `"function"`); without an
/// `apply` trap a call forwards `this` and every argument to the target;
/// the trap receives `(target, thisArgument, argsArray)` with `this` the
/// handler.
#[test]
fn apply_trap_and_forwarding() {
    assert_string(
        "function f(a, b) { return [this === undefined ? 'u' : this.n, a, b].join(':'); }
         const p = new Proxy(f, {});
         [typeof p, typeof new Proxy({}, {}), p(1, 2), p.call({ n: 'o' }, 3, 4)].join()",
        "function,object,u:1:2,o:3:4",
    );
    assert_string(
        "const handler = { apply(t, thisArg, args) {
             return [this === handler, t === f, String(thisArg), Array.isArray(args), args.join('/')].join(':');
           } };
         function f() {}
         const p = new Proxy(f, handler);
         p(1, 2, 3)",
        "true:true:undefined:true:1/2/3",
    );
}

/// A construct trap receives `(target, argsArray, newTarget)` and its result
/// must be an object; without one `new` constructs the target with the
/// proxy as `new.target`. A proxy over a non-constructor is not a
/// constructor; a proxy over an object is not callable.
#[test]
fn construct_trap_and_forwarding() {
    assert_string(
        "class C { constructor(a, b) { this.v = a + b; this.nt = new.target === p; } }
         const p = new Proxy(C, {});
         const o = new p(1, 2);
         [o.v, o.nt, o instanceof C].join()",
        "3,true,true",
    );
    assert_string(
        "function F() {}
         const handler = { construct(t, args, nt) {
             return { same: this === handler && t === F && nt === p, args: args.join('/') };
           } };
         const p = new Proxy(F, handler);
         const o = new p(1, 2, 3);
         [o.same, o.args].join()",
        "true,1/2/3",
    );
    assert_uncaught(
        "const p = new Proxy(function () {}, { construct() { return 1; } }); new p()",
        "TypeError: proxy [[Construct]] must return an object",
    );
    assert_type_errors(
        "",
        &[
            "new (new Proxy(() => 1, {}))()",
            "new (new Proxy(function () {}, { construct() { return null; } }))()",
            "new Proxy({}, {})()",
            "new (new Proxy({}, {}))()",
        ],
    );
}

/// Every call-site kind reaches a callable proxy: a plain call, a method
/// call (`this` the receiver), a spread call with more arguments than the
/// positional slots, `Function.prototype.call` / `apply`, an
/// `Array.prototype.map` callback, a bound proxy, a getter, a
/// `@@toPrimitive` method, a JSON replacer and a promise reaction.
#[test]
fn call_site_kinds() {
    assert_string(
        "const log = [];
         const p = new Proxy(function (...a) { return a.length; }, {
           apply(t, thisArg, args) { log.push(args.length); return Reflect_apply(t, thisArg, args); } });
         function Reflect_apply(t, th, a) { return t.apply(th, a); }
         const o = { m: p };
         p(1); o.m(1, 2); p(...[1, 2, 3, 4, 5, 6, 7, 8]); p.call(null, 1, 2, 3); p.apply(null, [1, 2, 3, 4]);
         [1, 2].map(p); p.bind(null, 1)(2);
         log.join()",
        "1,2,8,3,4,3,3,2",
    );
    assert_string(
        "const p = new Proxy(function () { return this; }, {});
         const o = { m: p };
         [o.m() === o, p.call(o) === o, p.bind(o)() === o].join()",
        "true,true,true",
    );
    assert_string(
        "const getter = new Proxy(function () { return this.x * 2; }, {});
         const o = { x: 21 }; Object.defineProperty(o, 'y', { get: getter });
         const prim = { [Symbol.toPrimitive]: new Proxy(function (hint) { return hint; }, {}) };
         const rep = new Proxy(function (k, v) { return typeof v === 'number' ? v + 1 : v; }, {});
         [o.y, `${prim}`, prim + '', JSON.stringify({ a: 1 }, rep)].join()",
        "42,string,default,{\"a\":2}",
    );
    assert_output(
        "const onOk = new Proxy(function (v) { console.log('then', v); }, {});
         Promise.resolve(7).then(onOk);",
        "then 7\n",
    );
}

/// The spread path past the positional slots keeps every argument, also when
/// the trap itself makes a nested spread call before reading them.
#[test]
fn apply_trap_args_past_k() {
    assert_string(
        "function sum(...a) { return a.reduce((x, y) => x + y, 0); }
         const p = new Proxy(sum, { apply(t, th, args) {
             const inner = sum(...[100, 200, 300, 400, 500, 600]);
             return inner + ':' + args.join('/');
           } });
         p(...[1, 2, 3, 4, 5, 6, 7])",
        "2100:1/2/3/4/5/6/7",
    );
    assert_string(
        "function C(...a) { this.a = a.join('/'); }
         const p = new Proxy(C, {});
         new p(...[1, 2, 3, 4, 5, 6, 7]).a",
        "1/2/3/4/5/6/7",
    );
}

/// `new.target` inside the target is the construct site's: the proxy for
/// `new P()`, the derived class for `super(..)` in `class B extends P`.
#[test]
fn new_target_and_super() {
    assert_string(
        "class A { constructor(x) { this.x = x; this.nt = new.target; } }
         const log = [];
         const P = new Proxy(A, { construct(t, args, nt) { log.push(nt === B); return Reflect_construct(t, args, nt); } });
         function Reflect_construct(t, args, nt) { const o = new t(...args); Object.setPrototypeOf(o, nt.prototype); return o; }
         class B extends P { constructor() { super(5); this.y = 6; } }
         const b = new B();
         [b.x, b.y, b instanceof B, log.join()].join()",
        "5,6,true,true",
    );
    assert_string(
        "class A { constructor() { this.nt = new.target; } }
         const P = new Proxy(A, {});
         class B extends P {}
         const b = new B(); const a = new P();
         [b.nt === B, b instanceof A, a.nt === P, Object.getPrototypeOf(B) === P].join()",
        "true,true,true,true",
    );
}

/// A revoked callable proxy is still `typeof` `"function"`; calling or
/// constructing it throws.
#[test]
fn revoked_callable_proxy() {
    assert_string(
        "const r = Proxy.revocable(function () {}, {}); r.revoke();
         typeof r.proxy",
        "function",
    );
    assert_uncaught(
        "const r = Proxy.revocable(function () {}, {}); r.revoke(); r.proxy()",
        "TypeError: Cannot perform 'apply' on a proxy that has been revoked",
    );
    assert_uncaught(
        "const r = Proxy.revocable(class {}, {}); r.revoke(); new r.proxy()",
        "TypeError: Cannot perform 'construct' on a proxy that has been revoked",
    );
}

/// A callable proxy can be an `apply` / `construct` trap, and a trap that
/// throws propagates out of the call.
#[test]
fn callable_proxy_as_trap_and_throwing_traps() {
    assert_string(
        "const trap = new Proxy(function (t, th, args) { return 'via:' + args[0]; }, {});
         new Proxy(function () {}, { apply: trap })(9)",
        "via:9",
    );
    assert_string(
        "const p = new Proxy(function () {}, { apply() { throw new RangeError('boom'); } });
         (function () { try { p(); return 'no'; } catch (e) { return e instanceof RangeError ? e.message : 'other'; } })()",
        "boom",
    );
}

// --- integration paths (P2b) ---------------------------------------------------------------

/// `Array.isArray` sees through proxies (chains included) and throws on a
/// revoked one; `Array.isArray` as a callback too.
#[test]
fn is_array_through_proxies() {
    assert_string(
        "const a = new Proxy([], {}); const o = new Proxy({}, {});
         [Array.isArray(a), Array.isArray(o), Array.isArray(new Proxy(a, {})),
          [a, o].map(Array.isArray).join('/')].join()",
        "true,false,true,true/false",
    );
    assert_uncaught(
        "const r = Proxy.revocable([], {}); r.revoke(); Array.isArray(r.proxy)",
        "TypeError: Cannot perform 'IsArray' on a proxy that has been revoked",
    );
}

/// `Object.prototype.toString` computes a proxy's builtin tag (Array,
/// Function, Object) before reading `@@toStringTag` through the `get` trap.
#[test]
fn object_to_string_of_proxies() {
    assert_string(
        "const ts = (v) => Object.prototype.toString.call(v);
         const log = [];
         const tagged = new Proxy({}, { get(t, k) { log.push(typeof k); return k === Symbol.toStringTag ? 'X' : undefined; } });
         [ts(new Proxy([], {})), ts(new Proxy(function () {}, {})), ts(new Proxy({}, {})),
          ts(new Proxy(new Error('e'), {})), ts(tagged), log.join('/')].join()",
        "[object Array],[object Function],[object Object],[object Object],[object X],symbol",
    );
    assert_type_errors(
        "const r = Proxy.revocable([], {}); r.revoke(); const p = r.proxy;",
        &["Object.prototype.toString.call(p)"],
    );
}

/// `JSON.stringify` serializes an array proxy through `length` and index
/// `get`s, an object proxy through `ownKeys` / `getOwnPropertyDescriptor` /
/// `get`, omits a callable proxy, uses a proxy array as the replacer's
/// property list, and throws on a revoked proxy.
#[test]
fn json_stringify_through_proxies() {
    assert_string(
        "const log = [];
         const a = new Proxy([1, 2], { get(t, k) { log.push(String(k)); return t[k]; } });
         [JSON.stringify(a), log.join('/')].join('|')",
        "[1,2]|toJSON/length/0/1",
    );
    assert_string(
        "const o = new Proxy({ a: 1, b: [2] }, {});
         const f = new Proxy(function () {}, {});
         [JSON.stringify(o), JSON.stringify({ f, g: 1 }), JSON.stringify([f]), String(JSON.stringify(f)),
          JSON.stringify({ a: 1, b: 2, c: 3 }, new Proxy(['c', 'a'], {})),
          JSON.stringify([new Proxy([1, [2]], {})])].join()",
        "{\"a\":1,\"b\":[2]},{\"g\":1},[null],undefined,{\"c\":3,\"a\":1},[[1,[2]]]",
    );
    assert_type_errors(
        "const r = Proxy.revocable([], {}); r.revoke(); const p = r.proxy;",
        &[
            "JSON.stringify(p)",
            "JSON.stringify({}, p)",
            "JSON.stringify([p])",
        ],
    );
}

/// The `JSON.parse` reviver walks a proxy value (here one a reviver
/// installed) through its traps: `IsArray`, `length`, then each index.
#[test]
fn json_parse_reviver_walks_proxies() {
    assert_string(
        "const seen = [];
         const r = JSON.parse('{\"a\":1,\"b\":2}', function (k, v) {
           seen.push(k);
           if (k === 'a') this.b = new Proxy([5, 6], {});
           return v;
         });
         [seen.join('/'), JSON.stringify(r)].join('|')",
        "a/0/1/b/|{\"a\":1,\"b\":[5,6]}",
    );
}

/// `instanceof` walks the left operand's chain through `getPrototypeOf`
/// traps, reads `prototype` and `@@hasInstance` of a callable proxy through
/// its `get` trap; `isPrototypeOf` walks the same way.
#[test]
fn instanceof_through_proxies() {
    assert_string(
        "class C {}
         const lhs = new Proxy({}, { getPrototypeOf() { return C.prototype; } });
         const arr = new Proxy({}, { getPrototypeOf() { return Array.prototype; } });
         const P = new Proxy(C, {});
         const H = new Proxy(function () {}, { get(t, k) { return k === Symbol.hasInstance ? () => true : t[k]; } });
         [lhs instanceof C, arr instanceof Array, arr instanceof Object, new C() instanceof P,
          1 instanceof H, C.prototype.isPrototypeOf(lhs), Array.prototype.isPrototypeOf(arr),
          Object.create(lhs) instanceof C].join()",
        "true,true,true,true,true,true,true,true",
    );
    assert_uncaught(
        "class C {}
         const lhs = new Proxy({}, { getPrototypeOf() { throw new RangeError('gp'); } });
         lhs instanceof C",
        "RangeError: gp",
    );
}

/// `concat` spreads a proxy of an array (`IsArray`) and an object proxy whose
/// `get` trap answers `@@isConcatSpreadable`; `flat` descends into a proxy
/// of an array; `ArraySpeciesCreate` sees a proxy receiver as an array.
#[test]
fn array_methods_through_proxies() {
    assert_string(
        "const spreadable = new Proxy({ length: 2, 0: 'x', 1: 'y' }, {
           get(t, k) { return k === Symbol.isConcatSpreadable ? true : t[k]; } });
         [[0].concat(new Proxy([1, 2], {})).join('/'), [0].concat(spreadable).join('/'),
          [[0], new Proxy([1, [2]], {})].flat().length,
          Array.prototype.map.call(new Proxy([1, 2], {}), (x) => x * 2).join('/')].join()",
        "0/1/2,0/x/y,3,2/4",
    );
    assert_type_errors(
        "const r = Proxy.revocable([], {}); r.revoke(); const p = r.proxy;",
        &["[].concat(p)", "[p].flat()"],
    );
    assert_string(
        "const r = Proxy.revocable([], {}); r.revoke(); [r.proxy].flat(0).length",
        "1",
    );
}

/// `Function.prototype.toString` of a callable proxy is the anonymous
/// native form; a non-callable proxy throws. `bind` of a callable proxy
/// reads the target's prototype through `getPrototypeOf`, then `length` and
/// `name` through the traps (after the `get` of `bind` itself).
#[test]
fn function_to_string_and_bind_of_proxies() {
    assert_string(
        "Function.prototype.toString.call(new Proxy(function f() {}, {}))",
        "function () { [native code] }",
    );
    assert_type_errors("", &["Function.prototype.toString.call(new Proxy({}, {}))"]);
    assert_string(
        "const log = [];
         function f(a, b, c) { return [this.n, a, b, c].join(); }
         const p = new Proxy(f, {
           getPrototypeOf(t) { log.push('proto'); return Object.getPrototypeOf(t); },
           getOwnPropertyDescriptor(t, k) { log.push('gopd:' + k); return Object.getOwnPropertyDescriptor(t, k); },
           get(t, k) { log.push('get:' + String(k)); return t[k]; } });
         const b = p.bind({ n: 'o' }, 1);
         [b(2, 3), b.name, b.length, log.join(' ')].join('|')",
        "o,1,2,3|bound f|2|get:bind proto gopd:length get:length get:name",
    );
    assert_string(
        "class C { constructor(a) { this.a = a; } }
         const B = new Proxy(C, {}).bind(null, 4);
         const o = new B();
         [o.a, o instanceof C].join()",
        "4,true",
    );
}

/// The trampoline's argument array and the construct path stay correct with
/// a collection at every allocation.
#[test]
fn callable_proxies_survive_gc_stress() {
    assert_same_under_gc_stress(
        "function f(...a) { return a.map((x) => [x]).length; }
         class C { constructor(...a) { this.a = a.map(String); } }
         const p = new Proxy(f, { apply(t, th, args) { return t(...args.concat(['z'])); } });
         const q = new Proxy(C, { construct(t, args, nt) { return new t(...args, 'w'); } });
         const r = new Proxy(C, {});
         const out = [];
         for (let i = 0; i < 10; i++) {
           out.push(p(...[i, 1, 2, 3, 4, 5, 6, 7]), new q(i, 1).a.join(''), new r(...[i, 2, 3, 4, 5, 6]).a.length);
         }
         console.log(out.join(), Array.isArray(new Proxy([], {})), JSON.stringify(new Proxy([1, { a: 2 }], {})));",
        "9,01w,6,9,11w,6,9,21w,6,9,31w,6,9,41w,6,9,51w,6,9,61w,6,9,71w,6,9,81w,6,9,91w,6 true [1,{\"a\":2}]\n",
    );
}

// --- GetFunctionRealm (F7) ---------------------------------------------------------------

/// A base constructor reached through a proxy `newTarget` whose `prototype`
/// read revokes it and returns a non-object: `GetFunctionRealm` of the now
/// revoked proxy throws before the body runs; a live proxy (or a proxy chain)
/// falls back to `%Object.prototype%`.
#[test]
fn construct_revoked_new_target_throws() {
    assert_type_errors(
        "",
        &[
            "const h = Proxy.revocable(function () {}, { get() { h.revoke(); } });
           new h.proxy()",
            "let ran = false;
           function F() { ran = true; }
           const h = Proxy.revocable(function () {}, { get() { h.revoke(); } });
           Reflect.construct(F, [], h.proxy)",
            "const inner = Proxy.revocable(function () {}, {});
           const outer = new Proxy(inner.proxy, { get() { inner.revoke(); return 1; } });
           Reflect.construct(function () {}, [], outer)",
        ],
    );
    assert_string(
        "let ran = false;
         function F() { ran = true; }
         const h = Proxy.revocable(function () {}, { get() { h.revoke(); } });
         try { Reflect.construct(F, [], h.proxy); } catch (e) {}
         const p = new Proxy(function () {}, { get() { return 1; } });
         const o = Reflect.construct(F, [], new Proxy(p, {}));
         [ran, Object.getPrototypeOf(o) === Object.prototype].join()",
        "false,true",
    );
}
