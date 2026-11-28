//! `ArrayBuffer` (DESIGN.md §5.8): the constructor, `ArrayBuffer.isView`
//! and the `%ArrayBuffer.prototype%` getters and methods (`resize`,
//! `slice`, `transfer`, `transferToFixedLength`).
//!
//! A buffer is a [`abi::TYPE_ARRAYBUFFER`] block over a `TYPE_RAW` byte
//! block (`__ab_alloc`) whose `len` is the capacity: a resizable buffer
//! grows within it, and reallocates (doubling, capped at its maximum) past
//! it. Bytes that leave the live range are zeroed at once, so a later growth
//! reads zeros. `transfer` hands the byte block to the new buffer when it is
//! large enough and detaches the source.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::emit::binary::RAW_DATA;
use crate::emit::realm::ArrayBufferOp;

/// `[message, stack]` of a `TypeError` with `message`.
macro_rules! type_error {
    ($message:expr) => {
        [$message, concat!("TypeError: ", $message)]
    };
}

/// `[message, stack]` of a `RangeError` with `message`.
macro_rules! range_error {
    ($message:literal) => {
        [$message, concat!("RangeError: ", $message)]
    };
}

/// The `TypeError` of `ArrayBuffer.prototype.<name>` on a receiver that is
/// not an `ArrayBuffer` (or, for `resize`, not a resizable one).
macro_rules! incompatible {
    ($name:literal) => {
        type_error!(concat!(
            "Method ArrayBuffer.prototype.",
            $name,
            " called on incompatible receiver"
        ))
    };
}

/// The `TypeError` of `ArrayBuffer.prototype.<name>` on a detached buffer.
macro_rules! detached {
    ($name:literal) => {
        type_error!(concat!(
            "Cannot perform ArrayBuffer.prototype.",
            $name,
            " on a detached ArrayBuffer"
        ))
    };
}

