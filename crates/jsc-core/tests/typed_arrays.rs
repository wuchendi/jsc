//! Integration tests: the typed-array core (DESIGN.md §5.8): every
//! constructor overload per kind, the element conversions, the
//! integer-indexed exotic object hooks (§5.8.5), length-tracking views over
//! resizable buffers with grow / shrink / out-of-bounds and detach,
//! `%TypedArray%` and its statics `from` / `of` / `@@species`, the
//! prototype getters, iteration, rendering, and the allocation-heavy paths
//! under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_same_under_gc_stress, assert_string};

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

/// The nine number kinds: `(constructor, element size)`.
const KINDS: [(&str, u32); 9] = [
    ("Int8Array", 1),
    ("Uint8Array", 1),
    ("Uint8ClampedArray", 1),
    ("Int16Array", 2),
    ("Uint16Array", 2),
    ("Int32Array", 4),
    ("Uint32Array", 4),
    ("Float32Array", 4),
    ("Float64Array", 8),
];

/// Every overload of each constructor: no argument, a length, an array (an
/// iterable), a typed array, an `ArrayBuffer` with offset and length, a whole
/// buffer, an array-like, a `Set`; plus the constructor's own facts and the
/// prototype chain.
#[test]
fn constructor_overloads_per_kind() {
    for (name, size) in KINDS {
        let src = format!(
            "const T = {name};
             const a = new T(), b = new T(3), c = new T([1, 2, 3]), d = new T(c);
             const buf = new ArrayBuffer(4 * {size});
             const e = new T(buf, {size}, 2), f = new T(buf), u = new T(undefined);
             const g = new T({{ length: 2, 0: 5, 1: '6' }}), h = new T(new Set([7, 8]));
             const i = new T(new Uint8Array([9, 10])), s = new T('2');
             [a.length, b.length, b[0], [...c].join('|'), [...d].join('|'), d.buffer !== c.buffer,
              e.length, e.byteOffset, e.byteLength, f.length, e.buffer === buf, u.length,
              [...g].join('|'), [...h].join('|'), [...i].join('|'), s.length,
              T.BYTES_PER_ELEMENT, T.prototype.BYTES_PER_ELEMENT, T.length, T.name,
              Object.getPrototypeOf(T) === TA, Object.getPrototypeOf(T.prototype) === TA.prototype,
              T.prototype.constructor === T, a instanceof T, Object.prototype.toString.call(b),
              b.buffer.byteLength, typeof b, Array.isArray(b), ArrayBuffer.isView(b)].join()"
        );
        let expected = format!(
            "0,3,0,1|2|3,1|2|3,true,2,{size},{},4,true,0,5|6,7|8,9|10,2,{size},{size},3,{name},\
             true,true,true,true,[object {name}],{},object,false,true",
            2 * size,
            3 * size,
        );
        assert_string(&with_prelude(&src), &expected);
    }
}

/// The constructors' errors, in spec order: called without `new`, the
/// length overload's `ToIndex` and allocation limit, the buffer overload's
/// offset, alignment, length and detach checks, an out-of-bounds source,
/// and the abstract `%TypedArray%`.
#[test]
fn constructor_errors() {
    assert_string(
        &with_prelude(
            "const d = new ArrayBuffer(4); d.transfer();
             const rab = new ArrayBuffer(4, { maxByteLength: 8 });
             const view = new Uint8Array(rab, 2, 2); rab.resize(2);
             [err(() => Uint8Array(1)),
              err(() => new Uint8Array(-1)),
              err(() => new Uint8Array(2 ** 53)),
              err(() => new Float64Array(2 ** 27)),
              err(() => new Int16Array(new ArrayBuffer(4), 1)),
              err(() => new Int16Array(new ArrayBuffer(4), 1, -1)),
              err(() => new Int16Array(new ArrayBuffer(3))),
              err(() => new Uint8Array(new ArrayBuffer(4), 5)),
              err(() => new Uint8Array(new ArrayBuffer(4), -1)),
              err(() => new Uint8Array(new ArrayBuffer(4), 2, 3)),
              err(() => new Uint8Array(new ArrayBuffer(4), 0, -1)),
              err(() => new Uint8Array(new ArrayBuffer(4, { maxByteLength: 8 }), 5)),
              err(() => new Uint8Array(d)),
              err(() => new Uint8Array(view)),
              err(() => TA()),
              err(() => new TA()),
              err(() => new Uint8Array(new ArrayBuffer(4), 1.5).byteOffset)].join('\\n')",
        ),
        "TypeError: Constructor Uint8Array requires 'new'\n\
         RangeError: Invalid typed array length\n\
         RangeError: Invalid typed array length\n\
         RangeError: Invalid typed array length\n\
         RangeError: Start offset of Int16Array should be a multiple of 2\n\
         RangeError: Start offset of Int16Array should be a multiple of 2\n\
         RangeError: Byte length of Int16Array should be a multiple of 2\n\
         RangeError: Start offset is outside the bounds of the buffer\n\
         RangeError: Start offset is outside the bounds of the buffer\n\
         RangeError: Invalid typed array length\n\
         RangeError: Invalid typed array length\n\
         RangeError: Start offset is outside the bounds of the buffer\n\
         TypeError: Cannot perform Construct on a detached ArrayBuffer\n\
         TypeError: Cannot perform Construct on a detached or out-of-bounds TypedArray\n\
         TypeError: Abstract class TypedArray not directly constructable\n\
         TypeError: Abstract class TypedArray not directly constructable\n\
         none",
    );
}

