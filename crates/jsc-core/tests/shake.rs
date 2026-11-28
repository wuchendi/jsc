//! Integration tests: runtime shaking (DESIGN.md §4.2). A module without the
//! unreachable runtime bodies behaves like the full one.
#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{compile_with, exec_with, gc_stress_from_env};
use jsc_core::emit::CompileOptions;

fn options(full_runtime: bool) -> CompileOptions {
    CompileOptions {
        print_completion: true,
        gc_stress: gc_stress_from_env(),
        no_stack_guard: false,
        full_runtime,
    }
}

fn size(src: &str, full_runtime: bool) -> usize {
    compile_with(src, options(full_runtime))
        .expect("compiles")
        .len()
}

/// Assert `src` prints `expected` with the shaken and with the full runtime.
fn same(src: &str, expected: &str) {
    for full_runtime in [false, true] {
        let done = exec_with(src, options(full_runtime)).expect("compiles and does not trap");
        assert_eq!(
            (done.code, done.stdout.trim(), done.stderr.as_str()),
            (0, expected, ""),
            "`{src}` with full_runtime = {full_runtime}"
        );
    }
}

#[test]
fn unreachable_bodies_are_dropped() {
    let src = "console.log('hi')";
    assert!(size(src, false) * 3 < size(src, true) * 2);
}

/// A program that cannot create a proxy keeps no `__proxy_*` body and no
/// proxy arm (DESIGN.md §6.11): naming `Proxy` grows the shaken module.
#[test]
fn proxy_helpers_are_dropped_without_proxy() {
    let plain = "const o = { a: 1 }; o.b = 2; o.a + o.b";
    let named = "const o = { a: 1 }; o.b = 2; typeof Proxy === 'function' ? o.a + o.b : 0";
    assert!(size(plain, false) + 1000 < size(named, false));
    same(plain, "3");
    same(named, "3");
}

/// A program that cannot reach binary data keeps no binary-data builtin, no
/// `Group::Binary` helper and no binary arm (DESIGN.md §6.4): naming a
/// typed-array constructor grows the shaken module by the typed-array
/// runtime.
#[test]
fn binary_helpers_are_dropped_without_binary() {
    let plain = "console.log('hi')";
    let named = "console.log('hi'); new Uint8Array(1)";
    let (plain_size, named_size) = (size(plain, false), size(named, false));
    assert!(
        plain_size + 20_000 < named_size,
        "{plain_size} vs {named_size}"
    );
    assert!(size(plain, false) * 2 < size(plain, true));
}

/// Hello world pays nothing for the binary-data runtime: its shaken module is
/// smaller than one naming the three binary-data globals by more than their
/// constructors, getters and helpers.
#[test]
fn hello_world_pays_nothing_for_binary_data() {
    let plain = "console.log('hi')";
    let named = "console.log('hi', typeof ArrayBuffer, typeof DataView, typeof Uint8Array)";
    let (plain_size, named_size) = (size(plain, false), size(named, false));
    assert!(
        plain_size + 80_000 < named_size,
        "{plain_size} vs {named_size}"
    );
}

/// Whether the module `wasm` carries the static string `text` (a one-byte
/// string's data is its bytes).
fn carries(wasm: &[u8], text: &str) -> bool {
    wasm.windows(text.len()).any(|w| w == text.as_bytes())
}

/// Binary-data texts only the binary-data builtins, helpers and render arms
/// look up: a receiver and a detached `TypeError`, an alignment `RangeError`,
/// a callback `TypeError`, a `DataView` and a base64 message, and the
/// `ArrayBuffer` rendering.
const BINARY_TEXTS: [&str; 7] = [
    "Method ArrayBuffer.prototype.resize called on incompatible receiver",
    "Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer",
    "Start offset of Int16Array should be a multiple of 2",
    "%TypedArray%.prototype.map callback is not a function",
    "Cannot perform DataView.prototype.getInt8 on a detached or out-of-bounds DataView",
    "The input must be a string",
    "[Uint8Contents]: <",
];

