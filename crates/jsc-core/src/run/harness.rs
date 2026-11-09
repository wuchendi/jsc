//! The WASI runner: load an emitted command module into `wasmtime`, link
//! `wasi_snapshot_preview1` plus the `env` imports still backed by the host,
//! call `_start`, and map `proc_exit` to an exit code.

use wasmtime::{Caller, Config, Engine, Linker, Module, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

use super::host::{
    alloc_string, heap_exhausted, method_dispatch, parse_float_str, parse_int_str, value_to_number,
};
use super::promise::{self, Host};
use super::value::{Value, decode, format_value, to_js_string};
use crate::abi;

/// In-memory stdout and stderr of a run; clones share the same buffers.
#[derive(Debug, Clone)]
pub struct Capture {
    stdout: MemoryOutputPipe,
    stderr: MemoryOutputPipe,
}

impl Capture {
    /// Fresh empty buffers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stdout: MemoryOutputPipe::new(usize::MAX),
            stderr: MemoryOutputPipe::new(usize::MAX),
        }
    }

    /// Everything written to fd 1 so far.
    #[must_use]
    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.stdout.contents()).into_owned()
    }

    /// Everything written to fd 2 so far.
    #[must_use]
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.contents()).into_owned()
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a module's fd 1 and fd 2 go.
#[derive(Debug)]
pub enum Sinks {
    /// The process's real stdout and stderr.
    Inherit,
    /// In-memory pipes read back through the [`Capture`].
    Capture(Capture),
}

/// How a module run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOutcome {
    /// The `proc_exit` code, or `0` when `_start` returned.
    pub code: i32,
    /// Fuel consumed; `0` on an unmetered run.
    pub fuel: u64,
}

/// A finished store and instance, for reading results back.
struct Finished {
    outcome: RunOutcome,
    #[cfg(test)]
    store: Store<Host>,
    #[cfg(test)]
    instance: wasmtime::Instance,
}

/// Run the command module `wasm`.
///
/// With `fuel: Some(budget)` the engine meters fuel and
/// [`RunOutcome::fuel`] reports the amount consumed.
///
/// # Errors
///
/// Returns an error if the bytes are not a valid module, a required import or
/// `_start` is missing, or the guest traps (including running out of fuel).
pub fn run(wasm: &[u8], sinks: Sinks, fuel: Option<u64>) -> anyhow::Result<RunOutcome> {
    Ok(execute(wasm, sinks, fuel)?.outcome)
}

fn execute(wasm: &[u8], sinks: Sinks, fuel: Option<u64>) -> anyhow::Result<Finished> {
    let engine = match fuel {
        Some(_) => Engine::new(Config::new().consume_fuel(true))?,
        None => Engine::default(),
    };
    let module = Module::new(&engine, wasm)?;
    let mut builder = WasiCtxBuilder::new();
    match sinks {
        Sinks::Inherit => builder.inherit_stdout().inherit_stderr(),
        Sinks::Capture(capture) => builder.stdout(capture.stdout).stderr(capture.stderr),
    };
    let mut store = Store::new(&engine, Host::new(builder.build_p1()));
    if let Some(budget) = fuel {
        store.set_fuel(budget)?;
    }
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |host: &mut Host| &mut host.wasi)?;
    link_conversions(&mut linker)?;
    link_promises(&mut linker)?;
    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, abi::START_EXPORT_NAME)?;
    let code = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(err) => match err.downcast_ref::<I32Exit>() {
            Some(exit) => exit.0,
            None => return Err(err.into()),
        },
    };
    let consumed = match fuel {
        Some(budget) => budget.saturating_sub(store.get_fuel()?),
        None => 0,
    };
    Ok(Finished {
        outcome: RunOutcome {
            code,
            fuel: consumed,
        },
        #[cfg(test)]
        store,
        #[cfg(test)]
        instance,
    })
}

/// What a test reads back from a finished run.
#[cfg(test)]
pub(crate) struct Inspected {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    /// The final linear memory.
    pub(crate) memory: Vec<u8>,
    /// The `__exc_value` global: how layout tests reach a heap block, now that
    /// `_start` returns nothing.
    pub(crate) thrown: u64,
}

