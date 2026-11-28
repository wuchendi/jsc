//! Integration tests: `DataView` (DESIGN.md §7.9): the constructor's
//! overloads, errors and coercion order, length-tracking views over
//! resizable buffers, detaching through `transfer`, the getters, every
//! number get / set pair in both byte orders with edge values, the index
//! checks and the `GetViewValue` / `SetViewValue` step order, `@@toStringTag`,
//! subclassing, rendering, and the allocation paths under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_same_under_gc_stress, assert_string};

/// `err(f)`: the `name: message` of what `f` throws, or `none`.
const ERR: &str = "function err(f) {
    try { f(); return 'none'; } catch (e) { return e.name + ': ' + e.message; }
}
";

/// `src` after the [`ERR`] helper.
fn with_err(src: &str) -> String {
    format!("{ERR}{src}")
}

/// The out-of-bounds `TypeError` of `DataView.prototype.<m>`.
fn oob(m: &str) -> String {
    format!(
        "TypeError: Cannot perform DataView.prototype.{m} on a detached or out-of-bounds DataView"
    )
}

/// The request-index `RangeError` of the get / set methods.
const OFFSET: &str = "RangeError: Offset is outside the bounds of the DataView";

/// The overloads: no offset, an offset, an offset and a length (through
/// `ToIndex`), an offset at the end; the constructor's own facts.
#[test]
fn constructor_overloads_and_facts() {
    assert_string(
        "const b = new ArrayBuffer(8);
         const v1 = new DataView(b), v2 = new DataView(b, 2), v3 = new DataView(b, 2, 3);
         const v4 = new DataView(b, 8), v5 = new DataView(b, '1', 2.9);
         [v1.byteLength, v1.byteOffset, v2.byteLength, v2.byteOffset, v3.byteLength,
          v3.byteOffset, v4.byteLength, v4.byteOffset, v5.byteOffset, v5.byteLength,
          v1.buffer === b, new DataView(b, undefined, undefined).byteLength,
          new DataView(b, null).byteOffset, new DataView(b, -0.9, NaN).byteLength,
          typeof v1, Object.getPrototypeOf(v1) === DataView.prototype, v1 instanceof DataView,
          DataView.length, DataView.name, DataView.prototype.constructor === DataView,
          Object.prototype.toString.call(v1), String(v1), ArrayBuffer.isView(v1),
          Object.keys(v1).length].join()",
        "8,0,6,2,3,2,0,8,1,2,true,8,0,0,object,true,true,1,DataView,true,\
         [object DataView],[object DataView],true,0",
    );
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(globalThis, 'DataView');
         const p = Object.getOwnPropertyDescriptor(DataView, 'prototype');
         [d.value === DataView, d.writable, d.enumerable, d.configurable,
          p.writable, p.enumerable, p.configurable,
          Object.getPrototypeOf(DataView.prototype) === Object.prototype].join()",
        "true,true,false,true,false,false,false,true",
    );
}

/// The constructor's errors: called without `new`, a non-`ArrayBuffer`
/// first argument (checked before `ToIndex(byteOffset)`), the offset and
/// length `RangeError`s, a detached buffer.
#[test]
fn constructor_errors() {
    assert_string(
        &with_err(
            "const d = new ArrayBuffer(4); d.transfer();
             [err(() => DataView(new ArrayBuffer(1))),
              err(() => new DataView()),
              err(() => new DataView({})),
              err(() => new DataView([1, 2])),
              err(() => new DataView(new DataView(new ArrayBuffer(1)))),
              err(() => new DataView(new ArrayBuffer(4), -1)),
              err(() => new DataView(new ArrayBuffer(4), 5)),
              err(() => new DataView(new ArrayBuffer(4), Infinity)),
              err(() => new DataView(new ArrayBuffer(4), 0, -1)),
              err(() => new DataView(new ArrayBuffer(4), 1, 4)),
              err(() => new DataView(new ArrayBuffer(4), 0, 2 ** 53)),
              err(() => new DataView(d)),
              err(() => new DataView(d, 0, 0)),
              err(() => new DataView(d, -1))].join('|')",
        ),
        "TypeError: Constructor DataView requires 'new'|\
         TypeError: First argument to DataView constructor must be an ArrayBuffer|\
         TypeError: First argument to DataView constructor must be an ArrayBuffer|\
         TypeError: First argument to DataView constructor must be an ArrayBuffer|\
         TypeError: First argument to DataView constructor must be an ArrayBuffer|\
         RangeError: Start offset is outside the bounds of the buffer|\
         RangeError: Start offset is outside the bounds of the buffer|\
         RangeError: Start offset is outside the bounds of the buffer|\
         RangeError: Invalid DataView length|\
         RangeError: Invalid DataView length|\
         RangeError: Invalid DataView length|\
         TypeError: Cannot perform Construct on a detached ArrayBuffer|\
         TypeError: Cannot perform Construct on a detached ArrayBuffer|\
         RangeError: Start offset is outside the bounds of the buffer",
    );
}

