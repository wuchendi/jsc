//! Integration tests: the `%TypedArray.prototype%` methods of stage T3b
//! (DESIGN.md §7.9): every method over each number kind, the element
//! conversions of the writing methods, `TypedArraySpeciesCreate` overrides,
//! `set` from typed arrays (overlapping same-buffer copies of the same and of
//! another kind) and array-likes, the default sort order and stability,
//! comparators and callbacks that detach or shrink the buffer, `with` / `at`
//! index edges, `subarray` of length-tracking views, `toString` identity,
//! and the allocating methods under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_same_under_gc_stress, assert_string};

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

/// The nine number kinds.
const KINDS: [&str; 9] = [
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float32Array",
    "Float64Array",
];

/// Every T3b method with its `length`.
const METHODS: [(&str, u32); 27] = [
    ("at", 1),
    ("copyWithin", 2),
    ("every", 1),
    ("fill", 1),
    ("filter", 1),
    ("find", 1),
    ("findIndex", 1),
    ("findLast", 1),
    ("findLastIndex", 1),
    ("forEach", 1),
    ("includes", 1),
    ("indexOf", 1),
    ("join", 1),
    ("lastIndexOf", 1),
    ("map", 1),
    ("reduce", 1),
    ("reduceRight", 1),
    ("reverse", 0),
    ("set", 1),
    ("slice", 2),
    ("some", 1),
    ("sort", 1),
    ("subarray", 2),
    ("toLocaleString", 0),
    ("toReversed", 0),
    ("toSorted", 1),
    ("with", 2),
];

/// Each method is a writable, configurable, non-enumerable own property of
/// `%TypedArray.prototype%` with its spec `name` and `length`; `toString` is
/// `Array.prototype.toString` itself.
#[test]
fn methods_are_installed() {
    let names: Vec<String> = METHODS.iter().map(|(m, _)| format!("'{m}'")).collect();
    let src = with_prelude(&format!(
        "const P = TA.prototype;
         [{}].map((n) => {{
             const d = Object.getOwnPropertyDescriptor(P, n);
             return n + ':' + P[n].length + ':' + P[n].name + ':' + d.writable + d.enumerable +
                 d.configurable;
         }}).join() + '|' + (P.toString === Array.prototype.toString) + ',' +
             Object.getOwnPropertyDescriptor(P, 'toString').enumerable + ',' +
             (Uint8Array.prototype.toString === Array.prototype.toString) + ',' +
             Object.prototype.hasOwnProperty.call(Uint8Array.prototype, 'map')",
        names.join(", ")
    ));
    let expected: Vec<String> = METHODS
        .iter()
        .map(|(m, len)| format!("{m}:{len}:{m}:truefalsetrue"))
        .collect();
    assert_string(
        &src,
        &format!("{}|true,false,true,false", expected.join(",")),
    );
}

/// Every method over each kind on `[1, 2, 3, 4]` (values every kind holds).
#[test]
fn methods_per_kind() {
    for name in KINDS {
        let src = format!(
            "const T = {name};
             const a = new T([1, 2, 3, 4]);
             [a.at(0), a.at(-1), String(a.at(4)), a.includes(3), a.includes(5), a.indexOf(2),
              a.indexOf(2, 2), a.lastIndexOf(4), a.lastIndexOf(1, -2), a.join(), a.join('-'),
              a.every((v) => v > 0), a.some((v) => v > 3), a.find((v) => v > 2),
              a.findIndex((v) => v > 2), a.findLast((v) => v < 3), a.findLastIndex((v) => v < 3),
              a.reduce((x, y) => x + y), a.reduceRight((x, y) => x + '' + y, ''),
              a.map((v) => v * 2).join(), a.filter((v) => v % 2).join(), a.slice(1, 3).join(),
              a.subarray(1, 3).join(), a.toReversed().join(), a.with(1, 9).join(),
              a.toLocaleString(), a.slice(-2).constructor === T,
              a.map((v) => v).constructor === T, a.toSorted((x, y) => y - x).join(), String(a),
              a.copyWithin(0, 2).join(), a.fill(7, 1, 3).join(), a.reverse().join(),
              a.subarray(1).buffer === a.buffer, a.slice().buffer !== a.buffer].join(' ')"
        );
        assert_string(
            &src,
            "1 4 undefined true false 1 -1 3 0 1,2,3,4 1-2-3-4 true true 3 2 2 1 10 4321 \
             2,4,6,8 1,3 2,3 2,3 4,3,2,1 1,9,3,4 1,2,3,4 true true 4,3,2,1 1,2,3,4 3,4,3,4 \
             3,7,7,4 4,7,7,3 true true",
        );
    }
}

