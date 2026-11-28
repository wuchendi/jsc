//! The binary-data helpers (DESIGN.md §5.8): the byte-block allocator
//! `__ab_alloc`, the console renderer arm `__render_binary`, and the
//! typed-array helpers (the buffer witness `__ta_length` / `__ta_oob`,
//! `__ta_validate`, `CanonicalNumericIndexString` `__ta_canonical`, the
//! element accessors `__ta_get` / `__ta_set` / `__ta_set_value`, the inline
//! fast-path halves `__ta_index_get` / `__ta_index_set`, and the shared
//! `__raw_load` / `__raw_store`), a helper group of their own after every
//! other ([`super::registry::Group::Binary`]).

use wasm_encoder::{BlockType, Function, Ieee64, Instruction, MemArg, ValType};

use super::{
    EXC_PENDING_GLOBAL, ElementKind, FnBuilder, HEAP_BITS, Helpers, LeafSet,
    PROP_THROW_TA_INCOMPATIBLE, PROP_THROW_TA_OUT_OF_BOUNDS, RAW_F64, RAW_I32, RAW_I64, ROOT_BASE,
    ROOT_JS, STR_DATA_OFFSET, Sink, StrOffsets, TYPE_STRING, UNDEFINED_BITS, abi, emit_is_number,
    emit_is_pointer, emit_load_type_byte, finish_builder, helper_builder, numeric::emit_to_int32,
    static_key,
};

/// Emit each listed instruction in order.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

use ValType::{I32, I64};

/// The `(params, results)` of the binary helpers, in function-index order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 13] = [
    (&[I32], &[I32]),             // __ab_alloc(size) -> block
    (&[I32, I32], &[]),           // __render_binary(p, depth)
    (&[I32], &[I32]),             // __ta_length(o) -> length
    (&[I32], &[I32]),             // __ta_oob(o) -> out of bounds
    (&[I64, I32], &[I32]),        // __ta_validate(o, name) -> length
    (&[I32], &[I32]),             // __ta_canonical(key) -> canonical numeric
    (&[I32, I32], &[I64]),        // __ta_get(o, i) -> element
    (&[I32, I32, I64], &[]),      // __ta_set(o, i, n)
    (&[I32, I32, I64], &[]),      // __ta_set_value(o, i, v)
    (&[I32, I64], &[I64]),        // __ta_index_get(o, k) -> element
    (&[I32, I64, I64], &[]),      // __ta_index_set(o, k, v)
    (&[I32, I32, I32], &[I64]),   // __raw_load(addr, kind, swap) -> number
    (&[I32, I32, I64, I32], &[]), // __raw_store(addr, kind, n, swap)
];

/// The data offset of a `TYPE_RAW` block (after the header and `len`).
pub(super) const RAW_DATA: u32 = abi::HEADER_SIZE + 4;

/// The `ArrayBuffer` rendering's static strings (DESIGN.md §5.8.8).
const OPEN: &str = "ArrayBuffer { ";
const DETACHED: &str = "(detached)";
const CONTENTS: &str = "[Uint8Contents]: <";
const CONTENTS_END: &str = ">";
const BYTE_LENGTH: &str = ", byteLength: ";
const CLOSE: &str = " }";

/// The typed-array rendering's static strings (DESIGN.md §5.8.8).
const TA_LENGTH_OPEN: &str = "(";
const TA_LENGTH_CLOSE: &str = ") ";
const TA_OPEN: &str = "[ ";
const TA_SEPARATOR: &str = ", ";
const TA_EMPTY: &str = "[]";
const TA_CLOSE: &str = " ]";

/// The one canonical numeric string `__ta_canonical` cannot decide by
/// round-tripping (`ToString(-0)` is `"0"`).
const MINUS_ZERO: &str = "-0";

/// The static strings the binary helpers write.
pub(super) const STRINGS: [&str; 13] = [
    OPEN,
    DETACHED,
    CONTENTS,
    CONTENTS_END,
    BYTE_LENGTH,
    CLOSE,
    TA_LENGTH_OPEN,
    TA_LENGTH_CLOSE,
    TA_OPEN,
    TA_SEPARATOR,
    TA_EMPTY,
    TA_CLOSE,
    MINUS_ZERO,
];

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

