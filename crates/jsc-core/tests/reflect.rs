//! Integration tests: the `Reflect` namespace (DESIGN.md §6.11, stage P3) —
//! its shape, the thirteen functions (argument checks, results where the
//! `Object` twin throws, receivers, own-key order), `Reflect.construct` with a
//! `newTarget` for user classes and every builtin constructor family, and the
//! constructor-body argument snapshots a user-code `newTarget.prototype` read
//! makes reachable.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_same_under_gc_stress, assert_string, assert_uncaught};

/// `body` (a function body run inside `try`) throws a `TypeError`.
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

// --- the namespace --------------------------------------------------------------------------

/// `Reflect` is an ordinary object inheriting from `%Object.prototype%`
/// with `@@toStringTag` `"Reflect"` (configurable only); it is neither
/// callable nor a constructor; its functions are non-enumerable methods with
/// the spec names and lengths.
#[test]
fn namespace_shape() {
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(Reflect, Symbol.toStringTag);
         [typeof Reflect, Object.getPrototypeOf(Reflect) === Object.prototype,
          Object.prototype.toString.call(Reflect), d.value, d.writable, d.enumerable,
          d.configurable, Object.keys(Reflect).length].join()",
        "object,true,[object Reflect],Reflect,false,false,true,0",
    );
    assert_string(
        "Object.getOwnPropertyNames(Reflect)
           .map((k) => k + '/' + Reflect[k].length + '/' + (Reflect[k].name === k)).join()",
        "apply/3/true,construct/2/true,defineProperty/3/true,deleteProperty/2/true,\
         get/2/true,getOwnPropertyDescriptor/2/true,getPrototypeOf/1/true,has/2/true,\
         isExtensible/1/true,ownKeys/1/true,preventExtensions/1/true,set/3/true,\
         setPrototypeOf/2/true",
    );
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(globalThis, 'Reflect');
         [d.writable, d.enumerable, d.configurable, d.value === Reflect,
          'prototype' in Reflect.get, typeof Reflect.get].join()",
        "true,false,true,true,false,function",
    );
    assert_type_errors(
        "",
        &[
            "Reflect()",
            "new Reflect()",
            "new Reflect.get({}, 'a')",
            "Reflect.construct(Reflect.get, [])",
        ],
    );
}

/// Every function throws a `TypeError` for a non-object target, before it
/// converts the property key.
#[test]
fn non_object_targets_throw() {
    let mut bodies = Vec::new();
    for target in ["1", "'s'", "undefined", "null", "Symbol()", "true"] {
        for call in [
            "Reflect.defineProperty(T, 'a', {})",
            "Reflect.deleteProperty(T, 'a')",
            "Reflect.get(T, 'a')",
            "Reflect.getOwnPropertyDescriptor(T, 'a')",
            "Reflect.getPrototypeOf(T)",
            "Reflect.has(T, 'a')",
            "Reflect.isExtensible(T)",
            "Reflect.ownKeys(T)",
            "Reflect.preventExtensions(T)",
            "Reflect.set(T, 'a', 1)",
            "Reflect.setPrototypeOf(T, null)",
        ] {
            bodies.push(call.replace('T', target));
        }
    }
    let refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    assert_type_errors("", &refs);
    assert_type_errors("", &["Reflect.get()", "Reflect.ownKeys()"]);
    // The target check comes before `ToPropertyKey`.
    assert_string(
        "const log = [];
         const key = { toString() { log.push('key'); return 'a'; } };
         for (const f of ['get', 'has', 'deleteProperty', 'getOwnPropertyDescriptor', 'set',
                          'defineProperty']) {
           try { Reflect[f](1, key, {}); } catch (e) { log.push(e.constructor.name); }
         }
         log.join()",
        "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
    );
    assert_uncaught(
        "Reflect.getPrototypeOf(1)",
        "TypeError: Reflect.getPrototypeOf called on non-object",
    );
    assert_uncaught(
        "Reflect.set('s', 'a', 1)",
        "TypeError: Reflect.set called on non-object",
    );
}

// --- apply / construct ----------------------------------------------------------------------