/// The receiver `TypeError`s, one per prototype builtin.
const RECEIVER_TEXTS: [(ArrayBufferOp, CalleeErrorText<'static>); 8] = [
    (ArrayBufferOp::ByteLength, incompatible!("byteLength")),
    (ArrayBufferOp::MaxByteLength, incompatible!("maxByteLength")),
    (ArrayBufferOp::Resizable, incompatible!("resizable")),
    (ArrayBufferOp::Detached, incompatible!("detached")),
    (ArrayBufferOp::Resize, incompatible!("resize")),
    (ArrayBufferOp::Slice, incompatible!("slice")),
    (ArrayBufferOp::Transfer, incompatible!("transfer")),
    (
        ArrayBufferOp::TransferToFixedLength,
        incompatible!("transferToFixedLength"),
    ),
];

/// The detached-receiver `TypeError`s of the methods.
const DETACHED_TEXTS: [(ArrayBufferOp, CalleeErrorText<'static>); 4] = [
    (ArrayBufferOp::Resize, detached!("resize")),
    (ArrayBufferOp::Slice, detached!("slice")),
    (ArrayBufferOp::Transfer, detached!("transfer")),
    (
        ArrayBufferOp::TransferToFixedLength,
        detached!("transferToFixedLength"),
    ),
];

/// `ToIndex(length)` out of range (the constructor, `transfer*`), and a
/// `transfer` length past a resizable buffer's maximum.
const INVALID_LENGTH: CalleeErrorText<'static> = range_error!("Invalid array buffer length");

/// `ToIndex(options.maxByteLength)` out of range, or `length >
/// maxByteLength`.
const INVALID_MAX_LENGTH: CalleeErrorText<'static> =
    range_error!("Invalid array buffer max length");

/// A byte length or maximum past [`abi::BYTE_LENGTH_MAX`].
const ALLOCATION_FAILED: CalleeErrorText<'static> = range_error!("Array buffer allocation failed");

/// `resize` with a length out of range or past the maximum.
const RESIZE_LENGTH: CalleeErrorText<'static> =
    range_error!("ArrayBuffer.prototype.resize: Invalid length parameter");

/// The `slice` result is not an `ArrayBuffer`.
const SLICE_NOT_BUFFER: CalleeErrorText<'static> =
    type_error!("ArrayBuffer subclass returned a non-ArrayBuffer from its species constructor");

/// The `slice` result is the receiver itself.
const SLICE_SAME: CalleeErrorText<'static> =
    type_error!("ArrayBuffer subclass returned this from species constructor");

/// The `slice` result is shorter than the slice.
const SLICE_TOO_SMALL: CalleeErrorText<'static> =
    type_error!("ArrayBuffer subclass returned a too small buffer from its species constructor");

/// The `slice` result is detached.
const SLICE_DETACHED_RESULT: CalleeErrorText<'static> =
    type_error!("ArrayBuffer subclass returned a detached buffer from its species constructor");

/// `SpeciesConstructor` over a non-object `constructor`.
const CTOR_NOT_OBJECT: CalleeErrorText<'static> =
    type_error!("object.constructor is not an object");

/// `SpeciesConstructor` over a `@@species` that is not a constructor.
const SPECIES_NOT_CTOR: CalleeErrorText<'static> =
    type_error!("object.constructor[Symbol.species] is not a constructor");

/// The `TypeError`s of the `slice` result and of `SpeciesConstructor`.
const SLICE_TEXTS: [CalleeErrorText<'static>; 6] = [
    SLICE_NOT_BUFFER,
    SLICE_SAME,
    SLICE_TOO_SMALL,
    SLICE_DETACHED_RESULT,
    CTOR_NOT_OBJECT,
    SPECIES_NOT_CTOR,
];

/// The `RangeError`s.
const RANGE_TEXTS: [CalleeErrorText<'static>; 4] = [
    INVALID_LENGTH,
    INVALID_MAX_LENGTH,
    ALLOCATION_FAILED,
    RESIZE_LENGTH,
];

/// The source name in the `TypeError` of a `@@species` that is not a
/// constructor.
const SPECIES_CALLEE: &str = "object.constructor[Symbol.species]";

/// `2^53 - 1`, the largest `ToIndex` result.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The static strings of this family (with the binary renderer's).
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    RECEIVER_TEXTS
        .into_iter()
        .chain(DETACHED_TEXTS)
        .flat_map(|(_, text)| text)
        .chain(SLICE_TEXTS.into_iter().flatten())
        .chain(RANGE_TEXTS.into_iter().flatten())
        .chain(["maxByteLength", "prototype", "constructor"])
        .chain(crate::emit::binary::STRINGS)
}

/// The IR fragments the bodies emit with `K = k`, for the static-string
/// scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame::new(k);
    vec![construct(frame, frame.y64(3), frame.y64(4))]
}

/// The text of `op` in `texts` (the first one when absent).
fn text_of(
    texts: &[(ArrayBufferOp, CalleeErrorText<'static>)],
    op: ArrayBufferOp,
) -> CalleeErrorText<'static> {
    texts
        .iter()
        .find_map(|&(o, text)| (o == op).then_some(text))
        .unwrap_or(texts[0].1)
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

/// Push the `u32` field at `offset` of the buffer jsval in i64 `buffer`.
fn field(function: &mut FnBuilder<'_>, buffer: u32, offset: u32) {
    wasm!(function; LocalGet(buffer), I32WrapI64, I32Load(m32(offset)));
}

/// Store the i32 local `value` into the `u32` field at `offset` of the
/// buffer jsval in i64 `buffer`.
fn set_field(function: &mut FnBuilder<'_>, buffer: u32, offset: u32, value: u32) {
    wasm!(function; LocalGet(buffer), I32WrapI64, LocalGet(value), I32Store(m32(offset)));
}

/// Push the header flags of the buffer jsval in i64 `buffer` masked by
/// `flag` (nonzero when set).
fn flag(function: &mut FnBuilder<'_>, buffer: u32, flag: u8) {
    wasm!(function;
        LocalGet(buffer), I32WrapI64, I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(flag)), I32And,
    );
}

/// Push `1` when the jsval in `local` is a block of type byte `kind`.
fn is_kind(function: &mut FnBuilder<'_>, local: u32, kind: u8) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(i32::from(kind)), I32Eq, Else, I32Const(0), End);
}

/// Throw a `RangeError` with `text` and return.
fn throw_range_error(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    [message, stack]: CalleeErrorText<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let error = IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(message.into())),
        IrNode::Str(stack.into()),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
}

