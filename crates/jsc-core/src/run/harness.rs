//! The execution harness: load an emitted module into `wasmtime`, wire up the
//! host imports (`print`/`__convert`/`__method`), call `main`, and surface its
//! result — plus the JS-source -> [`Value`] pipeline entry points.

use std::io::Write;

use wasmtime::{Caller, Engine, Linker, Module, Store};

use super::host::{
    heap_exhausted, host_alloc_string, method_dispatch, parse_float_str, parse_int_str,
    value_to_number,
};
use super::promise::{self, Host};
use crate::abi;

use super::{HEAP, Value, decode, format_value};

/// The `print` host-import flags (see [`abi`]): a value plus a trailing
/// space, or a value plus a newline.
const PRINT_FLAG_SEP: i32 = abi::PRINT_FLAG_SEP;
const PRINT_FLAG_END: i32 = abi::PRINT_FLAG_END;

/// The `__convert` host-import op flags (see [`abi`]): stringify the
/// argument, coerce it to a number, or parse a leading int/float from its string
/// form.
const CONVERT_OP_TO_STRING: i32 = abi::CONVERT_OP_TO_STRING;
const CONVERT_OP_TO_NUMBER: i32 = abi::CONVERT_OP_TO_NUMBER;
const CONVERT_OP_PARSE_INT: i32 = abi::CONVERT_OP_PARSE_INT;
const CONVERT_OP_PARSE_FLOAT: i32 = abi::CONVERT_OP_PARSE_FLOAT;

/// Box a host-computed `f64` as a number jsval's raw bits, mapping every NaN to
/// the canonical [`abi::QNAN`] (the guest-side twin of `emit::emit_box_f64`).
fn number_bits(n: f64) -> i64 {
    if n.is_nan() { abi::QNAN } else { n.to_bits() }.cast_signed()
}

/// The NaN-boxed `undefined` jsval bits (see [`abi::UNDEFINED`]), returned by the
/// `__method` host import for a type-mismatched receiver or a failed allocation.
const UNDEFINED_BITS: u64 = abi::UNDEFINED;

/// The names under which `crate::emit` exports the exception-unwinding globals
/// (see [`abi`]): the pending flag (`$exc_pending`, i32) and the in-flight
/// value (`$exc_value`, i64). After `main` returns, a set flag means the program
/// threw an exception that was never caught.
const EXC_PENDING_EXPORT_NAME: &str = abi::EXC_PENDING_EXPORT_NAME;
const EXC_VALUE_EXPORT_NAME: &str = abi::EXC_VALUE_EXPORT_NAME;

/// The rendered error for a program that exhausted the capped linear memory.
const OUT_OF_MEMORY: &str = "Uncaught RangeError: out of memory";

/// Pass a host import's result `bits` through, or trap the guest when the import
/// (or an earlier allocation) left the out-of-memory sentinel in `__heap_ptr`;
/// the run stage then reports [`OUT_OF_MEMORY`].
fn oom_guard<W>(caller: &mut Caller<'_, W>, bits: i64) -> wasmtime::Result<i64> {
    match caller
        .get_export(abi::HEAP_PTR_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_global)
    {
        Some(global) if heap_exhausted(&mut *caller, global) => {
            Err(wasmtime::Error::msg(OUT_OF_MEMORY))
        }
        _ => Ok(bits),
    }
}

/// Load `wasm`, instantiate it, and call the exported `main` function.
///
/// The module must export a function named `main` with signature `() -> i64`,
/// as produced by [`crate::emit::emit`]. It returns the raw jsval bits together
/// with a snapshot of the module's exported `"memory"` (needed to resolve
/// pointer jsvals; empty if the module has no memory export). Decode the pair
/// with [`decode`]. Any failure to compile, instantiate, resolve, or call the
/// export is returned as an [`anyhow::Error`].
///
/// # Errors
///
/// Returns an error if the bytes are not a valid module, if instantiation
/// fails, if the `main` export is missing or has the wrong signature, or if the
/// call traps.
pub fn run(wasm: &[u8]) -> anyhow::Result<(u64, Vec<u8>)> {
    // Default sink: `console.log` output streams to the process's real stdout
    // during execution.
    let (bits, memory, _out) = run_with_sink(wasm, std::io::stdout())?;
    Ok((bits, memory))
}

