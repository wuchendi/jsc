//! Async iteration (DESIGN.md §7.8): the async record nodes
//! ([`IrNode::OpenIterator`] with `is_async`, [`IrNode::AsyncIteratorNext`],
//! [`IrNode::AsyncIteratorComplete`], [`IrNode::AsyncIteratorReturn`],
//! [`IrNode::IteratorResultCheck`]), the internal builtins behind them, and
//! `%AsyncFromSyncIteratorPrototype%` (`CreateAsyncFromSyncIterator`, its
//! `next` / `return` / `throw` and the reactions of
//! `AsyncFromSyncIteratorContinuation`). There is no fast path: every step
//! is a microtask turn by specification, awaited by the caller's state
//! machine.

use super::iter::guarded;
use super::iter::{
    ITERATOR_NOT_OBJECT, ST_DONE, ST_PROTOCOL, ST_STEPPING, call_internal, call_method, concat,
    describe, emit_record_fill, emit_result_done, iter_result, load, not_a_function,
    result_not_object, state_is, store, throw_and_return, throw_type,
};
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::{AsyncGenRequest, AsyncIterOp, IterOp};
use crate::ir::ConvertOp;

/// The `kind` byte of an async-from-sync iterator object (DESIGN.md §5.2).
const KIND_ASYNC_FROM_SYNC: i32 = 32;
/// Offset of an iterator object's `[[IteratedObject]]` jsval.
const ITERATED_OFFSET: u32 = abi::OBJ_PREFIX_SIZE;
/// Offset of an iterator object's `[[NextIndex]]: u32`.
const INDEX_OFFSET: u32 = ITERATED_OFFSET + 8;
/// Offset of an iterator object's `kind: u8` (and its three pad bytes).
const KIND_OFFSET: u32 = ITERATED_OFFSET + 12;
/// Offset of an async-from-sync object's sync iterator jsval.
const SYNC_ITER_OFFSET: u32 = 32;
/// Offset of an async-from-sync object's cached sync `next` jsval.
const SYNC_NEXT_OFFSET: u32 = 40;
/// Byte size of an async-from-sync iterator object block: the 32-byte
/// iterator head, then the sync iterator record.
const BLOCK_SIZE: i32 = 48;

/// The `TypeError` of an `@@asyncIterator` method returning a non-object.
pub(super) const ASYNC_ITERATOR_NOT_OBJECT: CalleeErrorText<'static> = [
    "Result of the Symbol.asyncIterator method is not an object",
    "TypeError: Result of the Symbol.asyncIterator method is not an object",
];

/// The `TypeError` of `throw` on a sync iterator without a `throw` method
/// (after closing it).
const NO_THROW_METHOD: CalleeErrorText<'static> = [
    "The iterator does not provide a 'throw' method",
    "TypeError: The iterator does not provide a 'throw' method",
];

/// The receiver `TypeError`s of `next`, `return` and `throw`.
const INCOMPATIBLE: [CalleeErrorText<'static>; 3] = [
    [
        "next method called on incompatible receiver",
        "TypeError: next method called on incompatible receiver",
    ],
    [
        "return method called on incompatible receiver",
        "TypeError: return method called on incompatible receiver",
    ],
    [
        "throw method called on incompatible receiver",
        "TypeError: throw method called on incompatible receiver",
    ],
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    INCOMPATIBLE
        .into_iter()
        .flatten()
        .chain(ASYNC_ITERATOR_NOT_OBJECT)
        .chain(NO_THROW_METHOD)
        .chain([" is not async iterable", "return", "throw"])
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    let local = frame.i64(0);
    let mut nodes = vec![
        not_async_iterable(local, local),
        IrNode::PromiseResolve(Box::new(iter_result(IrNode::Local(local), true))),
        unwrapped(local, local),
        IrNode::Throw(Box::new(type_error(NO_THROW_METHOD))),
        IrNode::PromiseReject(Box::new(IrNode::Local(local))),
    ];
    nodes.extend(INCOMPATIBLE.map(|text| IrNode::PromiseReject(Box::new(type_error(text)))));
    nodes
}

/// A fresh `TypeError` with `text`.
fn type_error(text: CalleeErrorText<'static>) -> IrNode {
    IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Str(text[0].into())),
        IrNode::Str(text[1].into()),
    )
}

