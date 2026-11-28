//! Integration tests: `ArrayBuffer` (DESIGN.md §7.9): the constructor and
//! its `options.maxByteLength`, the prototype getters, `resize`, `slice`
//! with `SpeciesConstructor`, `transfer` / `transferToFixedLength` and the
//! detached state they leave, `isView`, `@@species`, `@@toStringTag`,
//! subclassing, rendering, and the allocation-heavy paths under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_same_under_gc_stress, assert_string, assert_uncaught};

/// `err(f)`: the `name: message` of what `f` throws, or `none`.
const ERR: &str = "function err(f) {
    try { f(); return 'none'; } catch (e) { return e.name + ': ' + e.message; }
}
";

/// `src` after the [`ERR`] helper.
fn with_err(src: &str) -> String {
    format!("{ERR}{src}")
}

/// `length` through `ToIndex` (truncation, strings, `undefined`), a fresh
/// zeroed buffer over `%ArrayBuffer.prototype%`, fixed by default, and the
/// constructor's own facts.
#[test]
fn constructor_lengths_and_facts() {
    assert_string(
        "const a = new ArrayBuffer(4);
         [typeof a, a.byteLength, new ArrayBuffer().byteLength, new ArrayBuffer(2.9).byteLength,
          new ArrayBuffer('3').byteLength, new ArrayBuffer(undefined).byteLength,
          new ArrayBuffer(-0.5).byteLength, new ArrayBuffer(null).byteLength,
          a.maxByteLength, a.resizable, a.detached,
          Object.getPrototypeOf(a) === ArrayBuffer.prototype, a instanceof ArrayBuffer,
          ArrayBuffer.length, ArrayBuffer.name, ArrayBuffer.prototype.constructor === ArrayBuffer,
          Object.prototype.toString.call(a), String(a)].join()",
        "object,4,0,2,3,0,0,0,4,false,false,true,true,1,ArrayBuffer,true,\
         [object ArrayBuffer],[object ArrayBuffer]",
    );
    assert_string(
        "const d = Object.getOwnPropertyDescriptor(globalThis, 'ArrayBuffer');
         const p = Object.getOwnPropertyDescriptor(ArrayBuffer, 'prototype');
         [d.value === ArrayBuffer, d.writable, d.enumerable, d.configurable,
          p.writable, p.enumerable, p.configurable].join()",
        "true,true,false,true,false,false,false",
    );
}

/// `options.maxByteLength` makes a resizable buffer; a non-object `options`
/// or an `undefined` maximum keeps it fixed.
#[test]
fn constructor_options() {
    assert_string(
        "const r = new ArrayBuffer(2, { maxByteLength: 8 });
         [r.byteLength, r.maxByteLength, r.resizable, r.detached,
          new ArrayBuffer(0, { maxByteLength: undefined }).resizable,
          new ArrayBuffer(3, { maxByteLength: undefined }).maxByteLength,
          new ArrayBuffer(0, null).resizable, new ArrayBuffer(0, 7).resizable,
          new ArrayBuffer(0, 'x').resizable, new ArrayBuffer(0, true).resizable,
          new ArrayBuffer(0, { maxByteLength: 0 }).resizable,
          new ArrayBuffer(5, { maxByteLength: '5.5' }).maxByteLength].join()",
        "2,8,true,false,false,3,false,false,false,false,true,5",
    );
}

