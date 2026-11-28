//! Typed arrays (DESIGN.md §5.8): the concrete constructors (`Int8Array` ..
//! `Float64Array`, every overload of `TypedArray(...args)`), the
//! `%TypedArray%` statics `from` / `of`, and the `%TypedArray.prototype%`
//! getters (`buffer`, `byteLength`, `byteOffset`, `length`,
//! `[@@toStringTag]`) and iterator methods (`keys`, `values`, `entries`).
//!
//! A typed array is a [`abi::TYPE_TYPEDARRAY`] block viewing a
//! [`abi::TYPE_ARRAYBUFFER`] block; it never caches a data address and reads
//! its buffer's state through the witness helpers (`__ta_length`,
//! `__ta_oob`) on every access. Every element read or write goes through
//! `__ta_get` / `__ta_set` / `__ta_set_value`, which re-check the index, so a
//! user conversion that detaches or shrinks the buffer can never make a
//! body touch bytes outside the live range.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::emit::binary::RAW_DATA;
use crate::emit::realm::{IterFamily, IterKind, TypedArrayOp};

/// `[message, stack]` of a `TypeError` with `message`.
macro_rules! type_error {
    ($message:expr) => {
        [$message, concat!("TypeError: ", $message)]
    };
}

/// `[message, stack]` of a `RangeError` with `message`.
macro_rules! range_error {
    ($message:expr) => {
        [$message, concat!("RangeError: ", $message)]
    };
}

/// The per-kind `RangeError`s of the `ArrayBuffer` overload, in
/// [`ElementKind::ALL`] order: `[byte offset misaligned, byte length
/// misaligned]`.
macro_rules! alignment_errors {
    ($($name:literal => $size:literal),* $(,)?) => {
        [$([
            range_error!(concat!("Start offset of ", $name, " should be a multiple of ", $size)),
            range_error!(concat!("Byte length of ", $name, " should be a multiple of ", $size)),
        ]),*]
    };
}

/// The alignment `RangeError`s of each kind (DESIGN.md §5.8.7).
const ALIGNMENT: [[CalleeErrorText<'static>; 2]; 9] = alignment_errors![
    "Int8Array" => "1",
    "Uint8Array" => "1",
    "Uint8ClampedArray" => "1",
    "Int16Array" => "2",
    "Uint16Array" => "2",
    "Int32Array" => "4",
    "Uint32Array" => "4",
    "Float32Array" => "4",
    "Float64Array" => "8",
];

/// A length that is not a valid index, or whose byte length cannot be
/// allocated or does not fit the buffer.
const INVALID_LENGTH: CalleeErrorText<'static> = range_error!("Invalid typed array length");

/// A byte offset that is not a valid index or lies past the buffer.
const OFFSET_OUT_OF_BOUNDS: CalleeErrorText<'static> =
    range_error!("Start offset is outside the bounds of the buffer");

/// The `ArrayBuffer` overload over a detached buffer.
const DETACHED_BUFFER: CalleeErrorText<'static> =
    type_error!("Cannot perform Construct on a detached ArrayBuffer");

/// The typed-array overload over an out-of-bounds or detached source.
const SOURCE_OUT_OF_BOUNDS: CalleeErrorText<'static> =
    type_error!("Cannot perform Construct on a detached or out-of-bounds TypedArray");

/// `from` / `of` with a `this` that is not a constructor.
const FROM_NOT_CONSTRUCTOR: CalleeErrorText<'static> =
    type_error!("%TypedArray%.from: this is not a constructor");

/// See [`FROM_NOT_CONSTRUCTOR`].
const OF_NOT_CONSTRUCTOR: CalleeErrorText<'static> =
    type_error!("%TypedArray%.of: this is not a constructor");

/// `from` with a `mapFn` that is neither `undefined` nor callable.
const FROM_NOT_CALLABLE: CalleeErrorText<'static> =
    type_error!("%TypedArray%.from: when provided, the second argument must be a function");

/// `from` over `null` / `undefined`.
pub(super) const FROM_NULLISH: CalleeErrorText<'static> =
    type_error!("Cannot convert undefined or null to object");

/// `TypedArrayCreateFromConstructor`: the constructor's result is not a
/// typed array.
const CREATE_NOT_TYPED_ARRAY: CalleeErrorText<'static> =
    type_error!("The TypedArray constructor did not return a TypedArray");

/// `TypedArrayCreateFromConstructor`: the result is out of bounds or
/// detached.
const CREATE_OUT_OF_BOUNDS: CalleeErrorText<'static> =
    type_error!("The TypedArray constructor returned a detached or out-of-bounds TypedArray");

/// `TypedArrayCreateFromConstructor`: the result is shorter than requested.
const CREATE_TOO_SHORT: CalleeErrorText<'static> =
    type_error!("The TypedArray constructor returned a too short TypedArray");

/// The fixed `TypeError` and `RangeError` texts of this family.
const TEXTS: [CalleeErrorText<'static>; 11] = [
    INVALID_LENGTH,
    OFFSET_OUT_OF_BOUNDS,
    DETACHED_BUFFER,
    SOURCE_OUT_OF_BOUNDS,
    FROM_NOT_CONSTRUCTOR,
    OF_NOT_CONSTRUCTOR,
    FROM_NOT_CALLABLE,
    FROM_NULLISH,
    CREATE_NOT_TYPED_ARRAY,
    CREATE_OUT_OF_BOUNDS,
    CREATE_TOO_SHORT,
];

/// The source name of `Construct(C, [len])` in `TypedArrayCreateFromConstructor`.
const CREATE_CALLEE: &str = "this";

/// The name each prototype builtin reports in its receiver `TypeError`s
/// (the `__prop_throw` / `__ta_validate` key).
const RECEIVER_NAMES: [(TypedArrayOp, &str); 7] = [
    (TypedArrayOp::Buffer, "get %TypedArray%.prototype.buffer"),
    (
        TypedArrayOp::ByteLength,
        "get %TypedArray%.prototype.byteLength",
    ),
    (
        TypedArrayOp::ByteOffset,
        "get %TypedArray%.prototype.byteOffset",
    ),
    (TypedArrayOp::Length, "get %TypedArray%.prototype.length"),
    (TypedArrayOp::Keys, "%TypedArray%.prototype.keys"),
    (TypedArrayOp::Values, "%TypedArray%.prototype.values"),
    (TypedArrayOp::Entries, "%TypedArray%.prototype.entries"),
];

/// The name of `%ArrayIteratorPrototype%.next` in its out-of-bounds
/// `TypeError` (DESIGN.md §5.8.7).
pub(super) const ITERATOR_NEXT: &str = "%ArrayIteratorPrototype%.next";

/// `2^53 - 1`, the largest `ToIndex` / `ToLength` result.
pub(super) const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    TEXTS
        .into_iter()
        .chain(ALIGNMENT.into_iter().flatten())
        .chain(SPECIES_TEXTS)
        .flatten()
        .chain(RECEIVER_NAMES.map(|(_, name)| name))
        .chain([ITERATOR_NEXT, "prototype", "length", "constructor", ""])
        .chain(ElementKind::ALL.map(ElementKind::name))
}

