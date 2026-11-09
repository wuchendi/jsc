//! Guest Unicode support over the static tables of [`unicode_data`]
//! (design/runtime-memory.md §3.7): binary search over sorted records, full
//! case mapping with `Final_Sigma`, and the `Alphanumeric` lookup of the
//! console renderer.
//!
//! The tables are one raw active data segment between the intern table and
//! the heap: never traced, never referenced by a jsval.

use wasm_encoder::{BlockType, Function, Ieee64, Instruction, MemArg, ValType};

use super::builtins::MAX_STRING_LENGTH;
use super::unicode_data::{
    ALPHANUMERIC, CASE_IGNORABLE, CASE_LOWER, CASE_UPPER, CASED, SPECIAL_LOWER, SPECIAL_UPPER,
};
use super::{
    Helpers, abi, emit_alloc_str, emit_str_two_byte, emit_str_unit, emit_surrogate_pair_at,
};

/// Emit each listed instruction in order.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// Bytes of a case run `(start: u24, len: u8, delta: i32, stride: u8)`.
const RUN_RECORD: usize = 9;
/// Bytes of a special mapping `(cp: u24, len: u8, units: [u16; 3])`.
const SPECIAL_RECORD: usize = 10;
/// Bytes of a range `(start: u24, end: u24)`.
const RANGE_RECORD: usize = 6;

/// Every table with its record size, in data-segment order.
const TABLES: [(&[u8], usize); 7] = [
    (CASE_UPPER, RUN_RECORD),
    (CASE_LOWER, RUN_RECORD),
    (SPECIAL_UPPER, SPECIAL_RECORD),
    (SPECIAL_LOWER, SPECIAL_RECORD),
    (CASED, RANGE_RECORD),
    (CASE_IGNORABLE, RANGE_RECORD),
    (ALPHANUMERIC, RANGE_RECORD),
];

/// Indices into [`TABLES`].
const T_CASE_UPPER: usize = 0;
const T_CASE_LOWER: usize = 1;
const T_SPECIAL_UPPER: usize = 2;
const T_SPECIAL_LOWER: usize = 3;
const T_CASED: usize = 4;
const T_CASE_IGNORABLE: usize = 5;
const T_ALPHANUMERIC: usize = 6;

/// The wasm signatures of this module's helpers, in [`emit_helpers`] order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 5] = {
    use ValType::I32;
    [
        (&[I32, I32, I32, I32], &[I32]), // __unicode_find(base, count, record, cp) -> addr
        (&[I32, I32, I32], &[I32]),      // __unicode_in(base, count, cp) -> bool
        (&[I32, I32], &[I32]),           // __case_simple(cp, upper) -> cp
        (&[I32, I32, I32, I32, I32], &[I32]), // __case_context(s, pos, forward, n, w) -> bool
        (&[I32, I32], &[I32]),           // __case_map(s, upper) -> s | 0
    ]
};

impl Helpers<'_> {
    /// The first helper of this module, right after `_start`.
    fn unicode_find(self) -> u32 {
        self.start() + 1
    }
    fn unicode_in(self) -> u32 {
        self.unicode_find() + 1
    }
    fn case_simple(self) -> u32 {
        self.unicode_find() + 2
    }
    fn case_context(self) -> u32 {
        self.unicode_find() + 3
    }
    /// `__case_map(s, upper)`: the full case mapping of the string block `s`,
    /// or `0` when the result is longer than [`MAX_STRING_LENGTH`].
    pub(super) fn case_map(self) -> u32 {
        self.unicode_find() + 4
    }
}

/// Every helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>) -> Vec<Function> {
    vec![
        emit_find(),
        emit_in(helpers),
        emit_simple(helpers),
        emit_context(helpers),
        emit_map(helpers),
    ]
}

/// The total size of the table data.
pub(super) fn data_size() -> i32 {
    i32::try_from(TABLES.iter().map(|(bytes, _)| bytes.len()).sum::<usize>()).unwrap_or(0)
}

/// The bytes of the table data segment.
pub(super) fn data() -> Vec<u8> {
    TABLES
        .iter()
        .flat_map(|(bytes, _)| bytes.iter().copied())
        .collect()
}

/// The address and record count of table `index`.
fn table(helpers: Helpers<'_>, index: usize) -> (i32, i32) {
    let offset: usize = TABLES[..index].iter().map(|(bytes, _)| bytes.len()).sum();
    let (bytes, record) = TABLES[index];
    (
        helpers.unicode + i32::try_from(offset).unwrap_or(0),
        i32::try_from(bytes.len() / record).unwrap_or(0),
    )
}