/// The constructor's coercion order: the buffer check first, then
/// `ToIndex(byteOffset)`, then the detached check, then
/// `ToIndex(byteLength)`; a conversion that detaches or shrinks the buffer
/// is caught by the next check.
#[test]
fn constructor_coercion_order() {
    assert_string(
        &with_err(
            "const log = [];
             const at = (name, n, f) => ({ valueOf() { log.push(name); if (f) f(); return n; } });
             const b = new ArrayBuffer(8);
             const v = new DataView(b, at('offset', 1), at('length', 2));
             const r1 = err(() => new DataView({}, at('never', 0)));
             const b2 = new ArrayBuffer(8);
             const r2 = err(() => new DataView(b2, at('o2', 0, () => b2.transfer()), at('never', 1)));
             const b3 = new ArrayBuffer(8);
             const r3 = err(() => new DataView(b3, 0, at('l3', 1, () => b3.transfer())));
             const b4 = new ArrayBuffer(8, { maxByteLength: 8 });
             const r4 = err(() => new DataView(b4, at('o4', 6, () => b4.resize(4))));
             const b5 = new ArrayBuffer(8, { maxByteLength: 8 });
             const r5 = err(() => new DataView(b5, 2, at('l5', 4, () => b5.resize(4))));
             [v.byteOffset, v.byteLength, log.join('+'), r1, r2, r3, r4, r5].join('|')",
        ),
        "1|2|offset+length+o2+l3+o4+l5|\
         TypeError: First argument to DataView constructor must be an ArrayBuffer|\
         TypeError: Cannot perform Construct on a detached ArrayBuffer|\
         TypeError: Cannot perform Construct on a detached ArrayBuffer|\
         RangeError: Start offset is outside the bounds of the buffer|\
         RangeError: Invalid DataView length",
    );
}

/// An `undefined` `byteLength` over a resizable buffer makes a
/// length-tracking view: it grows and shrinks with the buffer, and goes out
/// of bounds (and back) when the buffer ends before its offset. A fixed
/// view over a resizable buffer is out of bounds while the buffer is
/// shorter than its end.
#[test]
fn length_tracking_and_out_of_bounds() {
    let expected = [
        "3",
        "7",
        "2",
        &oob("byteLength"),
        &oob("byteOffset"),
        "true",
        "1",
        "1",
        &oob("byteLength"),
        &oob("byteOffset"),
        &oob("getInt8"),
        "0",
        &oob("getInt8"),
        "2",
        "1",
        "0",
        "4",
    ]
    .join("|");
    assert_string(
        &with_err(
            "const b = new ArrayBuffer(4, { maxByteLength: 8 });
             const fixed = new DataView(b, 1, 2), track = new DataView(b, 1);
             const end = new DataView(b, 4);
             const out = [track.byteLength];
             b.resize(8); out.push(track.byteLength, fixed.byteLength);
             b.resize(2);
             out.push(err(() => fixed.byteLength), err(() => fixed.byteOffset), fixed.buffer === b,
                      track.byteLength, track.byteOffset);
             b.resize(0);
             out.push(err(() => track.byteLength), err(() => track.byteOffset),
                      err(() => track.getInt8(0)));
             b.resize(1);
             out.push(track.byteLength, err(() => fixed.getInt8(0)));
             b.resize(3);
             out.push(fixed.byteLength, fixed.byteOffset);
             b.resize(4); out.push(end.byteLength);
             b.resize(8); out.push(end.byteLength);
             out.join('|')",
        ),
        &expected,
    );
    // A fixed-length buffer gives a fixed view with the rest of the buffer.
    assert_string(
        "const b = new ArrayBuffer(6);
         const v = new DataView(b, 2);
         const t = new ArrayBuffer(2, { maxByteLength: 4 });
         const w = new DataView(t, 0, 2);
         t.resize(4);
         [v.byteLength, w.byteLength].join()",
        "4,2",
    );
}

