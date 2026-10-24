//! Integration tests: guest linear memory grows on demand, and exhausting the
//! capped heap surfaces as `Uncaught RangeError: out of memory` (no trap
//! backtrace).
#![allow(clippy::expect_used)]

use jsc_core::run::compile_and_run;
mod common;
use common::{assert_number, assert_str, assert_uncaught};

/// A program that builds a 64 MiB string by doubling (touching ~128 MiB), then
/// spreads it into an array whose single `__alloc` request (8 bytes per element,
/// 512 MiB) exceeds the memory cap without ever being touched.
const HUGE: &str = "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } let a = [...s];";

/// Small string allocations past the initial page no longer trap.
#[test]
fn many_small_strings_grow_memory() {
    assert_number(
        "let c = 0; for (let i = 0; i < 5000; i++) { let s = 'k' + i; c = c + 1 } c",
        5000.0,
    );
}

/// Static data (string literals and the intern table) past the first page
/// still fits the initial memory.
#[test]
fn static_data_beyond_one_page() {
    let literals: Vec<String> = (0..3000)
        .map(|i| format!("'literal number {i:05}'"))
        .collect();
    let src = format!("let a = [{}]; a.length", literals.join(", "));
    assert_number(&src, 3000.0);
}

/// Static data (string literals, the realm image, the intern table) past the
/// first 64 KiB page is covered by the initial memory, so instantiation does
/// not trap.
#[test]
fn static_bindings_beyond_one_page() {
    let src: String = (0..3000)
        .map(|i| format!("let s{i} = 'literal number {i} of a long program'; "))
        .chain(["s2999.length".to_owned()])
        .collect();
    assert_number(&src, 37.0);
}

/// A million small string allocations complete.
#[test]
fn million_small_strings() {
    assert_number(
        "let c = 0; for (let i = 0; i < 1000000; i++) { let s = 'k' + i; c = c + 1 } c",
        1_000_000.0,
    );
}

/// A million small objects complete and stay readable.
#[test]
fn million_small_objects() {
    assert_number(
        "let c = 0; let last = {i: 0}; \
         for (let i = 0; i < 1000000; i++) { let o = {i: i}; last = o; c = c + o.i } \
         c + last.i",
        499_999_500_000.0 + 999_999.0,
    );
}

/// An array of objects built with `push` (which reallocates per push) grows the
/// heap well past the initial page and reads back correctly.
#[test]
fn pushed_objects_read_back() {
    assert_number(
        "let a = []; for (let i = 0; i < 2000; i++) { a.push({i: i}) } a.length + a[1999].i",
        3999.0,
    );
}

/// Host-allocated strings and arrays (`String(n)`, `__method` ops) still work
/// after the guest heap has grown past the initial page.
#[test]
fn host_allocations_after_growth() {
    let grow = "let c = 0; for (let i = 0; i < 20000; i++) { let s = 'k' + i; c = c + 1 }";
    assert_str(&format!("{grow} String(c)"), "20000");
    assert_str(
        &format!("{grow} [1, 2, 3].slice(1).join('-') + 'ab'.toUpperCase()"),
        "2-3AB",
    );
}

/// Promises (host-allocated blocks) still settle after the heap has grown.
#[test]
fn promises_after_growth() {
    assert_number(
        "let c = 0; for (let i = 0; i < 20000; i++) { let s = 'k' + i; c = c + 1 } \
         let r = await Promise.resolve(c).then((v) => v + 1); r",
        20001.0,
    );
}

/// Exhausting the capped heap is an uncaught `RangeError: out of memory`.
#[test]
fn huge_allocation_is_out_of_memory() {
    assert_uncaught(&format!("{HUGE} a.length"), "RangeError: out of memory");
}