/// Without binary data the binary-data builtins and string-reading helpers
/// are stubs, so their static strings are not interned (DESIGN.md §4.2):
/// hello world and a `Math.f16round` program carry none of them, while a
/// program naming `ArrayBuffer` carries them all and is larger by at least
/// their bytes.
#[test]
fn binary_strings_are_gated_with_the_bodies() {
    let plain = compile_with("console.log('hi')", options(false)).expect("compiles");
    let f16 = compile_with("Math.f16round(1.337)", options(false)).expect("compiles");
    let named =
        compile_with("console.log('hi', typeof ArrayBuffer)", options(false)).expect("compiles");
    for text in BINARY_TEXTS {
        assert!(!carries(&plain, text), "hello world carries `{text}`");
        assert!(!carries(&f16, text), "f16round carries `{text}`");
        assert!(
            carries(&named, text),
            "`ArrayBuffer` program lacks `{text}`"
        );
    }
    let unshaken = compile_with("console.log('hi')", options(true)).expect("compiles");
    for text in BINARY_TEXTS {
        assert!(
            !carries(&unshaken, text),
            "full hello world carries `{text}`"
        );
    }
    assert!(
        plain.len() + 15_000 < named.len(),
        "{} vs {}",
        plain.len(),
        named.len()
    );
}

/// A program that reaches binary data still renders and throws with the
/// gated strings.
#[test]
fn binary_programs_keep_their_strings() {
    same("new Uint8Array([1, 2])", "Uint8Array(2) [ 1, 2 ]");
    same(
        "try { ArrayBuffer.prototype.resize.call({}, 1) } catch (e) { String(e) }",
        "TypeError: Method ArrayBuffer.prototype.resize called on incompatible receiver",
    );
    same(
        "try { new Int16Array(new ArrayBuffer(4), 1) } catch (e) { String(e) }",
        "RangeError: Start offset of Int16Array should be a multiple of 2",
    );
    same(
        "try { Uint8Array.fromBase64(1) } catch (e) { String(e) }",
        "TypeError: The input must be a string",
    );
}

/// `Math.f16round` does not make a program reach binary data: it keeps the
/// two binary16 helpers and none of the typed-array runtime.
#[test]
fn f16round_does_not_pull_binary_data() {
    let f16 = "Math.f16round(1.337)";
    let named = "new Uint8Array(1); Math.f16round(1.337)";
    let (f16_size, named_size) = (size(f16, false), size(named, false));
    assert!(f16_size + 20_000 < named_size, "{f16_size} vs {named_size}");
    same(f16, "1.3369140625");
    same("const r = Math.f16round; r(65520)", "Infinity");
}

/// The base64 / hex methods and `Float16Array` survive shaking in a program
/// that names them.
#[test]
fn base64_and_float16_are_kept_when_named() {
    same(
        "new Uint8Array([1, 2, 3]).toBase64() + Uint8Array.fromHex('0a')[0]",
        "AQID10",
    );
    same(
        "const t = new Uint8Array(2); t.setFromBase64('AQI=').written + t.toHex()",
        "20102",
    );
    same("new Float16Array([0.1])[0]", "0.0999755859375");
}

/// Binary data reached through the global object, by a literal or a computed
/// name, keeps what it uses.
#[test]
fn binary_data_through_the_global_object_is_kept() {
    same("new globalThis['Uint8Array'](3).length", "3");
    same(
        "const a = new globalThis['Float64Array']([1.5, 2]); a[1] += a[0]; a[1]",
        "3.5",
    );
    let src = "const k = ['Uint8', 'Array'].join(''); const a = new globalThis[k](2); a[1] = 7; a[1] + a.length";
    assert_eq!(size(src, false), size(src, true));
    same(src, "9");
    same(
        "new DataView(new ArrayBuffer(4)).byteLength + new Uint8Array([1, 2]).buffer.byteLength",
        "6",
    );
    same(
        "Object.prototype.toString.call(new Int16Array(1)) + [...new Uint8Array([4, 5])].join()",
        "[object Int16Array]4,5",
    );
}

/// A key built at run time can name any builtin: the module stays whole.
#[test]
fn dynamic_string_keys_keep_the_runtime() {
    let src = "const k = 'ma' + 'p'; [1, 2][k](x => x * 2).join()";
    assert_eq!(size(src, false), size(src, true));
    same(src, "2,4");
    let src = "const o = { a: 1 }; let s = 0; for (const k in o) s += o[k]; s";
    assert_eq!(size(src, false), size(src, true));
    same(src, "1");
}

/// A numeric or literal key cannot name a builtin the program does not
/// mention.
#[test]
fn numeric_and_literal_keys_still_shake() {
    let src = "const a = [1, 2, 3]; a[0] + a[1 + 1] + a['length']";
    assert!(size(src, false) < size(src, true));
    same(src, "7");
    let src = "const o = { n: 1 }; o.n += 2; o.n++; o['n']";
    assert!(size(src, false) < size(src, true));
    same(src, "4");
}

