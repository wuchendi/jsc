//! Guest Unicode support over the static tables of [`unicode_data`]
//! (design/runtime-memory.md §3.7): binary search over sorted records, full
//! case mapping with `Final_Sigma`, Unicode normalization, and the
//! `Alphanumeric` lookup of the console renderer.
//!
//! The tables are one raw active data segment between the intern table and
//! the heap: never traced, never referenced by a jsval.

use wasm_encoder::{BlockType, Function, Ieee64, Instruction, MemArg, ValType};

use super::builtins::MAX_STRING_LENGTH;
use super::unicode_data::{
    ALPHANUMERIC, CASE_IGNORABLE, CASE_LOWER, CASE_UPPER, CASED, CCC, COMPOSITIONS,
    DECOMPOSITION_POOL, DECOMPOSITIONS, SPECIAL_LOWER, SPECIAL_UPPER,
};
use super::{
    FnBuilder, Helpers, LeafSet, LocalClass, RAW_I32, ROOT_BASE, Sink, abi, emit_alloc_str,
    emit_str_two_byte, emit_str_unit, emit_surrogate_pair_at, finish_builder, helper_builder,
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

/// Bytes of a combining class run `(start: u24, len: u8, ccc: u8)`.
const CCC_RECORD: usize = 5;
/// Bytes of a decomposition `(cp: u24, flags: u8, off: u16, len: u8)`.
const DECOMPOSITION_RECORD: usize = 7;
/// Bytes of one `u24` code point of the expansion pool.
const POOL_RECORD: usize = 3;
/// Bytes of a composition `(first: u24, second: u24, composite: u24)`.
const COMPOSITION_RECORD: usize = 9;

/// Every table with its record size, in data-segment order.
const TABLES: [(&[u8], usize); 11] = [
    (CASE_UPPER, RUN_RECORD),
    (CASE_LOWER, RUN_RECORD),
    (SPECIAL_UPPER, SPECIAL_RECORD),
    (SPECIAL_LOWER, SPECIAL_RECORD),
    (CASED, RANGE_RECORD),
    (CASE_IGNORABLE, RANGE_RECORD),
    (ALPHANUMERIC, RANGE_RECORD),
    (CCC, CCC_RECORD),
    (DECOMPOSITIONS, DECOMPOSITION_RECORD),
    (DECOMPOSITION_POOL, POOL_RECORD),
    (COMPOSITIONS, COMPOSITION_RECORD),
];

/// Indices into [`TABLES`].
const T_CASE_UPPER: usize = 0;
const T_CASE_LOWER: usize = 1;
const T_SPECIAL_UPPER: usize = 2;
const T_SPECIAL_LOWER: usize = 3;
const T_CASED: usize = 4;
const T_CASE_IGNORABLE: usize = 5;
const T_ALPHANUMERIC: usize = 6;
const T_CCC: usize = 7;
const T_DECOMPOSITIONS: usize = 8;
const T_DECOMPOSITION_POOL: usize = 9;
const T_COMPOSITIONS: usize = 10;

/// `form` bit of `__normalize`: compose after decomposing (NFC, NFKC).
pub(super) const FORM_COMPOSE: i32 = 1;
/// `form` bit of `__normalize`: use the compatibility mappings (NFKC, NFKD).
const FORM_COMPAT: i32 = 2;

/// Below this code point every combining class is `0`.
const FIRST_NON_STARTER: i32 = 0x300;
/// The bit position of the combining class in a working cell (§ `__normalize`).
const CELL_CCC_SHIFT: i32 = 24;
/// The code point bits of a working cell.
const CELL_CP_MASK: i32 = 0x00FF_FFFF;

/// The Hangul syllable algorithm (UAX #15 §16).
const S_BASE: i32 = 0xAC00;
const L_BASE: i32 = 0x1100;
const V_BASE: i32 = 0x1161;
const T_BASE: i32 = 0x11A7;
const L_COUNT: i32 = 19;
const V_COUNT: i32 = 21;
const T_COUNT: i32 = 28;
const N_COUNT: i32 = V_COUNT * T_COUNT;
const S_COUNT: i32 = L_COUNT * N_COUNT;

/// The wasm signatures of this module's helpers, in [`emit_helpers`] order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 9] = {
    use ValType::I32;
    [
        (&[I32, I32, I32, I32], &[I32]), // __unicode_find(base, count, record, cp) -> addr
        (&[I32, I32, I32], &[I32]),      // __unicode_in(base, count, cp) -> bool
        (&[I32, I32], &[I32]),           // __case_simple(cp, upper) -> cp
        (&[I32, I32, I32, I32, I32], &[I32]), // __case_context(s, pos, forward, n, w) -> bool
        (&[I32, I32], &[I32]),           // __case_map(s, upper) -> s | 0
        (&[I32], &[I32]),                // __normalize_form(s) -> form | -1
        (&[I32, I32, I32, I32], &[I32]), // __decompose(cp, compat, buf, n) -> n
        (&[I32, I32], &[I32]),           // __compose(first, second) -> cp | 0
        (&[I32, I32], &[I32]),           // __normalize(s, form) -> s | 0
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
    /// `__normalize_form(s)`: the `__normalize` form of the string block `s`,
    /// or `-1` when it names no normalization form.
    pub(super) fn normalize_form(self) -> u32 {
        self.unicode_find() + 5
    }
    fn decompose(self) -> u32 {
        self.unicode_find() + 6
    }
    fn compose(self) -> u32 {
        self.unicode_find() + 7
    }
    /// `__normalize(s, form)`: the normalization of the string block `s`, or
    /// `0` when the result is longer than [`MAX_STRING_LENGTH`].
    pub(super) fn normalize(self) -> u32 {
        self.unicode_find() + 8
    }
}

/// Every helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>, leaves: &LeafSet) -> Vec<Function> {
    vec![
        emit_find(leaves),
        emit_in(helpers, leaves),
        emit_simple(helpers, leaves),
        emit_context(helpers, leaves),
        emit_map(helpers, leaves),
        emit_normalize_form(leaves),
        emit_decompose(helpers, leaves),
        emit_compose(helpers, leaves),
        emit_normalize(helpers, leaves),
    ]
}