/// Read `this` into i64 `this` and throw `text` unless it is an
/// `ArrayBuffer` (a resizable one when `resizable`).
fn this_buffer(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    this: u32,
    resizable: bool,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_kind(function, this, abi::TYPE_ARRAYBUFFER);
    if resizable {
        wasm!(function; If(BlockType::Result(ValType::I32)));
        flag(function, this, abi::AB_FLAG_RESIZABLE);
        wasm!(function; Else, I32Const(0), End);
    }
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Throw `text` when the buffer jsval in i64 `buffer` is detached.
fn check_detached(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    buffer: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    flag(function, buffer, abi::AB_FLAG_DETACHED);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// `ToIntegerOrInfinity` of the jsval in i64 `v` into f64 `out` (NaN is
/// `0`; a throwing `valueOf` returns).
fn to_integer(function: &mut FnBuilder<'_>, v: u32, out: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(v), Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function;
        F64ReinterpretI64, F64Trunc, LocalTee(out), LocalGet(out), F64Ne,
        If(BlockType::Empty), F64Const(Ieee64::from(0.0)), LocalSet(out), End,
    );
}

/// `ToIndex` of the jsval in i64 `v` into f64 `out`: `ToIntegerOrInfinity`,
/// then a `RangeError` with `text` below `0` or above `2^53 - 1`.
fn to_index(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (v, out): (u32, u32),
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    to_integer(function, v, out, helpers);
    wasm!(function;
        LocalGet(out), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(out), F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Gt, I32Or,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Push the relative index of the integer in f64 `r` against the f64
/// length `len`: `max(len + r, 0)` for a negative `r`, else `min(r, len)`.
fn relative(function: &mut FnBuilder<'_>, r: u32, len: u32) {
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Result(ValType::F64)),
        LocalGet(len), LocalGet(r), F64Add, F64Const(Ieee64::from(0.0)), F64Max,
        Else,
        LocalGet(r), LocalGet(len), F64Min,
        End,
    );
}

/// The fresh `ArrayBuffer` block whose base the constant-size allocation
/// left on the stack into i64 `out`, with the header flags
/// in the i32 local `flags` and the prototype in the i64 local `proto` (that
/// allocation never returns `0`); `data`, `byte_length` and
/// `max_byte_length` start at `0`.
fn new_buffer(function: &mut FnBuilder<'_>, frame: Frame, flags: u32, proto: u32, out: u32) {
    let base = frame.base(0);
    wasm!(function;
        LocalTee(base),
        LocalGet(flags), I32Const(16), I32Shl, I32Const(i32::from(abi::TYPE_ARRAYBUFFER)), I32Or,
        I32Store(m32(0)),
        LocalGet(base), LocalGet(proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
    );
}

/// `Construct(c, [len])` for the constructor in i64 `c` and the length
/// number in i64 `len`; with no positional slot (`K = 0`) the length goes in
/// a spread argument array.
fn construct(frame: Frame, c: u32, len: u32) -> IrNode {
    let len = IrNode::Local(len);
    let (args, spread) = if frame.k == 0 {
        let spread = IrNode::ArraySpread(vec![SpreadElem::Item(len)]);
        (Vec::new(), Some(Box::new(spread)))
    } else {
        (vec![len], None)
    };
    IrNode::Construct {
        callee: Box::new(IrNode::Local(c)),
        args,
        spread,
        callee_local: frame.z64(2),
        this_local: frame.z64(3),
        reserve: 0,
        not_constructor: CalleeError::not_a_constructor(SPECIES_CALLEE),
        new_target: None,
    }
}

/// Emit the body of the `ArrayBuffer` builtin `builtin` into `f`.
pub(super) fn emit(
    builtin: Builtin,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::Ctor(_) => emit_ctor(f, frame, strings, helpers),
        Builtin::ArrayBuffer(ArrayBufferOp::IsView) => emit_is_view(f, frame),
        Builtin::ArrayBuffer(op @ ArrayBufferOp::Resize) => {
            emit_resize(op, f, frame, strings, helpers);
        }
        Builtin::ArrayBuffer(op @ ArrayBufferOp::Slice) => {
            emit_slice(op, f, frame, strings, helpers);
        }
        Builtin::ArrayBuffer(op @ ArrayBufferOp::Transfer) => {
            emit_transfer(op, true, f, frame, strings, helpers);
        }
        Builtin::ArrayBuffer(op @ ArrayBufferOp::TransferToFixedLength) => {
            emit_transfer(op, false, f, frame, strings, helpers);
        }
        Builtin::ArrayBuffer(op) => emit_getter(op, f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// `ArrayBuffer(length, options)` (DESIGN.md §5.8.7): the "requires 'new'"
/// `TypeError` when called; `ToIndex(length)`; `options.maxByteLength`
/// (read only from an object; `undefined` keeps the buffer fixed) through
/// `ToIndex`, `length > maxByteLength` a `RangeError`; then
/// `GetPrototypeFromConstructor(new.target)` (saved on entry, as the
/// conversions may construct), then the allocation limits and the byte
/// block. The arguments are read on entry, before any user code runs (and
/// can republish the spread argument array).
fn emit_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (nt, len_v, options, max_v) = (frame.y64(0), frame.y64(1), frame.y64(2), frame.y64(3));
    let (proto, out) = (frame.y64(4), frame.y64(5));
    let (len, max) = (frame.zf64(0), frame.zf64(1));
    let (resizable, flags, data) = (frame.w32(0), frame.w32(1), frame.w32(2));
    wasm!(function; GlobalGet(NEW_TARGET_GLOBAL), LocalSet(nt));
    // The IR reader decodes the construct marker in `argc` for an argument
    // past the `K` slots (a spread construct, or a species construct under
    // `K = 0`).
    for (i, local) in [(0, len_v), (1, options)] {
        emit_node(
            &frame.body().arg(i),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; LocalSet(local));
    }
    let requires_new = super::ctor::fragment(BuiltinCtor::ArrayBuffer, frame.k);
    emit_drop(function, frame, &requires_new, strings, helpers);
    return_if_pending(function);
    to_index(
        function,
        frame,
        (len_v, len),
        INVALID_LENGTH,
        strings,
        helpers,
    );
    wasm!(function; I32Const(0), LocalSet(resizable));
    emit_is_object(function, options);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(options), I32Const(static_key(strings, "maxByteLength")), Call(helpers.get),
        LocalSet(max_v),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(max_v), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
    );
    to_index(
        function,
        frame,
        (max_v, max),
        INVALID_MAX_LENGTH,
        strings,
        helpers,
    );
    wasm!(function;
        I32Const(1), LocalSet(resizable),
        LocalGet(len), LocalGet(max), F64Gt,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, INVALID_MAX_LENGTH, strings, helpers);
    wasm!(function; End, End, End);
    // GetPrototypeFromConstructor(new.target, %ArrayBuffer.prototype%).
    let realm_proto = intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::ArrayBuffer), helpers);
    wasm!(function;
        LocalGet(nt), I32Const(static_key(strings, "prototype")), Call(helpers.get),
        LocalSet(proto),
    );
    return_if_pending(function);
    emit_is_object(function, proto);
    wasm!(function;
        I32Eqz, If(BlockType::Empty), I64Const(realm_proto), LocalSet(proto), End,
        // CreateByteDataBlock: past the linear-memory cap nothing can be
        // allocated.
        LocalGet(len), F64Const(Ieee64::from(f64::from(abi::BYTE_LENGTH_MAX))), F64Gt,
        LocalGet(resizable),
        LocalGet(max), F64Const(Ieee64::from(f64::from(abi::BYTE_LENGTH_MAX))), F64Gt, I32And,
        I32Or,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, ALLOCATION_FAILED, strings, helpers);
    wasm!(function;
        End,
        LocalGet(resizable), I32Eqz, If(BlockType::Empty), LocalGet(len), LocalSet(max), End,
        I32Const(i32::from(abi::AB_FLAG_RESIZABLE)), I32Const(0), LocalGet(resizable), Select,
        LocalSet(flags),
    );
    emit_alloc_const(function, helpers, abi::AB_BLOCK_SIZE.cast_signed());
    new_buffer(function, frame, flags, proto, out);
    wasm!(function; LocalGet(len), I32TruncF64U, Call(helpers.ab_alloc()), LocalSet(data));
    return_if_pending(function);
    set_field(function, out, abi::AB_DATA_OFFSET, data);
    wasm!(function; LocalGet(len), I32TruncF64U, LocalSet(data));
    set_field(function, out, abi::AB_BYTE_LENGTH_OFFSET, data);
    wasm!(function; LocalGet(max), I32TruncF64U, LocalSet(data));
    set_field(function, out, abi::AB_MAX_BYTE_LENGTH_OFFSET, data);
    wasm!(function; LocalGet(out));
}