/// Descriptor reflection hands out builtins without naming them.
#[test]
fn reflection_keeps_the_runtime() {
    let src = "let f; for (const d of Object.values(Object.getOwnPropertyDescriptors(Math)))
                 if (!f && typeof d.value === 'function') f = d.value;
               typeof f(1)";
    assert_eq!(size(src, false), size(src, true));
    same(src, "number");
}

/// A builtin handed a key built at run time can expose any other builtin.
#[test]
fn runtime_keys_keep_the_runtime() {
    let key = "var k = ['m', 'a', 'p'].join('');";
    same(
        &format!(
            "{key} Object.defineProperty(Array.prototype, k, {{ enumerable: true }});
             var f = Object.values(Array.prototype)[0];
             f.call([1, 2], x => x * 2).join()"
        ),
        "2,4",
    );
    same(
        &format!(
            "{key} Object.defineProperties(Array.prototype, JSON.parse('{{\"map\":{{\"enumerable\":true}}}}'));
             Object.values(Array.prototype)[0].call([1], x => x + k.length).join()"
        ),
        "4",
    );
    same(
        &format!(
            "{key} var got; Function.prototype.toJSON = function () {{ got = this; return 1 }};
             JSON.stringify(Object.create(Array.prototype), [k]) + got.call([1, 2], x => x * 2).join()"
        ),
        "{\"map\":1}2,4",
    );
}

/// Builtins the runtime reaches by a name the program never writes.
#[test]
fn names_looked_up_by_the_runtime_are_kept() {
    same("String([1, [2, 3]])", "1,2,3");
    same("`${{ toString() { return 'x' } }}${[4]}`", "x4");
    same(
        "JSON.stringify([new Date(0)])",
        "[\"1970-01-01T00:00:00.000Z\"]",
    );
    same("[...'ab', ...new Set([1])].length", "3");
    same("Array.from(new Map([[1, 2]]))[0][1]", "2");
    same(
        "class A extends Array {} new A(1, 2).map(x => x) instanceof A",
        "true",
    );
    same("({}).constructor === Object", "true");
    same(
        "typeof Object.getPrototypeOf(function () {})()",
        "undefined",
    );
    same("1 + { valueOf() { return 2 } }", "3");
    same("[3, 1, 2].sort().join('')", "123");
    same("'a-b'.split('-').length + 'x'.toUpperCase().length", "3");
    same(
        "let r; try { null.x } catch (e) { r = e.constructor === TypeError } r",
        "true",
    );
    same(
        "Object.getPrototypeOf(Object.assign({}, JSON.parse('{\"__proto__\":null}'))) === null",
        "true",
    );
    same(
        "new Error('m', { cause: 1 }).cause + String(new RangeError('r'))",
        "1RangeError: r",
    );
    same(
        "function* g() { yield 1; yield 2 } const [a, b] = g(); a + b",
        "3",
    );
    same("Math.max(...[1, 5, 3]) + parseInt('7') + Number('2')", "14");
}

/// Promise machinery reached through `await` and a thenable.
#[test]
fn async_paths_are_kept() {
    for full_runtime in [false, true] {
        let done = exec_with(
            "async function f() { return (await { then(r) { r(20) } }) + (await 1) }
             f().then(v => console.log(v + 1)).finally(() => console.log('done'))",
            options(full_runtime),
        )
        .expect("runs");
        assert_eq!(
            (done.code, done.stdout.as_str()),
            (0, "22\ndone\nPromise { undefined }\n")
        );
    }
}

/// The shaken `console.log("hi")` module: 432 056 bytes at 11ca248 and
/// 487 068 after the phase-5b merges. The data segments are not shaken: the
/// growth is mostly the binary-data message strings (~270 new static strings,
/// ~19 KB), the intern table doubling to 4096 slots (16 KB) once the string
/// count passed 1024, ~7 KB of realm image (method blocks, property entries,
/// intrinsic blocks of the new constructors and prototypes) and function-info
/// entries, a few KB of `BigInt` strings and gated-arm residue, and the
/// `Proxy` property-error strings. The `Proxy` builtins' and `Reflect`
/// functions' own strings (~2 KB) exist only when their gate is open. Bounded
/// with headroom for `gc_stress`. The `Reflect` merge had left every program
/// at the full runtime (~1.2 MB): the renderer's `"constructor"` lookup keeps
/// the `Map` constructor, its `"set"` lookup kept `Reflect.set`, and that
/// body's run-time-key use left the module whole (DESIGN.md §6.11). The
/// binary-data builtins' strings (~340 static strings, ~23 KB of blobs, most
/// of them binary-only) are interned only when binary data is reachable
/// (DESIGN.md §4.2), which also brings the string count, and so possibly the
/// intern table, back down.
const HELLO_BOUND: usize = 480_000;

