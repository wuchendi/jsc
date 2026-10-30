//! `%Promise.prototype%` methods over the host promise engine.

use super::array::{CallArg, push_call};
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::{CombinatorOp, PromiseStaticOp, SpeciesOp};

/// The `TypeError` of `then` / `finally` on a non-promise receiver.
const NOT_PROMISE: [CalleeErrorText<'static>; 2] = [
    [
        "Method Promise.prototype.then called on incompatible receiver",
        "TypeError: Method Promise.prototype.then called on incompatible receiver",
    ],
    [
        "Method Promise.prototype.finally called on incompatible receiver",
        "TypeError: Method Promise.prototype.finally called on incompatible receiver",
    ],
];

/// The `TypeError` of `SpeciesConstructor` over a non-object `constructor`.
const CTOR_NOT_OBJECT: CalleeErrorText<'static> = [
    "object.constructor is not an object",
    "TypeError: object.constructor is not an object",
];

/// The `TypeError` of `SpeciesConstructor` over a non-constructor
/// `@@species`.
const SPECIES_NOT_CTOR: CalleeErrorText<'static> = [
    "object.constructor[Symbol.species] is not a constructor",
    "TypeError: object.constructor[Symbol.species] is not a constructor",
];

/// The `TypeError` of `NewPromiseCapability` when the constructor did not
/// pass callable resolving functions to the executor.
const NOT_CALLABLE_SETTLERS: CalleeErrorText<'static> = [
    "Promise resolve or reject function is not callable",
    "TypeError: Promise resolve or reject function is not callable",
];

/// The `TypeError` of a `GetCapabilitiesExecutor` called twice with
/// resolving functions.
const EXECUTOR_REUSED: CalleeErrorText<'static> = [
    "Promise executor has already been invoked with non-undefined arguments",
    "TypeError: Promise executor has already been invoked with non-undefined arguments",
];

/// The `TypeError` a promise resolved with itself is rejected with (Promise
/// Resolve Functions step 7), built by [`emit_promise_self_error`].
const SELF_RESOLUTION: CalleeErrorText<'static> = [
    "Chaining cycle detected for promise #<Promise>",
    "TypeError: Chaining cycle detected for promise #<Promise>",
];

/// The `TypeError` of a `Promise` static (`resolve`, `reject`, the
/// combinators) whose `this` is not a constructor (`NewPromiseCapability`).
const NOT_CONSTRUCTOR: CalleeErrorText<'static> = [
    "Promise receiver is not a constructor",
    "TypeError: Promise receiver is not a constructor",
];

/// The `TypeError` of a combinator whose `C.resolve` is not callable
/// (`GetPromiseResolve`).
const RESOLVE_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Promise resolve is not a function",
    "TypeError: Promise resolve is not a function",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    NOT_PROMISE
        .into_iter()
        .flatten()
        .chain(CTOR_NOT_OBJECT)
        .chain(SPECIES_NOT_CTOR)
        .chain(NOT_CALLABLE_SETTLERS)
        .chain(EXECUTOR_REUSED)
        .chain(SELF_RESOLUTION)
        .chain(NOT_CONSTRUCTOR)
        .chain(RESOLVE_NOT_CALLABLE)
        .chain(["then", CONSTRUCTOR, "resolve"])
}

/// `__promise_self_error() -> i64`: a fresh [`SELF_RESOLUTION`] `TypeError`.
/// Exported for the host promise engines, which settle every promise: a
/// promise fulfilled with itself is rejected with this error instead.
pub(in crate::emit) fn emit_promise_self_error(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) -> Function {
    let mut function = Function::new(type_error_helper_locals());
    emit_node(
        &callee_type_error(SELF_RESOLUTION),
        &mut function,
        0,
        strings,
        helpers,
    );
    wasm!(function; End);
    function
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    let local = IrNode::Local(frame.i64(0));
    vec![
        catch_call(frame),
        construct(frame, frame.i64(0), frame.i64(1)),
        invoke_then(frame, frame.i64(0), vec![local]),
        settled(false, frame.i64(0)),
        settled(true, frame.i64(0)),
        aggregate_error(frame.i64(0), frame.i64(1)),
    ]
}

/// The key `SpeciesConstructor` reads.
const CONSTRUCTOR: &str = "constructor";

/// Push `1` when the jsval in `local` is a promise.
fn is_promise(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(i32::from(abi::TYPE_PROMISE)), I32Eq, Else, I32Const(0), End);
}

/// `this.then(undefined, onRejected)` over the frame's locals.
fn catch_call(frame: Frame) -> IrNode {
    IrNode::MethodCallUser {
        obj: Box::new(IrNode::Local(frame.i64(0))),
        key: Box::new(IrNode::Str("then".into())),
        args: vec![IrNode::Undefined, IrNode::Local(frame.i64(1))],
        obj_local: frame.i64(3),
        callee_local: frame.x64(0),
        not_callable: CalleeError::not_a_function("then"),
    }
}