/// The constructor's errors, in spec order: called without `new`, then
/// `ToIndex(length)`, then `options.maxByteLength` (a throwing getter, its
/// `ToIndex`), `length > maxByteLength`, and the allocation limits.
#[test]
fn constructor_errors() {
    assert_string(
        &with_err(
            "[err(() => ArrayBuffer(1)),
              err(() => new ArrayBuffer(-1)),
              err(() => new ArrayBuffer(2 ** 53)),
              err(() => new ArrayBuffer(Infinity)),
              err(() => new ArrayBuffer(2 ** 30)),
              err(() => new ArrayBuffer(4, { maxByteLength: 2 })),
              err(() => new ArrayBuffer(0, { maxByteLength: -1 })),
              err(() => new ArrayBuffer(0, { maxByteLength: 2 ** 30 })),
              err(() => new ArrayBuffer(0, { get maxByteLength() { throw new Error('max'); } })),
              err(() => new ArrayBuffer({ valueOf() { throw new Error('len'); } })),
              err(() => new ArrayBuffer(Symbol())).split(':')[0]].join('|')",
        ),
        "TypeError: Constructor ArrayBuffer requires 'new'|\
         RangeError: Invalid array buffer length|\
         RangeError: Invalid array buffer length|\
         RangeError: Invalid array buffer length|\
         RangeError: Array buffer allocation failed|\
         RangeError: Invalid array buffer max length|\
         RangeError: Invalid array buffer max length|\
         RangeError: Array buffer allocation failed|\
         Error: max|\
         Error: len|\
         TypeError",
    );
    // `ToIndex(length)` runs before the `maxByteLength` read, which runs
    // before the length check.
    assert_string(
        "const log = [];
         try {
           new ArrayBuffer({ valueOf() { log.push('len'); return 1; } },
                           { get maxByteLength() { log.push('max'); return 0; } });
         } catch (e) { log.push(e.name); }
         log.join()",
        "len,max,RangeError",
    );
    assert_uncaught(
        "new ArrayBuffer(-1)",
        "RangeError: Invalid array buffer length",
    );
}

/// A subclass constructs through `super(length)` with its own prototype,
/// keeps its own properties and accessors, and inherits the getters.
#[test]
fn subclassing() {
    assert_string(
        "class Sub extends ArrayBuffer {
           constructor(n) { super(n, { maxByteLength: 2 * n }); this.tag = 'sub'; }
           get half() { return this.byteLength / 2; }
         }
         const s = new Sub(6);
         s.resize(12);
         [s instanceof Sub, s instanceof ArrayBuffer, s.byteLength, s.half, s.tag, s.maxByteLength,
          Object.getPrototypeOf(Sub) === ArrayBuffer, Object.keys(s).join('+'),
          Object.prototype.toString.call(s)].join()",
        "true,true,12,6,sub,12,true,tag,[object ArrayBuffer]",
    );
}

/// `ArrayBuffer.isView` is `false` for everything but a view (none exist
/// yet), the buffer itself included.
#[test]
fn is_view() {
    assert_string(
        "[ArrayBuffer.isView(), ArrayBuffer.isView(new ArrayBuffer(1)), ArrayBuffer.isView({}),
          ArrayBuffer.isView([]), ArrayBuffer.isView(1), ArrayBuffer.isView('x'),
          ArrayBuffer.isView(null), ArrayBuffer.isView.length, ArrayBuffer.isView.name].join()",
        "false,false,false,false,false,false,false,1,isView",
    );
}

/// `get ArrayBuffer[@@species]` returns `this`; `@@toStringTag` is the
/// configurable-only string `"ArrayBuffer"`; the getters are configurable,
/// non-enumerable accessors without a setter.
#[test]
fn species_tag_and_getter_descriptors() {
    assert_string(
        "const g = Object.getOwnPropertyDescriptor(ArrayBuffer, Symbol.species);
         class Sub extends ArrayBuffer {}
         [ArrayBuffer[Symbol.species] === ArrayBuffer, Sub[Symbol.species] === Sub, typeof g.get,
          g.set, g.get.name, g.get.length, g.configurable, g.enumerable].join()",
        "true,true,function,,get [Symbol.species],0,true,false",
    );
    assert_string(
        "const t = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, Symbol.toStringTag);
         [t.value, t.writable, t.enumerable, t.configurable].join()",
        "ArrayBuffer,false,false,true",
    );
    assert_string(
        "['byteLength', 'maxByteLength', 'resizable', 'detached'].map((k) => {
           const d = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, k);
           return [typeof d.get, d.set, d.get.name, d.get.length, d.enumerable, d.configurable].join();
         }).join('|')",
        "function,,get byteLength,0,false,true|function,,get maxByteLength,0,false,true|\
         function,,get resizable,0,false,true|function,,get detached,0,false,true",
    );
    assert_string(
        "const P = ArrayBuffer.prototype;
         [P.resize.length, P.resize.name, P.slice.length, P.slice.name, P.transfer.length,
          P.transfer.name, P.transferToFixedLength.length, P.transferToFixedLength.name].join()",
        "1,resize,2,slice,0,transfer,0,transferToFixedLength",
    );
}

