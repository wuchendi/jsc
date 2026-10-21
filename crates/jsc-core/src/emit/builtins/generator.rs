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
            (PropKey::Static("value".to_owned()), IrNode::Local(value)),
            (PropKey::Static("done".to_owned()), IrNode::Bool(true)),
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
