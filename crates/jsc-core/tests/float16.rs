//! Integration tests: binary16 (DESIGN.md §6.2): `Math.f16round`, the
//! `Float16Array` constructor and methods, and `DataView.prototype.getFloat16`
//! / `setFloat16`, with the encode edge table (signed zeros, subnormals, the
//! overflow threshold, ties to even, values a detour through `float32` would
//! round twice, NaN canonicalization) and the exact decode table; the
//! allocation paths under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_number, assert_same_under_gc_stress, assert_string};

/// `err(f)`: the `name: message` of what `f` throws, or `none`; `show(x)`:
/// `x` as a string with `-0` kept.
const PRELUDE: &str = "function err(f) {
    try { f(); return 'none'; } catch (e) { return e.name + ': ' + e.message; }
}
const show = (x) => Object.is(x, -0) ? '-0' : String(x);
";

/// `src` after the [`PRELUDE`].
fn with_prelude(src: &str) -> String {
    format!("{PRELUDE}{src}")
}

/// The encode table's inputs: `±0`, normal values, the largest finite half
/// and the overflow threshold `65520` (ties to `2^16`, i.e. infinity),
/// infinities, NaN, the smallest subnormal `2^-24`, the halfway point
/// `2^-25` (ties to `+0`) and the double just above it, the smallest normal
/// and the largest subnormal, ties to even at `1 + 2^-11` / `1 + 3·2^-11`,
/// `1 + 2^-11 + 2^-40` (just above a tie: through `float32` it would round
/// to the tie and then down), `3·2^-26` (rounds up to `2^-24`), a negative
/// value that rounds to `-0`, a value just below `2^-14` that rounds up to
/// it, and the ties below and above `65504`.
const ENCODE_INPUTS: &str = "[0, -0, 1, 1.337, 65504, 65519.99, 65520, -65520, Infinity, -Infinity,
     NaN, 5.960464477539063e-8, 2.9802322387695312e-8, 2.980232238769532e-8,
     0.00006103515625, 0.00006097555160522461, 1.0009765625, 1.00048828125,
     1.00146484375, -1.5, 0.1, 1.0004882812509095, 4.470348358154297e-8,
     -1.4901161193847656e-8, 0.00006102025508880615, 65488, 65512]";

/// The binary16 bits of [`ENCODE_INPUTS`] (computed with an IEEE
/// round-to-nearest-even reference).
const ENCODE_BITS: &str = "0,32768,15360,15705,31743,31743,31744,64512,31744,64512,32256,1,0,1,\
                           1024,1023,15361,15360,15362,48640,11878,15361,1,32768,1024,31742,31743";

/// Every input stores the reference bits through `Float16Array` (NaN as the
/// canonical quiet `0x7E00`, `-0` as `0x8000`), and `Math.f16round` agrees
/// with the store-then-load round trip.
#[test]
fn encode_edge_table() {
    assert_string(
        &with_prelude(&format!(
            "const v = {ENCODE_INPUTS};
             const f = new Float16Array(v);
             [...new Uint16Array(f.buffer)].join()"
        )),
        ENCODE_BITS,
    );
    assert_string(
        &with_prelude(&format!(
            "const v = {ENCODE_INPUTS};
             const f = new Float16Array(v.length);
             for (let i = 0; i < v.length; i++) f[i] = v[i];
             const bits = [...new Uint16Array(f.buffer)].join();
             const same = v.every((x, i) => Object.is(Math.f16round(x), f[i]));
             bits + ' ' + same"
        )),
        &format!("{ENCODE_BITS} true"),
    );
}

/// Decoding is exact: subnormals, the normal range ends, infinities, every
/// NaN pattern (quiet, signalling, negative) reads as the canonical `NaN`,
/// `-0` and `-2^-24` keep their sign.
#[test]
fn decode_table() {
    assert_string(
        &with_prelude(
            "const bits = [0x0001, 0x03ff, 0x0400, 0x7bff, 0x7c00, 0xfc00, 0x7c01, 0x7e00, 0xfe00,
                           0x8000, 0x3c00, 0xc000, 0x3555, 0x8001];
             const f = new Float16Array(new Uint16Array(bits).buffer);
             [...f].map(show).join() + ' ' + [...f].filter((x) => x !== x).length",
        ),
        "5.960464477539063e-8,0.00006097555160522461,0.00006103515625,65504,Infinity,-Infinity,\
         NaN,NaN,NaN,-0,1,-2,0.333251953125,-5.960464477539063e-8 3",
    );
}