/// The IR fragments the bodies emit with `K = k`, for the static-string
/// scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame::new(k);
    vec![
        construct(frame.i64(0), &[frame.i64(1)], frame),
        construct(frame.i64(0), &[frame.i64(1), frame.i64(2)], frame),
        construct(
            frame.i64(0),
            &[frame.i64(1), frame.i64(2), frame.i64(3)],
            frame,
        ),
    ]
}

/// The receiver name of `op` (the first one when absent).
fn receiver_name(op: TypedArrayOp) -> &'static str {
    RECEIVER_NAMES
        .iter()
        .find_map(|&(o, name)| (o == op).then_some(name))
        .unwrap_or(RECEIVER_NAMES[0].1)
}

/// A byte [`MemArg`] at `offset`.
pub(super) fn m8(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

/// A `u16` [`MemArg`] at `offset`.
fn m16(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 1,
        memory_index: 0,
    }
}

/// A `u32` [`MemArg`] at `offset`.
pub(super) fn m32(offset: u32) -> MemArg {
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

/// Push the `u32` field at `offset` of the block whose jsval is in i64
/// `local`.
pub(super) fn field(function: &mut FnBuilder<'_>, local: u32, offset: u32) {
    wasm!(function; LocalGet(local), I32WrapI64, I32Load(m32(offset)));
}

/// Push `1` when the jsval in `local` is a block of type byte `kind`.
pub(super) fn is_kind(function: &mut FnBuilder<'_>, local: u32, kind: u8) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(i32::from(kind)), I32Eq, Else, I32Const(0), End);
}

/// Throw a `RangeError` with `text` and return.
pub(super) fn throw_range_error(
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

/// Throw the `RangeError` `text` when the i32 condition on the stack holds.
pub(super) fn range_error_if(
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

/// Throw the `TypeError` `text` when the i32 condition on the stack holds.
pub(super) fn type_error_if(
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

/// `ToIntegerOrInfinity` of the jsval in i64 `v` into f64 `out` (NaN is
/// `0`; a throwing `valueOf` returns).
pub(super) fn to_integer(function: &mut FnBuilder<'_>, v: u32, out: u32, helpers: Helpers<'_>) {
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
    );
    range_error_if(function, frame, text, strings, helpers);
}

/// `GetPrototypeFromConstructor(newTarget, %<Kind>.prototype%)` of the
/// `new.target` in i64 `nt` into i64 `proto`: `Get(newTarget,
/// "prototype")` (a throw returns), the realm prototype of `kind` when that
/// is not an object.
fn proto_from_ctor(
    function: &mut FnBuilder<'_>,
    kind: ElementKind,
    (nt, proto): (u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let realm_proto = intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Typed(kind)), helpers);
    wasm!(function;
        LocalGet(nt), I32Const(static_key(strings, "prototype")), Call(helpers.get),
        LocalSet(proto),
    );
    return_if_pending(function);
    emit_is_object(function, proto);
    wasm!(function; I32Eqz, If(BlockType::Empty), I64Const(realm_proto), LocalSet(proto), End);
}

/// `AllocateArrayBuffer(%ArrayBuffer%, n)` for the byte length in i32 `n`
/// into i64 `out`: a fixed-length buffer over `%ArrayBuffer.prototype%`
/// whose zeroed byte block comes from `__ab_alloc` (out of memory returns
/// with the `RangeError` pending). `data` is i32 scratch.
fn new_buffer(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (n, data, out): (u32, u32, u32),
    helpers: Helpers<'_>,
) {
    let base = frame.base(0);
    let proto = intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::ArrayBuffer), helpers);
    emit_alloc_const(function, helpers, abi::AB_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalTee(base),
        I32Const(i32::from(abi::TYPE_ARRAYBUFFER)), I32Store(m32(0)),
        LocalGet(base), I64Const(proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
        LocalGet(n), Call(helpers.ab_alloc()), LocalSet(data),
    );
    return_if_pending(function);
    for (offset, value) in [
        (abi::AB_DATA_OFFSET, data),
        (abi::AB_BYTE_LENGTH_OFFSET, n),
        (abi::AB_MAX_BYTE_LENGTH_OFFSET, n),
    ] {
        wasm!(function; LocalGet(out), I32WrapI64, LocalGet(value), I32Store(m32(offset)));
    }
}

/// The locals of a fresh typed array's fields.
#[derive(Clone, Copy)]
struct View {
    /// The `ArrayBuffer` jsval (i64).
    buffer: u32,
    /// The prototype jsval (i64).
    proto: u32,
    /// The header flags (i32).
    flags: u32,
    /// The byte offset (i32).
    offset: u32,
    /// The array length (i32; ignored when length-tracking).
    length: u32,
}

/// A fresh `kind` typed array over `view` into i64 `out` (DESIGN.md
/// §5.8.3).
fn new_typed_array(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    kind: ElementKind,
    view: View,
    out: u32,
    helpers: Helpers<'_>,
) {
    let base = frame.base(0);
    let kind_shift = i32::from(kind.kind()) | (i32::from(kind.shift()) << 8);
    emit_alloc_const(function, helpers, abi::TA_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalTee(base),
        LocalGet(view.flags), I32Const(16), I32Shl, I32Const(i32::from(abi::TYPE_TYPEDARRAY)), I32Or,
        I32Store(m32(0)),
        LocalGet(base), LocalGet(view.proto), I64Store(m64(abi::OBJ_PROTO_OFFSET)),
        LocalGet(base), LocalGet(view.buffer), I32WrapI64, I32Store(m32(abi::TA_BUFFER_OFFSET)),
        LocalGet(base), I32Const(kind_shift), I32Store16(m16(abi::TA_KIND_OFFSET)),
        LocalGet(base), LocalGet(view.offset), I32Store(m32(abi::TA_BYTE_OFFSET_OFFSET)),
        LocalGet(base), LocalGet(view.length), I32Store(m32(abi::TA_LENGTH_OFFSET)),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
    );
}

/// The locals of a constructor body.
#[derive(Clone, Copy)]
struct Ctor {
    frame: Frame,
    kind: ElementKind,
    /// `new.target` (i64).
    nt: u32,
    /// The three leading arguments (i64).
    args: [u32; 3],
    /// The prototype (i64).
    proto: u32,
    /// The new buffer (i64).
    buffer: u32,
    /// The new typed array (i64).
    out: u32,
    /// i64 temps.
    v: u32,
    method: u32,
    /// f64 temps: a length, a byte offset, a buffer length.
    len: u32,
    off: u32,
    total: u32,
    /// i32 temps.
    n: u32,
    flags: u32,
    offset: u32,
    bytes: u32,
    k: u32,
    data: u32,
    /// A rooted i32: the drained list.
    list: u32,
}

impl Ctor {
    fn new(frame: Frame, kind: ElementKind) -> Self {
        Self {
            frame,
            kind,
            nt: frame.y64(0),
            args: [frame.y64(1), frame.y64(2), frame.y64(3)],
            proto: frame.y64(4),
            buffer: frame.y64(5),
            out: frame.x64(0),
            v: frame.x64(1),
            method: frame.i64(0),
            len: frame.zf64(0),
            off: frame.zf64(1),
            total: frame.zf64(2),
            n: frame.w32(0),
            flags: frame.w32(1),
            offset: frame.w32(2),
            bytes: frame.w32(3),
            k: frame.w32(4),
            data: frame.w32(5),
            list: frame.base(1),
        }
    }

    /// The view of the buffer in `buffer` with the current field locals.
    fn view(self) -> View {
        View {
            buffer: self.buffer,
            proto: self.proto,
            flags: self.flags,
            offset: self.offset,
            length: self.n,
        }
    }

    /// `AllocateTypedArray` with the length in f64 `len` (an integer
    /// `>= 0`): a byte length past [`abi::BYTE_LENGTH_MAX`] is the
    /// `RangeError`; else a fresh zeroed buffer and a fixed-length view over
    /// all of it into `out`.
    fn allocate(self, function: &mut FnBuilder<'_>, strings: &StrOffsets, helpers: Helpers<'_>) {
        let size = f64::from(self.kind.size());
        let max = f64::from(abi::BYTE_LENGTH_MAX);
        wasm!(function;
            LocalGet(self.len), F64Const(Ieee64::from(size)), F64Mul, F64Const(Ieee64::from(max)), F64Gt,
        );
        range_error_if(function, self.frame, INVALID_LENGTH, strings, helpers);
        wasm!(function;
            LocalGet(self.len), I32TruncF64U, LocalTee(self.n),
            I32Const(i32::from(self.kind.shift())), I32Shl, LocalSet(self.bytes),
        );
        new_buffer(
            function,
            self.frame,
            (self.bytes, self.data, self.buffer),
            helpers,
        );
        wasm!(function;
            I32Const(0), LocalSet(self.flags),
            I32Const(0), LocalSet(self.offset),
        );
        new_typed_array(
            function,
            self.frame,
            self.kind,
            self.view(),
            self.out,
            helpers,
        );
    }
}

/// Emit the body of the typed-array builtin `builtin` into `f`.
pub(super) fn emit(
    builtin: Builtin,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::Ctor(BuiltinCtor::Typed(kind)) => emit_ctor(kind, f, frame, strings, helpers),
        Builtin::TypedArray(TypedArrayOp::From) => emit_from(f, frame, strings, helpers),
        Builtin::TypedArray(TypedArrayOp::Of) => emit_of(f, frame, strings, helpers),
        Builtin::TypedArray(TypedArrayOp::Keys) => {
            emit_iter(
                TypedArrayOp::Keys,
                IterKind::Keys,
                f,
                frame,
                strings,
                helpers,
            );
        }
        Builtin::TypedArray(TypedArrayOp::Values) => {
            emit_iter(
                TypedArrayOp::Values,
                IterKind::Values,
                f,
                frame,
                strings,
                helpers,
            );
        }
        Builtin::TypedArray(TypedArrayOp::Entries) => {
            emit_iter(
                TypedArrayOp::Entries,
                IterKind::Entries,
                f,
                frame,
                strings,
                helpers,
            );
        }
        Builtin::TypedArray(TypedArrayOp::ToStringTag) => emit_to_string_tag(f, frame, strings),
        Builtin::TypedArray(op) => emit_getter(op, f, frame, strings, helpers),
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// `<Kind>Array(...args)` (DESIGN.md §5.8.7): the "requires 'new'"
/// `TypeError` when called; then by the first argument — an object takes
/// `GetPrototypeFromConstructor` first and is a typed array (a copy of its
/// current elements), an `ArrayBuffer` (a view) or any other object (its
/// `@@iterator` drained to a list, else an array-like); anything else is
/// `ToIndex(length)`, then the prototype, then a fresh zeroed buffer. The
/// arguments are read on entry, before any user code runs.
fn emit_ctor(
    kind: ElementKind,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let c = Ctor::new(frame, kind);
    wasm!(function; GlobalGet(NEW_TARGET_GLOBAL), LocalSet(c.nt));
    for (i, local) in (0..).zip(c.args) {
        emit_node(
            &frame.body().arg(i),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; LocalSet(local));
    }
    let requires_new = super::ctor::fragment(BuiltinCtor::Typed(kind), frame.k);
    emit_drop(function, frame, &requires_new, strings, helpers);
    return_if_pending(function);
    let first = c.args[0];
    emit_is_object(function, first);
    wasm!(function; If(BlockType::Empty));
    proto_from_ctor(function, kind, (c.nt, c.proto), strings, helpers);
    is_kind(function, first, abi::TYPE_TYPEDARRAY);
    wasm!(function; If(BlockType::Empty));
    from_typed_array(function, c, strings, helpers);
    wasm!(function; End);
    is_kind(function, first, abi::TYPE_ARRAYBUFFER);
    wasm!(function; If(BlockType::Empty));
    from_buffer(function, c, strings, helpers);
    wasm!(function; End);
    from_object(function, c, strings, helpers);
    wasm!(function; End);
    to_index(
        function,
        frame,
        (first, c.len),
        INVALID_LENGTH,
        strings,
        helpers,
    );
    proto_from_ctor(function, kind, (c.nt, c.proto), strings, helpers);
    c.allocate(function, strings, helpers);
    wasm!(function; LocalGet(c.out));
}

/// `InitializeTypedArrayFromTypedArray`: an out-of-bounds or detached source
/// throws; a fresh buffer of the source's current length, filled by one
/// `memory.copy` when the kinds match, else element by element (each read
/// and converted by the source's and the target's kind). Returns `out`.
fn from_typed_array(
    function: &mut FnBuilder<'_>,
    c: Ctor,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let src = c.args[0];
    wasm!(function; LocalGet(src), I32WrapI64, Call(helpers.ta_oob()));
    type_error_if(function, c.frame, SOURCE_OUT_OF_BOUNDS, strings, helpers);
    wasm!(function;
        LocalGet(src), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, LocalSet(c.len),
    );
    c.allocate(function, strings, helpers);
    // No user code ran since the witness: the source is unchanged.
    wasm!(function;
        LocalGet(src), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)),
        I32Const(i32::from(c.kind.kind())), I32Eq,
        If(BlockType::Empty),
    );
    field(function, c.buffer, abi::AB_DATA_OFFSET);
    wasm!(function; I32Const(RAW_DATA.cast_signed()), I32Add);
    field(function, src, abi::TA_BUFFER_OFFSET);
    wasm!(function;
        I32Load(m32(abi::AB_DATA_OFFSET)), I32Const(RAW_DATA.cast_signed()), I32Add,
    );
    field(function, src, abi::TA_BYTE_OFFSET_OFFSET);
    wasm!(function;
        I32Add,
        LocalGet(c.bytes),
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        Else,
        I32Const(0), LocalSet(c.k),
    );
    count_loop(function, c.k, c.n, |function| {
        wasm!(function;
            LocalGet(c.out), I32WrapI64, LocalGet(c.k),
            LocalGet(src), I32WrapI64, LocalGet(c.k), Call(helpers.ta_get()),
            Call(helpers.ta_set()),
        );
    });
    wasm!(function; End, LocalGet(c.out), Return);
}