/// The leaf declarations of the binary-data witness and element helpers
/// (see [`super::roots::helper_graph`]).
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.ta_length(), vec![]),
        (helpers.ta_oob(), vec![]),
        (helpers.raw_load(), vec![helpers.box_f64]),
        (helpers.raw_store(), vec![]),
        (
            helpers.ta_get(),
            vec![helpers.ta_length(), helpers.raw_load()],
        ),
        (
            helpers.ta_set(),
            vec![helpers.ta_length(), helpers.raw_store()],
        ),
        (helpers.ta_index_get(), vec![helpers.ta_get()]),
    ]
}

/// The callees of `__test_integrity`: a typed array's elements decide its
/// integrity (`__ta_length`, DESIGN.md §5.8.5) in a program that can reach
/// one.
pub(super) fn integrity_callees(helpers: Helpers<'_>) -> Vec<u32> {
    if helpers.has_binary() {
        vec![helpers.ta_length()]
    } else {
        vec![]
    }
}

/// Every binary helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Vec<Function> {
    vec![
        emit_ab_alloc(helpers, leaves),
        emit_render_binary(strings, helpers, leaves),
        emit_ta_length(false, leaves),
        emit_ta_length(true, leaves),
        emit_ta_validate(helpers, leaves),
        emit_ta_canonical(strings, helpers, leaves),
        emit_ta_get(helpers, leaves),
        emit_ta_set(helpers, leaves),
        emit_ta_set_value(helpers, leaves),
        emit_ta_index_get(helpers, leaves),
        emit_ta_index_set(helpers, leaves),
        emit_raw_load(helpers, leaves),
        emit_raw_store(leaves),
    ]
}

/// `__ab_alloc(size) -> block | 0` (DESIGN.md §5.8.2): a zeroed `TYPE_RAW`
/// block of `8 + size` bytes with `len = size` from the variable-size
/// `__alloc`; a `size` of `0` allocates nothing and returns `0`, as does an
/// out-of-memory `__alloc`, which leaves the `RangeError` pending.
fn emit_ab_alloc(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32]);
    let (size, block) = (0, 1);
    wasm!(f;
        LocalGet(size), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(size), I32Const(RAW_DATA.cast_signed()), I32Add, Call(helpers.alloc), LocalTee(block),
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(block), I32Const(i32::from(abi::TYPE_RAW)), I32Store8(m8(0)),
        LocalGet(block), LocalGet(size), I32Store(m32(abi::HEADER_SIZE)),
        LocalGet(block),
        End,
    );
    finish_builder(f)
}

/// Append the static string `text`.
fn lit(f: &mut FnBuilder<'_>, strings: &StrOffsets, helpers: Helpers<'_>, text: &str) {
    wasm!(f; I32Const(static_key(strings, text)), Call(helpers.out_str()));
}

/// Append the lowercase hex digit of the nibble in the i32 local `d`.
fn hex_digit(f: &mut FnBuilder<'_>, helpers: Helpers<'_>, d: u32) {
    wasm!(f;
        LocalGet(d), I32Const(0x30), I32Add,
        LocalGet(d), I32Const(9), I32GtU, I32Const(0x61 - 0x3A), I32Mul, I32Add,
        Call(helpers.out_byte()),
    );
}

