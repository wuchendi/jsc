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

/// The `TypeError` of `bind` on a non-callable receiver.
const BIND_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Bind must be called on a function",
    "TypeError: Bind must be called on a function",
];

/// The `TypeError` of `Function.prototype.toString` on a non-callable receiver.
const TO_STRING_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Function.prototype.toString requires that 'this' be a Function",
    "TypeError: Function.prototype.toString requires that 'this' be a Function",
];

/// The `name` prefix of a bound function.
const BOUND_PREFIX: &str = "bound ";

/// The two halves of the `NativeFunction` text `toString` builds around a name.
const NATIVE_HEAD: &str = "function ";
const NATIVE_TAIL: &str = "() { [native code] }";

/// The `TypeError` `%ThrowTypeError%` throws.
const RESTRICTED_PROPERTY: CalleeErrorText<'static> = [
    "'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them",
    "TypeError: 'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 15] = [
    BIND_NOT_CALLABLE[0],
    BIND_NOT_CALLABLE[1],
    TO_STRING_NOT_CALLABLE[0],
    TO_STRING_NOT_CALLABLE[1],
    BOUND_PREFIX,
    NATIVE_HEAD,
    NATIVE_TAIL,
    CALL_NOT_CALLABLE[0],
    CALL_NOT_CALLABLE[1],
    APPLY_NOT_CALLABLE[0],
    APPLY_NOT_CALLABLE[1],
    NOT_ARRAY_LIKE[0],
    NOT_ARRAY_LIKE[1],
    RESTRICTED_PROPERTY[0],
    RESTRICTED_PROPERTY[1],
];

/// The IR fragments of this family's bodies, for the static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    vec![bound_construct(Frame { k })]
}

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

/// Store the incoming arguments `from..argc` (the `K` slots, or past them
/// the published spread argument array) into the fresh dense array (i32 base
/// `dst`) at element `i + shift` (i32 local `shift`). Uses i32 `i` and i64
/// `tmp`; `n` holds the decoded argument count.
#[allow(clippy::too_many_arguments)]
fn store_args(
    function: &mut Function,
    frame: Frame,
    dst: u32,
    shift: u32,
    from: u32,
    n: u32,
    i: u32,
    tmp: u32,
) {
    let k = frame.k;
    wasm!(function;
        LocalGet(n), I32Const(k.cast_signed()), I32LeU,
        If(BlockType::Empty),
    );
    for slot in from..k {
        wasm!(function;
            LocalGet(n), I32Const(slot.cast_signed()), I32GtU,
            If(BlockType::Empty),
            LocalGet(dst),
        );
        emit_array_elems(function);
        wasm!(function;
            I32Const(slot.cast_signed()), LocalGet(shift), I32Add, I32Const(3), I32Shl, I32Add,
            LocalGet(slot), I64Store(elem_slot(0)),
            End,
        );
    }
    wasm!(function;
        Else,
        I32Const(from.cast_signed()), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        GlobalGet(SPREAD_ARGS_GLOBAL), I32WrapI64,
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), LocalSet(tmp),
        LocalGet(dst),
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(i), LocalGet(shift), I32Add, I32Const(3), I32Shl, I32Add);
    emit_elem_read(function, tmp);
    wasm!(function;
        I64Store(elem_slot(0)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
    );
}

