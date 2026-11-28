//! Integration tests: `Uint8Array` base64 / hex (DESIGN.md §7.9): the RFC
//! 4648 vectors both ways, `base64url`, padding and `omitPadding`, every
//! `lastChunkHandling` mode with its errors, ASCII whitespace, `setFrom*`
//! with short targets (`{ read, written }` and the bytes written before a
//! `SyntaxError`), argument and option validation order, detached and
//! out-of-bounds receivers, and the allocation paths under `gc_stress`.

#![allow(clippy::expect_used)]

mod common;
use common::{assert_same_under_gc_stress, assert_string};

/// `err(f)`: the `name` of what `f` throws (with its message for a
/// `TypeError` from a receiver check), or `none`; `u8(s)` / `str(a)`: bytes
/// from / to a Latin-1 string; `rw(r)`: a `{ read, written }` result.
const PRELUDE: &str = "function err(f) {
    try { f(); return 'none'; } catch (e) { return e.name; }
}
const u8 = (s) => new Uint8Array([...s].map((c) => c.charCodeAt(0)));
const str = (a) => [...a].map((b) => String.fromCharCode(b)).join('');
const rw = (r) => r.read + '/' + r.written;
";

/// `src` after the [`PRELUDE`].
fn with_prelude(src: &str) -> String {
    format!("{PRELUDE}{src}")
}

/// The RFC 4648 §10 test vectors.
#[test]
fn rfc4648_vectors() {
    assert_string(
        &with_prelude(
            "const v = [0, 1, 2, 3, 4, 5, 6].map((k) => 'foobar'.slice(0, k));
             [v.map((s) => u8(s).toBase64()).join('|'),
              v.map((s) => u8(s).toBase64({ omitPadding: true })).join('|'),
              v.map((s) => u8(s).toHex()).join('|'),
              v.every((s) => str(Uint8Array.fromBase64(u8(s).toBase64())) === s),
              v.every((s) => str(Uint8Array.fromHex(u8(s).toHex())) === s)].join(' ')",
        ),
        "|Zg==|Zm8=|Zm9v|Zm9vYg==|Zm9vYmE=|Zm9vYmFy \
         |Zg|Zm8|Zm9v|Zm9vYg|Zm9vYmE|Zm9vYmFy \
         |66|666f|666f6f|666f6f62|666f6f6261|666f6f626172 true true",
    );
}

/// Every byte value round-trips through both alphabets and hex; `toHex` is
/// lowercase and `fromHex` takes either case.
#[test]
fn all_bytes_round_trip() {
    assert_string(
        "const a = new Uint8Array(256);
         for (let i = 0; i < 256; i++) a[i] = i;
         const b = a.toBase64(), u = a.toBase64({ alphabet: 'base64url', omitPadding: true });
         const h = a.toHex();
         [b.length, u.length, h.length, b.slice(0, 8), b.slice(-8), u.slice(-6), h.slice(-6),
          Uint8Array.fromBase64(b).join() === a.join(),
          Uint8Array.fromBase64(u, { alphabet: 'base64url' }).join() === a.join(),
          Uint8Array.fromHex(h).join() === a.join(),
          Uint8Array.fromHex(h.toUpperCase()).join() === a.join(),
          new Uint8Array([0, 15, 16, 171, 255]).toHex(),
          Uint8Array.fromHex('aBcDeF').join()].join(' ')",
        "344 342 512 AAECAwQF /P3+/w== _P3-_w fdfeff true true true true 000f10abff 171,205,239",
    );
}

/// `base64url` maps `-` / `_` and rejects `+` / `/`, and the standard
/// alphabet the reverse.
#[test]
fn base64url_alphabet() {
    assert_string(
        &with_prelude(
            "const a = new Uint8Array([251, 255, 191]);
             [a.toBase64(), a.toBase64({ alphabet: 'base64url' }), a.toBase64({ alphabet: 'base64' }),
              Uint8Array.fromBase64('-_-_', { alphabet: 'base64url' }).join(),
              Uint8Array.fromBase64('+/+/').join(),
              err(() => Uint8Array.fromBase64('-_-_')),
              err(() => Uint8Array.fromBase64('+/+/', { alphabet: 'base64url' })),
              new Uint8Array([251]).toBase64({ alphabet: 'base64url' }),
              new Uint8Array([251]).toBase64({ alphabet: 'base64url', omitPadding: 1 })].join(' ')",
        ),
        "+/+/ -_-_ +/+/ 251,255,191 251,255,191 SyntaxError SyntaxError -w== -w",
    );
}