/// The helpers that never reach `__alloc`, with their declared callees for
/// [`leaf_set`](super::roots::leaf_set). `__case_map` and `__normalize`
/// allocate their results and stay safepoints.
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.unicode_find(), vec![]),
        (helpers.unicode_in(), vec![helpers.unicode_find()]),
        (helpers.case_simple(), vec![helpers.unicode_find()]),
        (helpers.case_context(), vec![helpers.unicode_in()]),
        (helpers.normalize_form(), vec![]),
        (helpers.decompose(), vec![helpers.unicode_find()]),
        (helpers.compose(), vec![helpers.unicode_find()]),
    ]
}

/// The locals of `__case_map` after its parameters: `dst` is the result block,
/// the only heap reference; `record` addresses a static table.
const CASE_MAP_LOCALS: [LocalClass; 12] = [
    RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32, RAW_I32,
    RAW_I32, RAW_I32,
];

/// The locals of `__normalize` after its parameters, in [`NormalizeLocals`]
/// order: `buf` and `dst` are block bases held across allocation; `addr` is
/// an interior address into `buf` or an index, never live across one.
const NORMALIZE_LOCALS: [LocalClass; 16] = [
    RAW_I32, RAW_I32, RAW_I32, RAW_I32, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32,
    RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, ROOT_BASE,
];

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
fn push_either(f: &mut FnBuilder<'_>, helpers: Helpers<'_>, upper: usize, lower: usize, flag: u32) {
    let (upper_base, upper_count) = table(helpers, upper);
    let (lower_base, lower_count) = table(helpers, lower);
    wasm!(f;
        I32Const(upper_base), I32Const(lower_base), LocalGet(flag), Select,
        I32Const(upper_count), I32Const(lower_count), LocalGet(flag), Select,
    );
}

/// Push `1` when the code point in i32 local `cp` is in range table `index`.
fn push_in(f: &mut FnBuilder<'_>, helpers: Helpers<'_>, index: usize, cp: u32) {
    let (base, count) = table(helpers, index);
    wasm!(f; I32Const(base), I32Const(count), LocalGet(cp), Call(helpers.unicode_in()));
}

/// Push `1` when the code point in i32 local `cp` is `Alphanumeric`.
pub(super) fn push_is_alphanumeric(f: &mut FnBuilder<'_>, helpers: Helpers<'_>, cp: u32) {
    push_in(f, helpers, T_ALPHANUMERIC, cp);
}