/// `Reflect.apply` calls with the given `this` and
/// `CreateListFromArrayLike(args)`: an array, an array-like object, many
/// arguments; a non-callable target or a non-object list (`undefined`
/// included, unlike `Function.prototype.apply`) throws.
#[test]
fn apply() {
    assert_string(
        "function f(a, b) { return [this.x, a, b].join(); }
         function all(...xs) { return xs.join(); }
         [Reflect.apply(f, { x: 1 }, [2, 3]),
          Reflect.apply(f, { x: 'o' }, { length: 2, 0: 'a', 1: 'b', 2: 'c' }),
          Reflect.apply(all, null, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
          Reflect.apply(Math.max, undefined, [1, 9, 3]),
          Reflect.apply(String.prototype.slice, 'hello', [1, 3]),
          Reflect.apply(all, null, [])].join('|')",
        "1,2,3|o,a,b|1,2,3,4,5,6,7,8,9,10|9|el|",
    );
    assert_type_errors(
        "function f() {}",
        &[
            "Reflect.apply(f, null)",
            "Reflect.apply(f, null, undefined)",
            "Reflect.apply(f, null, null)",
            "Reflect.apply(f, null, 1)",
            "Reflect.apply({}, null, [])",
            "Reflect.apply(1, null, [])",
            "Reflect.apply()",
        ],
    );
    // The callability check precedes reading the list.
    assert_string(
        "let read = false;
         const list = { get length() { read = true; return 0; } };
         try { Reflect.apply({}, null, list); } catch (e) {}
         String(read)",
        "false",
    );
    assert_uncaught(
        "Reflect.apply(1, null, [])",
        "TypeError: Reflect.apply target is not a function",
    );
}

/// `Reflect.construct(F, args, newTarget)`: `new.target` is `newTarget`,
/// the instance's prototype comes from `newTarget.prototype` (the realm's
/// `%Object.prototype%` when that is not an object), a derived class passes
/// it through `super`, and `newTarget` defaults to `F` only when absent.
#[test]
fn construct_user_classes() {
    assert_string(
        "function F(a, b) { this.sum = a + b; this.nt = new.target; }
         function NT() {}
         class A { constructor(x) { this.x = x; this.nt = new.target; } }
         class B extends A { constructor(x) { super(x * 2); } }
         const f = Reflect.construct(F, [1, 2], NT);
         const g = Reflect.construct(F, [3, 4]);
         const b = Reflect.construct(B, [5], NT);
         const a = Reflect.construct(A, [6], B);
         NT.prototype = 7;
         const h = Reflect.construct(F, [0, 0], NT);
         [f.sum, f.nt === NT, Object.getPrototypeOf(f) === NT.prototype || 'old',
          g.sum, g.nt === F, g instanceof F,
          b.x, b.nt === NT, b instanceof B,
          a.x, a.nt === B, a instanceof B,
          Object.getPrototypeOf(h) === Object.prototype].join()",
        "3,true,old,7,true,true,10,true,false,6,true,true,true",
    );
    assert_string(
        "function F() { this.ok = 1; }
         function NT() {}
         const o = Reflect.construct(F, [], NT);
         (Object.getPrototypeOf(o) === NT.prototype) + ',' + o.ok",
        "true,1",
    );
    assert_type_errors(
        "function F() {} const arrow = () => {}; const o = { m() {} };",
        &[
            "Reflect.construct(F, [], undefined)",
            "Reflect.construct(F, [], 1)",
            "Reflect.construct(F, [], arrow)",
            "Reflect.construct(F, [], o.m)",
            "Reflect.construct(arrow, [])",
            "Reflect.construct(o.m, [])",
            "Reflect.construct(Math.max, [])",
            "Reflect.construct(1, [])",
            "Reflect.construct(F)",
            "Reflect.construct(F, 1)",
            "Reflect.construct(F, null)",
            "Reflect.construct()",
        ],
    );
    assert_uncaught(
        "Reflect.construct(() => 1, [])",
        "TypeError: Reflect.construct target is not a constructor",
    );
    assert_uncaught(
        "Reflect.construct(function () {}, [], () => 1)",
        "TypeError: Reflect.construct newTarget is not a constructor",
    );
}

/// Every builtin constructor family honours `newTarget` for
/// `GetPrototypeFromConstructor` and keeps its own internal slots; a
/// `newTarget.prototype` that is not an object falls back to the
/// constructor's realm prototype.
#[test]
fn construct_builtin_families() {
    let prelude = "function check(C, args, test) {
           function NT() {}
           NT.prototype = Object.create(C.prototype);
           const o = Reflect.construct(C, args, NT);
           function Prim() {}
           Prim.prototype = null;
           const q = Reflect.construct(C, args, Prim);
           return [Object.getPrototypeOf(o) === NT.prototype, test(o),
                   Object.getPrototypeOf(q) === C.prototype, test(q)].join('/');
         }";
    for (call, expected) in [
        (
            "check(Error, ['m', { cause: 1 }], (o) => o.message + o.cause)",
            "true/m1/true/m1",
        ),
        (
            "check(TypeError, ['t'], (o) => o instanceof Error && o.message)",
            "true/t/true/t",
        ),
        (
            "check(RangeError, [], (o) => Object.prototype.toString.call(o))",
            "true/[object Error]/true/[object Error]",
        ),
        (
            "check(AggregateError, [[1, 2], 'a', { cause: 'c' }],
                   (o) => o.errors.join('+') + o.message + o.cause)",
            "true/1+2ac/true/1+2ac",
        ),
        (
            "check(Array, [1, 2, 3], (o) => Array.isArray(o) && o.join('-'))",
            "true/1-2-3/true/1-2-3",
        ),
        ("check(Array, [4], (o) => o.length)", "true/4/true/4"),
        (
            "check(Promise, [(r) => r(1)], (o) => typeof Promise.prototype.then.call(o, () => 0))",
            "true/object/true/object",
        ),
        (
            "check(Map, [[[1, 'x']]], (o) => Map.prototype.get.call(o, 1))",
            "true/x/true/x",
        ),
        (
            "check(Set, [[1, 2, 2]], (o) => Set.prototype.has.call(o, 2) && o.size)",
            "true/2/true/2",
        ),
        (
            "check(WeakMap, [], (o) => WeakMap.prototype.set.call(o, check, 1) === o)",
            "true/true/true/true",
        ),
        (
            "check(WeakSet, [[check]], (o) => WeakSet.prototype.has.call(o, check))",
            "true/true/true/true",
        ),
        (
            "check(WeakRef, [check], (o) => WeakRef.prototype.deref.call(o) === check)",
            "true/true/true/true",
        ),
        (
            "check(FinalizationRegistry, [() => 0],
                   (o) => FinalizationRegistry.prototype.unregister.call(o, check))",
            "true/false/true/false",
        ),
        (
            "check(Date, [86400000], (o) => Date.prototype.getTime.call(o))",
            "true/86400000/true/86400000",
        ),
        (
            "check(Date, [1970, 0, 2], (o) => Date.prototype.toISOString.call(o))",
            "true/1970-01-02T00:00:00.000Z/true/1970-01-02T00:00:00.000Z",
        ),
        (
            "check(Boolean, [1], (o) => Boolean.prototype.valueOf.call(o))",
            "true/true/true/true",
        ),
        (
            "check(Number, ['5'], (o) => Number.prototype.valueOf.call(o))",
            "true/5/true/5",
        ),
        (
            "check(String, ['ab'], (o) => String.prototype.toString.call(o) + o.length)",
            "true/ab2/true/ab2",
        ),
        (
            "check(Object, [], (o) => typeof o)",
            "true/object/true/object",
        ),
    ] {
        assert_string(&format!("{prelude}\n{call}"), expected);
    }
}