/// The out-of-memory error carries no wasmtime trap backtrace.
#[test]
fn out_of_memory_has_no_backtrace() {
    let err = compile_and_run(&format!("{HUGE} a.length")).expect_err("should run out of memory");
    let text = format!("{err:?}");
    assert!(
        !text.contains("wasm backtrace"),
        "unexpected backtrace: {text}"
    );
    assert!(
        !text.contains("unreachable"),
        "unexpected trap text: {text}"
    );
}

/// An out-of-memory inside `try` is not catchable (the heap is exhausted, so
/// the program terminates with the out-of-memory error instead).
#[test]
fn out_of_memory_inside_try_terminates() {
    assert_uncaught(
        &format!("try {{ {HUGE} }} catch (e) {{ 0 }} 1"),
        "RangeError: out of memory",
    );
}

/// A program whose guest strings fill ~448 MiB of the 512 MiB cap, so the next
/// 64 MiB HOST allocation (`s.slice(1)`, built through the guest
/// `__new_string` export) runs out of memory. Shared verbatim with the shim
/// parity test in `packages/jsc/tests/run.test.ts`.
const HOST_OOM: &str = "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } \
     let t = s + s; let u = t + s; s.slice(1).length";

/// A host-initiated allocation past the cap is the same uncaught
/// `RangeError: out of memory` as a guest one.
#[test]
fn host_initiated_out_of_memory() {
    assert_uncaught(HOST_OOM, "RangeError: out of memory");
}

/// A store far past the end makes the array sparse: the gap is never
/// allocated, so a length of a million costs no ~8 MB element store.
#[test]
fn sparse_store_does_not_allocate_the_gap() {
    let program = jsc_core::frontend::parse(
        "const a = []; a[1e6] = 1; a.length",
        jsc_core::frontend::Dialect::Js,
    )
    .expect("parses");
    let ir = jsc_core::codegen::lower(&program).expect("lowers");
    let wasm = jsc_core::emit::emit(&ir);
    let (bits, memory) = jsc_core::run::run(&wasm).expect("runs");
    assert_eq!(f64::from_bits(bits), 1_000_001.0);
    assert!(
        memory.len() < 4 * 1024 * 1024,
        "linear memory grew to {} bytes",
        memory.len()
    );
}

/// Run `src` and return its number result and final linear-memory size.
fn run_with_memory(src: &str) -> (f64, usize) {
    let program = jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js).expect("parses");
    let ir = jsc_core::codegen::lower(&program).expect("lowers");
    let wasm = jsc_core::emit::emit(&ir);
    let (bits, memory) = jsc_core::run::run(&wasm).expect("runs");
    (f64::from_bits(bits), memory.len())
}

/// The dense bound is a gap of 1024 holes past the length, whatever the store
/// capacity: stores that each land 1024 past the end (or roughly double the
/// index, test262 `S15.4_A1.1_T10`) stay sparse and small, while a gap of
/// 1023 still grows the store densely.
#[test]
fn dense_growth_is_bounded_by_the_gap() {
    let (len, sparse) = run_with_memory(
        "const a = []; for (let k = 0; k < 1000; k++) a[a.length + 1024] = k; a.length",
    );
    assert_eq!(len, 1_025_000.0);
    assert!(
        sparse < 4 * 1024 * 1024,
        "sparse stores grew memory to {sparse} bytes"
    );
    let (len, dense) = run_with_memory(
        "const a = []; for (let k = 0; k < 1000; k++) a[a.length + 1023] = k; a.length",
    );
    assert_eq!(len, 1_024_000.0);
    assert!(
        dense > 8 * 1024 * 1024,
        "dense stores only grew memory to {dense} bytes"
    );
    let (len, doubling) = run_with_memory(
        "const x = []; let k = 1; for (let i = 0; i < 32; i++) { k = k * 2; x[k - 2] = k } x.length",
    );
    assert_eq!(len, 4_294_967_295.0);
    assert!(
        doubling < 4 * 1024 * 1024,
        "doubling stores grew memory to {doubling} bytes"
    );
}
