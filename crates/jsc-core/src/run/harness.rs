//! The WASI runner: load an emitted command module into `wasmtime`, link
//! `wasi_snapshot_preview1` plus the `env` imports still backed by the host,
//! call `_start`, and map `proc_exit` to an exit code.

use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use wasmtime::{Caller, Config, Engine, Linker, Memory, Module, Store};

use super::host::{alloc_string, method_dispatch, parse_float_str, parse_int_str, value_to_number};
use super::promise::{self, Host};
use super::value::{Value, decode, to_js_string};
use crate::abi;

const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_FAULT: i32 = 21;
const ERRNO_INVAL: i32 = 28;
const ERRNO_IO: i32 = 29;

const FD_STDOUT: i32 = 1;
const FD_STDERR: i32 = 2;

const CLOCK_REALTIME: i32 = 0;
const CLOCK_MONOTONIC: i32 = 1;

/// Size of one `ciovec` (`buf: u32, buf_len: u32`) in guest memory.
const IOVEC_BYTES: u64 = 8;

/// The error `proc_exit` raises to unwind the guest back to the runner.
#[derive(Debug)]
struct Exit(i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// The per-run state behind the `wasi_snapshot_preview1` imports.
#[derive(Debug)]
pub(super) struct WasiCtx {
    sinks: Sinks,
    /// The origin of the monotonic clock.
    started: Instant,
    /// The xorshift64* state behind `random_get`; never zero.
    random: u64,
}

impl WasiCtx {
    fn new(sinks: Sinks) -> Self {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0x9E37_79B9_7F4A_7C15);
        Self {
            sinks,
            started: Instant::now(),
            random: hasher.finish() | 1,
        }
    }

    /// Append `bytes` to fd 1 or fd 2.
    fn write(&self, fd: i32, bytes: &[u8]) -> Result<(), i32> {
        match &self.sinks {
            Sinks::Inherit if fd == FD_STDOUT => write_flushed(std::io::stdout().lock(), bytes),
            Sinks::Inherit => write_flushed(std::io::stderr().lock(), bytes),
            Sinks::Capture(capture) => {
                let target = if fd == FD_STDOUT {
                    &capture.stdout
                } else {
                    &capture.stderr
                };
                let mut buffer = lock(target);
                buffer.try_reserve(bytes.len()).map_err(|_| ERRNO_IO)?;
                buffer.extend_from_slice(bytes);
                Ok(())
            }
        }
    }

    /// Overwrite `buf` with the next bytes of the random stream.
    fn fill_random(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(size_of::<u64>()) {
            self.random ^= self.random >> 12;
            self.random ^= self.random << 25;
            self.random ^= self.random >> 27;
            let bytes = self
                .random
                .wrapping_mul(0x2545_F491_4F6C_DD1D)
                .to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

fn write_flushed(mut out: impl Write, bytes: &[u8]) -> Result<(), i32> {
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(|_| ERRNO_IO)
}

/// A captured stream; a panic while holding it cannot leave the bytes invalid.
fn lock(buffer: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    buffer.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Prepare the failure of a guest call made from inside a host import for
/// propagation: a `proc_exit` passes through untouched, so the runner maps it
/// to the exit code; a trap gets a note on where it happened.
pub(super) fn reentry_error(err: wasmtime::Error) -> wasmtime::Error {
    if err.downcast_ref::<Exit>().is_some() {
        err
    } else {
        err.context("guest trap during a host re-entry")
    }
}

/// In-memory stdout and stderr of a run; clones share the same buffers.
#[derive(Debug, Clone)]
pub struct Capture {
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// Fresh empty buffers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stdout: Arc::default(),
            stderr: Arc::default(),
        }
    }

    /// Everything written to fd 1 so far.
    #[must_use]
    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&lock(&self.stdout)).into_owned()
    }

    /// Everything written to fd 2 so far.
    #[must_use]
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&lock(&self.stderr)).into_owned()
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
    /// In-memory buffers read back through the [`Capture`].
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
    let mut store = Store::new(&engine, Host::new(WasiCtx::new(sinks)));
    if let Some(budget) = fuel {
        store.set_fuel(budget)?;
    }
    let mut linker = Linker::new(&engine);
    link_wasi(&mut linker)?;
    link_conversions(&mut linker)?;
    link_promises(&mut linker)?;
    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, abi::START_EXPORT_NAME)?;
    let code = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(err) => match err.downcast_ref::<Exit>() {
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

/// The guest's exported linear memory.
fn guest_memory(caller: &mut Caller<'_, Host>) -> Option<Memory> {
    caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
}

/// The byte range of a guest buffer, or `fault` when it does not fit in
/// `data`.
fn span(data: &[u8], ptr: i32, len: u64) -> Result<Range<usize>, i32> {
    let start = usize::try_from(ptr.cast_unsigned()).map_err(|_| ERRNO_FAULT)?;
    let len = usize::try_from(len).map_err(|_| ERRNO_FAULT)?;
    let end = start.checked_add(len).ok_or(ERRNO_FAULT)?;
    if end > data.len() {
        return Err(ERRNO_FAULT);
    }
    Ok(start..end)
}

/// The guest buffer described by entry `index` of the iovec array at `iovs`.
fn iovec(data: &[u8], iovs: i32, index: u32) -> Result<&[u8], i32> {
    let field = |offset: u64| -> Result<u32, i32> {
        let at = u64::from(iovs.cast_unsigned()) + u64::from(index) * IOVEC_BYTES + offset;
        let at = u32::try_from(at).map_err(|_| ERRNO_FAULT)?;
        let range = span(data, at.cast_signed(), 4)?;
        let bytes = data[range].first_chunk().ok_or(ERRNO_FAULT)?;
        Ok(u32::from_le_bytes(*bytes))
    };
    let ptr = field(0)?;
    let len = field(4)?;
    Ok(&data[span(data, ptr.cast_signed(), u64::from(len))?])
}

/// `fd_write`: every iovec and `nwritten` are validated before any output, so
/// a fault writes nothing.
fn fd_write(
    data: &mut [u8],
    wasi: &WasiCtx,
    (fd, iovs, iovs_len, nwritten): (i32, i32, i32, i32),
) -> Result<(), i32> {
    if fd != FD_STDOUT && fd != FD_STDERR {
        return Err(ERRNO_BADF);
    }
    let iovs_len = iovs_len.cast_unsigned();
    let mut total: u32 = 0;
    for index in 0..iovs_len {
        let len = u32::try_from(iovec(data, iovs, index)?.len()).map_err(|_| ERRNO_INVAL)?;
        total = total.checked_add(len).ok_or(ERRNO_INVAL)?;
    }
    let result = span(data, nwritten, 4)?;
    for index in 0..iovs_len {
        wasi.write(fd, iovec(data, iovs, index)?)?;
    }
    data[result].copy_from_slice(&total.to_le_bytes());
    Ok(())
}

/// `clock_time_get`: nanoseconds as a little-endian `u64` at `time`.
fn clock_time_get(data: &mut [u8], wasi: &WasiCtx, id: i32, time: i32) -> Result<(), i32> {
    let nanos = match id {
        CLOCK_REALTIME => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
        CLOCK_MONOTONIC => wasi.started.elapsed().as_nanos(),
        _ => return Err(ERRNO_INVAL),
    };
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    let range = span(data, time, 8)?;
    data[range].copy_from_slice(&nanos.to_le_bytes());
    Ok(())
}

/// The errno of a WASI call's result.
fn errno(result: Result<(), i32>) -> i32 {
    result.err().unwrap_or(ERRNO_SUCCESS)
}

/// The `wasi_snapshot_preview1` imports: exactly the four an emitted module
/// declares, so a module needing any other fails to instantiate.
fn link_wasi(linker: &mut Linker<Host>) -> anyhow::Result<()> {
    linker.func_wrap(
        abi::WASI_MODULE,
        abi::WASI_FD_WRITE_NAME,
        |mut caller: Caller<'_, Host>, fd: i32, iovs: i32, iovs_len: i32, nwritten: i32| -> i32 {
            let Some(memory) = guest_memory(&mut caller) else {
                return ERRNO_FAULT;
            };
            let (data, host) = memory.data_and_store_mut(&mut caller);
            errno(fd_write(data, &host.wasi, (fd, iovs, iovs_len, nwritten)))
        },
    )?;
    linker.func_wrap(
        abi::WASI_MODULE,
        abi::WASI_CLOCK_TIME_GET_NAME,
        |mut caller: Caller<'_, Host>, id: i32, _precision: i64, time: i32| -> i32 {
            let Some(memory) = guest_memory(&mut caller) else {
                return ERRNO_FAULT;
            };
            let (data, host) = memory.data_and_store_mut(&mut caller);
            errno(clock_time_get(data, &host.wasi, id, time))
        },
    )?;
    linker.func_wrap(
        abi::WASI_MODULE,
        abi::WASI_RANDOM_GET_NAME,
        |mut caller: Caller<'_, Host>, buf: i32, buf_len: i32| -> i32 {
            let Some(memory) = guest_memory(&mut caller) else {
                return ERRNO_FAULT;
            };
            let (data, host) = memory.data_and_store_mut(&mut caller);
            errno(
                span(data, buf, u64::from(buf_len.cast_unsigned())).map(|range| {
                    host.wasi.fill_random(&mut data[range]);
                }),
            )
        },
    )?;
    linker.func_wrap(
        abi::WASI_MODULE,
        abi::WASI_PROC_EXIT_NAME,
        |code: i32| -> wasmtime::Result<()> { Err(wasmtime::Error::new(Exit(code))) },
    )?;
    Ok(())
}

