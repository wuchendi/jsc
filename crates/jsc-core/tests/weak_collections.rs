//! Integration tests: `WeakMap` and `WeakSet` (design/runtime-memory.md §6.1,
//! §6.2): the prototype methods, `CanBeHeldWeakly`, the constructors and
//! their iterable argument, and the collector's ephemeron semantics — a
//! value lives while its key does, and the entry of an unreachable key is
//! reclaimed. `WeakRef` and `FinalizationRegistry` (§6.3, §6.4): the API
//! surface and its `TypeError`s, `KeepDuringJob` until the microtask
//! checkpoint, and cleanup callbacks run from the `__drain()` checkpoint.

#![allow(clippy::expect_used)]

mod common;
use common::{
    assert_bool, assert_number, assert_output, assert_string, assert_uncaught, exec_gc_stress,
};

/// Assert `src` prints `expected` and exits 0 when compiled with
/// `gc_stress = 1` (a collection at every allocation).
fn stressed(src: &str, expected: &str) {
    let done = exec_gc_stress(src, false).expect("runs under gc_stress");
    assert_eq!(done.code, 0, "`{src}` failed: {}", done.stderr);
    assert_eq!(done.stdout, expected, "`{src}` under gc_stress");
}

/// `set` returns the map, `get` / `has` / `delete` find keys by identity
/// (objects, functions, arrays), `delete` reports whether it removed an
/// entry, a re-`set` overwrites, the store grows past its first four pairs,
/// and a key that cannot be held weakly is simply absent for `get` / `has` /
/// `delete`.
#[test]
fn weakmap_basic_api() {
    assert_string(
        "let k1 = {}, k2 = function () {}, k3 = [];
         let wm = new WeakMap();
         let same = wm.set(k1, 'a') === wm;
         wm.set(k2, 'b').set(k3, 'c').set(k1, 'A');
         [same, wm.get(k1), wm.get(k2), wm.get(k3), wm.get({}), wm.has(k2), wm.delete(k2),
          wm.has(k2), wm.delete(k2), wm.get(k2), wm.has({})].join()",
        "true,A,b,c,,true,true,false,false,,false",
    );
    assert_number(
        "let wm = new WeakMap(); let keys = [];
         for (let i = 0; i < 100; i++) { let k = { i }; keys.push(k); wm.set(k, i * 2); }
         for (let i = 0; i < 100; i += 2) wm.delete(keys[i]);
         for (let i = 0; i < 10; i++) wm.set(keys[i], -1);
         let sum = 0;
         for (let i = 0; i < 100; i++) if (wm.has(keys[i])) sum += wm.get(keys[i]);
         sum",
        // Odd i in 10..100 keep 2i; every i < 10 is -1.
        (11..100)
            .step_by(2)
            .map(|i| f64::from(i) * 2.0)
            .sum::<f64>()
            - 10.0,
    );
    assert_string(
        "let wm = new WeakMap();
         [wm.get(1), wm.get('k'), wm.has(null), wm.has(undefined), wm.delete(true),
          wm.delete(2)].join()",
        ",,false,false,false,false",
    );
    assert_string(
        "let wm = new WeakMap(); let r = [];
         for (const k of [1, 'k', null, undefined, true]) {
           try { wm.set(k, 1); r.push('no'); }
           catch (e) { r.push(e instanceof TypeError && e.message === 'Invalid value used as weak map key'); }
         }
         r.join()",
        "true,true,true,true,true",
    );
    assert_string(
        "[typeof new WeakMap(), Object.prototype.toString.call(new WeakMap()),
          WeakMap.length, WeakMap.name, WeakMap.prototype.set.length, WeakMap.prototype.get.length,
          WeakMap.prototype.constructor === WeakMap,
          Object.getPrototypeOf(new WeakMap()) === WeakMap.prototype,
          WeakMap.prototype[Symbol.toStringTag], new WeakMap() instanceof WeakMap].join()",
        "object,[object WeakMap],0,WeakMap,2,1,true,true,WeakMap,true",
    );
    assert_output(
        "console.log(new WeakMap(), new WeakSet())",
        "WeakMap { <items unknown> } WeakSet { <items unknown> }\n",
    );
}