/// `<x> is not async iterable` for the jsval in `local` (`String(x)` of a
/// primitive, `typeof x` of an object), thrown as a `TypeError` (`tmp` an
/// i64 temp).
fn not_async_iterable(local: u32, tmp: u32) -> IrNode {
    let name = IrNode::Ternary {
        cond: Box::new(IrNode::IsObject(Box::new(IrNode::Local(local)))),
        then_branch: Box::new(IrNode::TypeOf(Box::new(IrNode::Local(local)))),
        else_branch: Box::new(describe(local)),
    };
    throw_type(
        concat(name, IrNode::Str(" is not async iterable".into())),
        tmp,
    )
}

/// `{ value, done }` for the jsvals in `value` and `done`.
fn unwrapped(value: u32, done: u32) -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".into()), IrNode::Local(value)),
            (PropKey::Static("done".into()), IrNode::Local(done)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

fn m32(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    }
}

fn m64(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 3,
        memory_index: 0,
    }
}

/// Emit the body of the async iteration builtin `op`.
pub(super) fn emit(
    op: AsyncIterOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        AsyncIterOp::Open => emit_open_body(function, frame, strings, helpers),
        AsyncIterOp::Step => emit_step_body(function, frame, strings, helpers),
        AsyncIterOp::Complete => {
            let (r, done, tmp) = (frame.i64(0), frame.i64(1), frame.i64(2));
            wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(r));
            emit_result_done(function, frame, r, done, tmp, strings, helpers);
        }
        AsyncIterOp::Return => emit_return_body(function, frame, strings, helpers),
        AsyncIterOp::FromSync(request) => {
            emit_from_sync(request, function, frame, strings, helpers);
        }
        AsyncIterOp::Unwrap => {
            let value = frame.i64(0);
            frame.arg(function, 0);
            wasm!(function; LocalSet(value));
            emit_node(
                &unwrapped(value, frame.k),
                function,
                frame.scratch(),
                strings,
                helpers,
            );
        }
        AsyncIterOp::CloseRejected => emit_close_rejected(function, frame, helpers),
        AsyncIterOp::FromAsync(op) => {
            super::array::emit_from_async(op, function, frame, strings, helpers);
        }
    }
}

// --- the async record nodes ---------------------------------------------------

/// Emit [`IrNode::OpenIterator`] with `is_async`: evaluate `value`, then
/// open its async iterator ([`AsyncIterOp::Open`]) into `rec` (a throw
/// leaves the record done).
pub(in crate::emit) fn emit_open_async_iterator(
    value: &IrNode,
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, it) = (scratch + 22, scratch + 24);
    emit_node(value, function, scratch, strings, helpers);
    wasm!(function; LocalSet(v));
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    call_internal(
        function,
        Builtin::Iter(IterOp::Async(AsyncIterOp::Open)),
        v,
        None,
        helpers,
    );
    wasm!(function; LocalSet(it));
    emit_propagate_if_pending(function, helpers);
    emit_record_fill(function, it, rec, scratch, strings, helpers);
}

/// Emit [`IrNode::AsyncIteratorNext`], pushing the result of `rec.next`
/// called on `rec.iter` ([`AsyncIterOp::Step`]) with the record marked
/// stepping.
pub(in crate::emit) fn emit_async_iterator_next(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (iter, next) = (scratch + 22, scratch + 24);
    store(
        function,
        &rec.state,
        IrNode::Num(ST_STEPPING),
        scratch,
        strings,
        helpers,
    );
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function; LocalSet(iter));
    load(function, &rec.next, scratch, strings, helpers);
    wasm!(function; LocalSet(next));
    call_internal(
        function,
        Builtin::Iter(IterOp::Async(AsyncIterOp::Step)),
        iter,
        Some(next),
        helpers,
    );
    emit_propagate_if_pending(function, helpers);
}

