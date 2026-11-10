//! The guest promise engine (design/runtime-memory.md §3.4): promise
//! allocation, settle-once, reaction records, the intrusive microtask FIFO,
//! the drain loop with its checkpoint, and unhandled-rejection tracking.
//!
//! A reaction record is one `TYPE_ENV` block of [`abi::REACTION_SLOTS`] jsval
//! slots. It is first a reaction on a pending promise (a prepend list hanging
//! off the promise's raw `reactions` ref), then, once its source settles, a
//! queued job (the FIFO `$__job_head` .. `$__job_tail`), so settling never
//! allocates. Every `next` link is a `HEAP | record` jsval (or `0`).
//!
//! Rejected promises nobody handled are linked through `rejected_next` from
//! `$__rejected_head`; the checkpoint after each drain prunes the ones a late
//! reaction marked `HANDLED`, and `_start` reports the oldest survivor by
//! creation `seq`.

use wasm_encoder::{
    BlockType, Function, FunctionSection, Instruction, MemArg, TypeSection, ValType,
};

use super::{
    BuiltinCtor, EXC_PENDING_GLOBAL, EXC_VALUE_GLOBAL, FnBuilder, HEAP_BITS, Helpers, Intrinsic,
    JOB_HEAD_GLOBAL, JOB_TAIL_GLOBAL, LeafSet, PROMISE_SEQ_GLOBAL, RAW_I32, REJECTED_HEAD_GLOBAL,
    ROOT_BASE, ROOT_JS, Sink, UNDEFINED_BITS, abi, emit_is_function, emit_is_pointer,
    emit_load_type_byte, finish_builder, helper_builder, intrinsic_bits, libm, unicode,
};

/// Emit each listed instruction in order.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// A byte [`MemArg`] at `offset`.
fn m8(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

/// A `u32` [`MemArg`] at `offset`.
fn m32(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    }
}

/// An `i64` [`MemArg`] at `offset`.
fn m64(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 3,
        memory_index: 0,
    }
}

const TYPE_PROMISE: i32 = abi::TYPE_PROMISE as i32;
const TYPE_ENV: i32 = abi::TYPE_ENV as i32;
const PENDING: i32 = abi::PROMISE_PENDING;
const FULFILLED: i32 = abi::PROMISE_FULFILLED;
const REJECTED: i32 = abi::PROMISE_REJECTED;
const STATE: u32 = abi::PROMISE_STATE_OFFSET;
const REACTIONS: u32 = abi::PROMISE_REACTIONS_OFFSET;
const VALUE: u32 = abi::PROMISE_VALUE_OFFSET;
const SEQ: u32 = abi::PROMISE_SEQ_OFFSET;
const REJECTED_NEXT: u32 = abi::PROMISE_REJECTED_NEXT_OFFSET;
const FLAGS: u32 = abi::HEADER_FLAGS_OFFSET;
const HANDLED: i32 = abi::PROMISE_FLAG_HANDLED as i32;
const IN_REJECTED_LIST: i32 = abi::PROMISE_FLAG_IN_REJECTED_LIST as i32;
const ON_FULFILLED: u32 = abi::REACTION_ON_FULFILLED_OFFSET;
const ON_REJECTED: u32 = abi::REACTION_ON_REJECTED_OFFSET;
const RESULT: u32 = abi::REACTION_RESULT_OFFSET;
const KIND: u32 = abi::REACTION_KIND_OFFSET;
const JOB_VALUE: u32 = abi::REACTION_VALUE_OFFSET;
const NEXT: u32 = abi::REACTION_NEXT_OFFSET;
const FINALLY: i32 = abi::REACTION_KIND_FINALLY;
const STATE_SHIFT: i32 = abi::REACTION_KIND_STATE_SHIFT;

use ValType::{I32, I64};

