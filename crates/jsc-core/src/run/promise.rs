//! The host-side promise engine: the microtask queue, the per-promise
//! reaction lists, and the settle/drain machinery backing `Promise.resolve` /
//! `Promise.reject` / `p.then` / `p.catch` / `p.finally` and top-level `await`.
//!
//! A promise's *settlement* (state + value) lives in guest linear memory as a
//! [`abi::TYPE_PROMISE`] block, so a bare memory snapshot still renders it (see
//! [`super::value`]); the *scheduling* state — which reactions are registered on
//! a still-pending promise, and which reactions are queued to run — lives here,
//! host-side, keyed by the promise block's base offset.
//!
//! Because this runtime has no timers or I/O, the only async primitive is the
//! microtask: a promise can settle only synchronously (`Promise.resolve/reject`)
//! or through a reaction (a `.then` callback). So "drive the queue until `p`
//! settles" (top-level `await`) and "drain the queue" (after `main`) fully model
//! the event loop. Reaction callbacks are guest functions, so the engine runs
//! them by RE-ENTERING the guest through the exported `__invoke1` trampoline.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use wasmtime::{AsContextMut, Caller, Func, Global, Memory, StoreContextMut, Val};

use super::harness::{WasiCtx, reentry_error};
use crate::abi;

use abi::{
    HEAP, PROMISE_FULFILLED, PROMISE_PENDING, PROMISE_REJECTED, TYPE_FUNCTION, TYPE_PROMISE,
};

const GEN_DONE_OFFSET: usize = abi::GEN_DONE_OFFSET as usize;
const PROMISE_STATE_OFFSET: usize = abi::PROMISE_STATE_OFFSET as usize;
const PROMISE_VALUE_OFFSET: usize = abi::PROMISE_VALUE_OFFSET as usize;

/// The NaN-boxed `undefined` jsval bits.
const UNDEFINED: u64 = abi::UNDEFINED;

/// The store data threaded through `wasmtime`: the WASI context plus the
/// promise engine.
pub(super) struct Host {
    /// The preview-1 context behind the `wasi_snapshot_preview1` imports.
    pub(super) wasi: WasiCtx,
    /// The promise engine's microtask queue and reaction lists.
    pub(super) engine: PromiseEngine,
}

impl Host {
    /// Wrap a WASI context with a fresh, empty promise engine.
    pub(super) fn new(wasi: WasiCtx) -> Self {
        Self {
            wasi,
            engine: PromiseEngine::default(),
        }
    }
}

/// A reaction registered on a still-pending promise: the two handlers (one is
/// selected by the settling state; `undefined` = passthrough), the chained result
/// promise's block base to settle, and whether it has `finally` semantics (run
/// the callback for effect, then pass the original settlement through).
#[derive(Clone, Copy)]
struct Reaction {
    on_fulfilled: u64,
    on_rejected: u64,
    result: u32,
    finally: bool,
    /// An `async function` continuation: `Some(gen_handle)` when this reaction
    /// resumes an async body's generator `gen_handle` with the settlement (sending
    /// the fulfillment value, or throwing the rejection reason into the body),
    /// rather than running a guest callback. `result` is then the async body's result promise.
    async_gen: Option<u64>,
}

/// A queued microtask: a reaction whose source promise has settled, ready to run.
/// `callback` is the selected callback (or `undefined` for a passthrough), `state`
/// / `value` are the source settlement, and `result` is the chained promise to
/// settle from the callback's outcome.
#[derive(Clone, Copy)]
struct Job {
    callback: u64,
    state: i32,
    value: u64,
    result: u32,
    finally: bool,
    /// An `async function` continuation; see [`Reaction::async_gen`].
    async_gen: Option<u64>,
}