/// The writing methods convert by the target kind (`ToNumber`, then the
/// kind's conversion).
#[test]
fn element_conversions() {
    assert_string(
        "[new Uint8Array(2).fill(300).join(), new Int8Array(2).fill(200).join(),
          new Uint8ClampedArray(2).fill(300).join(), new Uint8ClampedArray([1]).with(0, 2.5).join(),
          new Int16Array([1]).with(0, 1.5).join(),
          new Float32Array([0]).with(0, 1.1)[0] === Math.fround(1.1),
          new Uint8Array([1, 2]).map((v) => v * 200).join(),
          new Float64Array([1, 2]).map((v) => v / 4).join(), new Uint8Array(2).fill('3').join(),
          new Int32Array(2).fill({ valueOf() { return 5; } }).join(),
          new Uint16Array([1]).map(() => 'x').join(), new Float64Array([1]).map(() => undefined).join(),
          new Int8Array([127]).map((v) => v + 1).join()].join(' ')",
        "44,44 -56,-56 255,255 2 1 true 200,144 0.25,0.5 3,3 5,5 0 NaN -128",
    );
}

/// The receiver, callback, comparator, index and offset errors.
#[test]
fn argument_errors() {
    assert_string(
        &with_prelude(
            "const P = TA.prototype;
             [err(() => new Uint8Array(1).map()), err(() => new Uint8Array(1).forEach(1)),
              err(() => new Uint8Array(1).every({})), err(() => new Uint8Array(0).filter(null)),
              err(() => new Uint8Array(0).reduce((a, b) => a)),
              err(() => new Uint8Array(0).reduceRight((a, b) => a)),
              new Uint8Array(0).reduce((a, b) => a, 'init'),
              new Uint8Array(0).reduceRight((a, b) => a, undefined) === undefined,
              err(() => new Uint8Array(1).sort(1)), err(() => P.sort.call([], 1)),
              err(() => new Uint8Array(1).toSorted(null)),
              err(() => new Uint8Array(2).with(2, 0)), err(() => new Uint8Array(2).with(-3, 0)),
              new Uint8Array(2).with(-1, 5).join(),
              err(() => new Uint8Array(2).set([1], -1)), err(() => new Uint8Array(2).set([1, 2, 3])),
              err(() => new Uint8Array(2).set([1], Infinity)),
              err(() => new Uint8Array(2).set(new Uint8Array(3))),
              err(() => new Uint8Array(2).set(new Uint8Array(2), 1)),
              err(() => new Uint8Array(2).set(null)), err(() => new Uint8Array(2).set()),
              new Uint8Array(2).set(new Uint8Array(0), 2)].join('\\n')",
        ),
        "TypeError: %TypedArray%.prototype.map callback is not a function\n\
         TypeError: %TypedArray%.prototype.forEach callback is not a function\n\
         TypeError: %TypedArray%.prototype.every callback is not a function\n\
         TypeError: %TypedArray%.prototype.filter callback is not a function\n\
         TypeError: Reduce of empty array with no initial value\n\
         TypeError: Reduce of empty array with no initial value\n\
         init\ntrue\n\
         TypeError: The comparison function must be either a function or undefined\n\
         TypeError: The comparison function must be either a function or undefined\n\
         TypeError: The comparison function must be either a function or undefined\n\
         RangeError: Invalid typed array index\n\
         RangeError: Invalid typed array index\n\
         0,5\n\
         RangeError: offset is out of bounds\n\
         RangeError: offset is out of bounds\n\
         RangeError: offset is out of bounds\n\
         RangeError: offset is out of bounds\n\
         RangeError: offset is out of bounds\n\
         TypeError: Cannot convert undefined or null to object\n\
         TypeError: Cannot convert undefined or null to object\n",
    );
}