/// `InitializeTypedArrayFromArrayBuffer`: `ToIndex(byteOffset)` (a
/// misaligned one is a `RangeError`), `ToIndex(length)` unless `undefined`,
/// then a detached buffer is a `TypeError`; a resizable buffer with an
/// `undefined` length gives a length-tracking view (an offset past the
/// buffer is a `RangeError`), anything else a fixed view whose bytes must
/// lie inside the buffer. Returns `out`.
fn from_buffer(function: &mut FnBuilder<'_>, c: Ctor, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [buffer, offset_v, length_v] = c.args;
    let [misaligned_offset, misaligned_length] = ALIGNMENT
        .get(
            ElementKind::ALL
                .iter()
                .position(|k| *k == c.kind)
                .unwrap_or(0),
        )
        .copied()
        .unwrap_or(ALIGNMENT[0]);
    let size = f64::from(c.kind.size());
    to_index(
        function,
        c.frame,
        (offset_v, c.off),
        OFFSET_OUT_OF_BOUNDS,
        strings,
        helpers,
    );
    // offset modulo size (exact: `size` is a power of two).
    wasm!(function;
        LocalGet(c.off), LocalGet(c.off), F64Const(Ieee64::from(1.0 / size)), F64Mul, F64Floor,
        F64Const(Ieee64::from(size)), F64Mul, F64Ne,
    );
    range_error_if(function, c.frame, misaligned_offset, strings, helpers);
    wasm!(function;
        LocalGet(length_v), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
    );
    to_index(
        function,
        c.frame,
        (length_v, c.len),
        INVALID_LENGTH,
        strings,
        helpers,
    );
    wasm!(function; End);
    wasm!(function;
        LocalGet(buffer), I32WrapI64, I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::AB_FLAG_DETACHED)), I32And,
    );
    type_error_if(function, c.frame, DETACHED_BUFFER, strings, helpers);
    field(function, buffer, abi::AB_BYTE_LENGTH_OFFSET);
    wasm!(function;
        F64ConvertI32U, LocalSet(c.total),
        // flags = RESIZABLE_BUFFER when the buffer is resizable.
        LocalGet(buffer), I32WrapI64, I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::AB_FLAG_RESIZABLE)), I32And,
        I32Const(0), I32Ne, I32Const(i32::from(abi::TA_FLAG_RESIZABLE_BUFFER)), I32Mul,
        LocalSet(c.flags),
        LocalGet(length_v), I64Const(UNDEFINED_BITS), I64Eq,
        LocalGet(c.flags), I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
        // A length-tracking view.
        LocalGet(c.off), LocalGet(c.total), F64Gt,
    );
    range_error_if(function, c.frame, OFFSET_OUT_OF_BOUNDS, strings, helpers);
    wasm!(function;
        LocalGet(c.flags), I32Const(i32::from(abi::TA_FLAG_LENGTH_TRACKING)), I32Or,
        LocalSet(c.flags),
        I32Const(0), LocalSet(c.n),
        Else,
        LocalGet(length_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
        // The rest of the buffer, which must be a whole number of elements.
        LocalGet(c.total), LocalGet(c.total), F64Const(Ieee64::from(1.0 / size)), F64Mul, F64Floor,
        F64Const(Ieee64::from(size)), F64Mul, F64Ne,
    );
    range_error_if(function, c.frame, misaligned_length, strings, helpers);
    wasm!(function; LocalGet(c.off), LocalGet(c.total), F64Gt);
    range_error_if(function, c.frame, OFFSET_OUT_OF_BOUNDS, strings, helpers);
    wasm!(function;
        LocalGet(c.total), LocalGet(c.off), F64Sub, F64Const(Ieee64::from(1.0 / size)), F64Mul,
        I32TruncF64U, LocalSet(c.n),
        Else,
        // `length` elements, which must lie inside the buffer.
        LocalGet(c.off), LocalGet(c.len), F64Const(Ieee64::from(size)), F64Mul, F64Add,
        LocalGet(c.total), F64Gt,
    );
    range_error_if(function, c.frame, INVALID_LENGTH, strings, helpers);
    wasm!(function;
        LocalGet(c.len), I32TruncF64U, LocalSet(c.n),
        End,
        End,
        LocalGet(c.off), I32TruncF64U, LocalSet(c.offset),
        LocalGet(buffer), LocalSet(c.buffer),
    );
    new_typed_array(function, c.frame, c.kind, c.view(), c.out, helpers);
    wasm!(function; LocalGet(c.out), Return);
}