/// `ArrayBuffer.isView(arg)`: `true` for a typed array or a `DataView`.
fn emit_is_view(function: &mut FnBuilder<'_>, frame: Frame) {
    let (arg, kind) = (frame.y64(0), frame.w32(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    emit_is_pointer(function, arg);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, arg);
    wasm!(function;
        LocalTee(kind), I32Const(i32::from(abi::TYPE_TYPEDARRAY)), I32Eq,
        LocalGet(kind), I32Const(i32::from(abi::TYPE_DATAVIEW)), I32Eq, I32Or,
        Else, I32Const(0), End,
        LocalSet(kind),
        I64Const(TRUE_BITS), I64Const(FALSE_BITS), LocalGet(kind), Select,
    );
}

/// The `byteLength` / `maxByteLength` / `resizable` / `detached` getters: the
/// receiver check, then the field (a detached buffer's lengths are `0`, a
/// fixed buffer's maximum is its length).
fn emit_getter(
    op: ArrayBufferOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.y64(0);
    let text = text_of(&RECEIVER_TEXTS, op);
    this_buffer(function, frame, this, false, text, strings, helpers);
    let length = |function: &mut FnBuilder<'_>, offset: u32| {
        field(function, this, offset);
        wasm!(function; F64ConvertI32U, I64ReinterpretF64);
    };
    let boolean = |function: &mut FnBuilder<'_>, bit: u8| {
        wasm!(function; I64Const(TRUE_BITS), I64Const(FALSE_BITS));
        flag(function, this, bit);
        wasm!(function; Select);
    };
    match op {
        ArrayBufferOp::MaxByteLength => length(function, abi::AB_MAX_BYTE_LENGTH_OFFSET),
        ArrayBufferOp::Resizable => boolean(function, abi::AB_FLAG_RESIZABLE),
        ArrayBufferOp::Detached => boolean(function, abi::AB_FLAG_DETACHED),
        _ => length(function, abi::AB_BYTE_LENGTH_OFFSET),
    }
}

