//! The binary-data helpers (DESIGN.md §5.8): the byte-block allocator
//! `__ab_alloc`, the console renderer arm `__render_binary`, and the
//! `DataView` helpers `__dv_length`, `__dv_get`, `__dv_set`, a helper group
//! of their own after every other ([`super::registry::Group::Binary`]).

use wasm_encoder::{BlockType, Function, Ieee64, Instruction, MemArg, ValType};

use super::{
    FnBuilder, Helpers, LeafSet, RAW_I32, RAW_I64, ROOT_BASE, Sink, StrOffsets, abi,
    finish_builder, helper_builder, static_key,
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
pub(super) const TYPES: [(&[ValType], &[ValType]); 5] = [
    (&[I32], &[I32]),   // __ab_alloc(size) -> block
    (&[I32, I32], &[]), // __render_binary(p, depth)
    // The `DataView` helpers.
    (&[I32], &[I32]),             // __dv_length(view) -> byte length | -1
    (&[I32, I32, I32], &[I64]),   // __dv_get(addr, kind, swap) -> number
    (&[I32, I32, I64, I32], &[]), // __dv_set(addr, kind, n, swap)
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

/// The static strings the binary helpers write.
pub(super) const STRINGS: [&str; 6] = [OPEN, DETACHED, CONTENTS, CONTENTS_END, BYTE_LENGTH, CLOSE];

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

/// Every binary helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Vec<Function> {
    vec![
        emit_ab_alloc(helpers, leaves),
        emit_render_binary(strings, helpers, leaves),
        emit_dv_length(leaves),
        emit_dv_get(helpers, leaves),
        emit_dv_set(leaves),
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
/// binary-data block `p`, read directly (no getter runs): `ArrayBuffer {
/// [Uint8Contents]: <01 02 ff>, byteLength: 3 }`, or `ArrayBuffer {
/// (detached), byteLength: 0 }`. Allocates only through the output buffer
/// and never throws; `p` stays rooted, so its byte block does too.
fn emit_render_binary(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut function = helper_builder(leaves, &[ROOT_BASE, RAW_I32], &[RAW_I32, RAW_I32, RAW_I32]);
    let (block, depth, at, count, nibble) = (0, 1, 2, 3, 4);
    render_data_view(
        &mut function,
        strings,
        helpers,
        [block, depth, count, nibble],
    );
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

// The `DataView` helpers (DESIGN.md §5.8.4, §5.8.6, §5.8.8): the view
// length, the raw element load / store with the big-endian byte swap, and
// the rendering arm.

/// The `DataView` helpers' leaf declarations (see `roots::helper_graph`):
/// they only read and write the buffer's bytes.
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.dv_length(), vec![]),
        (helpers.dv_get(), vec![helpers.box_f64]),
        (helpers.dv_set(), vec![]),
    ]
}

/// The `DataView` rendering's static strings (DESIGN.md §5.8.8).
const DV_OPEN: &str = "DataView { byteLength: ";
const DV_BYTE_OFFSET: &str = ", byteOffset: ";
const DV_BUFFER: &str = ", buffer: ";

/// The static strings the `DataView` helpers write.
pub(super) const DV_STRINGS: [&str; 3] = [DV_OPEN, DV_BYTE_OFFSET, DV_BUFFER];

/// The `DataView` arm of `__render_binary` for the block in i32 local `block`
/// (the i32 locals `count` and `off` are temps): `DataView { byteLength: 2,
/// byteOffset: 0, buffer: ArrayBuffer { .. } }`, both numbers `0` when the
/// view is out of bounds or detached, then returns.
fn render_data_view(
    function: &mut FnBuilder<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    [block, depth, count, off]: [u32; 4],
) {
    wasm!(function;
        LocalGet(block), I32Load8U(m8(0)), I32Const(i32::from(abi::TYPE_DATAVIEW)), I32Eq,
        If(BlockType::Empty),
        LocalGet(block), Call(helpers.dv_length()), LocalTee(count),
        I32Const(0), I32LtS,
        If(BlockType::Empty),
        I32Const(0), LocalSet(count), I32Const(0), LocalSet(off),
        Else,
        LocalGet(block), I32Load(m32(abi::DV_BYTE_OFFSET_OFFSET)), LocalSet(off),
        End,
    );
    lit(function, strings, helpers, DV_OPEN);
    wasm!(function; LocalGet(count), Call(helpers.out_uint()));
    lit(function, strings, helpers, DV_BYTE_OFFSET);
    wasm!(function; LocalGet(off), Call(helpers.out_uint()));
    lit(function, strings, helpers, DV_BUFFER);
    wasm!(function;
        LocalGet(block), I32Load(m32(abi::DV_BUFFER_OFFSET)),
        LocalGet(depth), I32Const(1), I32Add,
        Call(helpers.render_binary()),
    );
    lit(function, strings, helpers, CLOSE);
    wasm!(function; Return, End);
}

/// Push the header flags of the block in i32 local `block` masked by `flag`.
fn block_flag(function: &mut FnBuilder<'_>, block: u32, flag: u8) {
    wasm!(function;
        LocalGet(block), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(flag)), I32And,
    );
}

