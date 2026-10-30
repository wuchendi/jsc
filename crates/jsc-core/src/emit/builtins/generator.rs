//! `%GeneratorPrototype%` methods over the generator state machine.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The receiver `TypeError`s of `next`, `return` and `throw`.
const NOT_GENERATOR: [CalleeErrorText<'static>; 3] = [
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

/// The `TypeError` of `return()` at a suspended `yield` that must run a
/// `finally` or forward to a `yield*` delegate.
const RETURN_UNSUPPORTED: CalleeErrorText<'static> = [
    "generator return() at a yield inside try/finally or yield* is not supported",
    "TypeError: generator return() at a yield inside try/finally or yield* is not supported",
];

/// The `TypeError` of `throw()` at a suspended `yield` that must forward to
/// a `yield*` delegate.
const THROW_UNSUPPORTED: CalleeErrorText<'static> = [
    "generator throw() at a yield* is not supported",
    "TypeError: generator throw() at a yield* is not supported",
];

/// A `state` past every state-machine block: resuming it returns
/// `undefined`, like a generator the machine itself completed.
const COMPLETED_STATE: i32 = i32::MAX;

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    NOT_GENERATOR
        .into_iter()
        .flatten()
        .chain(RETURN_UNSUPPORTED)
        .chain(THROW_UNSUPPORTED)
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let local = Frame { k }.i64(0);
    vec![
        resume(local, local),
        done_result(local),
        IrNode::Throw(Box::new(IrNode::Local(local))),
    ]
}

/// `gen.next(sent)` on the generator in `gen`.
fn resume(generator: u32, sent: u32) -> IrNode {
    IrNode::GenResume {
        handle: Box::new(IrNode::Local(generator)),
        sent: Box::new(IrNode::Local(sent)),
    }
}