/// The `(params, results)` of the engine helpers, in function-index order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 12] = [
    (&[I64], &[I32]),                  // __promise_new(proto) -> base
    (&[I32], &[]),                     // __promise_enqueue(rec)
    (&[I64, I64, I64, I64, I32], &[]), // __promise_react(p, on_f, on_r, result, kind)
    (&[I64, I32, I64], &[]),           // __promise_settle(p, state, v)
    (&[I64, I64, I64], &[I64]),        // __promise_then(p, on_f, on_r) -> derived
    (&[I64], &[I64]),                  // __promise_resolve(v) -> promise
    (&[I64], &[I64]),                  // __promise_reject(v) -> promise
    (&[I64, I64], &[I64]),             // __promise_finally(p, on_finally) -> derived
    (&[I32], &[]),                     // __run_job(rec)
    (&[], &[]),                        // __drain()
    (&[], &[I64]),                     // __first_unhandled() -> promise or 0
    (&[I64], &[I64]),                  // __promise_await(v) -> value
];

/// Append the engine helpers' types and function entries (after libm's).
pub(super) fn register(types: &mut TypeSection, functions: &mut FunctionSection) {
    for (params, results) in TYPES {
        let index = types.len();
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        functions.function(index);
    }
}

impl Helpers<'_> {
    /// The first engine helper: they follow the libm helpers.
    fn engine_base(self) -> u32 {
        let before = unicode::TYPES.len() + libm::HELPER_COUNT;
        self.start() + 1 + u32::try_from(before).unwrap_or(0)
    }
    /// `__promise_new(proto: i64) -> i32`: a fresh pending promise block.
    pub(super) fn promise_new(self) -> u32 {
        self.engine_base()
    }
    /// `__promise_enqueue(rec: i32)`: append a record to the job queue.
    fn promise_enqueue(self) -> u32 {
        self.engine_base() + 1
    }
    /// `__promise_react(p, on_f, on_r, result, kind)`: register a reaction.
    fn promise_react(self) -> u32 {
        self.engine_base() + 2
    }
    /// `__promise_settle(p: i64, state: i32, v: i64)`: settle once.
    pub(super) fn promise_settle(self) -> u32 {
        self.engine_base() + 3
    }
    /// `__promise_then(p, on_f, on_r) -> i64`: the derived promise.
    pub(super) fn promise_then(self) -> u32 {
        self.engine_base() + 4
    }
    /// `__promise_resolve(v) -> i64`: `v` when a promise, else fulfilled.
    pub(super) fn promise_resolve(self) -> u32 {
        self.engine_base() + 5
    }
    /// `__promise_reject(v) -> i64`: a fresh promise rejected with `v`.
    pub(super) fn promise_reject(self) -> u32 {
        self.engine_base() + 6
    }
    /// `__promise_finally(p, on_finally) -> i64`: the derived promise.
    pub(super) fn promise_finally(self) -> u32 {
        self.engine_base() + 7
    }
    /// `__run_job(rec: i32)`: run one queued job.
    fn run_job(self) -> u32 {
        self.engine_base() + 8
    }
    /// `__drain()`: run every queued job and the checkpoints.
    pub(super) fn drain(self) -> u32 {
        self.engine_base() + 9
    }
    /// `__first_unhandled() -> i64`: the oldest unhandled rejected promise.
    pub(super) fn first_unhandled(self) -> u32 {
        self.engine_base() + 10
    }
    /// `__promise_await(v) -> i64`: top-level `await`.
    pub(super) fn promise_await(self) -> u32 {
        self.engine_base() + 11
    }
}

/// The helper call graph of the engine, for the leaf analysis.
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![(helpers.promise_enqueue(), vec![])]
}

/// Every engine helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>, leaves: &LeafSet) -> Vec<Function> {
    vec![
        emit_promise_new(helpers, leaves),
        emit_enqueue(leaves),
        emit_react(helpers, leaves),
        emit_settle(helpers, leaves),
        emit_then(helpers, leaves, false),
        emit_resolve(helpers, leaves, false),
        emit_resolve(helpers, leaves, true),
        emit_then(helpers, leaves, true),
        emit_run_job(helpers, leaves),
        emit_drain(helpers, leaves),
        emit_first_unhandled(leaves),
        emit_await(helpers, leaves),
    ]
}

/// The `%Promise.prototype%` jsval every engine-allocated promise gets.
fn promise_proto(helpers: Helpers<'_>) -> i64 {
    intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Promise), helpers)
}