/// Detaching the buffer (`transfer`, `transferToFixedLength`): `buffer`
/// still answers, the other getters and every access throw the
/// out-of-bounds `TypeError` after the index conversion.
#[test]
fn detached_views() {
    let expected = [
        "true",
        &oob("byteLength"),
        &oob("byteOffset"),
        &oob("getInt8"),
        &oob("setFloat64"),
        OFFSET,
        "true",
        &oob("getUint16"),
    ]
    .join("|");
    assert_string(
        &with_err(
            "const b = new ArrayBuffer(4); const v = new DataView(b, 1);
             b.transfer();
             const r = new ArrayBuffer(4, { maxByteLength: 8 }); const w = new DataView(r);
             r.transferToFixedLength();
             [v.buffer === b, err(() => v.byteLength), err(() => v.byteOffset),
              err(() => v.getInt8(0)), err(() => v.setFloat64(0, 1)), err(() => v.getInt8(-1)),
              w.buffer === r, err(() => w.getUint16(0, true))].join('|')",
        ),
        &expected,
    );
}

/// Bytes written one at a time read back by every getter in both byte
/// orders (big-endian by default), at aligned and unaligned offsets.
#[test]
fn reads_in_both_byte_orders() {
    assert_string(
        "const b = new ArrayBuffer(8), v = new DataView(b);
         for (let i = 0; i < 8; i++) v.setUint8(i, i + 1);
         [v.getInt8(0), v.getUint8(7), v.getInt16(0), v.getInt16(0, true), v.getUint16(6),
          v.getUint16(6, true), v.getInt32(0), v.getInt32(0, true), v.getUint32(4),
          v.getUint32(4, true), v.getInt16(1), v.getUint32(3, 1), v.getInt16(0, false),
          v.getInt16(0, 0), v.getInt16(0, 'x'), v.getInt16(0, {})].join()",
        "1,8,258,513,1800,2055,16909060,67305985,84281096,134678021,515,117835012,\
         258,258,513,513",
    );
    assert_string(
        "const v = new DataView(new ArrayBuffer(4));
         v.setUint32(0, 0xfffefdfc);
         [v.getInt8(0), v.getUint8(0), v.getInt16(0), v.getUint16(0), v.getInt16(2, true),
          v.getInt32(0), v.getUint32(0), v.getInt32(0, true), v.getUint32(0, true)].join()",
        "-1,255,-2,65534,-771,-66052,4294901244,-50462977,4244504319",
    );
    assert_string(
        "const v = new DataView(new ArrayBuffer(12));
         v.setUint32(0, 0x3ff80000); v.setUint32(8, 0x0000c03f);
         [v.getFloat64(0), v.getFloat32(0), v.getFloat32(8, true), v.getFloat32(4)].join()",
        "1.5,1.9375,1.5,0",
    );
}

/// Every setter in both byte orders, observed through the buffer's bytes
/// (the rendering prints them in memory order).
#[test]
fn writes_in_both_byte_orders() {
    assert_output(
        "const b = new ArrayBuffer(8), v = new DataView(b);
         v.setFloat64(0, 1.5); console.log(b);
         v.setFloat64(0, 1.5, true); console.log(b);
         v.setFloat32(0, 1.5); v.setFloat32(4, -2, true); console.log(b);
         v.setInt32(0, -2); v.setUint32(4, 0xdeadbeef, true); console.log(b);
         v.setInt16(0, -2, false); v.setUint16(2, 0x1234, true); v.setInt8(4, -1);
         v.setUint8(5, 0x80); console.log(b);
         console.log(v.setInt8(0, 1), v.setFloat64(0, 2));",
        "ArrayBuffer { [Uint8Contents]: <3f f8 00 00 00 00 00 00>, byteLength: 8 }\n\
         ArrayBuffer { [Uint8Contents]: <00 00 00 00 00 00 f8 3f>, byteLength: 8 }\n\
         ArrayBuffer { [Uint8Contents]: <3f c0 00 00 00 00 00 c0>, byteLength: 8 }\n\
         ArrayBuffer { [Uint8Contents]: <ff ff ff fe ef be ad de>, byteLength: 8 }\n\
         ArrayBuffer { [Uint8Contents]: <ff fe 34 12 ff 80 ad de>, byteLength: 8 }\n\
         undefined undefined\n",
    );
}

