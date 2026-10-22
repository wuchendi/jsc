//! `%Function.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `call` on a non-callable receiver.
const CALL_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Function.prototype.call called on a value that is not a function",
    "TypeError: Function.prototype.call called on a value that is not a function",
];

/// The `TypeError` of `apply` on a non-callable receiver.
const APPLY_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Function.prototype.apply called on a value that is not a function",
    "TypeError: Function.prototype.apply called on a value that is not a function",
];

/// The `TypeError` of `CreateListFromArrayLike` on a non-object.
const NOT_ARRAY_LIKE: CalleeErrorText<'static> = [
    "CreateListFromArrayLike called on non-object",
    "TypeError: CreateListFromArrayLike called on non-object",
];

/// The `TypeError` `%ThrowTypeError%` throws.
const RESTRICTED_PROPERTY: CalleeErrorText<'static> = [
    "'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them",
    "TypeError: 'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 8] = [
    CALL_NOT_CALLABLE[0],
    CALL_NOT_CALLABLE[1],
    APPLY_NOT_CALLABLE[0],
    APPLY_NOT_CALLABLE[1],
    NOT_ARRAY_LIKE[0],
    NOT_ARRAY_LIKE[1],
    RESTRICTED_PROPERTY[0],
    RESTRICTED_PROPERTY[1],
];

/// `%ThrowTypeError%`: throw the restricted-property `TypeError`.
pub(super) fn emit_throw_type_error(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    throw_type_error(function, frame, RESTRICTED_PROPERTY, strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// Read `this` into `target` and throw `text` unless it is callable.
fn this_callable(
    function: &mut Function,
    frame: Frame,
    target: u32,
    text: CalleeErrorText<'_>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(target));
    emit_is_function(function, target);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Call the function in i64 local `target` with `this` = i64 local `this`
/// and the elements of the fresh dense array (i32 base `args`, i32 length
/// `len`) as its arguments, exactly like a spread call: the first `K` fill the
/// slots and the array is published for a callee gathering the rest.
fn call_with_array(
    function: &mut Function,
    target: u32,
    this: u32,
    args: u32,
    len: u32,
    helpers: Helpers<'_>,
) {
    emit_spread_arg_slots(args, len, function, helpers);
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    invoke(
        function,
        target,
        |function| {
            emit_publish_spread_args(args, function);
            emit_spread_argc(len, function);
        },
        helpers,
    );
}

/// `Function.prototype.call(thisArg, ...args)`: call `this` with `thisArg`
/// and the remaining arguments. With at most `K` arguments they are shifted
/// down one slot in place; more can only arrive through a spread call, whose
/// published argument array is copied without its first element.
pub(super) fn emit_call(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (target, this) = (frame.i64(0), frame.i64(1));
    let (n, src, dst, i) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let k = frame.k;
    this_callable(function, frame, target, CALL_NOT_CALLABLE, strings, helpers);
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U, LocalTee(n),
        I32Const(k.cast_signed()), I32LeU,
        If(BlockType::Result(ValType::I64)),
    );
    for slot in 1..=k {
        frame.arg(function, slot);
    }
    frame.arg(function, 0);
    wasm!(function; GlobalSet(THIS_GLOBAL));
    invoke(
        function,
        target,
        |function| {
            // argc = max(n - 1, 0).
            wasm!(function;
                LocalGet(n), I32Const(1), I32Sub, I32Const(0), LocalGet(n), Select,
                F64ConvertI32U, I64ReinterpretF64,
            );
        },
        helpers,
    );
    wasm!(function;
        Else,
        GlobalGet(SPREAD_ARGS_GLOBAL), I32WrapI64, LocalTee(src),
    );
    emit_array_elems(function);
    wasm!(function;
        I64Load(elem_slot(0)), LocalSet(this),
        LocalGet(n), I32Const(1), I32Sub, LocalTee(n), Call(helpers.new_array()), LocalSet(dst),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(dst),
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(i), I32Const(3), I32Shl, I32Add, LocalGet(src));
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(1)),
        I64Store(elem_slot(0)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
    call_with_array(function, target, this, dst, n, helpers);
    wasm!(function; End);
}

/// `Function.prototype.apply(thisArg, argList)`: call `this` with `thisArg`
/// and `CreateListFromArrayLike(argList)` (no arguments for `undefined` /
/// `null`): an array's elements (holes read `undefined`), else
/// `ToLength(argList.length)` indexed reads; a non-object throws.
pub(super) fn emit_apply(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (target, this, list, v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (n, src, dst, i) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    this_callable(
        function,
        frame,
        target,
        APPLY_NOT_CALLABLE,
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function; LocalSet(this));
    frame.arg(function, 1);
    wasm!(function; LocalSet(list));
    emit_is_nullish(function, list);
    wasm!(function;
        If(BlockType::Empty),
        I32Const(0), Call(helpers.new_array()), LocalSet(dst),
        Else,
    );
    emit_is_object(function, list);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_ARRAY_LIKE, strings, helpers);
    wasm!(function;
        End,
        LocalGet(list), I32WrapI64, LocalTee(src), I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }),
        I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        I32Const(0), Call(helpers.new_array()), LocalGet(src), Call(helpers.array_concat), LocalSet(dst),
        Else,
        LocalGet(list), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get),
    );
    return_if_pending(function);
    wasm!(function;
        Call(helpers.to_number), F64ReinterpretI64, I32TruncSatF64U, LocalTee(n),
        Call(helpers.new_array()), LocalSet(dst),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(list),
        LocalGet(i), F64ConvertI32U, I64ReinterpretF64, I32Const(0), Call(helpers.to_key),
        Call(helpers.get), LocalSet(v),
    );
    return_if_pending(function);
    wasm!(function; LocalGet(dst));
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, LocalGet(v), I64Store(elem_slot(0)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
        End,
        LocalGet(dst), I32Load(MemArg { offset: ARRAY_LEN_OFFSET, align: 2, memory_index: 0 }), LocalSet(n),
    );
    call_with_array(function, target, this, dst, n, helpers);
}