/// `Object`'s realm fallback, the wrapper constructors' single
/// `newTarget.prototype` read, the constructors that refuse `new`, and the
/// abstract `Iterator`.
#[test]
fn construct_builtin_fallbacks_and_refusals() {
    // `Object` with a `newTarget` other than `%Object%` falls back to
    // `%Object.prototype%`, its own realm prototype.
    assert_string(
        "function Prim() {} Prim.prototype = 1;
         String(Object.getPrototypeOf(Reflect.construct(Object, [], Prim)) === Object.prototype)",
        "true",
    );
    // A wrapper constructor reads `newTarget.prototype` once.
    assert_string(
        "let reads = 0;
         const NT = function () {}.bind(null);
         Object.defineProperty(NT, 'prototype', { get() { reads++; return Number.prototype; } });
         const n = Reflect.construct(Number, [3], NT);
         const s = Reflect.construct(String, ['x'], NT);
         const b = Reflect.construct(Boolean, [0], NT);
         [reads, Number.prototype.valueOf.call(n), typeof s, typeof b].join()",
        "3,3,object,object",
    );
    assert_type_errors(
        "function NT() {}",
        &[
            "Reflect.construct(Symbol, [], NT)",
            "Reflect.construct(Function, [], NT)",
            "Reflect.construct(Proxy, [{}, {}], 1)",
        ],
    );
    // `Iterator` is abstract unless `newTarget` differs.
    assert_string(
        "function NT() {} NT.prototype = Object.create(Iterator.prototype);
         const i = Reflect.construct(Iterator, [], NT);
         let threw = false;
         try { Reflect.construct(Iterator, []); } catch (e) { threw = e instanceof TypeError; }
         [Object.getPrototypeOf(i) === NT.prototype, threw].join()",
        "true,true",
    );
}