/// The value conversions of `byteConversionValues.js`: each value set and
/// read back by the same kind, little- and big-endian alike, at an
/// unaligned offset.
#[test]
fn value_conversions() {
    let src = |kind: &str, values: &str| {
        format!(
            "function rt(le) {{
               const v = new DataView(new ArrayBuffer(16));
               return [{values}].map((x) => {{ v.set{kind}(3, x, le); return v.get{kind}(3, le); }})
                 .join();
             }}
             rt(false) === rt(true) ? rt(false) : 'mismatch ' + rt(false) + ' / ' + rt(true)"
        )
    };
    let all = "127, 128, -128, -129, 255, 256, 1.9, -1.9, NaN, Infinity, -Infinity, -0, \
               2 ** 32 + 1, 'x', '7', true, null, undefined, { valueOf() { return 300; } }";
    assert_string(
        &src("Int8", all),
        "127,-128,-128,127,-1,0,1,-1,0,0,0,0,1,0,7,1,0,0,44",
    );
    assert_string(
        &src("Uint8", all),
        "127,128,128,127,255,0,1,255,0,0,0,0,1,0,7,1,0,0,44",
    );
    let words = "32767, 32768, -32769, 65535, 65536, 1.5, -1.5, NaN, -65537, 2 ** 53";
    assert_string(&src("Int16", words), "32767,-32768,32767,-1,0,1,-1,0,-1,0");
    assert_string(
        &src("Uint16", words),
        "32767,32768,32767,65535,0,1,65535,0,65535,0",
    );
    let dwords = "2147483647, 2147483648, -2147483649, 4294967295, 4294967296, 1e10, -1.5, \
                  2 ** 64 + 2 ** 12, -Infinity, 1e21";
    assert_string(
        &src("Int32", dwords),
        "2147483647,-2147483648,2147483647,-1,0,1410065408,-1,4096,0,-559939584",
    );
    assert_string(
        &src("Uint32", dwords),
        "2147483647,2147483648,2147483647,4294967295,0,1410065408,4294967295,4096,0,3735027712",
    );
    assert_string(
        &src(
            "Float32",
            "1.5, 0.1, 1e40, -1e40, 1e-50, NaN, Infinity, 16777217, 3.4028234663852886e38, \
             1.401298464324817e-45, '2.5'",
        ),
        "1.5,0.10000000149011612,Infinity,-Infinity,0,NaN,Infinity,16777216,\
         3.4028234663852886e+38,1.401298464324817e-45,2.5",
    );
    assert_string(
        &src(
            "Float64",
            "0.1, NaN, 5e-324, 1.7976931348623157e308, -Infinity, '1e3', 2 ** 53 + 1",
        ),
        "0.1,NaN,5e-324,1.7976931348623157e+308,-Infinity,1000,9007199254740992",
    );
    // `-0` survives both float kinds and both byte orders.
    assert_string(
        "const v = new DataView(new ArrayBuffer(8));
         v.setFloat32(1, -0); const a = 1 / v.getFloat32(1);
         v.setFloat32(1, -0, true); const b = 1 / v.getFloat32(1, true);
         v.setFloat64(0, -0); const c = 1 / v.getFloat64(0);
         v.setFloat64(0, -0, true); const d = 1 / v.getFloat64(0, true);
         [a, b, c, d, v.getUint8(0)].join()",
        "-Infinity,-Infinity,-Infinity,-Infinity,0",
    );
}

