//! Integration tests: the `BigInt` typed arrays and the `DataView` `BigInt`
//! accessors (DESIGN.md §6.2, stage T5b): `BigInt64Array` /
//! `BigUint64Array` facts and constructor overloads, the content-type
//! `TypeError`s, 64-bit wraparound, `TypedArraySetElement`'s `ToBigInt`
//! errors, every `%TypedArray.prototype%` method that converts or compares
//! elements, `getBigInt64` / `getBigUint64` / `setBigInt64` /
//! `setBigUint64` in both byte orders and their step order, rendering, and
//! the allocating element reads under `gc_stress` and with the heap
//! exhausted.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_number, assert_output, assert_same_under_gc_stress, assert_string};

/// `err(f)`: the `name: message` of what `f` throws, or `none`; `TA` is
/// `%TypedArray%`.
const PRELUDE: &str = "function err(f) {
    try { f(); return 'none'; } catch (e) { return e.name + ': ' + e.message; }
}
const TA = Object.getPrototypeOf(Int8Array);
";

/// `src` after the [`PRELUDE`].
fn with_prelude(src: &str) -> String {
    format!("{PRELUDE}{src}")
}

/// The content-type `TypeError` of mixing Number and `BigInt` typed arrays.
const MIX: &str = "TypeError: Cannot mix BigInt and other types, use explicit conversions";

/// The constructors' and prototypes' own facts; elements read as `BigInt`.
#[test]
fn constructor_facts() {
    assert_string(
        &with_prelude(
            "[BigInt64Array.BYTES_PER_ELEMENT, BigUint64Array.prototype.BYTES_PER_ELEMENT,
              BigInt64Array.length, BigInt64Array.name, BigUint64Array.name,
              Object.getPrototypeOf(BigInt64Array) === TA,
              Object.getPrototypeOf(BigUint64Array.prototype) === TA.prototype,
              new BigInt64Array(2).length, new BigUint64Array(3).byteLength,
              Object.prototype.toString.call(new BigInt64Array(1)),
              new BigUint64Array(1)[Symbol.toStringTag], typeof new BigInt64Array(1)[0],
              new BigInt64Array(1)[0] === 0n, err(() => BigInt64Array())].join()",
        ),
        "8,8,3,BigInt64Array,BigUint64Array,true,true,2,24,[object BigInt64Array],\
         BigUint64Array,bigint,true,TypeError: Constructor BigInt64Array requires 'new'",
    );
}

/// Every overload: a length, an array, an iterable, an array-like (through
/// `ToBigInt`), a buffer view (with the 8-byte alignment errors), a typed
/// array of either `BigInt` kind.
#[test]
fn constructor_overloads() {
    assert_string(
        &with_prelude(
            "const b = new ArrayBuffer(16);
             [new BigInt64Array(2).join(), new BigInt64Array([1n, -2n]).join(),
              new BigUint64Array(new Set([5n])).join(),
              new BigInt64Array({ length: 2, 0: 1n, 1: '7' }).join(),
              new BigInt64Array([true, '0x10']).join(),
              new BigInt64Array(b, 8).length, new BigUint64Array(b).length,
              err(() => new BigInt64Array(b, 4)),
              err(() => new BigUint64Array(new ArrayBuffer(12))),
              new BigInt64Array(new BigInt64Array([3n])).join(),
              new BigUint64Array(new BigInt64Array([-1n])).join()].join(';')",
        ),
        "0,0;1,-2;5;1,7;1,16;1;2;\
         RangeError: Start offset of BigInt64Array should be a multiple of 8;\
         RangeError: Byte length of BigUint64Array should be a multiple of 8;\
         3;18446744073709551615",
    );
}

/// A Number typed array cannot initialize a `BigInt` one, nor the other way
/// round; Number and bad-string elements fail `ToBigInt`.
#[test]
fn constructor_content_type_and_to_bigint_errors() {
    assert_string(
        &with_prelude(
            "[err(() => new BigInt64Array(new Int8Array(1))),
              err(() => new BigUint64Array(new Float64Array(1))),
              err(() => new Float64Array(new BigInt64Array(1))),
              err(() => new Uint8Array(new BigUint64Array(1))),
              err(() => new BigInt64Array([1])),
              err(() => new BigInt64Array(['x'])),
              err(() => new BigUint64Array({ length: 1, 0: undefined }))].join(';')",
        ),
        &format!(
            "{MIX};{MIX};{MIX};{MIX};TypeError: Cannot convert 1 to a BigInt;\
             SyntaxError: Cannot convert x to a BigInt;\
             TypeError: Cannot convert undefined to a BigInt"
        ),
    );
}