/// Every prototype builtin rejects a receiver that is not an
/// `ArrayBuffer`, the prototype itself included.
#[test]
fn incompatible_receivers() {
    assert_string(
        &with_err(
            "const P = ArrayBuffer.prototype;
             const get = (k) => Object.getOwnPropertyDescriptor(P, k).get;
             [err(() => get('byteLength').call({})),
              err(() => get('maxByteLength').call(1)),
              err(() => get('resizable').call([])),
              err(() => get('detached').call(undefined)),
              err(() => P.byteLength),
              err(() => P.resize.call({}, 1)),
              err(() => P.slice.call({})),
              err(() => P.transfer.call('x')),
              err(() => P.transferToFixedLength.call(null))].join('|')",
        ),
        "TypeError: Method ArrayBuffer.prototype.byteLength called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.maxByteLength called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.resizable called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.detached called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.byteLength called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.resize called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.slice called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.transfer called on incompatible receiver|\
         TypeError: Method ArrayBuffer.prototype.transferToFixedLength called on incompatible receiver",
    );
}

/// `resize` grows and shrinks within the maximum (truncating its argument,
/// `undefined` is `0`) and returns `undefined`; grown bytes read zero.
#[test]
fn resize_grow_and_shrink() {
    assert_string(
        "const b = new ArrayBuffer(4, { maxByteLength: 64 });
         const r = [b.resize(10), b.byteLength];
         b.resize(2); r.push(b.byteLength);
         b.resize(0); r.push(b.byteLength);
         b.resize(64); r.push(b.byteLength, b.maxByteLength);
         b.resize(5.7); r.push(b.byteLength);
         b.resize('3'); r.push(b.byteLength);
         b.resize(); r.push(b.byteLength, b.resizable, b.detached);
         r.join()",
        ",10,2,0,64,64,5,3,0,true,false",
    );
    assert_output(
        "const z = new ArrayBuffer(3, { maxByteLength: 3000 });
         z.resize(1); z.resize(3);
         console.log(z);
         z.resize(2000); z.resize(2);
         console.log(z, z.maxByteLength);",
        "ArrayBuffer { [Uint8Contents]: <00 00 00>, byteLength: 3 }\n\
         ArrayBuffer { [Uint8Contents]: <00 00>, byteLength: 2 } 3000\n",
    );
}

/// `resize`'s errors: a fixed buffer is an incompatible receiver;
/// `ToIndex(newLength)` comes before the detached check, which comes before
/// the maximum check.
#[test]
fn resize_errors() {
    assert_string(
        &with_err(
            "const b = new ArrayBuffer(4, { maxByteLength: 8 });
             const d = new ArrayBuffer(1, { maxByteLength: 2 });
             d.transfer();
             [err(() => new ArrayBuffer(4).resize(2)),
              err(() => b.resize(9)),
              err(() => b.resize(-1)),
              err(() => b.resize(Infinity)),
              err(() => d.resize(1)),
              err(() => d.resize(-1)),
              err(() => d.resize(100)),
              b.byteLength].join('|')",
        ),
        "TypeError: Method ArrayBuffer.prototype.resize called on incompatible receiver|\
         RangeError: ArrayBuffer.prototype.resize: Invalid length parameter|\
         RangeError: ArrayBuffer.prototype.resize: Invalid length parameter|\
         RangeError: ArrayBuffer.prototype.resize: Invalid length parameter|\
         TypeError: Cannot perform ArrayBuffer.prototype.resize on a detached ArrayBuffer|\
         RangeError: ArrayBuffer.prototype.resize: Invalid length parameter|\
         TypeError: Cannot perform ArrayBuffer.prototype.resize on a detached ArrayBuffer|\
         4",
    );
}