/// `this.then(...args)` (`Invoke`) over the frame's locals.
fn invoke_then(frame: Frame, this: u32, args: Vec<IrNode>) -> IrNode {
    IrNode::MethodCallUser {
        obj: Box::new(IrNode::Local(this)),
        key: Box::new(IrNode::Str("then".into())),
        args,
        obj_local: frame.y64(2),
        callee_local: frame.y64(3),
        not_callable: CalleeError::not_a_function("then"),
    }
}

/// Push `Call(f, undefined, [arg])` (no argument when `None`) for the
/// callable in i64 `f` (a throw left pending); `undef` is an i64 local
/// holding `undefined`.
fn call(
    function: &mut Function,
    frame: Frame,
    f: u32,
    arg: Option<u32>,
    undef: u32,
    helpers: Helpers<'_>,
) {
    let args: Vec<CallArg> = arg.map(CallArg::Val).into_iter().collect();
    push_call(function, frame, f, undef, &args, helpers);
}

/// `Construct(c, [executor])` for the constructor in i64 `c`.
fn construct(frame: Frame, c: u32, executor: u32) -> IrNode {
    IrNode::Construct {
        callee: Box::new(IrNode::Local(c)),
        args: vec![IrNode::Local(executor)],
        spread: None,
        callee_local: frame.y64(2),
        this_local: frame.y64(3),
        reserve: 0,
        not_constructor: CalleeError::not_a_constructor("object.constructor[Symbol.species]"),
        new_target: None,
    }
}

/// Emit `node` in the body's scratch layout into i64 `out`, returning
/// `undefined` when it throws.
fn emit_into(
    function: &mut Function,
    frame: Frame,
    node: &IrNode,
    out: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(node, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(out));
    return_if_pending(function);
}

/// Push element `i` of the array jsval in i64 `array`.
fn element(function: &mut Function, array: u32, i: u64) {
    wasm!(function; LocalGet(array), I32WrapI64);
    emit_array_elems(function);
    wasm!(function; I64Load(elem_slot(i)));
}

/// A fresh array of the jsvals in `items` into i64 `out` (`tmp` a free i32
/// local).
fn record(function: &mut Function, items: &[u32], out: u32, tmp: u32, helpers: Helpers<'_>) {
    let n = i32::try_from(items.len()).unwrap_or(0);
    wasm!(function; I32Const(n), Call(helpers.new_array()), LocalSet(tmp));
    for (i, &item) in (0u64..).zip(items) {
        wasm!(function; LocalGet(tmp));
        emit_array_elems(function);
        wasm!(function; LocalGet(item), I64Store(elem_slot(i)));
    }
    wasm!(function; LocalGet(tmp), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out));
}

/// A fresh closure running `op` over the `env` in i64 `env`, into i64 `out`.
fn closure(
    function: &mut Function,
    frame: Frame,
    op: SpeciesOp,
    env: u32,
    out: u32,
    helpers: Helpers<'_>,
) {
    let slot = helpers.builtin_slot(Builtin::Species(op));
    emit_settler_value(slot, env, function, frame.scratch(), helpers);
    wasm!(function; LocalSet(out));
}

/// `SpeciesConstructor(o, %Promise%)` of the object in i64 `o` into i64
/// `c`, `undefined` standing for `%Promise%`: `C = Get(o, "constructor")`;
/// `undefined` is the default, a non-object throws, else
/// `S = Get(C, @@species)`: `null` / `undefined` is the default, a
/// constructor is the result, anything else throws. `C === %Promise%` with
/// `%Promise%`'s dirty bit clear skips the `@@species` read.
fn species_constructor(
    function: &mut Function,
    frame: Frame,
    o: u32,
    c: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let promise = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Promise), helpers);
    wasm!(function;
        LocalGet(o), I32Const(static_key(strings, CONSTRUCTOR)), Call(helpers.get), LocalSet(c),
    );
    return_if_pending(function);
    let bit = Intrinsic::Ctor(BuiltinCtor::Promise).watch_bit();
    let fast = bit.filter(|_| helpers.dirty_tracking);
    if let Some(bit) = fast {
        wasm!(function;
            LocalGet(c), I64Const(promise), I64Eq,
            GlobalGet(DIRTY_GLOBAL), I32Const(1 << bit), I32And, I32Eqz, I32And,
            If(BlockType::Empty), I64Const(UNDEFINED_BITS), LocalSet(c), Else,
        );
    }
    wasm!(function; LocalGet(c), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    emit_is_object(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, CTOR_NOT_OBJECT, strings, helpers);
    wasm!(function;
        End,
        LocalGet(c), I32Const(realm::symbol_addr(WellKnown::Species, helpers)),
        Call(helpers.symbol_get()), LocalSet(c),
    );
    return_if_pending(function);
    emit_is_nullish(function, c);
    wasm!(function; If(BlockType::Empty), I64Const(UNDEFINED_BITS), LocalSet(c), Else);
    emit_is_constructor(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, SPECIES_NOT_CTOR, strings, helpers);
    wasm!(function;
        End,
        LocalGet(c), I64Const(promise), I64Eq,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), LocalSet(c), End,
        End,
        End,
    );
    if fast.is_some() {
        wasm!(function; End);
    }
}