#[test]
fn hello_world_stays_near_the_base() {
    let src = "console.log('hi')";
    let shaken = size(src, false);
    assert!(shaken < HELLO_BOUND, "hello world is {shaken} bytes");
    assert!(shaken * 3 < size(src, true) * 2);
}

/// A member store whose base may be a primitive runs `ToObject(base).[[Set]]`
/// through `__set_bool` (a full `[[Set]]` body). The runtime's own stores
/// (`Array(n)`'s `length` write, the generic `Array.prototype` and
/// `Object.assign` `Set`s, all on Objects) and a base class constructor's
/// `this.x = ..` carry no such arm, so hello world keeps none: one user store
/// adds over 2 KB there (the arm plus the plain store path `__set` /
/// `__define`, which hello world does not keep either; L1 measured +4.7 KB).
/// Next to a class program, which already keeps the plain store path, the
/// increment is only `__set_bool` and the new function (CI measured 1740
/// bytes), so the class pair is checked by stub, not by size
/// (`emit::tests::primitive_store_arm_keeps_set_bool_only_for_user_stores`).
#[test]
fn primitive_store_arm_is_kept_only_by_user_stores() {
    let hello = size("console.log('hi')", false);
    let store = "function f(o) { o.x = 1 } f({}); console.log('hi')";
    assert!(
        size(store, false) > hello + 2_000,
        "hello world is {hello} bytes"
    );
    let class = "class P { constructor(v) { this.v = v } }\nconsole.log(new P(1).v)";
    let class_store = "class P { constructor(v) { this.v = v } }\n\
        function f(o) { o.w = 2 }\nconst p = new P(1); f(p); console.log(p.v, p.w)";
    same(class, "1\nundefined");
    same(class_store, "1 2\nundefined");
    same(store, "hi\nundefined");
}

/// The runtime stores that lost their primitive arm still store.
#[test]
fn runtime_object_stores_still_run() {
    same(
        "const a = Array(3); const o = {}; Array.prototype.push.call(o, 7, 8);\n\
         const t = Object.assign({}, {x: 1}); console.log(a.length, o.length, o[1], t.x)",
        "3 2 8 1\nundefined",
    );
}

/// Without a way to reach `%Reflect%` or a proxy, the `Reflect` and `Proxy`
/// bodies are stubs that record no use. Naming `Reflect`, even only in a
/// string, opens the `Reflect` gate: `Reflect.set` (kept by the `"set"`
/// lookups) then has its real body, so the module grows beyond a twin that
/// differs only in the string's text.
#[test]
fn reflect_and_proxy_bodies_are_stubbed_when_unreachable() {
    let program = |s: &str| {
        format!("const m = new Map([[1, 2]]); m.set(3, 4); const s = '{s}'; m.size + s.length - 7")
    };
    let (plain, named) = (program("Refract"), program("Reflect"));
    let (plain_size, named_size) = (size(&plain, false), size(&named, false));
    assert!(
        plain_size + 100 < named_size,
        "{plain_size} vs {named_size}"
    );
    assert!(plain_size * 3 < size(&plain, true) * 2);
    same(&plain, "2");
    same(&named, "2");
    // The full runtime stubs them too: the gate is a compile-time fact.
    assert!(size(&plain, true) + 100 < size(&named, true));
}

/// A program calling one `Reflect` function keeps it (and only what it
/// reaches): the module stays well below the full runtime even though the
/// runtime looks up `"set"` / `"get"` on objects of its own. (Naming `get`
/// anywhere, `m.get(1)` included, would count `Reflect.get`'s run-time-key
/// use; these programs avoid it.)
#[test]
fn one_reflect_function_keeps_it_and_still_shakes() {
    let src = "const m = new Map([[1, 2]]); \
               Reflect.getPrototypeOf(m) === Map.prototype ? m.size : 0";
    assert!(size(src, false) * 3 < size(src, true) * 2);
    same(src, "1");
    let src = "Reflect.apply(Math.max, undefined, [1, 3, 2]) + Reflect.has({ a: 1 }, 'a')";
    assert!(size(src, false) * 3 < size(src, true) * 2);
    same(src, "4");
    let src = "const o = {}; Reflect.set(o, 'a', 5); Reflect.deleteProperty(o, 'b'); o.a";
    assert!(size(src, false) * 3 < size(src, true) * 2);
    same(src, "5");
}