/// `GetPrototypeFromConstructor` and subclassing: a subclass instance has
/// the subclass's prototype and the kind's behaviour.
#[test]
fn subclassing() {
    assert_string(
        "class S extends Uint8Array {}
         const s = new S([1, 300]), t = new S(2), o = Object.create(S.prototype);
         [s instanceof S, s instanceof Uint8Array, Object.getPrototypeOf(s) === S.prototype,
          s.constructor === S, [...s].join('|'), t.length, s.length, s[1],
          Object.prototype.toString.call(s), o instanceof Uint8Array].join()",
        "true,true,true,true,1|44,2,2,44,[object Uint8Array],true",
    );
}

/// The stored value of each test value per kind: `ToInt8` .. `ToUint32`
/// (modulo reduction of the `ToInt32` pattern), `ToUint8Clamp` (clamping,
/// round half to even, `NaN` to `0`), `Float32` rounding and `Float64`
/// identity (`-0` and `NaN` kept), as in test262's `byteConversionValues`.
#[test]
fn element_conversions_per_kind() {
    let values = "[NaN, 0, -0, Infinity, -Infinity, 2 ** 31, -1, 255.5, 256, 1.5, -1.5, 2.5, 300, \
                  65539, 2 ** 32 + 5]";
    let expected = [
        ("Int8Array", "0,0,0,0,0,0,-1,-1,0,1,-1,2,44,3,5"),
        ("Uint8Array", "0,0,0,0,0,0,255,255,0,1,255,2,44,3,5"),
        (
            "Uint8ClampedArray",
            "0,0,0,255,0,255,0,255,255,2,0,2,255,255,255",
        ),
        ("Int16Array", "0,0,0,0,0,0,-1,255,256,1,-1,2,300,3,5"),
        ("Uint16Array", "0,0,0,0,0,0,65535,255,256,1,65535,2,300,3,5"),
        (
            "Int32Array",
            "0,0,0,0,0,-2147483648,-1,255,256,1,-1,2,300,65539,5",
        ),
        (
            "Uint32Array",
            "0,0,0,0,0,2147483648,4294967295,255,256,1,4294967295,2,300,65539,5",
        ),
        (
            "Float32Array",
            "NaN,0,-0,Infinity,-Infinity,2147483648,-1,255.5,256,1.5,-1.5,2.5,300,65539,4294967296",
        ),
        (
            "Float64Array",
            "NaN,0,-0,Infinity,-Infinity,2147483648,-1,255.5,256,1.5,-1.5,2.5,300,65539,4294967301",
        ),
    ];
    for (name, stored) in expected {
        // Through the array-like overload and through element stores.
        let src = format!(
            "const show = (x) => Object.is(x, -0) ? '-0' : String(x);
             const v = {values};
             const a = new {name}(v), b = new {name}(v.length);
             for (let i = 0; i < v.length; i++) b[i] = v[i];
             const sa = [...a].map(show).join(), sb = [...b].map(show).join();
             sa === sb ? sa : sa + ' / ' + sb"
        );
        assert_string(&src, stored);
    }
    assert_string(
        "const f = new Float32Array([0.1, 3.4028235677973366e38, 1e-46, -1e-46]);
         const c = new Uint8ClampedArray([0.5, 1.5, 254.5, 253.5, -0.5]);
         [f[0], f[1], f[2], Object.is(f[3], -0), [...c].join('|'),
          new Uint8Array([{ valueOf() { return 513; } }])[0],
          new Int8Array(['0x7f', '200', ' 12 ', 'x'])[1]].join()",
        "0.10000000149011612,Infinity,0,true,0|2|254|254|0,1,-56",
    );
}

