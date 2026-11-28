//! Integration tests: the binary-data gate (DESIGN.md §6.4) opens whenever
//! the program holds the global object, in every form it can take — an
//! escaping `globalThis` (which lowers to a global-object lookup), an alias
//! of it, a top-level `this` — so a binary-data constructor found by a
//! computed name runs instead of reaching a stubbed body.
#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::{exec_with, gc_stress_from_env};
use jsc_core::emit::CompileOptions;

fn options(full_runtime: bool) -> CompileOptions {
    CompileOptions {
        print_completion: false,
        gc_stress: gc_stress_from_env(),
        no_stack_guard: false,
        full_runtime,
    }
}

/// Assert `src` prints `expected` with the shaken and with the full runtime.
fn prints(src: &str, expected: &str) {
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
fn computed_global_typed_array() {
    prints(
        r#"let k = "Uint8" + "Array"; console.log(new globalThis[k](2).length)"#,
        "2",
    );
    prints(
        r#"let k = "Uint8" + "Array"; console.log(new globalThis[k](2))"#,
        "Uint8Array(2) [ 0, 0 ]",
    );
}

#[test]
fn computed_global_array_buffer() {
    prints(
        r#"let k = "Array" + "Buffer"; console.log(new globalThis[k](2).byteLength)"#,
        "2",
    );
}

#[test]
fn computed_global_data_view() {
    prints(
        r#"let k = "Data" + "View"; console.log(new globalThis[k](new ArrayBuffer(4)).byteLength)"#,
        "4",
    );
    prints(
        r#"let k = "Data" + "View"; let b = "Array" + "Buffer"; console.log(new globalThis[k](new globalThis[b](4)).byteLength)"#,
        "4",
    );
}

#[test]
fn global_alias_static_member() {
    prints(
        "let o = globalThis; console.log(new o.Int8Array(1).length)",
        "1",
    );
}

#[test]
fn top_level_this_computed_key() {
    prints(
        r#"let k = "Uint8" + "Array"; console.log(new this[k](2).length)"#,
        "2",
    );
    prints(
        r#"let k = "Array" + "Buffer"; console.log(new this[k](2).byteLength)"#,
        "2",
    );
    prints(
        r#"let k = "Data" + "View"; let b = "Array" + "Buffer"; console.log(new this[k](new this[b](4)).byteLength)"#,
        "4",
    );
    prints(
        r#"const f = (k) => new this[k](2); console.log(f("Uint8" + "Array").length)"#,
        "2",
    );
}

#[test]
fn global_alias_computed_key() {
    prints(
        r#"const g = globalThis; let k = "Uint8" + "Array"; console.log(new g[k](2).length)"#,
        "2",
    );
    prints(
        r#"const g = globalThis; let k = "Array" + "Buffer"; console.log(new g[k](2).byteLength)"#,
        "2",
    );
    prints(
        r#"const g = globalThis; let k = "Data" + "View"; let b = "Array" + "Buffer"; console.log(new g[k](new g[b](4)).byteLength)"#,
        "4",
    );
}

/// The computed key written inline (no `let`).
#[test]
fn inline_computed_global_key() {
    prints(
        r#"console.log(new globalThis["Uint8" + "Array"](2).length)"#,
        "2",
    );
    prints(
        r#"console.log(new globalThis["Array" + "Buffer"](2).byteLength)"#,
        "2",
    );
}

#[test]
fn computed_global_inside_a_function() {
    prints(
        r#"function f(k) { return new globalThis[k](3); } console.log(f("Uint8" + "Array").length)"#,
        "3",
    );
}

/// The inline forms exactly as written in the report (no spaces around `+`).
#[test]
fn inline_computed_global_key_unspaced() {
    prints(
        r#"console.log(new globalThis["Uint8"+"Array"](2).length)"#,
        "2",
    );
    prints(
        r#"console.log(new globalThis["Array"+"Buffer"](2).byteLength)"#,
        "2",
    );
}
