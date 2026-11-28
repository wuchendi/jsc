//! `DataView` (DESIGN.md §5.8): the constructor and the
//! `%DataView.prototype%` getters (`buffer`, `byteLength`, `byteOffset`) and
//! get / set methods of the number and `BigInt` element kinds.
//!
//! A view is a [`abi::TYPE_DATAVIEW`] block over an `ArrayBuffer`. It caches
//! nothing: every access re-reads the buffer's state through `__dv_length`
//! (`-1` when out of bounds or detached) and the buffer's `data` block, and
//! loads or stores the element through the element accessors it shares with
//! typed arrays, `__raw_load` / `__raw_store`, which byte-swap for a
//! big-endian access.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::emit::binary::{DV_STRINGS, RAW_DATA};
use crate::emit::realm::DataViewOp;
use crate::emit::registry::Big;

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

/// `(key, incompatible receiver, out of bounds)` texts of the
/// `DataView.prototype.<key>` builtin.
macro_rules! op_texts {
    ($key:literal) => {
        (
            $key,
            type_error!(concat!(
                "Method DataView.prototype.",
                $key,
                " called on incompatible receiver"
            )),
            type_error!(concat!(
                "Cannot perform DataView.prototype.",
                $key,
                " on a detached or out-of-bounds DataView"
            )),
        )
    };
}

/// The receiver and out-of-bounds `TypeError`s of every prototype builtin,
/// by property key.
const OP_TEXTS: [(&str, CalleeErrorText<'static>, CalleeErrorText<'static>); 25] = [
    op_texts!("buffer"),
    op_texts!("byteLength"),
    op_texts!("byteOffset"),
    op_texts!("getInt8"),
    op_texts!("setInt8"),
    op_texts!("getUint8"),
    op_texts!("setUint8"),
    op_texts!("getInt16"),
    op_texts!("setInt16"),
    op_texts!("getUint16"),
    op_texts!("setUint16"),
    op_texts!("getInt32"),
    op_texts!("setInt32"),
    op_texts!("getUint32"),
    op_texts!("setUint32"),
    op_texts!("getFloat32"),
    op_texts!("setFloat32"),
    op_texts!("getFloat64"),
    op_texts!("setFloat64"),
    op_texts!("getFloat16"),
    op_texts!("setFloat16"),
    op_texts!("getBigInt64"),
    op_texts!("setBigInt64"),
    op_texts!("getBigUint64"),
    op_texts!("setBigUint64"),
];

/// The constructor's first argument is not an `ArrayBuffer`.
const NOT_BUFFER: CalleeErrorText<'static> =
    type_error!("First argument to DataView constructor must be an ArrayBuffer");

/// The constructor's buffer is detached.
const CTOR_DETACHED: CalleeErrorText<'static> =
    type_error!("Cannot perform Construct on a detached ArrayBuffer");

/// `ToIndex(byteOffset)` out of range, or an offset past the buffer.
const START_OFFSET: CalleeErrorText<'static> =
    range_error!("Start offset is outside the bounds of the buffer");

/// `ToIndex(byteLength)` out of range, or a view past the buffer's end.
const INVALID_LENGTH: CalleeErrorText<'static> = range_error!("Invalid DataView length");

/// A get / set `ToIndex(requestIndex)` out of range, or an element past the
/// view's end.
const OFFSET_OUT_OF_BOUNDS: CalleeErrorText<'static> =
    range_error!("Offset is outside the bounds of the DataView");

/// `2^53 - 1`, the largest `ToIndex` result.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The static strings of this family (with the `DataView` renderer's).
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    OP_TEXTS
        .into_iter()
        .flat_map(|(_, receiver, oob)| receiver.into_iter().chain(oob))
        .chain(
            [
                NOT_BUFFER,
                CTOR_DETACHED,
                START_OFFSET,
                INVALID_LENGTH,
                OFFSET_OUT_OF_BOUNDS,
            ]
            .into_iter()
            .flatten(),
        )
        .chain(["prototype"])
        .chain(DV_STRINGS)
}