/// Push `1` when the jsval in `local` is a promise block.
fn is_promise(f: &mut FnBuilder<'_>, local: u32) {
    emit_is_pointer(f, local);
    wasm!(f; If(BlockType::Result(I32)));
    emit_load_type_byte(f, local);
    wasm!(f; I32Const(TYPE_PROMISE), I32Eq, Else, I32Const(0), End);
}

/// Push `HEAP | base` for the `i32` base on top of the stack.
fn box_heap(f: &mut FnBuilder<'_>) {
    wasm!(f; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Push the `next` jsval for the raw record ref in `local`: `HEAP | rec`, or
/// `0` for none.
fn link(f: &mut FnBuilder<'_>, local: u32) {
    wasm!(f; LocalGet(local), If(BlockType::Result(I64)), LocalGet(local));
    box_heap(f);
    wasm!(f; Else, I64Const(0), End);
}

/// `__promise_new(proto) -> base`: allocate a pending promise with prototype
/// `proto`, `seq = $__promise_seq++`, no reactions, not in the rejected list.
fn emit_promise_new(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS], &[ROOT_BASE]);
    let (proto, base) = (0, 1);
    let size = abi::PROMISE_BLOCK_SIZE.cast_signed();
    wasm!(f;
        I32Const(size), Call(helpers.alloc), LocalSet(base),
        // Header word (type byte, zero gc/flags/class) and a zero `props`.
        LocalGet(base), I64Const(i64::from(TYPE_PROMISE)), I64Store(m64(0)),
        LocalGet(base), LocalGet(proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(base), I32Const(PENDING), I32Store(m32(STATE)),
        LocalGet(base), I32Const(0), I32Store(m32(REACTIONS)),
        LocalGet(base), I64Const(UNDEFINED_BITS), I64Store(m64(VALUE)),
        LocalGet(base), GlobalGet(PROMISE_SEQ_GLOBAL), I32Store(m32(SEQ)),
        GlobalGet(PROMISE_SEQ_GLOBAL), I32Const(1), I32Add, GlobalSet(PROMISE_SEQ_GLOBAL),
        LocalGet(base), I32Const(0), I32Store(m32(REJECTED_NEXT)),
        LocalGet(base),
        End,
    );
    finish_builder(f)
}

/// `__promise_enqueue(rec)`: append the record to the job FIFO.
fn emit_enqueue(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[]);
    let rec = 0;
    wasm!(f;
        LocalGet(rec), I64Const(0), I64Store(m64(NEXT)),
        GlobalGet(JOB_TAIL_GLOBAL), I32Eqz,
        If(BlockType::Empty),
        LocalGet(rec), GlobalSet(JOB_HEAD_GLOBAL),
        Else,
        GlobalGet(JOB_TAIL_GLOBAL), LocalGet(rec),
    );
    box_heap(&mut f);
    wasm!(f;
        I64Store(m64(NEXT)),
        End,
        LocalGet(rec), GlobalSet(JOB_TAIL_GLOBAL),
        End,
    );
    finish_builder(f)
}