/// The §5.8.5 key cases on reads: a canonical numeric key never reaches the
/// chain (`"-0"`, `"1.5"`, `"Infinity"`, `"NaN"`, an out-of-range index, a
/// negative or fractional number), while a non-canonical one (`"1.0"`,
/// `"1e3"`, `"01"`, `"+1"`) and any other string or symbol is ordinary.
#[test]
fn numeric_keys_read() {
    assert_string(
        "const t = new Uint8Array([10, 20, 30]);
         for (const k of ['-0', '5', '1.5', '1.0', '1e3', '-1', 'Infinity', 'NaN', '01', '+1',
                          '4294967295', 'foo']) Object.prototype[k] = 'P' + k;
         const s = Symbol('s'); Object.prototype[s] = 'S';
         const show = (x) => x === undefined ? 'u' : String(x);
         [t['-0'], t[5], t['1.5'], t['1.0'], t['1e3'], t[-1], t['-1'], t[1.5], t['Infinity'],
          t['NaN'], t[2 ** 32], t['4294967295'], t[4294967295], t['01'], t['+1'], t.foo, t[s],
          t['1'], t[1], t[-0], t[NaN], t[Infinity]].map(show).join()",
        "u,u,u,P1.0,P1e3,u,u,u,u,u,u,u,u,P01,P+1,Pfoo,S,20,20,10,u,u",
    );
    assert_string(
        "const t = new Uint8Array([10, 20, 30]);
         for (const k of ['-0', '5', '1.5', '1.0']) Object.prototype[k] = 1;
         ['-0' in t, 5 in t, 2 in t, '1.5' in t, '1.0' in t, 'foo' in t, -0 in t, 3 in t,
          t.hasOwnProperty(0), t.hasOwnProperty('-0'), t.hasOwnProperty('length'),
          'length' in t].join()",
        "false,false,true,false,true,false,true,false,true,false,false,true",
    );
}

/// The §5.8.5 key cases on writes: a numeric key on the receiver is
/// `TypedArraySetElement` (`ToNumber` first, even for an invalid index,
/// which then stores nothing and creates nothing), a non-canonical key is
/// an ordinary property, and an inherited setter of a numeric key never
/// runs.
#[test]
fn numeric_keys_write() {
    assert_string(
        "const t = new Uint8Array(3);
         let calls = 0;
         const v = { valueOf() { calls++; return 7; } };
         Object.defineProperty(Object.prototype, '1', { set() { calls += 100; }, configurable: true });
         t[10] = v; t['-0'] = v; t['1.5'] = v; t[-1] = v; t[1] = v; t['2'] = v; t[0.5] = v;
         t['1.0'] = 6; t.x = 1;
         delete Object.prototype[1];
         [calls, [...t].join('|'), Object.keys(t).join('|'), t['1.0'], t.length,
          t.hasOwnProperty('-0'), t.hasOwnProperty('10')].join()",
        "7,0|7|7,0|1|2|1.0|x,6,3,false,false",
    );
}

/// A typed array used as a prototype answers its own indices; a store
/// through it creates the receiver's own property for a valid index and
/// nothing for an invalid one, also for an array receiver's holes.
#[test]
fn typed_array_as_prototype() {
    assert_string(
        "const proto = new Uint8Array([1, 2]);
         const o = Object.create(proto);
         const r = [o[0], o[1], o[5], '0' in o, '5' in o, o['-0']];
         o[0] = 9; o[5] = 9; o['-0'] = 9;
         r.push(Object.keys(o).join('|'), proto[0], o[0]);
         const arr = [];
         Object.setPrototypeOf(arr, new Uint8Array(2));
         arr[5] = 1; arr[1] = 3;
         const holes = [, 'x'];
         Object.setPrototypeOf(holes, new Uint8Array([7]));
         r.push(arr.length, arr[1], arr[5], holes[0]);
         r.map((x) => x === undefined ? 'u' : String(x)).join()",
        "1,2,u,true,false,u,0,1,9,2,3,u,7",
    );
}