/// `Reflect.get` under a key built at run time can hand out any builtin:
/// naming it keeps the runtime whole.
#[test]
fn reflect_get_keeps_the_runtime() {
    let src = "const k = ['m', 'a', 'p'].join(''); \
               Reflect.get(Array.prototype, k).call([1, 2], x => x * 2).join()";
    assert_eq!(size(src, false), size(src, true));
    same(src, "2,4");
}

/// `Proxy` built with `Reflect` as its handler forwards every trap.
#[test]
fn reflect_handler_proxy_runs_shaken() {
    same(
        "const p = new Proxy({ a: 1 }, Reflect); p.b = 2; ('a' in p) + p.a + p.b",
        "4",
    );
}

/// A get- and set-trap proxy shakes: the proxy helpers hand descriptors and
/// key lists only to attribute checks and invariant tests, and the
/// `Object` / `Reflect` functions kept by the trap-name lookups cannot be
/// called with a key the program does not write (DESIGN.md §4.2, §6.11).
#[test]
fn trap_proxy_still_shakes() {
    let src = "const seen = [];
               const p = new Proxy({}, {
                 get(t, k) { return k === 'x' ? 1 : 2 },
                 set(t, k, v) { seen.push(v); return true },
               });
               p.y = 5;
               [p.x + p.y, seen.length, seen[0]].join()";
    let (shaken, full) = (size(src, false), size(src, true));
    assert!(shaken * 3 < full * 2, "{shaken} vs {full}");
    same(src, "3,1,5");
    // The traps see the forwarded key and receiver; no trap forwards.
    let src = "const p = new Proxy({ a: 1 }, {});
               p.b = 2;
               ('a' in p) + p.a + p.b + (delete p.a) + ('a' in p)";
    let (shaken, full) = (size(src, false), size(src, true));
    assert!(shaken * 3 < full * 2, "{shaken} vs {full}");
    same(src, "5");
}

/// Descriptor and key-list reflection on a proxy works (the module may be
/// whole): the trap's descriptor reaches `Object.getOwnPropertyDescriptor`
/// and makes `a` enumerable for `Object.keys`.
#[test]
fn proxy_descriptor_reflection_runs() {
    same(
        "const p = new Proxy({ a: 1 }, {
           getOwnPropertyDescriptor(t, k) { return { value: 7, enumerable: true, configurable: true } },
         });
         const d = Object.getOwnPropertyDescriptor(p, 'a');
         [d.value, d.enumerable, Object.keys(p).join()].join()",
        "7,true,a",
    );
    same(
        "const p = new Proxy({ a: 1, b: 2 }, { ownKeys(t) { return ['b', 'a'] } });
         Object.keys(p).join() + Object.values(p).join() + JSON.stringify({ ...p })",
        "b,a2,1{\"b\":2,\"a\":1}",
    );
}

/// A lying `getOwnPropertyDescriptor` trap makes a target's builtin look
/// enumerable: `Object.values` / `Object.assign` / spread then hand it out,
/// so with `Proxy` in the program they keep the runtime whole.
#[test]
fn proxy_listed_values_keep_the_runtime() {
    let proxy = "const p = new Proxy(Object.prototype, {
                   ownKeys(t) { return [['is', 'Prototype', 'Of'].join('')] },
                   getOwnPropertyDescriptor(t, k) {
                     return { value: 0, writable: true, enumerable: true, configurable: true }
                   },
                 });";
    for (tail, expected) in [
        ("Object.values(p)[0].call(Object.prototype, {})", "true"),
        (
            "Object.values(Object.assign({}, p))[0].call(Object.prototype, {})",
            "true",
        ),
        (
            "Object.values({ ...p })[0].call(Object.prototype, {})",
            "true",
        ),
    ] {
        let src = format!("{proxy} {tail}");
        assert_eq!(size(&src, false), size(&src, true), "{tail}");
        same(&src, expected);
    }
}