/// A bound function constructs its target with `newTarget` (or the target
/// when `newTarget` is the bound function itself); a constructor proxy
/// hands `newTarget` to its `construct` trap, or to its target without one.
#[test]
fn construct_bound_and_proxy() {
    assert_string(
        "function F(a, b) { this.v = a + b; this.nt = new.target; }
         function NT() {}
         const bf = F.bind(null, 1);
         const x = Reflect.construct(bf, [2], NT);
         const y = Reflect.construct(bf, [3]);
         const P = new Proxy(F, {});
         const z = Reflect.construct(P, [4, 5], NT);
         let seen;
         const Q = new Proxy(F, { construct(t, args, nt) { seen = nt; return { n: args.length }; } });
         const w = Reflect.construct(Q, [1, 2, 3], NT);
         [x.v, x.nt === NT, Object.getPrototypeOf(x) === NT.prototype,
          y.v, y.nt === F, y instanceof F,
          z.v, z.nt === NT, Object.getPrototypeOf(z) === NT.prototype,
          w.n, seen === NT].join()",
        "3,true,true,4,true,true,9,true,true,3,true",
    );
    // A proxy is an acceptable `newTarget`: its `get` trap supplies
    // `prototype`.
    assert_string(
        "class A {}
         const proto = { tag: 'p' };
         const NT = new Proxy(function () {}, {
           get(t, k) { return k === 'prototype' ? proto : t[k]; } });
         const a = Reflect.construct(A, [], NT);
         const m = Reflect.construct(Map, [], NT);
         [a.tag, Object.getPrototypeOf(m) === proto].join()",
        "p,true",
    );
    // The collection constructors look the adder up on the new object, whose
    // prototype comes from `newTarget`: a `newTarget.prototype` without `set`
    // makes a non-empty iterable throw.
    assert_uncaught(
        "function NT() {} Reflect.construct(Map, [[[1, 2]]], NT)",
        "TypeError: 'undefined' returned for property 'set' of object '#<Map>' is not a function",
    );
}

// --- property functions ---------------------------------------------------------------------

/// `Reflect.defineProperty` returns `false` where `Object.defineProperty`
/// throws; `ToPropertyKey` then `ToPropertyDescriptor`; a non-object
/// descriptor throws.
#[test]
fn define_property() {
    assert_string(
        "const o = {};
         const sym = Symbol('s');
         const r = [
           Reflect.defineProperty(o, 'a', { value: 1 }),
           Reflect.defineProperty(o, 'a', { value: 2 }),
           Reflect.defineProperty(o, 'a', { value: 1 }),
           Reflect.defineProperty(o, sym, { get() { return 's'; }, configurable: true }),
           Reflect.defineProperty(o, { toString() { return 'k'; } }, { value: 3, writable: true }),
           Reflect.defineProperty(Object.freeze({}), 'x', { value: 1 }),
           Reflect.defineProperty([], 'length', { value: 2 }),
           Reflect.defineProperty(Object.preventExtensions({}), 'x', { value: 1 }),
         ];
         r.join() + '|' + [o.a, o[sym], o.k].join()",
        "true,false,true,true,true,false,true,false|1,s,3",
    );
    assert_type_errors(
        "const o = {};",
        &[
            "Reflect.defineProperty(o, 'a')",
            "Reflect.defineProperty(o, 'a', 1)",
            "Reflect.defineProperty(o, 'a', { get: 1 })",
            "Reflect.defineProperty(o, 'a', { get() {}, value: 1 })",
        ],
    );
    // An invalid array `length` is a `RangeError`, not `false`.
    assert_string(
        "try { Reflect.defineProperty([], 'length', { value: -1 }); 'none' }
         catch (e) { e.constructor.name }",
        "RangeError",
    );
}

/// `Reflect.deleteProperty` returns `false` for a non-configurable property
/// (no throw) and `true` for a deleted or absent one.
#[test]
fn delete_property() {
    assert_string(
        "const o = { a: 1 };
         Object.defineProperty(o, 'fixed', { value: 1 });
         const arr = [1, 2];
         [Reflect.deleteProperty(o, 'a'), 'a' in o, Reflect.deleteProperty(o, 'missing'),
          Reflect.deleteProperty(o, 'fixed'), o.fixed,
          Reflect.deleteProperty(arr, 0), 0 in arr, Reflect.deleteProperty(arr, 'length'),
          Reflect.deleteProperty(Object.freeze({ z: 1 }), 'z')].join()",
        "true,false,true,false,1,true,false,false,false",
    );
}