/// The host promise engine: the FIFO microtask queue, the reaction lists of
/// each still-pending promise (keyed by block base offset), and the
/// unhandled-rejection tracking reported after the final drain.
#[derive(Default)]
pub(super) struct PromiseEngine {
    queue: VecDeque<Job>,
    reactions: HashMap<u32, Vec<Reaction>>,
    /// Every promise that has been rejected, by block base. Blocks are
    /// bump-allocated, so ascending base order is creation order.
    rejected: BTreeSet<u32>,
    /// Every promise that has had a reaction registered on it (then / catch /
    /// finally, `await`, or adoption into another promise).
    handled: HashSet<u32>,
}

impl std::fmt::Debug for PromiseEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromiseEngine")
            .field("queued", &self.queue.len())
            .field("pending", &self.reactions.len())
            .field("rejected", &self.rejected.len())
            .field("handled", &self.handled.len())
            .finish()
    }
}

/// The `Copy` guest handles the engine needs, resolved once (from a [`Caller`]
/// mid-run or from an [`Instance`] after `main`) so every engine op takes a plain
/// store context plus these.
#[derive(Clone, Copy)]
struct Handles {
    memory: Memory,
    /// The `__new_promise() -> base` guest constructor every promise block is
    /// allocated through.
    new_promise: Func,
    /// The `__promise_self_error() -> jsval` guest constructor of the
    /// `TypeError` a promise resolved with itself is rejected with.
    self_error: Func,
    exc_pending: Global,
    exc_value: Global,
    /// The `__invoke1` callback trampoline. `None` when the program declares no
    /// user functions (so no reaction can carry a callback).
    invoke1: Option<Func>,
    /// The `__async_step(gen_handle, sent)` generator-step trampoline the async
    /// driver calls to advance an `async function` body one suspend. `None` when
    /// the export is absent (no `async function` in the program).
    async_step: Option<Func>,
}

impl Handles {
    /// Resolve the handles from a host-import [`Caller`].
    fn from_caller(caller: &mut Caller<'_, Host>) -> Option<Self> {
        Some(Self {
            memory: caller.get_export("memory")?.into_memory()?,
            new_promise: caller
                .get_export(abi::NEW_PROMISE_EXPORT_NAME)?
                .into_func()?,
            self_error: caller
                .get_export(abi::PROMISE_SELF_ERROR_EXPORT_NAME)?
                .into_func()?,
            exc_pending: caller
                .get_export(abi::EXC_PENDING_EXPORT_NAME)?
                .into_global()?,
            exc_value: caller
                .get_export(abi::EXC_VALUE_EXPORT_NAME)?
                .into_global()?,
            invoke1: caller
                .get_export(abi::INVOKE_EXPORT_NAME)
                .and_then(wasmtime::Extern::into_func),
            async_step: caller
                .get_export(abi::ASYNC_STEP_EXPORT_NAME)
                .and_then(wasmtime::Extern::into_func),
        })
    }
}

/// Handle a `__promise(op, a, b)` host-import call: `Promise.resolve`/`reject`,
/// top-level `await`, or `p.finally`. Returns the result jsval bits (a promise,
/// or the awaited value). A missing export set makes it a no-op returning
/// `undefined` rather than trapping the guest.
pub(super) fn dispatch(
    caller: &mut Caller<'_, Host>,
    op: i32,
    a: u64,
    b: u64,
) -> wasmtime::Result<u64> {
    let Some(handles) = Handles::from_caller(caller) else {
        return Ok(UNDEFINED);
    };
    let mut ctx = caller.as_context_mut();
    Ok(match op {
        x if x == abi::PROMISE_OP_RESOLVE => resolve(ctx, &handles, a)?,
        x if x == abi::PROMISE_OP_REJECT => {
            alloc_promise(ctx, &handles, PROMISE_REJECTED, a)?.unwrap_or(UNDEFINED)
        }
        x if x == abi::PROMISE_OP_FINALLY => finally(ctx, &handles, a, b)?,
        x if x == abi::PROMISE_OP_AWAIT => await_promise(ctx, &handles, a)?,
        // `new Promise` support: allocate a pending promise, or settle a
        // promise from a `resolve`/`reject` settler call.
        x if x == abi::PROMISE_OP_NEW => {
            alloc_promise(ctx, &handles, PROMISE_PENDING, UNDEFINED)?.unwrap_or(UNDEFINED)
        }
        x if x == abi::PROMISE_OP_SETTLE_FULFILL => {
            if let Some(base) = as_promise(handles.memory.data(&ctx), a) {
                settle(ctx.as_context_mut(), &handles, base, PROMISE_FULFILLED, b)?;
            }
            UNDEFINED
        }
        x if x == abi::PROMISE_OP_SETTLE_REJECT => {
            if let Some(base) = as_promise(handles.memory.data(&ctx), a) {
                settle(ctx.as_context_mut(), &handles, base, PROMISE_REJECTED, b)?;
            }
            UNDEFINED
        }
        // Spawn an `async function` body: `a` is the generator handle.
        x if x == abi::PROMISE_OP_ASYNC_SPAWN => spawn_async(ctx, &handles, a)?,
        // Deleted in R4 with the host promise engine.
        x if x == abi::PROMISE_OP_DRAIN => {
            drain(ctx.as_context_mut(), &handles)?;
            let engine = &ctx.data().engine;
            engine
                .rejected
                .iter()
                .find(|base| !engine.handled.contains(base))
                .map_or(0, |base| HEAP | u64::from(*base))
        }
        _ => UNDEFINED,
    })
}