/// A NaN of any payload written as raw bits reads back as the canonical NaN
/// (DESIGN.md §6.2): never a pointer, boolean or `undefined`.
#[test]
fn nan_reads_are_canonical() {
    assert_string(
        "const v = new DataView(new ArrayBuffer(8));
         const out = [];
         for (const [hi, lo] of [[0x7ff00000, 1], [0xfff80000, 1], [0xffffffff, 0xffffffff],
                                 [0x7ff80000, 5], [0xfffc0000, 0], [0x7ff40000, 0x12345678]]) {
           v.setUint32(0, hi); v.setUint32(4, lo);
           const x = v.getFloat64(0);
           const y = v.getFloat32(0);
           v.setUint32(0, lo, true); v.setUint32(4, hi, true);
           const z = v.getFloat64(0, true);
           out.push([typeof x, x !== x, Number.isNaN(y), String(z + 1), Object.is(x, NaN),
                     [x, y, z].join('/')].join(' '));
         }
         out.join('|')",
        &["number true true NaN true NaN/NaN/NaN"; 6].join("|"),
    );
    assert_output(
        "const v = new DataView(new ArrayBuffer(8));
         v.setUint32(0, 0xfff80000); v.setUint32(4, 2);
         console.log(v.getFloat64(0), [v.getFloat64(0)], { x: v.getFloat64(0) });",
        "NaN [ NaN ] { x: NaN }\n",
    );
}

/// The request index: `ToIndex` (`RangeError` below `0` or past `2^53 -
/// 1`), then `index + size > byteLength` is a `RangeError`; truncation,
/// strings, `undefined` and `NaN` are index `0`.
#[test]
fn index_checks() {
    assert_string(
        &with_err(
            "const v = new DataView(new ArrayBuffer(4), 1);
             [err(() => v.getInt8(3)), err(() => v.getInt16(2)), err(() => v.getUint32(0)),
              v.getUint16(1), err(() => v.getInt8(-1)), err(() => v.getInt8(Infinity)),
              err(() => v.setInt8(2 ** 53, 0)), v.getInt8(-0.9), v.getInt8('2'), v.getInt8(),
              v.getInt8(NaN), err(() => v.setFloat64(0, 1)), err(() => v.getFloat32(-1.5)),
              err(() => v.setUint16(2, 1)), v.setUint8(2.7, 9), v.getUint8(2),
              err(() => new DataView(new ArrayBuffer(0)).getInt8(0)),
              err(() => new DataView(new ArrayBuffer(8), 8).getFloat64(0))].join('|')",
        ),
        &[
            OFFSET, OFFSET, OFFSET, "0", OFFSET, OFFSET, OFFSET, "0", "0", "0", "0", OFFSET,
            OFFSET, OFFSET, "", "9", OFFSET, OFFSET,
        ]
        .join("|"),
    );
}

/// `GetViewValue` / `SetViewValue` order: the receiver check, then
/// `ToIndex(requestIndex)`, then the setter's `ToNumber(value)`, then
/// `ToBoolean(littleEndian)` (no user code), then the out-of-bounds check
/// against the buffer as the conversions left it, then the range check.
#[test]
fn coercion_order() {
    let expected = [
        "index+value",
        OFFSET,
        "TypeError: Method DataView.prototype.getInt8 called on incompatible receiver",
        &oob("setUint8"),
        &oob("setUint8"),
        OFFSET,
        &oob("getInt8"),
        "0",
        "i2+v2",
        "513",
    ]
    .join("|");
    assert_string(
        &with_err(
            "const log = [];
             const at = (name, n, f) => ({ valueOf() { log.push(name); if (f) f(); return n; } });
             const b = new ArrayBuffer(8), v = new DataView(b);
             v.setInt16(at('index', 0), at('value', 258), { valueOf() { log.push('le'); } });
             const first = log.join('+'); log.length = 0;
             const r1 = err(() => v.setInt8(-1, at('never', 0)));
             const r2 = err(() => DataView.prototype.getInt8.call({}, at('never', 0)));
             const b2 = new ArrayBuffer(4), v2 = new DataView(b2);
             const r3 = err(() => v2.setUint8(at('i2', 0, () => b2.transfer()), at('v2', 1)));
             const second = log.join('+');
             const b3 = new ArrayBuffer(4), v3 = new DataView(b3);
             const r4 = err(() => v3.setUint8(0, at('v3', 1, () => b3.transfer())));
             const b4 = new ArrayBuffer(4, { maxByteLength: 8 }), v4 = new DataView(b4);
             const r5 = err(() => v4.setUint32(0, at('v4', 1, () => b4.resize(2))));
             const v5 = new DataView(b4, 0, 2);
             const r6 = err(() => v5.getInt8(at('i5', 0, () => b4.resize(1))));
             const r7 = v4.getUint32(at('i4', 4, () => b4.resize(8)));
             [first, r1, r2, r3, r4, r5, r6, r7, second, v.getInt16(0)].join('|')",
        ),
        &expected,
    );
}