/// `{ value, done: true }` for the value in `value`.
fn done_result(value: u32) -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".into()), IrNode::Local(value)),
            (PropKey::Static("done".into()), IrNode::Bool(true)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// Read `this` into `this` and argument 0 into `arg`, throwing `text`
/// unless `this` is a generator.
fn this_generator(
    function: &mut Function,
    frame: Frame,
    this: u32,
    arg: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    emit_is_generator(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
    field(function, this, abi::GEN_LIMITS_OFFSET);
    wasm!(function; I32Const(abi::GEN_RUNNING.cast_signed()), I32And, If(BlockType::Empty));
    throw_type_error(function, frame, GENERATOR_RUNNING, strings, helpers);
    wasm!(function; End);
}

/// Push the generator's i32 field at `offset`.
fn field(function: &mut Function, generator: u32, offset: u32) {
    wasm!(function;
        LocalGet(generator), I32WrapI64,
        I32Load(MemArg { offset: u64::from(offset), align: 2, memory_index: 0 }),
    );
}

/// Push `1` when the generator has not started or has completed (no `yield`
/// is suspended).
fn not_suspended(function: &mut Function, generator: u32) {
    field(function, generator, abi::GEN_DONE_OFFSET);
    field(function, generator, abi::GEN_STATE_OFFSET);
    wasm!(function; I32Eqz, I32Or);
}

/// Complete the generator: `done = 1` and a state past every block.
fn complete(function: &mut Function, generator: u32) {
    for (offset, value) in [
        (abi::GEN_DONE_OFFSET, 1),
        (abi::GEN_STATE_OFFSET, COMPLETED_STATE),
    ] {
        wasm!(function;
            LocalGet(generator), I32WrapI64, I32Const(value),
            I32Store(MemArg { offset: u64::from(offset), align: 2, memory_index: 0 }),
        );
    }
}

/// Throw `text` when the generator's `limits` has `bit`.
#[allow(clippy::too_many_arguments)]
fn check_limit(
    function: &mut Function,
    frame: Frame,
    generator: u32,
    bit: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    field(function, generator, abi::GEN_LIMITS_OFFSET);
    wasm!(function; I32Const(bit.cast_signed()), I32And, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// `next(value)`: resume at the suspended `yield` with `value`.
pub(super) fn emit_next(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, arg) = (frame.i64(0), frame.i64(1));
    this_generator(
        function,
        frame,
        this,
        arg,
        NOT_GENERATOR[0],
        strings,
        helpers,
    );
    emit_node(
        &resume(this, arg),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}

/// `return(value)`: complete the generator and yield `{ value, done: true }`.
/// At a suspended `yield` this is only supported when no `finally` or
/// `yield*` must run (the generator's `limits`).
pub(super) fn emit_return(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, arg) = (frame.i64(0), frame.i64(1));
    this_generator(
        function,
        frame,
        this,
        arg,
        NOT_GENERATOR[1],
        strings,
        helpers,
    );
    not_suspended(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    check_limit(
        function,
        frame,
        this,
        abi::GEN_LIMIT_RETURN,
        RETURN_UNSUPPORTED,
        strings,
        helpers,
    );
    wasm!(function; End);
    complete(function, this);
    emit_node(
        &done_result(arg),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}

/// `throw(exception)`: throw it at the suspended `yield` (resuming the state
/// machine with it pending), or, when no `yield` is suspended, complete the
/// generator and throw it.
pub(super) fn emit_throw(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, arg) = (frame.i64(0), frame.i64(1));
    this_generator(
        function,
        frame,
        this,
        arg,
        NOT_GENERATOR[2],
        strings,
        helpers,
    );
    let throw = IrNode::Throw(Box::new(IrNode::Local(arg)));
    not_suspended(function, this);
    wasm!(function; If(BlockType::Empty));
    complete(function, this);
    emit_drop(function, frame, &throw, strings, helpers);
    wasm!(function; End);
    check_limit(
        function,
        frame,
        this,
        abi::GEN_LIMIT_THROW,
        THROW_UNSUPPORTED,
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(arg), GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
    );
    let undefined = frame.i64(2);
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(undefined));
    emit_node(
        &resume(this, undefined),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}

// ---------------------------------------------------------------------------
// Async generators (DESIGN.md §7.8): `%AsyncGeneratorPrototype%` queues each
// `next` / `return` / `throw` as a request record holding the request's
// promise, and serves the queue from the front: a request resumes the body
// (its state machine, where `await` suspends are flagged `GEN_AWAIT`) until it
// yields or completes, which settles the front request's promise; an `await`
// (or a `return(v)` value) registers promise reactions that continue serving
// the queue from the microtask queue.

/// The `TypeError` of an `%AsyncGeneratorPrototype%` method on a receiver
/// that is not an async generator (the returned promise rejects with it).
const NOT_ASYNC_GENERATOR: [CalleeErrorText<'static>; 3] = [
    [
        "next method called on incompatible receiver",
        "TypeError: next method called on incompatible receiver",
    ],
    [
        "throw method called on incompatible receiver",
        "TypeError: throw method called on incompatible receiver",
    ],
    [
        "return method called on incompatible receiver",
        "TypeError: return method called on incompatible receiver",
    ],
];

/// The rejection of `return()` at a suspended `yield` that must run a
/// `finally`.
const ASYNC_RETURN_UNSUPPORTED: CalleeErrorText<'static> = [
    "async generator return() at a yield inside try/finally is not supported",
    "TypeError: async generator return() at a yield inside try/finally is not supported",
];

/// The static strings of the async generator bodies.
pub(super) fn async_strings() -> impl Iterator<Item = &'static str> {
    NOT_ASYNC_GENERATOR
        .into_iter()
        .flatten()
        .chain(ASYNC_RETURN_UNSUPPORTED)
}

/// The IR fragments the async generator bodies emit, for the static-string
/// scan.
pub(super) fn async_fragments(k: u32) -> Vec<IrNode> {
    let local = Frame { k }.i64(0);
    vec![
        iter_result(local, false),
        type_error(ASYNC_RETURN_UNSUPPORTED),
    ]
}

/// A request record: a `TYPE_ENV` block of four slots — the request's
/// value, its promise, its [`AsyncGenRequest`] code, and the next record's
/// base (`0` at the tail).
const REQ_VALUE: u64 = HEADER_SIZE as u64 + 4;
const REQ_PROMISE: u64 = REQ_VALUE + 8;
const REQ_KIND: u64 = REQ_VALUE + 16;
const REQ_NEXT: u64 = REQ_VALUE + 24;
const REQ_SIZE: i32 = HEADER_SIZE + 4 + 32;

/// The queue's head (u32) and tail (u32) in the generator block.
const QUEUE_HEAD: u64 = abi::GEN_QUEUE_OFFSET as u64;
const QUEUE_TAIL: u64 = QUEUE_HEAD + 4;

/// A request's code in its record.
fn request_code(request: AsyncGenRequest) -> i64 {
    match request {
        AsyncGenRequest::Next => 0,
        AsyncGenRequest::Throw => 1,
        AsyncGenRequest::Return => 2,
    }
}

fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn m64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// `{ value, done }` for the value in `value`.
fn iter_result(value: u32, done: bool) -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".into()), IrNode::Local(value)),
            (PropKey::Static("done".into()), IrNode::Bool(done)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// A fresh `TypeError` with `text`.
fn type_error(text: CalleeErrorText<'static>) -> IrNode {
    IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Str(text[0].into())),
        IrNode::Str(text[1].into()),
    )
}

/// Set (`on`) or clear the `limits` `bits` of the generator in `generator`.
fn set_limits(function: &mut Function, generator: u32, bits: u32, on: bool) {
    wasm!(function;
        LocalGet(generator), I32WrapI64,
        LocalGet(generator), I32WrapI64, I32Load(m32(u64::from(abi::GEN_LIMITS_OFFSET))),
    );
    if on {
        wasm!(function; I32Const(bits.cast_signed()), I32Or);
    } else {
        wasm!(function; I32Const(!bits.cast_signed()), I32And);
    }
    wasm!(function; I32Store(m32(u64::from(abi::GEN_LIMITS_OFFSET))));
}

/// Push the `limits` `bits` of the generator in `generator` (nonzero when
/// any is set).
fn limits_bits(function: &mut Function, generator: u32, bits: u32) {
    field(function, generator, abi::GEN_LIMITS_OFFSET);
    wasm!(function; I32Const(bits.cast_signed()), I32And);
}

/// Push the queue's front record base (`0` when empty).
fn queue_head(function: &mut Function, generator: u32) {
    wasm!(function; LocalGet(generator), I32WrapI64, I32Load(m32(QUEUE_HEAD)));
}

/// Settle the front request's promise (`op` = `PROMISE_OP_SETTLE_*`) with
/// the value `value` pushes, then drop the request.
fn settle_front(
    function: &mut Function,
    generator: u32,
    op: i32,
    value: impl FnOnce(&mut Function),
    helpers: Helpers<'_>,
) {
    wasm!(function; I32Const(op));
    queue_head(function, generator);
    wasm!(function; I64Load(m64(REQ_PROMISE)));
    value(function);
    wasm!(function; Call(helpers.promise), Drop);
    // head = head.next; an emptied queue clears the tail too.
    wasm!(function;
        LocalGet(generator), I32WrapI64,
        LocalGet(generator), I32WrapI64, I32Load(m32(QUEUE_HEAD)), I64Load(m64(REQ_NEXT)), I32WrapI64,
        I32Store(m32(QUEUE_HEAD)),
        LocalGet(generator), I32WrapI64, I32Load(m32(QUEUE_HEAD)), I32Eqz,
        If(BlockType::Empty),
        LocalGet(generator), I32WrapI64, I32Const(0), I32Store(m32(QUEUE_TAIL)),
        End,
    );
}

/// Fulfill the front request with `{ value, done }` for the value in
/// `value`, then drop it.
#[allow(clippy::too_many_arguments)]
fn fulfill_front(
    function: &mut Function,
    frame: Frame,
    generator: u32,
    value: u32,
    done: bool,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let result = iter_result(value, done);
    settle_front(
        function,
        generator,
        PROMISE_OP_SETTLE_FULFILL,
        |f| emit_node(&result, f, frame.scratch(), strings, helpers),
        helpers,
    );
}

/// Register `reaction`'s fulfilled/rejected pair (`env` = the generator in
/// `generator`) on the value in `value`, `Promise.resolve` semantics.
fn await_value(
    function: &mut Function,
    frame: Frame,
    generator: u32,
    value: u32,
    [fulfilled, rejected]: [AsyncGenReaction; 2],
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(value));
    for reaction in [fulfilled, rejected] {
        let slot = helpers.builtin_slot(Builtin::AsyncGeneratorReaction(reaction));
        emit_settler_value(slot, generator, function, frame.scratch(), helpers);
    }
    wasm!(function; Call(helpers.then), Drop);
}