/// ASCII whitespace (TAB, LF, FF, CR, SPACE) is skipped anywhere in base64,
/// including between and after the padding; other whitespace is malformed,
/// and hex takes no whitespace at all.
#[test]
fn whitespace() {
    assert_string(
        &with_prelude(
            "[str(Uint8Array.fromBase64(' Zm9v\\n YmFy\\t\\r\\f')),
              str(Uint8Array.fromBase64('Zm9vYg= =\\n')), str(Uint8Array.fromBase64('Z m 9 v Y m E = ')),
              err(() => Uint8Array.fromBase64('\\u00a0Zm9v')), err(() => Uint8Array.fromBase64('Zm9v\\v')),
              err(() => Uint8Array.fromHex(' 66')), err(() => Uint8Array.fromHex('66 ')),
              str(Uint8Array.fromBase64('   ')), Uint8Array.fromBase64('').length].join('|')",
        ),
        "foobar|foob|fooba|SyntaxError|SyntaxError|SyntaxError|SyntaxError||0",
    );
}

/// `lastChunkHandling`: `loose` (the default) decodes a final partial chunk
/// and ignores its extra bits, `strict` requires padding and zero extra
/// bits, `stop-before-partial` stops before a final partial chunk; a lone
/// final character, padding after fewer than two characters, a single `=`
/// after two, or data after the padding are malformed in every mode.
#[test]
fn last_chunk_handling() {
    let modes = [
        (
            "loose",
            "foob|fooba|SyntaxError|foob|foob|SyntaxError|SyntaxError|SyntaxError|fooba",
        ),
        (
            "strict",
            "SyntaxError|SyntaxError|SyntaxError|foob|SyntaxError|SyntaxError|SyntaxError|SyntaxError|fooba",
        ),
        (
            "stop-before-partial",
            "foo|foo|foo|foob|foob|SyntaxError|foo|SyntaxError|fooba",
        ),
    ];
    for (mode, expected) in modes {
        let src = with_prelude(&format!(
            "const d = (s) => {{
                 try {{ return str(Uint8Array.fromBase64(s, {{ lastChunkHandling: '{mode}' }})); }}
                 catch (e) {{ return e.name; }}
             }};
             ['Zm9vYg', 'Zm9vYmE', 'Zm9vY', 'Zm9vYg==', 'Zm9vYh==', 'Zm9v=', 'Zm9vYg=',
              'Zm9vYg==Zm9v', 'Zm9vYmE='].map(d).join('|')"
        ));
        assert_string(&src, expected);
    }
    assert_string(
        &with_prelude(
            "[str(Uint8Array.fromBase64('Zm9vYmF=')), err(() => Uint8Array.fromBase64('Zm9vYmF=', { lastChunkHandling: 'strict' })),
              str(Uint8Array.fromBase64('Zm9vYmE', { lastChunkHandling: undefined }))].join('|')",
        ),
        "fooba|SyntaxError|fooba",
    );
}

/// `setFromBase64` / `setFromHex` write at most the target's length, stop
/// before a chunk that does not fit, keep the bytes decoded before a
/// malformed position, and return `{ read, written }` (an ordinary object).
#[test]
fn set_from_short_targets() {
    assert_string(
        &with_prelude(
            "const t4 = new Uint8Array(4), t5 = new Uint8Array(5), t6 = new Uint8Array(6);
             const a = [rw(t4.setFromBase64('Zm9vYmFy')), t4.join(),
                        rw(t5.setFromBase64('Zm9vYmFy')), rw(new Uint8Array(5).setFromBase64('Zm9vYmE=')),
                        rw(t6.setFromBase64('Zm9vYmFy')), str(t6),
                        rw(new Uint8Array(3).setFromBase64('Zm9v!!!')),
                        rw(new Uint8Array(0).setFromBase64('!!!')),
                        rw(new Uint8Array(1).setFromBase64('QQ==')),
                        rw(new Uint8Array(8).setFromBase64('Zm9vYg', { lastChunkHandling: 'stop-before-partial' })),
                        rw(new Uint8Array(8).setFromBase64(' Zm9v ')),
                        rw(new Uint8Array(2).setFromHex('666f6f')),
                        rw(new Uint8Array(8).setFromHex('666f6f')),
                        rw(new Uint8Array(0).setFromHex(''))];
             const r = new Uint8Array(2).setFromHex('0102');
             a.push(Object.getPrototypeOf(r) === Object.prototype, Object.keys(r).join(), r.read, r.written);
             a.join(' ')",
        ),
        "4/3 102,111,111,0 4/3 8/5 8/6 foobar 4/3 0/0 4/1 4/3 6/3 4/2 6/3 0/0 \
         true read,written 4 2",
    );
    assert_string(
        &with_prelude(
            "const t = new Uint8Array(6), h = new Uint8Array(4), o = new Uint8Array([9, 9]);
             const w = new Uint8Array(6);
             [err(() => t.setFromBase64('Zm9v!')), t.join(),
              err(() => h.setFromHex('66zz')), h.join(),
              err(() => o.setFromHex('666')), o.join(),
              err(() => w.setFromBase64('Zm9v\\u0100')), w.join(),
              err(() => Uint8Array.fromHex('6g')), err(() => Uint8Array.fromHex('666')),
              Uint8Array.fromHex('').length].join(' ')",
        ),
        "SyntaxError 102,111,111,0,0,0 SyntaxError 102,0,0,0 SyntaxError 9,9 \
         SyntaxError 102,111,111,0,0,0 SyntaxError SyntaxError 0",
    );
}

