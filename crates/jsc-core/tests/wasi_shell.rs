//! The WASI command shape (design/runtime-memory.md §2, §3.1): imports,
//! entry point, guest rendering, the exit-code and stream contract.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, compile, exec};
use wasmparser::{ExternalKind, Parser, Payload, TypeRef};

/// `(module, name)` of every function import, in order.
fn function_imports(wasm: &[u8]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ImportSection(reader) = payload.expect("valid payload") {
            for import in reader.into_imports() {
                let import = import.expect("valid import");
                if matches!(import.ty, TypeRef::Func(_)) {
                    found.push((import.module.to_owned(), import.name.to_owned()));
                }
            }
        }
    }
    found
}

/// Every export's `(name, kind)`.
fn exports(wasm: &[u8]) -> Vec<(String, ExternalKind)> {
    let mut found = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::ExportSection(reader) = payload.expect("valid payload") {
            for export in reader {
                let export = export.expect("valid export");
                found.push((export.name.to_owned(), export.kind));
            }
        }
    }
    found
}

#[test]
fn module_imports_only_wasi_and_env_remainder() {
    let wasm = compile("console.log(1)", false).expect("compiles");
    let preview1 = "wasi_snapshot_preview1";
    let expected: Vec<(String, String)> = [
        (preview1, "fd_write"),
        (preview1, "clock_time_get"),
        (preview1, "random_get"),
        (preview1, "proc_exit"),
        ("env", "__promise"),
        ("env", "__then"),
    ]
    .iter()
    .map(|(module, name)| ((*module).to_owned(), (*name).to_owned()))
    .collect();
    assert_eq!(function_imports(&wasm), expected);
}

#[test]
fn start_is_only_entry_export() {
    // The host-needed exports of the R1 interim state (§7.1) remain until R4.
    const HOST_NEEDED: [&str; 6] = [
        "__new_string",
        "__new_promise",
        "__promise_self_error",
        "__invoke1",
        "__async_step",
        "__exc_pending",
    ];
    let wasm = compile("function f() { return 1 } f()", false).expect("compiles");
    let all = exports(&wasm);
    let has = |name: &str, kind: ExternalKind| all.iter().any(|e| e.0 == name && e.1 == kind);
    assert!(has("_start", ExternalKind::Func), "exports: {all:?}");
    assert!(has("memory", ExternalKind::Memory), "exports: {all:?}");
    for (name, _) in &all {
        assert!(
            name == "_start"
                || name == "memory"
                || name == "__exc_value"
                || HOST_NEEDED.contains(&name.as_str()),
            "unexpected export {name}"
        );
    }
}

/// One assertion per row of the §3.1 table:
///
/// | value | rendered as |
/// |---|---|
/// | number | `Number::toString`; `-0` → `0` |
/// | boolean / null / undefined | `true` / `null` / `undefined` |
/// | string | raw text, unquoted at the top level and nested |
/// | symbol | `Symbol(desc)` / `Symbol()` |
/// | array | `[ e0, e1 ]`, `[]`, hole runs, then own named props |
/// | ordinary object | `{ a: 1 }` / `{}`, symbol keys last, accessors |
/// | class instance | `A { x: 1 }` |
/// | wrapper | `[Number: 1]`, `[String: 'ab']`, ... then own props |
/// | `arguments` | `[Arguments] { '0': 1 }` |
/// | error | `Name: message`, or `Name` when the message is empty |
/// | function / class | `[Function: f]`, `[class A extends B]`, ... |
/// | Map / Set | `Map(N)` / `Set(N)` |
/// | promise | `Promise { v }`, `<pending>`, `<rejected> v` |
/// | generator | `Object [Generator]` |
/// | Date | ISO 8601 UTC, `Invalid Date` for NaN |
/// | cycles | `<ref *N> ...` / `[Circular *N]` |
/// | other type byte | `undefined` (not reachable from source) |
#[test]
fn render_parity_every_kind() {
    assert_output("console.log(1.5, -0)", "1.5 0\n");
    assert_output(
        "console.log(true, null, undefined)",
        "true null undefined\n",
    );
    assert_output("console.log('a', ['a', 'b'])", "a [ a, b ]\n");
    assert_output("console.log(Symbol('a'), Symbol())", "Symbol(a) Symbol()\n");
    assert_output(
        "let a = [1, , , 2]; a.k = 3; console.log(a, [], [1, , 2])",
        "[ 1, <2 empty items>, 2, k: 3 ] [] [ 1, <1 empty item>, 2 ]\n",
    );
    assert_output(
        "let s = Symbol('a'); let o = { [s]: 1, a: 1, get g() { return 1 }, set h(v) {}, \
         get i() { return 1 }, set i(v) {} }; console.log(o, {})",
        "{ a: 1, g: [Getter], h: [Setter], i: [Getter/Setter], [Symbol(a)]: 1 } {}\n",
    );
    assert_output(
        "class A { constructor() { this.x = 1 } } console.log(new A())",
        "A { x: 1 }\n",
    );
    assert_output(
        "console.log(new Number(1), new Number(-0), new String('ab'), new Boolean(true), \
         Object(Symbol('a')))",
        "[Number: 1] [Number: -0] [String: 'ab'] [Boolean: true] [Symbol: Symbol(a)]\n",
    );
    assert_output(
        "function f() { console.log(arguments) } f(1)",
        "[Arguments] { '0': 1 }\n",
    );
    assert_output(
        "console.log(new TypeError('x'), new Error())",
        "TypeError: x Error\n",
    );
    assert_output(
        "function f() {} class A {} class B extends A {} \
         console.log(f, [() => 1][0], A, B, [class {}][0])",
        "[Function: f] [Function (anonymous)] [class A] [class B extends A] [class (anonymous)]\n",
    );
    assert_output(
        "console.log(new Map([[1, 2]]), new Set())",
        "Map(1) Set(0)\n",
    );
    assert_output(
        "let r = Promise.reject(2); r.catch(() => 0); \
         console.log(Promise.resolve(1), new Promise(() => {}), r)",
        "Promise { 1 } Promise { <pending> } Promise { <rejected> 2 }\n",
    );
    assert_output("function* g() {} console.log(g())", "Object [Generator]\n");
    assert_output(
        "console.log(new Date(0), new Date(NaN), new Date(-1e14))",
        "1970-01-01T00:00:00.000Z Invalid Date -001199-02-15T14:13:20.000Z\n",
    );
    assert_output(
        "let a = [1]; a[1] = a; console.log(a)",
        "<ref *1> [ 1, [Circular *1] ]\n",
    );
}