/// `ArrayBuffer.prototype.resize(newLength)` (DESIGN.md §5.8.4): a
/// resizable receiver (else the incompatible-receiver `TypeError`),
/// `ToIndex(newLength)`, then a detached receiver throws, then a length past
/// the maximum is a `RangeError`. Shrinking zeroes the dropped tail; growing
/// within the byte block's capacity only raises the length, past it
/// reallocates `min(max, max(n, 2 * capacity))` bytes and copies (an
/// allocation failure leaves the buffer unchanged with the `RangeError`
/// pending).
fn emit_resize(
    op: ArrayBufferOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, v) = (frame.y64(0), frame.y64(1));
    let n = frame.zf64(0);
    let (ni, len, data, cap, size, fresh) = (
        frame.w32(0),
        frame.w32(1),
        frame.w32(2),
        frame.w32(3),
        frame.w32(4),
        frame.w32(5),
    );
    this_buffer(
        function,
        frame,
        this,
        true,
        text_of(&RECEIVER_TEXTS, op),
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    to_index(function, frame, (v, n), RESIZE_LENGTH, strings, helpers);
    check_detached(
        function,
        frame,
        this,
        text_of(&DETACHED_TEXTS, op),
        strings,
        helpers,
    );
    wasm!(function; LocalGet(n));
    field(function, this, abi::AB_MAX_BYTE_LENGTH_OFFSET);
    wasm!(function; F64ConvertI32U, F64Gt, If(BlockType::Empty));
    throw_range_error(function, frame, RESIZE_LENGTH, strings, helpers);
    wasm!(function; End, LocalGet(n), I32TruncF64U, LocalSet(ni));
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; LocalSet(len));
    field(function, this, abi::AB_DATA_OFFSET);
    wasm!(function;
        LocalSet(data),
        LocalGet(ni), LocalGet(len), I32LeU,
        If(BlockType::Empty),
        // Shrink (or keep): zero the bytes that leave the live range.
        LocalGet(data), If(BlockType::Empty),
        LocalGet(data), I32Const(RAW_DATA.cast_signed()), I32Add, LocalGet(ni), I32Add,
        I32Const(0),
        LocalGet(len), LocalGet(ni), I32Sub,
        MemoryFill(0),
        End,
    );
    set_field(function, this, abi::AB_BYTE_LENGTH_OFFSET, ni);
    wasm!(function;
        Else,
        LocalGet(data), If(BlockType::Result(ValType::I32)),
        LocalGet(data), I32Load(m32(abi::HEADER_SIZE)),
        Else, I32Const(0), End,
        LocalSet(cap),
        LocalGet(ni), LocalGet(cap), I32LeU,
        If(BlockType::Empty),
    );
    set_field(function, this, abi::AB_BYTE_LENGTH_OFFSET, ni);
    wasm!(function;
        Else,
        // size = min(max, max(n, 2 * capacity)).
        LocalGet(cap), I32Const(1), I32Shl, LocalTee(size),
        LocalGet(ni), LocalGet(size), LocalGet(ni), I32GtU, Select, LocalSet(size),
    );
    field(function, this, abi::AB_MAX_BYTE_LENGTH_OFFSET);
    wasm!(function;
        LocalTee(cap), LocalGet(size), LocalGet(size), LocalGet(cap), I32GtU, Select,
        Call(helpers.ab_alloc()), LocalSet(fresh),
    );
    return_if_pending(function);
    // The allocation may have collected (blocks never move): reload.
    field(function, this, abi::AB_DATA_OFFSET);
    wasm!(function;
        LocalTee(data), If(BlockType::Empty),
        LocalGet(fresh), I32Const(RAW_DATA.cast_signed()), I32Add,
        LocalGet(data), I32Const(RAW_DATA.cast_signed()), I32Add,
        LocalGet(len),
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
    );
    set_field(function, this, abi::AB_DATA_OFFSET, fresh);
    set_field(function, this, abi::AB_BYTE_LENGTH_OFFSET, ni);
    wasm!(function; End, End, I64Const(UNDEFINED_BITS));
}