/// Test-only: run `wasm` capturing its output, then read its final state.
#[cfg(test)]
pub(crate) fn run_inspect(wasm: &[u8]) -> anyhow::Result<Inspected> {
    let capture = Capture::new();
    let Finished {
        outcome,
        mut store,
        instance,
    } = execute(wasm, Sinks::Capture(capture.clone()), None)?;
    let memory = instance
        .get_memory(&mut store, "memory")
        .map_or_else(Vec::new, |mem| mem.data(&store).to_vec());
    let thrown = instance
        .get_global(&mut store, abi::EXC_VALUE_EXPORT_NAME)
        .and_then(|global| global.get(&mut store).i64())
        .unwrap_or(0)
        .cast_unsigned();
    Ok(Inspected {
        code: outcome.code,
        stdout: capture.stdout(),
        stderr: capture.stderr(),
        memory,
        thrown,
    })
}

/// Box a host-computed `f64` as a number jsval's raw bits, canonicalizing NaN.
fn number_bits(n: f64) -> i64 {
    if n.is_nan() { abi::QNAN } else { n.to_bits() }.cast_signed()
}

/// The rendered error for a program that exhausted the capped linear memory.
const OUT_OF_MEMORY: &str = "Uncaught RangeError: out of memory";

/// Pass an import's result through, or trap when an allocation left the
/// out-of-memory sentinel in `__heap_ptr`.
fn oom_guard(caller: &mut Caller<'_, Host>, bits: i64) -> wasmtime::Result<i64> {
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

/// Decode a jsval, reading linear memory for heap operands.
fn decode_arg(caller: &mut Caller<'_, Host>, bits: i64) -> Value {
    let memory = caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory);
    decode(
        bits.cast_unsigned(),
        memory.map_or(&[][..], |mem| mem.data(&*caller)),
    )
}

/// The `__convert`/`__method`/`__now`/`__parse_int` imports.
fn link_conversions(linker: &mut Linker<Host>) -> anyhow::Result<()> {
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::CONVERT_IMPORT_NAME,
        |mut caller: Caller<'_, Host>, bits: i64, op: i32| -> wasmtime::Result<i64> {
            let value = decode_arg(&mut caller, bits);
            let result = match op {
                abi::CONVERT_OP_TO_NUMBER => number_bits(value_to_number(&value)),
                abi::CONVERT_OP_PARSE_FLOAT => number_bits(parse_float_str(&to_js_string(&value))),
                abi::CONVERT_OP_TO_STRING => {
                    if matches!(value, Value::Str(_)) {
                        return Ok(bits);
                    }
                    alloc_string(&mut caller, &to_js_string(&value))
                        .map_or(abi::UNDEFINED, |base| abi::HEAP | base)
                        .cast_signed()
                }
                _ => abi::UNDEFINED.cast_signed(),
            };
            oom_guard(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::METHOD_IMPORT_NAME,
        |mut caller: Caller<'_, Host>,
         recv: i64,
         op: i32,
         arg0: i64,
         arg1: i64|
         -> wasmtime::Result<i64> {
            let result = method_dispatch(&mut caller, recv.cast_unsigned(), op, arg0, arg1)
                .unwrap_or(abi::UNDEFINED)
                .cast_signed();
            oom_guard(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::NOW_IMPORT_NAME,
        |_caller: Caller<'_, Host>| -> f64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
        },
    )?;
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::PARSE_INT_IMPORT_NAME,
        |mut caller: Caller<'_, Host>, bits: i64, radix: i64| -> i64 {
            let text = format_value(&decode_arg(&mut caller, bits));
            let radix = value_to_number(&decode_arg(&mut caller, radix));
            number_bits(parse_int_str(&text, radix))
        },
    )?;
    Ok(())
}

/// The `__promise`/`__then` imports driving the host promise engine.
fn link_promises(linker: &mut Linker<Host>) -> anyhow::Result<()> {
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::PROMISE_IMPORT_NAME,
        |mut caller: Caller<'_, Host>, op: i32, a: i64, b: i64| -> wasmtime::Result<i64> {
            let result = promise::dispatch(&mut caller, op, a.cast_unsigned(), b.cast_unsigned())
                .cast_signed();
            oom_guard(&mut caller, result)
        },
    )?;
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::THEN_IMPORT_NAME,
        |mut caller: Caller<'_, Host>, p: i64, on_f: i64, on_r: i64| -> wasmtime::Result<i64> {
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
    Ok(())
}