/// `Reflect.get` walks the prototype chain and calls a getter with the
/// receiver (the target when absent, `undefined` when passed).
#[test]
fn get_with_receiver() {
    assert_string(
        "const sym = Symbol();
         const proto = { get g() { return this === undefined ? 'undef' : this.v; }, [sym]: 's' };
         const o = Object.create(proto); o.v = 'own';
         const r = { v: 'recv' };
         [Reflect.get(o, 'g'), Reflect.get(o, 'g', r), Reflect.get(o, 'g', undefined),
          Reflect.get(o, sym), Reflect.get(o, 'missing'), Reflect.get([7, 8], 1),
          Reflect.get('ab'.split(''), 'length'),
          Reflect.get(o, { toString() { return 'v'; } })].join()",
        "own,recv,undef,s,,8,2,own",
    );
}

/// `Reflect.getOwnPropertyDescriptor` is the own descriptor or
/// `undefined`, without `ToObject` of the target.
#[test]
fn get_own_property_descriptor() {
    assert_string(
        "const o = { a: 1, get g() { return 2; } };
         const d = Reflect.getOwnPropertyDescriptor(o, 'a');
         const g = Reflect.getOwnPropertyDescriptor(o, 'g');
         const l = Reflect.getOwnPropertyDescriptor([1, 2], 'length');
         [d.value, d.writable, d.enumerable, d.configurable, typeof g.get, g.set,
          Reflect.getOwnPropertyDescriptor(o, 'toString'),
          l.value, l.writable, l.configurable].join()",
        "1,true,true,true,function,,,2,true,false",
    );
}

/// `Reflect.getPrototypeOf` / `setPrototypeOf`: `setPrototypeOf` returns
/// `false` for a non-extensible target, a cycle and the immutable
/// `%Object.prototype%`; a prototype that is neither an object nor `null`
/// throws.
#[test]
fn prototypes() {
    assert_string(
        "const a = {}; const b = Object.create(a);
         const sealed = Object.preventExtensions({});
         [Reflect.getPrototypeOf(b) === a, Reflect.getPrototypeOf(Object.create(null)),
          Reflect.getPrototypeOf([]) === Array.prototype,
          Reflect.setPrototypeOf(a, Array.prototype), Array.isArray(a) || a instanceof Array,
          Reflect.setPrototypeOf(a, b),
          Reflect.setPrototypeOf(sealed, null), Reflect.setPrototypeOf(sealed, Object.prototype),
          Reflect.setPrototypeOf(Object.prototype, {}),
          Reflect.setPrototypeOf(Object.prototype, null),
          Reflect.setPrototypeOf(b, null), Reflect.getPrototypeOf(b)].join()",
        "true,,true,true,true,false,false,true,false,true,true,",
    );
    assert_type_errors(
        "const o = {};",
        &[
            "Reflect.setPrototypeOf(o)",
            "Reflect.setPrototypeOf(o, undefined)",
            "Reflect.setPrototypeOf(o, 1)",
            "Reflect.setPrototypeOf(o, 'p')",
        ],
    );
}

/// `Reflect.has` is `HasProperty` (the prototype chain included).
#[test]
fn has() {
    assert_string(
        "const sym = Symbol();
         const o = Object.create({ inherited: 1, [sym]: 2 }); o.own = 0;
         [Reflect.has(o, 'own'), Reflect.has(o, 'inherited'), Reflect.has(o, sym),
          Reflect.has(o, 'toString'), Reflect.has(o, 'nope'), Reflect.has([1], 0),
          Reflect.has([1], 1), Reflect.has([], 'length'),
          Reflect.has(function () {}, 'call')].join()",
        "true,true,true,true,false,true,false,true,true",
    );
}

/// `Reflect.isExtensible` / `preventExtensions` return booleans.
#[test]
fn extensibility() {
    assert_string(
        "const o = {};
         [Reflect.isExtensible(o), Reflect.preventExtensions(o), Reflect.isExtensible(o),
          Reflect.preventExtensions(o), Reflect.isExtensible(Object.freeze([])),
          Reflect.isExtensible(function () {})].join()",
        "true,true,false,true,false,true",
    );
}