/// `CanBeHeldWeakly` for symbols: a fresh symbol and a well-known symbol are
/// valid keys; a `Symbol.for` (registered) symbol is a `TypeError` for
/// `set` / `add` and absent for the queries.
#[test]
fn weakmap_symbol_keys() {
    assert_string(
        "let wm = new WeakMap(); let s = Symbol('d');
         wm.set(s, 1).set(Symbol.iterator, 2);
         let r = [wm.get(s), wm.get(Symbol.iterator), wm.has(Symbol('d'))];
         try { wm.set(Symbol.for('r'), 3); r.push('no'); }
         catch (e) { r.push(e instanceof TypeError, e.message); }
         r.push(wm.has(Symbol.for('r')), wm.get(Symbol.for('r')), wm.delete(Symbol.for('r')));
         r.push(wm.delete(s), wm.has(s));
         r.join()",
        "1,2,false,true,Invalid value used as weak map key,false,,false,true,false",
    );
    assert_string(
        "let ws = new WeakSet(); let s = Symbol();
         ws.add(s).add(Symbol.asyncIterator);
         let r = [ws.has(s), ws.has(Symbol.asyncIterator)];
         try { ws.add(Symbol.for('r')); r.push('no'); }
         catch (e) { r.push(e instanceof TypeError, e.message); }
         r.push(ws.has(Symbol.for('r')), ws.delete(Symbol.for('r')));
         r.join()",
        "true,true,true,Invalid value used in weak set,false,false",
    );
}

/// The `WeakSet` methods: `add` returns the set and is idempotent, `has` /
/// `delete` by identity, primitives absent, and `add` of a primitive a
/// `TypeError`.
#[test]
fn weakset_basic_api() {
    assert_string(
        "let a = {}, b = [], c = () => 1;
         let ws = new WeakSet();
         let same = ws.add(a) === ws;
         ws.add(b).add(a).add(c);
         [same, ws.has(a), ws.has(b), ws.has(c), ws.has({}), ws.delete(b), ws.has(b),
          ws.delete(b), ws.has(1), ws.delete('x')].join()",
        "true,true,true,true,false,true,false,false,false,false",
    );
    assert_number(
        "let ws = new WeakSet(); let keys = [];
         for (let i = 0; i < 50; i++) { let k = {}; keys.push(k); ws.add(k); }
         for (let i = 0; i < 50; i += 5) ws.delete(keys[i]);
         let n = 0; for (const k of keys) if (ws.has(k)) n++;
         n",
        40.0,
    );
    assert_string(
        "let r = [];
         try { new WeakSet().add(1); r.push('no'); }
         catch (e) { r.push(e instanceof TypeError, e.message); }
         r.push(typeof new WeakSet(), Object.prototype.toString.call(new WeakSet()),
                WeakSet.length, WeakSet.prototype.add.length, WeakSet.prototype[Symbol.toStringTag]);
         r.join()",
        "true,Invalid value used in weak set,object,[object WeakSet],0,1,WeakSet",
    );
}

/// Calling either constructor without `new` is a `TypeError`.
#[test]
fn weak_constructor_without_new_throws() {
    assert_uncaught("WeakMap()", "TypeError: Constructor WeakMap requires 'new'");
    assert_uncaught(
        "WeakSet([])",
        "TypeError: Constructor WeakSet requires 'new'",
    );
}