/// Emit [`IrNode::AsyncIteratorComplete`], pushing i32 `1` when the
/// awaited `result` produced a value (left in `rec.result`, the record open
/// again), `0` once done ([`AsyncIterOp::Complete`]).
pub(in crate::emit) fn emit_async_iterator_complete(
    rec: &IterRecord,
    result: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let r = scratch + 22;
    emit_node(result, function, scratch, strings, helpers);
    wasm!(function; LocalSet(r));
    call_internal(
        function,
        Builtin::Iter(IterOp::Async(AsyncIterOp::Complete)),
        r,
        None,
        helpers,
    );
    wasm!(function; LocalSet(r));
    emit_propagate_if_pending(function, helpers);
    wasm!(function; LocalGet(r), I64Const(HOLE_BITS), I64Eq, If(BlockType::Result(ValType::I32)));
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(0), Else);
    store(
        function,
        &rec.result,
        IrNode::Local(r),
        scratch,
        strings,
        helpers,
    );
    store(
        function,
        &rec.state,
        IrNode::Num(ST_PROTOCOL),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(1), End);
}

/// Emit [`IrNode::AsyncIteratorReturn`], pushing the unawaited result of
/// the `return()` call of an open record ([`AsyncIterOp::Return`]), or the
/// hole (a done or stepping record, no `return` method, or with `throwing`
/// a discarded throw). The record is done afterwards.
pub(in crate::emit) fn emit_async_iterator_return(
    rec: &IterRecord,
    throwing: bool,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (iter, r) = (scratch + 22, scratch + 24);
    state_is(function, rec, ST_PROTOCOL, scratch, strings, helpers);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function; LocalSet(iter));
    call_internal(
        function,
        Builtin::Iter(IterOp::Async(AsyncIterOp::Return)),
        iter,
        None,
        helpers,
    );
    wasm!(function; LocalSet(r));
    if throwing {
        wasm!(function;
            GlobalGet(EXC_PENDING_GLOBAL),
            If(BlockType::Empty),
            I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
            I64Const(HOLE_BITS), LocalSet(r),
            End,
        );
    } else {
        emit_propagate_if_pending(function, helpers);
    }
    wasm!(function; LocalGet(r));
    leave_frame(helpers);
    wasm!(function; Else, I64Const(HOLE_BITS), End);
}

/// Emit [`IrNode::IteratorResultCheck`]: a `TypeError` unless the operand
/// is an object; pushes `undefined`.
pub(in crate::emit) fn emit_iterator_result_check(
    operand: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (r, tmp) = (scratch + 22, scratch + 23);
    emit_node(operand, function, scratch, strings, helpers);
    wasm!(function; LocalSet(r));
    emit_is_object(function, r);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    enter_frame(helpers);
    emit_node(
        &result_not_object(r, tmp),
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function; Drop);
    leave_frame(helpers);
    wasm!(function; End, I64Const(UNDEFINED_BITS));
}

// --- the internal builtins ----------------------------------------------------

/// The [`AsyncIterOp::Open`] body: `GetIterator(this, async)`.
/// `GetMethod(this, @@asyncIterator)` is called (a `TypeError` unless the
/// result is an object); absent, `GetMethod(this, @@iterator)` is called
/// (likewise) and its iterator and `next` wrapped by
/// `CreateAsyncFromSyncIterator`; neither callable throws `<x> is not async
/// iterable`.
fn emit_open_body(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, method, it, next, obj) = (
        frame.i64(0),
        frame.i64(1),
        frame.i64(2),
        frame.i64(3),
        frame.i32(0),
    );
    let tmp = frame.x64(0);
    let get_method = |function: &mut Function, symbol| {
        wasm!(function;
            LocalGet(v), I32Const(realm::symbol_addr(symbol, helpers)),
            Call(helpers.symbol_get()), LocalSet(method),
        );
        return_if_pending(function);
    };
    let require_callable = |function: &mut Function| {
        emit_is_function(function, method);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        throw_and_return(
            function,
            frame,
            &not_async_iterable(v, tmp),
            strings,
            helpers,
        );
        wasm!(function; End);
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(v));
    get_method(function, WellKnown::AsyncIterator);
    emit_is_nullish(function, method);
    wasm!(function; If(BlockType::Empty));
    get_method(function, WellKnown::Iterator);
    require_callable(function);
    call_method(function, frame, method, v, it, helpers);
    emit_is_object(function, it);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, ITERATOR_NOT_OBJECT, strings, helpers);
    wasm!(function;
        End,
        LocalGet(it), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(next),
    );
    return_if_pending(function);
    create(function, it, next, obj, helpers);
    wasm!(function; Return, End);
    require_callable(function);
    call_method(function, frame, method, v, it, helpers);
    emit_is_object(function, it);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, ASYNC_ITERATOR_NOT_OBJECT, strings, helpers);
    wasm!(function; End, LocalGet(it));
}

