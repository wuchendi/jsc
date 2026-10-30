//! `%Promise.prototype%` methods over the host promise engine.

use super::array::{CallArg, push_call};
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::SpeciesOp;

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
        .chain(["then", CONSTRUCTOR])
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

/// `PromiseResolve(C, v)` for the constructor in i64 `c` and the value in
/// i64 `v`, into i64 `promise`: a promise whose `constructor` is `C` itself,
/// else a fresh capability of `C` resolved with `v`.
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