/// Call builtin `builtin` directly with argument 0 from `generator`,
/// argument 1 from `arg` (or `undefined`) and `env` = `env`, dropping its
/// result. Only emitted when `K >= 2`.
fn call_internal(
    function: &mut Function,
    frame: Frame,
    builtin: Builtin,
    generator: u32,
    arg: Option<u32>,
    env: i64,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(generator));
    if let Some(arg) = arg {
        wasm!(function; LocalGet(arg));
    } else {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    for _ in 2..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; I64Const(env));
    emit_argc(2, function);
    wasm!(function; Call(helpers.builtin(builtin)), Drop);
}

/// `next(v)` / `throw(e)` / `return(v)`: queue the request (a receiver that
/// is not an async generator rejects the returned promise) and serve the
/// queue unless the body is busy.
pub(super) fn emit_async_method(
    request: AsyncGenRequest,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if frame.k < 2 {
        // No async generator exists without a state machine (K >= 2).
        wasm!(function; I64Const(UNDEFINED_BITS));
        return;
    }
    let (this, arg, promise) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (record, tail) = (frame.i32(0), frame.i32(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    // An async generator: a TYPE_GENERATOR handle with GEN_ASYNC.
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function; I32Const(TYPE_GENERATOR), I32Eq, Else, I32Const(0), End);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    limits_bits(function, this, abi::GEN_ASYNC);
    wasm!(function; Else, I32Const(0), End, I32Eqz, If(BlockType::Empty));
    let text = NOT_ASYNC_GENERATOR[usize::try_from(request_code(request)).unwrap_or(0)];
    let rejected = IrNode::PromiseReject(Box::new(type_error(text)));
    emit_node(&rejected, function, frame.scratch(), strings, helpers);
    wasm!(function; Return, End);
    // The request's promise and record, appended at the tail.
    wasm!(function;
        I32Const(abi::PROMISE_OP_NEW), I64Const(UNDEFINED_BITS), I64Const(UNDEFINED_BITS),
        Call(helpers.promise), LocalSet(promise),
        I32Const(REQ_SIZE), Call(helpers.alloc), LocalTee(record),
        I32Const(TYPE_ENV), I32Store8(MemArg { offset: 0, align: 0, memory_index: 0 }),
        LocalGet(record), I32Const(4), I32Store(m32(u64::from(HEADER_SIZE.cast_unsigned()))),
        LocalGet(record), LocalGet(arg), I64Store(m64(REQ_VALUE)),
        LocalGet(record), LocalGet(promise), I64Store(m64(REQ_PROMISE)),
        LocalGet(record), I64Const(request_code(request)), I64Store(m64(REQ_KIND)),
        LocalGet(record), I64Const(0), I64Store(m64(REQ_NEXT)),
        LocalGet(this), I32WrapI64, I32Load(m32(QUEUE_TAIL)), LocalTee(tail), I32Eqz,
        If(BlockType::Empty),
        LocalGet(this), I32WrapI64, LocalGet(record), I32Store(m32(QUEUE_HEAD)),
        Else,
        LocalGet(tail), LocalGet(record), I64ExtendI32U, I64Store(m64(REQ_NEXT)),
        End,
        LocalGet(this), I32WrapI64, LocalGet(record), I32Store(m32(QUEUE_TAIL)),
    );
    call_internal(
        function,
        frame,
        Builtin::AsyncGeneratorDrain,
        this,
        None,
        UNDEFINED_BITS,
        helpers,
    );
    wasm!(function; LocalGet(promise));
}

/// Serve the queue of the async generator in argument 0 from the front
/// until it empties or the body is busy (executing, or awaiting).
#[allow(clippy::too_many_lines)]
pub(super) fn emit_async_drain(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if frame.k < 2 {
        wasm!(function; I64Const(UNDEFINED_BITS));
        return;
    }
    let (generator, value) = (frame.i64(0), frame.i64(1));
    let kind = frame.i32(0);
    let [next, throw, ret] = [
        AsyncGenRequest::Next,
        AsyncGenRequest::Throw,
        AsyncGenRequest::Return,
    ]
    .map(|r| i32::try_from(request_code(r)).unwrap_or(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(generator));
    wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
    limits_bits(function, generator, abi::GEN_BUSY);
    wasm!(function; BrIf(1));
    queue_head(function, generator);
    wasm!(function; I32Eqz, BrIf(1));
    queue_head(function, generator);
    wasm!(function; I64Load(m64(REQ_KIND)), I32WrapI64, LocalSet(kind));
    queue_head(function, generator);
    wasm!(function; I64Load(m64(REQ_VALUE)), LocalSet(value));
    // An abrupt request before the body started completes the generator.
    wasm!(function; LocalGet(kind), I32Const(next), I32Ne);
    not_suspended(function, generator);
    wasm!(function; I32And, If(BlockType::Empty));
    complete(function, generator);
    wasm!(function; End);
    field(function, generator, abi::GEN_DONE_OFFSET);
    wasm!(function; If(BlockType::Empty));
    // Completed: `return(v)` awaits `v`; `throw(e)` rejects; `next` is done.
    wasm!(function; LocalGet(kind), I32Const(ret), I32Eq, If(BlockType::Empty));
    set_limits(function, generator, abi::GEN_BUSY, true);
    await_value(
        function,
        frame,
        generator,
        value,
        [
            AsyncGenReaction::ReturnFulfilled,
            AsyncGenReaction::ReturnRejected,
        ],
        helpers,
    );
    wasm!(function; Br(3), End);
    wasm!(function; LocalGet(kind), I32Const(throw), I32Eq, If(BlockType::Empty));
    settle_front(
        function,
        generator,
        PROMISE_OP_SETTLE_REJECT,
        |f| wasm!(f; LocalGet(value)),
        helpers,
    );
    wasm!(function; Else, I64Const(UNDEFINED_BITS), LocalSet(value));
    fulfill_front(function, frame, generator, value, true, strings, helpers);
    wasm!(function; End, Br(1), End);
    // Suspended at a `yield`: `return(v)` completes (unless a `finally`
    // must run) and awaits `v`; `next` / `throw` resume the body.
    wasm!(function; LocalGet(kind), I32Const(ret), I32Eq, If(BlockType::Empty));
    limits_bits(function, generator, abi::GEN_LIMIT_RETURN);
    wasm!(function; If(BlockType::Empty));
    let error = type_error(ASYNC_RETURN_UNSUPPORTED);
    settle_front(
        function,
        generator,
        PROMISE_OP_SETTLE_REJECT,
        |f| emit_node(&error, f, frame.scratch(), strings, helpers),
        helpers,
    );
    wasm!(function; Br(2), End);
    complete(function, generator);
    set_limits(function, generator, abi::GEN_BUSY, true);
    await_value(
        function,
        frame,
        generator,
        value,
        [
            AsyncGenReaction::ReturnFulfilled,
            AsyncGenReaction::ReturnRejected,
        ],
        helpers,
    );
    wasm!(function; Br(2), End);
    let mode = |throws: bool| if throws { TRUE_BITS } else { FALSE_BITS };
    wasm!(function; LocalGet(kind), I32Const(throw), I32Eq, If(BlockType::Empty));
    call_internal(
        function,
        frame,
        Builtin::AsyncGeneratorStep,
        generator,
        Some(value),
        mode(true),
        helpers,
    );
    wasm!(function; Else);
    call_internal(
        function,
        frame,
        Builtin::AsyncGeneratorStep,
        generator,
        Some(value),
        mode(false),
        helpers,
    );
    wasm!(function; End, Br(0), End, End, I64Const(UNDEFINED_BITS));
}

/// Resume the async generator in argument 0 with argument 1 (thrown at the
/// suspend when `env` is `true`): an `await` registers the reactions that
/// resume it (the body stays busy); a `yield`, completion or throw settles
/// the front request.
pub(super) fn emit_async_step(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if frame.k < 2 {
        wasm!(function; I64Const(UNDEFINED_BITS));
        return;
    }
    let (generator, sent, result) = (frame.i64(0), frame.i64(1), frame.i64(2));
    frame.arg(function, 0);
    wasm!(function; LocalSet(generator));
    frame.arg(function, 1);
    wasm!(function; LocalSet(sent));
    set_limits(function, generator, abi::GEN_BUSY | abi::GEN_RUNNING, true);
    set_limits(function, generator, abi::GEN_AWAIT, false);
    wasm!(function;
        LocalGet(frame.k), I64Const(TRUE_BITS), I64Eq,
        If(BlockType::Empty),
        LocalGet(sent), GlobalSet(EXC_VALUE_GLOBAL), I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I64Const(UNDEFINED_BITS), LocalSet(sent),
        End,
    );
    // SM(sent, gen, …, env = gen.env, argc = 2).
    wasm!(function; LocalGet(sent), LocalGet(generator));
    for _ in 2..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(generator), I32WrapI64, I64Load(m64(u64::from(abi::GEN_ENV_OFFSET))));
    emit_argc(2, function);
    field(function, generator, abi::GEN_SM_IDX_OFFSET);
    wasm!(function;
        CallIndirect { type_index: helpers.fn_type, table_index: 0 },
        LocalSet(result),
    );
    set_limits(function, generator, abi::GEN_RUNNING, false);
    // A throw out of the body rejects the front request.
    wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    wasm!(function;
        GlobalGet(EXC_VALUE_GLOBAL), LocalSet(result),
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL), I64Const(0), GlobalSet(EXC_VALUE_GLOBAL),
    );
    set_limits(function, generator, abi::GEN_BUSY, false);
    settle_front(
        function,
        generator,
        PROMISE_OP_SETTLE_REJECT,
        |f| wasm!(f; LocalGet(result)),
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    field(function, generator, abi::GEN_DONE_OFFSET);
    wasm!(function; If(BlockType::Empty));
    set_limits(function, generator, abi::GEN_BUSY, false);
    fulfill_front(function, frame, generator, result, true, strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    limits_bits(function, generator, abi::GEN_AWAIT);
    wasm!(function; If(BlockType::Empty));
    await_value(
        function,
        frame,
        generator,
        result,
        [
            AsyncGenReaction::AwaitFulfilled,
            AsyncGenReaction::AwaitRejected,
        ],
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    set_limits(function, generator, abi::GEN_BUSY, false);
    fulfill_front(function, frame, generator, result, false, strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// A promise reaction of the async generator in `env`: resume the body after
/// an `await`, or complete a `return(v)` request, then serve the queue.
pub(super) fn emit_async_reaction(
    reaction: AsyncGenReaction,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if frame.k < 2 {
        wasm!(function; I64Const(UNDEFINED_BITS));
        return;
    }
    let (generator, value) = (frame.i64(0), frame.i64(1));
    wasm!(function; LocalGet(frame.k), LocalSet(generator));
    frame.arg(function, 0);
    wasm!(function; LocalSet(value));
    match reaction {
        AsyncGenReaction::AwaitFulfilled | AsyncGenReaction::AwaitRejected => {
            let throws = reaction == AsyncGenReaction::AwaitRejected;
            call_internal(
                function,
                frame,
                Builtin::AsyncGeneratorStep,
                generator,
                Some(value),
                if throws { TRUE_BITS } else { FALSE_BITS },
                helpers,
            );
        }
        AsyncGenReaction::ReturnFulfilled => {
            set_limits(function, generator, abi::GEN_BUSY, false);
            fulfill_front(function, frame, generator, value, true, strings, helpers);
        }
        AsyncGenReaction::ReturnRejected => {
            set_limits(function, generator, abi::GEN_BUSY, false);
            settle_front(
                function,
                generator,
                PROMISE_OP_SETTLE_REJECT,
                |f| wasm!(f; LocalGet(value)),
                helpers,
            );
        }
    }
    call_internal(
        function,
        frame,
        Builtin::AsyncGeneratorDrain,
        generator,
        None,
        UNDEFINED_BITS,
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS));
}