/// `ToBigInt64` / `ToBigUint64` keep the low 64 bits.
#[test]
fn stores_wrap_to_64_bits() {
    assert_string(
        "const s = new BigInt64Array(3), u = new BigUint64Array(3);
         s[0] = 2n ** 63n; s[1] = 2n ** 64n + 1n; s[2] = -(2n ** 63n) - 1n;
         u[0] = -1n; u[1] = 2n ** 64n + 1n; u[2] = 2n ** 63n;
         [...s, ...u].join()",
        "-9223372036854775808,1,9223372036854775807,18446744073709551615,1,\
         9223372036854775808",
    );
}

/// `TypedArraySetElement` converts with `ToBigInt` first — even for an
/// invalid index — through every store path (inline, string key,
/// `defineProperty`).
#[test]
fn element_stores_use_to_bigint() {
    assert_string(
        &with_prelude(
            "const t = new BigInt64Array(1), k = '0';
             [err(() => { t[0] = 1; }), err(() => { t[k] = 'x'; }), err(() => { t[5] = 1; }),
              err(() => { t[0] = undefined; }), (t[0] = '12', t[0]), (t[k] = true, t[0]),
              (t[0] = { valueOf() { return 3n; } }, t[0]),
              (Object.defineProperty(t, '0', { value: 4n }), t[0]),
              err(() => Object.defineProperty(t, '0', { value: 4 }))].join(';')",
        ),
        "TypeError: Cannot convert 1 to a BigInt;SyntaxError: Cannot convert x to a BigInt;\
         TypeError: Cannot convert 1 to a BigInt;TypeError: Cannot convert undefined to a BigInt;\
         12;1;3;4;TypeError: Cannot convert 4 to a BigInt",
    );
}

/// The searches compare `BigInt` values (`SameValueZero` / strict
/// equality): a Number never matches a `BigInt` element.
#[test]
fn searches_compare_bigint_values() {
    assert_string(
        "const t = new BigInt64Array([3n, -1n, 2n, 3n]);
         [t.includes(3n), t.includes(3), t.includes(2n, 3), t.indexOf(2n), t.indexOf(2),
          t.indexOf(3n, 1), t.lastIndexOf(3n), t.lastIndexOf(3n, 2), t.lastIndexOf(4n), t.at(-1),
          t.find((x) => x < 0n), t.findIndex((x) => x === 2n), t.findLast((x) => x > 2n),
          t.some((x) => x === -1n), t.every((x) => typeof x === 'bigint'),
          t.reduce((a, x) => a + x, 0n), t.reduceRight((a, x) => a + x)].join()",
        "true,false,false,2,-1,3,3,0,-1,3,-1,2,3,true,true,7,7",
    );
}

/// `fill` and `with` convert with `ToBigInt`; `join`, `toString` and
/// `toLocaleString` print the decimal digits.
#[test]
fn fill_with_and_join() {
    assert_string(
        &with_prelude(
            "const t = new BigInt64Array([3n, -1n, 2n, 3n]);
             [new BigUint64Array(3).fill(-1n, 1).join(), err(() => new BigInt64Array(1).fill(1)),
              new BigInt64Array(2).fill('5').join(), t.with(1, 9n).join(), t.with(-1, '-4').join(),
              err(() => t.with(0, 1)), err(() => t.with(9, 1n)), t.join('|'), String(t),
              t.toLocaleString()].join(';')",
        ),
        "0,18446744073709551615,18446744073709551615;TypeError: Cannot convert 1 to a BigInt;\
         5,5;3,9,2,3;3,-1,2,-4;TypeError: Cannot convert 1 to a BigInt;\
         RangeError: Invalid typed array index;3|-1|2|3;3,-1,2,3;3,-1,2,3",
    );
}

/// The default sort order is the `BigInt` value (signed or unsigned by
/// kind); a comparator sorts by its result.
#[test]
fn sort_orders_by_bigint_value() {
    assert_string(
        "const t = new BigInt64Array([3n, -5n, 2n ** 62n, 0n, -(2n ** 63n)]);
         const sorted = t.toSorted();
         [sorted.join(), t.join(), new BigUint64Array([2n ** 63n, 1n, 0n]).sort().join(),
          t.sort((a, b) => Number(b - a)).join(), t.join(),
          sorted instanceof BigInt64Array].join(';')",
        "-9223372036854775808,-5,0,3,4611686018427387904;\
         3,-5,4611686018427387904,0,-9223372036854775808;0,1,9223372036854775808;\
         4611686018427387904,3,0,-5,-9223372036854775808;\
         4611686018427387904,3,0,-5,-9223372036854775808;true",
    );
}

