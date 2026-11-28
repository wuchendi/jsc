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

/// The shaken `console.log("hi")` module: 432 056 bytes at 11ca248, plus
/// about 200 KB the binary-data and `BigInt` work added since (the typed-array
/// constructors and `%TypedArray%.prototype` accessors are kept by the
/// `"constructor"` / accessor rules), plus a few KB of `Proxy` / `Reflect`
/// strings. Above this bound a runtime body with a run-time-key use is kept
/// again: the `Reflect` merge left every program at the full runtime
/// (~1.18 MB) because the renderer's `"constructor"` lookup keeps the `Map`
/// constructor, its `"set"` lookup kept `Reflect.set`, and that body's
/// run-time-key use left the module whole (DESIGN.md §6.11).
const HELLO_BOUND: usize = 700_000;

#[test]
fn hello_world_stays_near_the_base() {
    let src = "console.log('hi')";
    let shaken = size(src, false);
    assert!(shaken < HELLO_BOUND, "hello world is {shaken} bytes");
    assert!(shaken * 3 < size(src, true) * 2);
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