/// `__render_binary(p, depth)` (DESIGN.md §5.8.8): the console form of the
/// binary-data block `p`, read directly (no getter runs): a typed array by
/// [`render_typed_array`], else `ArrayBuffer { [Uint8Contents]: <01 02 ff>,
/// byteLength: 3 }`, or `ArrayBuffer { (detached), byteLength: 0 }`. Allocates only through the output buffer
/// and never throws; `p` stays rooted, so its byte block does too.
fn emit_render_binary(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut function = helper_builder(
        leaves,
        &[ROOT_BASE, RAW_I32],
        &[RAW_I32, RAW_I32, RAW_I32, RAW_I32],
    );
    let (block, at, count, nibble) = (0, 2, 3, 4);
    let (depth, first) = (1, 5);
    wasm!(function;
        LocalGet(block), I32Load8U(m8(0)), I32Const(i32::from(abi::TYPE_TYPEDARRAY)), I32Eq,
        If(BlockType::Empty),
    );
    render_typed_array(
        &mut function,
        strings,
        helpers,
        [block, depth, at, count, first],
    );
    wasm!(function; Return, End);
    lit(&mut function, strings, helpers, OPEN);
    wasm!(function;
        LocalGet(block), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::AB_FLAG_DETACHED)), I32And,
        If(BlockType::Empty),
    );
    lit(&mut function, strings, helpers, DETACHED);
    wasm!(function; Else);
    lit(&mut function, strings, helpers, CONTENTS);
    wasm!(function;
        LocalGet(block), I32Load(m32(abi::AB_BYTE_LENGTH_OFFSET)), LocalSet(count),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(count), I32GeU, BrIf(1),
        LocalGet(at), If(BlockType::Empty), I32Const(0x20), Call(helpers.out_byte()), End,
        // The byte, reloaded each step: appending may collect (never moves).
        LocalGet(block), I32Load(m32(abi::AB_DATA_OFFSET)), LocalGet(at), I32Add,
        I32Load8U(m8(RAW_DATA)), I32Const(4), I32ShrU, LocalSet(nibble),
    );
    hex_digit(&mut function, helpers, nibble);
    wasm!(function;
        LocalGet(block), I32Load(m32(abi::AB_DATA_OFFSET)), LocalGet(at), I32Add,
        I32Load8U(m8(RAW_DATA)), I32Const(15), I32And, LocalSet(nibble),
    );
    hex_digit(&mut function, helpers, nibble);
    wasm!(function;
        LocalGet(at), I32Const(1), I32Add, LocalSet(at),
        Br(0), End, End,
    );
    lit(&mut function, strings, helpers, CONTENTS_END);
    wasm!(function; End);
    lit(&mut function, strings, helpers, BYTE_LENGTH);
    wasm!(function; LocalGet(block), I32Load(m32(abi::AB_BYTE_LENGTH_OFFSET)), Call(helpers.out_uint()));
    lit(&mut function, strings, helpers, CLOSE);
    wasm!(function; End);
    finish_builder(function)
}

/// The typed-array arm of `__render_binary` (DESIGN.md §5.8.8): `<Kind>(n) [
/// e0, e1, .., name: v ]` with the kind's constructor name (so a subclass
/// instance renders as its kind), the current length (`0` when out of
/// bounds or detached), each element by the number rule, then the own named
/// properties as for an array; `<Kind>(0) []` when there is nothing to list.
fn render_typed_array(
    f: &mut FnBuilder<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    [p, depth, i, n, first]: [u32; 5],
) {
    // The kind's name: a `select` chain over the kind byte.
    let [head, rest @ ..] = ElementKind::ALL;
    wasm!(f;
        LocalGet(p), I32Load8U(m8(abi::TA_KIND_OFFSET)), LocalSet(i),
        I32Const(static_key(strings, head.name())),
    );
    for kind in rest {
        wasm!(f;
            I32Const(static_key(strings, kind.name())),
            LocalGet(i), I32Const(i32::from(kind.kind())), I32Ne,
            Select,
        );
    }
    wasm!(f; Call(helpers.out_str()));
    lit(f, strings, helpers, TA_LENGTH_OPEN);
    wasm!(f; LocalGet(p), Call(helpers.ta_length()), LocalTee(n), Call(helpers.out_uint()));
    lit(f, strings, helpers, TA_LENGTH_CLOSE);
    wasm!(f;
        I32Const(1), LocalSet(first),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(first), If(BlockType::Empty),
    );
    lit(f, strings, helpers, TA_OPEN);
    wasm!(f; Else);
    lit(f, strings, helpers, TA_SEPARATOR);
    wasm!(f;
        End,
        I32Const(0), LocalSet(first),
        // Re-read through the witness: nothing here runs user code, so the
        // length cannot change, but the element read stays checked.
        LocalGet(p), LocalGet(i), Call(helpers.ta_get()), LocalGet(depth), Call(helpers.render()),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0), End, End,
        LocalGet(p), I32Const(1), I32Const(0), LocalGet(first),
        I32Const(static_key(strings, TA_OPEN)), LocalGet(depth), Call(helpers.render_props()),
        LocalSet(first),
        LocalGet(first), I32Eqz, If(BlockType::Empty),
    );
    lit(f, strings, helpers, TA_CLOSE);
    wasm!(f; Else);
    lit(f, strings, helpers, TA_EMPTY);
    wasm!(f; End);
}

