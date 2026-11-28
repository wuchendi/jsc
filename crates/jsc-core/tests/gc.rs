//! Integration tests: the page allocator and the collector
//! (design/runtime-memory.md §5.4-§5.7). Collection must never change what a
//! program computes.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_bool, assert_number, assert_same_under_gc_stress as same_under_stress};
use jsc_core::emit::CompileOptions;

/// Fresh blocks reusing freed slots of the same class read as new: a reused
/// object slot has no stale `props` (no keys), a reused array handle no stale
/// store, a reused Map no stale entries. The churn exceeds the 4 MiB budget
/// many times, so with collection enabled the later blocks come from the free
/// lists.
#[test]
fn gc_free_list_reuse_zeroes() {
    assert_bool(
        "for (let i = 0; i < 200000; i++) {
           let o = {}; o.x = i; o.y = 'v' + i;
           let a = []; a.push(i, i, i);
           let m = new Map(); m.set(i, o);
         }
         let ok = true;
         for (let i = 0; i < 20000; i++) {
           let o = {};
           if (Object.keys(o).length !== 0 || o.x !== undefined) ok = false;
           let a = [];
           if (a.length !== 0 || a[0] !== undefined) ok = false;
           let m = new Map();
           if (m.size !== 0 || m.has(i)) ok = false;
         }
         ok",
        true,
    );
}

/// About 2 GiB allocated in 1 MiB strings (large spans) and small objects,
/// with a small live set, runs to completion under the 512 MiB cap: the
/// collector reclaims both.
#[test]
fn gc_reclaims_churn() {
    let rounds = 2048.0;
    let len = 1_048_576.0;
    assert_number(
        "let keep = [];
         for (let i = 0; i < 64; i++) keep.push({ i: i, s: 'k' + i });
         let total = 0;
         for (let i = 0; i < 2048; i++) {
           let big = 'x'.repeat(1048576);
           let small = [];
           for (let j = 0; j < 64; j++) small.push({ j: j });
           total = total + big.length + small.length;
         }
         total + keep[63].i + keep[63].s.length",
        rounds * (len + 64.0) + 63.0 + 3.0,
    );
}

/// Values waiting on the operand stack (call arguments, binary operands)
/// survive the allocations of the operands evaluated after them.
#[test]
fn gc_keeps_operand_stack_temps() {
    same_under_stress(
        "function f(a, b, c) { return a + b + c; }
         console.log(f('x' + 1, ['y' + 2].join(), String({ toString() { return 'z' + 3; } })));
         console.log(('a' + 1) + ('b' + 2) + [1, 2].concat([3]).length);",
        "x1y2z3\na1b23\n",
    );
}

/// An object or array literal under construction keeps its block and the
/// elements already stored while later elements allocate.
#[test]
fn gc_keeps_partially_built_literals() {
    same_under_stress(
        "let o = { a: 'p' + 1, b: ['q' + 2, { c: 'r' + 3 }], d: 's' + 4 };
         let a = ['t' + 5, { e: 'u' + 6 }, 'v' + 7];
         console.log(o.a, o.b[0], o.b[1].c, o.d, a[0], a[1].e, a[2]);",
        "p1 q2 r3 s4 t5 u6 v7\n",
    );
}

/// A suspended generator's frame (its `env` cells) survives collections
/// between resumptions.
#[test]
fn gc_generator_frame_survives() {
    same_under_stress(
        "function* g(n) {
           let acc = [];
           for (let i = 0; i < n; i++) { let s = 'k' + i; acc.push(s); yield s; }
           return acc.join(',');
         }
         const it = g(4);
         let out = [];
         let r;
         while (!(r = it.next()).done) out.push(r.value + '!');
         console.log(out.join(' '), r.value);",
        "k0! k1! k2! k3! k0,k1,k2,k3\n",
    );
}

/// Requests queued on an async generator (raw `queue` / `queue_tail` refs
/// to request records) survive until served.
#[test]
fn gc_async_gen_queue_survives() {
    same_under_stress(
        "async function* ag() { for (let i = 0; i < 3; i++) { yield 'v' + i; } }
         const it = ag();
         const ps = [it.next(), it.next(), it.next(), it.next()];
         Promise.all(ps).then(rs => console.log(rs.map(r => r.done ? 'done' : r.value).join(' ')));",
        "v0 v1 v2 done\n",
    );
}