/// `Math.f16round`: `ToNumber` first (a `valueOf` runs), the rounding of
/// the edge values, the function's own facts.
#[test]
fn math_f16round() {
    assert_string(
        &with_prelude(
            "const d = Object.getOwnPropertyDescriptor(Math, 'f16round');
             [Math.f16round(1.337), Math.f16round(65520), Math.f16round(-65504),
              show(Math.f16round(-0)), show(Math.f16round(-1e-8)), Math.f16round(NaN),
              Math.f16round(5.960464477539063e-8), Math.f16round(2.9802322387695312e-8),
              Math.f16round(1.0004882812509095), Math.f16round(1.00048828125),
              Math.f16round('2.5'), Math.f16round(null), Math.f16round(true),
              Math.f16round(undefined), Math.f16round({ valueOf() { return 3.14159; } }),
              Math.f16round(), Math.f16round.length, Math.f16round.name, d.writable,
              d.enumerable, d.configurable].join()",
        ),
        "1.3369140625,Infinity,-65504,-0,-0,NaN,5.960464477539063e-8,0,1.0009765625,1,\
         2.5,0,1,NaN,3.140625,NaN,1,f16round,true,false,true",
    );
    assert_string(
        &with_prelude(
            "err(() => Math.f16round({ valueOf() { throw new RangeError('v'); } }))
             + ' ' + err(() => Math.f16round(Symbol()))",
        ),
        "RangeError: v TypeError: Cannot convert a Symbol value to a number",
    );
}

/// A program that never names binary data still has `Math.f16round`
/// (through the method and as a value).
#[test]
fn math_f16round_without_binary_data() {
    assert_number("Math.f16round(1.337)", 1.336_914_062_5);
    assert_number(
        "const r = Math.f16round; r(0.1) + r(65519)",
        65_504.099_975_585_94,
    );
}

/// The constructor's facts and overloads, the alignment errors and the
/// conversions from and to the other kinds.
#[test]
fn float16_array_constructor() {
    assert_string(
        "const TA = Object.getPrototypeOf(Int8Array);
         const d = Object.getOwnPropertyDescriptor(globalThis, 'Float16Array');
         [Float16Array.BYTES_PER_ELEMENT, Float16Array.prototype.BYTES_PER_ELEMENT,
          Float16Array.length, Float16Array.name, Object.getPrototypeOf(Float16Array) === TA,
          Object.getPrototypeOf(Float16Array.prototype) === TA.prototype,
          Float16Array.prototype.constructor === Float16Array,
          Object.prototype.toString.call(new Float16Array(1)), new Float16Array(3).byteLength,
          new Float16Array(new ArrayBuffer(8), 2, 2).length, new Float16Array(new ArrayBuffer(8), 4).length,
          d.writable, d.enumerable, d.configurable, new Float16Array(2) instanceof TA].join()",
        "2,2,3,Float16Array,true,true,true,[object Float16Array],6,2,2,true,false,true,true",
    );
    assert_string(
        &with_prelude(
            "[err(() => new Float16Array(new ArrayBuffer(4), 1)),
              err(() => new Float16Array(new ArrayBuffer(3))), err(() => Float16Array(1))].join('|')",
        ),
        "RangeError: Start offset of Float16Array should be a multiple of 2|\
         RangeError: Byte length of Float16Array should be a multiple of 2|\
         TypeError: Constructor Float16Array requires 'new'",
    );
    assert_string(
        &with_prelude(
            "[...new Float16Array(new Float64Array([1.337, 70000, -0]))].map(show).join() + ' ' +
             new Float64Array(new Float16Array([0.1]))[0] + ' ' +
             [...new Int16Array(new Float16Array([1.5, -2.5, 65504]))].join() + ' ' +
             [...new Float16Array(new Set([1 / 3]))].join() + ' ' +
             [...Float16Array.from([1, 2], (x) => x / 3)].join() + ' ' +
             [...Float16Array.of(0.1, 65520)].join()",
        ),
        "1.3369140625,Infinity,-0 0.0999755859375 1,-2,-32 0.333251953125 \
         0.333251953125,0.66650390625 0.0999755859375,Infinity",
    );
}