/// `__dv_length(view) -> i32` (DESIGN.md §5.8.4): `GetViewByteLength` of the
/// `DataView` block `view`, `-1` when `IsViewOutOfBounds` (a detached buffer
/// included). A length-tracking view spans `byte_length - byte_offset` of
/// its buffer; a fixed one is out of bounds when it ends past the buffer.
/// A non-allocating leaf.
fn emit_dv_length(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32, RAW_I32, RAW_I32]);
    let (view, buffer, len, off) = (0, 1, 2, 3);
    wasm!(f; LocalGet(view), I32Load(m32(abi::DV_BUFFER_OFFSET)), LocalSet(buffer));
    block_flag(&mut f, buffer, abi::AB_FLAG_DETACHED);
    wasm!(f;
        If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(buffer), I32Load(m32(abi::AB_BYTE_LENGTH_OFFSET)), LocalSet(len),
        LocalGet(view), I32Load(m32(abi::DV_BYTE_OFFSET_OFFSET)), LocalSet(off),
    );
    block_flag(&mut f, view, abi::DV_FLAG_LENGTH_TRACKING);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(off), LocalGet(len), I32GtU, If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(len), LocalGet(off), I32Sub, Return,
        End,
        // Offsets and lengths are at most 2^29, so the sum cannot wrap.
        LocalGet(off), LocalGet(view), I32Load(m32(abi::DV_BYTE_LENGTH_OFFSET)), I32Add,
        LocalGet(len), I32GtU, If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(view), I32Load(m32(abi::DV_BYTE_LENGTH_OFFSET)),
        End,
    );
    finish_builder(f)
}

/// An unaligned `MemArg` at offset `0` (`DataView` accesses take any byte
/// offset; the align field is only a hint).
const UNALIGNED: MemArg = MemArg {
    offset: 0,
    align: 0,
    memory_index: 0,
};

/// Byte-swap the low 16 bits of the i32 local `x` (the high bits are zero).
fn swap16(f: &mut FnBuilder<'_>, x: u32) {
    wasm!(f;
        LocalGet(x), I32Const(0xff), I32And, I32Const(8), I32Shl,
        LocalGet(x), I32Const(8), I32ShrU, I32Const(0xff), I32And,
        I32Or, LocalSet(x),
    );
}

/// Byte-swap the i32 local `x`.
fn swap32(f: &mut FnBuilder<'_>, x: u32) {
    wasm!(f;
        LocalGet(x), I32Const(8), I32Rotl, I32Const(0x00ff_00ff), I32And,
        LocalGet(x), I32Const(8), I32Rotr, I32Const(0xff00_ff00_u32.cast_signed()), I32And,
        I32Or, LocalSet(x),
    );
}

/// Byte-swap the i64 local `y`: swap the bytes of each 16-bit lane, then the
/// lanes of each 32-bit half, then the halves.
fn swap64(f: &mut FnBuilder<'_>, y: u32) {
    for (mask, shift) in [
        (0x00ff_00ff_00ff_00ff_i64, 8),
        (0x0000_ffff_0000_ffff_i64, 16),
    ] {
        wasm!(f;
            LocalGet(y), I64Const(mask), I64And, I64Const(shift), I64Shl,
            LocalGet(y), I64Const(shift), I64ShrU, I64Const(mask), I64And,
            I64Or, LocalSet(y),
        );
    }
    wasm!(f; LocalGet(y), I64Const(32), I64Rotl, LocalSet(y));
}

/// `if (swap) <swap the local>` for the i32 local `swap`.
fn when_swap(f: &mut FnBuilder<'_>, swap: u32, body: impl FnOnce(&mut FnBuilder<'_>)) {
    wasm!(f; LocalGet(swap), If(BlockType::Empty));
    body(f);
    wasm!(f; End);
}