/// The `(incompatible receiver, out of bounds)` texts of `op`.
fn texts_of(op: DataViewOp) -> (CalleeErrorText<'static>, CalleeErrorText<'static>) {
    let name = op.name();
    let key = name.strip_prefix("get ").unwrap_or(name);
    OP_TEXTS
        .iter()
        .find(|entry| entry.0 == key)
        .map_or((OP_TEXTS[0].1, OP_TEXTS[0].2), |entry| (entry.1, entry.2))
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

/// Push the `u32` field at `offset` of the block jsval in i64 `block`.
fn field(function: &mut FnBuilder<'_>, block: u32, offset: u32) {
    wasm!(function; LocalGet(block), I32WrapI64, I32Load(m32(offset)));
}

/// Push the header flags of the block jsval in i64 `block` masked by `flag`
/// (nonzero when set).
fn flag(function: &mut FnBuilder<'_>, block: u32, flag: u8) {
    wasm!(function;
        LocalGet(block), I32WrapI64, I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
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

/// Throw a `RangeError` with `text` when the i32 on the stack is nonzero.
fn range_error_if(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; If(BlockType::Empty));
    throw_range_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Throw a `TypeError` with `text` when the i32 on the stack is nonzero.
fn type_error_if(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// `ToIndex` of the jsval in i64 `v` into f64 `out`: `ToIntegerOrInfinity`
/// (NaN is `0`; a throwing `valueOf` returns), then a `RangeError` with
/// `text` below `0` or above `2^53 - 1`.
fn to_index(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (v, out): (u32, u32),
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(v), Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function;
        F64ReinterpretI64, F64Trunc, LocalTee(out), LocalGet(out), F64Ne,
        If(BlockType::Empty), F64Const(Ieee64::from(0.0)), LocalSet(out), End,
        LocalGet(out), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(out), F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Gt, I32Or,
    );
    range_error_if(function, frame, text, strings, helpers);
}

/// Read `this` into i64 `this` and throw `text` unless it is a `DataView`.
fn this_view(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    this: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_kind(function, this, abi::TYPE_DATAVIEW);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, text, strings, helpers);
}

/// `__dv_length` of the view jsval in i64 `view` into the i32 local `len`,
/// throwing `text` when the view is out of bounds or detached.
fn view_length(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (view, len): (u32, u32),
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(view), I32WrapI64, Call(helpers.dv_length()), LocalTee(len),
        I32Const(0), I32LtS,
    );
    type_error_if(function, frame, text, strings, helpers);
}

/// Emit the body of the `DataView` builtin `builtin` into `f`.
pub(super) fn emit(
    builtin: Builtin,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::Ctor(_) => emit_ctor(f, frame, strings, helpers),
        Builtin::DataView(op @ (DataViewOp::Get(_) | DataViewOp::Set(_))) => {
            emit_access(op, f, frame, strings, helpers);
        }
        Builtin::DataView(op) => emit_getter(op, f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// `DataView(buffer, byteOffset, byteLength)` (DESIGN.md §5.8.7): the
/// "requires 'new'" `TypeError` when called; a non-`ArrayBuffer` `buffer`
/// throws; `ToIndex(byteOffset)`; a detached buffer throws; an offset past
/// the buffer is a `RangeError`; an `undefined` `byteLength` takes the rest
/// of a fixed buffer or tracks a resizable one, else `ToIndex(byteLength)`
/// and a view past the buffer is a `RangeError`; then
/// `GetPrototypeFromConstructor(new.target)` (saved on entry), after which
/// the buffer is re-checked (detached: `TypeError`; shrunk below the view:
/// `RangeError`). The arguments are read on entry, before any user code.
fn emit_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (nt, buffer, off_v, len_v, proto) = (
        frame.y64(0),
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
    );
    let (off, len, buffer_len) = (frame.zf64(0), frame.zf64(1), frame.zf64(2));
    let tracking = frame.w32(0);
    wasm!(function; GlobalGet(NEW_TARGET_GLOBAL), LocalSet(nt));
    // The IR reader decodes the construct marker in `argc` for an argument
    // past the `K` slots.
    for (i, local) in [(0, buffer), (1, off_v), (2, len_v)] {
        emit_node(
            &frame.body().arg(i),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; LocalSet(local));
    }
    let requires_new = super::ctor::fragment(BuiltinCtor::DataView, frame.k);
    emit_drop(function, frame, &requires_new, strings, helpers);
    return_if_pending(function);
    is_kind(function, buffer, abi::TYPE_ARRAYBUFFER);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, NOT_BUFFER, strings, helpers);
    to_index(
        function,
        frame,
        (off_v, off),
        START_OFFSET,
        strings,
        helpers,
    );
    // The bounds against the buffer's current state (before and again after
    // `GetPrototypeFromConstructor`, which can run user code).
    let check_buffer = |function: &mut FnBuilder<'_>| {
        flag(function, buffer, abi::AB_FLAG_DETACHED);
        type_error_if(function, frame, CTOR_DETACHED, strings, helpers);
        field(function, buffer, abi::AB_BYTE_LENGTH_OFFSET);
        wasm!(function;
            F64ConvertI32U, LocalSet(buffer_len),
            LocalGet(off), LocalGet(buffer_len), F64Gt,
        );
        range_error_if(function, frame, START_OFFSET, strings, helpers);
    };
    check_buffer(function);
    wasm!(function;
        I32Const(0), LocalSet(tracking),
        LocalGet(len_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
    );
    flag(function, buffer, abi::AB_FLAG_RESIZABLE);
    wasm!(function;
        If(BlockType::Empty),
        I32Const(1), LocalSet(tracking), F64Const(Ieee64::from(0.0)), LocalSet(len),
        Else,
        LocalGet(buffer_len), LocalGet(off), F64Sub, LocalSet(len),
        End,
        Else,
    );
    to_index(
        function,
        frame,
        (len_v, len),
        INVALID_LENGTH,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(off), LocalGet(len), F64Add, LocalGet(buffer_len), F64Gt);
    range_error_if(function, frame, INVALID_LENGTH, strings, helpers);
    wasm!(function; End);
    // GetPrototypeFromConstructor(new.target, %DataView.prototype%).
    let realm_proto = intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::DataView), helpers);
    wasm!(function;
        LocalGet(nt), I32Const(static_key(strings, "prototype")), Call(helpers.get),
        LocalSet(proto),
    );
    return_if_pending(function);
    emit_is_object(function, proto);
    wasm!(function; I32Eqz, If(BlockType::Empty), I64Const(realm_proto), LocalSet(proto), End);
    check_buffer(function);
    wasm!(function;
        LocalGet(len_v), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Result(ValType::I32)),
        LocalGet(off), LocalGet(len), F64Add, LocalGet(buffer_len), F64Gt,
        Else, I32Const(0), End,
    );
    range_error_if(function, frame, INVALID_LENGTH, strings, helpers);
    new_view(function, frame, (buffer, proto, off, len), helpers);
}