/// `NewPromiseCapability(C)` for the constructor in i64 `c`: construct `C`
/// with a fresh `GetCapabilitiesExecutor` over a `[resolve, reject]`
/// record, into the i64 locals `promise`, `resolve` and `reject` (both
/// callable, else a `TypeError`). Uses `y64(1)` and `z64(0)`.
fn new_capability(
    function: &mut Function,
    frame: Frame,
    c: u32,
    [promise, resolve, reject]: [u32; 3],
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (rec, executor) = (frame.y64(1), frame.z64(0));
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(resolve));
    record(function, &[resolve, resolve], rec, frame.i32(0), helpers);
    closure(function, frame, SpeciesOp::Executor, rec, executor, helpers);
    let node = construct(frame, c, executor);
    emit_into(function, frame, &node, promise, strings, helpers);
    for (i, out) in [(0, resolve), (1, reject)] {
        element(function, rec, i);
        wasm!(function; LocalSet(out));
        emit_is_function(function, out);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        throw_type_error(function, frame, NOT_CALLABLE_SETTLERS, strings, helpers);
        wasm!(function; End);
    }
}

/// `then(onFulfilled, onRejected)` (`catch = false`) on a promise: the
/// result promise comes from `SpeciesConstructor(this, %Promise%)` — the
/// host `__then` chain for `%Promise%`, else `NewPromiseCapability(C)`
/// settled by two [`SpeciesOp::Reaction`]s `__then` registers — or
/// `catch(onRejected)`: `this.then(undefined, onRejected)` on any receiver.
pub(super) fn emit_then(
    catch: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, a0, a1) = (frame.i64(0), frame.i64(1), frame.i64(2));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    if catch {
        emit_node(
            &catch_call(frame),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        return;
    }
    frame.arg(function, 1);
    wasm!(function; LocalSet(a1));
    is_promise(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_PROMISE[0], strings, helpers);
    wasm!(function; End);
    let c = frame.y64(0);
    species_constructor(function, frame, this, c, strings, helpers);
    wasm!(function;
        LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I64)),
    );
    let node = IrNode::PromiseThen {
        promise: Box::new(IrNode::Local(this)),
        on_fulfilled: Box::new(IrNode::Local(a0)),
        on_rejected: Box::new(IrNode::Local(a1)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; Else);
    let (promise, resolve, reject) = (frame.y64(4), frame.y64(5), frame.x64(1));
    new_capability(
        function,
        frame,
        c,
        [promise, resolve, reject],
        strings,
        helpers,
    );
    let (env, on_fulfilled, on_rejected) = (frame.z64(1), frame.z64(2), frame.z64(3));
    for (handler, reject_side, out) in [(a0, false, on_fulfilled), (a1, true, on_rejected)] {
        record(
            function,
            &[handler, resolve, reject],
            env,
            frame.i32(0),
            helpers,
        );
        let op = SpeciesOp::Reaction {
            reject: reject_side,
        };
        closure(function, frame, op, env, out, helpers);
    }
    wasm!(function;
        LocalGet(this), LocalGet(on_fulfilled), LocalGet(on_rejected), Call(helpers.then), Drop,
        LocalGet(promise),
        End,
    );
}

/// `finally(onFinally)` on any object: `C = SpeciesConstructor(this,
/// %Promise%)`; a promise whose `C` is `%Promise%` runs the host op, any
/// other receiver `this.then(thenFinally, catchFinally)` (both `onFinally`
/// when it is not callable).
pub(super) fn emit_finally(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, a0) = (frame.i64(0), frame.i64(1));
    let c = frame.y64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    emit_is_object(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_PROMISE[1], strings, helpers);
    wasm!(function; End);
    species_constructor(function, frame, this, c, strings, helpers);
    is_promise(function, this);
    wasm!(function;
        LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, I32And,
        If(BlockType::Result(ValType::I64)),
    );
    let node = IrNode::PromiseFinally {
        promise: Box::new(IrNode::Local(this)),
        on_finally: Box::new(IrNode::Local(a0)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; Else);
    let (env, then_finally, catch_finally) = (frame.z64(1), frame.z64(2), frame.z64(3));
    let promise = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Promise), helpers);
    wasm!(function;
        LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty), I64Const(promise), LocalSet(c), End,
        LocalGet(a0), LocalSet(then_finally),
        LocalGet(a0), LocalSet(catch_finally),
    );
    emit_is_function(function, a0);
    wasm!(function; If(BlockType::Empty));
    record(function, &[a0, c], env, frame.i32(0), helpers);
    for (reject, out) in [(false, then_finally), (true, catch_finally)] {
        let op = SpeciesOp::Finally { reject };
        closure(function, frame, op, env, out, helpers);
    }
    wasm!(function; End);
    let node = invoke_then(
        frame,
        this,
        vec![IrNode::Local(then_finally), IrNode::Local(catch_finally)],
    );
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; End);
}