/// `Reflect.ownKeys`: integer keys ascending, then the other strings in
/// insertion order, then symbols in insertion order; an array's `length`
/// follows its indices; non-enumerable keys included.
#[test]
fn own_keys_order() {
    assert_string(
        "const s1 = Symbol('s1'); const s2 = Symbol('s2');
         const o = { b: 1, [s2]: 1, 2: 1, a: 1, 0: 1, [s1]: 1 };
         Object.defineProperty(o, 'hidden', { value: 1 });
         Reflect.ownKeys(o).map(String).join()",
        "0,2,b,a,hidden,Symbol(s2),Symbol(s1)",
    );
    assert_string(
        "const s = Symbol('t');
         const a = [1, , 3]; a.x = 1; a[s] = 2;
         Reflect.ownKeys(a).map(String).join()",
        "0,2,length,x,Symbol(t)",
    );
    assert_string(
        "function f(a, b) {}
         const ks = Reflect.ownKeys(f);
         [ks.includes('length'), ks.includes('name'), ks.includes('prototype'),
          Reflect.ownKeys({}).length, Array.isArray(Reflect.ownKeys({}))].join()",
        "true,true,true,0,true",
    );
}

/// `Reflect.set` returns `false` where an assignment would throw, and
/// honours the receiver: a setter runs with it, a data property is
/// created or updated on it, and a non-object or read-only receiver fails.
#[test]
fn set_with_receiver() {
    assert_string(
        "const o = {};
         Object.defineProperty(o, 'ro', { value: 1 });
         const log = [];
         const proto = { set s(v) { log.push(this === r ? 'recv' : 'other'); } };
         const t = Object.create(proto);
         const r = {};
         const frozen = Object.freeze({ a: 1 });
         [Reflect.set(o, 'x', 1), o.x, Reflect.set(o, 'ro', 2), o.ro,
          Reflect.set(t, 's', 1, r), log.join(),
          Reflect.set({ d: 1 }, 'd', 5, r), r.d,
          Reflect.set(frozen, 'a', 2), Reflect.set({}, 'a', 2, frozen),
          Reflect.set({}, 'a', 2, 1),
          Reflect.set([], 'length', 3), Reflect.set(Object.preventExtensions({}), 'n', 1),
          Reflect.set({ get g() { return 1; } }, 'g', 2),
          Reflect.set({}, { toString() { return 'k'; } }, 4, r), r.k].join()",
        "true,1,false,1,true,recv,true,5,false,false,false,true,false,false,true,4",
    );
    // An existing receiver accessor or read-only data property fails.
    assert_string(
        "const r = { get a() { return 1; } };
         const q = {}; Object.defineProperty(q, 'a', { value: 1, configurable: true });
         [Reflect.set({}, 'a', 2, r), Reflect.set({}, 'a', 2, q), q.a].join()",
        "false,false,1",
    );
}

// --- proxies --------------------------------------------------------------------------------

/// Over a proxy each function runs the matching trap with the spec
/// arguments and returns the trap's boolean instead of throwing.
#[test]
fn functions_over_proxies() {
    assert_string(
        "const log = [];
         const t = { a: 1 };
         const p = new Proxy(t, {
           get(t, k, r) { log.push('get:' + String(k) + ':' + (r === recv)); return 'g'; },
           set(t, k, v, r) { log.push('set:' + k + '=' + v + ':' + (r === p)); return false; },
           has(t, k) { log.push('has:' + k); return false; },
           deleteProperty(t, k) { log.push('del:' + k); return false; },
           defineProperty(t, k, d) { log.push('def:' + k + ':' + d.value); return false; },
           ownKeys(t) { log.push('keys'); return ['a', 'z']; },
           getPrototypeOf(t) { log.push('gpo'); return Array.prototype; },
           setPrototypeOf(t, v) { log.push('spo'); return false; },
           isExtensible(t) { log.push('ie'); return true; },
           preventExtensions(t) { log.push('pe'); return false; },
           getOwnPropertyDescriptor(t, k) { log.push('gopd:' + k); return undefined; },
         });
         const recv = {};
         const r = [Reflect.get(p, 'a', recv), Reflect.set(p, 'a', 2), Reflect.has(p, 'a'),
           Reflect.deleteProperty(p, 'a'), Reflect.defineProperty(p, 'b', { value: 3 }),
           Reflect.ownKeys(p).join('+'), Reflect.getPrototypeOf(p) === Array.prototype,
           Reflect.setPrototypeOf(p, null), Reflect.isExtensible(p),
           Reflect.preventExtensions(p), Reflect.getOwnPropertyDescriptor(p, 'q')];
         r.join() + '|' + log.join()",
        "g,false,false,false,false,a+z,true,false,true,false,|get:a:true,set:a=2:true,has:a,\
         del:a,def:b:3,keys,gpo,spo,ie,pe,gopd:q",
    );
    // A trap-less proxy forwards; a revoked one throws.
    assert_string(
        "const t = { a: 1 };
         const { proxy, revoke } = Proxy.revocable(t, {});
         const before = [Reflect.get(proxy, 'a'), Reflect.set(proxy, 'b', 2), t.b,
           Reflect.has(proxy, 'b'), Reflect.ownKeys(proxy).join('+')].join();
         revoke();
         let threw = 0;
         for (const f of [() => Reflect.get(proxy, 'a'), () => Reflect.ownKeys(proxy),
                          () => Reflect.isExtensible(proxy), () => Reflect.apply(proxy, null, [])]) {
           try { f(); } catch (e) { if (e instanceof TypeError) threw++; }
         }
         before + '|' + threw",
        "1,true,2,true,a+b|4",
    );
    // `Reflect.apply` of a callable proxy runs its `apply` trap.
    assert_string(
        "const f = new Proxy(function (a, b) { return a + b; },
           { apply(t, th, args) { return 'trap:' + args.join('+') + ':' + th.n; } });
         Reflect.apply(f, { n: 1 }, [2, 3])",
        "trap:2+3:1",
    );
}