/// Push the base and count of table `upper` or `lower`, chosen at run time by
/// the i32 local `flag`.
fn push_either(f: &mut Function, helpers: Helpers<'_>, upper: usize, lower: usize, flag: u32) {
    let (upper_base, upper_count) = table(helpers, upper);
    let (lower_base, lower_count) = table(helpers, lower);
    wasm!(f;
        I32Const(upper_base), I32Const(lower_base), LocalGet(flag), Select,
        I32Const(upper_count), I32Const(lower_count), LocalGet(flag), Select,
    );
}

/// Push `1` when the code point in i32 local `cp` is in range table `index`.
fn push_in(f: &mut Function, helpers: Helpers<'_>, index: usize, cp: u32) {
    let (base, count) = table(helpers, index);
    wasm!(f; I32Const(base), I32Const(count), LocalGet(cp), Call(helpers.unicode_in()));
}

/// Push `1` when the code point in i32 local `cp` is `Alphanumeric`.
pub(super) fn push_is_alphanumeric(f: &mut Function, helpers: Helpers<'_>, cp: u32) {
    push_in(f, helpers, T_ALPHANUMERIC, cp);
}

/// Push the code point of the surrogate pair at unit `at` (i32 local) of the
/// two-byte string block in i32 local `s`.
pub(super) fn push_pair_cp(f: &mut Function, s: u32, at: u32) {
    let address =
        |f: &mut Function| wasm!(f; LocalGet(s), LocalGet(at), I32Const(1), I32Shl, I32Add);
    address(f);
    wasm!(f; I32Load16U(m16(DATA)), I32Const(0xD800), I32Sub, I32Const(10), I32Shl);
    address(f);
    wasm!(f;
        I32Load16U(m16(DATA + 2)), I32Const(0xDC00), I32Sub, I32Add,
        I32Const(0x1_0000), I32Add,
    );
}

/// The string data offset as a [`MemArg`] offset.
const DATA: u64 = abi::STR_DATA_OFFSET as u64;

/// An unaligned byte access at `offset`.
fn m8(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

/// A 2-byte access at `offset`, aligned for string units.
fn m16(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 1,
        memory_index: 0,
    }
}

/// Push the `u24` at the record address in i32 local `record`.
fn push_u24(f: &mut Function, record: u32) {
    wasm!(f; LocalGet(record), I32Load(m8(0)), I32Const(0x00FF_FFFF), I32And);
}

/// `__unicode_find(base, count, record, cp) -> addr`: the address of the last
/// of the `count` records of `record` bytes at `base`, sorted by their leading
/// `u24`, that starts at or before `cp`; `0` when there is none.
fn emit_find() -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (base, hi, record, cp, lo, mid) = (0, 1, 2, 3, 4, 5);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(lo), LocalGet(hi), I32GeU, BrIf(1),
        LocalGet(lo), LocalGet(hi), I32Add, I32Const(1), I32ShrU, LocalSet(mid),
        LocalGet(base), LocalGet(mid), LocalGet(record), I32Mul, I32Add,
        I32Load(m8(0)), I32Const(0x00FF_FFFF), I32And, LocalGet(cp), I32LeU,
        If(BlockType::Empty),
        LocalGet(mid), I32Const(1), I32Add, LocalSet(lo),
        Else,
        LocalGet(mid), LocalSet(hi),
        End,
        Br(0), End, End,
        LocalGet(lo), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(base), LocalGet(lo), I32Const(1), I32Sub, LocalGet(record), I32Mul, I32Add,
        End,
    );
    f
}

/// `__unicode_in(base, count, cp) -> bool`: whether `cp` is in the range table
/// of `count` records at `base`.
fn emit_in(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(1, ValType::I32)]);
    let (base, count, cp, record) = (0, 1, 2, 3);
    let size = i32::try_from(RANGE_RECORD).unwrap_or(0);
    wasm!(f;
        LocalGet(base), LocalGet(count), I32Const(size), LocalGet(cp), Call(helpers.unicode_find()),
        LocalTee(record), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        // Bytes 2..=5 of the record, so the `end` field reads inside it.
        LocalGet(cp), LocalGet(record), I32Load(m8(2)), I32Const(8), I32ShrU, I32LeU,
        End,
    );
    f
}

