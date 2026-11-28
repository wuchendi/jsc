//! The binary-data helpers (DESIGN.md §5.8): the byte-block allocator
//! `__ab_alloc` and the console renderer arm `__render_binary`, a helper
//! group of their own after every other ([`super::registry::Group::Binary`]).

use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

use super::{
    FnBuilder, Helpers, LeafSet, RAW_I32, ROOT_BASE, Sink, StrOffsets, abi, finish_builder,
    helper_builder, static_key,
};

/// Emit each listed instruction in order.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

use ValType::I32;

/// The `(params, results)` of the binary helpers, in function-index order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 2] = [
    (&[I32], &[I32]),   // __ab_alloc(size) -> block
    (&[I32, I32], &[]), // __render_binary(p, depth)
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
    let (block, at, count, nibble) = (0, 2, 3, 4);
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