/// `SpeciesConstructor(o, %ArrayBuffer%)` of the buffer in i64 `o` into i64
/// `c`: `C = Get(o, "constructor")`; `undefined` is the default, a
/// non-object throws, else `S = Get(C, @@species)`: `null` / `undefined` is
/// the default, a constructor is the result, anything else throws.
fn species_constructor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    o: u32,
    c: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let default = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::ArrayBuffer), helpers);
    wasm!(function;
        LocalGet(o), I32Const(static_key(strings, "constructor")), Call(helpers.get), LocalSet(c),
    );
    return_if_pending(function);
    wasm!(function; LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
    wasm!(function; I64Const(default), LocalSet(c), Else);
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
    wasm!(function; If(BlockType::Empty), I64Const(default), LocalSet(c), Else);
    emit_is_constructor(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, SPECIES_NOT_CTOR, strings, helpers);
    wasm!(function; End, End, End);
}

/// `ArrayBuffer.prototype.slice(start, end)` (DESIGN.md §5.8.7): a detached
/// receiver throws; the relative `first` / `final` over the length; the
/// result from `Construct(SpeciesConstructor(O, %ArrayBuffer%), [newLen])`
/// must be a different, non-detached `ArrayBuffer` of at least `newLen`
/// bytes; the receiver is re-checked for detachment, then the bytes
/// `[first, first + newLen)` that still exist are copied.
fn emit_slice(
    op: ArrayBufferOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, start, end, c, len_v, out) = (
        frame.y64(0),
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
        frame.y64(5),
    );
    let (len, r, first, last, new_len) = (
        frame.zf64(0),
        frame.zf64(1),
        frame.zf64(2),
        frame.zf64(3),
        frame.zf64(4),
    );
    let (current, from, count) = (frame.w32(0), frame.w32(1), frame.w32(2));
    let detached = text_of(&DETACHED_TEXTS, op);
    this_buffer(
        function,
        frame,
        this,
        false,
        text_of(&RECEIVER_TEXTS, op),
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function; LocalSet(start));
    frame.arg(function, 1);
    wasm!(function; LocalSet(end));
    check_detached(function, frame, this, detached, strings, helpers);
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; F64ConvertI32U, LocalSet(len));
    to_integer(function, start, r, helpers);
    relative(function, r, len);
    wasm!(function;
        LocalSet(first),
        LocalGet(end), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty), LocalGet(len), LocalSet(last), Else,
    );
    to_integer(function, end, r, helpers);
    relative(function, r, len);
    wasm!(function;
        LocalSet(last),
        End,
        LocalGet(last), LocalGet(first), F64Sub, F64Const(Ieee64::from(0.0)), F64Max,
        LocalTee(new_len), I64ReinterpretF64, LocalSet(len_v),
    );
    species_constructor(function, frame, this, c, strings, helpers);
    emit_node(
        &construct(frame, c, len_v),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; LocalSet(out));
    return_if_pending(function);
    is_kind(function, out, abi::TYPE_ARRAYBUFFER);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, SLICE_NOT_BUFFER, strings, helpers);
    wasm!(function; End);
    check_detached(
        function,
        frame,
        out,
        SLICE_DETACHED_RESULT,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(out), LocalGet(this), I64Eq, If(BlockType::Empty));
    throw_type_error(function, frame, SLICE_SAME, strings, helpers);
    wasm!(function; End);
    field(function, out, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; F64ConvertI32U, LocalGet(new_len), F64Lt, If(BlockType::Empty));
    throw_type_error(function, frame, SLICE_TOO_SMALL, strings, helpers);
    wasm!(function; End);
    // The species constructor may have detached or resized the receiver.
    check_detached(function, frame, this, detached, strings, helpers);
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function;
        LocalSet(current),
        LocalGet(first), I32TruncF64U, LocalTee(from), LocalGet(current), I32LtU,
        If(BlockType::Empty),
        // count = min(newLen, current - first).
        LocalGet(current), LocalGet(from), I32Sub, LocalTee(count),
        LocalGet(new_len), I32TruncF64U, LocalTee(current),
        LocalGet(count), LocalGet(current), I32LtU, Select, LocalSet(count),
    );
    field(function, out, abi::AB_DATA_OFFSET);
    wasm!(function; I32Const(RAW_DATA.cast_signed()), I32Add);
    field(function, this, abi::AB_DATA_OFFSET);
    wasm!(function;
        I32Const(RAW_DATA.cast_signed()), I32Add, LocalGet(from), I32Add,
        LocalGet(count),
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
        LocalGet(out),
    );
}