/// `[[GetOwnProperty]]` / `[[DefineOwnProperty]]` / `[[Delete]]`: a valid
/// index is a writable, enumerable, configurable data property that cannot
/// be made otherwise; an invalid numeric key is absent, cannot be defined,
/// and deletes trivially.
#[test]
fn descriptors_define_delete() {
    assert_string(
        &with_prelude(
            "const t = new Int8Array(2);
             const d = Object.getOwnPropertyDescriptor(t, 0);
             const r = [d.value, d.writable, d.enumerable, d.configurable,
                        Object.getOwnPropertyDescriptor(t, 2), Object.getOwnPropertyDescriptor(t, '-0'),
                        Object.getOwnPropertyDescriptor(t, 'length')];
             Object.defineProperty(t, 0, { value: 5 });
             Object.defineProperty(t, '1', { value: 300, writable: true, enumerable: true, configurable: true });
             r.push(t[0], t[1],
                    err(() => Object.defineProperty(t, 0, { value: 1, configurable: false })),
                    err(() => Object.defineProperty(t, 0, { value: 1, enumerable: false })),
                    err(() => Object.defineProperty(t, 0, { value: 1, writable: false })),
                    err(() => Object.defineProperty(t, 0, { get() { return 1; } })),
                    err(() => Object.defineProperty(t, 2, { value: 1 })),
                    err(() => Object.defineProperty(t, '-0', { value: 1 })),
                    t[0], Object.defineProperty(t, 0, {}) === t, t[0],
                    Object.defineProperty(t, '1.0', { value: 2, enumerable: true })['1.0']);
             t.y = 1;
             r.push(err(() => { delete t[0]; }), delete t[5], delete t['-0'], delete t.y, t.y,
                    Object.propertyIsEnumerable.call(t, 0), t.propertyIsEnumerable(3));
             r.map((x) => x === undefined ? 'u' : String(x)).join('\\n')",
        ),
        "0\ntrue\ntrue\ntrue\nu\nu\nu\n5\n44\n\
         TypeError: Cannot redefine property: 0\n\
         TypeError: Cannot redefine property: 0\n\
         TypeError: Cannot redefine property: 0\n\
         TypeError: Cannot redefine property: 0\n\
         TypeError: Cannot redefine property: 2\n\
         TypeError: Cannot redefine property: -0\n\
         5\ntrue\n5\n2\n\
         TypeError: Cannot delete property '0' of object\n\
         true\ntrue\ntrue\nu\ntrue\nfalse",
    );
}

/// `[[OwnPropertyKeys]]`: the current indices ascending, then the string
/// keys in insertion order, then the symbols; `Object.keys` / `values` /
/// `entries`, `getOwnPropertyNames`, `for-in`, spread and `JSON.stringify`
/// all see them.
#[test]
fn own_keys_and_enumeration() {
    assert_string(
        "const t = new Uint8Array([1, 2]);
         t.b = 1; t.a = 2;
         const s = Symbol('s'); t[s] = 3;
         const fi = []; for (const k in t) fi.push(k);
         const proto = Object.create(new Uint8Array([5, 6, 7]));
         proto.z = 1;
         const inherited = []; for (const k in proto) inherited.push(k);
         [Object.keys(t).join('|'), Object.getOwnPropertyNames(t).join('|'),
          Object.getOwnPropertySymbols(t).length, fi.join('|'), Object.values(t).join('|'),
          Object.entries(t).map((e) => e.join(':')).join('|'), JSON.stringify(t),
          Object.keys({ ...t }).join('|'), Object.keys(Object.assign({}, t)).join('|'),
          inherited.join('|'), Object.keys(new Float64Array(0)).length].join()",
        "0|1|b|a,0|1|b|a,1,0|1|b|a,1|2|1|2,0:1|1:2|b:1|a:2,\
         {\"0\":1,\"1\":2,\"b\":1,\"a\":2},0|1|b|a,0|1|b|a,z|0|1|2,0",
    );
}