/// `Function.prototype.bind(thisArg, ...args)` (`BoundFunctionCreate`): a
/// fresh non-`LAZY` `BOUND` function block whose `func_idx` is the
/// [`Builtin::BoundCall`] trampoline, whose `env` is the array `[target,
/// thisArg, ...args]`, whose `proto` is the target's, and which is a
/// constructor (`SELF_ALLOC`, the trampoline constructs) iff the target is.
/// Then its own `length` (`max(0, ToIntegerOrInfinity(target.length) -
/// args.length)` when the target has an own number `length`, else `0`) and
/// `name` (`"bound " + target.name`, `""` for a non-string), both
/// configurable only.
pub(super) fn emit_bind(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (target, v, tmp) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (n, rec, i, f) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (shift, nb) = (frame.x32(0), frame.x32(1));
    let len = frame.f64(0);
    this_callable(function, frame, target, BIND_NOT_CALLABLE, strings, helpers);
    // nb = max(n - 1, 0); rec = [target, thisArg, ...args].
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U, LocalTee(n),
        I32Const(1), I32Sub, I32Const(0), LocalGet(n), Select, LocalTee(nb),
        I32Const(2), I32Add, Call(helpers.new_array()), LocalTee(rec),
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(target), I64Store(elem_slot(0)), LocalGet(rec));
    emit_array_elems(function);
    frame.arg(function, 0);
    wasm!(function; I64Store(elem_slot(1)), I32Const(1), LocalSet(shift));
    store_args(function, frame, rec, shift, 1, n, i, tmp);
    let ctor_bits = i32::from(abi::FUNC_FLAG_CONSTRUCTOR | abi::FUNC_FLAG_SELF_ALLOC) << 16;
    wasm!(function;
        I32Const(abi::FUNC_BLOCK_SIZE.cast_signed()), Call(helpers.alloc), LocalTee(f),
        I32Const(TYPE_FUNCTION | (i32::from(abi::FUNC_FLAG_BOUND) << 16)),
        I32Const(ctor_bits), I32Const(0),
        LocalGet(target), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(abi::HEADER_FLAGS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(i32::from(abi::FUNC_FLAG_CONSTRUCTOR)), I32And, Select, I32Or,
        I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(f), I32Const(0),
        I32Store(MemArg { offset: u64::from(abi::OBJ_PROPS_OFFSET), align: 2, memory_index: 0 }),
        LocalGet(f), LocalGet(target), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(f), I32Const(helpers.builtin_slot(Builtin::BoundCall).cast_signed()),
        I32Store(MemArg { offset: FUNC_INDEX_OFFSET, align: 2, memory_index: 0 }),
        LocalGet(f), I32Const(0),
        I32Store(MemArg { offset: FUNC_INDEX_OFFSET + 4, align: 2, memory_index: 0 }),
        LocalGet(f), LocalGet(rec), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        I64Store(MemArg { offset: FUNC_ENV_OFFSET, align: 3, memory_index: 0 }),
        // length
        F64Const(Ieee64::from(0.0)), LocalSet(len),
        I32Const(static_key(strings, INDEX_LENGTH_KEY)), LocalGet(target), Call(helpers.has_own()),
        If(BlockType::Empty),
        LocalGet(target), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get),
        LocalSet(v),
    );
    return_if_pending(function);
    emit_is_number(function, v);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(v), F64ReinterpretI64, F64Trunc, LocalTee(len), LocalGet(len), F64Eq,
        If(BlockType::Result(ValType::F64)),
        F64Const(Ieee64::from(0.0)), LocalGet(len), LocalGet(nb), F64ConvertI32U, F64Sub, F64Max,
        Else, F64Const(Ieee64::from(0.0)),
        End,
        LocalSet(len),
        Else, F64Const(Ieee64::from(0.0)), LocalSet(len),
        End,
        End,
        LocalGet(f), I32Const(static_key(strings, INDEX_LENGTH_KEY)),
        LocalGet(len), I64ReinterpretF64,
        I32Const(abi::ATTR_CONFIGURABLE.cast_signed()), Call(helpers.define),
        // name
        LocalGet(target), I32Const(static_key(strings, "name")), Call(helpers.get), LocalSet(v),
    );
    return_if_pending(function);
    wasm!(function; I32Const(static_key(strings, BOUND_PREFIX)));
    emit_is_string(function, v);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), I32WrapI64,
        Else, I32Const(static_key(strings, "")),
        End,
        Call(helpers.concat), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(v),
        LocalGet(f), I32Const(static_key(strings, "name")), LocalGet(v),
        I32Const(abi::ATTR_CONFIGURABLE.cast_signed()), Call(helpers.define),
        LocalGet(f), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// The `[[Construct]]` of a bound function: construct the target (i64 temp
/// 0) with the argument array (i64 temp 2) and `new.target` (i64 temp 3).
fn bound_construct(frame: Frame) -> IrNode {
    IrNode::Construct {
        callee: Box::new(IrNode::Local(frame.i64(0))),
        args: Vec::new(),
        spread: Some(Box::new(IrNode::ArraySpread(vec![SpreadElem::Spread(
            IrNode::Local(frame.i64(2)),
        )]))),
        callee_local: frame.x64(0),
        this_local: frame.x64(1),
        reserve: 0,
        not_constructor: CalleeError::not_a_constructor("bound target"),
        new_target: Some(Box::new(IrNode::Local(frame.i64(3)))),
    }
}