/// `slice` takes relative `start` / `end` (negative from the end, clamped,
/// `end` defaulting to the length), always yields a fresh fixed buffer, and
/// leaves the receiver alone.
#[test]
fn slice_indices() {
    assert_string(
        "const b = new ArrayBuffer(8);
         const s = [b.slice(), b.slice(2), b.slice(-3), b.slice(1, -1), b.slice(-100, 100),
                    b.slice(5, 2), b.slice(2, undefined), b.slice('1', '3'), b.slice(NaN, Infinity),
                    b.slice(8), b.slice(-Infinity, -7)];
         s.map((x) => x.byteLength).join() + ' ' +
           [s[0] !== b, s[0] instanceof ArrayBuffer, s[0].detached, b.byteLength, b.detached,
            new ArrayBuffer(4, { maxByteLength: 8 }).slice(1).resizable].join()",
        "8,6,3,6,8,0,6,2,8,0,1 true,true,false,8,false,false",
    );
    assert_output(
        "console.log(new ArrayBuffer(5).slice(1, 4));",
        "ArrayBuffer { [Uint8Contents]: <00 00 00>, byteLength: 3 }\n",
    );
}

/// `slice` constructs its result through `SpeciesConstructor`: a subclass
/// gets its own kind, a custom `@@species` is called with the new length,
/// and an `undefined` constructor or a `null` / `undefined` species falls
/// back to `%ArrayBuffer%`; a larger result is accepted.
#[test]
fn slice_species() {
    assert_string(
        "class MyBuf extends ArrayBuffer {}
         const sl = new MyBuf(4).slice(1);
         const calls = [];
         class Spy2 extends ArrayBuffer {
           constructor(n) { calls.push('ctor ' + n); super(n + 2); }
         }
         class Spy extends ArrayBuffer {
           static get [Symbol.species]() { calls.push('species'); return Spy2; }
         }
         const r = new Spy(6).slice(2, 5);
         const u = new ArrayBuffer(4); u.constructor = undefined;
         const n = new ArrayBuffer(4); n.constructor = { [Symbol.species]: null };
         const big = new ArrayBuffer(4);
         big.constructor = { [Symbol.species]: function (len) { return new ArrayBuffer(len + 5); } };
         [sl instanceof MyBuf, sl.byteLength, r instanceof Spy2, r.byteLength, calls.join('/'),
          Object.getPrototypeOf(u.slice(1)) === ArrayBuffer.prototype,
          Object.getPrototypeOf(n.slice(0)) === ArrayBuffer.prototype,
          big.slice(1).byteLength].join()",
        "true,3,true,5,species/ctor 3,true,true,8",
    );
}

/// `slice`'s errors: a detached receiver (checked before the arguments are
/// converted, and again after the species constructor ran), a bad
/// `constructor` / `@@species`, and a result that is not an `ArrayBuffer`,
/// is the receiver, is detached or is too small.
#[test]
fn slice_errors() {
    assert_string(
        &with_err(
            "const log = [];
             const d = new ArrayBuffer(2); d.transfer();
             const species = (f) => { const b = new ArrayBuffer(4); b.constructor = { [Symbol.species]: f }; return b; };
             const ctor = (c) => { const b = new ArrayBuffer(4); b.constructor = c; return b; };
             const self = new ArrayBuffer(4);
             self.constructor = { [Symbol.species]: function () { return self; } };
             const victim = new ArrayBuffer(4);
             victim.constructor = {
               [Symbol.species]: function (n) { victim.transfer(); return new ArrayBuffer(n); },
             };
             [err(() => d.slice({ valueOf() { log.push('start'); return 0; } })),
              log.length,
              err(() => ctor(1).slice()),
              err(() => species({}).slice()),
              err(() => species(1).slice()),
              err(() => species(function () { return {}; }).slice()),
              err(() => self.slice()),
              err(() => species(function (n) { return new ArrayBuffer(n - 1); }).slice()),
              err(() => species(function (n) { const x = new ArrayBuffer(n); x.transfer(); return x; }).slice()),
              err(() => victim.slice()),
              victim.detached].join('|')",
        ),
        "TypeError: Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer|\
         0|\
         TypeError: object.constructor is not an object|\
         TypeError: object.constructor[Symbol.species] is not a constructor|\
         TypeError: object.constructor[Symbol.species] is not a constructor|\
         TypeError: ArrayBuffer subclass returned a non-ArrayBuffer from its species constructor|\
         TypeError: ArrayBuffer subclass returned this from species constructor|\
         TypeError: ArrayBuffer subclass returned a too small buffer from its species constructor|\
         TypeError: ArrayBuffer subclass returned a detached buffer from its species constructor|\
         TypeError: Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer|\
         true",
    );
}