/// `ArrayBuffer.prototype.transfer(newLength)` (`preserve`) /
/// `transferToFixedLength(newLength)` (`ArrayBufferCopyAndDetach`, DESIGN.md
/// §5.8.4): `newLength` is the current length when `undefined`, else
/// `ToIndex`; a detached receiver throws; `transfer` keeps a resizable
/// buffer resizable with its maximum (a longer `newLength` is a
/// `RangeError`). The new buffer (over `%ArrayBuffer.prototype%`) takes the
/// byte block over when it holds `newLength` bytes, zeroing a dropped tail,
/// else gets a fresh block and a copy; then the receiver is detached.
fn emit_transfer(
    op: ArrayBufferOp,
    preserve: bool,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, v, proto, out) = (frame.y64(0), frame.y64(1), frame.y64(2), frame.y64(3));
    let (n, max) = (frame.zf64(0), frame.zf64(1));
    let (flags, ni) = (frame.w32(0), frame.w32(1));
    this_buffer(
        function,
        frame,
        this,
        false,
        text_of(&RECEIVER_TEXTS, op),
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
    );
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; F64ConvertI32U, LocalSet(n), Else);
    to_index(function, frame, (v, n), INVALID_LENGTH, strings, helpers);
    wasm!(function; End);
    check_detached(
        function,
        frame,
        this,
        text_of(&DETACHED_TEXTS, op),
        strings,
        helpers,
    );
    if preserve {
        flag(function, this, abi::AB_FLAG_RESIZABLE);
    } else {
        wasm!(function; I32Const(0));
    }
    wasm!(function;
        LocalSet(flags),
        LocalGet(flags), If(BlockType::Empty),
    );
    field(function, this, abi::AB_MAX_BYTE_LENGTH_OFFSET);
    wasm!(function;
        F64ConvertI32U, LocalSet(max),
        LocalGet(n), LocalGet(max), F64Gt, If(BlockType::Empty),
    );
    throw_range_error(function, frame, INVALID_LENGTH, strings, helpers);
    wasm!(function;
        End,
        Else, LocalGet(n), LocalSet(max), End,
        LocalGet(n), F64Const(Ieee64::from(f64::from(abi::BYTE_LENGTH_MAX))), F64Gt,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, ALLOCATION_FAILED, strings, helpers);
    wasm!(function;
        End,
        LocalGet(n), I32TruncF64U, LocalSet(ni),
        I64Const(intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::ArrayBuffer), helpers)),
        LocalSet(proto),
    );
    emit_alloc_const(function, helpers, abi::AB_BLOCK_SIZE.cast_signed());
    new_buffer(function, frame, flags, proto, out);
    set_field(function, out, abi::AB_BYTE_LENGTH_OFFSET, ni);
    wasm!(function; LocalGet(max), I32TruncF64U, LocalSet(flags));
    set_field(function, out, abi::AB_MAX_BYTE_LENGTH_OFFSET, flags);
    hand_over(function, frame, (this, out, ni), helpers);
}