/// `map`, `filter`, `slice`, `subarray`, `reverse`, `toReversed`,
/// `copyWithin` and the iterators over `BigInt` elements; `map`'s results go
/// through `ToBigInt`; a species of the other content type throws.
#[test]
fn element_copying_methods() {
    assert_string(
        &with_prelude(
            "const t = new BigInt64Array([3n, -1n, 2n, 3n]);
             class S extends BigInt64Array { static get [Symbol.species]() { return Int8Array; } }
             [t.map((x) => x * 2n).join(), err(() => t.map(() => 1)),
              t.filter((x) => x > 0n).join(), t.slice(1, 3).join(), t.subarray(2).join(),
              t.subarray(2) instanceof BigInt64Array, new BigInt64Array([1n, 2n, 3n]).reverse().join(),
              t.toReversed().join(), new BigUint64Array([1n, 2n, 3n]).copyWithin(0, 1).join(),
              [...new BigUint64Array([1n]).entries()][0].join(), Object.values(t).join(),
              err(() => new S(1).map((x) => x)), err(() => new S(1).slice())].join(';')",
        ),
        "6,-2,4,6;TypeError: Cannot convert 1 to a BigInt;3,2,3;-1,2;2,3;true;3,2,1;3,2,-1,3;\
         2,3,3;0,1;3,-1,2,3;\
         TypeError: The TypedArray species constructor returned a TypedArray of another content type;\
         TypeError: The TypedArray species constructor returned a TypedArray of another content type",
    );
}

/// `set` from arrays (through `ToBigInt`) and from typed arrays of either
/// `BigInt` kind (an overlapping same-buffer source too); a source of the
/// other content type throws.
#[test]
fn set_across_kinds_and_content_types() {
    assert_string(
        &with_prelude(
            "const u = new BigInt64Array(4);
             u.set([1n, 2n], 1); u.set(new BigUint64Array([5n]));
             const b = new ArrayBuffer(16), x = new BigInt64Array(b), y = new BigUint64Array(b, 8, 1);
             x[0] = -1n; y.set(x.subarray(0, 1));
             [u.join(), x[1], y[0], err(() => u.set(new Int8Array(1))),
              err(() => new Int8Array(1).set(u)), err(() => u.set([1])),
              err(() => new Float32Array(1).set([1n]))].join(';')",
        ),
        &format!(
            "5,1,2,0;-1;18446744073709551615;{MIX};{MIX};TypeError: Cannot convert 1 to a BigInt;\
             TypeError: Cannot convert a BigInt value to a number"
        ),
    );
}

/// `from` / `of` store through `ToBigInt`.
#[test]
fn from_and_of() {
    assert_string(
        &with_prelude(
            "[BigInt64Array.of(1n, 2n).join(), err(() => BigUint64Array.of(1)),
              BigInt64Array.from([1, 2], (x) => BigInt(x) * 3n).join(),
              BigInt64Array.from({ length: 1, 0: '5' }).join(),
              err(() => BigInt64Array.from([1.5]))].join(';')",
        ),
        "1,2;TypeError: Cannot convert 1 to a BigInt;3,6;5;TypeError: Cannot convert 1.5 to a BigInt",
    );
}

/// The `DataView` `BigInt` accessors in both byte orders, and their facts.
#[test]
fn data_view_bigint_accessors() {
    assert_string(
        "const v = new DataView(new ArrayBuffer(16));
         v.setBigInt64(0, -2n);
         v.setBigUint64(8, 0x0102030405060708n, true);
         const p = DataView.prototype;
         [v.getBigInt64(0), v.getBigUint64(0), v.getBigInt64(0, true), v.getUint8(0), v.getUint8(7),
          v.getBigUint64(8, true), v.getBigUint64(8), v.getUint8(8), v.getUint8(15),
          typeof v.getBigInt64(0), p.getBigInt64.length, p.setBigInt64.length,
          p.getBigUint64.name, p.setBigUint64.length,
          (v.setBigInt64(0, 2n ** 64n + 5n, true), v.getBigInt64(0, true)),
          (v.setBigUint64(0, -1n), v.getBigInt64(0))].join()",
        "-2,18446744073709551614,-72057594037927937,255,254,72623859790382856,\
         578437695752307201,8,1,bigint,1,2,getBigUint64,2,5,-1",
    );
}