/// Like [`run`], but capture `console.log` output into a string instead of
/// writing to stdout. Returns the raw `main` result bits, the linear
/// memory snapshot, and everything `console.log` printed (in execution order).
///
/// Used by [`compile_and_run_capturing`] and the test suite to assert printed
/// output without touching the process's real stdout.
///
/// # Errors
///
/// Returns an error under the same conditions as [`run`].
pub fn run_capturing(wasm: &[u8]) -> anyhow::Result<(u64, Vec<u8>, String)> {
    let (bits, memory, out) = run_with_sink(wasm, Vec::<u8>::new())?;
    Ok((bits, memory, String::from_utf8_lossy(&out).into_owned()))
}

/// Instantiate `wasm` with a `wasmtime::Linker` that supplies the host `print`
/// import (backing `console.log`), call `main`, and return its result
/// bits, a linear-memory snapshot, and the output sink `out`.
///
/// The `print` import reads the caller's linear memory via [`Caller`] and reuses
/// [`decode`] + [`format_value`], so a printed jsval renders exactly like a final
/// program value. `out` is any [`Write`] sink (real stdout, or an in-memory
/// buffer for capture); it is threaded through the [`Store`] and handed back so
/// captured output can be recovered.
// One straight-line harness: engine setup, the six host imports (print,
// __convert, __method, __promise, __then, __now), the call, and the after-`main`
// microtask drain — length reflects the import roster, not branching.
#[allow(clippy::too_many_lines)]
fn run_with_sink<W: Write + Send + 'static>(
    wasm: &[u8],
    out: W,
) -> anyhow::Result<(u64, Vec<u8>, W)> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    let mut store = Store::new(&engine, Host::new(out));
    let mut linker = Linker::new(&engine);
    linker.func_wrap(
        "env",
        "print",
        |mut caller: Caller<'_, Host<W>>, bits: i64, flag: i32| {
            // Format the jsval (unless the flag is newline-only), reading linear
            // memory for pointer jsvals, then write the piece to the sink. The
            // memory borrow ends before the mutable sink borrow begins.
            let piece = if flag == PRINT_FLAG_SEP || flag == PRINT_FLAG_END {
                let text = match caller
                    .get_export("memory")
                    .and_then(wasmtime::Extern::into_memory)
                {
                    Some(mem) => format_value(&decode(bits.cast_unsigned(), mem.data(&caller))),
                    None => format_value(&decode(bits.cast_unsigned(), &[])),
                };
                match flag {
                    PRINT_FLAG_SEP => format!("{text} "),
                    _ => format!("{text}\n"),
                }
            } else {
                // Newline-only (zero-argument `console.log()`).
                "\n".to_owned()
            };
            // A write failure to the sink should not trap the guest; ignore it.
            let _ = caller.data_mut().sink.write_all(piece.as_bytes());
        },
    )?;
    // The `__convert` import backs `String`/`Number`/`parseInt`/`parseFloat`.
    // It decodes the argument jsval (reading linear memory for heap-string
    // operands), reuses `format_value`/`value_to_number` and the parse helpers, and
    // returns the converted jsval. `String` allocates a fresh heap string in guest
    // memory via the exported heap-pointer global; the rest return an immediate
    // number jsval and never allocate.
    linker.func_wrap(
        "env",
        "__convert",
        |mut caller: Caller<'_, Host<W>>, bits: i64, op: i32| -> wasmtime::Result<i64> {
            let ubits = bits.cast_unsigned();
            // Decode the argument (heap-string operands need the memory snapshot).
            let value = match caller
                .get_export("memory")
                .and_then(wasmtime::Extern::into_memory)
            {
                Some(mem) => decode(ubits, mem.data(&caller)),
                None => decode(ubits, &[]),
            };
            let result = match op {
                CONVERT_OP_TO_NUMBER => number_bits(value_to_number(&value)),
                // `parseInt`/`parseFloat` operate on the argument's string form.
                CONVERT_OP_PARSE_INT => number_bits(parse_int_str(&format_value(&value))),
                CONVERT_OP_PARSE_FLOAT => number_bits(parse_float_str(&format_value(&value))),
                CONVERT_OP_TO_STRING => {
                    // A string argument is returned unchanged (no allocation);
                    // anything else is formatted and allocated as a heap string.
                    if matches!(value, Value::Str(_)) {
                        return Ok(bits);
                    }
                    let text = format_value(&value);
                    match host_alloc_string(&mut caller, &text) {
                        Some(base) => (HEAP | base).cast_signed(),
                        // Allocation failed: out of memory traps via
                        // `oom_guard`; a missing export yields `undefined`.
                        None => UNDEFINED_BITS.cast_signed(),
                    }
                }
                // An unknown op flag never reaches here from `crate::emit`; yield
                // `undefined` to keep the import total.
                _ => UNDEFINED_BITS.cast_signed(),
            };
            oom_guard(&mut caller, result)
        },
    )?;
    // The `__method` import backs the non-callback array/string methods:
    // `indexOf`/`includes`/`join`/`slice` on arrays and
    // `toUpperCase`/`toLowerCase`/`indexOf`/`slice`/`charAt` on strings. It
    // snapshots linear memory to decode the receiver + arguments, branches on the
    // receiver's TYPE BYTE for the overloaded `indexOf`/`slice`, and returns the
    // result jsval — allocating a fresh heap string/array in guest memory (via the
    // exported heap-pointer global) for the methods that build one.
    linker.func_wrap(
        "env",
        "__method",
        |mut caller: Caller<'_, Host<W>>,
         recv: i64,
         op: i32,
         arg0: i64,
         arg1: i64|
         -> wasmtime::Result<i64> {
            let result = method_dispatch(&mut caller, recv.cast_unsigned(), op, arg0, arg1)
                .unwrap_or(UNDEFINED_BITS)
                .cast_signed();
            oom_guard(&mut caller, result)
        },
    )?;
    // The host promise imports: `__promise` backs Promise.resolve/reject,
    // top-level `await`, and `p.finally`; `__then` backs `p.then`/`p.catch`. Both
    // drive the host promise engine (`super::promise`).
    linker.func_wrap(
        "env",
        "__promise",
        |mut caller: Caller<'_, Host<W>>, op: i32, a: i64, b: i64| -> wasmtime::Result<i64> {
            let result = promise::dispatch(&mut caller, op, a.cast_unsigned(), b.cast_unsigned())
                .cast_signed();
            oom_guard(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        "env",
        "__then",
        |mut caller: Caller<'_, Host<W>>, p: i64, on_f: i64, on_r: i64| -> wasmtime::Result<i64> {
            let result = promise::then(
                &mut caller,
                p.cast_unsigned(),
                on_f.cast_unsigned(),
                on_r.cast_unsigned(),
            )
            .cast_signed();
            oom_guard(&mut caller, result)
        },
    )?;
    // The `__now` import (`Date`): the host wall clock backing
    // `Date.now()` / `new Date()`, returning milliseconds since the Unix epoch as
    // an f64. A clock stepped backwards before the epoch yields `0` rather than
    // trapping.
    linker.func_wrap("env", "__now", |_caller: Caller<'_, Host<W>>| -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    })?;
    let instance = linker.instantiate(&mut store, &module)?;
    let main = instance.get_typed_func::<(), i64>(&mut store, "main")?;
    // Out of memory (the sticky `__heap_ptr` sentinel, set by a failed guest or
    // host grow) wins over the trap or result it caused: report it as an
    // uncaught `RangeError` with no trap backtrace.
    let exhausted = |store: &mut Store<Host<W>>| {
        instance
            .get_global(&mut *store, abi::HEAP_PTR_EXPORT_NAME)
            .is_some_and(|g| heap_exhausted(&mut *store, g))
    };
    let called = main.call(&mut store, ());
    if exhausted(&mut store) {
        return Err(anyhow::anyhow!(OUT_OF_MEMORY));
    }
    let result = called?;
    // Drain any microtasks scheduled by `.then`/`.catch`/`.finally` but never
    // awaited, UNLESS `main` itself completed abruptly with a pending
    // exception (an uncaught throw or a top-level `await` rejection) — in which
    // case the program has already terminated and the queue must not run.
    let main_pending = instance
        .get_global(&mut store, EXC_PENDING_EXPORT_NAME)
        .and_then(|g| g.get(&mut store).i32())
        .unwrap_or(0);
    if main_pending == 0 {
        promise::drain_after_main(&mut store, &instance);
        if exhausted(&mut store) {
            return Err(anyhow::anyhow!(OUT_OF_MEMORY));
        }
    }
    // Snapshot linear memory AFTER the drain so any promise settled by a microtask
    // renders its final state and pointer jsvals resolve against current memory.
    let memory = match instance.get_memory(&mut store, "memory") {
        Some(mem) => mem.data(&store).to_vec(),
        None => Vec::new(),
    };
    // Uncaught-exception check: if `main` returned with `$exc_pending`
    // set, the program threw an exception that propagated all the way out. Render
    // the in-flight `$exc_value` (resolving heap pointers against the snapshot)
    // and surface it as a distinct error instead of a normal result — never a
    // panic. Modules always export these globals; a missing one reads as 0.
    let pending = instance
        .get_global(&mut store, EXC_PENDING_EXPORT_NAME)
        .and_then(|g| g.get(&mut store).i32())
        .unwrap_or(0);
    if pending != 0 {
        let value_bits = instance
            .get_global(&mut store, EXC_VALUE_EXPORT_NAME)
            .and_then(|g| g.get(&mut store).i64())
            .unwrap_or(0)
            .cast_unsigned();
        let rendered = format_value(&decode(value_bits, &memory));
        return Err(anyhow::anyhow!("Uncaught {rendered}"));
    }
    Ok((result.cast_unsigned(), memory, store.into_data().sink))
}