/// The iterable argument: entries go through the new collection's own
/// `set` / `add` (a user override on the prototype runs, `this` the new
/// collection); a non-callable adder, a non-object `WeakMap` entry and an
/// invalid key throw; an abrupt adder closes the iterator; `null` and
/// `undefined` mean no entries; subclasses construct through `new.target`.
#[test]
fn weak_constructor_iterable_uses_adder() {
    assert_string(
        "let k1 = {}, k2 = {};
         let wm = new WeakMap([[k1, 'a'], [k2, 'b']]);
         let ws = new WeakSet([k1, k2, k1]);
         [wm.get(k1), wm.get(k2), ws.has(k1), ws.has(k2),
          new WeakMap(null).has(k1), new WeakSet(undefined).has(k1)].join()",
        "a,b,true,true,false,false",
    );
    assert_string(
        "let seen = [];
         let set = WeakMap.prototype.set;
         WeakMap.prototype.set = function (k, v) { seen.push(v); return set.call(this, k, v); };
         let k = {};
         let wm = new WeakMap([[k, 1], [{}, 2]]);
         WeakMap.prototype.set = set;
         let add = WeakSet.prototype.add;
         let self;
         WeakSet.prototype.add = function (v) { self = this; seen.push('add'); return add.call(this, v); };
         let ws = new WeakSet([k]);
         WeakSet.prototype.add = add;
         seen.push(wm.get(k), self === ws, ws.has(k));
         seen.join()",
        "1,2,add,1,true,true",
    );
    assert_string(
        "let r = [];
         let set = WeakMap.prototype.set;
         WeakMap.prototype.set = 0;
         try { new WeakMap([]); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         WeakMap.prototype.set = set;
         try { new WeakMap([1]); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         try { new WeakMap([[1, 1]]); r.push('no'); } catch (e) { r.push(e.message); }
         try { new WeakSet(['s']); r.push('no'); } catch (e) { r.push(e.message); }
         r.join()",
        "true,true,Invalid value used as weak map key,Invalid value used in weak set",
    );
    assert_bool(
        "let closed = false;
         let it = { [Symbol.iterator]() { return this; },
                    next() { return { done: false, value: 1 }; },
                    return() { closed = true; return {}; } };
         try { new WeakSet(it); } catch (e) {}
         closed",
        true,
    );
    assert_string(
        "class M extends WeakMap { tag() { return 'm'; } }
         class S extends WeakSet {}
         let k = {};
         let m = new M([[k, 7]]);
         let s = new S([k]);
         [m.tag(), m.get(k), m instanceof WeakMap, s.has(k), s instanceof S].join()",
        "m,7,true,true,true",
    );
}

/// Each method checks its receiver's kind: a Map, a `WeakSet` for a `WeakMap`
/// method (and the reverse) or a plain object is a `TypeError`.
#[test]
fn weak_methods_reject_wrong_receiver() {
    assert_string(
        "let r = [];
         for (const [f, recv] of [[WeakMap.prototype.get, new Map()], [WeakMap.prototype.set, new WeakSet()],
                                  [WeakMap.prototype.has, {}], [WeakMap.prototype.delete, undefined],
                                  [WeakSet.prototype.add, new WeakMap()], [WeakSet.prototype.has, new Set()],
                                  [WeakSet.prototype.delete, 1]]) {
           try { f.call(recv, {}); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         }
         r.join()",
        "true,true,true,true,true,true,true",
    );
    assert_uncaught(
        "WeakMap.prototype.get.call({}, {})",
        "TypeError: Method WeakMap.prototype.get called on incompatible receiver",
    );
}

/// Ephemeron reclamation observed through memory: each round sets a fresh,
/// immediately dropped key to a 1 MiB string value (which points back at
/// its key), about 1 GiB in total — twice the 512 MiB cap — so the program
/// completes without the out-of-memory `RangeError` only if the collector
/// drops the entries of unreachable keys even though their values reference
/// those keys. A `WeakSet` likewise does not keep its keys alive. The entry of
/// a key still reachable keeps its value.
#[test]
fn weakmap_entry_dropped_after_key_unreachable() {
    assert_number(
        "let wm = new WeakMap();
         let kept = {};
         wm.set(kept, 'v'.repeat(3));
         let total = 0;
         for (let i = 0; i < 1024; i++) {
           let k = {};
           let big = 'x'.repeat(1048576) + i;
           wm.set(k, { k: k, big: big });
           total = total + wm.get(k).big.length;
         }
         total + wm.get(kept).length",
        1024.0 * 1_048_576.0 + 10.0 + 90.0 * 2.0 + 900.0 * 3.0 + 24.0 * 4.0 + 3.0,
    );
    assert_number(
        "let ws = new WeakSet();
         let n = 0;
         for (let i = 0; i < 1024; i++) {
           let k = { big: 'y'.repeat(1048576) + i };
           ws.add(k);
           if (ws.has(k)) n++;
         }
         n",
        1024.0,
    );
}

/// Under a collection at every allocation, the entries of live keys keep
/// their values through churn that drops other keys, deletes and re-adds
/// entries, and grows the store.
#[test]
fn weakmap_live_keys_survive_gc_stress() {
    stressed(
        "let wm = new WeakMap(); let ws = new WeakSet(); let keys = [];
         for (let i = 0; i < 8; i++) { let k = { i }; keys.push(k); wm.set(k, 'v' + i); ws.add(k); }
         for (let i = 0; i < 40; i++) { wm.set({}, ['junk' + i]); ws.add({ j: i }); }
         wm.delete(keys[3]); ws.delete(keys[3]);
         wm.set(keys[3], 'again');
         for (let i = 0; i < 40; i++) wm.set({ i }, 'more' + i);
         let out = [];
         for (const k of keys) out.push(wm.get(k) + ':' + ws.has(k));
         console.log(out.join());",
        "v0:true,v1:true,v2:true,again:false,v4:true,v5:true,v6:true,v7:true\n",
    );
}

/// The ephemeron fixpoint under a collection at every allocation: a chain
/// `k1 -> v1 = { next: k2 } -> v2 = { next: k3 } ...` reachable only through
/// the map, stored back to front so each fixpoint pass uncovers one more
/// live key. While `k1` is live every value survives: walking the chain by
/// `get` reaches its end with every tag intact.
#[test]
fn ephemeron_chain_fixpoint() {
    stressed(
        "let wm = new WeakMap();
         let k1 = {};
         function build(k, i) {
           if (i === 20) return;
           let next = { i };
           build(next, i + 1);
           wm.set(k, { next, tag: 'v' + i });
         }
         build(k1, 0);
         for (let i = 0; i < 50; i++) { let junk = { s: 'j' + i }; wm.set(junk, [junk]); }
         let k = k1, n = 0;
         while (wm.has(k)) {
           let v = wm.get(k);
           if (v.tag !== 'v' + n) break;
           k = v.next;
           n++;
         }
         console.log(n);",
        "20\n",
    );
}

/// Assert `src`, compiled with `gc_stress = 1`, exits 1 after printing
/// `stdout`, reporting exactly `Uncaught {expected}` on stderr.
fn stressed_uncaught(src: &str, stdout: &str, expected: &str) {
    let done = exec_gc_stress(src, false).expect("runs under gc_stress");
    assert_eq!(
        (done.code, done.stdout.as_str(), done.stderr.as_str()),
        (1, stdout, format!("Uncaught {expected}\n").as_str()),
        "`{src}` under gc_stress"
    );
}

/// A `WeakRef` whose target only it references keeps the target for the
/// rest of the job that created it (`KeepDuringJob`), even though every
/// allocation collects; once the checkpoint of a top-level `await` cleared
/// the kept objects, the next collection clears the reference and `deref()`
/// returns `undefined`.
#[test]
fn weakref_cleared_after_checkpoint() {
    stressed(
        "let r;
         function make() { r = new WeakRef({ tag: 'x' }); }
         make();
         for (let i = 0; i < 20; i++) { let junk = [i, { i }]; }
         function tag() { let t = r.deref(); return t === undefined ? 'gone' : t.tag; }
         let before = tag();
         await 0;
         for (let i = 0; i < 20; i++) { let junk = [i, { i }]; }
         console.log(before, tag());",
        "x gone\n",
    );
}

/// Within one job the target survives collections at every allocation:
/// created, churned and dereferenced repeatedly, it is the same object; a
/// target that stays strongly reachable survives a checkpoint too.
#[test]
fn weakref_kept_during_job() {
    stressed(
        "let r;
         function make() { r = new WeakRef({ tag: 'kept' }); }
         make();
         let seen = [];
         for (let i = 0; i < 5; i++) {
           for (let j = 0; j < 10; j++) { let junk = { j, s: 'j' + j }; }
           seen.push(r.deref().tag);
         }
         let first = r.deref();
         for (let j = 0; j < 10; j++) { let junk = [j]; }
         console.log(seen.join(), first === r.deref());",
        "kept,kept,kept,kept,kept true\n",
    );
    stressed(
        "let held = { tag: 'strong' };
         let r = new WeakRef(held);
         await 0;
         for (let i = 0; i < 20; i++) { let junk = [i]; }
         console.log(r.deref() === held, r.deref().tag);",
        "true strong\n",
    );
}

/// The cleanup callback runs once the target is collected, with the held
/// value, only after `main` and every queued microtask finished.
#[test]
fn finreg_cleanup_runs_after_drain() {
    stressed(
        "const fr = new FinalizationRegistry(h => console.log('cleanup', h));
         (function () { fr.register({}, 'held'); })();
         for (let i = 0; i < 10; i++) { let junk = [i]; }
         Promise.resolve().then(() => console.log('micro'));
         console.log('main');",
        "main\nmicro\ncleanup held\n",
    );
    // A microtask the cleanup queues runs before the program ends; a symbol
    // target is collected like an object.
    stressed(
        "const fr = new FinalizationRegistry(h => {
           console.log('cleanup', h);
           Promise.resolve(h).then(v => console.log('after', v));
         });
         (function () { fr.register(Symbol('t'), 'sym'); })();
         for (let i = 0; i < 10; i++) { let junk = [i]; }
         console.log('main');",
        "main\ncleanup sym\nafter sym\n",
    );
}

/// `unregister(token)` removes every cell registered with the token,
/// collected or not, and reports whether it removed any; the cleanup then
/// runs for the remaining cell only. A target may be its own token.
#[test]
fn finreg_unregister_token() {
    stressed(
        "const fr = new FinalizationRegistry(h => console.log('cleanup', h));
         const token = {};
         (function () { fr.register({}, 'a', token); fr.register({}, 'b'); fr.register({}, 'c', token); })();
         for (let i = 0; i < 10; i++) { let junk = [i]; }
         console.log(fr.unregister(token), fr.unregister(token), fr.unregister({}));",
        "true false false\ncleanup b\n",
    );
    assert_string(
        "const fr = new FinalizationRegistry(() => {});
         const t = {}, s = Symbol('k');
         let r = [fr.register(t, 1, t), fr.register(s, 2, s)];
         r.push(fr.unregister(t), fr.unregister(s), fr.unregister(t), fr.unregister(Symbol.iterator));
         r.join()",
        ",,true,true,false,false",
    );
}

/// `register(target, held)` with `held` the target itself is a `TypeError`.
#[test]
fn finreg_target_equals_held_throws() {
    assert_string(
        "const fr = new FinalizationRegistry(() => {});
         const o = {}, s = Symbol();
         let r = [];
         for (const v of [o, s]) {
           try { fr.register(v, v); r.push('no'); }
           catch (e) { r.push(e instanceof TypeError, e.message); }
         }
         r.push(fr.register(o, {}, o) === undefined);
         r.join()",
        "true,FinalizationRegistry.register: target and holdings must not be same,\
         true,FinalizationRegistry.register: target and holdings must not be same,true",
    );
}

/// The API surface: prototypes, names, lengths, `@@toStringTag`, `deref`
/// returning the target, `register` returning `undefined`, the console
/// forms, and subclassing through `new.target`.
#[test]
fn weakref_finreg_api_surface() {
    assert_string(
        "let o = {};
         let r = new WeakRef(o);
         let fr = new FinalizationRegistry(() => {});
         [typeof r, typeof fr, r.deref() === o, Object.prototype.toString.call(r),
          Object.prototype.toString.call(fr), WeakRef.length, WeakRef.name,
          FinalizationRegistry.length, FinalizationRegistry.name, WeakRef.prototype.deref.length,
          FinalizationRegistry.prototype.register.length,
          FinalizationRegistry.prototype.unregister.length,
          Object.getPrototypeOf(r) === WeakRef.prototype,
          Object.getPrototypeOf(fr) === FinalizationRegistry.prototype,
          WeakRef.prototype.constructor === WeakRef,
          FinalizationRegistry.prototype.constructor === FinalizationRegistry,
          r instanceof WeakRef, fr instanceof FinalizationRegistry,
          fr.register(o, 'h') === undefined, Object.getOwnPropertyNames(r).length,
          'cleanupSome' in fr].join()",
        "object,object,true,[object WeakRef],[object FinalizationRegistry],1,WeakRef,1,\
         FinalizationRegistry,0,2,1,true,true,true,true,true,true,true,0,false",
    );
    assert_output(
        "console.log(new WeakRef({}), new FinalizationRegistry(() => {}))",
        "WeakRef {} FinalizationRegistry {}\n",
    );
    assert_string(
        "class R extends WeakRef { tag() { return 'r'; } }
         class F extends FinalizationRegistry {}
         let o = {};
         let r = new R(o), f = new F(() => {});
         [r.tag(), r.deref() === o, r instanceof WeakRef, f instanceof F,
          f.register(o, 1) === undefined].join()",
        "r,true,true,true,true",
    );
}

/// Calling either constructor without `new` is a `TypeError`, checked
/// before the argument.
#[test]
fn weakref_finreg_constructor_without_new_throws() {
    assert_uncaught(
        "WeakRef({})",
        "TypeError: Constructor WeakRef requires 'new'",
    );
    assert_uncaught(
        "WeakRef(1)",
        "TypeError: Constructor WeakRef requires 'new'",
    );
    assert_uncaught(
        "FinalizationRegistry(() => {})",
        "TypeError: Constructor FinalizationRegistry requires 'new'",
    );
}

/// The argument `TypeError`s: a `WeakRef` target, a registry target or a
/// token that cannot be held weakly (primitives, registered symbols), a
/// non-callable cleanup, and the receiver checks; from a derived constructor
/// the argument check throws out of `super(...)` before `this` is bound.
#[test]
fn weakref_finreg_invalid_arguments_throw() {
    assert_string(
        "let r = [];
         for (const t of [1, 'k', null, undefined, true, Symbol.for('r')]) {
           try { new WeakRef(t); r.push('no'); }
           catch (e) { r.push(e instanceof TypeError && e.message === 'WeakRef: target must be an object'); }
         }
         r.join()",
        "true,true,true,true,true,true",
    );
    assert_uncaught(
        "new FinalizationRegistry(1)",
        "TypeError: FinalizationRegistry: cleanup must be callable",
    );
    assert_string(
        "const fr = new FinalizationRegistry(() => {});
         let r = [];
         for (const [f, args] of [[fr.register, [1, 'h']], [fr.register, [Symbol.for('x'), 'h']],
                                  [fr.register, [{}, 'h', 1]], [fr.register, [{}, 'h', Symbol.for('t')]],
                                  [fr.unregister, [1]], [fr.unregister, [undefined]],
                                  [fr.unregister, [Symbol.for('t')]]]) {
           try { f.apply(fr, args); r.push('no'); } catch (e) { r.push(e.message); }
         }
         r.join('|')",
        "FinalizationRegistry.register: invalid target|FinalizationRegistry.register: invalid target|\
         FinalizationRegistry.register: invalid unregister token|\
         FinalizationRegistry.register: invalid unregister token|\
         FinalizationRegistry.unregister: invalid unregister token|\
         FinalizationRegistry.unregister: invalid unregister token|\
         FinalizationRegistry.unregister: invalid unregister token",
    );
    assert_string(
        "let r = [];
         for (const [f, recv] of [[WeakRef.prototype.deref, {}], [WeakRef.prototype.deref, new WeakMap()],
                                  [FinalizationRegistry.prototype.register, new WeakRef({})],
                                  [FinalizationRegistry.prototype.unregister, undefined]]) {
           try { f.call(recv, {}); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         }
         r.join()",
        "true,true,true,true",
    );
    assert_uncaught(
        "WeakRef.prototype.deref.call({})",
        "TypeError: Method WeakRef.prototype.deref called on incompatible receiver",
    );
    assert_string(
        "let log = [];
         class R extends WeakRef { constructor(t) { log.push('r'); super(t); log.push('r-done'); } }
         class F extends FinalizationRegistry { constructor(c) { log.push('f'); super(c); log.push('f-done'); } }
         let r = [];
         try { new R(1); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         try { new F(1); r.push('no'); } catch (e) { r.push(e instanceof TypeError); }
         r.push(new R({}) instanceof R, log.join(' '));
         r.join()",
        "true,true,true,r f r r-done",
    );
}

/// Symbols that are not in the `Symbol.for` registry are valid `WeakRef`
/// targets and registry targets and tokens; well-known symbols too.
#[test]
fn weakref_symbol_targets() {
    assert_string(
        "let s = Symbol('d');
         let r = [new WeakRef(s).deref() === s, new WeakRef(Symbol.iterator).deref() === Symbol.iterator];
         const fr = new FinalizationRegistry(() => {});
         r.push(fr.register(s, 'h', Symbol.asyncIterator) === undefined, fr.unregister(Symbol.asyncIterator));
         r.join()",
        "true,true,true,true",
    );
}

/// A throw out of a cleanup callback is reported like an uncaught exception
/// (`Uncaught ...` on stderr, exit 1) after `main` finished.
#[test]
fn finreg_cleanup_throw_is_uncaught() {
    stressed_uncaught(
        "const fr = new FinalizationRegistry(h => { throw new Error('cleanup ' + h); });
         (function () { fr.register({}, 'x'); })();
         for (let i = 0; i < 10; i++) { let junk = [i]; }
         console.log('end');",
        "end\n",
        "Error: cleanup x",
    );
}