/// Push the code point of the surrogate pair at unit `at` (i32 local) of the
/// two-byte string block in i32 local `s`.
pub(super) fn push_pair_cp(f: &mut FnBuilder<'_>, s: u32, at: u32) {
    let address =
        |f: &mut FnBuilder<'_>| wasm!(f; LocalGet(s), LocalGet(at), I32Const(1), I32Shl, I32Add);
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

/// A 4-byte access at `offset`, aligned for header fields and working cells.
fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
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
fn push_u24(f: &mut FnBuilder<'_>, record: u32) {
    wasm!(f; LocalGet(record), I32Load(m8(0)), I32Const(0x00FF_FFFF), I32And);
}

/// `__unicode_find(base, count, record, cp) -> addr`: the address of the last
/// of the `count` records of `record` bytes at `base`, sorted by their leading
/// `u24`, that starts at or before `cp`; `0` when there is none.
fn emit_find(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32; 4], &[RAW_I32; 2]);
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
    finish_builder(f)
}

/// `__unicode_in(base, count, cp) -> bool`: whether `cp` is in the range table
/// of `count` records at `base`.
fn emit_in(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32; 3], &[RAW_I32; 1]);
    let (base, count, cp, record) = (0, 1, 2, 3);
    let size = i32::try_from(RANGE_RECORD).unwrap_or(0);
    wasm!(f;
        LocalGet(base), LocalGet(count), I32Const(size), LocalGet(cp), Call(helpers.unicode_find()),
        LocalTee(record), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        // Bytes 2..=5 of the record, so the `end` field reads inside it.
        LocalGet(cp), LocalGet(record), I32Load(m8(2)), I32Const(8), I32ShrU, I32LeU,
        End,
    );
    finish_builder(f)
}

/// `__case_simple(cp, upper) -> cp`: the single-scalar case mapping of `cp`,
/// `cp` itself when it has none.
fn emit_simple(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32; 2], &[RAW_I32; 3]);
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
    finish_builder(f)
}

/// `__case_context(s, pos, forward, n, w) -> bool`: starting at unit boundary
/// `pos` of the string block `s` (`n` units, width `w`) and walking forward or
/// backward over its well-formed run, whether the first scalar that is not
/// `Case_Ignorable` exists and is `Cased`. A lone surrogate ends the run.
fn emit_context(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, RAW_I32, RAW_I32, RAW_I32, RAW_I32],
        &[RAW_I32; 2],
    );
    let (s, pos, forward, n, w, at, cp) = (0, 1, 2, 3, 4, 5, 6);
    let by_direction = |f: &mut FnBuilder<'_>, ahead: i32, behind: i32, base: u32| {
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
    finish_builder(f)
}

/// Output the first `count` (i32 local) of the three `units`: with `dst`
/// still `0` only note whether one needs two bytes in `wide`, else store them
/// at index `j` of the block `dst`; `j` advances either way.
fn put_units(f: &mut FnBuilder<'_>, units: [u32; 3], count: u32, dst: u32, wide: u32, j: u32) {
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
fn emit_map(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32], &CASE_MAP_LOCALS);
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
    let context = |f: &mut FnBuilder<'_>, pos: i32, forward: i32| {
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
    finish_builder(f)
}