/// `CreateAsyncFromSyncIterator`: push a fresh `CLASS_ITERATOR` object of
/// kind [`KIND_ASYNC_FROM_SYNC`] over `%AsyncFromSyncIteratorPrototype%`
/// holding the sync iterator in i64 `it` and its `next` in i64 `next`
/// (`obj` an i32 temp).
pub(super) fn create(function: &mut Function, it: u32, next: u32, obj: u32, helpers: Helpers<'_>) {
    let header = TYPE_OBJECT | (i32::from(abi::CLASS_ITERATOR) << 24);
    let proto = intrinsic_bits(Intrinsic::AsyncFromSyncIteratorPrototype, helpers);
    wasm!(function;
        I32Const(BLOCK_SIZE), Call(helpers.alloc), LocalTee(obj),
        I32Const(header), I32Store(m32(0)),
        LocalGet(obj), I32Const(0), I32Store(m32(abi::OBJ_PROPS_OFFSET)),
        LocalGet(obj), I64Const(proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(obj), I64Const(UNDEFINED_BITS), I64Store(m64(ITERATED_OFFSET)),
        LocalGet(obj), I32Const(0), I32Store(m32(INDEX_OFFSET)),
        LocalGet(obj), I32Const(KIND_ASYNC_FROM_SYNC), I32Store(m32(KIND_OFFSET)),
        LocalGet(obj), LocalGet(it), I64Store(m64(SYNC_ITER_OFFSET)),
        LocalGet(obj), LocalGet(next), I64Store(m64(SYNC_NEXT_OFFSET)),
        LocalGet(obj), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// The [`AsyncIterOp::Step`] body: `this` is the iterator, `env` its cached
/// `next` (a `TypeError` unless callable); the result of `next()`.
fn emit_step_body(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (it, r, tmp, next) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.k);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(it));
    emit_is_function(function, next);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &not_a_function(next, tmp),
        strings,
        helpers,
    );
    wasm!(function; End);
    call_method(function, frame, next, it, r, helpers);
    wasm!(function; LocalGet(r));
}

/// The [`AsyncIterOp::Return`] body: `this` is the iterator;
/// `GetMethod(this, "return")` (the hole when nullish, a `TypeError` unless
/// callable) and the result of its call.
fn emit_return_body(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (it, method, r, tmp) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wasm!(function;
        GlobalGet(THIS_GLOBAL), LocalTee(it),
        I32Const(static_key(strings, "return")), Call(helpers.get), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_nullish(function, method);
    wasm!(function; If(BlockType::Empty), I64Const(HOLE_BITS), Return, End);
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &not_a_function(method, tmp),
        strings,
        helpers,
    );
    wasm!(function; End);
    call_method(function, frame, method, it, r, helpers);
    wasm!(function; LocalGet(r));
}

/// The [`AsyncIterOp::CloseRejected`] body: `IteratorClose` of the sync
/// iterator in `env` with a throw completion (any throw of the close is
/// discarded), then throw argument 0 (rejecting the derived promise).
fn emit_close_rejected(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let (reason, it) = (frame.i64(0), frame.k);
    frame.arg(function, 0);
    wasm!(function; LocalSet(reason));
    call_internal(
        function,
        Builtin::Protocol(ProtocolOp::Close),
        it,
        None,
        helpers,
    );
    wasm!(function;
        Drop,
        LocalGet(reason), GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I64Const(UNDEFINED_BITS),
    );
}

// --- %AsyncFromSyncIteratorPrototype% -----------------------------------------

/// The locals of a `%AsyncFromSyncIteratorPrototype%` method body.
#[derive(Clone, Copy)]
struct Wrap {
    frame: Frame,
    /// The receiver (i64).
    this: u32,
    /// Its sync iterator (i64).
    sync: u32,
    /// The sync method, then the result's `value` (i64).
    method: u32,
    /// The sync method's result (i64).
    result: u32,
    /// Argument 0 (i64).
    value: u32,
    /// The returned promise (i64).
    out: u32,
    /// `ToBoolean(result.done)` (i64).
    done: u32,
    /// An i64 temp.
    tmp: u32,
}