/// Every method rejects a receiver that is not a typed array, and every
/// validating method (all but `set` and `subarray`) a detached or
/// out-of-bounds one, naming itself.
#[test]
fn receiver_errors() {
    for (m, _) in METHODS {
        assert_string(
            &with_prelude(&format!("err(() => TA.prototype.{m}.call({{}}, () => 0))")),
            &format!(
                "TypeError: Method %TypedArray%.prototype.{m} called on incompatible receiver"
            ),
        );
        if m == "set" || m == "subarray" {
            continue;
        }
        let detached = format!(
            "TypeError: Cannot perform %TypedArray%.prototype.{m} on a detached or \
             out-of-bounds TypedArray"
        );
        assert_string(
            &with_prelude(&format!(
                "const b = new ArrayBuffer(2), u = new Uint8Array(b);
                 b.transfer();
                 err(() => u.{m}(() => 0))"
            )),
            &detached,
        );
        assert_string(
            &with_prelude(&format!(
                "const rab = new ArrayBuffer(4, {{ maxByteLength: 4 }});
                 const f = new Uint8Array(rab, 0, 4);
                 rab.resize(2);
                 err(() => f.{m}(() => 0))"
            )),
            &detached,
        );
    }
}

/// `set` and `subarray` over detached or out-of-bounds receivers: `set`
/// re-validates after converting the offset; `subarray` never validates
/// (out of bounds is length `0`) and leaves the checks to the constructor.
#[test]
fn set_and_subarray_out_of_bounds() {
    assert_string(
        &with_prelude(
            "const b = new ArrayBuffer(4), u = new Uint8Array(b);
             b.transfer();
             const rab = new ArrayBuffer(4, { maxByteLength: 8 });
             const f = new Uint8Array(rab, 0, 4);
             rab.resize(2);
             [err(() => u.set([1])), err(() => u.set(new Uint8Array(1))),
              err(() => new Uint8Array(1).set(u)), err(() => u.subarray(0)),
              f.subarray(0).length, f.subarray(0, 1).length].join('\\n')",
        ),
        "TypeError: Cannot perform %TypedArray%.prototype.set on a detached or out-of-bounds \
         TypedArray\n\
         TypeError: Cannot perform %TypedArray%.prototype.set on a detached or out-of-bounds \
         TypedArray\n\
         TypeError: Cannot perform %TypedArray%.prototype.set on a detached or out-of-bounds \
         TypedArray\n\
         TypeError: Cannot perform Construct on a detached ArrayBuffer\n\
         0\n0",
    );
}