/// The `%TypedArray.prototype%` methods over Float16 elements: the default
/// `sort` / `toSorted` order (`-0 < +0`, `NaN` last), `fill`, `set`,
/// `map`, `includes` / `indexOf`, `subarray`, `with`, and rendering.
#[test]
fn float16_array_methods() {
    assert_string(
        &with_prelude(
            "const t = new Float16Array([3, -0, NaN, 0, -Infinity, 1.5, -2]);
             const s = t.toSorted();
             t.sort();
             [[...s].map(show).join(), [...t].map(show).join(),
              [...new Float16Array(2).fill(0.1)].join(),
              (() => { const a = new Float16Array(3); a.set([65520, 1e-8, -1e-8]);
                       return [...a].map(show).join(); })(),
              [...Float16Array.of(1, 2).map((x) => x / 3)].join(),
              t.includes(NaN), t.indexOf(1.5), t.indexOf(NaN), [...t.subarray(2, 4)].map(show).join(),
              [...t.with(0, 0.1)].slice(0, 1).join(),
              new Float16Array([0.1, 0.2]).join(';')].join(' ')",
        ),
        "-Infinity,-2,-0,0,1.5,3,NaN -Infinity,-2,-0,0,1.5,3,NaN 0.0999755859375,0.0999755859375 \
         Infinity,0,-0 0.333251953125,0.66650390625 true 4 -1 -0,0 0.0999755859375 \
         0.0999755859375;0.199951171875",
    );
    assert_string(
        "const t = new Float16Array([1.337, 1e5]);
         const c = [];
         t.forEach((x, i) => c.push(i + ':' + x));
         [c.join(), t.reduce((a, b) => a + b, 0), [...t.entries()].join(';')].join(' ')",
        "0:1.3369140625,1:Infinity Infinity 0,1.3369140625;1,Infinity",
    );
}

/// The console form of a `Float16Array`.
#[test]
fn float16_array_rendering() {
    common::assert_output(
        "console.log(new Float16Array([1.5, NaN, 65520, 0.1]));",
        "Float16Array(4) [ 1.5, NaN, Infinity, 0.0999755859375 ]\n",
    );
}

/// `DataView.prototype.getFloat16` / `setFloat16`: both byte orders, the
/// rounding on write, NaN canonical on store and on load, the method facts
/// and the index / bounds errors.
#[test]
fn data_view_float16() {
    assert_string(
        &with_prelude(
            "const v = new DataView(new ArrayBuffer(4));
             const r = v.setFloat16(0, 1.5);
             const a = [r, v.getUint8(0), v.getUint8(1), v.getFloat16(0), v.getFloat16(0, true)];
             v.setFloat16(2, -65504, true);
             a.push(v.getUint8(2), v.getUint8(3), v.getFloat16(2, true), v.getFloat16(2));
             v.setFloat16(0, 65520);
             a.push(v.getFloat16(0));
             v.setFloat16(0, NaN);
             a.push(v.getUint16(0));
             v.setFloat16(0, -0, true);
             a.push(v.getUint16(0, true), show(v.getFloat16(0, true)));
             v.setUint16(0, 0xfc01);
             a.push(v.getFloat16(0));
             v.setFloat16(1, 1.0004882812509095);
             a.push(v.getUint16(1), v.getFloat16(1));
             v.setFloat16(0, '0.1', 1);
             a.push(v.getFloat16(0, 'yes'));
             a.join()",
        ),
        "undefined,62,0,1.5,0.0000036954879760742188,255,251,-65504,NaN,Infinity,32256,32768,-0,\
         NaN,15361,1.0009765625,0.0999755859375",
    );
    assert_string(
        &with_prelude(
            "const P = DataView.prototype;
             const v = new DataView(new ArrayBuffer(4));
             const g = Object.getOwnPropertyDescriptor(P, 'getFloat16');
             const b = new ArrayBuffer(2), w = new DataView(b);
             b.transfer();
             [P.getFloat16.length, P.setFloat16.length, P.getFloat16.name, P.setFloat16.name,
              g.writable, g.enumerable, g.configurable, err(() => v.getFloat16(3)),
              err(() => v.setFloat16(-1, 0)), err(() => w.getFloat16(0)),
              err(() => P.setFloat16.call(new Uint8Array(2), 0, 1))].join('|')",
        ),
        "1|2|getFloat16|setFloat16|true|false|true|\
         RangeError: Offset is outside the bounds of the DataView|\
         RangeError: Offset is outside the bounds of the DataView|\
         TypeError: Cannot perform DataView.prototype.getFloat16 on a detached or out-of-bounds DataView|\
         TypeError: Method DataView.prototype.setFloat16 called on incompatible receiver",
    );
}

/// Allocation-heavy Float16 paths under `gc_stress`.
#[test]
fn float16_allocation_paths_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const out = [];
         for (let i = 0; i < 20; i++) {
           const t = Float16Array.from([i, i / 3, i * 1000], (x) => x + 0.1);
           out.push(t.map((x) => x * 2).toSorted().join(';'));
         }
         console.log(out.slice(18).join(' '));",
        "12.203125;36.1875;36000 12.8671875;38.1875;38016\n",
    );
    assert_same_under_gc_stress(
        "const s = [];
         for (let i = 0; i < 30; i++) s.push(Math.f16round(i / 7) + ':' + new DataView(new ArrayBuffer(2)).getFloat16(0));
         console.log(s.slice(28).join(' '));",
        "4:0 4.14453125:0\n",
    );
}