/// `__normalize_form(s) -> form | -1`: the form named by the string block `s`
/// (`NFC`, `NFD`, `NFKC`, `NFKD`), as [`FORM_COMPOSE`] and [`FORM_COMPAT`] bits.
fn emit_normalize_form(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[RAW_I32; 4]);
    let (s, len, w, i, form) = (0, 1, 2, 3, 4);
    // Refuse unless unit `at` of `s` is `unit`.
    let expect = |f: &mut FnBuilder<'_>, at: i32, unit: u8| {
        wasm!(f; I32Const(at), LocalSet(i));
        emit_str_unit(f, s, i, w);
        wasm!(f;
            I32Const(i32::from(unit)), I32Ne,
            If(BlockType::Empty), I32Const(-1), Return, End,
        );
    };
    wasm!(f; LocalGet(s), I32Load(m32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(len));
    emit_str_two_byte(&mut f, s);
    wasm!(f;
        LocalSet(w),
        LocalGet(len), I32Const(3), I32Sub, I32Const(1), I32GtU,
        If(BlockType::Empty), I32Const(-1), Return, End,
    );
    expect(&mut f, 0, b'N');
    expect(&mut f, 1, b'F');
    wasm!(f; LocalGet(len), I32Const(1), I32Sub, LocalSet(i));
    emit_str_unit(&mut f, s, i, w);
    wasm!(f;
        LocalTee(form), I32Const(i32::from(b'C')), I32Ne,
        LocalGet(form), I32Const(i32::from(b'D')), I32Ne, I32And,
        If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(form), I32Const(i32::from(b'C')), I32Eq, LocalSet(form),
        LocalGet(len), I32Const(4), I32Eq,
        If(BlockType::Empty),
    );
    expect(&mut f, 2, b'K');
    wasm!(f;
        LocalGet(form), I32Const(FORM_COMPAT), I32Or, LocalSet(form),
        End,
        LocalGet(form),
        End,
    );
    finish_builder(f)
}

/// Output one code point at cell `n` of the working buffer `buf` (i32
/// locals): with `buf` still `0` only count it; `n` advances either way.
fn put_cell(f: &mut FnBuilder<'_>, buf: u32, n: u32, value: impl Fn(&mut FnBuilder<'_>)) {
    wasm!(f;
        LocalGet(buf), If(BlockType::Empty),
        LocalGet(buf), LocalGet(n), I32Const(2), I32Shl, I32Add,
    );
    value(f);
    wasm!(f;
        I32Store(m32(DATA)),
        End,
        LocalGet(n), I32Const(1), I32Add, LocalSet(n),
    );
}

/// `__decompose(cp, compat, buf, n) -> n`: output the full canonical (or, with
/// `compat`, compatibility) decomposition of `cp` from cell `n` of the working
/// buffer `buf`, returning the next free cell. A `buf` of `0` only counts.
fn emit_decompose(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(
        leaves,
        &[RAW_I32, RAW_I32, ROOT_BASE, RAW_I32],
        &[RAW_I32; 4],
    );
    let (cp, compat, buf, n, record, at, count, h) = (0, 1, 2, 3, 4, 5, 6, 7);
    let (base, records) = table(helpers, T_DECOMPOSITIONS);
    let (pool, _) = table(helpers, T_DECOMPOSITION_POOL);
    let size = i32::try_from(DECOMPOSITION_RECORD).unwrap_or(0);
    // Clear `record` unless it belongs to `cp`.
    let keep_own = |f: &mut FnBuilder<'_>| {
        push_u24(f, record);
        wasm!(f;
            LocalGet(cp), I32Ne,
            If(BlockType::Empty), I32Const(0), LocalSet(record), End,
        );
    };
    wasm!(f;
        LocalGet(cp), I32Const(S_BASE), I32Sub, LocalTee(h), I32Const(S_COUNT), I32LtU,
        If(BlockType::Empty),
    );
    put_cell(&mut f, buf, n, |f| {
        wasm!(f; I32Const(L_BASE), LocalGet(h), I32Const(N_COUNT), I32DivU, I32Add);
    });
    put_cell(&mut f, buf, n, |f| {
        wasm!(f;
            I32Const(V_BASE), LocalGet(h), I32Const(N_COUNT), I32RemU,
            I32Const(T_COUNT), I32DivU, I32Add,
        );
    });
    wasm!(f; LocalGet(h), I32Const(T_COUNT), I32RemU, If(BlockType::Empty));
    put_cell(&mut f, buf, n, |f| {
        wasm!(f; I32Const(T_BASE), LocalGet(h), I32Const(T_COUNT), I32RemU, I32Add);
    });
    wasm!(f;
        End,
        LocalGet(n), Return,
        End,
        I32Const(base), I32Const(records), I32Const(size), LocalGet(cp),
        Call(helpers.unicode_find()),
        LocalTee(record), If(BlockType::Empty),
    );
    keep_own(&mut f);
    wasm!(f;
        End,
        // The last record of `cp` applies, unless it is compatibility-only
        // and the form is canonical: then a canonical record precedes it, or
        // `cp` has no canonical decomposition.
        LocalGet(record), If(BlockType::Empty),
        LocalGet(compat), I32Eqz, LocalGet(record), I32Load8U(m8(3)), I32And,
        If(BlockType::Empty),
        LocalGet(record), I32Const(size), I32Sub, LocalTee(record),
        I32Const(base), I32LtU,
        If(BlockType::Empty), I32Const(0), LocalSet(record), Else,
    );
    keep_own(&mut f);
    wasm!(f;
        End,
        End,
        End,
        LocalGet(record), I32Eqz, If(BlockType::Empty),
    );
    put_cell(&mut f, buf, n, |f| wasm!(f; LocalGet(cp)));
    wasm!(f;
        LocalGet(n), Return,
        End,
        LocalGet(record), I32Load16U(m8(4)), I32Const(3), I32Mul, I32Const(pool), I32Add, LocalSet(at),
        LocalGet(record), I32Load8U(m8(6)), LocalSet(count),
        Loop(BlockType::Empty),
    );
    put_cell(&mut f, buf, n, |f| push_u24(f, at));
    wasm!(f;
        LocalGet(at), I32Const(3), I32Add, LocalSet(at),
        LocalGet(count), I32Const(1), I32Sub, LocalTee(count), BrIf(0),
        End,
        LocalGet(n),
        End,
    );
    finish_builder(f)
}