// --- argument snapshots (phase-4 leftover) ------------------------------------------------

/// The program of [`construct_new_target_prototype_getter_keeps_args`]:
/// `newTarget` is a proxy over a function whose `get` trap, for
/// `"prototype"`, first makes a nested spread call with more than `K`
/// arguments (every direct call here passes at most three, so `K` is small)
/// and then returns the realm prototype; every constructor must still see
/// its own arguments, not the nested call's.
const STALE_ARGS_PROGRAM: &str = "
    function sum(...xs) { let s = 0; for (const x of xs) s += x; return s; }
    let seen = 0;
    function nt(proto) {
      return new Proxy(function () {}, {
        get(t, k, r) {
          if (k === 'prototype') { seen += sum(...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]); return proto; }
          return t[k];
        },
      });
    }
    const d = Reflect.construct(Date, [2020, 0, 2, 3, 4, 5, 6], nt(Date.prototype));
    const e = Reflect.construct(Error, ['msg', { cause: 'why' }], nt(Error.prototype));
    const g = Reflect.construct(AggregateError, [[1, 2], 'agg', { cause: 'c' }],
                                nt(AggregateError.prototype));
    const a = Reflect.construct(Array, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], nt(Array.prototype));
    class C { constructor(...xs) { this.xs = xs; } }
    const c = Reflect.construct(C, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], nt(C.prototype));
    const bnt = function () {}.bind(null);
    Object.defineProperty(bnt, 'prototype', {
      get() { seen += sum(...[10, 20, 30, 40, 50, 60]); return Date.prototype; } });
    const d2 = Reflect.construct(Date, [2021, 1, 3, 4, 5, 6, 7], bnt);
    const p = Reflect.construct(Promise, [(res) => res('ok')], nt(Promise.prototype));
    p.then((v) => console.log([d.toISOString(), e.message, e.cause, g.errors.join('+'), g.message,
      g.cause, a.join(), a.length, Array.isArray(a), c.xs.join(), c instanceof C,
      d2.toISOString(), v, seen].join(' | ')));
";

/// The expected output of [`STALE_ARGS_PROGRAM`]: six proxy `prototype`
/// reads (55 each) and one accessor read (210), one per construction.
const STALE_ARGS_OUTPUT: &str = "2020-01-02T03:04:05.006Z | msg | why | 1+2 | agg | c | \
    1,2,3,4,5,6,7,8,9,10,11,12 | 12 | true | 1,2,3,4,5,6,7,8,9,10,11,12 | true | \
    2021-02-03T04:05:06.007Z | ok | 540\n";

/// A user-code `newTarget.prototype` read that makes a nested spread call
/// runs before the constructor bodies read their arguments past the `K`
/// slots (`Body::snapshotted`, `Frame::snapshot`): builtin constructors and
/// a user class still see their own arguments.
#[test]
fn construct_new_target_prototype_getter_keeps_args() {
    assert_output(STALE_ARGS_PROGRAM, STALE_ARGS_OUTPUT);
}