/// `__promise_react(p, on_f, on_r, result, kind)`: allocate a reaction
/// record. A non-promise `p` acts as an already-fulfilled value and queues
/// the job at once; a promise gets `HANDLED`; pending, the record is
/// prepended to its reactions, settled, queued with its settlement.
fn emit_react(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(
        leaves,
        &[ROOT_JS, ROOT_JS, ROOT_JS, ROOT_JS, RAW_I32],
        &[ROOT_BASE, RAW_I32, RAW_I32, RAW_I32],
    );
    let (p, on_f, on_r, result, kind) = (0, 1, 2, 3, 4);
    let (rec, pb, state, old) = (5, 6, 7, 8);
    let size = abi::REACTION_BLOCK_SIZE.cast_signed();
    let slots = abi::REACTION_SLOTS.cast_signed();
    wasm!(f;
        I32Const(size), Call(helpers.alloc), LocalSet(rec),
        LocalGet(rec), I32Const(TYPE_ENV), I32Store8(m8(0)),
        LocalGet(rec), I32Const(slots), I32Store(m32(abi::HEADER_SIZE)),
        LocalGet(rec), LocalGet(on_f), I64Store(m64(ON_FULFILLED)),
        LocalGet(rec), LocalGet(on_r), I64Store(m64(ON_REJECTED)),
        LocalGet(rec), LocalGet(result), I64Store(m64(RESULT)),
        LocalGet(rec), LocalGet(kind), I64ExtendI32U, I64Store(m64(KIND)),
        LocalGet(rec), I64Const(UNDEFINED_BITS), I64Store(m64(JOB_VALUE)),
        LocalGet(rec), I64Const(0), I64Store(m64(NEXT)),
    );
    is_promise(&mut f, p);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(rec), LocalGet(kind), I32Const(FULFILLED << STATE_SHIFT), I32Or,
        I64ExtendI32U, I64Store(m64(KIND)),
        LocalGet(rec), LocalGet(p), I64Store(m64(JOB_VALUE)),
        LocalGet(rec), Call(helpers.promise_enqueue()),
        Return,
        End,
        LocalGet(p), I32WrapI64, LocalSet(pb),
        LocalGet(pb), LocalGet(pb), I32Load8U(m8(FLAGS)), I32Const(HANDLED), I32Or,
        I32Store8(m8(FLAGS)),
        LocalGet(pb), I32Load(m32(STATE)), LocalTee(state), I32Eqz,
        If(BlockType::Empty),
        LocalGet(pb), I32Load(m32(REACTIONS)), LocalSet(old),
        LocalGet(rec),
    );
    link(&mut f, old);
    wasm!(f;
        I64Store(m64(NEXT)),
        LocalGet(pb), LocalGet(rec), I32Store(m32(REACTIONS)),
        Return,
        End,
        LocalGet(rec), LocalGet(kind), LocalGet(state), I32Const(STATE_SHIFT), I32Shl, I32Or,
        I64ExtendI32U, I64Store(m64(KIND)),
        LocalGet(rec), LocalGet(pb), I64Load(m64(VALUE)), I64Store(m64(JOB_VALUE)),
        LocalGet(rec), Call(helpers.promise_enqueue()),
        End,
    );
    finish_builder(f)
}