/// Callbacks and conversions that resize or detach the buffer mid-way: the
/// length is read once, reads past the live bytes are `undefined`, writes
/// there are ignored, and the spec's re-validations throw.
#[test]
fn callbacks_resize_or_detach() {
    assert_string(
        &with_prelude(
            "const out = [];
             const rab = new ArrayBuffer(4, { maxByteLength: 8 });
             const t = new Uint8Array(rab);
             t.set([1, 2, 3, 4]);
             const seen = [];
             t.forEach((v, i) => { if (i === 0) rab.resize(2); seen.push(v); });
             out.push(seen.map(String).join());
             rab.resize(4);
             t.set([1, 2, 3, 4]);
             out.push(t.map((v, i) => { if (i === 1) rab.resize(1); return v * 10; }).join());
             const b = new ArrayBuffer(3), d = new Uint8Array(b);
             d.set([5, 6, 7]);
             out.push(d.findIndex((v, i) => { if (i === 0) b.transfer(); return v === undefined; }));
             const b2 = new ArrayBuffer(2), e = new Uint8Array(b2);
             out.push(e.includes(undefined, { valueOf() { b2.transfer(); return 0; } }));
             const b3 = new ArrayBuffer(2), g = new Uint8Array(b3);
             out.push(g.indexOf(0, { valueOf() { b3.transfer(); return 0; } }));
             const b4 = new ArrayBuffer(2), h = new Uint8Array(b4);
             out.push(err(() => h.fill({ valueOf() { b4.transfer(); return 1; } })));
             const b5 = new ArrayBuffer(2), j = new Uint8Array(b5);
             out.push(j.join({ toString() { b5.transfer(); return '-'; } }));
             const b6 = new ArrayBuffer(2), q = new Uint8Array(b6);
             out.push(String(q.at({ valueOf() { b6.transfer(); return 0; } })));
             const rab2 = new ArrayBuffer(2, { maxByteLength: 8 }), lt = new Uint8Array(rab2);
             let count = 0;
             lt.forEach(() => { rab2.resize(8); count++; });
             out.push(count, lt.length);
             const b7 = new ArrayBuffer(3), s = new Uint8Array(b7);
             s.set([1, 2, 3]);
             out.push(s.reduce((acc, v) => { if (!b7.detached) b7.transfer(); return acc + ',' + v; }));
             const b8 = new ArrayBuffer(2), w = new Uint8Array(b8);
             out.push(err(() => w.slice({ valueOf() { b8.transfer(); return 0; } })));
             const b9 = new ArrayBuffer(2), x = new Uint8Array(b9);
             out.push(err(() => x.copyWithin(0, { valueOf() { b9.transfer(); return 1; } })));
             const b10 = new ArrayBuffer(2), y = new Uint8Array(b10);
             out.push(y.lastIndexOf(0, { valueOf() { b10.transfer(); return 1; } }));
             out.join('|')",
        ),
        "1,2,undefined,undefined|10,20,0,0|1|true|-1|\
         TypeError: Cannot perform %TypedArray%.prototype.fill on a detached or out-of-bounds \
         TypedArray|-|undefined|2|8|1,2,undefined|\
         TypeError: Cannot perform %TypedArray%.prototype.slice on a detached or out-of-bounds \
         TypedArray|\
         TypeError: Cannot perform %TypedArray%.prototype.copyWithin on a detached or \
         out-of-bounds TypedArray|-1",
    );
}