/// `SetViewValue`'s order: `ToIndex`, then `ToBigInt(value)`, then the
/// detached / out-of-bounds `TypeError`, then the range `RangeError`.
#[test]
fn data_view_bigint_step_order() {
    assert_string(
        &with_prelude(
            "const v = new DataView(new ArrayBuffer(8));
             const b = new ArrayBuffer(8), w = new DataView(b);
             b.transfer();
             [err(() => v.setBigInt64(4, 1n)), err(() => v.setBigInt64(100, 1)),
              err(() => v.setBigInt64(-1, 1)), err(() => v.getBigUint64(1)),
              err(() => w.setBigInt64(0, 'x')), err(() => w.getBigInt64(0)),
              err(() => w.setBigUint64(0, 1n))].join(';')",
        ),
        "RangeError: Offset is outside the bounds of the DataView;\
         TypeError: Cannot convert 1 to a BigInt;\
         RangeError: Offset is outside the bounds of the DataView;\
         RangeError: Offset is outside the bounds of the DataView;\
         SyntaxError: Cannot convert x to a BigInt;\
         TypeError: Cannot perform DataView.prototype.getBigInt64 on a detached or out-of-bounds DataView;\
         TypeError: Cannot perform DataView.prototype.setBigUint64 on a detached or out-of-bounds DataView",
    );
}

/// The console form prints `BigInt` elements with their `n`.
#[test]
fn rendering() {
    assert_output(
        "console.log(new BigInt64Array([1n, -2n]), new BigUint64Array(0));
         console.log(new BigUint64Array([2n ** 64n - 1n]));",
        "BigInt64Array(2) [ 1n, -2n ] BigUint64Array(0) []\n\
         BigUint64Array(1) [ 18446744073709551615n ]\n",
    );
}

/// Every `BigInt` element read allocates: reads, copies, maps, sorts and
/// spreads kept alive under a collection at every allocation.
#[test]
fn bigint_element_reads_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 30; i++) {
           const t = new BigInt64Array([BigInt(i), -BigInt(i) * 2n ** 40n]);
           const u = BigUint64Array.from(t);
           keep.push(t, u, [...t], t.map((x) => x + 1n), t.toSorted());
         }
         let s = 0n;
         for (let i = 0; i < keep.length; i += 5) s += keep[i][1] + keep[i + 1][0];
         const v = new DataView(new ArrayBuffer(8));
         v.setBigUint64(0, 2n ** 63n);
         console.log(s, keep[145], keep[146], keep[147], keep[149], v.getBigUint64(0));",
        "-478287558082125n BigInt64Array(2) [ 29n, -31885837205504n ] \
         BigUint64Array(2) [ 29n, 18446712187872346112n ] [ 29n, -31885837205504n ] \
         BigInt64Array(2) [ -31885837205504n, 29n ] 9223372036854775808n\n",
    );
}

/// The renderer prints a `BigInt` element straight from its 64 bits: zero,
/// both `i64` extremes, the largest `u64`, and the powers of ten around the
/// widest digit count.
#[test]
fn rendering_digits_from_bits() {
    assert_output(
        "console.log(new BigInt64Array([0n, -1n, 9n, 10n, -(2n ** 63n), 2n ** 63n - 1n]));
         console.log(new BigUint64Array([10n ** 19n, 10n ** 19n - 1n, 2n ** 64n - 1n, 0n]));",
        "BigInt64Array(6) [ 0n, -1n, 9n, 10n, -9223372036854775808n, 9223372036854775807n ]\n\
         BigUint64Array(4) [ 10000000000000000000n, 9999999999999999999n, \
         18446744073709551615n, 0n ]\n",
    );
}

/// Exhaust the heap with live buffers of halving sizes (each failure is a
/// caught `RangeError: out of memory`), then read `BigInt` elements through
/// every kind of reader. A read whose `BigInt` allocation fails must throw
/// the `RangeError`, never yield a wrong value: `bad` counts a wrong value
/// (`1`) or another exception (`100`). Whether a given read still finds
/// room depends on the allocator's size classes, so the test asserts only
/// that no read is ever wrong.
#[test]
fn bigint_element_reads_never_wrong_out_of_memory() {
    assert_number(
        "const t = new BigInt64Array([5n, -7n]);
         const u = new BigUint64Array([9n]);
         let keep = [];
         for (let i = 0; i < 4096; i++) keep.push(0);
         let n = 0;
         for (let size = 1 << 28; size >= 1; ) {
           try { keep[n] = new ArrayBuffer(size); n = n + 1; }
           catch (e) { size = size >>> 1; }
         }
         let bad = 0;
         try { if (t[0] !== 5n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (u[0] !== 9n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (t.at(1) !== -7n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (t.includes(5n) !== true) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (t.indexOf(-7n) !== 1) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { for (const x of t) { if (x !== 5n) bad = bad + 1; break; } }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (Object.values(t)[1] !== -7n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (Object.getOwnPropertyDescriptor(t, 0).value !== 5n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         try { if (new BigUint64Array(t)[1] !== 2n ** 64n - 7n) bad = bad + 1; }
         catch (e) { if (!(e instanceof RangeError)) bad = bad + 100; }
         keep = 0;
         bad",
        0.0,
    );
}