/// Reaction records hanging off promises and queued jobs (raw refs from the
/// promise and the job queue) survive until they run.
#[test]
fn gc_promise_reactions_survive() {
    same_under_stress(
        "let log = [];
         let p = new Promise(r => r('a' + 1));
         p.then(v => { log.push(v + 'b'); return { s: 'c' + 2 }; })
           .then(o => { log.push(o.s); console.log(log.join(' ')); });
         p.then(v => log.push(v + 'd'));",
        "a1b a1d c2\n",
    );
}

/// Both resolving functions of `new Promise` survive the allocations between
/// their creation and the executor call (the executor closure itself), and
/// stay callable after the executor returned.
#[test]
fn gc_promise_executor_settlers_survive() {
    same_under_stress(
        "let res, rej;
         const p = new Promise((a, b) => { res = a; rej = b; });
         const q = new Promise((a, b) => { const junk = [{ k: 'x' + 1 }]; b('no' + junk.length); });
         let junk = [];
         for (let i = 0; i < 20; i++) junk.push({ i: 'j' + i });
         p.then(v => console.log('p', v));
         q.catch(e => console.log('q', e));
         res('ok' + 1);
         rej('late');",
        "q no1\np ok1\n",
    );
}

/// Heap values stored on static blocks (a realm intrinsic's static table
/// written in place or rebuilt on the heap, a materialized builtin method, a
/// top-level declaration, the `Symbol.for` registry) are reached through the
/// static root list.
#[test]
fn gc_static_proto_heap_props_survive() {
    same_under_stress(
        "Array.prototype.extra = { tag: 'x' + 1 };
         Object.prototype.toString = function () { return 'T' + 2; };
         Math.max.label = 'm' + 3;
         function decl() {}
         decl.note = 'd' + 4;
         const sym = Symbol.for('k' + 5);
         let junk = [];
         for (let i = 0; i < 50; i++) junk.push({ i: 'j' + i });
         console.log([].extra.tag, String({}), Math.max.label, decl.note, Symbol.for('k5') === sym);",
        "x1 T2 m3 d4 true\n",
    );
}

/// Interned heap keys nobody references become intern-table tombstones:
/// lookups and insertions probe past them, growth drops them, and a key
/// interned again is found again.
#[test]
fn gc_intern_tombstones_probe() {
    same_under_stress(
        "for (let i = 0; i < 200; i++) { let t = {}; t['key' + i] = i; }
         let o = {};
         for (let i = 0; i < 200; i += 7) o['key' + i] = i;
         let sum = 0;
         for (let i = 0; i < 200; i += 7) sum += o['key' + i];
         console.log(sum, Object.keys(o).length, o.key7, o['key' + 14], 'key0' in o, 'key1' in o);",
        "2842 29 7 14 true false\n",
    );
}

/// A live Map / Set iterator keeps its collection and backing store
/// reachable across collections, including deletions mid-iteration.
#[test]
fn gc_map_iterator_survives() {
    same_under_stress(
        "const m = new Map();
         for (let i = 0; i < 5; i++) m.set('k' + i, { v: 'v' + i });
         const it = m.entries();
         let out = [];
         for (const [k, o] of it) { out.push(k + '=' + o.v); m.delete('k' + 3); }
         const s = new Set(['a' + 1, 'b' + 2]);
         const si = s.values();
         si.next();
         console.log(out.join(','), si.next().value, [...m.keys()].join(''));",
        "k0=v0,k1=v1,k2=v2,k4=v4 b2 k0k1k2k4\n",
    );
}

/// design/runtime-memory.md §7.1 `gc_stress_run_suite`: the whole run-test
/// suite under `gc_stress` is the CI `gc-stress` job, where `JSC_GC_STRESS=N`
/// makes the shared helpers (`common::compile` / `common::exec`, and the
/// in-crate ones) build every program with the collection interval `N`.
/// This anchor checks that plumbing: the stressed module differs from the
/// plain one, the helpers pick the one the environment asks for, and both
/// compute the same.
#[test]
fn gc_stress_run_suite() {
    let src = "let a = []; for (let i = 0; i < 100; i++) a.push({ s: 'v' + i }); \
               console.log(a.length, a[99].s);";
    let options = |gc_stress| CompileOptions {
        print_completion: false,
        gc_stress,
        no_stack_guard: false,
        full_runtime: false,
    };
    let plain = common::compile_with(src, options(0)).expect("compiles");
    let stressed = common::compile_with(src, options(1)).expect("compiles");
    assert_ne!(plain, stressed, "gc_stress changes the module");
    let expected = common::compile_with(src, options(common::gc_stress_from_env()));
    assert_eq!(
        common::compile(src, false).expect("compiles"),
        expected.expect("compiles")
    );
    same_under_stress(src, "100 v99\n");
}