/// `__dv_get(addr, kind, swap) -> i64` (DESIGN.md §5.8.6): load the element
/// of `kind` (`0` `Int8`, `1` `Uint8`, `3` `Int16`, `4` `Uint16`, `5`
/// `Int32`, `6` `Uint32`, `8` `Float32`, else `Float64`) at the byte address
/// `addr` as integer bits, byte-swapped when `swap` (a big-endian read), and
/// return it as a number jsval. Floats go through `__box_f64`, so a stored
/// NaN of any payload reads as the canonical NaN. A non-allocating leaf.
fn emit_dv_get(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32, RAW_I32], &[RAW_I32, RAW_I64]);
    let (addr, kind, swap, x, y) = (0, 1, 2, 3, 4);
    let is = |f: &mut FnBuilder<'_>, k: i32| {
        wasm!(f; LocalGet(kind), I32Const(k), I32Eq, If(BlockType::Empty));
    };
    // 8-bit kinds: no swap.
    is(&mut f, 0);
    wasm!(f;
        LocalGet(addr), I32Load8S(UNALIGNED), F64ConvertI32S, I64ReinterpretF64, Return, End,
    );
    is(&mut f, 1);
    wasm!(f;
        LocalGet(addr), I32Load8U(UNALIGNED), F64ConvertI32U, I64ReinterpretF64, Return, End,
    );
    // 16-bit kinds.
    wasm!(f;
        LocalGet(kind), I32Const(3), I32Eq, LocalGet(kind), I32Const(4), I32Eq, I32Or,
        If(BlockType::Empty),
        LocalGet(addr), I32Load16U(UNALIGNED), LocalSet(x),
    );
    when_swap(&mut f, swap, |f| swap16(f, x));
    wasm!(f;
        LocalGet(kind), I32Const(3), I32Eq,
        If(BlockType::Empty),
        LocalGet(x), I32Extend16S, F64ConvertI32S, I64ReinterpretF64, Return,
        End,
        LocalGet(x), F64ConvertI32U, I64ReinterpretF64, Return,
        End,
    );
    // 32-bit kinds.
    wasm!(f;
        LocalGet(kind), I32Const(9), I32Ne,
        If(BlockType::Empty),
        LocalGet(addr), I32Load(UNALIGNED), LocalSet(x),
    );
    when_swap(&mut f, swap, |f| swap32(f, x));
    is(&mut f, 5);
    wasm!(f; LocalGet(x), F64ConvertI32S, I64ReinterpretF64, Return, End);
    is(&mut f, 6);
    wasm!(f; LocalGet(x), F64ConvertI32U, I64ReinterpretF64, Return, End);
    wasm!(f;
        LocalGet(x), F32ReinterpretI32, F64PromoteF32, Call(helpers.box_f64), Return,
        End,
    );
    // Float64.
    wasm!(f; LocalGet(addr), I64Load(UNALIGNED), LocalSet(y));
    when_swap(&mut f, swap, |f| swap64(f, y));
    wasm!(f; LocalGet(y), F64ReinterpretI64, Call(helpers.box_f64), End);
    finish_builder(f)
}

/// `__dv_set(addr, kind, n, swap)` (DESIGN.md §5.8.6): convert the number
/// jsval `n` to the bits of `kind` (the kinds of [`emit_dv_get`]) — the
/// `ToInt32` bit pattern stored at the kind's width for the integer kinds
/// (a narrowing store keeps the low bits: the spec's modulo reduction),
/// `f32.demote_f64` for `Float32`, the number's own bits for `Float64` —
/// byte-swap them when `swap` (a big-endian write), and store them at the
/// byte address `addr`. A non-allocating leaf.
fn emit_dv_set(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(
        leaves,
        &[RAW_I32, RAW_I32, RAW_I64, RAW_I32],
        &[RAW_I32, RAW_I64],
    );
    let (addr, kind, n, swap, x, y) = (0, 1, 2, 3, 4, 5);
    // Float64.
    wasm!(f;
        LocalGet(kind), I32Const(9), I32Eq,
        If(BlockType::Empty),
        LocalGet(n), LocalSet(y),
    );
    when_swap(&mut f, swap, |f| swap64(f, y));
    wasm!(f; LocalGet(addr), LocalGet(y), I64Store(UNALIGNED), Return, End);
    // Float32.
    wasm!(f;
        LocalGet(kind), I32Const(8), I32Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(n), F64ReinterpretI64, F32DemoteF64, I32ReinterpretF32,
        Else,
        // ToInt32 (the bitwise operators' `numeric::emit_to_int32`):
        // `trunc(x) - floor(trunc(x) / 2^32) * 2^32`, exact in `f64`, then
        // a saturating unsigned truncation (NaN and the infinities give 0).
        LocalGet(n), F64ReinterpretI64, F64Trunc,
        LocalGet(n), F64ReinterpretI64, F64Trunc,
        F64Const(Ieee64::from(1.0 / 4_294_967_296.0)), F64Mul, F64Floor,
        F64Const(Ieee64::from(4_294_967_296.0)), F64Mul, F64Sub,
        I32TruncSatF64U,
        End,
        LocalSet(x),
        // 8-bit kinds: no swap.
        LocalGet(kind), I32Const(1), I32LeU,
        If(BlockType::Empty), LocalGet(addr), LocalGet(x), I32Store8(UNALIGNED), Return, End,
        // 16-bit kinds.
        LocalGet(kind), I32Const(3), I32Eq, LocalGet(kind), I32Const(4), I32Eq, I32Or,
        If(BlockType::Empty),
        LocalGet(x), I32Const(0xffff), I32And, LocalSet(x),
    );
    when_swap(&mut f, swap, |f| swap16(f, x));
    wasm!(f; LocalGet(addr), LocalGet(x), I32Store16(UNALIGNED), Return, End);
    // 32-bit kinds (integers and Float32).
    when_swap(&mut f, swap, |f| swap32(f, x));
    wasm!(f; LocalGet(addr), LocalGet(x), I32Store(UNALIGNED), End);
    finish_builder(f)
}