/// The trampoline of every bound function (`env` = `[target, boundThis,
/// ...boundArgs]`): the bound arguments then the call's arguments form one
/// fresh array. A call calls the target with `this = boundThis`; a construct
/// (the marker in `argc`) constructs the target with `new.target` = the
/// target when it was this bound function (recognised by its `env`), else
/// the incoming `new.target`.
pub(super) fn emit_bound_call(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (target, this, args, nt) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (n, rec, dst, i) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (nb, total) = (frame.x32(0), frame.x32(1));
    let (c, tmp) = (frame.f64(0), frame.x64(0));
    let env = frame.k;
    wasm!(function;
        LocalGet(env), I32WrapI64, LocalTee(rec),
    );
    emit_array_elems(function);
    wasm!(function;
        I64Load(elem_slot(0)), LocalSet(target),
        LocalGet(rec),
        I32Load(MemArg { offset: ARRAY_LEN_OFFSET, align: 2, memory_index: 0 }),
        I32Const(2), I32Sub, LocalSet(nb),
        // n = argc, decoding the construct marker -(n + 1).
        LocalGet(frame.argc()), F64ReinterpretI64, LocalTee(c),
        F64Const(Ieee64::from(-1.0)), LocalGet(c), F64Sub,
        LocalGet(c), LocalGet(c), F64Const(Ieee64::from(0.0)), F64Lt, Select,
        I32TruncSatF64U, LocalTee(n),
        LocalGet(nb), I32Add, LocalTee(total), Call(helpers.new_array()), LocalSet(dst),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(nb), I32GeU, BrIf(1),
        LocalGet(dst),
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(i), I32Const(3), I32Shl, I32Add, LocalGet(rec));
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(2)),
        I64Store(elem_slot(0)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
    store_args(function, frame, dst, nb, 0, n, i, tmp);
    wasm!(function;
        LocalGet(c), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty),
        GlobalGet(NEW_TARGET_GLOBAL), LocalSet(nt),
    );
    emit_is_function(function, nt);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(nt), I32WrapI64,
        I64Load(MemArg { offset: FUNC_ENV_OFFSET, align: 3, memory_index: 0 }),
        LocalGet(env), I64Eq,
        If(BlockType::Empty), LocalGet(target), LocalSet(nt), End,
        End,
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(args),
    );
    emit_node(&bound_construct(frame), function, frame.scratch(), strings, helpers);
    wasm!(function; Return, End, LocalGet(rec));
    emit_array_elems(function);
    wasm!(function; I64Load(elem_slot(1)), LocalSet(this));
    call_with_array(function, target, this, dst, total, helpers);
}

/// `Function.prototype.toString`: the `NativeFunction` text `function
/// <name>() { [native code] }` for every callable `this` (no source text is
/// kept), `<name>` being the function's initial name (its `__fn_info` entry)
/// when that is an `IdentifierName`, optionally after `get ` / `set `, else
/// empty (a bound function's info name is empty).
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let target = frame.i64(0);
    let (name, len, i, start) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let c = frame.x32(0);
    let at = |function: &mut Function| {
        wasm!(function;
            LocalGet(name), LocalGet(i), I32Add,
            I32Load8U(MemArg { offset: u64::from(abi::HEADER_SIZE) + 4, align: 0, memory_index: 0 }),
            LocalTee(c),
        );
    };
    this_callable(
        function,
        frame,
        target,
        TO_STRING_NOT_CALLABLE,
        strings,
        helpers,
    );
    let get_ = i32::from_le_bytes(*b"get ");
    let set_ = i32::from_le_bytes(*b"set ");
    wasm!(function;
        LocalGet(target), I32WrapI64,
        I32Load(MemArg { offset: FUNC_INDEX_OFFSET, align: 2, memory_index: 0 }),
        I32Const(abi::FN_INFO_ENTRY_SIZE.cast_signed()), I32Mul, I32Const(helpers.fn_info), I32Add,
        I32Load(MemArg { offset: u64::from(abi::FN_INFO_NAME_OFFSET), align: 2, memory_index: 0 }),
        LocalTee(name),
        I32Load(MemArg { offset: u64::from(abi::HEADER_SIZE), align: 2, memory_index: 0 }),
        LocalSet(len),
        // An accessor's `get ` / `set ` prefix.
        I32Const(0), LocalSet(start),
        LocalGet(len), I32Const(4), I32GtU,
        If(BlockType::Empty),
        LocalGet(name),
        I32Load(MemArg { offset: u64::from(abi::HEADER_SIZE) + 4, align: 0, memory_index: 0 }),
        LocalTee(c), I32Const(get_), I32Eq, LocalGet(c), I32Const(set_), I32Eq, I32Or,
        If(BlockType::Empty), I32Const(4), LocalSet(start), End,
        End,
        LocalGet(start), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
    );
    // Valid: `$`, `_`, a letter, a non-ASCII byte, or a digit past the start.
    at(function);
    wasm!(function;
        I32Const(0x20), I32Or, I32Const(i32::from(b'a')), I32Sub, I32Const(26), I32LtU,
        LocalGet(c), I32Const(i32::from(b'$')), I32Eq, I32Or,
        LocalGet(c), I32Const(i32::from(b'_')), I32Eq, I32Or,
        LocalGet(c), I32Const(0x80), I32GeU, I32Or,
        LocalGet(c), I32Const(i32::from(b'0')), I32Sub, I32Const(10), I32LtU,
        LocalGet(i), LocalGet(start), I32GtU, I32And, I32Or,
        I32Eqz,
        If(BlockType::Empty), I32Const(static_key(strings, "")), LocalSet(name), Br(2), End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        I32Const(static_key(strings, NATIVE_HEAD)), LocalGet(name), Call(helpers.concat),
        I32Const(static_key(strings, NATIVE_TAIL)), Call(helpers.concat),
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