/// A subarray target writes at its own offset; a length-tracking target
/// over a resizable buffer takes the buffer's current length.
#[test]
fn set_from_views() {
    assert_string(
        &with_prelude(
            "const a = new Uint8Array(6), s = a.subarray(2, 4);
             const r1 = rw(s.setFromHex('aabbcc'));
             const rb = new ArrayBuffer(2, { maxByteLength: 6 }), t = new Uint8Array(rb);
             rb.resize(6);
             const r2 = rw(t.setFromBase64('AQIDBAUG'));
             [r1, a.join(), r2, t.join(), new Uint8Array(rb, 4).toHex(),
              new Uint8Array(rb, 1, 3).toBase64()].join(' ')",
        ),
        "4/2 0,0,170,187,0,0 8/6 1,2,3,4,5,6 0506 AgME",
    );
}

/// The functions' own facts: names, lengths, attributes, and that they
/// exist only on `Uint8Array`.
#[test]
fn function_facts() {
    assert_string(
        "const P = Uint8Array.prototype;
         const f = (o, k) => { const d = Object.getOwnPropertyDescriptor(o, k);
                               return [k, d.value.length, d.value.name, d.writable, d.enumerable,
                                       d.configurable].join(':'); };
         [f(Uint8Array, 'fromBase64'), f(Uint8Array, 'fromHex'), f(P, 'toBase64'), f(P, 'toHex'),
          f(P, 'setFromBase64'), f(P, 'setFromHex'), typeof Int8Array.fromBase64,
          typeof Uint8ClampedArray.fromHex, typeof new Int8Array(1).toBase64,
          typeof Object.getPrototypeOf(Int8Array).prototype.toHex].join(' ')",
        "fromBase64:1:fromBase64:true:false:true fromHex:1:fromHex:true:false:true \
         toBase64:0:toBase64:true:false:true toHex:0:toHex:true:false:true \
         setFromBase64:1:setFromBase64:true:false:true setFromHex:1:setFromHex:true:false:true \
         undefined undefined undefined undefined",
    );
}