/// `__promise_settle(p, state, v)`: settle the promise `p` once (anything
/// else, or an already settled promise, is a no-op). Fulfilling with `p`
/// itself rejects with a fresh `TypeError`; fulfilling with another promise
/// adopts it (`p` becomes the derived promise of a passthrough reaction on
/// `v`). Otherwise the settlement is written, the reaction list is reversed
/// in place into registration order, each record gets its source state and
/// value, and the whole chain is spliced onto the job queue. A rejection of a
/// promise nobody handles links it into the rejected list.
fn emit_settle(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    // The detached reaction records are reachable only from the raw locals
    // between the detach and the splice, which contain no safepoint.
    let mut f = helper_builder(
        leaves,
        &[ROOT_JS, RAW_I32, ROOT_JS],
        &[RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32],
    );
    let (p, state, v) = (0, 1, 2);
    let (pb, cur, prev, next, last) = (3, 4, 5, 6, 7);
    is_promise(&mut f, p);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), Return, End,
        LocalGet(p), I32WrapI64, I32Load(m32(STATE)), If(BlockType::Empty), Return, End,
        LocalGet(state), I32Const(FULFILLED), I32Eq,
        If(BlockType::Empty),
        LocalGet(v), LocalGet(p), I64Eq,
        If(BlockType::Empty),
        Call(helpers.promise_self_error()), LocalSet(v),
        I32Const(REJECTED), LocalSet(state),
        Else,
    );
    is_promise(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I64Const(UNDEFINED_BITS), I64Const(UNDEFINED_BITS), LocalGet(p), I32Const(0),
        Call(helpers.promise_react()),
        Return,
        End,
        End,
        End,
        // Write the settlement and detach the reactions.
        LocalGet(p), I32WrapI64, LocalTee(pb), LocalGet(state), I32Store(m32(STATE)),
        LocalGet(pb), LocalGet(v), I64Store(m64(VALUE)),
        LocalGet(pb), I32Load(m32(REACTIONS)), LocalTee(cur), LocalSet(last),
        LocalGet(pb), I32Const(0), I32Store(m32(REACTIONS)),
        // Reverse newest-first into registration order, filling slots 3/4.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(cur), I32Eqz, BrIf(1),
        LocalGet(cur), I64Load(m64(NEXT)), I32WrapI64, LocalSet(next),
        LocalGet(cur),
    );
    link(&mut f, prev);
    wasm!(f;
        I64Store(m64(NEXT)),
        LocalGet(cur), LocalGet(cur), I64Load(m64(KIND)),
        LocalGet(state), I32Const(STATE_SHIFT), I32Shl, I64ExtendI32U, I64Or,
        I64Store(m64(KIND)),
        LocalGet(cur), LocalGet(v), I64Store(m64(JOB_VALUE)),
        LocalGet(cur), LocalSet(prev),
        LocalGet(next), LocalSet(cur),
        Br(0), End, End,
        LocalGet(prev),
        If(BlockType::Empty),
        // Splice `prev` (oldest) .. `last` (newest) onto the queue.
        GlobalGet(JOB_TAIL_GLOBAL), I32Eqz,
        If(BlockType::Empty),
        LocalGet(prev), GlobalSet(JOB_HEAD_GLOBAL),
        Else,
        GlobalGet(JOB_TAIL_GLOBAL),
    );
    link(&mut f, prev);
    wasm!(f;
        I64Store(m64(NEXT)),
        End,
        LocalGet(last), GlobalSet(JOB_TAIL_GLOBAL),
        Return,
        End,
        // No reaction: track a rejection nobody handled yet.
        LocalGet(state), I32Const(REJECTED), I32Eq,
        LocalGet(pb), I32Load8U(m8(FLAGS)), I32Const(HANDLED | IN_REJECTED_LIST), I32And, I32Eqz,
        I32And,
        If(BlockType::Empty),
        LocalGet(pb), GlobalGet(REJECTED_HEAD_GLOBAL), I32Store(m32(REJECTED_NEXT)),
        LocalGet(pb), LocalGet(pb), I32Load8U(m8(FLAGS)), I32Const(IN_REJECTED_LIST), I32Or,
        I32Store8(m8(FLAGS)),
        LocalGet(pb), GlobalSet(REJECTED_HEAD_GLOBAL),
        End,
        End,
    );
    finish_builder(f)
}

/// `__promise_then(p, on_f, on_r)` (`finally = false`) or
/// `__promise_finally(p, on_finally)`: a fresh derived promise registered as
/// the result of a reaction on `p`.
fn emit_then(helpers: Helpers<'_>, leaves: &LeafSet, finally: bool) -> Function {
    let params: &[_] = if finally {
        &[ROOT_JS, ROOT_JS]
    } else {
        &[ROOT_JS, ROOT_JS, ROOT_JS]
    };
    let mut f = helper_builder(leaves, params, &[ROOT_JS]);
    let (p, on_f) = (0, 1);
    let on_r = if finally { 1 } else { 2 };
    let derived = if finally { 2 } else { 3 };
    wasm!(f; I64Const(promise_proto(helpers)), Call(helpers.promise_new()));
    box_heap(&mut f);
    wasm!(f;
        LocalSet(derived),
        LocalGet(p), LocalGet(on_f), LocalGet(on_r), LocalGet(derived),
        I32Const(if finally { FINALLY } else { 0 }),
        Call(helpers.promise_react()),
        LocalGet(derived),
        End,
    );
    finish_builder(f)
}

/// `__promise_resolve(v)` (`reject = false`): `v` itself when it is a
/// promise, else a fresh promise fulfilled with `v`; `__promise_reject(v)`:
/// a fresh promise rejected with `v`.
fn emit_resolve(helpers: Helpers<'_>, leaves: &LeafSet, reject: bool) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS], &[ROOT_JS]);
    let (v, out) = (0, 1);
    if !reject {
        is_promise(&mut f, v);
        wasm!(f; If(BlockType::Empty), LocalGet(v), Return, End);
    }
    wasm!(f; I64Const(promise_proto(helpers)), Call(helpers.promise_new()));
    box_heap(&mut f);
    wasm!(f;
        LocalTee(out),
        I32Const(if reject { REJECTED } else { FULFILLED }),
        LocalGet(v),
        Call(helpers.promise_settle()),
        LocalGet(out),
        End,
    );
    finish_builder(f)
}