/// An unaligned (`align = 0`) [`MemArg`] at `offset`: the element accessors
/// serve `DataView`'s arbitrary offsets too, and the alignment is only a hint.
fn ma(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

/// `__ta_length(o) -> i32` (`oob` = `false`) / `__ta_oob(o) -> i32` (`oob`)
/// (DESIGN.md §5.8.4): `TypedArrayLength` of the buffer witness with out of
/// bounds folded to `0`, or `IsTypedArrayOutOfBounds`. A detached buffer is
/// out of bounds; a length-tracking view is out of bounds iff its offset is
/// past the buffer's length and has length `(L - off) >> shift`; a fixed one
/// is out of bounds iff `off + (array_length << shift) > L`. Non-allocating
/// leaves.
fn emit_ta_length(oob: bool, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[RAW_I32, RAW_I32, RAW_I32, RAW_I32]);
    let (o, b, len, off, n) = (0, 1, 2, 3, 4);
    wasm!(f;
        LocalGet(o), I32Load(m32(abi::TA_BUFFER_OFFSET)), LocalTee(b),
        I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)), I32Const(i32::from(abi::AB_FLAG_DETACHED)), I32And,
        If(BlockType::Empty), I32Const(i32::from(oob)), Return, End,
        LocalGet(b), I32Load(m32(abi::AB_BYTE_LENGTH_OFFSET)), LocalSet(len),
        LocalGet(o), I32Load(m32(abi::TA_BYTE_OFFSET_OFFSET)), LocalSet(off),
        LocalGet(o), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::TA_FLAG_LENGTH_TRACKING)), I32And,
        If(BlockType::Result(I32)),
        // Length-tracking: out of bounds iff off > L.
        LocalGet(off), LocalGet(len), I32GtU,
    );
    if !oob {
        wasm!(f;
            If(BlockType::Result(I32)), I32Const(0),
            Else,
            LocalGet(len), LocalGet(off), I32Sub,
            LocalGet(o), I32Load8U(m8(abi::TA_SHIFT_OFFSET)), I32ShrU,
            End,
        );
    }
    wasm!(f;
        Else,
        // Fixed: out of bounds iff off + (array_length << shift) > L.
        LocalGet(o), I32Load(m32(abi::TA_LENGTH_OFFSET)), LocalTee(n),
        LocalGet(o), I32Load8U(m8(abi::TA_SHIFT_OFFSET)), I32Shl,
        LocalGet(off), I32Add, LocalGet(len), I32GtU,
    );
    if !oob {
        wasm!(f; If(BlockType::Result(I32)), I32Const(0), Else, LocalGet(n), End);
    }
    wasm!(f; End, End);
    finish_builder(f)
}

/// `__ta_validate(o: i64, name: i32) -> i32`: `ValidateTypedArray`
/// (DESIGN.md §5.8.4) — a receiver that is not a typed array throws
/// `TypeError: Method <name> called on incompatible receiver`, an out of
/// bounds or detached one `TypeError: Cannot perform <name> on a detached or
/// out-of-bounds TypedArray` (`name` an interned static string; the
/// exception is left pending and the result is `0`); else the length.
fn emit_ta_validate(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_JS, ROOT_BASE], &[]);
    let (o, name) = (0, 1);
    let throw = |f: &mut FnBuilder<'_>, mode: i32| {
        wasm!(f;
            LocalGet(o), LocalGet(name), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
            I32Const(mode), Call(helpers.prop_throw), I32Const(0), Return,
        );
    };
    emit_is_pointer(&mut f, o);
    wasm!(f; If(BlockType::Result(I32)));
    emit_load_type_byte(&mut f, o);
    wasm!(f;
        I32Const(i32::from(abi::TYPE_TYPEDARRAY)), I32Eq,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty),
    );
    throw(&mut f, PROP_THROW_TA_INCOMPATIBLE);
    wasm!(f; End, LocalGet(o), I32WrapI64, Call(helpers.ta_oob()), If(BlockType::Empty));
    throw(&mut f, PROP_THROW_TA_OUT_OF_BOUNDS);
    wasm!(f; End, LocalGet(o), I32WrapI64, Call(helpers.ta_length()), End);
    finish_builder(f)
}