/// `__compose(first, second) -> cp | 0`: the primary composite of the pair,
/// `0` when it has none.
fn emit_compose(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32; 2], &[RAW_I32; 2]);
    let (first, second, record, second_of) = (0, 1, 2, 3);
    let (base, records) = table(helpers, T_COMPOSITIONS);
    let size = i32::try_from(COMPOSITION_RECORD).unwrap_or(0);
    wasm!(f;
        // L + V
        LocalGet(first), I32Const(L_BASE), I32Sub, I32Const(L_COUNT), I32LtU,
        LocalGet(second), I32Const(V_BASE), I32Sub, I32Const(V_COUNT), I32LtU, I32And,
        If(BlockType::Empty),
        I32Const(S_BASE),
        LocalGet(first), I32Const(L_BASE), I32Sub, I32Const(N_COUNT), I32Mul, I32Add,
        LocalGet(second), I32Const(V_BASE), I32Sub, I32Const(T_COUNT), I32Mul, I32Add,
        Return,
        End,
        // LV + T
        LocalGet(first), I32Const(S_BASE), I32Sub, I32Const(S_COUNT), I32LtU,
        LocalGet(first), I32Const(S_BASE), I32Sub, I32Const(T_COUNT), I32RemU, I32Eqz, I32And,
        LocalGet(second), I32Const(T_BASE + 1), I32Sub, I32Const(T_COUNT - 1), I32LtU, I32And,
        If(BlockType::Empty),
        LocalGet(first), LocalGet(second), I32Const(T_BASE), I32Sub, I32Add,
        Return,
        End,
        I32Const(base), I32Const(records), I32Const(size), LocalGet(first),
        Call(helpers.unicode_find()), LocalSet(record),
        Block(BlockType::Empty),
        LocalGet(record), I32Eqz, BrIf(0),
        // The records of `first` end at the one found.
        Loop(BlockType::Empty),
        LocalGet(record), I32Const(base), I32LtU, BrIf(1),
    );
    push_u24(&mut f, record);
    wasm!(f;
        LocalGet(first), I32Ne, BrIf(1),
        LocalGet(record), I32Load(m8(3)), I32Const(CELL_CP_MASK), I32And, LocalSet(second_of),
        LocalGet(second_of), LocalGet(second), I32Eq,
        If(BlockType::Empty),
        LocalGet(record), I32Load(m8(6)), I32Const(CELL_CP_MASK), I32And, Return,
        End,
        LocalGet(record), I32Const(size), I32Sub, LocalSet(record),
        Br(0),
        End,
        End,
        I32Const(0),
        End,
    );
    finish_builder(f)
}

/// Run `body` for each `i` (i32 local) from its current value up to the i32
/// local `limit`. The body sits two labels deep: `Br(1)` breaks.
fn count_up(f: &mut FnBuilder<'_>, i: u32, limit: u32, body: impl FnOnce(&mut FnBuilder<'_>)) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(limit), I32GeU, BrIf(1),
    );
    body(f);
    wasm!(f; LocalGet(i), I32Const(1), I32Add, LocalSet(i), Br(0), End, End);
}