/// Drain the iterator `GetIteratorFromMethod(obj, method)` of the jsvals in
/// i64 `obj` / `method` (clobbered) into a fresh array in the rooted i32
/// `list` (`IteratorToList`; a throw propagates, the record left done).
fn drain(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (obj, method, v, list): (u32, u32, u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let rec = super::iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    super::iter::emit_iter_open_with(function, obj, method, &rec, scratch, strings, helpers);
    wasm!(function;
        I32Const(0), Call(helpers.new_array()), LocalSet(list),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    enter_frame(helpers);
    enter_frame(helpers);
    emit_iterator_step(&rec, function, scratch, strings, helpers);
    wasm!(function; I32Eqz, BrIf(1));
    emit_iterator_value(&rec, function, scratch, strings, helpers);
    wasm!(function;
        LocalSet(v),
        LocalGet(list), LocalGet(v), Call(helpers.array_push), Drop,
        Br(0),
    );
    leave_frame(helpers);
    leave_frame(helpers);
    wasm!(function; End, End);
}

/// Push element `k` (i32 local) of the array whose rooted base is in i32
/// `list`.
pub(super) fn list_element(function: &mut FnBuilder<'_>, list: u32, k: u32) {
    wasm!(function; LocalGet(list));
    emit_array_elems(function);
    wasm!(function; LocalGet(k), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)));
}