/// Box a host-computed `f64` as a number jsval's raw bits, canonicalizing NaN.
fn number_bits(n: f64) -> i64 {
    if n.is_nan() { abi::QNAN } else { n.to_bits() }.cast_signed()
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
            Ok(match op {
                abi::CONVERT_OP_TO_NUMBER => number_bits(value_to_number(&value)),
                abi::CONVERT_OP_PARSE_FLOAT => number_bits(parse_float_str(&to_js_string(&value))),
                abi::CONVERT_OP_TO_STRING => {
                    if matches!(value, Value::Str(_)) {
                        return Ok(bits);
                    }
                    let mut failed = None;
                    let base = alloc_string(&mut caller, &mut failed, &to_js_string(&value));
                    if let Some(err) = failed {
                        return Err(err);
                    }
                    base.map_or(abi::UNDEFINED, |base| abi::HEAP | base)
                        .cast_signed()
                }
                _ => abi::UNDEFINED.cast_signed(),
            })
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
            let mut failed = None;
            let result = method_dispatch(
                &mut caller,
                &mut failed,
                recv.cast_unsigned(),
                op,
                arg0,
                arg1,
            );
            if let Some(err) = failed {
                return Err(err);
            }
            Ok(result.unwrap_or(abi::UNDEFINED).cast_signed())
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
            let text = to_js_string(&decode_arg(&mut caller, bits));
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
            promise::dispatch(&mut caller, op, a.cast_unsigned(), b.cast_unsigned())
                .map(u64::cast_signed)
        },
    )?;
    linker.func_wrap(
        abi::IMPORT_MODULE,
        abi::THEN_IMPORT_NAME,
        |mut caller: Caller<'_, Host>, p: i64, on_f: i64, on_r: i64| -> wasmtime::Result<i64> {
            promise::then(
                &mut caller,
                p.cast_unsigned(),
                on_f.cast_unsigned(),
                on_r.cast_unsigned(),
            )
            .map(u64::cast_signed)
        },
    )?;
    Ok(())
}