/// Compile JavaScript `src` all the way to a value: parse, lower, emit, run.
///
/// This is the full JS-source -> [`Value`] pipeline, chaining
/// [`crate::frontend::parse`], [`crate::codegen::lower`], [`crate::emit::emit`],
/// [`run`], and [`decode`].
///
/// # Errors
///
/// Returns an error if the source fails to parse, cannot be lowered to the
/// supported IR, or if executing the emitted module fails.
pub fn compile_and_run(src: &str) -> anyhow::Result<Value> {
    let program = crate::frontend::parse(src)?;
    let ir = crate::codegen::lower(&program)?;
    let wasm = crate::emit::emit(&ir);
    let (bits, memory) = run(&wasm)?;
    Ok(decode(bits, &memory))
}

/// Compile and run JavaScript `src`, capturing everything `console.log` prints
/// instead of writing it to stdout.
///
/// Returns the program's final [`Value`] (the last expression statement's value)
/// paired with the captured `console.log` output, in execution order. This is
/// the same pipeline as [`compile_and_run`]; only the output sink differs.
///
/// # Errors
///
/// Returns an error under the same conditions as [`compile_and_run`].
pub fn compile_and_run_capturing(src: &str) -> anyhow::Result<(Value, String)> {
    let program = crate::frontend::parse(src)?;
    let ir = crate::codegen::lower(&program)?;
    let wasm = crate::emit::emit(&ir);
    let (bits, memory, output) = run_capturing(&wasm)?;
    Ok((decode(bits, &memory), output))
}