/// Argument validation: the input must be a string primitive (checked
/// before any option is read), options must be `undefined` or an object,
/// `alphabet` / `lastChunkHandling` must be one of their strings; the
/// receiver must be a `Uint8Array`.
#[test]
fn argument_validation() {
    assert_string(
        &with_prelude(
            "const log = [];
             const spy = { get alphabet() { log.push('a'); } };
             [err(() => Uint8Array.fromBase64(1, spy)), err(() => Uint8Array.fromBase64(new String('Zg=='))),
              err(() => Uint8Array.fromHex(new String('aa'))), err(() => Uint8Array.fromHex()),
              err(() => Uint8Array.fromBase64('', null)), err(() => Uint8Array.fromBase64('', 1)),
              err(() => Uint8Array.fromBase64('', 'base64')),
              err(() => Uint8Array.fromBase64('', { alphabet: 'other' })),
              err(() => Uint8Array.fromBase64('', { alphabet: new String('base64') })),
              err(() => Uint8Array.fromBase64('', { alphabet: 'BASE64' })),
              err(() => Uint8Array.fromBase64('', { lastChunkHandling: 'bogus' })),
              err(() => new Uint8Array(1).toBase64({ alphabet: null })),
              err(() => new Uint8Array(1).toBase64(0)),
              err(() => new Uint8Array(1).setFromBase64(1)),
              err(() => new Uint8Array(1).setFromHex({})),
              Uint8Array.fromBase64('', function () {}).length,
              log.length].join(' ')",
        ),
        "TypeError TypeError TypeError TypeError TypeError TypeError TypeError TypeError \
         TypeError TypeError TypeError TypeError TypeError TypeError TypeError 0 0",
    );
    assert_string(
        &with_prelude(
            "const P = Uint8Array.prototype;
             [err(() => P.toBase64.call(new Int8Array(1))),
              err(() => P.toHex.call(new Uint8ClampedArray(1))), err(() => P.toHex.call([])),
              err(() => P.setFromHex.call(new Uint16Array(1), '00')),
              err(() => P.setFromBase64.call({}, 'AA')), err(() => P.toBase64.call(undefined)),
              err(() => P.setFromHex.call(new Int8Array(1), 1))].join(' ')",
        ),
        "TypeError TypeError TypeError TypeError TypeError TypeError TypeError",
    );
    assert_string(
        "class S extends Uint8Array {}
         const s = new S(2);
         s.setFromHex('0a0b');
         [s.toHex(), s.toBase64(), Uint8Array.fromHex('00') instanceof S,
          Object.getPrototypeOf(Uint8Array.fromBase64('AA==')) === Uint8Array.prototype].join(' ')",
        "0a0b Cgs= false true",
    );
}

/// The options are read in spec order through `Get` (getters run, a throw
/// propagates), after the receiver and string checks and before the
/// detached / out-of-bounds check, which therefore sees a buffer the
/// getters detached.
#[test]
fn option_order_and_detached_receivers() {
    assert_string(
        &with_prelude(
            "const log = [];
             const opts = { get alphabet() { log.push('alphabet'); return 'base64url'; },
                            get lastChunkHandling() { log.push('last'); return 'strict'; },
                            get omitPadding() { log.push('omit'); return 'yes'; } };
             Uint8Array.fromBase64('-_8=', opts);
             new Uint8Array(1).setFromBase64('AA==', opts);
             const e = new Uint8Array([255, 255]).toBase64(opts);
             log.push(e);
             const t = new RangeError('x');
             const thrower = { get alphabet() { throw t; } };
             log.push(err(() => Uint8Array.fromBase64('', thrower)));
             log.push(err(() => new Uint8Array(1).toBase64({ get omitPadding() { throw t; } })));
             log.join(' ')",
        ),
        "alphabet last alphabet last alphabet omit __8 RangeError RangeError",
    );
    assert_string(
        &with_prelude(
            "const b = new ArrayBuffer(2), t = new Uint8Array(b);
             b.transfer();
             const b2 = new ArrayBuffer(3), t2 = new Uint8Array(b2);
             const detach = { get alphabet() { b2.transfer(); return 'base64'; } };
             const b3 = new ArrayBuffer(3), t3 = new Uint8Array(b3);
             const rb = new ArrayBuffer(4, { maxByteLength: 4 }), oob = new Uint8Array(rb, 2, 2);
             rb.resize(2);
             [err(() => t.toBase64()), err(() => t.toHex()), err(() => t.setFromBase64('AA')),
              err(() => t.setFromHex('00')), err(() => t2.toBase64(detach)),
              err(() => t3.setFromBase64('AAAA', { get lastChunkHandling() { b3.transfer(); } })),
              err(() => oob.toHex()), err(() => oob.setFromHex('')),
              err(() => t.setFromHex(1)), err(() => t.toBase64(1))].join(' ')",
        ),
        "TypeError TypeError TypeError TypeError TypeError TypeError TypeError TypeError \
         TypeError TypeError",
    );
}

/// Allocation-heavy base64 / hex paths under `gc_stress`.
#[test]
fn base64_allocation_paths_survive_gc_stress() {
    assert_same_under_gc_stress(
        "const out = [];
         for (let i = 0; i < 24; i++) {
           const a = new Uint8Array(i).map((_, j) => j * 37 + i);
           const b = Uint8Array.fromBase64(a.toBase64({ alphabet: 'base64url' }), { alphabet: 'base64url' });
           const h = Uint8Array.fromHex(b.toHex());
           const r = new Uint8Array(i).setFromBase64(h.toBase64());
           out.push(h.join() === a.join() ? r.read + ':' + r.written : 'bad ' + i);
         }
         console.log(out.slice(20).join(' '));",
        "28:20 28:21 32:22 32:23\n",
    );
}