/// `ToLength(Get(obj, "length"))` of the object in i64 `obj` into f64
/// `out` (`v` is i64 scratch; a throw returns).
pub(super) fn length_of_array_like(
    function: &mut FnBuilder<'_>,
    (obj, v, out): (u32, u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(obj), I32Const(static_key(strings, "length")), Call(helpers.get), LocalSet(v),
    );
    return_if_pending(function);
    to_integer(function, v, out, helpers);
    wasm!(function;
        LocalGet(out), F64Const(Ieee64::from(0.0)), F64Max,
        F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Min, LocalSet(out),
    );
}

/// Push `Get(obj, k)` for the object in i64 `obj` and the i32 index `k`.
pub(super) fn get_index(function: &mut FnBuilder<'_>, obj: u32, k: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(obj));
    let mark = function.temp_mark();
    function.root_temp(TempClass::Jsval);
    wasm!(function;
        LocalGet(k), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
        Call(helpers.get),
    );
    function.release_temps(mark);
}

/// The object overload: `GetMethod(obj, @@iterator)`; an iterator is
/// drained to a list first (`InitializeTypedArrayFromList`), else the object
/// is an array-like (`InitializeTypedArrayFromArrayLike`: `length`, the
/// allocation, then `Get` of each index). Each element is stored by
/// `TypedArraySetElement` in index order. Returns `out`.
fn from_object(function: &mut FnBuilder<'_>, c: Ctor, strings: &StrOffsets, helpers: Helpers<'_>) {
    let obj = c.args[0];
    wasm!(function;
        LocalGet(obj), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(c.method),
    );
    return_if_pending(function);
    emit_is_nullish(function, c.method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    drain(
        function,
        c.frame,
        (obj, c.method, c.v, c.list),
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(c.list), I32Load(m32(abi::ARRAY_LEN_OFFSET)), F64ConvertI32U, LocalSet(c.len),
    );
    c.allocate(function, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(c.k));
    count_loop(function, c.k, c.n, |function| {
        list_element(function, c.list, c.k);
        wasm!(function;
            LocalSet(c.v),
            LocalGet(c.out), I32WrapI64, LocalGet(c.k), LocalGet(c.v), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; LocalGet(c.out), Return, End);
    length_of_array_like(function, (obj, c.v, c.len), strings, helpers);
    c.allocate(function, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(c.k));
    count_loop(function, c.k, c.n, |function| {
        get_index(function, obj, c.k, helpers);
        wasm!(function; LocalSet(c.v));
        return_if_pending(function);
        wasm!(function;
            LocalGet(c.out), I32WrapI64, LocalGet(c.k), LocalGet(c.v), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; LocalGet(c.out), Return);
}

/// `Construct(c, args)` for the constructor in i64 `c` and the jsvals in
/// the i64 locals `args`; with fewer positional slots than arguments (`K <
/// args.len()`) they all go in a spread argument array.
fn construct(c: u32, args: &[u32], frame: Frame) -> IrNode {
    let args: Vec<IrNode> = args.iter().map(|&a| IrNode::Local(a)).collect();
    let (args, spread) = if (frame.k as usize) < args.len() {
        let spread = IrNode::ArraySpread(args.into_iter().map(SpreadElem::Item).collect());
        (Vec::new(), Some(Box::new(spread)))
    } else {
        (args, None)
    };
    IrNode::Construct {
        callee: Box::new(IrNode::Local(c)),
        args,
        spread,
        callee_local: frame.z64(2),
        this_local: frame.z64(3),
        reserve: 0,
        not_constructor: CalleeError::not_a_constructor(CREATE_CALLEE),
        new_target: None,
    }
}

/// `TypedArrayCreateFromConstructor(c, [len])` for the constructor in i64
/// `c` and the length number in i64 `len` into i64 `out`: the result must
/// be a typed array (`ValidateTypedArray`: not out of bounds or detached)
/// of at least `len` elements, else a `TypeError` (a throw returns).
fn create_from_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (c, len, out): (u32, u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    create_with(function, frame, (c, &[len], out), strings, helpers);
}

/// `TypedArrayCreateFromConstructor(c, args)` for the constructor in i64
/// `c` and the argument jsvals in the i64 locals `args` into i64 `out`: the
/// result must be a typed array that is not out of bounds or detached and,
/// when `args` is a single number, holds at least that many elements; else
/// a `TypeError` (a throw returns). `frame.z64(2)` / `z64(3)` are clobbered.
pub(super) fn create_with(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (c, args, out): (u32, &[u32], u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(
        &construct(c, args, frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; LocalSet(out));
    return_if_pending(function);
    is_kind(function, out, abi::TYPE_TYPEDARRAY);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, CREATE_NOT_TYPED_ARRAY, strings, helpers);
    wasm!(function; LocalGet(out), I32WrapI64, Call(helpers.ta_oob()));
    type_error_if(function, frame, CREATE_OUT_OF_BOUNDS, strings, helpers);
    if let [len] = args {
        wasm!(function;
            LocalGet(out), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U,
            LocalGet(*len), F64ReinterpretI64, F64Lt,
        );
        type_error_if(function, frame, CREATE_TOO_SHORT, strings, helpers);
    }
}

/// Push the intrinsic constructor of the kind of the typed array in i64
/// `o` (`%<Kind>Array%`, the default of `TypedArraySpeciesCreate` and the
/// constructor of `TypedArrayCreateSameType`) as a jsval; `kind` is an i32
/// temp.
pub(super) fn push_kind_ctor(
    function: &mut FnBuilder<'_>,
    o: u32,
    kind: u32,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(o), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)), LocalSet(kind));
    let ctor = |k: ElementKind| intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Typed(k)), helpers);
    let [head, rest @ ..] = ElementKind::ALL;
    wasm!(function; I64Const(ctor(head)));
    for k in rest {
        wasm!(function;
            I64Const(ctor(k)),
            LocalGet(kind), I32Const(i32::from(k.kind())), I32Ne,
            Select,
        );
    }
}

/// `TypedArrayCreateSameType(o, [len])` for the typed array in i64 `o` and
/// the length number in i64 `len` into i64 `out`: the realm constructor of
/// `o`'s kind (no user code runs) through [`create_from_ctor`]; `c` is an
/// i64 temp, `kind` an i32 temp.
pub(super) fn create_same_type(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (o, len, c, kind, out): (u32, u32, u32, u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    push_kind_ctor(function, o, kind, helpers);
    wasm!(function; LocalSet(c));
    create_from_ctor(function, frame, (c, len, out), strings, helpers);
}

/// `SpeciesConstructor` over a non-object `constructor`.
const SPECIES_CTOR_NOT_OBJECT: CalleeErrorText<'static> =
    type_error!("object.constructor is not an object");

/// `SpeciesConstructor` over a `@@species` that is not a constructor.
const SPECIES_NOT_CTOR: CalleeErrorText<'static> =
    type_error!("object.constructor[Symbol.species] is not a constructor");

/// `TypedArraySpeciesCreate`: the result's content type differs.
const SPECIES_CONTENT_TYPE: CalleeErrorText<'static> =
    type_error!("The TypedArray species constructor returned a TypedArray of another content type");

/// The `TypeError`s of `TypedArraySpeciesCreate`.
const SPECIES_TEXTS: [CalleeErrorText<'static>; 3] = [
    SPECIES_CTOR_NOT_OBJECT,
    SPECIES_NOT_CTOR,
    SPECIES_CONTENT_TYPE,
];

/// The first `BigInt` element kind (DESIGN.md §5.8.6): a kind at or past it
/// has the `BigInt` content type.
const FIRST_BIGINT_KIND: i32 = 10;

/// `TypedArraySpeciesCreate(o, args)` for the typed array in i64 `o` and the
/// argument jsvals in the i64 locals `args` into i64 `out`:
/// `SpeciesConstructor(o, %<Kind>Array%)` into i64 `c` (`C = Get(o,
/// "constructor")`: `undefined` is the default, a non-object throws; `S =
/// Get(C, @@species)`: `null` / `undefined` is the default, a constructor is
/// the result, anything else throws), then [`create_with`], then the
/// content types of `o` and the result must match. `kind` is an i32 temp; a
/// throw returns.
pub(super) fn species_create(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (o, args, c, kind, out): (u32, &[u32], u32, u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(o), I32Const(static_key(strings, "constructor")), Call(helpers.get), LocalSet(c),
    );
    return_if_pending(function);
    wasm!(function; LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
    push_kind_ctor(function, o, kind, helpers);
    wasm!(function; LocalSet(c), Else);
    emit_is_object(function, c);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, SPECIES_CTOR_NOT_OBJECT, strings, helpers);
    wasm!(function;
        LocalGet(c), I32Const(realm::symbol_addr(WellKnown::Species, helpers)),
        Call(helpers.symbol_get()), LocalSet(c),
    );
    return_if_pending(function);
    emit_is_nullish(function, c);
    wasm!(function; If(BlockType::Empty));
    push_kind_ctor(function, o, kind, helpers);
    wasm!(function; LocalSet(c), Else);
    emit_is_constructor(function, c, helpers);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, SPECIES_NOT_CTOR, strings, helpers);
    wasm!(function; End, End);
    create_with(function, frame, (c, args, out), strings, helpers);
    wasm!(function;
        LocalGet(o), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)),
        I32Const(FIRST_BIGINT_KIND), I32GeU,
        LocalGet(out), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)),
        I32Const(FIRST_BIGINT_KIND), I32GeU,
        I32Ne,
    );
    type_error_if(function, frame, SPECIES_CONTENT_TYPE, strings, helpers);
}