/// Extensibility and integrity: a variable-length view cannot be made
/// non-extensible, one with elements cannot be sealed or frozen (it is left
/// non-extensible), and it is neither sealed nor frozen while it has any.
#[test]
fn integrity_levels() {
    assert_string(
        &with_prelude(
            "const one = new Uint8Array(1);
             const rab = new ArrayBuffer(2, { maxByteLength: 4 });
             [err(() => Object.freeze(one)), Object.isExtensible(one),
              err(() => Object.seal(new Uint8Array(1))),
              Object.isFrozen(Object.freeze(new Uint8Array(0))),
              err(() => Object.preventExtensions(new Uint8Array(rab))),
              err(() => Object.preventExtensions(new Uint8Array(rab, 0, 1))),
              Object.isExtensible(new Uint8Array(rab)),
              Object.isExtensible(Object.preventExtensions(new Uint8Array(2))),
              Object.isSealed(Object.preventExtensions(new Uint8Array(2))),
              Object.isFrozen(Object.preventExtensions(new Uint8Array(2))),
              Object.isSealed(Object.preventExtensions(new Uint8Array(0)))].join('\\n')",
        ),
        "TypeError: Cannot freeze array buffer views with elements\nfalse\n\
         TypeError: Cannot seal array buffer views with elements\ntrue\n\
         TypeError: Cannot prevent extensions on a variable-length TypedArray\n\
         TypeError: Cannot prevent extensions on a variable-length TypedArray\n\
         true\nfalse\nfalse\nfalse\ntrue",
    );
}

/// Length-tracking and fixed views over a resizable buffer through grow,
/// shrink (out of bounds and back), and detach: lengths, byte lengths and
/// offsets from the buffer's current length (`0` out of bounds), element
/// reads `undefined` and writes ignored outside the live bytes, and zeroed
/// bytes after a shrink.
#[test]
fn resize_and_detach_matrix() {
    assert_string(
        "const rab = new ArrayBuffer(8, { maxByteLength: 16 });
         const fixed = new Uint16Array(rab, 2, 2), track = new Uint16Array(rab, 2);
         const all = new Uint8Array(rab);
         const state = () => [fixed, track, all].map((t) => t.length + '/' + t.byteLength + '/' +
             t.byteOffset).join(' ');
         const out = [state()];
         for (const n of [12, 5, 1, 16]) { rab.resize(n); out.push(state()); }
         rab.transfer();
         out.push(state());
         out.join(', ')",
        "2/4/2 3/6/2 8/8/0, 2/4/2 5/10/2 12/12/0, 0/0/0 1/2/2 5/5/0, \
         0/0/0 0/0/0 1/1/0, 2/4/2 7/14/2 16/16/0, 0/0/0 0/0/0 0/0/0",
    );
    assert_string(
        "const rab = new ArrayBuffer(4, { maxByteLength: 8 });
         const u = new Uint8Array(rab), f = new Uint8Array(rab, 0, 4);
         u[0] = 1; u[3] = 4;
         rab.resize(2);
         const r = [u.length, u[3], u[0], f.length, f[0]];
         f[0] = 9; u[3] = 9;
         r.push(u[0]);
         rab.resize(4);
         r.push(u[3], f[0], f.length);
         const b = new ArrayBuffer(2), v = new Uint8Array(b);
         v[0] = 5;
         const moved = new Uint8Array(b.transfer());
         v[0] = 6;
         r.push(v[0], v.length, moved[0], 0 in v, Object.keys(v).length);
         r.map((x) => x === undefined ? 'u' : String(x)).join()",
        "2,u,1,0,u,1,0,1,4,u,0,5,false,0",
    );
}