#[test]
fn render_cycles_ref_numbering() {
    assert_output(
        "let a = []; a[0] = a; let o = {}; o.s = o; console.log([a, o])",
        "[ <ref *1> [ [Circular *1] ], <ref *2> { s: [Circular *2] } ]\n",
    );
}

#[test]
fn render_lone_surrogate_as_fffd() {
    assert_output("console.log('\\uD800a')", "\u{FFFD}a\n");
}

#[test]
fn uncaught_goes_to_fd2_exit1() {
    let done = exec("console.log('before'); throw new TypeError('x')", false).expect("runs");
    assert_eq!(done.code, 1);
    assert_eq!(done.stdout, "before\n");
    assert_eq!(done.stderr, "Uncaught TypeError: x\n");
}

#[test]
fn unhandled_rejection_after_drain_exit1() {
    let done = exec(
        "Promise.reject(7); Promise.resolve().then(() => console.log('drained'))",
        false,
    )
    .expect("runs");
    assert_eq!(done.code, 1);
    assert_eq!(done.stdout, "drained\n");
    assert_eq!(done.stderr, "Uncaught (in promise) 7\n");
}

#[test]
fn oom_static_line_exit1() {
    let huge = "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } let a = [...s]; a.length";
    let done = exec(huge, false).expect("runs");
    assert_eq!(done.code, 1);
    assert_eq!(done.stderr, "Uncaught RangeError: out of memory\n");
}

/// `__alloc`'s `proc_exit(1)` inside a microtask leaves through the host's
/// re-entrant call: the program ends there and no later job runs.
#[test]
fn proc_exit_propagates_from_microtask() {
    let src = "console.log('sync'); \
               Promise.resolve() \
                 .then(() => { let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } \
                               let a = [...s]; console.log('after oom', a.length) }) \
                 .then(() => console.log('next job'), () => console.log('next job'));";
    let done = exec(src, false).expect("runs");
    assert_eq!(done.code, 1);
    assert_eq!(done.stdout, "sync\n");
    assert_eq!(done.stderr, "Uncaught RangeError: out of memory\n");
}

/// One program per builtin family under `emit/builtins/`; `compile` fails the
/// test on an invalid module. Proxy and typed arrays do not exist yet.
#[test]
fn every_compiled_module_validates() {
    for src in [
        "Object.keys({ a: 1 }); Object.defineProperty({}, 'a', { value: 1 })",
        "Reflect.ownKeys({})",
        "function f(a) { return a } f.call(null, 1); f.apply(null, [1]); f.bind(null)",
        "[3, 1].sort().map((x) => x).concat([1]).join(); Array.from([1])",
        "'ab'.toUpperCase().padStart(4).split('').concat(String.fromCharCode(65))",
        "(1.5).toFixed(1); Number.isInteger(1); Number('1')",
        "new Boolean(true).valueOf()",
        "Symbol('a').description; Symbol.iterator; Symbol.for('a')",
        "Math.sqrt(2) + Math.max(1, 2)",
        "JSON.parse(JSON.stringify({ a: [1] }))",
        "new Date(0).toISOString(); Date.now()",
        "new Map([[1, 2]]).get(1)",
        "new Set([1]).has(1)",
        "Promise.all([1]).then((x) => x).catch((e) => e).finally(() => 0)",
        "function* g() { yield 1 } [...g()]",
        "async function f() { await 1 } f()",
        "async function* g() { yield 1 } (async () => { for await (const x of g()) x })()",
        "[1].values().map((x) => x).toArray()",
        "new Error('e'); new TypeError('e'); new RangeError('e'); new AggregateError([])",
        "parseInt('1'); parseFloat('1.5'); isNaN(1); isFinite(1); globalThis",
    ] {
        compile(src, false).expect("compiles");
        compile(src, true).expect("compiles with print_completion");
    }
}