/// Throw [`NOT_CONSTRUCTOR`] unless the jsval in i64 `c` is a constructor.
fn require_constructor(
    function: &mut Function,
    frame: Frame,
    c: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_constructor(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    enter_frame(helpers);
    throw_type_error(function, frame, NOT_CONSTRUCTOR, strings, helpers);
    leave_frame(helpers);
    wasm!(function; End);
}

/// `PromiseResolve(C, v)` for the constructor in i64 `c` and the value in
/// i64 `v`, into i64 `promise`: a promise whose `constructor` is `C` itself,
/// else a fresh capability of `C` (a constructor, else a `TypeError`)
/// resolved with `v`.
fn promise_resolve(
    function: &mut Function,
    frame: Frame,
    c: u32,
    v: u32,
    promise: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (resolve, reject, same, undef) = (frame.y64(5), frame.x64(1), frame.i32(1), frame.i64(3));
    wasm!(function; I32Const(0), LocalSet(same));
    is_promise(function, v);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(v), I32Const(static_key(strings, CONSTRUCTOR)), Call(helpers.get),
        LocalSet(promise),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(promise), LocalGet(c), I64Eq, LocalSet(same),
        End,
        LocalGet(same),
        If(BlockType::Empty), LocalGet(v), LocalSet(promise), Else,
    );
    require_constructor(function, frame, c, strings, helpers);
    new_capability(
        function,
        frame,
        c,
        [promise, resolve, reject],
        strings,
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(undef));
    call(function, frame, resolve, Some(v), undef, helpers);
    wasm!(function; Drop);
    return_if_pending(function);
    wasm!(function; End);
}

/// Push i32 `1` when `c` (i64) is `%Promise%` and, under dirty tracking,
/// none of `intrinsics` has its dirty bit set; without dirty tracking the
/// bits prove nothing, so `0`.
fn is_clean_promise_ctor(
    function: &mut Function,
    c: u32,
    intrinsics: &[Intrinsic],
    helpers: Helpers<'_>,
) {
    if !helpers.dirty_tracking {
        wasm!(function; I32Const(0));
        return;
    }
    let mask = intrinsics
        .iter()
        .filter_map(|intrinsic| intrinsic.watch_bit())
        .fold(0, |mask, bit| mask | (1 << bit));
    let promise = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Promise), helpers);
    wasm!(function;
        LocalGet(c), I64Const(promise), I64Eq,
        GlobalGet(DIRTY_GLOBAL), I32Const(mask), I32And, I32Eqz, I32And,
    );
}

/// Emit the body of the `Promise` static `op`.
pub(super) fn emit_static(
    op: PromiseStaticOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        PromiseStaticOp::Resolve => emit_resolve(false, function, frame, strings, helpers),
        PromiseStaticOp::Reject => emit_resolve(true, function, frame, strings, helpers),
        PromiseStaticOp::Combinator(op) => emit_combinator(op, function, frame, strings, helpers),
    }
}

/// `Promise.resolve(x)` (`reject = false`) / `Promise.reject(r)` with
/// `C = this` (an object, else a `TypeError`): `PromiseResolve(C, x)` /
/// `NewPromiseCapability(C)` rejected with `r`. `C === %Promise%` takes the
/// host op (for `resolve` only a non-promise `x`, whose result does not
/// depend on its `constructor`): `NewPromiseCapability(%Promise%)` reads no
/// property of `%Promise%`, so no dirty bit is involved.
fn emit_resolve(
    reject: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (c, x, out, undef) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(c));
    frame.arg(function, 0);
    wasm!(function; LocalSet(x));
    emit_is_object(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_CONSTRUCTOR, strings, helpers);
    wasm!(function; End);
    let promise = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Promise), helpers);
    wasm!(function; LocalGet(c), I64Const(promise), I64Eq);
    let op = if reject {
        PROMISE_OP_REJECT
    } else {
        is_promise(function, x);
        wasm!(function; I32Eqz, I32And);
        PROMISE_OP_RESOLVE
    };
    wasm!(function;
        If(BlockType::Empty),
        I32Const(op), LocalGet(x), I64Const(UNDEFINED_BITS), Call(helpers.promise), Return,
        End,
    );
    if reject {
        let (resolve, settle) = (frame.y64(5), frame.x64(1));
        require_constructor(function, frame, c, strings, helpers);
        new_capability(function, frame, c, [out, resolve, settle], strings, helpers);
        wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(undef));
        call(function, frame, settle, Some(x), undef, helpers);
        wasm!(function; Drop);
        return_if_pending(function);
    } else {
        promise_resolve(function, frame, c, x, out, strings, helpers);
    }
    wasm!(function; LocalGet(out));
}