/// The end of `ArrayBufferCopyAndDetach`: give the new buffer in i64 `out`
/// the receiver's (in i64 `this`) first `ni` bytes (the i32 local) — its
/// byte block when that holds them, zeroing the bytes past `ni`, else a
/// fresh block and a copy (out of memory returns with the `RangeError`
/// pending, the receiver intact) — then detach the receiver and push `out`.
fn hand_over(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (this, out, ni): (u32, u32, u32),
    helpers: Helpers<'_>,
) {
    let (len, data, cap, fresh, zero) = (
        frame.w32(2),
        frame.w32(3),
        frame.w32(4),
        frame.w32(5),
        frame.w32(6),
    );
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; LocalSet(len));
    field(function, this, abi::AB_DATA_OFFSET);
    wasm!(function;
        LocalTee(data), If(BlockType::Result(ValType::I32)),
        LocalGet(data), I32Load(m32(abi::HEADER_SIZE)),
        Else, I32Const(0), End,
        LocalSet(cap),
        LocalGet(ni), LocalGet(cap), I32LeU,
        If(BlockType::Empty),
        // Take the block over, zeroing the bytes past the new length.
        LocalGet(ni), LocalGet(len), I32LtU, If(BlockType::Empty),
        LocalGet(data), I32Const(RAW_DATA.cast_signed()), I32Add, LocalGet(ni), I32Add,
        I32Const(0),
        LocalGet(len), LocalGet(ni), I32Sub,
        MemoryFill(0),
        End,
    );
    set_field(function, out, abi::AB_DATA_OFFSET, data);
    wasm!(function; Else, LocalGet(ni), Call(helpers.ab_alloc()), LocalSet(fresh));
    return_if_pending(function);
    // The allocation may have collected (blocks never move): reload.
    field(function, this, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function; LocalSet(len));
    field(function, this, abi::AB_DATA_OFFSET);
    wasm!(function;
        LocalTee(data), If(BlockType::Empty),
        LocalGet(fresh), I32Const(RAW_DATA.cast_signed()), I32Add,
        LocalGet(data), I32Const(RAW_DATA.cast_signed()), I32Add,
        LocalGet(len),
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
    );
    set_field(function, out, abi::AB_DATA_OFFSET, fresh);
    wasm!(function; End);
    // DetachArrayBuffer(O).
    wasm!(function; I32Const(0), LocalSet(zero));
    for offset in [
        abi::AB_DATA_OFFSET,
        abi::AB_BYTE_LENGTH_OFFSET,
        abi::AB_MAX_BYTE_LENGTH_OFFSET,
    ] {
        set_field(function, this, offset, zero);
    }
    wasm!(function; LocalGet(this), I32WrapI64);
    flag(function, this, 0xff);
    wasm!(function;
        I32Const(i32::from(abi::AB_FLAG_DETACHED)), I32Or,
        I32Store8(m8(abi::HEADER_FLAGS_OFFSET)),
        LocalGet(out),
    );
}