/// `__run_job(rec)`: run one job. The callback is selected by the source
/// state; a non-callable one passes the settlement through. Otherwise it is
/// called through `__invoke1` (a `finally` callback with `undefined`), and the
/// derived promise is settled from the outcome: a throw (the pending
/// exception, cleared here) rejects it, a `finally` callback's normal return
/// passes the source settlement through, anything else fulfills it.
fn emit_run_job(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE],
        &[RAW_I32, RAW_I32, ROOT_JS, ROOT_JS, ROOT_JS, ROOT_JS],
    );
    let (rec, kind, state, callback, value, result, out) = (0, 1, 2, 3, 4, 5, 6);
    wasm!(f;
        LocalGet(rec), I64Load(m64(KIND)), I32WrapI64, LocalTee(kind),
        I32Const(STATE_SHIFT), I32ShrU, I32Const(3), I32And, LocalSet(state),
        LocalGet(rec), I64Load(m64(JOB_VALUE)), LocalSet(value),
        LocalGet(rec), I64Load(m64(RESULT)), LocalSet(result),
        LocalGet(state), I32Const(FULFILLED), I32Eq,
        If(BlockType::Result(I64)),
        LocalGet(rec), I64Load(m64(ON_FULFILLED)),
        Else,
        LocalGet(rec), I64Load(m64(ON_REJECTED)),
        End,
        LocalSet(callback),
    );
    emit_is_function(&mut f, callback);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(result), LocalGet(state), LocalGet(value), Call(helpers.promise_settle()),
        Return,
        End,
        LocalGet(callback),
        LocalGet(kind), I32Const(FINALLY), I32And,
        If(BlockType::Result(I64)), I64Const(UNDEFINED_BITS), Else, LocalGet(value), End,
        Call(helpers.invoke1), LocalSet(out),
        GlobalGet(EXC_PENDING_GLOBAL),
        If(BlockType::Empty),
        GlobalGet(EXC_VALUE_GLOBAL), LocalSet(out),
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
        I64Const(0), GlobalSet(EXC_VALUE_GLOBAL),
        LocalGet(result), I32Const(REJECTED), LocalGet(out), Call(helpers.promise_settle()),
        Return,
        End,
        LocalGet(kind), I32Const(FINALLY), I32And,
        If(BlockType::Empty),
        LocalGet(result), LocalGet(state), LocalGet(value), Call(helpers.promise_settle()),
        Return,
        End,
        LocalGet(result), I32Const(FULFILLED), LocalGet(out), Call(helpers.promise_settle()),
        End,
    );
    finish_builder(f)
}

