//! Builder forms of the string, array and identity emitters the helper sweep
//! still declares over plain [`Function`]s. Delete this module, and its `use`
//! in the parent, once those emitters accept a [`FnBuilder`].

#[allow(clippy::wildcard_imports)]
use super::*;

const DATA: u64 = abi::STR_DATA_OFFSET as u64;

fn mem(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

fn mem8(offset: u64) -> MemArg {
    mem(offset, 0)
}

fn mem16(offset: u64) -> MemArg {
    mem(offset, 1)
}

fn mem32(offset: u64) -> MemArg {
    mem(offset, 2)
}

/// See `runtime::emit_str_unit`.
pub(super) fn emit_str_unit(function: &mut FnBuilder<'_>, s: u32, i: u32, w: u32) {
    wasm!(function;
        LocalGet(w),
        If(BlockType::Result(ValType::I32)),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        Else,
        LocalGet(s), LocalGet(i), I32Add, I32Load8U(mem8(DATA)),
        End,
    );
}

/// See `runtime::emit_store_unit`.
pub(super) fn emit_store_unit(
    function: &mut FnBuilder<'_>,
    s: u32,
    i: u32,
    w: u32,
    unit: impl Fn(&mut FnBuilder<'_>),
) {
    wasm!(function;
        LocalGet(w),
        If(BlockType::Empty),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add,
    );
    unit(function);
    wasm!(function; I32Store16(mem16(DATA)), Else, LocalGet(s), LocalGet(i), I32Add);
    unit(function);
    wasm!(function; I32Store8(mem8(DATA)), End);
}

/// See `runtime::emit_str_canonical`.
pub(super) fn emit_str_canonical(function: &mut FnBuilder<'_>, w: u32, s: u32, i: u32, n: u32) {
    wasm!(function; LocalGet(w), If(BlockType::Empty));
    emit_str_narrow(function, s, i, n);
    wasm!(function; End);
}

/// See `runtime::emit_str_narrow`.
pub(super) fn emit_str_narrow(function: &mut FnBuilder<'_>, s: u32, i: u32, n: u32) {
    let flags = mem8(u64::from(abi::HEADER_FLAGS_OFFSET));
    wasm!(function;
        LocalGet(s), I32Load(mem32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(n),
        Block(BlockType::Empty),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        I32Const(0xFF), I32GtU, BrIf(2),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(s), LocalGet(i), I32Add,
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        I32Store8(mem8(DATA)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(s), LocalGet(s), I32Load8U(flags), I32Const(!i32::from(abi::STR_FLAG_TWO_BYTE)), I32And, I32Store8(flags),
        End,
    );
}

/// See `props::emit_materialize`.
pub(super) fn emit_materialize(function: &mut FnBuilder<'_>, o: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(o), I32Load(mem32(0)), I32Const(0xFF | i32::from(abi::FUNC_FLAG_LAZY) << 16), I32And, I32Const(TYPE_FUNCTION | i32::from(abi::FUNC_FLAG_LAZY) << 16), I32Eq,
        If(BlockType::Empty), LocalGet(o), Call(helpers.define + 1), End,
    );
}

/// See `props::emit_array_contains`.
pub(super) fn emit_array_contains(
    function: &mut FnBuilder<'_>,
    arr: u32,
    needle: u32,
    i: u32,
    n: u32,
) {
    wasm!(function;
        LocalGet(arr), I32Load(mem32(ARRAY_LEN_OFFSET)), LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Result(ValType::I32)), Loop(BlockType::Empty),
        I32Const(0), LocalGet(i), LocalGet(n), I32GeU, BrIf(1), Drop,
        LocalGet(arr), I32Load(mem32(ARRAY_ELEMS_OFFSET)), LocalGet(i), I32Const(3), I32Shl, I32Add,
        I64Load(mem(ELEMS_DATA_OFFSET, 3)), LocalGet(needle), I64Eq,
        If(BlockType::Empty), I32Const(1), Br(2), End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End,
        Unreachable,
        End,
    );
}

/// See `props::descriptor::emit_same_value`.
pub(super) fn emit_same_value(function: &mut FnBuilder<'_>, a: u32, b: u32, helpers: Helpers<'_>) {
    emit_is_number(function, a);
    emit_is_number(function, b);
    wasm!(function;
        I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(a), LocalGet(b), I64Eq,
        LocalGet(a), F64ReinterpretI64, LocalGet(a), F64ReinterpretI64, F64Ne,
        LocalGet(b), F64ReinterpretI64, LocalGet(b), F64ReinterpretI64, F64Ne,
        I32And, I32Or,
        Else,
        LocalGet(a), LocalGet(b), Call(helpers.same_value_zero),
        End,
    );
}