/// Out of memory is a catchable `RangeError` (§5.8): the same static
/// `%OutOfMemoryError%` every time, its message and stack own and
/// non-enumerable, and the program goes on (the reserve is taken back by the
/// next collection) to exit normally.
#[test]
fn gc_oom_is_catchable_range_error() {
    let done = common::exec(
        "function exhaust() {
           let keep = [];
           for (;;) keep.push('x'.repeat(1048576) + keep.length);
         }
         let first, second;
         try { exhaust(); } catch (e) { first = e; }
         try { exhaust(); } catch (e) { second = e; }
         console.log(first instanceof RangeError, first.name, first.message);
         console.log(first === second, Object.keys(first).length, first.stack, String(second));
         let after = [];
         for (let i = 0; i < 1000; i++) after.push({ i: 'a' + i });
         console.log('after', after.length, after[999].i);",
        false,
    )
    .expect("runs");
    assert_eq!(done.stderr, "");
    assert_eq!(done.code, 0);
    assert_eq!(
        done.stdout,
        "true RangeError out of memory\n\
         true 0 RangeError: out of memory RangeError: out of memory\n\
         after 1000 a999\n"
    );
}

/// The M1 spill elisions (design/runtime-memory.md §5.3, numeric operator
/// scratch): every operand is an object whose `valueOf` allocates, so the
/// operands still waiting (on the operand stack or in raw scratch) live
/// across collections under `gc_stress`.
const ELISION_PRELUDE: &str = "function churn() {
       let t = [];
       for (let i = 0; i < 8; i++) t.push({ i: 'g' + i });
       return t.length;
     }
     function num(n) { return { n: n, tag: 'n' + n, valueOf() { churn(); return this.n; } }; }
";

/// [`same_under_stress`] of `body` after [`ELISION_PRELUDE`].
fn elision(body: &str, expected: &str) {
    same_under_stress(&format!("{ELISION_PRELUDE}{body}"), expected);
}

#[test]
fn gc_stress_elision_emit_cond() {
    elision(
        "const a = num(6), b = num(3);
         let out = [];
         if (a > b) out.push('gt');
         if (b < a && a >= b) out.push('and');
         let i = 0;
         while (num(i) < b) i++;
         out.push(a <= b ? 'le' : 'nle', i);
         console.log(out.join(' '));",
        "gt and nle 3\n",
    );
}

#[test]
fn gc_stress_elision_emit_mod() {
    elision(
        "console.log(num(7) % num(3), num(-7) % num(2), num(5.5) % num(2));",
        "1 -1 1.5\n",
    );
}

#[test]
fn gc_stress_elision_emit_arith() {
    elision(
        "console.log(num(6) * num(3), num(6) - num(3), num(6) / num(3), num(1) / num(4));",
        "18 3 2 0.25\n",
    );
}

#[test]
fn gc_stress_elision_emit_bitwise() {
    elision(
        "console.log(num(6) | num(3), num(6) & num(3), num(6) ^ num(3), num(6) << num(3),
                     num(-16) >> num(2), num(-16) >>> num(28));",
        "7 2 5 48 -4 15\n",
    );
}

#[test]
fn gc_stress_elision_emit_add() {
    elision(
        "console.log(num(6) + num(3), 's' + num(3), num(2) + 'x' + num(1), num(1) + num(2) + num(3));",
        "9 s3 2x1 6\n",
    );
}

#[test]
fn gc_stress_elision_relational() {
    elision(
        "console.log(num(6) < num(3), num(6) > num(3), num(3) <= num(3), num(2) >= num(3),
                     'b' < num(3));",
        "false true true false false\n",
    );
}