/// `{ status: "fulfilled", value: x }` / `{ status: "rejected", reason: x }`
/// (`reject`) of `allSettled` for the jsval in `x`.
fn settled(reject: bool, x: u32) -> IrNode {
    let (status, key) = if reject {
        ("rejected", "reason")
    } else {
        ("fulfilled", "value")
    };
    IrNode::Object {
        entries: vec![
            (PropKey::Static("status".into()), IrNode::Str(status.into())),
            (PropKey::Static(key.into()), IrNode::Local(x)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// The `AggregateError` `Promise.any` rejects with: no `message`, an own
/// non-enumerable `errors` = the array in `errors` (`tmp` an i64 temp).
fn aggregate_error(errors: u32, tmp: u32) -> IrNode {
    let ctor = IrNode::Intrinsic(GlobalIntrinsic::Ctor(BuiltinCtor::Error(
        ErrorKind::AggregateError,
    )));
    let error = IrNode::Object {
        entries: vec![(
            PropKey::Hidden("stack".into()),
            IrNode::Str("AggregateError".into()),
        )],
        proto: Some(Box::new(IrNode::Index {
            obj: Box::new(ctor),
            index: Box::new(IrNode::Str("prototype".into())),
        })),
        class: abi::CLASS_ERROR,
        reserve: 0,
    };
    IrNode::DefineProps {
        obj: Box::new(error),
        local: tmp,
        entries: vec![(PropKey::Hidden("errors".into()), IrNode::Local(errors))],
    }
}

/// The locals of a combinator body ([`emit_combinator`]).
#[derive(Clone, Copy)]
struct Combinator {
    /// The combinator.
    op: CombinatorOp,
    /// `C` (`this`).
    c: u32,
    /// The capability's promise, `resolve` and `reject`.
    capability: [u32; 3],
    /// `promiseResolve` (`C.resolve`, read once).
    promise_resolve: u32,
    /// The shared `[values, remainingElements, settle]` state array.
    state: u32,
    /// The value (then `nextPromise`).
    v: u32,
    /// The element closures' `env` (and a temp).
    env: u32,
    /// The `then` arguments.
    on_fulfilled: u32,
    on_rejected: u32,
    /// i32: the value's index, the `values` array handle, the fast path.
    index: u32,
    values: u32,
    fast: u32,
}

impl Combinator {
    /// The locals of `op`'s body under `frame` (`on_rejected` reuses the
    /// local of the iterable, dead once the record is open).
    fn new(op: CombinatorOp, frame: Frame) -> Self {
        Self {
            op,
            c: frame.i64(0),
            capability: [frame.i64(1), frame.i64(2), frame.i64(3)],
            promise_resolve: frame.x64(1),
            state: frame.y64(0),
            v: frame.y64(4),
            env: frame.y64(5),
            on_fulfilled: frame.y64(1),
            on_rejected: frame.x64(0),
            index: frame.y32(0),
            values: frame.y32(1),
            fast: frame.y32(2),
        }
    }

    /// Whether the combinator collects values (all but `race`).
    fn collects(self) -> bool {
        self.op != CombinatorOp::Race
    }
}

/// `Promise.all` / `allSettled` / `any` / `race` (`op`) over `iterable`
/// with `C = this` (design/symbol-strings.md §5.2, §5.7):
/// `NewPromiseCapability(C)` (a throw propagates), `GetPromiseResolve(C)`
/// (read once), `GetIterator(iterable)`, then per value
/// [`combinator_value`]. An abrupt completion closes the iterator unless it
/// came from the iterator itself, then rejects the capability
/// (`IfAbruptRejectPromise`).
fn emit_combinator(
    op: CombinatorOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let cb = Combinator::new(op, frame);
    let [promise, resolve, reject] = cb.capability;
    let iterable = cb.on_rejected;
    let rec = iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(cb.c));
    frame.arg(function, 0);
    wasm!(function; LocalSet(iterable));
    require_constructor(function, frame, cb.c, strings, helpers);
    new_capability(function, frame, cb.c, cb.capability, strings, helpers);
    iter::guarded(function, helpers, |function| {
        wasm!(function;
            LocalGet(cb.c), I32Const(static_key(strings, "resolve")), Call(helpers.get),
            LocalSet(cb.promise_resolve),
        );
        emit_propagate_if_pending(function, helpers);
        emit_is_function(function, cb.promise_resolve);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        enter_frame(helpers);
        throw_type_error(function, frame, RESOLVE_NOT_CALLABLE, strings, helpers);
        leave_frame(helpers);
        wasm!(function; End);
        let iterable = IrNode::Local(iterable);
        emit_get_iterator(&iterable, &rec, false, function, scratch, strings, helpers);
        if cb.collects() {
            let settle = if op == CombinatorOp::Any {
                reject
            } else {
                resolve
            };
            wasm!(function;
                I32Const(0), Call(helpers.new_array()), LocalTee(cb.values),
                I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(cb.v),
                I64Const(1f64.to_bits().cast_signed()), LocalSet(cb.env),
            );
            record(
                function,
                &[cb.v, cb.env, settle],
                cb.state,
                frame.i32(0),
                helpers,
            );
        }
        wasm!(function;
            I32Const(0), LocalSet(cb.index),
            Block(BlockType::Empty), Loop(BlockType::Empty),
        );
        enter_frame(helpers);
        enter_frame(helpers);
        emit_iterator_step(&rec, function, scratch, strings, helpers);
        wasm!(function; I32Eqz, BrIf(1));
        emit_iterator_value(&rec, function, scratch, strings, helpers);
        wasm!(function; LocalSet(cb.v));
        combinator_value(cb, function, frame, strings, helpers);
        leave_frame(helpers);
        leave_frame(helpers);
        wasm!(function; Br(0), End, End);
        if cb.collects() {
            combinator_done(cb, function, frame, strings, helpers);
        }
    });
    wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    emit_iterator_close(
        &rec,
        CloseMode::Pending,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        GlobalGet(EXC_VALUE_GLOBAL), LocalSet(cb.v),
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
        I64Const(0), GlobalSet(EXC_VALUE_GLOBAL),
        I64Const(UNDEFINED_BITS), LocalSet(cb.env),
    );
    call(function, frame, reject, Some(cb.v), cb.env, helpers);
    wasm!(function; Drop);
    return_if_pending(function);
    wasm!(function; End, LocalGet(promise));
}

/// One value `v` of a combinator: append a `values` slot, `nextPromise =
/// Call(promiseResolve, C, «v»)`, build the element closures over `env =
/// [state, index, alreadyCalled]` (`remainingElements += 1`) and
/// `Invoke(nextPromise, "then", «onF, onR»)`; `index += 1`. `C ===
/// %Promise%` (with its and `%Promise.prototype%`'s dirty bits clear), its
/// builtin `resolve` and a `%Promise%` instance without own properties skip
/// the `resolve` call (`nextPromise` is `v`) and register the reactions
/// through the host `__then` directly.
fn combinator_value(
    cb: Combinator,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let [_, resolve, reject] = cb.capability;
    let (v, on_fulfilled, on_rejected) = (cb.v, cb.on_fulfilled, cb.on_rejected);
    if cb.collects() {
        wasm!(function;
            LocalGet(cb.values), I64Const(UNDEFINED_BITS), Call(helpers.array_push), Drop,
        );
    }
    let clean = [
        Intrinsic::Ctor(BuiltinCtor::Promise),
        Intrinsic::Prototype(BuiltinCtor::Promise),
    ];
    is_clean_promise_ctor(function, cb.c, &clean, helpers);
    let builtin = Builtin::PromiseStatic(PromiseStaticOp::Resolve);
    let resolve_bits = realm::method_bits(builtin, helpers);
    let proto = intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Promise), helpers);
    wasm!(function; LocalGet(cb.promise_resolve), I64Const(resolve_bits), I64Eq, I32And);
    is_promise(function, v);
    wasm!(function;
        I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), I32WrapI64,
        I32Load(MemArg { offset: u64::from(abi::OBJ_PROPS_OFFSET), align: 2, memory_index: 0 }),
        I32Eqz,
        LocalGet(v), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        I64Const(proto), I64Eq, I32And,
        Else, I32Const(0), End,
        LocalTee(cb.fast), I32Eqz, If(BlockType::Empty),
    );
    enter_frame(helpers);
    let args = [CallArg::Val(v)];
    push_call(function, frame, cb.promise_resolve, cb.c, &args, helpers);
    wasm!(function; LocalSet(v));
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    wasm!(function; End);
    let (element, settle_f, settle_r) = match cb.op {
        CombinatorOp::All => (Some((true, false)), None, Some(reject)),
        CombinatorOp::AllSettled => (Some((true, true)), None, None),
        CombinatorOp::Any => (Some((false, true)), Some(resolve), None),
        CombinatorOp::Race => (None, Some(resolve), Some(reject)),
    };
    if let Some((fulfill, reject_side)) = element {
        wasm!(function;
            LocalGet(cb.index), F64ConvertI32U, I64ReinterpretF64, LocalSet(on_fulfilled),
            I64Const(FALSE_BITS), LocalSet(on_rejected),
        );
        let env = [cb.state, on_fulfilled, on_rejected];
        record(function, &env, cb.env, frame.i32(0), helpers);
        for (wanted, reject, out) in [
            (fulfill, false, on_fulfilled),
            (reject_side, true, on_rejected),
        ] {
            if wanted {
                let combinator = cb.op;
                let element = SpeciesOp::Element { combinator, reject };
                closure(function, frame, element, cb.env, out, helpers);
            }
        }
        adjust_remaining(function, cb.state, 1.0);
        wasm!(function; Drop);
    }
    for (settle, out) in [(settle_f, on_fulfilled), (settle_r, on_rejected)] {
        if let Some(settle) = settle {
            wasm!(function; LocalGet(settle), LocalSet(out));
        }
    }
    wasm!(function;
        LocalGet(cb.fast), If(BlockType::Empty),
        LocalGet(v), LocalGet(on_fulfilled), LocalGet(on_rejected), Call(helpers.then), Drop,
        Else,
    );
    enter_frame(helpers);
    let handlers = vec![IrNode::Local(on_fulfilled), IrNode::Local(on_rejected)];
    emit_drop(
        function,
        frame,
        &invoke_then(frame, v, handlers),
        strings,
        helpers,
    );
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    wasm!(function; End, LocalGet(cb.index), I32Const(1), I32Add, LocalSet(cb.index));
}