impl Wrap {
    fn new(frame: Frame) -> Self {
        Self {
            frame,
            this: frame.i64(0),
            sync: frame.i64(1),
            method: frame.i64(2),
            result: frame.i64(3),
            value: frame.x64(0),
            out: frame.x64(1),
            done: frame.y64(0),
            tmp: frame.y64(1),
        }
    }

    /// Emit `node` in the body's scratch layout.
    fn emit(
        self,
        function: &mut Function,
        node: &IrNode,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        emit_node(node, function, self.frame.scratch(), strings, helpers);
    }

    /// Throw `node` (an IR throw) to the body's handler.
    fn throw(
        self,
        function: &mut Function,
        node: &IrNode,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        self.emit(function, node, strings, helpers);
        wasm!(function; Drop);
    }

    /// Throw `node` (an IR throw) when the i32 on the stack is zero.
    fn throw_unless(
        self,
        function: &mut Function,
        node: &IrNode,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        wasm!(function; I32Eqz, If(BlockType::Empty));
        enter_frame(helpers);
        self.throw(function, node, strings, helpers);
        leave_frame(helpers);
        wasm!(function; End);
    }

    /// `method = Get(sync, key)` (a throw propagates to the handler).
    fn get_method(
        self,
        function: &mut Function,
        key: &str,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        wasm!(function;
            LocalGet(self.sync), I32Const(static_key(strings, key)), Call(helpers.get),
            LocalSet(self.method),
        );
        emit_propagate_if_pending(function, helpers);
    }