/// `%TypedArray%.of(...items)`: `this` must be a constructor; then
/// `TypedArrayCreateFromConstructor(this, [n])` and `Set` of each item in
/// order (`TypedArraySetElement`; a throw returns).
fn emit_of(function: &mut FnBuilder<'_>, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (c, target, item, len) = (frame.y64(0), frame.y64(1), frame.y64(2), frame.y64(3));
    let saved = frame.x64(0);
    let (n, i) = (frame.w32(0), frame.w32(1));
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    push_argc(function, frame);
    wasm!(function;
        LocalTee(n), F64ConvertI32U, I64ReinterpretF64, LocalSet(len),
        GlobalGet(THIS_GLOBAL), LocalSet(c),
    );
    emit_is_constructor(function, c, helpers);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, OF_NOT_CONSTRUCTOR, strings, helpers);
    create_from_ctor(function, frame, (c, len, target), strings, helpers);
    wasm!(function; I32Const(0), LocalSet(i));
    count_loop(function, i, n, |function| {
        arg_at(function, frame, i, saved, item);
        wasm!(function;
            LocalGet(target), I32WrapI64, LocalGet(i), LocalGet(item), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; LocalGet(target));
}

/// `%TypedArray%.from(source, mapFn, thisArg)`: `this` must be a
/// constructor and `mapFn` `undefined` or callable; `GetMethod(source,
/// @@iterator)` decides: an iterator is drained to a list first, then
/// `TypedArrayCreateFromConstructor(this, [len])`; anything else is an
/// array-like (`ToObject`, `LengthOfArrayLike`, the creation, `Get` per
/// index). Each value, mapped by `mapFn(v, k)` with `this = thisArg` when
/// given, is stored by `TypedArraySetElement`; a throw returns.
fn emit_from(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (c, source, map_fn, this_arg) = (frame.y64(0), frame.y64(1), frame.y64(2), frame.y64(3));
    let (target, len_v, v, method) = (frame.y64(4), frame.y64(5), frame.x64(0), frame.x64(1));
    let len = frame.zf64(0);
    let (n, k, mapping) = (frame.w32(0), frame.w32(1), frame.w32(2));
    let list = frame.base(1);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(c));
    for (i, local) in [(0, source), (1, map_fn), (2, this_arg)] {
        frame.arg(function, i);
        wasm!(function; LocalSet(local));
    }
    emit_is_constructor(function, c, helpers);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, FROM_NOT_CONSTRUCTOR, strings, helpers);
    wasm!(function;
        LocalGet(map_fn), I64Const(UNDEFINED_BITS), I64Ne, LocalTee(mapping),
        If(BlockType::Empty),
    );
    emit_is_function(function, map_fn, helpers);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, FROM_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    emit_is_nullish(function, source);
    type_error_if(function, frame, FROM_NULLISH, strings, helpers);
    // Map `v` (by `mapFn(v, k)` when mapping) and store it as element `k`.
    let store = |function: &mut FnBuilder<'_>| {
        wasm!(function; LocalGet(mapping), If(BlockType::Empty));
        super::array::push_call(
            function,
            frame,
            map_fn,
            this_arg,
            &[
                super::array::CallArg::Val(v),
                super::array::CallArg::Index(k),
            ],
            helpers,
        );
        wasm!(function; LocalSet(v));
        return_if_pending(function);
        wasm!(function;
            End,
            LocalGet(target), I32WrapI64, LocalGet(k), LocalGet(v), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    };
    wasm!(function;
        LocalGet(source), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_nullish(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    drain(function, frame, (source, method, v, list), strings, helpers);
    wasm!(function;
        LocalGet(list), I32Load(m32(abi::ARRAY_LEN_OFFSET)), LocalTee(n),
        F64ConvertI32U, I64ReinterpretF64, LocalSet(len_v),
    );
    create_from_ctor(function, frame, (c, len_v, target), strings, helpers);
    wasm!(function; I32Const(0), LocalSet(k));
    count_loop(function, k, n, |function| {
        list_element(function, list, k);
        wasm!(function; LocalSet(v));
        store(function);
    });
    wasm!(function; LocalGet(target), Return, End);
    wasm!(function; LocalGet(source), Call(helpers.to_object()), LocalSet(source));
    length_of_array_like(function, (source, v, len), strings, helpers);
    wasm!(function; LocalGet(len), I64ReinterpretF64, LocalSet(len_v));
    create_from_ctor(function, frame, (c, len_v, target), strings, helpers);
    // The result holds at least `len` elements, so `len < 2^29`.
    wasm!(function; LocalGet(len), I32TruncF64U, LocalSet(n), I32Const(0), LocalSet(k));
    count_loop(function, k, n, |function| {
        get_index(function, source, k, helpers);
        wasm!(function; LocalSet(v));
        return_if_pending(function);
        store(function);
    });
    wasm!(function; LocalGet(target));
}

/// Read `this` into i64 `this` and raise the incompatible-receiver
/// `TypeError` of `op` (returning `undefined`) unless it is a typed array.
fn this_typed_array(
    function: &mut FnBuilder<'_>,
    this: u32,
    op: TypedArrayOp,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_kind(function, this, abi::TYPE_TYPEDARRAY);
    wasm!(function;
        I32Eqz, If(BlockType::Empty),
        LocalGet(this), I64Const(HEAP_BITS | i64::from(static_key(strings, receiver_name(op)))),
        I32Const(PROP_THROW_TA_INCOMPATIBLE), Call(helpers.prop_throw),
        I64Const(UNDEFINED_BITS), Return,
        End,
    );
}

/// The `buffer` / `byteLength` / `byteOffset` / `length` getters: the
/// receiver check, then the field; the three numbers are `0` for an
/// out-of-bounds or detached view (a length-tracking view's lengths come
/// from its buffer's current length).
fn emit_getter(
    op: TypedArrayOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.y64(0);
    this_typed_array(function, this, op, strings, helpers);
    match op {
        TypedArrayOp::Buffer => {
            field(function, this, abi::TA_BUFFER_OFFSET);
            wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
        }
        TypedArrayOp::ByteLength => {
            wasm!(function;
                LocalGet(this), I32WrapI64, Call(helpers.ta_length()),
                LocalGet(this), I32WrapI64, I32Load8U(m8(abi::TA_SHIFT_OFFSET)), I32Shl,
                F64ConvertI32U, I64ReinterpretF64,
            );
        }
        TypedArrayOp::ByteOffset => {
            wasm!(function; I32Const(0));
            field(function, this, abi::TA_BYTE_OFFSET_OFFSET);
            wasm!(function;
                LocalGet(this), I32WrapI64, Call(helpers.ta_oob()), Select,
                F64ConvertI32U, I64ReinterpretF64,
            );
        }
        _ => wasm!(function;
            LocalGet(this), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, I64ReinterpretF64,
        ),
    }
}

/// `get %TypedArray%.prototype[@@toStringTag]`: the kind's constructor name
/// for a typed array, `undefined` for any other value (never throwing).
fn emit_to_string_tag(function: &mut FnBuilder<'_>, frame: Frame, strings: &StrOffsets) {
    let (this, kind) = (frame.y64(0), frame.w32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_kind(function, this, abi::TYPE_TYPEDARRAY);
    wasm!(function;
        I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(this), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)), LocalSet(kind),
    );
    let [head, rest @ ..] = ElementKind::ALL;
    wasm!(function; I32Const(static_key(strings, head.name())));
    for k in rest {
        wasm!(function;
            I32Const(static_key(strings, k.name())),
            LocalGet(kind), I32Const(i32::from(k.kind())), I32Ne,
            Select,
        );
    }
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `keys` / `values` / `entries`: `ValidateTypedArray(this)` (a receiver
/// that is not a typed array, or an out-of-bounds or detached one, throws),
/// then an Array Iterator of `kind` over it (DESIGN.md §5.8.7).
fn emit_iter(
    op: TypedArrayOp,
    kind: IterKind,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, obj) = (frame.y64(0), frame.base(0));
    wasm!(function;
        GlobalGet(THIS_GLOBAL), LocalTee(this),
        I32Const(static_key(strings, receiver_name(op))), Call(helpers.ta_validate()), Drop,
    );
    return_if_pending(function);
    super::iter::create(function, this, obj, IterFamily::Array, kind, helpers);
}