/// The iterator of a collecting combinator is done: `remainingElements -=
/// 1`; at zero `all` / `allSettled` resolve with the values, `any` rejects
/// with an `AggregateError` over them (thrown: the record is done, so the
/// abrupt path closes nothing).
fn combinator_done(
    cb: Combinator,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    adjust_remaining(function, cb.state, -1.0);
    wasm!(function; I64Eqz, If(BlockType::Empty));
    enter_frame(helpers);
    wasm!(function;
        LocalGet(cb.values), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(cb.v),
    );
    if cb.op == CombinatorOp::Any {
        let error = aggregate_error(cb.v, cb.on_fulfilled);
        emit_throw_error(&error, function, frame.scratch(), strings, helpers);
    } else {
        wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(cb.env));
        call(
            function,
            frame,
            cb.capability[1],
            Some(cb.v),
            cb.env,
            helpers,
        );
        wasm!(function; Drop);
        emit_propagate_if_pending(function, helpers);
    }
    leave_frame(helpers);
    wasm!(function; End);
}

/// `state[1] += delta` (`remainingElements`) for the combinator state array
/// in i64 `state`, pushing the new count's bits (`0` once it is `+0`).
fn adjust_remaining(function: &mut Function, state: u32, delta: f64) {
    wasm!(function; LocalGet(state), I32WrapI64);
    emit_array_elems(function);
    element(function, state, 1);
    wasm!(function;
        F64ReinterpretI64, F64Const(Ieee64::from(delta)), F64Add, I64ReinterpretF64,
        I64Store(elem_slot(1)),
    );
    element(function, state, 1);
}