/// Handle a `__then(p, on_f, on_r)` host-import call: allocate the chained result
/// promise, register the reactions on `p` (or, if `p` is already settled / not a
/// promise, queue the appropriate microtask), and return the result promise.
pub(super) fn then(
    caller: &mut Caller<'_, Host>,
    p: u64,
    on_fulfilled: u64,
    on_rejected: u64,
) -> wasmtime::Result<u64> {
    let Some(handles) = Handles::from_caller(caller) else {
        return Ok(UNDEFINED);
    };
    let mut ctx = caller.as_context_mut();
    let Some(result) = alloc_promise(ctx.as_context_mut(), &handles, PROMISE_PENDING, UNDEFINED)?
    else {
        return Ok(UNDEFINED);
    };
    let result_base = base_of(result);
    register_source(
        ctx,
        &handles,
        p,
        on_fulfilled,
        on_rejected,
        result_base,
        false,
        None,
    );
    Ok(result)
}

/// `Promise.resolve(a)`: `a` itself when it is already a promise, else a fresh
/// promise fulfilled with `a`.
fn resolve(mut ctx: StoreContextMut<'_, Host>, handles: &Handles, a: u64) -> wasmtime::Result<u64> {
    let is_promise = as_promise(handles.memory.data(&ctx), a).is_some();
    if is_promise {
        Ok(a)
    } else {
        Ok(
            alloc_promise(ctx.as_context_mut(), handles, PROMISE_FULFILLED, a)?
                .unwrap_or(UNDEFINED),
        )
    }
}

/// `p.finally(fn)`: a chained promise that runs `fn` on settlement (both paths)
/// and passes `p`'s original settlement through.
fn finally(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    p: u64,
    on_finally: u64,
) -> wasmtime::Result<u64> {
    let Some(result) = alloc_promise(ctx.as_context_mut(), handles, PROMISE_PENDING, UNDEFINED)?
    else {
        return Ok(UNDEFINED);
    };
    let result_base = base_of(result);
    // The finalizer is the callback for BOTH fulfill and reject; `finally: true`
    // makes the job pass the original settlement through afterwards.
    register_source(
        ctx,
        handles,
        p,
        on_finally,
        on_finally,
        result_base,
        true,
        None,
    );
    Ok(result)
}