/// `__ta_canonical(key: i32) -> i32` (DESIGN.md §5.8.5): `1` iff
/// `CanonicalNumericIndexString(key)` is not `undefined`. A symbol, an empty
/// or two-byte string (a canonical numeric string is ASCII, and an all-Latin-1
/// string is always one-byte) or one whose first unit is not a digit, `-`,
/// `I` or `N` is `0` at once; `"-0"` is `1`; anything else is `ToString(
/// ToNumber(key)) === key`. Allocates (`__num_to_str`).
fn emit_ta_canonical(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[RAW_I32]);
    let (key, c) = (0, 1);
    wasm!(f;
        LocalGet(key), I32Load8U(m8(0)), I32Const(TYPE_STRING), I32Ne,
        LocalGet(key), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::STR_FLAG_TWO_BYTE)), I32And, I32Or,
        LocalGet(key), I32Load(m32(STR_LEN_OFFSET_U32)), I32Eqz, I32Or,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(key), I32Load8U(m8(STR_DATA_OFFSET.cast_unsigned())), LocalTee(c),
        I32Const(i32::from(b'0')), I32Sub, I32Const(9), I32LeU,
        LocalGet(c), I32Const(i32::from(b'-')), I32Eq, I32Or,
        LocalGet(c), I32Const(i32::from(b'I')), I32Eq, I32Or,
        LocalGet(c), I32Const(i32::from(b'N')), I32Eq, I32Or,
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(key), I32Const(static_key(strings, MINUS_ZERO)), Call(helpers.str_eq),
        If(BlockType::Empty), I32Const(1), Return, End,
        LocalGet(key), Call(helpers.string_to_number()), Call(helpers.box_f64),
        Call(helpers.num_to_str()), LocalGet(key), Call(helpers.str_eq),
        End,
    );
    finish_builder(f)
}

/// The `u32` offset of a string's length field.
const STR_LEN_OFFSET_U32: u32 = abi::STR_LEN_OFFSET;

/// Push the byte address of element `i` (i32 local) of the typed array in
/// i32 local `o`: `buffer.data + 8 + byte_offset + (i << shift)`. Only for a
/// valid index (a valid index implies a nonzero byte block).
fn element_addr(f: &mut impl Sink, o: u32, i: u32) {
    wasm!(f;
        LocalGet(o), I32Load(m32(abi::TA_BUFFER_OFFSET)), I32Load(m32(abi::AB_DATA_OFFSET)),
        I32Const(RAW_DATA.cast_signed()), I32Add,
        LocalGet(o), I32Load(m32(abi::TA_BYTE_OFFSET_OFFSET)), I32Add,
        LocalGet(i), LocalGet(o), I32Load8U(m8(abi::TA_SHIFT_OFFSET)), I32Shl, I32Add,
    );
}

/// `__ta_get(o, i) -> i64` (DESIGN.md §5.8.5): element `i` of the typed
/// array `o` as a number, or `undefined` when `IsValidIntegerIndex` fails.
/// A non-allocating leaf.
fn emit_ta_get(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32], &[]);
    let (o, i) = (0, 1);
    wasm!(f;
        LocalGet(i), LocalGet(o), Call(helpers.ta_length()), I32GeU,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
    element_addr(&mut f, o, i);
    wasm!(f;
        LocalGet(o), I32Load8U(m8(abi::TA_KIND_OFFSET)), I32Const(0), Call(helpers.raw_load()),
        End,
    );
    finish_builder(f)
}

/// `__ta_set(o, i, n)` (DESIGN.md §5.8.5): store the number `n` (already
/// converted) as element `i` of the typed array `o` when `IsValidIntegerIndex`
/// holds (re-checked here, after any conversion), else nothing. A
/// non-allocating leaf.
fn emit_ta_set(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32, RAW_I64], &[]);
    let (o, i, n) = (0, 1, 2);
    wasm!(f;
        LocalGet(i), LocalGet(o), Call(helpers.ta_length()), I32GeU,
        If(BlockType::Empty), Return, End,
    );
    element_addr(&mut f, o, i);
    wasm!(f;
        LocalGet(o), I32Load8U(m8(abi::TA_KIND_OFFSET)), LocalGet(n), I32Const(0),
        Call(helpers.raw_store()),
        End,
    );
    finish_builder(f)
}