/// The getters' and methods' receiver checks, their descriptors, `name` and
/// `length`, and `@@toStringTag`.
#[test]
fn prototype_shape_and_receivers() {
    assert_string(
        "const P = DataView.prototype;
         ['getInt8', 'setInt8', 'getUint8', 'setUint8', 'getInt16', 'setInt16', 'getUint16',
          'setUint16', 'getInt32', 'setInt32', 'getUint32', 'setUint32', 'getFloat32',
          'setFloat32', 'getFloat64', 'setFloat64', 'getFloat16', 'setFloat16'].map((n) => {
            const d = Object.getOwnPropertyDescriptor(P, n);
            return [n, d.value.length, d.value.name === n, d.writable, d.enumerable,
                    d.configurable].join(':');
          }).join(' ')",
        "getInt8:1:true:true:false:true setInt8:2:true:true:false:true \
         getUint8:1:true:true:false:true setUint8:2:true:true:false:true \
         getInt16:1:true:true:false:true setInt16:2:true:true:false:true \
         getUint16:1:true:true:false:true setUint16:2:true:true:false:true \
         getInt32:1:true:true:false:true setInt32:2:true:true:false:true \
         getUint32:1:true:true:false:true setUint32:2:true:true:false:true \
         getFloat32:1:true:true:false:true setFloat32:2:true:true:false:true \
         getFloat64:1:true:true:false:true setFloat64:2:true:true:false:true \
         getFloat16:1:true:true:false:true setFloat16:2:true:true:false:true",
    );
    assert_string(
        "const P = DataView.prototype;
         const t = Object.getOwnPropertyDescriptor(P, Symbol.toStringTag);
         ['buffer', 'byteLength', 'byteOffset'].map((k) => {
            const d = Object.getOwnPropertyDescriptor(P, k);
            return [k, typeof d.get, d.set === undefined, d.enumerable, d.configurable,
                    d.get.name, d.get.length].join(':');
          }).concat([t.value, t.writable, t.enumerable, t.configurable]).join(' ')",
        "buffer:function:true:false:true:get buffer:0 \
         byteLength:function:true:false:true:get byteLength:0 \
         byteOffset:function:true:false:true:get byteOffset:0 \
         DataView false false true",
    );
    assert_string(
        &with_err(
            "const P = DataView.prototype;
             const g = (k) => Object.getOwnPropertyDescriptor(P, k).get;
             [err(() => P.byteLength), err(() => P.buffer), err(() => g('byteOffset').call({})),
              err(() => g('buffer').call(new ArrayBuffer(1))),
              err(() => P.getInt8.call(new ArrayBuffer(1), 0)),
              err(() => P.setFloat64.call(undefined, 0, 1)),
              err(() => P.getUint32.call(1, 0))].join('|')",
        ),
        "TypeError: Method DataView.prototype.byteLength called on incompatible receiver|\
         TypeError: Method DataView.prototype.buffer called on incompatible receiver|\
         TypeError: Method DataView.prototype.byteOffset called on incompatible receiver|\
         TypeError: Method DataView.prototype.buffer called on incompatible receiver|\
         TypeError: Method DataView.prototype.getInt8 called on incompatible receiver|\
         TypeError: Method DataView.prototype.setFloat64 called on incompatible receiver|\
         TypeError: Method DataView.prototype.getUint32 called on incompatible receiver",
    );
}