/// `__case_simple(cp, upper) -> cp`: the single-scalar case mapping of `cp`,
/// `cp` itself when it has none.
fn emit_simple(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(3, ValType::I32)]);
    let (cp, upper, record, offset, stride) = (0, 1, 2, 3, 4);
    let size = i32::try_from(RUN_RECORD).unwrap_or(0);
    push_either(&mut f, helpers, T_CASE_UPPER, T_CASE_LOWER, upper);
    wasm!(f;
        I32Const(size), LocalGet(cp), Call(helpers.unicode_find()),
        LocalTee(record), I32Eqz, If(BlockType::Empty), LocalGet(cp), Return, End,
        LocalGet(cp),
    );
    push_u24(&mut f, record);
    wasm!(f;
        I32Sub, LocalSet(offset),
        LocalGet(record), I32Load8U(m8(8)), LocalSet(stride),
        LocalGet(offset),
        LocalGet(record), I32Load8U(m8(3)), I32Const(1), I32Sub, LocalGet(stride), I32Mul, I32GtU,
        LocalGet(offset), LocalGet(stride), I32RemU, I32Or,
        If(BlockType::Empty), LocalGet(cp), Return, End,
        LocalGet(cp), LocalGet(record), I32Load(m8(4)), I32Add,
        End,
    );
    f
}

/// `__case_context(s, pos, forward, n, w) -> bool`: starting at unit boundary
/// `pos` of the string block `s` (`n` units, width `w`) and walking forward or
/// backward over its well-formed run, whether the first scalar that is not
/// `Case_Ignorable` exists and is `Cased`. A lone surrogate ends the run.
fn emit_context(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (s, pos, forward, n, w, at, cp) = (0, 1, 2, 3, 4, 5, 6);
    let by_direction = |f: &mut Function, ahead: i32, behind: i32, base: u32| {
        wasm!(f;
            LocalGet(base), I32Const(ahead), I32Add,
            LocalGet(base), I32Const(behind), I32Add,
            LocalGet(forward), Select,
        );
    };
    wasm!(f;
        Loop(BlockType::Empty),
        LocalGet(forward), If(BlockType::Result(ValType::I32)),
        LocalGet(pos), LocalGet(n), I32GeU,
        Else,
        LocalGet(pos), I32Eqz,
        End,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(w), LocalGet(forward), LocalGet(pos), I32Const(2), I32GeU, I32Or, I32And,
        If(BlockType::Result(ValType::I32)),
    );
    by_direction(&mut f, 0, -2, pos);
    wasm!(f; LocalSet(at));
    emit_surrogate_pair_at(&mut f, s, at, n);
    wasm!(f; Else, I32Const(0), End, If(BlockType::Empty));
    push_pair_cp(&mut f, s, at);
    wasm!(f; LocalSet(cp));
    by_direction(&mut f, 2, 0, at);
    wasm!(f; LocalSet(pos), Else);
    by_direction(&mut f, 0, -1, pos);
    wasm!(f; LocalSet(at));
    emit_str_unit(&mut f, s, at, w);
    wasm!(f;
        LocalTee(cp), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq,
        If(BlockType::Empty), I32Const(0), Return, End,
    );
    by_direction(&mut f, 1, 0, at);
    wasm!(f; LocalSet(pos), End);
    push_in(&mut f, helpers, T_CASE_IGNORABLE, cp);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    push_in(&mut f, helpers, T_CASED, cp);
    wasm!(f; Return, End, Br(0), End, I32Const(0), End);
    f
}

/// Output the first `count` (i32 local) of the three `units`: with `dst`
/// still `0` only note whether one needs two bytes in `wide`, else store them
/// at index `j` of the block `dst`; `j` advances either way.
fn put_units(f: &mut Function, units: [u32; 3], count: u32, dst: u32, wide: u32, j: u32) {
    for (slot, unit) in (0..).zip(units) {
        wasm!(f;
            LocalGet(count), I32Const(slot), I32GtU, If(BlockType::Empty),
            LocalGet(dst), I32Eqz, If(BlockType::Empty),
            LocalGet(unit), I32Const(0xFF), I32GtU, If(BlockType::Empty),
            I32Const(1), LocalSet(wide),
            End,
            Else,
            LocalGet(wide), If(BlockType::Empty),
            LocalGet(dst), LocalGet(j), I32Const(1), I32Shl, I32Add, LocalGet(unit),
            I32Store16(m16(DATA)),
            Else,
            LocalGet(dst), LocalGet(j), I32Add, LocalGet(unit), I32Store8(m8(DATA)),
            End,
            End,
            LocalGet(j), I32Const(1), I32Add, LocalSet(j),
            End,
        );
    }
}