/// `ToNumber(v)` of the jsval in local `v` (a number as is) into the i64
/// local `n`; a throw leaves the exception pending and returns from the
/// helper.
fn to_number_or_return(f: &mut FnBuilder<'_>, v: u32, n: u32, helpers: Helpers<'_>) {
    emit_is_number(f, v);
    wasm!(f;
        If(BlockType::Result(I64)), LocalGet(v), Else, LocalGet(v), Call(helpers.to_number), End,
        LocalSet(n),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), Return, End,
    );
}

/// `__ta_set_value(o, i, v)`: `TypedArraySetElement` (DESIGN.md §5.8.5) —
/// `ToNumber(v)` first (a user `valueOf` may run, detach or shrink the buffer,
/// or throw: the exception is left pending), then [`emit_ta_set`]'s checked
/// store of element `i` (`-1` for an invalid canonical numeric key, which
/// is never valid).
fn emit_ta_set_value(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32, ROOT_JS], &[RAW_I64]);
    let (ta, index, value, number) = (0, 1, 2, 3);
    to_number_or_return(&mut f, value, number, helpers);
    wasm!(f;
        LocalGet(ta), LocalGet(index), LocalGet(number), Call(helpers.ta_set()),
        End,
    );
    finish_builder(f)
}

/// Push `1` when the number in f64 local `d` is an integral `0 <= d < 2^32 -
/// 1` (`-0` included): `ToPropertyKey` of it is an array index.
fn is_index_number(f: &mut impl Sink, d: u32) {
    wasm!(f;
        LocalGet(d), F64Const(Ieee64::from(0.0)), F64Ge,
        LocalGet(d), F64Const(Ieee64::from(f64::from(u32::MAX))), F64Lt, I32And,
        LocalGet(d), F64Trunc, LocalGet(d), F64Eq, I32And,
    );
}

/// `__ta_index_get(o, k: i64) -> i64` (DESIGN.md §5.8.5, the inline `ta[k]`
/// read for a number `k`): an array-index number is the element (or
/// `undefined` past the length); any other number is an invalid integer
/// index (`undefined`). The chain is never consulted. A non-allocating leaf.
fn emit_ta_index_get(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I64], &[RAW_F64]);
    let (o, k, d) = (0, 1, 2);
    wasm!(f; LocalGet(k), F64ReinterpretI64, LocalSet(d));
    is_index_number(&mut f, d);
    wasm!(f;
        If(BlockType::Result(I64)),
        LocalGet(o), LocalGet(d), I32TruncSatF64U, Call(helpers.ta_get()),
        Else, I64Const(UNDEFINED_BITS), End,
        End,
    );
    finish_builder(f)
}

/// `__ta_index_set(o, k: i64, v)` (DESIGN.md §5.8.5, the inline `ta[k] = v`
/// store for a number `k`): `ToNumber(v)` first (a throw is left pending),
/// then an array-index number stores through [`emit_ta_set`] (checked); any
/// other number is an invalid integer index and the store is ignored.
fn emit_ta_index_set(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I64, ROOT_JS], &[RAW_I64, RAW_F64]);
    let (ta, key, value, number, index) = (0, 1, 2, 3, 4);
    to_number_or_return(&mut f, value, number, helpers);
    wasm!(f; LocalGet(key), F64ReinterpretI64, LocalSet(index));
    is_index_number(&mut f, index);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(ta), LocalGet(index), I32TruncSatF64U, LocalGet(number), Call(helpers.ta_set()),
        End,
        End,
    );
    finish_builder(f)
}

/// The number of element kinds the `kind` byte can name (DESIGN.md §5.8.6),
/// the reserved `Float16` and `BigInt` kinds included.
const KIND_COUNT: u32 = 12;

/// Emit a `br_table` switch over the i32 local `kind` with one arm per kind
/// `0..KIND_COUNT`, inside a block of type `result`; `arm(f, kind)` emits
/// the arm's body (leaving a `result` value), after which the switch exits.
fn kind_switch<S: Sink>(f: &mut S, kind: u32, result: BlockType, mut arm: impl FnMut(&mut S, u32)) {
    wasm!(f; Block(result));
    for _ in 0..KIND_COUNT {
        wasm!(f; Block(BlockType::Empty));
    }
    let targets: Vec<u32> = (0..KIND_COUNT).collect();
    f.instruction(&Instruction::LocalGet(kind));
    f.instruction(&Instruction::BrTable(targets.into(), KIND_COUNT - 1));
    for k in 0..KIND_COUNT {
        wasm!(f; End);
        arm(f, k);
        wasm!(f; Br(KIND_COUNT - 1 - k));
    }
    wasm!(f; End);
}