/// A subclass constructs through `super(buffer, offset)` with its own
/// prototype and properties; a view over an `ArrayBuffer` subclass works;
/// two views over one buffer see each other's writes.
#[test]
fn subclassing_and_aliasing() {
    assert_string(
        "class Sub extends DataView {
           constructor(b) { super(b, 1); this.tag = 'sub'; }
           get first() { return this.getUint8(0); }
         }
         class Buf extends ArrayBuffer {}
         const b = new ArrayBuffer(4); new DataView(b).setUint8(1, 7);
         const s = new Sub(b);
         const w = new DataView(new Buf(2));
         w.setInt16(0, -3);
         [s instanceof Sub, s instanceof DataView, s.byteOffset, s.byteLength, s.first, s.tag,
          Object.getPrototypeOf(Sub) === DataView, Object.prototype.toString.call(s),
          ArrayBuffer.isView(s), w.buffer instanceof Buf, w.getUint16(0),
          new DataView(b, 2).getUint8(0) === new DataView(b).getUint8(2)].join()",
        "true,true,1,3,7,sub,true,[object DataView],true,true,65533,true",
    );
}

/// The console form (DESIGN.md §6.9): length, offset and the buffer;
/// both numbers `0` once out of bounds or detached; nested like any value.
#[test]
fn rendering() {
    assert_output(
        "const b = new ArrayBuffer(2);
         console.log(new DataView(b));
         const r = new ArrayBuffer(3, { maxByteLength: 8 });
         const v = new DataView(r, 1); v.setUint8(0, 0xab);
         console.log(v);
         r.resize(0); console.log(v);
         const d = new ArrayBuffer(1); const dv = new DataView(d); d.transfer();
         console.log([dv], { v: new DataView(new ArrayBuffer(1), 1) });",
        "DataView { byteLength: 2, byteOffset: 0, buffer: ArrayBuffer { [Uint8Contents]: <00 00>, byteLength: 2 } }\n\
         DataView { byteLength: 2, byteOffset: 1, buffer: ArrayBuffer { [Uint8Contents]: <00 ab 00>, byteLength: 3 } }\n\
         DataView { byteLength: 0, byteOffset: 0, buffer: ArrayBuffer { [Uint8Contents]: <>, byteLength: 0 } }\n\
         [ DataView { byteLength: 0, byteOffset: 0, buffer: ArrayBuffer { (detached), byteLength: 0 } } ] \
         { v: DataView { byteLength: 0, byteOffset: 1, buffer: ArrayBuffer { [Uint8Contents]: <00>, byteLength: 1 } } }\n",
    );
}

/// Views over buffers whose byte blocks move (a reallocating `resize`) under
/// `gc_stress` (a collection at every allocation): the views keep their
/// buffers alive and read through the moved block.
#[test]
fn allocation_paths_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 40; i++) {
           const b = new ArrayBuffer(16, { maxByteLength: 64 });
           const v = new DataView(b, i % 8);
           v.setFloat64(0, i + 0.5, i % 2 === 0);
           b.resize(64);
           keep.push(v);
           new ArrayBuffer(100);
         }
         let sum = 0;
         for (let i = 0; i < 40; i++) sum += keep[i].getFloat64(0, i % 2 === 0) + keep[i].byteLength;
         const small = new DataView(new ArrayBuffer(2));
         small.setInt16(0, -2);
         console.log(sum, keep[39].byteOffset, keep[39].byteLength, keep[39].buffer.byteLength);
         console.log(small);",
        "3220 7 57 64\n\
         DataView { byteLength: 2, byteOffset: 0, buffer: ArrayBuffer { [Uint8Contents]: <ff fe>, byteLength: 2 } }\n",
    );
    assert_same_under_gc_stress(
        "class Sub extends DataView {}
         const views = [];
         for (let i = 0; i < 30; i++) {
           const v = new Sub(new ArrayBuffer(8), i % 4);
           v.setUint32(0, i * 1000, true);
           views.push(v);
         }
         let total = 0;
         for (const v of views) total += v.getUint32(0, true);
         console.log(total, views.every((v) => v instanceof Sub), views[5]);",
        "435000 true DataView { byteLength: 7, byteOffset: 1, buffer: ArrayBuffer { [Uint8Contents]: <00 88 13 00 00 00 00 00>, byteLength: 8 } }\n",
    );
}
