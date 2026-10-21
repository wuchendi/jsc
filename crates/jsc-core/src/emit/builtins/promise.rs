//! `%Promise.prototype%` methods over the host promise engine.

#[allow(clippy::wildcard_imports)]
use super::*;

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

/// The `TypeError` of `finally` on an object that is not a promise: the
/// spec's `this.then(thenFinally, catchFinally)` path is not supported.
const FINALLY_THENABLE: CalleeErrorText<'static> = [
    "Promise.prototype.finally on a non-promise object is not supported",
    "TypeError: Promise.prototype.finally on a non-promise object is not supported",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    NOT_PROMISE
        .into_iter()
        .flatten()
        .chain(FINALLY_THENABLE)
        .chain(["then"])
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    vec![catch_call(Frame { k })]
}

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
        key: Box::new(IrNode::Str("then".to_owned())),
        args: vec![IrNode::Undefined, IrNode::Local(frame.i64(1))],
        obj_local: frame.i64(3),
        not_callable: CalleeError::not_a_function("then"),
    }
}

/// `then(onFulfilled, onRejected)` (`catch = false`) on a promise, or
/// `catch(onRejected)`: `this.then(undefined, onRejected)` on any receiver.
pub(super) fn emit_then(
    catch: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
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
    let node = IrNode::PromiseThen {
        promise: Box::new(IrNode::Local(this)),
        on_fulfilled: Box::new(IrNode::Local(a0)),
        on_rejected: Box::new(IrNode::Local(a1)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}

/// `finally(onFinally)` on a promise.
pub(super) fn emit_finally(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, a0) = (frame.i64(0), frame.i64(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    is_promise(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_object(function, this);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, FINALLY_THENABLE, strings, helpers);
    wasm!(function; End);
    throw_type_error(function, frame, NOT_PROMISE[1], strings, helpers);
    wasm!(function; End);
    let node = IrNode::PromiseFinally {
        promise: Box::new(IrNode::Local(this)),
        on_finally: Box::new(IrNode::Local(a0)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}