/// Top-level `await p`: turn the event loop — drain the microtask queue to empty
/// — then return `p`'s fulfillment value. Draining to empty (rather than stopping
/// the instant `p` settles) is what makes `await` YIELD: microtasks queued before
/// the await run before the code after it, even when `p` is already settled,
/// matching real top-level-await ordering. In this timer-less runtime the queue
/// always drains `p` to a settlement. A rejection is surfaced through the
/// exception globals (`__exc_pending`/`__exc_value`) so the emitted propagation
/// unwinds it as an uncaught error; awaiting a non-promise yields it unchanged.
fn await_promise(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    p: u64,
) -> wasmtime::Result<u64> {
    let Some(base) = as_promise(handles.memory.data(&ctx), p) else {
        return Ok(p);
    };
    ctx.data_mut().engine.handled.insert(base);
    drain(ctx.as_context_mut(), handles)?;
    let mem = handles.memory.data(&ctx);
    Ok(match read_state(mem, base) {
        s if s == PROMISE_FULFILLED => read_value(mem, base),
        s if s == PROMISE_REJECTED => {
            let value = read_value(mem, base);
            let _ = handles
                .exc_value
                .set(&mut ctx, Val::I64(value.cast_signed()));
            let _ = handles.exc_pending.set(&mut ctx, Val::I32(1));
            UNDEFINED
        }
        // Still pending after the queue drained: a deadlocked promise. It resolves
        // to `undefined` as a defensive fallback rather than a hang.
        _ => UNDEFINED,
    })
}

/// Spawn an `async function` body: allocate the result promise, drive
/// the body's generator through its first step synchronously, and return the
/// result promise — pending if the body suspended on an `await`, already settled
/// if it ran straight to completion or threw synchronously. `gen_handle` is the freshly
/// built generator handle jsval (the async body compiled as a state machine).
///
/// The first step, and every later resumption, re-enters the guest through
/// `__async_step` — the same re-entrant pattern the reaction engine uses for
/// `__invoke1`, so a nested `await`/`.then` chain is driven to completion by the
/// enclosing top-level `await` or the after-`main` drain.
fn spawn_async(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    gen_handle: u64,
) -> wasmtime::Result<u64> {
    let Some(result) = alloc_promise(ctx.as_context_mut(), handles, PROMISE_PENDING, UNDEFINED)?
    else {
        return Ok(UNDEFINED);
    };
    let result_base = base_of(result);
    drive_async(ctx, handles, gen_handle, result_base, UNDEFINED)?;
    Ok(result)
}

/// Advance an `async function` body's generator by one step and route the outcome.
/// `sent` resumes the pending `await` (`undefined` for the first step, or for a
/// rejected `await` resumed with its reason pending as an exception).
/// If the step threw, reject the result promise; if the body completed (`done`),
/// fulfill it with the returned value; otherwise the yielded value is the awaited
/// operand — register a continuation that resumes the body when the operand
/// settles.
fn drive_async(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    gen_handle: u64,
    result_base: u32,
    sent: u64,
) -> wasmtime::Result<()> {
    let (value, threw) = async_step(ctx.as_context_mut(), handles, gen_handle, sent)?;
    if threw {
        return settle(ctx, handles, result_base, PROMISE_REJECTED, value);
    }
    if read_gen_done(handles.memory.data(&ctx), gen_handle) {
        return settle(ctx, handles, result_base, PROMISE_FULFILLED, value);
    }
    // The body suspended on `await value`: resume it (or reject the result) when
    // `value` settles. `register_source` gives `Promise.resolve` semantics — a
    // non-promise awaited value resumes as an already-fulfilled microtask.
    register_source(
        ctx,
        handles,
        value,
        UNDEFINED,
        UNDEFINED,
        result_base,
        false,
        Some(gen_handle),
    );
    Ok(())
}