#[test]
fn gc_stress_elision_math_atan2() {
    elision(
        "console.log(Math.atan2(num(1), num(1)) * 4, Math.atan2(num(0), num(-1)));",
        "3.141592653589793 3.141592653589793\n",
    );
}

#[test]
fn gc_stress_elision_math_imul() {
    elision(
        "console.log(Math.imul(num(6), num(3)), Math.imul(num(-5), num(12)),
                     Math.imul(num(0xffffffff), num(5)));",
        "18 -60 -5\n",
    );
}

#[test]
fn gc_stress_elision_math_unary() {
    elision(
        "console.log([Math.sqrt(num(16)), Math.floor(num(2.5)), Math.abs(num(-3)),
                      Math.round(num(2.5)), Math.trunc(num(-1.5))].join(' '));",
        "4 2 3 3 -1\n",
    );
}

#[test]
fn gc_stress_elision_math_min_max() {
    elision(
        "console.log(Math.max(num(1), num(5), num(3)), Math.min(num(4), num(2), num(9)),
                     Math.max(num(1), Math.min(num(7), num(3)), num(2)));",
        "5 2 3\n",
    );
}

#[test]
fn gc_stress_elision_math_spread_fold() {
    elision(
        "const xs = [num(4), num(9), num(2)];
         console.log(Math.max(...xs), Math.min(...xs), Math.max(...[num(1)], ...xs));",
        "9 2 9\n",
    );
}

#[test]
fn gc_stress_elision_emit_pow() {
    elision(
        "console.log(num(2) ** num(10), num(3) ** num(2) ** num(2), num(4) ** num(0.5));",
        "1024 81 2\n",
    );
}

/// A spread array literal's fresh handle rides the operand stack while each
/// part runs (a generator drain, an array, a string, a pushed item): it is
/// rooted for the whole build, so no part's allocation frees it.
#[test]
fn gc_spread_array_literal_base_survives() {
    same_under_stress(
        "function* g() { yield 'a' + 1; yield 'b' + 2; }
         const a = [0, ...g(), ...[3, 'c' + 4], 'd' + 5, ...'xy'];
         console.log(JSON.stringify(a), [...g()].join());
         function add(x, y) { return x + y; }
         console.log(add(...g()));",
        "[0,\"a1\",\"b2\",3,\"c4\",\"d5\",\"x\",\"y\"] a1,b2\na1b2\n",
    );
}

/// An object-literal accessor block is built while its getter and setter
/// closures allocate: its base is rooted across both builds.
#[test]
fn gc_accessor_literal_survives() {
    same_under_stress(
        "const o = { v: 'a', get x() { return this.v + '!' }, set x(n) { this.v = n + '?' } };
         o.x = 'b' + 1;
         console.log(o.x, o.v);",
        "b1?! b1?\n",
    );
}

/// A spread call's callee is held on the operand stack while the argument
/// array is built; an unnamed closure is reachable only from there.
#[test]
fn gc_spread_call_callee_survives() {
    same_under_stress(
        "console.log(((a, b) => a + b)(...['p' + 1, 'q' + 2]));",
        "p1q2\n",
    );
}

/// A `finally` sets the in-flight exception aside on the operand stack; a
/// throw caught inside the body replaces `$exc_value`, so the stash alone
/// keeps the original error alive until it is restored.
#[test]
fn gc_finally_stashed_exception_survives() {
    same_under_stress(
        "function f() {
           try { throw new Error('e' + 1); }
           finally { try { throw 'x' + 2; } catch (_) {} const junk = []; for (let i = 0; i < 20; i++) junk.push({ i: 'j' + i }); }
         }
         try { f(); } catch (e) { console.log(e.message); }",
        "e1\n",
    );
}

/// `JSON.stringify(x)` holds the freshly computed value on the operand stack
/// while it allocates the cycle stack: a value reachable only from there
/// survives (no "circular structure" from a reused block, no `{}` from a
/// freed one).
#[test]
fn gc_json_stringify_operand_survives() {
    same_under_stress(
        "function mk() { return [7, 'x' + 1, { k: 'v' + 2 }]; }
         console.log(JSON.stringify(mk()), JSON.stringify(Array.of(1, 'y' + 3)));",
        "[7,\"x1\",{\"k\":\"v2\"}] [1,\"y3\"]\n",
    );
}