/// `__raw_load(addr, kind, swap) -> i64` (DESIGN.md §5.8.6): the kind's
/// width loaded at `addr` (unaligned) and read as a number by the kind's
/// rule; a float goes through `__box_f64`, so a stored NaN of any payload
/// reads as the canonical `NaN`. `swap` (a big-endian `DataView` access) is
/// reserved for the `DataView` stage; typed arrays pass `0`. The reserved
/// kinds (`Float16`, the `BigInt` kinds) are never stored in a typed array
/// yet and read `undefined`. A non-allocating leaf.
fn emit_raw_load(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32, RAW_I32], &[]);
    let (addr, kind) = (0, 1);
    kind_switch(&mut f, kind, BlockType::Result(I64), |f, k| {
        let int = |f: &mut FnBuilder<'_>, load: Instruction<'static>, signed: bool| {
            f.instruction(&Instruction::LocalGet(addr));
            f.instruction(&load);
            f.instruction(&if signed {
                Instruction::F64ConvertI32S
            } else {
                Instruction::F64ConvertI32U
            });
            f.instruction(&Instruction::I64ReinterpretF64);
        };
        match k {
            0 => int(f, Instruction::I32Load8S(ma(0)), true),
            1 | 2 => int(f, Instruction::I32Load8U(ma(0)), false),
            3 => int(f, Instruction::I32Load16S(ma(0)), true),
            4 => int(f, Instruction::I32Load16U(ma(0)), false),
            5 => int(f, Instruction::I32Load(ma(0)), true),
            6 => int(f, Instruction::I32Load(ma(0)), false),
            8 => wasm!(f; LocalGet(addr), F32Load(ma(0)), F64PromoteF32, Call(helpers.box_f64)),
            9 => wasm!(f; LocalGet(addr), F64Load(ma(0)), Call(helpers.box_f64)),
            _ => wasm!(f; I64Const(UNDEFINED_BITS)),
        }
    });
    wasm!(f; End);
    finish_builder(f)
}

/// `__raw_store(addr, kind, n, swap)` (DESIGN.md §5.8.6): the number `n`
/// converted by the kind's rule and stored at `addr` (unaligned): the
/// integer kinds keep the low bits of the `ToInt32` pattern (`ToInt8` ..
/// `ToUint32`), `Uint8Clamped` rounds half to even after clamping to `0 ..
/// 255` (`NaN` is `0`), `Float32` demotes, `Float64` stores the bits. `swap`
/// is reserved for the `DataView` stage; the reserved kinds store nothing.
/// A non-allocating leaf.
fn emit_raw_store(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32, RAW_I64, RAW_I32], &[]);
    let (addr, kind, n) = (0, 1, 2);
    kind_switch(&mut f, kind, BlockType::Empty, |f, k| {
        let int = |f: &mut FnBuilder<'_>, store: Instruction<'static>| {
            f.instruction(&Instruction::LocalGet(addr));
            emit_to_int32(f, n);
            f.instruction(&store);
        };
        match k {
            0 | 1 => int(f, Instruction::I32Store8(ma(0))),
            3 | 4 => int(f, Instruction::I32Store16(ma(0))),
            5 | 6 => int(f, Instruction::I32Store(ma(0))),
            2 => wasm!(f;
                LocalGet(addr),
                LocalGet(n), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Max,
                F64Const(Ieee64::from(255.0)), F64Min, F64Nearest, I32TruncSatF64U,
                I32Store8(ma(0)),
            ),
            8 => {
                wasm!(f; LocalGet(addr), LocalGet(n), F64ReinterpretI64, F32DemoteF64, F32Store(ma(0)));
            }
            9 => wasm!(f; LocalGet(addr), LocalGet(n), I64Store(ma(0))),
            _ => {}
        }
    });
    wasm!(f; End);
    finish_builder(f)
}