/// The body of a [`SpeciesOp`] builtin: a `get [Symbol.species]` returns
/// `this`; an internal closure reads its captured values from `env`.
pub(super) fn emit_species(
    op: SpeciesOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let env = frame.k;
    let (a0, a1, v, undef) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    if let SpeciesOp::Getter(_) = op {
        wasm!(function; GlobalGet(THIS_GLOBAL));
    } else {
        wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(undef));
        frame.arg(function, 0);
        wasm!(function; LocalSet(a0));
    }
    match op {
        SpeciesOp::Getter(_) => {}
        // `GetCapabilitiesExecutor(resolve, reject)`.
        SpeciesOp::Executor => {
            frame.arg(function, 1);
            wasm!(function; LocalSet(a1));
            element(function, env, 0);
            wasm!(function; I64Const(UNDEFINED_BITS), I64Ne);
            element(function, env, 1);
            wasm!(function; I64Const(UNDEFINED_BITS), I64Ne, I32Or, If(BlockType::Empty));
            throw_type_error(function, frame, EXECUTOR_REUSED, strings, helpers);
            wasm!(function; End);
            for (i, arg) in [(0, a0), (1, a1)] {
                wasm!(function; LocalGet(env), I32WrapI64);
                emit_array_elems(function);
                wasm!(function; LocalGet(arg), I64Store(elem_slot(i)));
            }
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
        // The reaction job of a species `then`: the handler's result (or
        // the settlement itself without a handler) resolves the capability,
        // a throw from the handler rejects it.
        SpeciesOp::Reaction { reject } => {
            let target = frame.x64(0);
            element(function, env, 0);
            wasm!(function; LocalSet(v));
            emit_is_function(function, v);
            wasm!(function; If(BlockType::Empty));
            call(function, frame, v, Some(a0), undef, helpers);
            wasm!(function;
                LocalSet(a0),
                GlobalGet(EXC_PENDING_GLOBAL),
                If(BlockType::Empty),
                GlobalGet(EXC_VALUE_GLOBAL), LocalSet(a0),
                I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
                I64Const(0), GlobalSet(EXC_VALUE_GLOBAL),
            );
            element(function, env, 2);
            wasm!(function; LocalSet(target), Else);
            element(function, env, 1);
            wasm!(function; LocalSet(target), End, Else);
            element(function, env, if reject { 2 } else { 1 });
            wasm!(function; LocalSet(target), End);
            call(function, frame, target, Some(a0), undef, helpers);
            wasm!(function; Drop, I64Const(UNDEFINED_BITS));
        }
        // `thenFinally(value)` / `catchFinally(reason)`: call `onFinally`,
        // `PromiseResolve(C, result)`, then chain a thunk returning `value`
        // (throwing `reason`).
        SpeciesOp::Finally { reject } => {
            let (on_finally, c) = (frame.x64(0), frame.y64(0));
            let promise = frame.y64(4);
            element(function, env, 0);
            wasm!(function; LocalSet(on_finally));
            element(function, env, 1);
            wasm!(function; LocalSet(c));
            call(function, frame, on_finally, None, undef, helpers);
            wasm!(function; LocalSet(v));
            return_if_pending(function);
            promise_resolve(function, frame, c, v, promise, strings, helpers);
            let thunk = frame.z64(1);
            let op = SpeciesOp::Thunk { throw: reject };
            closure(function, frame, op, a0, thunk, helpers);
            let node = invoke_then(frame, promise, vec![IrNode::Local(thunk)]);
            emit_node(&node, function, frame.scratch(), strings, helpers);
        }
        SpeciesOp::Element { combinator, reject } => {
            emit_element(combinator, reject, function, frame, strings, helpers);
        }
        SpeciesOp::Thunk { throw } => {
            if throw {
                wasm!(function;
                    LocalGet(env), GlobalSet(EXC_VALUE_GLOBAL),
                    I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
                    I64Const(UNDEFINED_BITS),
                );
            } else {
                wasm!(function; LocalGet(env));
            }
        }
    }
}