/// The end of the constructor: allocate the view over the buffer jsval in
/// i64 `buffer` with the prototype in i64 `proto`, the f64 offset `off` and
/// length `len`, length-tracking when the i32 local `frame.w32(0)` is set,
/// and push it.
fn new_view(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (buffer, proto, off, len): (u32, u32, u32, u32),
    helpers: Helpers<'_>,
) {
    let (tracking, flags) = (frame.w32(0), frame.w32(1));
    wasm!(function;
        I32Const(i32::from(abi::DV_FLAG_LENGTH_TRACKING)), I32Const(0), LocalGet(tracking), Select,
    );
    flag(function, buffer, abi::AB_FLAG_RESIZABLE);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        I32Const(i32::from(abi::DV_FLAG_RESIZABLE_BUFFER)),
        Else, I32Const(0), End,
        I32Or, LocalSet(flags),
    );
    emit_alloc_const(function, helpers, abi::DV_BLOCK_SIZE.cast_signed());
    let base = frame.base(0);
    wasm!(function;
        LocalTee(base),
        LocalGet(flags), I32Const(16), I32Shl, I32Const(i32::from(abi::TYPE_DATAVIEW)), I32Or,
        I32Store(m32(0)),
        LocalGet(base), LocalGet(proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(base), LocalGet(buffer), I32WrapI64, I32Store(m32(abi::DV_BUFFER_OFFSET)),
        LocalGet(base), LocalGet(off), I32TruncF64U, I32Store(m32(abi::DV_BYTE_OFFSET_OFFSET)),
        // A length-tracking view keeps `byte_length` at `0`.
        LocalGet(base),
        LocalGet(len), I32TruncF64U, I32Const(0), LocalGet(tracking), I32Eqz, Select,
        I32Store(m32(abi::DV_BYTE_LENGTH_OFFSET)),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// The `buffer` / `byteLength` / `byteOffset` getters: the receiver check;
/// `buffer` returns the buffer even when detached, the other two throw when
/// the view is out of bounds or detached.
fn emit_getter(
    op: DataViewOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.y64(0);
    let len = frame.w32(0);
    let (receiver, oob) = texts_of(op);
    this_view(function, frame, this, receiver, strings, helpers);
    if op == DataViewOp::Buffer {
        field(function, this, abi::DV_BUFFER_OFFSET);
        wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
        return;
    }
    view_length(function, frame, (this, len), oob, strings, helpers);
    if op == DataViewOp::ByteOffset {
        field(function, this, abi::DV_BYTE_OFFSET_OFFSET);
    } else {
        wasm!(function; LocalGet(len));
    }
    wasm!(function; F64ConvertI32U, I64ReinterpretF64);
}

/// `GetViewValue` / `SetViewValue` (DESIGN.md §5.8.7) for a get / set
/// method: the receiver check, `ToIndex(requestIndex)`, a setter's
/// `ToNumber(value)` (`ToBigInt(value)` for a `BigInt` kind),
/// `ToBoolean(littleEndian)`, then an out-of-bounds or
/// detached view throws a `TypeError`, an element past the view's end a
/// `RangeError`, else `__raw_load` / `__raw_store` at `data + byte_offset +
/// index` with `swap = !littleEndian`. The arguments are read on entry,
/// before any user code runs.
fn emit_access(
    op: DataViewOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (set, kind) = match op {
        DataViewOp::Set(kind) => (true, kind),
        DataViewOp::Get(kind) => (false, kind),
        _ => return,
    };
    let size = kind.size();
    let (this, index_v, value_v, little_v, value) = (
        frame.y64(0),
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
    );
    let index = frame.zf64(0);
    let (little, len) = (frame.w32(0), frame.w32(1));
    let (receiver, oob) = texts_of(op);
    this_view(function, frame, this, receiver, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(index_v));
    if set {
        frame.arg(function, 1);
        wasm!(function; LocalSet(value_v));
        frame.arg(function, 2);
    } else {
        frame.arg(function, 1);
    }
    wasm!(function; LocalSet(little_v));
    to_index(
        function,
        frame,
        (index_v, index),
        OFFSET_OUT_OF_BOUNDS,
        strings,
        helpers,
    );
    if set {
        let convert = if kind.is_bigint() {
            helpers.bigint(Big::ToBigInt)
        } else {
            helpers.to_number
        };
        wasm!(function; LocalGet(value_v), Call(convert), LocalSet(value));
        return_if_pending(function);
    }
    let truthy = IrNode::ConvertCall {
        op: ConvertOp::ToBoolean,
        arg: Box::new(IrNode::Local(little_v)),
        radix: None,
    };
    emit_node(&truthy, function, frame.scratch(), strings, helpers);
    wasm!(function; I64Const(TRUE_BITS), I64Eq, LocalSet(little));
    view_length(function, frame, (this, len), oob, strings, helpers);
    wasm!(function;
        LocalGet(index), F64Const(Ieee64::from(f64::from(size))), F64Add,
        LocalGet(len), F64ConvertI32U, F64Gt,
    );
    range_error_if(function, frame, OFFSET_OUT_OF_BOUNDS, strings, helpers);
    // addr = buffer.data + RAW_DATA + byte_offset + index.
    field(function, this, abi::DV_BUFFER_OFFSET);
    wasm!(function;
        I32Load(m32(abi::AB_DATA_OFFSET)), I32Const(RAW_DATA.cast_signed()), I32Add,
    );
    field(function, this, abi::DV_BYTE_OFFSET_OFFSET);
    wasm!(function;
        I32Add,
        LocalGet(index), I32TruncF64U, I32Add,
        I32Const(i32::from(kind.kind())),
    );
    if set {
        wasm!(function;
            LocalGet(value), LocalGet(little), I32Eqz, Call(helpers.raw_store()),
            I64Const(UNDEFINED_BITS),
        );
    } else {
        wasm!(function; LocalGet(little), I32Eqz, Call(helpers.raw_load()));
    }
}