/// `%TypedArray%` and its statics: `from` (iterables, array-likes, `mapFn`
/// with `thisArg`), `of`, `get [@@species]`, and
/// `TypedArrayCreateFromConstructor`'s result checks.
#[test]
fn statics_from_of_species() {
    assert_string(
        "const TA = Object.getPrototypeOf(Int8Array);
         [TA.name, TA.length, TA.prototype.constructor === TA, TA[Symbol.species] === TA,
          Uint8Array[Symbol.species] === Uint8Array, Uint8Array.from === TA.from,
          Uint8Array.of === TA.of, TA.from.length, TA.of.length,
          [...Uint8Array.of(1, 2, 257)].join('|'),
          [...Int16Array.from([1, '2', 3.7])].join('|'),
          [...Float64Array.from({ length: 3, 0: 1, 2: 3 })].join('|'),
          [...Uint8Array.from([1, 2], function (v, k) { return v * 10 + k + this.x; }, { x: 100 })]
              .join('|'),
          [...Uint8Array.from('123')].join('|'), [...Uint8Array.from(new Set([5, 6]))].join('|'),
          Uint8Array.of().length, Object.getPrototypeOf(Float32Array.of(1)) === Float32Array.prototype,
          Object.getOwnPropertyDescriptor(TA, Symbol.species).get.name].join()",
        "TypedArray,0,true,true,true,true,true,1,0,1|2|1,1|2|3,1|NaN|3,110|121,1|2|3,5|6,0,true,\
         get [Symbol.species]",
    );
    assert_string(
        "function err(f) {
             try { f(); return 'none'; } catch (e) { return e.name + ': ' + e.message; }
         }
         const TA = Object.getPrototypeOf(Int8Array);
         class S extends Int32Array {}
         [err(() => TA.of.call({}, 1)), err(() => TA.from.call({}, [])),
          err(() => Uint8Array.from([], 1)), err(() => Uint8Array.from(null)),
          err(() => Uint8Array.of.call(function () { return {}; }, 1)),
          err(() => Uint8Array.of.call(function () { return new Uint8Array(1); }, 1, 2)),
          err(() => Uint8Array.from.call(function () { return new Uint8Array(5); }, [1, 2])),
          S.of(1, 2) instanceof S, S.from([3]) instanceof S].join('\\n')",
        "TypeError: %TypedArray%.of: this is not a constructor\n\
         TypeError: %TypedArray%.from: this is not a constructor\n\
         TypeError: %TypedArray%.from: when provided, the second argument must be a function\n\
         TypeError: Cannot convert undefined or null to object\n\
         TypeError: The TypedArray constructor did not return a TypedArray\n\
         TypeError: The TypedArray constructor returned a too short TypedArray\n\
         none\ntrue\ntrue",
    );
}

/// The `%TypedArray.prototype%` getters and their receiver checks, the
/// `@@toStringTag` getter (`undefined` for a non-typed-array), and the
/// property facts of `BYTES_PER_ELEMENT` and the globals.
#[test]
fn prototype_getters() {
    assert_string(
        &with_prelude(
            "const g = (k) => Object.getOwnPropertyDescriptor(TA.prototype, k).get;
             const t = new Int32Array(new ArrayBuffer(16), 4, 2);
             const b = Object.getOwnPropertyDescriptor(Uint8Array, 'BYTES_PER_ELEMENT');
             const glob = Object.getOwnPropertyDescriptor(globalThis, 'Uint8Array');
             [g('buffer').call(t) === t.buffer, g('byteLength').call(t), g('byteOffset').call(t),
              g('length').call(t), g('length').name, g(Symbol.toStringTag).name,
              g(Symbol.toStringTag).call({}), g(Symbol.toStringTag).call(3),
              g(Symbol.toStringTag).call(new Float32Array(0)), t[Symbol.toStringTag],
              err(() => g('length').call({})), err(() => g('buffer').call([])),
              err(() => g('byteOffset').call(new ArrayBuffer(1))),
              b.writable, b.enumerable, b.configurable, glob.writable, glob.enumerable,
              glob.configurable, 'TypedArray' in globalThis].join('\\n')",
        ),
        "true\n8\n4\n2\nget length\nget [Symbol.toStringTag]\n\n\nFloat32Array\nInt32Array\n\
         TypeError: Method get %TypedArray%.prototype.length called on incompatible receiver\n\
         TypeError: Method get %TypedArray%.prototype.buffer called on incompatible receiver\n\
         TypeError: Method get %TypedArray%.prototype.byteOffset called on incompatible receiver\n\
         false\nfalse\nfalse\ntrue\nfalse\ntrue\nfalse",
    );
}