/// Advance the async body's generator one suspend by re-entering the guest through
/// the exported `__async_step(gen_handle, sent)` trampoline. Returns the state
/// machine's value bits and whether it threw — detected by `__exc_pending`, which
/// (with `__exc_value`) is cleared here so a surfaced rejection does not leak into
/// the next step. Mirrors [`invoke1`].
fn async_step(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    gen_handle: u64,
    sent: u64,
) -> wasmtime::Result<(u64, bool)> {
    let Some(func) = handles.async_step else {
        return Ok((UNDEFINED, false));
    };
    let mut results = [Val::I64(0)];
    func.call(
        &mut ctx,
        &[
            Val::I64(gen_handle.cast_signed()),
            Val::I64(sent.cast_signed()),
        ],
        &mut results,
    )
    .map_err(reentry_error)?;
    if handles.exc_pending.get(&mut ctx).i32().unwrap_or(0) != 0 {
        let value = handles
            .exc_value
            .get(&mut ctx)
            .i64()
            .unwrap_or(0)
            .cast_unsigned();
        let _ = handles.exc_pending.set(&mut ctx, Val::I32(0));
        let _ = handles.exc_value.set(&mut ctx, Val::I64(0));
        return Ok((value, true));
    }
    let result = match results[0] {
        Val::I64(v) => v.cast_unsigned(),
        _ => UNDEFINED,
    };
    Ok((result, false))
}

/// Read an async-body generator handle's `done` flag: `true` once the
/// state machine has completed (fall-through or `return`). A non-heap value or a
/// truncated block reads as not-done.
fn read_gen_done(mem: &[u8], gen_handle: u64) -> bool {
    let Some(base) = heap_ptr(gen_handle) else {
        return false;
    };
    let at = base + GEN_DONE_OFFSET;
    mem.get(at..at + 4)
        .is_some_and(|b| i32::from_le_bytes(b.try_into().unwrap_or([0; 4])) != 0)
}

/// Drive the microtask queue to empty, running each job FIFO. Shared by top-level
/// `await` (turning the event loop) and the after-`main` drain. A job may enqueue
/// more jobs (a reaction that chains); they are appended and run in turn.
fn drain(mut ctx: StoreContextMut<'_, Host>, handles: &Handles) -> wasmtime::Result<()> {
    while let Some(job) = ctx.data_mut().engine.queue.pop_front() {
        run_job(ctx.as_context_mut(), handles, job)?;
    }
    Ok(())
}

/// Run one microtask: invoke its callback (re-entering the guest) and settle the
/// chained result promise from the outcome. A passthrough job (no callback)
/// forwards the source settlement; a `finally` job runs the callback for effect
/// and forwards the original settlement (unless the callback throws, which
/// rejects the result).
fn run_job(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    job: Job,
) -> wasmtime::Result<()> {
    // An `async function` continuation: the awaited value has settled.
    // On fulfillment, resume the body's generator with the settled value (which
    // may suspend again on the next `await` or complete and settle the result);
    // on rejection, resume it with the reason pending as an exception, so the
    // `await` throws into the body's active `catch`/`finally` (or, uncaught,
    // completes the body and rejects the result).
    if let Some(gen_handle) = job.async_gen {
        let sent = if job.state == PROMISE_FULFILLED {
            job.value
        } else {
            let _ = handles
                .exc_value
                .set(&mut ctx, Val::I64(job.value.cast_signed()));
            let _ = handles.exc_pending.set(&mut ctx, Val::I32(1));
            UNDEFINED
        };
        return drive_async(ctx, handles, gen_handle, job.result, sent);
    }
    let is_fn = as_function(handles.memory.data(&ctx), job.callback);
    if !is_fn {
        // No callback for this path: pass the source settlement straight through.
        return settle(ctx, handles, job.result, job.state, job.value);
    }
    let (outcome, threw) = invoke1(ctx.as_context_mut(), handles, job.callback, job.value)?;
    if threw {
        settle(ctx, handles, job.result, PROMISE_REJECTED, outcome)
    } else if job.finally {
        // A finalizer's normal return is ignored: forward the original settlement.
        settle(ctx, handles, job.result, job.state, job.value)
    } else {
        settle(ctx, handles, job.result, PROMISE_FULFILLED, outcome)
    }
}

