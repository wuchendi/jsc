//! Integration tests: the page allocator and the collector
//! (design/runtime-memory.md §5.6-§5.7). Collection must never change what a
//! program computes.

mod common;
use common::{assert_bool, assert_number};

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