/// The locals of `__normalize`: the parameters `s` and `form`, then its own.
#[derive(Clone, Copy)]
struct NormalizeLocals {
    s: u32,
    form: u32,
    len: u32,
    width: u32,
    i: u32,
    cp: u32,
    buf: u32,
    count: u32,
    addr: u32,
    record: u32,
    class: u32,
    j: u32,
    cell: u32,
    starter: u32,
    last: u32,
    units: u32,
    wide: u32,
    dst: u32,
}

const NORMALIZE: NormalizeLocals = NormalizeLocals {
    s: 0,
    form: 1,
    len: 2,
    width: 3,
    i: 4,
    cp: 5,
    buf: 6,
    count: 7,
    addr: 8,
    record: 9,
    class: 10,
    j: 11,
    cell: 12,
    starter: 13,
    last: 14,
    units: 15,
    wide: 16,
    dst: 17,
};

/// Push the address (before the data offset) of the working cell whose index
/// is in i32 local `index`.
fn cell_at(f: &mut FnBuilder<'_>, index: u32) {
    wasm!(f; LocalGet(NORMALIZE.buf), LocalGet(index), I32Const(2), I32Shl, I32Add);
}

/// `__normalize(s, form) -> s | 0`: the normalization of the string block `s`
/// (UAX #15), `0` when the result is longer than [`MAX_STRING_LENGTH`].
///
/// A one-byte receiver that the form leaves alone returns itself. Otherwise
/// the decomposition goes into a working buffer of `u32` cells, a two-unit
/// per cell string block so it stays an ordinary leaf allocation: a first pass
/// counts the cells, a second writes them. A cell holds the code point with
/// its combining class in the top byte, so reordering and composing never
/// repeat the table lookup. A lone surrogate is a cell of its own unit: class
/// `0`, no mappings.
fn emit_normalize(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[ROOT_BASE, RAW_I32], &NORMALIZE_LOCALS);
    let NormalizeLocals {
        s,
        form,
        len,
        width,
        i,
        ..
    } = NORMALIZE;
    wasm!(f; LocalGet(s), I32Load(m32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(len));
    emit_str_two_byte(&mut f, s);
    wasm!(f; LocalTee(width), I32Eqz, If(BlockType::Empty));
    // Latin-1 is NFC already; ASCII is every form already.
    wasm!(f;
        LocalGet(form), I32Const(FORM_COMPOSE), I32Eq,
        If(BlockType::Empty), LocalGet(s), Return, End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU,
        If(BlockType::Empty), LocalGet(s), Return, End,
        LocalGet(s), LocalGet(i), I32Add, I32Load8U(m8(DATA)), I32Const(0x80), I32GeU, BrIf(1),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0), End, End,
        End,
    );
    normalize_decompose(&mut f, helpers);
    normalize_classes(&mut f, helpers);
    normalize_order(&mut f);
    normalize_compose(&mut f, helpers);
    normalize_encode(&mut f, helpers);
    wasm!(f; LocalGet(NORMALIZE.dst), End);
    finish_builder(f)
}

/// The working buffer: the full decomposition of every scalar.
fn normalize_decompose(f: &mut FnBuilder<'_>, helpers: Helpers<'_>) {
    let NormalizeLocals {
        s,
        form,
        len,
        width,
        i,
        cp,
        buf,
        count,
        ..
    } = NORMALIZE;
    // Decompose: count on the first pass, write on the second.
    wasm!(f;
        Loop(BlockType::Empty),
        I32Const(0), LocalSet(i), I32Const(0), LocalSet(count),
    );
    count_up(f, i, len, |f| {
        emit_str_unit(f, s, i, width);
        wasm!(f;
            LocalTee(cp), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq, LocalGet(width), I32And,
            If(BlockType::Empty),
        );
        emit_surrogate_pair_at(f, s, i, len);
        wasm!(f; If(BlockType::Empty));
        push_pair_cp(f, s, i);
        wasm!(f;
            LocalSet(cp), LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            End,
            End,
            LocalGet(cp), LocalGet(form), I32Const(FORM_COMPAT), I32And, I32Const(0), I32Ne,
            LocalGet(buf), LocalGet(count), Call(helpers.decompose()), LocalTee(count),
            F64ConvertI32U, F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt,
            If(BlockType::Empty), I32Const(0), Return, End,
        );
    });
    wasm!(f; LocalGet(buf), I32Eqz, If(BlockType::Empty));
    emit_alloc_str(
        f,
        helpers.alloc,
        |f| wasm!(f; LocalGet(count), I32Const(1), I32Shl),
        |f| wasm!(f; I32Const(1)),
        buf,
    );
    wasm!(f; Br(1), End, End);
}