/// `TypedArraySpeciesCreate`: subclass instances, `constructor` /
/// `@@species` overrides, the default for `undefined` / `null`, the
/// arguments passed, and the result checks; `toReversed` / `toSorted` /
/// `with` always create the receiver's kind.
#[test]
fn species_overrides() {
    assert_string(
        &with_prelude(
            "class Sub extends Uint8Array {}
             const s = new Sub([1, 2, 3]);
             const out = [s.map((v) => v).constructor === Sub, s.filter(() => true) instanceof Sub,
                 s.slice(1) instanceof Sub, s.subarray(1) instanceof Sub,
                 s.toReversed().constructor === Uint8Array, s.toSorted().constructor === Uint8Array,
                 s.with(0, 1).constructor === Uint8Array];
             const a = new Uint8Array([1, 2, 3]);
             a.constructor = { [Symbol.species]: Int16Array };
             out.push(a.map((v) => v * 100).join(), a.slice(0) instanceof Int16Array,
                 a.slice(1).join(), a.filter((v) => v > 1).join());
             a.constructor = undefined;
             out.push(a.map((v) => v).constructor === Uint8Array);
             a.constructor = { [Symbol.species]: null };
             out.push(a.slice().constructor === Uint8Array);
             a.constructor = 1;
             out.push(err(() => a.map((v) => v)));
             a.constructor = { [Symbol.species]: 1 };
             out.push(err(() => a.slice()));
             a.constructor = { [Symbol.species]: function () { return {}; } };
             out.push(err(() => a.filter(() => true)));
             a.constructor = { [Symbol.species]: function () { return new Uint8Array(1); } };
             out.push(err(() => a.map((v) => v)), a.filter(() => false).length,
                 a.subarray(0, 3).length);
             const shrunk = new ArrayBuffer(4, { maxByteLength: 4 });
             a.constructor = { [Symbol.species]: function (n) {
                 const r = new Uint8Array(shrunk, 0, 4);
                 shrunk.resize(2);
                 return r;
             } };
             out.push(err(() => a.slice()));
             let args;
             a.constructor = { [Symbol.species]: function (...x) {
                 args = x;
                 return new Uint8Array(...x);
             } };
             a.subarray(1, 2);
             out.push(args.length, args[0] === a.buffer, args[1], args[2]);
             a.slice(1);
             out.push(args.join());
             out.join('\\n')",
        ),
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n100,200,300\ntrue\n2,3\n2,3\ntrue\ntrue\n\
         TypeError: object.constructor is not an object\n\
         TypeError: object.constructor[Symbol.species] is not a constructor\n\
         TypeError: The TypedArray constructor did not return a TypedArray\n\
         TypeError: The TypedArray constructor returned a too short TypedArray\n1\n1\n\
         TypeError: The TypedArray constructor returned a detached or out-of-bounds TypedArray\n\
         3\ntrue\n1\n1\n2",
    );
}

/// `slice` copies bytes when the result has the receiver's kind (also into
/// a species result viewing the same buffer) and converts otherwise.
#[test]
fn slice_byte_copy_and_conversion() {
    assert_string(
        "const f = new Float64Array([NaN, -0, 1.5]);
         const g = f.slice();
         const b = new ArrayBuffer(6), u = new Uint8Array(b);
         u.set([1, 2, 3, 4, 5, 6]);
         u.constructor = { [Symbol.species]: function (n) { return new Uint8Array(b, 2, n); } };
         const s = u.slice(0, 4);
         const i = new Int16Array([-1, 300]);
         i.constructor = { [Symbol.species]: Uint8Array };
         [Object.is(g[1], -0), Number.isNaN(g[0]), g[2], s.join(), u.join(), i.slice().join()]
             .join(' ')",
        "true true 1.5 1,2,1,2 1,2,1,2,1,2 255,44",
    );
}

/// `set` from typed arrays: same-kind overlap in either direction (as if
/// the source were cloned first), another kind over the same buffer
/// through a temporary, conversions, and array-like sources.
#[test]
fn set_cases() {
    assert_string(
        "const out = [];
         const buf = new ArrayBuffer(8), u8 = new Uint8Array(buf);
         const all = [1, 2, 3, 4, 5, 6, 7, 8];
         u8.set(all);
         u8.set(u8.subarray(0, 4), 2);
         out.push(u8.join());
         u8.set(all);
         u8.set(u8.subarray(2, 6), 0);
         out.push(u8.join());
         u8.set(all);
         new Uint16Array(buf).set(u8.subarray(0, 4));
         out.push(u8.join());
         u8.set(all);
         new Uint8Array(buf, 4).set(new Uint16Array(buf, 0, 2));
         out.push(u8.join());
         const i8 = new Int8Array(3);
         i8.set(new Float64Array([1.5, -1.5, 300]));
         out.push(i8.join());
         const t = new Uint8Array(4);
         t.set({ length: 2, 0: 7, 1: '8' }, 2);
         out.push(t.join());
         t.set('12');
         out.push(t.join(), String(t.set([])));
         const order = [];
         t.set({ length: 2, get 0() { order.push('g0'); return { valueOf() { order.push('v0'); return 9; } }; },
                 get 1() { order.push('g1'); return 9; } });
         out.push(order.join(), t.join());
         const b = new ArrayBuffer(2), d = new Uint8Array(b);
         out.push(String(d.set({ length: 2, get 0() { b.transfer(); return 1; }, 1: 2 })), d.length);
         const f = new Float32Array(2);
         f.set([1.1, 'x']);
         out.push(f[0] === Math.fround(1.1), Number.isNaN(f[1]));
         out.join(' ')",
        "1,2,1,2,3,4,7,8 3,4,5,6,5,6,7,8 1,0,2,0,3,0,4,0 1,2,3,4,1,3,7,8 1,-1,44 0,0,7,8 \
         1,2,7,8 undefined g0,v0,g1 9,9,7,8 undefined 0 true true",
    );
}