    /// `result = Call(method, sync, « value »)` (no argument when the
    /// method itself got none): a `TypeError` unless `method` is callable.
    fn call(self, function: &mut Function, strings: &StrOffsets, helpers: Helpers<'_>) {
        emit_is_function(function, self.method);
        self.throw_unless(
            function,
            &not_a_function(self.method, self.tmp),
            strings,
            helpers,
        );
        wasm!(function; LocalGet(self.value));
        for _ in 1..self.frame.k {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
        let [one, zero] = [1.0f64, 0.0].map(|n| n.to_bits().cast_signed());
        wasm!(function; LocalGet(self.sync), GlobalSet(THIS_GLOBAL));
        invoke(
            function,
            self.method,
            |function| {
                wasm!(function;
                    I64Const(one), I64Const(zero),
                    LocalGet(self.frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Gt,
                    Select,
                );
            },
            helpers,
        );
        wasm!(function; LocalSet(self.result));
        emit_propagate_if_pending(function, helpers);
    }

    /// `AsyncFromSyncIteratorContinuation(result, closeOnRejection)` into
    /// `out`: a `TypeError` unless `result` is an object; `done`, `value`;
    /// then `PromiseResolve(value).then(v => ({ value: v, done }),
    /// onRejected)` where `onRejected` closes the sync iterator and rethrows
    /// when `close_on_rejection` and not `done`, and passes the rejection
    /// through otherwise.
    fn continuation(
        self,
        close_on_rejection: bool,
        function: &mut Function,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        emit_is_object(function, self.result);
        self.throw_unless(
            function,
            &result_not_object(self.result, self.tmp),
            strings,
            helpers,
        );
        wasm!(function;
            LocalGet(self.result), I32Const(static_key(strings, "done")), Call(helpers.get),
            LocalSet(self.done),
        );
        emit_propagate_if_pending(function, helpers);
        let truthy = IrNode::ConvertCall {
            op: ConvertOp::ToBoolean,
            arg: Box::new(IrNode::Local(self.done)),
            radix: None,
        };
        self.emit(function, &truthy, strings, helpers);
        wasm!(function;
            LocalSet(self.done),
            LocalGet(self.result), I32Const(static_key(strings, "value")), Call(helpers.get),
            LocalSet(self.method),
        );
        emit_propagate_if_pending(function, helpers);
        let scratch = self.frame.scratch();
        let slot = |op| helpers.builtin_slot(Builtin::Iter(IterOp::Async(op)));
        wasm!(function; LocalGet(self.method));
        emit_settler_value(
            slot(AsyncIterOp::Unwrap),
            self.done,
            function,
            scratch,
            helpers,
        );
        if close_on_rejection {
            wasm!(function;
                LocalGet(self.done), I64Const(TRUE_BITS), I64Eq,
                If(BlockType::Result(ValType::I64)), I64Const(UNDEFINED_BITS), Else,
            );
            emit_settler_value(
                slot(AsyncIterOp::CloseRejected),
                self.sync,
                function,
                scratch,
                helpers,
            );
            wasm!(function; End);
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
        wasm!(function; Call(helpers.then), LocalSet(self.out));
    }
}

/// `%AsyncFromSyncIteratorPrototype%.next` / `return` / `throw` (ES2025
/// §27.1.6): never a synchronous throw — a receiver that is not an
/// async-from-sync iterator, and any abrupt completion of the sync
/// iterator's method, its result checks or the continuation, reject the
/// returned promise. `return` without a sync `return` resolves `{ value,
/// done: true }`; `throw` without a sync `throw` closes the sync iterator
/// (a non-throw completion), then rejects with a `TypeError`.
fn emit_from_sync(
    request: AsyncGenRequest,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if frame.k == 0 {
        // The async iteration consumers widen `K` (they pass the received
        // value through the slots); no program without them has a wrapper.
        wasm!(function; Unreachable);
        return;
    }
    let w = Wrap::new(frame);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(w.this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(w.value));
    emit_is_wrapper(function, w.this, abi::CLASS_ITERATOR);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(w.this), I32WrapI64, I32Load8U(MemArg { offset: u64::from(KIND_OFFSET), align: 0, memory_index: 0 }),
        I32Const(KIND_ASYNC_FROM_SYNC), I32Eq,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty),
    );
    let text = INCOMPATIBLE[match request {
        AsyncGenRequest::Next => 0,
        AsyncGenRequest::Return => 1,
        AsyncGenRequest::Throw => 2,
    }];
    w.emit(
        function,
        &IrNode::PromiseReject(Box::new(type_error(text))),
        strings,
        helpers,
    );
    wasm!(function;
        Return, End,
        LocalGet(w.this), I32WrapI64, I64Load(m64(SYNC_ITER_OFFSET)), LocalSet(w.sync),
    );
    guarded(function, helpers, |function| match request {
        AsyncGenRequest::Next => {
            wasm!(function;
                LocalGet(w.this), I32WrapI64, I64Load(m64(SYNC_NEXT_OFFSET)), LocalSet(w.method),
            );
            w.call(function, strings, helpers);
            w.continuation(true, function, strings, helpers);
        }
        AsyncGenRequest::Return => {
            w.get_method(function, "return", strings, helpers);
            emit_is_nullish(function, w.method);
            wasm!(function; If(BlockType::Empty));
            enter_frame(helpers);
            let done = IrNode::PromiseResolve(Box::new(iter_result(IrNode::Local(w.value), true)));
            w.emit(function, &done, strings, helpers);
            wasm!(function; LocalSet(w.out), Else);
            w.call(function, strings, helpers);
            w.continuation(false, function, strings, helpers);
            leave_frame(helpers);
            wasm!(function; End);
        }
        AsyncGenRequest::Throw => {
            w.get_method(function, "throw", strings, helpers);
            emit_is_nullish(function, w.method);
            wasm!(function; If(BlockType::Empty));
            enter_frame(helpers);
            call_internal(
                function,
                Builtin::Protocol(ProtocolOp::Close),
                w.sync,
                None,
                helpers,
            );
            wasm!(function; Drop);
            emit_propagate_if_pending(function, helpers);
            w.throw(
                function,
                &IrNode::Throw(Box::new(type_error(NO_THROW_METHOD))),
                strings,
                helpers,
            );
            wasm!(function; Else);
            w.call(function, strings, helpers);
            w.continuation(true, function, strings, helpers);
            leave_frame(helpers);
            wasm!(function; End);
        }
    });
    // IfAbruptRejectPromise.
    wasm!(function;
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty),
        GlobalGet(EXC_VALUE_GLOBAL), LocalSet(w.tmp),
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL), I64Const(0), GlobalSet(EXC_VALUE_GLOBAL),
    );
    w.emit(
        function,
        &IrNode::PromiseReject(Box::new(IrNode::Local(w.tmp))),
        strings,
        helpers,
    );
    wasm!(function; LocalSet(w.out), End, LocalGet(w.out));
}