/// `__case_map(s, upper) -> s | 0`: the full default case mapping of the
/// string block `s`. A first pass measures the result (length and width), a
/// second writes it; the string is one-byte when every unit fits. A lone
/// surrogate passes through as its unit.
fn emit_map(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(12, ValType::I32)]);
    let (s, upper, len, w, i, cp, out, dst, wide, record, count) =
        (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    let (u0, u1, u2) = (11, 12, 13);
    let size = i32::try_from(SPECIAL_RECORD).unwrap_or(0);
    wasm!(f; LocalGet(s), I32Load(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }), LocalSet(len));
    emit_str_two_byte(&mut f, s);
    wasm!(f;
        LocalSet(w),
        Loop(BlockType::Empty),
        I32Const(0), LocalSet(i), I32Const(0), LocalSet(out),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
    );
    emit_str_unit(&mut f, s, i, w);
    wasm!(f; LocalTee(cp), LocalSet(u0), I32Const(1), LocalSet(count));
    // A scalar maps; a lone surrogate passes through as `u0`.
    wasm!(f;
        LocalGet(cp), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq, LocalGet(w), I32And,
        If(BlockType::Result(ValType::I32)),
    );
    emit_surrogate_pair_at(&mut f, s, i, len);
    wasm!(f; If(BlockType::Result(ValType::I32)));
    push_pair_cp(&mut f, s, i);
    wasm!(f;
        LocalSet(cp), LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        I32Const(1),
        Else, I32Const(0), End,
        Else, I32Const(1), End,
        If(BlockType::Empty),
        LocalGet(upper), I32Eqz, LocalGet(cp), I32Const(0x3A3), I32Eq, I32And,
        If(BlockType::Empty),
    );
    let context = |f: &mut Function, pos: i32, forward: i32| {
        wasm!(f;
            LocalGet(s), LocalGet(i), I32Const(pos), I32Add, I32Const(forward),
            LocalGet(len), LocalGet(w), Call(helpers.case_context()),
        );
    };
    // `Final_Sigma`: a cased scalar before the sigma and none after it.
    wasm!(f; I32Const(0x3C2), I32Const(0x3C3));
    context(&mut f, 0, 0);
    context(&mut f, 1, 1);
    wasm!(f; I32Eqz, I32And, Select, LocalSet(u0), Else);
    push_either(&mut f, helpers, T_SPECIAL_UPPER, T_SPECIAL_LOWER, upper);
    wasm!(f;
        I32Const(size), LocalGet(cp), Call(helpers.unicode_find()), LocalTee(record),
        If(BlockType::Result(ValType::I32)),
    );
    push_u24(&mut f, record);
    wasm!(f;
        LocalGet(cp), I32Eq,
        Else, I32Const(0), End,
        If(BlockType::Empty),
        LocalGet(record), I32Load8U(m8(3)), LocalSet(count),
        LocalGet(record), I32Load16U(m8(4)), LocalSet(u0),
        LocalGet(record), I32Load16U(m8(6)), LocalSet(u1),
        LocalGet(record), I32Load16U(m8(8)), LocalSet(u2),
        Else,
        LocalGet(cp), LocalGet(upper), Call(helpers.case_simple()), LocalTee(cp),
        I32Const(0x1_0000), I32GeU,
        If(BlockType::Empty),
        LocalGet(cp), I32Const(0x1_0000), I32Sub, I32Const(10), I32ShrU, I32Const(0xD800), I32Add,
        LocalSet(u0),
        LocalGet(cp), I32Const(0x3FF), I32And, I32Const(0xDC00), I32Add, LocalSet(u1),
        I32Const(2), LocalSet(count),
        Else,
        LocalGet(cp), LocalSet(u0),
        End,
        End,
        End,
        End,
    );
    put_units(&mut f, [u0, u1, u2], count, dst, wide, out);
    wasm!(f;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0), End, End,
        LocalGet(dst), If(BlockType::Empty), LocalGet(dst), Return, End,
        LocalGet(out), F64ConvertI32U, F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt,
        If(BlockType::Empty), I32Const(0), Return, End,
    );
    emit_alloc_str(
        &mut f,
        helpers.alloc,
        |f| wasm!(f; LocalGet(out)),
        |f| wasm!(f; LocalGet(wide)),
        dst,
    );
    wasm!(f; Br(0), End, I32Const(0), End);
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_tables_within_budget() {
        assert!(
            usize::try_from(data_size()).unwrap_or(usize::MAX) <= 96 * 1024,
            "tables total {} bytes",
            data_size()
        );
        for (bytes, record) in TABLES {
            assert_eq!(bytes.len() % record, 0, "whole records");
        }
    }
}