/// [`construct_new_target_prototype_getter_keeps_args`] with a collection at
/// every allocation.
#[test]
fn construct_new_target_prototype_getter_keeps_args_gc_stress() {
    assert_same_under_gc_stress(STALE_ARGS_PROGRAM, STALE_ARGS_OUTPUT);
}

/// The allocating paths (argument lists, descriptor objects, key lists, the
/// constructed instances) stay correct with a collection at every
/// allocation.
#[test]
fn functions_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const out = [];
         function F(...xs) { this.n = xs.length; }
         function NT() {}
         NT.prototype = Object.create(Map.prototype);
         for (let i = 0; i < 10; i++) {
           const o = { ['k' + i]: i, [Symbol('s')]: i };
           Reflect.defineProperty(o, 'd' + i, { value: [i], enumerable: true });
           const f = Reflect.construct(F, Array.from({ length: i + 3 }, (_, j) => j), NT);
           const m = Reflect.construct(Map, [[[i, 'v' + i]]], NT);
           out.push(Reflect.ownKeys(o).length + ':' + Reflect.get(o, 'd' + i)[0] + ':' +
             Reflect.apply(Array.prototype.join, Reflect.ownKeys(o).slice(0, 2), ['+']) + ':' +
             f.n + ':' + Map.prototype.get.call(m, i) + ':' +
             Reflect.getOwnPropertyDescriptor(o, 'k' + i).value);
         }
         console.log(out.join(' '));",
        "3:0:k0+d0:3:v0:0 3:1:k1+d1:4:v1:1 3:2:k2+d2:5:v2:2 3:3:k3+d3:6:v3:3 \
         3:4:k4+d4:7:v4:4 3:5:k5+d5:8:v5:5 3:6:k6+d6:9:v6:6 3:7:k7+d7:10:v7:7 \
         3:8:k8+d8:11:v8:8 3:9:k9+d9:12:v9:9\n",
    );
}

// --- argument checks before GetPrototypeFromConstructor (F7) -----------------------------

/// `Promise` checks `IsCallable(executor)` before it reads
/// `newTarget.prototype`, and `Date` converts its arguments first; the
/// weak constructors check their argument first too.
#[test]
fn ctor_argument_checks_precede_prototype_read() {
    let prelude = "const log = [];
         const nt = function () {}.bind();
         Object.defineProperty(nt, 'prototype', { get() { log.push('proto'); throw new RangeError('p'); } });";
    let kind = |body: &str| {
        format!(
            "{prelude}
             let r;
             try {{ {body}; r = 'no throw'; }} catch (e) {{ r = e.constructor.name; }}
             [r, log.join(' ')].join('|')"
        )
    };
    assert_string(&kind("Reflect.construct(Promise, [], nt)"), "TypeError|");
    assert_string(&kind("Reflect.construct(Promise, [1], nt)"), "TypeError|");
    assert_string(
        &kind("Reflect.construct(Promise, [() => {}], nt)"),
        "RangeError|proto",
    );
    assert_string(&kind("Reflect.construct(WeakRef, [1], nt)"), "TypeError|");
    assert_string(
        &kind("Reflect.construct(FinalizationRegistry, [1], nt)"),
        "TypeError|",
    );
    assert_string(
        &kind("Reflect.construct(Date, [{ valueOf() { log.push('value'); return 0; } }], nt)"),
        "RangeError|value proto",
    );
    assert_string(
        &kind(
            "Reflect.construct(Date, [2020, { valueOf() { log.push('month'); return 1; } }], nt)",
        ),
        "RangeError|month proto",
    );
    assert_string(
        "class D extends Date {}
         const d = new D(0);
         [d instanceof D, d.getTime(), Object.getPrototypeOf(d) === D.prototype].join()",
        "true,0,true",
    );
}

/// The `Date` subclass construct keeps its proto across a collection at
/// every allocation.
#[test]
fn date_construct_order_survives_gc_stress() {
    assert_same_under_gc_stress(
        "class D extends Date {}
         const out = [];
         for (let i = 0; i < 10; i++) {
           const d = Reflect.construct(Date, [{ valueOf() { return [i].map((x) => x * 1000)[0]; } }], D);
           out.push(d instanceof D, d.getTime());
         }
         console.log(out.join());",
        "true,0,true,1000,true,2000,true,3000,true,4000,true,5000,true,6000,true,7000,true,8000,true,9000\n",
    );
}