/// `k in obj` converts a computed key to an interned index string, which the
/// weak intern table alone does not keep alive while `obj` evaluates.
#[test]
fn gc_in_computed_key_survives() {
    same_under_stress(
        "let n = 3; console.log(n in new String('a\u{e9}\u{1F600}'), (n - 1) in ['p' + 1, 'q' + 2, 'r' + 3]);",
        "true true\n",
    );
}

/// `Math.sumPrecise` keeps its exact accumulator (a `TYPE_RAW` block) alive
/// across every iterator step.
#[test]
fn gc_sum_precise_accumulator_survives() {
    same_under_stress(
        "function* g() { yield -0.1; yield 'x' + 1 ? -0.2 : 0; yield 1e300; yield -1e300; }
         console.log(Math.sumPrecise(g()), Math.sumPrecise([0.1, 0.2, 0.3]));",
        "-0.30000000000000004 0.6\n",
    );
}

/// The first `Symbol.for` registration grows the empty static registry table
/// onto the heap with room for the entry.
#[test]
fn gc_symbol_registry_first_entry_survives() {
    same_under_stress(
        "const a = Symbol.for('r' + 1);
         let junk = []; for (let i = 0; i < 20; i++) junk.push({ i: 'j' + i });
         const b = Symbol.for('s' + 2);
         console.log(Symbol.for('r1') === a, Symbol.for('s2') === b, Symbol.keyFor(a));",
        "true true r1\n",
    );
}

/// A spread `new F(...a)` / `super(...a)` with more arguments than `K` holds
/// its argument array across `Get(new.target, "prototype")` (a lazy function's
/// first `prototype` read materializes it) and the `this` allocation, and
/// publishes it only after both: every argument, heap strings all, reaches
/// the callee past `K`.
#[test]
fn gc_spread_construct_args_survive() {
    same_under_stress(
        "function churn() { let t = []; for (let i = 0; i < 8; i++) t.push({ i: 'g' + i }); return t.length; }
         function F(...r) { churn(); this.v = r.join(','); }
         function P(...r) { this.w = r.join('|'); }
         class D extends P { constructor(...a) { churn(); super(...a); } }
         const a = []; for (let i = 0; i < 10; i++) a.push('a' + i);
         const x = new F(...a), y = new D(...a);
         console.log(x.v, x instanceof F, y.w, y instanceof D);",
        "a0,a1,a2,a3,a4,a5,a6,a7,a8,a9 true a0|a1|a2|a3|a4|a5|a6|a7|a8|a9 true\n",
    );
}

/// Run a `K = 0` program (no call passes an argument, so its report rides an
/// uncaught throw) plain and under `gc_stress`, asserting the same report.
fn assert_uncaught_same_under_stress(src: &str, expected: &str) {
    let expected = format!("Uncaught {expected}\n");
    let plain = common::exec(src, false).expect("program should compile and not trap");
    assert_eq!(
        (plain.code, plain.stderr.as_str()),
        (1, expected.as_str()),
        "`{src}`"
    );
    let stressed = common::exec_gc_stress(src, false).expect("runs under gc_stress");
    assert_eq!(
        (stressed.code, stressed.stderr.as_str()),
        (1, expected.as_str()),
        "`{src}` under gc_stress"
    );
}

/// With no positional argument slot (`K = 0`) a user `@@toPrimitive` still
/// receives its hint: it travels in a one-element spread argument array.
#[test]
fn accessor_call_without_argument_slots_passes_the_hint() {
    assert_uncaught_same_under_stress(
        "const o = { [Symbol.toPrimitive]() { return arguments.length + ':' + arguments[0]; } };
         throw `${o}` + ' ' + (o + '');",
        "1:string 1:default",
    );
}

/// Arguments past the positional slots (`K <= 1`), snapshotted before a
/// coercion makes a nested spread call, survive a collection at every
/// allocation.
#[test]
fn arguments_past_the_slots_under_gc_stress() {
    same_under_stress(
        "const s = { valueOf() { Math.max(...[1, 2, 3]); return 1; } };
         console.log(new Date(...[2020, s, 2, 3, 4, 5, 6]).toISOString() + ' '
           + 'abcd'.slice(...[s, 3]) + ' ' + [1, 2, 3].reduce(...[function () {
             return arguments[0] + arguments[1]; }, 'r']));",
        "2020-02-02T03:04:05.006Z bc r123\n",
    );
}
