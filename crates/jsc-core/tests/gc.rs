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
/// suite under `gc_stress` is the CI `gc-stress` job, where `JSC_GC_STRESS=1`
/// makes the shared helpers (`common::compile` / `common::exec`, and the
/// in-crate ones) build every program with a collection at each allocation.
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
    };
    let plain = common::compile_with(src, options(false)).expect("compiles");
    let stressed = common::compile_with(src, options(true)).expect("compiles");
    assert_ne!(plain, stressed, "gc_stress changes the module");
    let expected = if common::gc_stress_from_env() {
        stressed
    } else {
        plain
    };
    assert_eq!(common::compile(src, false).expect("compiles"), expected);
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