/// The default order (numeric, `-0` before `+0`, `NaN` last), stability
/// under a comparator, comparator results through `ToNumber` (`NaN` is
/// `0`), and `toSorted` leaving the receiver alone.
#[test]
fn sort_order_and_stability() {
    assert_string(
        "const f = new Float64Array([3, NaN, -0, 0, -Infinity, 1, -0, NaN, Infinity]);
         f.sort();
         const show = (t) => Array.from(t, (v) => Object.is(v, -0) ? '-0' : String(v)).join();
         const o = new Uint8Array([3, 1, 2]);
         const s = o.toSorted();
         const g = new Float32Array([0, -0, NaN, -1]);
         [show(f), new Int8Array([5, -1, 3, -128, 127]).sort().join(),
          new Uint32Array([4294967295, 1, 2147483648]).sort().join(),
          new Uint8Array([10, 9, 1]).sort().join(),
          new Uint8Array([23, 12, 21, 13, 22, 11]).sort((a, b) => Math.floor(a / 10) - Math.floor(b / 10))
              .join(),
          new Int16Array([3, 1, 2]).sort((a, b) => ({ valueOf() { return a - b; } })).join(),
          new Uint8Array([3, 1, 2]).sort(() => NaN).join(), o.join(), s.join(), show(g.toSorted()),
          show(g), new Uint8Array(0).sort().length, new Float64Array([2, 1]).sort(undefined).join(),
          o.sort() === o].join(' ')",
        "-Infinity,-0,-0,0,1,3,Infinity,NaN,NaN -128,-1,3,5,127 1,2147483648,4294967295 1,9,10 \
         12,13,11,23,21,22 1,2,3 3,1,2 3,1,2 1,2,3 -1,-0,0,NaN 0,-0,NaN,-1 0 1,2 true",
    );
}

/// A comparator that detaches or shrinks the buffer: the sort finishes on
/// its private copy and the write-back skips the indices that are gone.
#[test]
fn comparator_detaches_or_shrinks() {
    assert_string(
        "const b = new ArrayBuffer(4), d = new Uint8Array(b);
         d.set([4, 3, 2, 1]);
         let calls = 0;
         const res = d.sort((x, y) => { if (calls++ === 0) b.transfer(); return x - y; });
         const rab = new ArrayBuffer(4, { maxByteLength: 4 }), lt = new Uint8Array(rab);
         lt.set([4, 3, 2, 1]);
         let first = true;
         lt.sort((x, y) => { if (first) { first = false; rab.resize(2); } return x - y; });
         const b2 = new ArrayBuffer(3), e = new Uint8Array(b2);
         e.set([3, 1, 2]);
         const ts = e.toSorted((x, y) => { if (!b2.detached) b2.transfer(); return x - y; });
         const seen = [];
         new Uint8Array([2, 1]).sort((x, y) => { seen.push(typeof x, typeof y); return x - y; });
         [res === d, d.length, calls > 1, lt.join(), ts.join(), e.length, seen.join()].join(' ')",
        "true 0 true 1,2 1,2,3 0 number,number",
    );
}