/// `transfer` / `transferToFixedLength` hand the bytes to a fresh buffer
/// (the current length by default, else `ToIndex(newLength)`) and detach
/// the receiver: its lengths read `0`, `detached` is `true`, resizability is
/// kept.
#[test]
fn transfer_detaches() {
    assert_string(
        "const a = new ArrayBuffer(4);
         const t = a.transfer();
         [a.detached, a.byteLength, a.maxByteLength, a.resizable, t.byteLength, t.maxByteLength,
          t.detached, t.resizable, t !== a, Object.getPrototypeOf(t) === ArrayBuffer.prototype].join()",
        "true,0,0,false,4,4,false,false,true,true",
    );
    assert_string(
        "const a = new ArrayBuffer(4), b = new ArrayBuffer(4), c = new ArrayBuffer(4);
         const d = new ArrayBuffer(4);
         const t1 = a.transfer(8), t2 = b.transfer(2), t3 = c.transfer(0), t4 = d.transfer('6');
         [t1.byteLength, t2.byteLength, t3.byteLength, t4.byteLength,
          a.detached, b.detached, c.detached, d.detached].join()",
        "8,2,0,6,true,true,true,true",
    );
    assert_string(
        "const r = new ArrayBuffer(4, { maxByteLength: 16 });
         const t = r.transfer(10);
         const f = t.transferToFixedLength();
         const g = new ArrayBuffer(4, { maxByteLength: 8 }).transferToFixedLength(6);
         const h = new ArrayBuffer(4, { maxByteLength: 8 }).transferToFixedLength(9);
         const k = new ArrayBuffer(2, { maxByteLength: 8 }).transfer();
         k.resize(8);
         [t.resizable, t.byteLength, t.maxByteLength, r.resizable, r.detached, r.maxByteLength,
          f.resizable, f.byteLength, f.maxByteLength, t.detached, g.resizable, g.byteLength,
          g.maxByteLength, h.byteLength, k.byteLength, k.maxByteLength].join()",
        "true,10,16,true,true,0,false,10,10,true,false,6,6,9,8,8",
    );
    // A subclass instance transfers into a plain `ArrayBuffer`.
    assert_string(
        "class Sub extends ArrayBuffer {}
         const t = new Sub(3).transfer();
         [t instanceof Sub, Object.getPrototypeOf(t) === ArrayBuffer.prototype, t.byteLength].join()",
        "false,true,3",
    );
    assert_output(
        "const a = new ArrayBuffer(3);
         const t = a.transfer(5);
         const s = new ArrayBuffer(4).transfer(2);
         console.log(t, s);",
        "ArrayBuffer { [Uint8Contents]: <00 00 00 00 00>, byteLength: 5 } \
         ArrayBuffer { [Uint8Contents]: <00 00>, byteLength: 2 }\n",
    );
}

/// `transfer*`'s errors: `ToIndex(newLength)` before the detached check, a
/// length past a resizable buffer's maximum (kept only by `transfer`), and
/// the allocation limit; every operation on a detached buffer throws while
/// its getters keep answering.
#[test]
fn transfer_errors_and_detached_operations() {
    assert_string(
        &with_err(
            "const d = new ArrayBuffer(4); d.transfer();
             const rd = new ArrayBuffer(4, { maxByteLength: 8 }); rd.transfer();
             [err(() => d.transfer()),
              err(() => d.transferToFixedLength()),
              err(() => d.transfer(-1)),
              err(() => d.slice()),
              err(() => rd.resize(1)),
              err(() => new ArrayBuffer(4, { maxByteLength: 8 }).transfer(9)),
              err(() => new ArrayBuffer(4).transfer(-1)),
              err(() => new ArrayBuffer(4).transfer(2 ** 30)),
              err(() => new ArrayBuffer(4).transferToFixedLength(2 ** 53)),
              d.byteLength, d.maxByteLength, d.resizable, d.detached,
              rd.byteLength, rd.maxByteLength, rd.resizable, rd.detached].join('|')",
        ),
        "TypeError: Cannot perform ArrayBuffer.prototype.transfer on a detached ArrayBuffer|\
         TypeError: Cannot perform ArrayBuffer.prototype.transferToFixedLength on a detached ArrayBuffer|\
         RangeError: Invalid array buffer length|\
         TypeError: Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer|\
         TypeError: Cannot perform ArrayBuffer.prototype.resize on a detached ArrayBuffer|\
         RangeError: Invalid array buffer length|\
         RangeError: Invalid array buffer length|\
         RangeError: Array buffer allocation failed|\
         RangeError: Invalid array buffer length|\
         0|0|false|true|0|0|true|true",
    );
}