/// Tag each working cell with its combining class.
fn normalize_classes(f: &mut FnBuilder<'_>, helpers: Helpers<'_>) {
    let NormalizeLocals {
        i,
        cp,
        count,
        addr,
        record,
        ..
    } = NORMALIZE;
    let (ccc_base, ccc_records) = table(helpers, T_CCC);
    let ccc_size = i32::try_from(CCC_RECORD).unwrap_or(0);
    wasm!(f; I32Const(0), LocalSet(i));
    // Tag each cell with its combining class.
    count_up(f, i, count, |f| {
        cell_at(f, i);
        wasm!(f;
            LocalTee(addr), I32Load(m32(DATA)), LocalTee(cp),
            I32Const(FIRST_NON_STARTER), I32GeU, If(BlockType::Empty),
            I32Const(ccc_base), I32Const(ccc_records), I32Const(ccc_size), LocalGet(cp),
            Call(helpers.unicode_find()), LocalTee(record), If(BlockType::Empty),
            LocalGet(cp),
        );
        push_u24(f, record);
        wasm!(f;
            I32Sub, LocalGet(record), I32Load8U(m8(3)), I32LtU, If(BlockType::Empty),
            LocalGet(addr), LocalGet(cp),
            LocalGet(record), I32Load8U(m8(4)), I32Const(CELL_CCC_SHIFT), I32Shl, I32Or,
            I32Store(m32(DATA)),
            End,
            End,
            End,
        );
    });
}

/// Canonical ordering of the working cells.
fn normalize_order(f: &mut FnBuilder<'_>) {
    let NormalizeLocals {
        i,
        cp,
        count,
        addr,
        class,
        j,
        cell,
        ..
    } = NORMALIZE;
    // Canonical ordering: a stable insertion sort; a starter (class 0) never
    // moves and stops every non-starter before it.
    wasm!(f; I32Const(1), LocalSet(i));
    count_up(f, i, count, |f| {
        cell_at(f, i);
        wasm!(f;
            I32Load(m32(DATA)), LocalTee(cell), I32Const(CELL_CCC_SHIFT), I32ShrU, LocalTee(class),
            If(BlockType::Empty),
            LocalGet(i), LocalSet(j),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(j), I32Eqz, BrIf(1),
            LocalGet(j), I32Const(1), I32Sub, LocalSet(addr),
        );
        cell_at(f, addr);
        wasm!(f;
            I32Load(m32(DATA)), LocalTee(cp), I32Const(CELL_CCC_SHIFT), I32ShrU,
            LocalGet(class), I32LeU, BrIf(1),
        );
        cell_at(f, j);
        wasm!(f; LocalGet(cp), I32Store(m32(DATA)), LocalGet(addr), LocalSet(j), Br(0), End, End);
        cell_at(f, j);
        wasm!(f; LocalGet(cell), I32Store(m32(DATA)), End);
    });
}