/// `__drain()`: pop and run jobs until the queue is empty (a loop, never
/// recursion, so a long reaction chain uses no wasm stack), then the
/// checkpoint; repeat while the checkpoint left jobs queued.
fn emit_drain(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[], &[ROOT_BASE, RAW_I32, RAW_I32, RAW_I32]);
    let (rec, cur, prev, next) = (0, 1, 2, 3);
    wasm!(f;
        Loop(BlockType::Empty),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        GlobalGet(JOB_HEAD_GLOBAL), LocalTee(rec), I32Eqz, BrIf(1),
        LocalGet(rec), I64Load(m64(NEXT)), I32WrapI64, GlobalSet(JOB_HEAD_GLOBAL),
        GlobalGet(JOB_HEAD_GLOBAL), I32Eqz,
        If(BlockType::Empty), I32Const(0), GlobalSet(JOB_TAIL_GLOBAL), End,
        LocalGet(rec), Call(helpers.run_job()),
        Br(0), End, End,
        // Checkpoint 1: prune the promises a late reaction handled from the
        // rejected list.
        I32Const(0), LocalSet(prev),
        GlobalGet(REJECTED_HEAD_GLOBAL), LocalSet(cur),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(cur), I32Eqz, BrIf(1),
        LocalGet(cur), I32Load(m32(REJECTED_NEXT)), LocalSet(next),
        LocalGet(cur), I32Load8U(m8(FLAGS)), I32Const(HANDLED), I32And,
        If(BlockType::Empty),
        LocalGet(cur), LocalGet(cur), I32Load8U(m8(FLAGS)), I32Const(!IN_REJECTED_LIST), I32And,
        I32Store8(m8(FLAGS)),
        LocalGet(cur), I32Const(0), I32Store(m32(REJECTED_NEXT)),
        LocalGet(prev), I32Eqz,
        If(BlockType::Empty),
        LocalGet(next), GlobalSet(REJECTED_HEAD_GLOBAL),
        Else,
        LocalGet(prev), LocalGet(next), I32Store(m32(REJECTED_NEXT)),
        End,
        Else,
        LocalGet(cur), LocalSet(prev),
        End,
        LocalGet(next), LocalSet(cur),
        Br(0), End, End,
        // Checkpoint 2 (M3 hook): ClearKeptObjects.
        // Checkpoint 3 (M3 hook): queue the finalization cleanup jobs.
        // Loop again when the checkpoint queued jobs.
        GlobalGet(JOB_HEAD_GLOBAL), BrIf(0),
        End,
        End,
    );
    finish_builder(f)
}

/// `__first_unhandled() -> i64`: walk the rejected list, skip `HANDLED`
/// promises, and return the one with the minimum `seq` (creation order) as a
/// jsval, or `0` when there is none.
fn emit_first_unhandled(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[], &[RAW_I32, RAW_I32]);
    let (cur, best) = (0, 1);
    wasm!(f;
        GlobalGet(REJECTED_HEAD_GLOBAL), LocalSet(cur),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(cur), I32Eqz, BrIf(1),
        LocalGet(cur), I32Load8U(m8(FLAGS)), I32Const(HANDLED), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(best), I32Eqz,
        If(BlockType::Result(I32)),
        I32Const(1),
        Else,
        LocalGet(cur), I32Load(m32(SEQ)), LocalGet(best), I32Load(m32(SEQ)), I32LtU,
        End,
        If(BlockType::Empty), LocalGet(cur), LocalSet(best), End,
        End,
        LocalGet(cur), I32Load(m32(REJECTED_NEXT)), LocalSet(cur),
        Br(0), End, End,
        LocalGet(best), I32Eqz,
        If(BlockType::Result(I64)),
        I64Const(0),
        Else,
        LocalGet(best), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        End,
        End,
    );
    finish_builder(f)
}

/// `__promise_await(v) -> i64`: top-level `await v`. Mark a promise `v`
/// handled, drain the queue (so `await` yields even on a settled or
/// non-promise operand), then return the fulfillment value, or leave the
/// rejection reason in flight (`$exc_pending`) for the caller to propagate.
/// A non-promise yields itself; a promise still pending after the drain
/// (nothing can settle it any more) yields `undefined`.
fn emit_await(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS], &[RAW_I32]);
    let (v, promise) = (0, 1);
    is_promise(&mut f, v);
    wasm!(f;
        LocalTee(promise),
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, LocalGet(v), I32WrapI64, I32Load8U(m8(FLAGS)),
        I32Const(HANDLED), I32Or, I32Store8(m8(FLAGS)),
        End,
        Call(helpers.drain()),
        LocalGet(promise), I32Eqz, If(BlockType::Empty), LocalGet(v), Return, End,
        LocalGet(v), I32WrapI64, I32Load(m32(STATE)), I32Const(FULFILLED), I32Eq,
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, I64Load(m64(VALUE)), Return,
        End,
        LocalGet(v), I32WrapI64, I32Load(m32(STATE)), I32Const(REJECTED), I32Eq,
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, I64Load(m64(VALUE)), GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        End,
        I64Const(UNDEFINED_BITS),
        End,
    );
    finish_builder(f)
}
