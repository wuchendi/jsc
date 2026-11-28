//! Integration tests: Proxy exotic objects (DESIGN.md §6.11, stage P2a) —
//! `new Proxy`, `Proxy.revocable`, the eleven object internal-method traps
//! with their invariant checks, proxies on prototype chains and as
//! receivers, the generic algorithms over proxies (`Object.keys`, spread,
//! `for-in`, integrity levels) and the renderer.

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
         const b = { __proto__: base, m() { return super.x; } };
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
}