/// Settle promise `base` with `(state, value)`, once. Fulfilling with a value
/// that is itself a promise ADOPTS it (the promise stays pending and re-settles
/// when the inner one does); fulfilling a promise with ITSELF rejects it with a
/// fresh `TypeError` instead (Promise Resolve Functions step 7). On a real
/// settlement, its registered reactions are moved into the microtask queue.
fn settle(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    base: u32,
    mut state: i32,
    mut value: u64,
) -> wasmtime::Result<()> {
    if read_state(handles.memory.data(&ctx), base) != PROMISE_PENDING {
        return Ok(()); // settle-once.
    }
    if state == PROMISE_FULFILLED {
        match as_promise(handles.memory.data(&ctx), value) {
            Some(inner) if inner == base => {
                let mut results = [Val::I64(0)];
                handles
                    .self_error
                    .call(&mut ctx, &[], &mut results)
                    .map_err(reentry_error)?;
                state = PROMISE_REJECTED;
                value = match results[0] {
                    Val::I64(v) => v.cast_unsigned(),
                    _ => UNDEFINED,
                };
            }
            // Resolving with another promise: adopt it instead of storing a
            // promise-as-value.
            Some(_) => {
                register_source(ctx, handles, value, UNDEFINED, UNDEFINED, base, false, None);
                return Ok(());
            }
            None => {}
        }
    }
    write_settlement(ctx.as_context_mut(), handles, base, state, value);
    if state == PROMISE_REJECTED {
        ctx.data_mut().engine.rejected.insert(base);
    }
    let reactions = ctx
        .data_mut()
        .engine
        .reactions
        .remove(&base)
        .unwrap_or_default();
    for r in reactions {
        let callback = if state == PROMISE_FULFILLED {
            r.on_fulfilled
        } else {
            r.on_rejected
        };
        ctx.data_mut().engine.queue.push_back(Job {
            callback,
            state,
            value,
            result: r.result,
            finally: r.finally,
            async_gen: r.async_gen,
        });
    }
    Ok(())
}

/// Register a reaction whose source is `source` (a promise, or a plain value):
/// on a pending promise, store the reaction; on a settled promise, queue the
/// matching microtask now; on a non-promise value, queue a fulfilled microtask
/// (so `.then` on a non-promise behaves like `Promise.resolve(value).then`).
#[allow(clippy::too_many_arguments)]
fn register_source(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    source: u64,
    on_fulfilled: u64,
    on_rejected: u64,
    result: u32,
    finally: bool,
    async_gen: Option<u64>,
) {
    let mem = handles.memory.data(&ctx);
    let Some(base) = as_promise(mem, source) else {
        // A non-promise source acts as an already-fulfilled value.
        ctx.data_mut().engine.queue.push_back(Job {
            callback: on_fulfilled,
            state: PROMISE_FULFILLED,
            value: source,
            result,
            finally,
            async_gen,
        });
        return;
    };
    let state = read_state(mem, base);
    let value = read_value(mem, base);
    ctx.data_mut().engine.handled.insert(base);
    if state == PROMISE_PENDING {
        ctx.data_mut()
            .engine
            .reactions
            .entry(base)
            .or_default()
            .push(Reaction {
                on_fulfilled,
                on_rejected,
                result,
                finally,
                async_gen,
            });
        return;
    }
    let callback = if state == PROMISE_FULFILLED {
        on_fulfilled
    } else {
        on_rejected
    };
    ctx.data_mut().engine.queue.push_back(Job {
        callback,
        state,
        value,
        result,
        finally,
        async_gen,
    });
}

/// Invoke a reaction's guest callback `callback` with `arg` through the exported
/// `__invoke1` trampoline (re-entering the guest). Returns the callback's result
/// bits and whether it threw — detected by the `__exc_pending` global, which is
/// cleared here so a caught rejection does not leak into the next microtask.
fn invoke1(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    callback: u64,
    arg: u64,
) -> wasmtime::Result<(u64, bool)> {
    let Some(func) = handles.invoke1 else {
        return Ok((UNDEFINED, false));
    };
    let mut results = [Val::I64(0)];
    func.call(
        &mut ctx,
        &[
            Val::I64(callback.cast_signed()),
            Val::I64(arg.cast_signed()),
        ],
        &mut results,
    )
    .map_err(reentry_error)?;
    if handles.exc_pending.get(&mut ctx).i32().unwrap_or(0) != 0 {
        let value = handles
            .exc_value
            .get(&mut ctx)
            .i64()
            .unwrap_or(0)
            .cast_unsigned();
        let _ = handles.exc_pending.set(&mut ctx, Val::I32(0));
        let _ = handles.exc_value.set(&mut ctx, Val::I64(0));
        return Ok((value, true));
    }
    let result = match results[0] {
        Val::I64(v) => v.cast_unsigned(),
        _ => UNDEFINED,
    };
    Ok((result, false))
}