/// `with` and `at`: relative indices, truncation, `ToNumber(value)` before
/// the index check, and the live length deciding the check.
#[test]
fn with_and_at_edges() {
    assert_string(
        &with_prelude(
            "const t = new Int8Array([1, 2, 3]);
             const out = [String(t.at(3)), String(t.at(-4)), t.at(-3), t.at(1.9), t.at('2'),
                 t.at(NaN), t.at(-0), String(t.at(Infinity)), t.with(-1, 7).join(),
                 t.with(0.5, 7).join(), t.with('1', '8').join(), err(() => t.with(3, 1)),
                 err(() => t.with(-4, 1)), err(() => t.with(Infinity, 1)), t.join()];
             const log = [];
             out.push(err(() => t.with(5, { valueOf() { log.push('v'); return 0; } })), log.join());
             const rab = new ArrayBuffer(3, { maxByteLength: 3 }), lt = new Uint8Array(rab);
             out.push(err(() => lt.with(2, { valueOf() { rab.resize(1); return 0; } })));
             rab.resize(3);
             lt.set([1, 2, 3]);
             out.push(lt.with(0, { valueOf() { rab.resize(1); return 9; } }).join());
             out.push(new Float64Array([1, 2]).with(1, undefined).join());
             out.join(' ')",
        ),
        "undefined undefined 1 2 3 1 1 undefined 1,2,7 7,2,3 1,8,3 \
         RangeError: Invalid typed array index RangeError: Invalid typed array index \
         RangeError: Invalid typed array index 1,2,3 RangeError: Invalid typed array index v \
         RangeError: Invalid typed array index 9,0,0 1,NaN",
    );
}

/// `subarray` of length-tracking and fixed views: a length-tracking source
/// with `end` `undefined` gives a length-tracking result; anything else a
/// fixed one; offsets in elements of the kind.
#[test]
fn subarray_of_length_tracking_views() {
    assert_string(
        "const rab = new ArrayBuffer(8, { maxByteLength: 16 });
         const lt = new Uint16Array(rab, 2);
         const s1 = lt.subarray(1), s2 = lt.subarray(1, 2), s3 = lt.subarray(-2, undefined);
         const fixed = new Uint16Array(rab, 0, 2), s4 = fixed.subarray(1);
         const before = [lt.length, s1.length, s1.byteOffset, s2.length, s3.length, s3.byteOffset,
             s4.length, s4.byteOffset];
         rab.resize(16);
         const after = [lt.length, s1.length, s2.length, s3.length, s4.length];
         rab.resize(4);
         const small = [lt.length, s1.length, s2.length, s3.length];
         [before.join(), after.join(), small.join(), lt.subarray(5).length,
          new Uint8Array([1, 2, 3]).subarray(-1).join(), new Uint8Array([1, 2, 3]).subarray(2, 1).length]
             .join(' ')",
        "3,2,4,1,2,4,1,2 7,6,1,6,1 1,0,0,0 0 3 0",
    );
}

/// Callback arguments and `thisArg`, `reduce` / `reduceRight` folding
/// order, and `copyWithin` cases (incl. a shrinking conversion).
#[test]
fn callback_arguments_and_copy_within() {
    assert_string(
        "const t = new Uint8Array([5, 6]);
         const log = [];
         t.forEach(function (v, i, o) { log.push(v + ':' + i + ':' + (o === t) + ':' + this.x); },
             { x: 'X' });
         const mk = () => new Uint8Array([1, 2, 3, 4, 5]);
         const rab = new ArrayBuffer(4, { maxByteLength: 4 }), lt = new Uint8Array(rab);
         lt.set([1, 2, 3, 4]);
         const shrunk = lt.copyWithin(0, { valueOf() { rab.resize(3); return 2; } }).join();
         [log.join(), t.reduce((acc, v, i, o) => acc + '|' + v + i + (o === t), 's'),
          t.reduceRight((acc, v, i) => acc + ',' + v + i), t.findLast(() => true),
          t.findLastIndex(() => true), String(t.find(() => false)), t.findIndex(() => false),
          mk().copyWithin(0, 3).join(), mk().copyWithin(1, 0, 3).join(),
          mk().copyWithin(-2, 0).join(), mk().copyWithin(0, -2, -1).join(),
          mk().copyWithin(2, 2).join(), new Int16Array([1, 2, 3, 4]).copyWithin(0, 2).join(),
          shrunk].join(' ')",
        "5:0:true:X,6:1:true:X s|50true|61true 6,50 6 1 undefined -1 4,5,3,4,5 1,1,2,3,5 \
         1,2,3,1,2 4,2,3,4,5 1,2,3,4,5 3,4,3,4 3,2,3",
    );
}