/// Canonical composition of the working cells, for the composing forms.
fn normalize_compose(f: &mut FnBuilder<'_>, helpers: Helpers<'_>) {
    let NormalizeLocals {
        form,
        i,
        cp,
        count,
        class,
        j,
        cell,
        starter,
        last,
        ..
    } = NORMALIZE;
    // Canonical composition, compacting in place: `j` is the next output
    // cell, `starter` the last starter's cell (`-1` before the first) and
    // `last` the class of the last cell output.
    wasm!(f;
        LocalGet(form), I32Const(FORM_COMPOSE), I32And, If(BlockType::Empty),
        I32Const(0), LocalSet(i), I32Const(0), LocalSet(j), I32Const(-1), LocalSet(starter),
        I32Const(0), LocalSet(last),
    );
    count_up(f, i, count, |f| {
        cell_at(f, i);
        wasm!(f;
            I32Load(m32(DATA)), LocalTee(cell), I32Const(CELL_CCC_SHIFT), I32ShrU, LocalSet(class),
            Block(BlockType::Empty), Block(BlockType::Empty),
            LocalGet(starter), I32Const(0), I32LtS, BrIf(0),
            // Blocked unless adjacent to the starter, or every cell since it
            // has a smaller non-zero class.
            LocalGet(j), I32Const(1), I32Sub, LocalGet(starter), I32Eq,
            LocalGet(last), I32Const(0), I32Ne, LocalGet(last), LocalGet(class), I32LtU, I32And,
            I32Or, I32Eqz, BrIf(0),
        );
        cell_at(f, starter);
        wasm!(f;
            I32Load(m32(DATA)),
            LocalGet(cell), I32Const(CELL_CP_MASK), I32And,
            Call(helpers.compose()), LocalTee(cp), I32Eqz, BrIf(0),
        );
        cell_at(f, starter);
        wasm!(f;
            LocalGet(cp), I32Store(m32(DATA)),
            Br(1),
            End,
            LocalGet(class), I32Eqz, If(BlockType::Empty), LocalGet(j), LocalSet(starter), End,
        );
        cell_at(f, j);
        wasm!(f;
            LocalGet(cell), I32Store(m32(DATA)),
            LocalGet(j), I32Const(1), I32Add, LocalSet(j),
            LocalGet(class), LocalSet(last),
            End,
        );
    });
    wasm!(f; LocalGet(j), LocalSet(count), End);
}

/// The result string block in `dst`, from the working cells.
fn normalize_encode(f: &mut FnBuilder<'_>, helpers: Helpers<'_>) {
    let NormalizeLocals {
        i,
        cp,
        count,
        j,
        units,
        wide,
        dst,
        ..
    } = NORMALIZE;
    wasm!(f; I32Const(0), LocalSet(i));
    // Measure the UTF-16 result, then encode it at the narrowest width.
    count_up(f, i, count, |f| {
        cell_at(f, i);
        wasm!(f;
            I32Load(m32(DATA)), I32Const(CELL_CP_MASK), I32And, LocalTee(cp),
            I32Const(0xFF), I32GtU, LocalGet(wide), I32Or, LocalSet(wide),
            LocalGet(units), I32Const(1), LocalGet(cp), I32Const(0xFFFF), I32GtU, I32Add, I32Add,
            LocalSet(units),
        );
    });
    wasm!(f;
        LocalGet(units), F64ConvertI32U, F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt,
        If(BlockType::Empty), I32Const(0), Return, End,
    );
    emit_alloc_str(
        f,
        helpers.alloc,
        |f| wasm!(f; LocalGet(units)),
        |f| wasm!(f; LocalGet(wide)),
        dst,
    );
    wasm!(f; I32Const(0), LocalSet(i), I32Const(0), LocalSet(j));
    let put = |f: &mut FnBuilder<'_>, value: &dyn Fn(&mut FnBuilder<'_>)| {
        wasm!(f; LocalGet(wide), If(BlockType::Empty), LocalGet(dst), LocalGet(j), I32Const(1), I32Shl, I32Add);
        value(f);
        wasm!(f; I32Store16(m16(DATA)), Else, LocalGet(dst), LocalGet(j), I32Add);
        value(f);
        wasm!(f; I32Store8(m8(DATA)), End, LocalGet(j), I32Const(1), I32Add, LocalSet(j));
    };
    count_up(f, i, count, |f| {
        cell_at(f, i);
        wasm!(f;
            I32Load(m32(DATA)), I32Const(CELL_CP_MASK), I32And, LocalTee(cp),
            I32Const(0xFFFF), I32GtU, If(BlockType::Empty),
        );
        put(f, &|f| {
            wasm!(f;
                LocalGet(cp), I32Const(0x1_0000), I32Sub, I32Const(10), I32ShrU,
                I32Const(0xD800), I32Add,
            );
        });
        put(f, &|f| {
            wasm!(f; LocalGet(cp), I32Const(0x3FF), I32And, I32Const(0xDC00), I32Add);
        });
        wasm!(f; Else);
        put(f, &|f| wasm!(f; LocalGet(cp)));
        wasm!(f; End);
    });
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