/// `Reflect.get(o, 'x')` / `Object.getOwnPropertyDescriptor(o, 'x')` with a
/// constant key shake like `o.x`; the same function under a run-time key, or
/// read as a value, keeps the runtime whole. `get` has a `Map` fast path, so
/// `Reflect.get(..)` lowers to a guarded builtin call, not an ordinary method
/// call: with side-effect-free arguments its interned key, otherwise its early
/// callee read and receiver nullish guard, look up `"get"`; each must stay
/// exempt (a first version wrapped only the ordinary method call, and every
/// `Reflect.get` program stayed whole).
#[test]
fn constant_key_reflection_still_shakes() {
    for (src, expected) in [
        (
            "const o = { x: 4 }; Reflect.get(o, 'x') + Reflect.get([5], 0)",
            "9",
        ),
        // Arguments with side effects: the early callee read.
        (
            "Reflect.get({ x: 4 }, 'x') + Reflect.get([5].concat([6]), 1)",
            "10",
        ),
        (
            "const d = Object.getOwnPropertyDescriptor({ x: 3 }, 'x'); d.value + d.writable",
            "4",
        ),
        (
            "const o = {}; Reflect.defineProperty(o, 'x', { value: 6 }); o.x",
            "6",
        ),
    ] {
        let (shaken, full) = (size(src, false), size(src, true));
        assert!(shaken * 3 < full * 2, "`{src}`: {shaken} vs {full}");
        same(src, expected);
    }
    for src in [
        "const k = ['m', 'a', 'p'].join(''); \
         Reflect.get({ x: 1 }, 'x') + Reflect.get(Array.prototype, k).call([1, 2], x => x * 2).join()",
        "const k = ['m', 'a', 'p'].join(''); const get = Reflect.get; \
         Reflect.get({ x: 1 }, 'x') + get(Array.prototype, k).call([1, 2], x => x * 2).join()",
        // A run-time key with side-effect-free arguments: the interned key.
        "const k = ['m', 'a', 'p'].join(''); const a = Array.prototype; \
         Reflect.get({ x: 1 }, 'x') + Reflect.get(a, k).call([1, 2], x => x * 2).join()",
    ] {
        assert_eq!(size(src, false), size(src, true), "`{src}`");
        same(src, "12,4");
    }
}

/// Exactly the program L1 measured whole after the constant-key exemption
/// (1.30 MB vs hello world's 0.49 MB): `{a: 1}` is not side-effect free, so
/// the call reads `Reflect.get` early and nullish-guards `Reflect` first.
/// Shaken, it is hello world plus the open `Reflect` gate's strings and the
/// bodies kept by the `"get"` lookup (`Reflect.get` and `super_get`, the
/// `Map` / `WeakMap` `get` methods and the `Map` fast path's helpers), well
/// under 40 KB on top of [`HELLO_BOUND`].
#[test]
fn reflect_get_of_a_literal_shakes() {
    let src = r#"console.log(Reflect.get({a:1},"a"))"#;
    let (shaken, full) = (size(src, false), size(src, true));
    assert!(shaken * 3 < full * 2, "{shaken} vs {full}");
    assert!(shaken < HELLO_BOUND + 40_000, "{shaken} bytes");
    same(src, "1\nundefined");
}

/// A constant-key `get` call on a nullish receiver still throws the read
/// `TypeError` before its arguments run (the early read now guards it).
#[test]
fn constant_key_get_on_nullish_throws_first() {
    same(
        "let r = null, ran = 0, m; \
         try { r.get({ a: (ran = 1) }, 'a') } catch (e) { m = e.message + ' ' + ran } m",
        "Cannot read properties of null (reading 'get') 0",
    );
}

/// A program holding the global object opens the `Proxy` and `Reflect`
/// gates, however it holds it: a computed member on `globalThis` (which
/// opens the global object, so the read is a global-object lookup, not the
/// intrinsic), a top-level `this`, an alias. The constructor found by a
/// run-time key then runs its real body, never the `unreachable` stub
/// (DESIGN.md §4.2, §6.11).
#[test]
fn global_object_by_run_time_key_opens_the_gates() {
    for (src, expected) in [
        (r#"let k = "Pro" + "xy"; new globalThis[k]({}, {})"#, "{}"),
        (
            r#"let k = "Ref" + "lect"; globalThis[k].get({a:1}, "a")"#,
            "1",
        ),
        (r#"let k = "Ref" + "lect"; this[k].get({ a: 1 }, "a")"#, "1"),
        (r#"let k = "Pro" + "xy"; new this[k]({ a: 2 }, {}).a"#, "2"),
        (
            r#"const g = globalThis; let k = "Pro" + "xy"; new g[k]({ a: 3 }, {}).a"#,
            "3",
        ),
        (
            r#"const g = globalThis; let k = "Ref" + "lect"; g[k].has({ a: 1 }, "a")"#,
            "true",
        ),
    ] {
        same(src, expected);
    }
}