/// `includes` uses `SameValueZero` (`NaN` found, `-0` equals `+0`),
/// `indexOf` / `lastIndexOf` strict equality (`NaN` never found), with the
/// `fromIndex` rules; `join` / `toLocaleString` / `toString` render.
#[test]
fn search_and_join() {
    assert_string(
        "const f = new Float64Array([1, NaN, -0, 2]);
         const n = Number.prototype.toLocaleString;
         Number.prototype.toLocaleString = function () { return '<' + this + '>'; };
         const loc = new Uint8Array([1, 2]).toLocaleString();
         Number.prototype.toLocaleString = n;
         [f.includes(NaN), f.includes(0), f.includes(2, -1), f.includes(1, 1), f.includes(1, -Infinity),
          f.includes(2, Infinity), f.includes('2'), f.indexOf(NaN), f.indexOf(0), f.indexOf(2, -1),
          f.indexOf(1, 10), f.indexOf('1'), f.lastIndexOf(1), f.lastIndexOf(2, -2),
          f.lastIndexOf(1, -Infinity), f.lastIndexOf(2, 10), f.lastIndexOf(1, undefined),
          f.join(), f.join(undefined), f.join(''), new Uint8Array(0).join(), loc,
          new Int8Array([-1, 2]).toString(), new Uint8Array(0).indexOf(0),
          new Uint8Array(0).includes(undefined)].join(' ')",
        "true true true false true false false -1 2 3 -1 -1 0 -1 -1 3 0 1,NaN,0,2 1,NaN,0,2 \
         1NaN02  <1>,<2> -1,2 -1 false",
    );
}

/// The allocating methods (`map`, `filter`, `slice`, `subarray`, `toSorted`,
/// `toReversed`, `with`, `set`, species subclasses, `sort` with a
/// comparator, `join` / `toLocaleString`) under `gc_stress`.
#[test]
fn allocating_methods_under_gc_stress() {
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 30; i++) {
           const t = new Uint8Array([i, i + 1, i + 2, 3]);
           const sub = t.subarray(1);
           keep.push(t.map((v) => v * 2), t.filter((v) => v % 2), t.slice(1, 3), sub, t.toSorted(),
             t.toReversed(), t.with(0, 9));
           const f = new Float64Array(4);
           f.set(t);
           f.set([i], 3);
           keep.push(f);
         }
         let s = 0;
         for (const x of keep) s += x.length;
         console.log(s, keep.length, keep[232].join('|'), keep[233].join('|'), keep[234].join('|'),
           keep[235].join('|'), keep[236].join('|'), keep[237].join('|'), keep[238].join('|'),
           keep[239].join('|'));",
        "825 240 58|60|62|6 29|31|3 30|31 30|31|3 3|29|30|31 3|31|30|29 9|30|31|3 29|30|31|29\n",
    );
    assert_same_under_gc_stress(
        "class S extends Int16Array {}
         const out = [];
         for (let i = 0; i < 25; i++) {
           const s = new S([i, -i, 2 * i]);
           out.push(s.map((v) => v + 1), s.slice(0, 2), s.subarray(1), s.filter(() => true));
           out.push(s.sort((a, b) => b - a).join(), s.toLocaleString());
         }
         console.log(out.length, out[144] instanceof S, out[145] instanceof S, out[146] instanceof S,
           out[147] instanceof S, out[144].join('|'), out[146].join('|'), out[148], out[149]);",
        "150 true true true true 25|-23|49 24|-24 48,24,-24 48,24,-24\n",
    );
}