/// The out-of-line generic method call of the guarded builtin call sites
/// (DESIGN.md §6.4): a `null`/`undefined` receiver (`this`) throws the member-read
/// `TypeError`; else `__get` the method (low half of `env`, an interned key)
/// from it, `IsCallable` (else the not-a-function `TypeError` whose message is
/// the high half of `env`), then call it with the same argument slots and
/// count and `this` = the receiver.
pub(super) fn emit_method_call(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (recv, method, msg, stack) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let env = frame.k;
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(recv));
    emit_is_nullish(function, recv);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(recv),
        LocalGet(env), I64Const(0xffff_ffff), I64And, I64Const(HEAP_BITS), I64Or,
        I32Const(0), Call(helpers.nullish_throw),
        I64Const(UNDEFINED_BITS), Return,
        End,
        LocalGet(recv), LocalGet(env), I32WrapI64, Call(helpers.get), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_function(function, method);
    wasm!(function;
        I32Eqz, If(BlockType::Empty),
        LocalGet(env), I64Const(32), I64ShrU, I64Const(HEAP_BITS), I64Or, LocalTee(msg),
        I32WrapI64, LocalSet(frame.i32(0)),
        I32Const(static_key(strings, TYPE_ERROR_STACK_PREFIX)), LocalGet(frame.i32(0)),
        Call(helpers.concat), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(stack),
    );
    let error = IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Local(msg)),
        IrNode::Local(stack),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
    wasm!(function; End);
    for slot in 0..frame.k {
        wasm!(function; LocalGet(slot));
    }
    wasm!(function; LocalGet(recv), GlobalSet(THIS_GLOBAL));
    invoke(
        function,
        method,
        |function| wasm!(function; LocalGet(frame.argc())),
        helpers,
    );
}