/// `keys` / `values` / `entries` / `[@@iterator]`: Array Iterators over the
/// typed array with its current length, a shrink ending the iteration, an
/// out-of-bounds or detached view throwing on `next` (exhausting the
/// iterator) and on the creating method.
#[test]
fn iteration() {
    assert_string(
        &with_prelude(
            "const t = new Uint8Array([3, 4]);
             let sum = 0; for (const x of t) sum += x;
             const rab = new ArrayBuffer(3, { maxByteLength: 3 });
             const u = new Uint8Array(rab); u[0] = 1; u[1] = 2; u[2] = 3;
             const it = u.values();
             const shrink = [it.next().value];
             rab.resize(1);
             shrink.push(it.next().done);
             const b = new ArrayBuffer(2), v = new Uint8Array(b), vi = v.keys();
             vi.next(); b.transfer();
             [[...t.keys()].join('|'), [...t.values()].join('|'),
              [...t.entries()].map((e) => e.join(':')).join('|'), sum,
              t[Symbol.iterator] === t.values, TA.prototype[Symbol.iterator] === TA.prototype.values,
              t.values === Uint8Array.prototype.values, t.values !== Array.prototype.values,
              Object.prototype.toString.call(t.keys()), Array.from(t).join('|'), shrink.join('|'),
              err(() => vi.next()), vi.next().done,
              err(() => TA.prototype.keys.call([])), err(() => v.values()),
              err(() => v.entries()), [...new Float64Array(0)].length].join('\\n')",
        ),
        "0|1\n3|4\n0:3|1:4\n7\ntrue\ntrue\ntrue\ntrue\n[object Array Iterator]\n3|4\n1|true\n\
         TypeError: Cannot perform %ArrayIteratorPrototype%.next on a detached or out-of-bounds TypedArray\n\
         true\n\
         TypeError: Method %TypedArray%.prototype.keys called on incompatible receiver\n\
         TypeError: Cannot perform %TypedArray%.prototype.values on a detached or out-of-bounds TypedArray\n\
         TypeError: Cannot perform %TypedArray%.prototype.entries on a detached or out-of-bounds TypedArray\n\
         0",
    );
}

/// The console form (DESIGN.md §5.8.8): `<Kind>(n) [ .. ]` with the
/// elements by the number rule, own named properties after them, the kind's
/// name for a subclass, length `0` out of bounds, nested like any value.
#[test]
fn rendering() {
    assert_output(
        "console.log(new Uint8Array([1, 2, 3]));
         console.log(new Float64Array(0));
         console.log(new Float32Array([-0, NaN, 1.5]));
         const i8 = new Int8Array(1); i8.foo = 1;
         console.log(i8);
         class S extends Uint16Array {}
         console.log(new S(2));
         const b = new ArrayBuffer(2), d = new Uint8Array(b); b.transfer();
         console.log(d);
         console.log([new Int8Array(1)], { t: new Uint8ClampedArray([300]) });",
        "Uint8Array(3) [ 1, 2, 3 ]\n\
         Float64Array(0) []\n\
         Float32Array(3) [ 0, NaN, 1.5 ]\n\
         Int8Array(1) [ 0, foo: 1 ]\n\
         Uint16Array(2) [ 0, 0 ]\n\
         Uint8Array(0) []\n\
         [ Int8Array(1) [ 0 ] ] { t: Uint8ClampedArray(1) [ 255 ] }\n",
    );
}

/// Allocation-heavy paths under `gc_stress` (a collection at every
/// allocation): every constructor overload, `from` with a mapping function,
/// subclass `of`, iteration and own-key listing, all kept alive and read
/// back.
#[test]
fn allocation_paths_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 40; i++) {
           const b = new ArrayBuffer(16, { maxByteLength: 64 });
           const t = new Float64Array(b, 8);
           const u = Uint8Array.from([i, i + 1, i + 2], (v) => v * 2);
           const c = new Int16Array(u);
           const f = new Float32Array(new Set([i / 2]));
           keep.push(t, u, c, f, [...u.entries()]);
         }
         let s = 0;
         for (const x of keep) s += x.length;
         console.log(s, keep[5], keep[196]);",
        "440 Float64Array(1) [ 0 ] Uint8Array(3) [ 78, 80, 82 ]\n",
    );
    assert_same_under_gc_stress(
        "class S extends Int32Array {}
         const all = [];
         for (let i = 0; i < 30; i++) {
           const s = new S([i, -i]);
           const o = S.of(i, i * 2, i * 3);
           all.push(s, o, Object.keys(o), Object.values(s));
         }
         console.log(all.length, all[116] instanceof S, all[118].join('|'), all[119].join('|'),
           all[117]);",
        "120 true 0|1|2 29|-29 Int32Array(3) [ 29, 58, 87 ]\n",
    );
}