/// Allocate a fresh promise block through the guest `__new_promise` export,
/// settle it to `state`/`value` unless pending, and return its heap jsval.
/// `None` when the export has an unexpected type; a failed call propagates.
fn alloc_promise(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    state: i32,
    value: u64,
) -> wasmtime::Result<Option<u64>> {
    let Ok(new_promise) = handles.new_promise.typed::<(), i32>(&ctx) else {
        return Ok(None);
    };
    let raw = new_promise.call(&mut ctx, ()).map_err(reentry_error)?;
    let Ok(base) = u32::try_from(raw) else {
        return Ok(None);
    };
    if state != PROMISE_PENDING {
        write_settlement(ctx.as_context_mut(), handles, base, state, value);
    }
    if state == PROMISE_REJECTED {
        ctx.data_mut().engine.rejected.insert(base);
    }
    Ok(Some(HEAP | u64::from(base)))
}

/// Write a promise block's settlement (`state` + `value`) into guest memory.
fn write_settlement(
    mut ctx: StoreContextMut<'_, Host>,
    handles: &Handles,
    base: u32,
    state: i32,
    value: u64,
) {
    let base = base as usize;
    let _ = handles
        .memory
        .write(&mut ctx, base + PROMISE_STATE_OFFSET, &state.to_le_bytes());
    let _ = handles
        .memory
        .write(&mut ctx, base + PROMISE_VALUE_OFFSET, &value.to_le_bytes());
}

/// The base offset (low 32 bits) of a heap jsval.
fn base_of(bits: u64) -> u32 {
    u32::try_from(bits & 0xFFFF_FFFF).unwrap_or(0)
}

/// The block base offset of a heap jsval whose TYPE BYTE is [`TYPE_PROMISE`], or
/// `None` for any non-promise value.
fn as_promise(mem: &[u8], bits: u64) -> Option<u32> {
    let base = heap_ptr(bits)?;
    if mem.get(base).copied() == Some(TYPE_PROMISE) {
        u32::try_from(base).ok()
    } else {
        None
    }
}

/// Whether `bits` is a heap function value (TYPE BYTE [`TYPE_FUNCTION`]).
fn as_function(mem: &[u8], bits: u64) -> bool {
    heap_ptr(bits).is_some_and(|base| mem.get(base).copied() == Some(TYPE_FUNCTION))
}

/// The linear-memory byte offset of a heap jsval (single heap tag `0b101`), or
/// `None` for any non-heap value.
fn heap_ptr(bits: u64) -> Option<usize> {
    if bits & abi::QNAN == abi::QNAN && (bits >> 48) & 0b111 == 0b101 {
        usize::try_from(bits & 0xFFFF_FFFF).ok()
    } else {
        None
    }
}

/// Read a promise block's `state` (i32 at `base + PROMISE_STATE_OFFSET`); a
/// truncated block reads as [`PROMISE_PENDING`].
fn read_state(mem: &[u8], base: u32) -> i32 {
    let at = base as usize + PROMISE_STATE_OFFSET;
    mem.get(at..at + 4).map_or(PROMISE_PENDING, |b| {
        i32::from_le_bytes(b.try_into().unwrap_or([0; 4]))
    })
}

/// Read a promise block's settled `value` (i64 at `base + PROMISE_VALUE_OFFSET`);
/// a truncated block reads as `undefined`.
fn read_value(mem: &[u8], base: u32) -> u64 {
    let at = base as usize + PROMISE_VALUE_OFFSET;
    mem.get(at..at + 8).map_or(UNDEFINED, |b| {
        u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))
    })
}