/// A resolve / reject element function of `combinator` (`reject`: the
/// rejection one) called with the settlement in `a0`: once per value
/// (`alreadyCalled`), store it (as an `allSettled` result object) at the
/// value's index, then `remainingElements -= 1`; at zero settle the
/// combinator's capability with the values (`any`: reject with an
/// `AggregateError` over them), returning that call's result.
fn emit_element(
    combinator: CombinatorOp,
    reject: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let env = frame.k;
    let (a0, v, undef) = (frame.i64(0), frame.i64(2), frame.i64(3));
    let (state, values, settle, tmp) = (frame.x64(0), frame.y64(0), frame.y64(1), frame.y64(2));
    element(function, env, 2);
    wasm!(function; I64Const(TRUE_BITS), I64Eq, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End);
    wasm!(function; LocalGet(env), I32WrapI64);
    emit_array_elems(function);
    wasm!(function; I64Const(TRUE_BITS), I64Store(elem_slot(2)));
    element(function, env, 0);
    wasm!(function; LocalSet(state));
    element(function, state, 0);
    wasm!(function; LocalSet(values));
    if combinator == CombinatorOp::AllSettled {
        emit_node(
            &settled(reject, a0),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; LocalSet(v));
    } else {
        wasm!(function; LocalGet(a0), LocalSet(v));
    }
    wasm!(function; LocalGet(values), I32WrapI64);
    emit_array_elems(function);
    element(function, env, 1);
    wasm!(function;
        F64ReinterpretI64, I32TruncSatF64U, I32Const(3), I32Shl, I32Add,
        LocalGet(v), I64Store(elem_slot(0)),
    );
    adjust_remaining(function, state, -1.0);
    wasm!(function; I64Eqz, If(BlockType::Result(ValType::I64)));
    element(function, state, 2);
    wasm!(function; LocalSet(settle));
    if combinator == CombinatorOp::Any {
        emit_node(
            &aggregate_error(values, tmp),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; LocalSet(v));
    } else {
        wasm!(function; LocalGet(values), LocalSet(v));
    }
    call(function, frame, settle, Some(v), undef, helpers);
    wasm!(function; Else, I64Const(UNDEFINED_BITS), End);
}