/// The test262 runner's `$262.detachArrayBuffer` prelude (DESIGN.md
/// §6.2) works in jsc: it detaches through the captured `transfer` even
/// after the test replaced it, and an already detached buffer is a no-op.
#[test]
fn test262_detach_hook() {
    assert_string(
        "'use strict';
         var $262 = (function (transfer, detached) {
           return { detachArrayBuffer: function (buffer) {
             if (!detached.call(buffer)) transfer.call(buffer);
             return null;
           } };
         })(ArrayBuffer.prototype.transfer,
            Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'detached').get);
         const b = new ArrayBuffer(4);
         ArrayBuffer.prototype.transfer = null;
         [$262.detachArrayBuffer(b) === null, b.detached, b.byteLength,
          $262.detachArrayBuffer(b) === null, b.detached].join()",
        "true,true,0,true,true",
    );
}

/// The console form (DESIGN.md §6.9): the bytes as lowercase hex, `<>`
/// when empty, `(detached)` once detached, nested like any value.
#[test]
fn rendering() {
    assert_output(
        "const d = new ArrayBuffer(2); d.transfer();
         console.log(new ArrayBuffer(3));
         console.log(new ArrayBuffer(0));
         console.log(d);
         console.log([new ArrayBuffer(1)], { b: new ArrayBuffer(2, { maxByteLength: 4 }) });",
        "ArrayBuffer { [Uint8Contents]: <00 00 00>, byteLength: 3 }\n\
         ArrayBuffer { [Uint8Contents]: <>, byteLength: 0 }\n\
         ArrayBuffer { (detached), byteLength: 0 }\n\
         [ ArrayBuffer { [Uint8Contents]: <00>, byteLength: 1 } ] \
         { b: ArrayBuffer { [Uint8Contents]: <00 00>, byteLength: 2 } }\n",
    );
}

/// Allocation-heavy paths under `gc_stress` (a collection at every
/// allocation): buffers, reallocating `resize`, `slice`, copying and
/// taking-over `transfer`, all kept alive and read back.
#[test]
fn allocation_paths_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const keep = [];
         for (let i = 0; i < 50; i++) {
           const b = new ArrayBuffer(i, { maxByteLength: 4096 });
           b.resize(i * 40);
           const s = b.slice(1, -1);
           const t = s.transfer(i + 3);
           keep.push(b, t);
         }
         let sum = 0;
         for (const x of keep) sum += x.byteLength;
         console.log(sum, keep[99].byteLength, keep[98].maxByteLength);
         console.log(keep[3]);",
        "50375 52 4096\n\
         ArrayBuffer { [Uint8Contents]: <00 00 00 00>, byteLength: 4 }\n",
    );
    assert_same_under_gc_stress(
        "const b = new ArrayBuffer(2, { maxByteLength: 64 });
         for (let i = 0; i < 30; i++) { b.resize(64); b.resize(i % 5); new ArrayBuffer(100); }
         const t = b.transfer();
         const f = t.transferToFixedLength(6);
         class Sub extends ArrayBuffer {}
         const subs = [];
         for (let i = 0; i < 20; i++) subs.push(new Sub(i).slice(0, 2));
         console.log(f, b.detached, t.detached, subs.every((s) => s instanceof Sub));",
        "ArrayBuffer { [Uint8Contents]: <00 00 00 00 00 00>, byteLength: 6 } true true true\n",
    );
}
