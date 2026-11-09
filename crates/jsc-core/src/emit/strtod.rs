//! The guest number parsers (design/runtime-memory.md §3.3): `StringToNumber`,
//! `parseFloat` and `parseInt`.
//!
//! Decimal input takes Clinger's exact fast path when it can, and otherwise
//! Simple Decimal Conversion: a port of `core::num::dec2flt`'s
//! `parse_long_mantissa` over a static 768-digit buffer. Everything reads code
//! units through the tagged `data` pointer (`address | 1` for UTF-16), so a
//! lone surrogate is simply a unit that is neither whitespace nor a digit.
//!
//! The digit buffer and its three counters are static: the parsers call no
//! user code and allocate nothing, so they are never re-entered.

use super::builtins::emit_is_whitespace;
use super::value::{emit_is_number, emit_is_string};
use super::{
    BlockType, FALSE_BITS, Function, Helpers, Instruction, MemArg, NULL_BITS, QNAN_BITS, TRUE_BITS,
    ValType, abi,
};

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// The buffer's significant digit count.
const NUM: u64 = abi::DEC_SCRATCH as u64;
/// The buffer's `decimal_point`: the value is `0.d1d2.. * 10^decimal_point`.
const DP: u64 = NUM + 4;
/// Non-zero when digits past the buffer were dropped.
const TRUNC: u64 = NUM + 8;
/// The code-unit index just past the literal `__dec_parse` accepted.
const END_CELL: u64 = NUM + 12;
/// The digit values.
const DIGITS: u64 = NUM + 16;
/// Capacity of the digit buffer.
const MAX_DIGITS: i32 = 768;
/// `decimal_point` magnitude beyond which the value is certainly 0 or infinite.
const DP_RANGE: i32 = 2047;
/// `2^53`, the largest mantissa the fast path represents exactly.
const TWO_53: i64 = 1 << 53;
/// `2^52`, the hidden bit.
const TWO_52: i64 = 1 << 52;
/// The bits of `+Infinity`.
const INF_BITS: i64 = 0x7FF0_0000_0000_0000;
/// The largest explicit shift of one conversion step.
const MAX_SHIFT: i32 = 60;
/// An explicit exponent stops accumulating here: a larger one could only be
/// cancelled by more digits than linear memory can hold.
const EXP_CAP: i32 = 100_000_000;

fn mem8(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

fn mem16(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 1,
        memory_index: 0,
    }
}

fn mem32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn mem64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// Push the i32 field at `addr`.
fn field(f: &mut Function, addr: u64) {
    wasm!(f; I32Const(0), I32Load(mem32(addr)));
}

/// Store the i32 local `local` into the field at `addr`.
fn set_field(f: &mut Function, addr: u64, local: u32) {
    wasm!(f; I32Const(0), LocalGet(local), I32Store(mem32(addr)));
}

/// Store the constant `value` into the field at `addr`.
fn set_field_const(f: &mut Function, addr: u64, value: i32) {
    wasm!(f; I32Const(0), I32Const(value), I32Store(mem32(addr)));
}

/// `local += delta`.
fn bump(f: &mut Function, local: u32, delta: i32) {
    wasm!(f; LocalGet(local), I32Const(delta), I32Add, LocalSet(local));
}

/// Push the `f64` power of ten whose index is in i32 local `index`.
fn pow10(f: &mut Function, index: u32) {
    wasm!(f;
        LocalGet(index), I32Const(3), I32Shl,
        F64Load(mem64(u64::from(abi::DEC_POW10))),
    );
}

/// Push `10^index` for the constant `index`.
fn pow10_const(f: &mut Function, index: i32) {
    wasm!(f; I32Const(index * 8), F64Load(mem64(u64::from(abi::DEC_POW10))));
}

/// Push the canonical NaN as an `f64`.
fn nan(f: &mut Function) {
    wasm!(f; I64Const(QNAN_BITS), F64ReinterpretI64);
}

/// Set i32 local `data` to the tagged unit pointer of the string in i32 local
/// `s`.
fn set_data(f: &mut Function, s: u32, data: u32) {
    wasm!(f;
        LocalGet(s), I32Const(abi::STR_DATA_OFFSET.cast_signed()), I32Add,
        LocalGet(s), I32Load8U(mem8(u64::from(abi::HEADER_FLAGS_OFFSET))),
        I32Const(i32::from(abi::STR_FLAG_TWO_BYTE)), I32And, I32Const(0), I32Ne,
        I32Or, LocalSet(data),
    );
}

/// Advance i32 local `pos` past the whitespace units before `end`.
fn skip_leading_ws(f: &mut Function, h: Helpers<'_>, [data, pos, end]: [u32; 3]) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pos), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(data), LocalGet(pos), Call(h.num_unit()), Call(h.str_ws()), I32Eqz, BrIf(1),
    );
    bump(f, pos, 1);
    wasm!(f; Br(0), End, End);
}

/// i32 local `d` = the digit value of the unit in i32 local `c` (letters
/// case-insensitive), or `99` for a non-digit.
fn digit_value(f: &mut Function, c: u32, d: u32) {
    wasm!(f;
        LocalGet(c), I32Const(48), I32Sub, LocalTee(d), I32Const(10), I32GeU,
        If(BlockType::Empty),
        LocalGet(c), I32Const(32), I32Or, I32Const(97), I32Sub, LocalTee(d),
        I32Const(26), I32LtU,
        If(BlockType::Result(ValType::I32)),
        LocalGet(d), I32Const(10), I32Add,
        Else, I32Const(99), End,
        LocalSet(d),
        End,
    );
}

/// `__num_unit(data: i32, i: i32) -> i32`: code unit `i` behind a tagged
/// pointer.
pub(super) fn emit_num_unit() -> Function {
    let mut f = Function::new(vec![]);
    let (data, i) = (0, 1);
    wasm!(f;
        LocalGet(data), I32Const(1), I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(data), I32Const(-2), I32And,
        LocalGet(i), I32Const(1), I32Shl, I32Add,
        I32Load16U(mem16(0)),
        Else,
        LocalGet(data), LocalGet(i), I32Add, I32Load8U(mem8(0)),
        End,
        End,
    );
    f
}

/// `__str_ws(c: i32) -> i32`: whether unit `c` is a `StrWhiteSpaceChar`.
pub(super) fn emit_str_ws() -> Function {
    let mut f = Function::new(vec![]);
    emit_is_whitespace(&mut f, 0);
    wasm!(f; End);
    f
}

// The `__dec_parse` locals: params, then 13 i32, one i64, one f64.
const DATA: u32 = 0;
const START: u32 = 1;
const END: u32 = 2;
const POS: u32 = 3;
const C: u32 = 4;
const NEG: u32 = 5;
const NDIG: u32 = 6;
const NSTORED: u32 = 7;
const LASTNZ: u32 = 8;
const POINT: u32 = 9;
const EXP10: u32 = 10;
const MANY: u32 = 11;
const FRAC: u32 = 12;
const E: u32 = 13;
const ENEG: u32 = 14;
const Q: u32 = 15;
const MANT: u32 = 16;
const POW: u32 = 17;
const VAL: u32 = 18;

/// `__dec_parse(data: i32, start: i32, end: i32) -> f64`: the longest
/// `StrDecimalLiteral` prefix of the units `start..end`, correctly rounded.
/// The index past it goes to [`END_CELL`]; with no literal that is `start`
/// and the result is NaN.
pub(super) fn emit_dec_parse(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![
        (13, ValType::I32),
        (2, ValType::I64),
        (1, ValType::F64),
    ]);
    dec_parse_sign(&mut f, h);
    dec_parse_infinity(&mut f, h);
    dec_parse_mantissa(&mut f, h);
    dec_parse_exponent(&mut f, h);
    dec_parse_publish(&mut f);
    dec_parse_fast(&mut f);
    // The slow path, then the sign.
    wasm!(f;
        Call(h.dec_slow()),
        LocalGet(NEG), I64ExtendI32U, I64Const(63), I64Shl, I64Or,
        F64ReinterpretI64,
        End,
    );
    f
}

/// Reset the outputs and consume an optional sign.
fn dec_parse_sign(f: &mut Function, h: Helpers<'_>) {
    set_field(f, END_CELL, START);
    set_field_const(f, TRUNC, 0);
    wasm!(f;
        LocalGet(START), LocalSet(POS),
        LocalGet(POS), LocalGet(END), I32LtU,
        If(BlockType::Empty),
        LocalGet(DATA), LocalGet(POS), Call(h.num_unit()), LocalTee(C),
        I32Const(45), I32Eq, LocalTee(NEG),
        LocalGet(C), I32Const(43), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    bump(f, POS, 1);
    wasm!(f; End, End);
}

/// Return `±Infinity` when the units at the cursor spell it.
fn dec_parse_infinity(f: &mut Function, h: Helpers<'_>) {
    wasm!(f;
        LocalGet(END), LocalGet(POS), I32Sub, I32Const(8), I32GeS,
        If(BlockType::Empty),
        Block(BlockType::Empty),
    );
    for (k, unit) in (0..).zip(b"Infinity") {
        wasm!(f;
            LocalGet(DATA), LocalGet(POS), I32Const(k), I32Add, Call(h.num_unit()),
            I32Const(i32::from(*unit)), I32Ne, BrIf(0),
        );
    }
    wasm!(f;
        I32Const(0), LocalGet(POS), I32Const(8), I32Add, I32Store(mem32(END_CELL)),
        I64Const(INF_BITS), F64ReinterpretI64, F64Neg,
        I64Const(INF_BITS), F64ReinterpretI64,
        LocalGet(NEG), Select, Return,
        End,
        End,
    );
}

/// Scan `digits [. digits]` into the buffer and the u64 mantissa; return NaN
/// when there is no digit.
fn dec_parse_mantissa(f: &mut Function, h: Helpers<'_>) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(POS), LocalGet(END), I32GeU, BrIf(1),
        LocalGet(DATA), LocalGet(POS), Call(h.num_unit()), LocalSet(C),
        // A second '.' ends the literal.
        LocalGet(C), I32Const(46), I32Eq,
        If(BlockType::Empty),
        LocalGet(FRAC), BrIf(2),
        I32Const(1), LocalSet(FRAC),
    );
    bump(f, POS, 1);
    wasm!(f;
        Br(1),
        End,
        LocalGet(C), I32Const(48), I32Sub, LocalTee(C), I32Const(10), I32GeU, BrIf(1),
    );
    bump(f, POS, 1);
    bump(f, NDIG, 1);
    dec_parse_digit(f);
    wasm!(f;
        Br(0),
        End,
        End,
        LocalGet(NDIG), I32Eqz,
        If(BlockType::Empty),
    );
    nan(f);
    wasm!(f; Return, End);
    set_field(f, END_CELL, POS);
}

/// Account for the digit in `C` (inside the mantissa loop, so `Br(1)` from a
/// nested `if` continues it).
fn dec_parse_digit(f: &mut Function) {
    // A leading zero only moves the point when it is fractional.
    wasm!(f;
        LocalGet(NSTORED), I32Eqz, LocalGet(C), I32Eqz, I32And,
        If(BlockType::Empty),
        LocalGet(FRAC),
        If(BlockType::Empty),
    );
    bump(f, POINT, -1);
    bump(f, EXP10, -1);
    wasm!(f;
        End,
        Br(1),
        End,
        LocalGet(NSTORED), I32Const(MAX_DIGITS), I32LtS,
        If(BlockType::Empty),
        LocalGet(NSTORED), LocalGet(C), I32Store8(mem8(DIGITS)),
        End,
    );
    bump(f, NSTORED, 1);
    wasm!(f;
        LocalGet(C),
        If(BlockType::Empty),
        LocalGet(NSTORED), LocalSet(LASTNZ),
        End,
        LocalGet(FRAC), I32Eqz,
        If(BlockType::Empty),
    );
    bump(f, POINT, 1);
    wasm!(f;
        End,
        LocalGet(NSTORED), I32Const(19), I32LeS,
        If(BlockType::Empty),
        LocalGet(MANT), I64Const(10), I64Mul,
        LocalGet(C), I64ExtendI32U, I64Add, LocalSet(MANT),
        LocalGet(FRAC),
        If(BlockType::Empty),
    );
    bump(f, EXP10, -1);
    wasm!(f;
        End,
        Else,
        I32Const(1), LocalSet(MANY),
        End,
    );
}

/// Consume `e [+-] digits` if it is complete; a bare `e` is not part of the
/// literal.
fn dec_parse_exponent(f: &mut Function, h: Helpers<'_>) {
    wasm!(f;
        LocalGet(POS), LocalGet(END), I32LtU,
        If(BlockType::Empty),
        LocalGet(DATA), LocalGet(POS), Call(h.num_unit()),
        I32Const(32), I32Or, I32Const(101), I32Eq,
        If(BlockType::Empty),
        LocalGet(POS), I32Const(1), I32Add, LocalSet(Q),
        LocalGet(Q), LocalGet(END), I32LtU,
        If(BlockType::Empty),
        LocalGet(DATA), LocalGet(Q), Call(h.num_unit()), LocalTee(C),
        I32Const(45), I32Eq, LocalTee(ENEG),
        LocalGet(C), I32Const(43), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    bump(f, Q, 1);
    wasm!(f;
        End,
        End,
        I32Const(0), LocalSet(NDIG),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(Q), LocalGet(END), I32GeU, BrIf(1),
        LocalGet(DATA), LocalGet(Q), Call(h.num_unit()), I32Const(48), I32Sub,
        LocalTee(C), I32Const(10), I32GeU, BrIf(1),
        LocalGet(E), I32Const(EXP_CAP), I32LtS,
        If(BlockType::Empty),
        LocalGet(E), I32Const(10), I32Mul, LocalGet(C), I32Add, LocalSet(E),
        End,
        I32Const(1), LocalSet(NDIG),
    );
    bump(f, Q, 1);
    wasm!(f;
        Br(0),
        End,
        End,
        LocalGet(NDIG),
        If(BlockType::Empty),
        LocalGet(Q), LocalSet(POS),
    );
    set_field(f, END_CELL, POS);
    wasm!(f;
        I32Const(0), LocalGet(E), I32Sub, LocalGet(E), LocalGet(ENEG), Select, LocalSet(E),
        LocalGet(POINT), LocalGet(E), I32Add, LocalSet(POINT),
        LocalGet(EXP10), LocalGet(E), I32Add, LocalSet(EXP10),
        End,
        End,
        End,
    );
}

/// Publish the buffer counters: trailing zeros are not significant.
fn dec_parse_publish(f: &mut Function) {
    set_field(f, DP, POINT);
    wasm!(f;
        LocalGet(LASTNZ), I32Const(MAX_DIGITS), I32GtS,
        If(BlockType::Empty),
        I32Const(MAX_DIGITS), LocalSet(LASTNZ),
    );
    set_field_const(f, TRUNC, 1);
    wasm!(f; End);
    set_field(f, NUM, LASTNZ);
}

/// Clinger's fast path, and its disguised form (`10^(e-22)` folded into the
/// mantissa): return when it applies, fall through otherwise.
fn dec_parse_fast(f: &mut Function) {
    wasm!(f;
        Block(BlockType::Empty),
        LocalGet(MANY), BrIf(0),
        LocalGet(MANT), I64Const(TWO_53), I64GtU, BrIf(0),
        LocalGet(EXP10), I32Const(-22), I32LtS, BrIf(0),
        LocalGet(EXP10), I32Const(37), I32GtS, BrIf(0),
        LocalGet(EXP10), I32Const(22), I32LeS,
        If(BlockType::Empty),
        LocalGet(EXP10), I32Const(0), I32LtS,
        If(BlockType::Empty),
        I32Const(0), LocalGet(EXP10), I32Sub, LocalSet(Q),
        LocalGet(MANT), F64ConvertI64U,
    );
    pow10(f, Q);
    wasm!(f;
        F64Div, LocalSet(VAL),
        Else,
        LocalGet(MANT), F64ConvertI64U,
    );
    pow10(f, EXP10);
    wasm!(f;
        F64Mul, LocalSet(VAL),
        End,
        Else,
        LocalGet(EXP10), I32Const(22), I32Sub, LocalSet(Q),
    );
    pow10(f, Q);
    wasm!(f;
        I64TruncF64U, LocalSet(POW),
        LocalGet(MANT), I64Const(TWO_53), LocalGet(POW), I64DivU, I64GtU, BrIf(1),
        LocalGet(MANT), LocalGet(POW), I64Mul, F64ConvertI64U,
    );
    pow10_const(f, 22);
    wasm!(f;
        F64Mul, LocalSet(VAL),
        End,
        LocalGet(VAL), F64Neg, LocalGet(VAL), LocalGet(NEG), Select, Return,
        End,
    );
}

/// Push the conversion step for a decimal point `n` places out (i32 local).
fn shift_for(f: &mut Function, n: u32) {
    wasm!(f;
        LocalGet(n), I32Const(19), I32LtU,
        If(BlockType::Result(ValType::I32)),
        LocalGet(n), I32Load8U(mem8(u64::from(abi::DEC_POWERS))),
        Else,
        I32Const(MAX_SHIFT),
        End,
    );
}

/// `__dec_slow() -> i64`: the bits of the magnitude in the buffer, rounded
/// half to even.
pub(super) fn emit_dec_slow(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (exp2, shift, dp, n, m) = (0, 1, 2, 3, 4);
    field(&mut f, NUM);
    wasm!(f; I32Eqz);
    field(&mut f, DP);
    wasm!(f;
        I32Const(-324), I32LtS, I32Or,
        If(BlockType::Empty), I64Const(0), Return, End,
    );
    field(&mut f, DP);
    wasm!(f;
        I32Const(310), I32GeS,
        If(BlockType::Empty), I64Const(INF_BITS), Return, End,
    );
    slow_normalize(&mut f, h, [exp2, shift, dp, n]);
    // Below the smallest normal exponent the value is denormalized first.
    wasm!(f;
        LocalGet(exp2), I32Const(1), I32Sub, LocalSet(exp2),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(exp2), I32Const(-1022), I32GeS, BrIf(1),
        I32Const(-1022), LocalGet(exp2), I32Sub, LocalSet(n),
        I32Const(MAX_SHIFT), LocalGet(n), LocalGet(n), I32Const(MAX_SHIFT), I32GtS, Select,
        LocalSet(n),
        LocalGet(n), Call(h.dec_right()),
        LocalGet(exp2), LocalGet(n), I32Add, LocalSet(exp2),
        Br(0),
        End,
        End,
    );
    slow_encode(&mut f, h, exp2, m);
    f
}

/// Scale the buffer into `[0.5, 1)`, tracking the binary exponent.
fn slow_normalize(f: &mut Function, h: Helpers<'_>, [exp2, shift, dp, n]: [u32; 4]) {
    wasm!(f; Block(BlockType::Empty), Loop(BlockType::Empty));
    field(f, DP);
    wasm!(f; LocalTee(dp), I32Const(0), I32LeS, BrIf(1));
    shift_for(f, dp);
    wasm!(f; LocalTee(shift), Call(h.dec_right()));
    field(f, DP);
    wasm!(f;
        I32Const(-DP_RANGE), I32LtS,
        If(BlockType::Empty), I64Const(0), Return, End,
        LocalGet(exp2), LocalGet(shift), I32Add, LocalSet(exp2),
        Br(0),
        End,
        End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    field(f, DP);
    wasm!(f;
        LocalTee(dp), I32Const(0), I32GtS, BrIf(1),
        LocalGet(dp), I32Eqz,
        If(BlockType::Result(ValType::I32)),
        I32Const(0), I32Load8U(mem8(DIGITS)), LocalTee(n), I32Const(5), I32GeU, BrIf(2),
        I32Const(2), I32Const(1), LocalGet(n), I32Const(2), I32LtU, Select,
        Else,
        I32Const(0), LocalGet(dp), I32Sub, LocalSet(n),
    );
    shift_for(f, n);
    wasm!(f;
        End,
        LocalTee(shift), Call(h.dec_left()),
    );
    field(f, DP);
    wasm!(f;
        I32Const(DP_RANGE), I32GtS,
        If(BlockType::Empty), I64Const(INF_BITS), Return, End,
        LocalGet(exp2), LocalGet(shift), I32Sub, LocalSet(exp2),
        Br(0),
        End,
        End,
    );
}

/// Round to 53 bits and assemble the IEEE encoding.
fn slow_encode(f: &mut Function, h: Helpers<'_>, exp2: u32, m: u32) {
    wasm!(f;
        LocalGet(exp2), I32Const(1023), I32Add, I32Const(2047), I32GeS,
        If(BlockType::Empty), I64Const(INF_BITS), Return, End,
        I32Const(53), Call(h.dec_left()),
        Call(h.dec_round()), LocalTee(m), I64Const(TWO_53), I64GeU,
        If(BlockType::Empty),
        // Rounding carried into a 54th bit.
        I32Const(1), Call(h.dec_right()),
        LocalGet(exp2), I32Const(1), I32Add, LocalSet(exp2),
        Call(h.dec_round()), LocalSet(m),
        LocalGet(exp2), I32Const(1023), I32Add, I32Const(2047), I32GeS,
        If(BlockType::Empty), I64Const(INF_BITS), Return, End,
        End,
        // A subnormal has no hidden bit and the biased exponent 0.
        LocalGet(exp2), I32Const(1023), I32Add,
        LocalGet(m), I64Const(TWO_52), I64LtU, I32Sub,
        I64ExtendI32U, I64Const(52), I64Shl,
        LocalGet(m), I64Const(TWO_52 - 1), I64And,
        I64Or,
        End,
    );
}

/// Push digit `index` (i32 local) of the buffer.
fn digit(f: &mut Function, index: u32) {
    wasm!(f; LocalGet(index), I32Load8U(mem8(DIGITS)));
}

/// `__dec_round() -> i64`: the buffer's integer part, rounded half to even
/// (with `truncated` breaking an apparent tie upward).
pub(super) fn emit_dec_round() -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (num, dp, i, up, n) = (0, 1, 2, 3, 4);
    field(&mut f, NUM);
    wasm!(f; LocalTee(num), I32Const(0), I32Ne);
    field(&mut f, DP);
    wasm!(f;
        LocalTee(dp), I32Const(0), I32GeS, I32And, I32Eqz,
        If(BlockType::Empty), I64Const(0), Return, End,
        LocalGet(dp), I32Const(19), I32GeS,
        If(BlockType::Empty), I64Const(-1), Return, End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(dp), I32GeU, BrIf(1),
        LocalGet(n), I64Const(10), I64Mul, LocalSet(n),
        LocalGet(i), LocalGet(num), I32LtU,
        If(BlockType::Empty),
        LocalGet(n),
    );
    digit(&mut f, i);
    wasm!(f;
        I64ExtendI32U, I64Add, LocalSet(n),
        End,
    );
    bump(&mut f, i, 1);
    wasm!(f;
        Br(0),
        End,
        End,
        LocalGet(dp), LocalGet(num), I32LtU,
        If(BlockType::Empty),
    );
    digit(&mut f, dp);
    wasm!(f; LocalTee(up), I32Const(5), I32Eq, LocalGet(dp), I32Const(1), I32Add, LocalGet(num), I32Eq, I32And);
    round_tie(&mut f, dp, up);
    wasm!(f;
        End,
        LocalGet(n), LocalGet(up), I64ExtendI32U, I64Add,
        End,
    );
    f
}

/// Consume the `is exact tie` flag on the stack and leave `up` as the round-up
/// decision for digit value `up` at position `dp`.
fn round_tie(f: &mut Function, dp: u32, up: u32) {
    wasm!(f; If(BlockType::Result(ValType::I32)));
    field(f, TRUNC);
    wasm!(f;
        I32Const(0), I32Ne,
        LocalGet(dp),
        If(BlockType::Result(ValType::I32)),
        LocalGet(dp), I32Const(1), I32Sub, I32Load8U(mem8(DIGITS)), I32Const(1), I32And,
        Else,
        I32Const(0),
        End,
        I32Or,
        Else,
        LocalGet(up), I32Const(5), I32GeU,
        End,
        LocalSet(up),
    );
}

/// Store the i64-local value `value` as digit `at` (i32 local) when it fits,
/// otherwise record a non-zero dropped digit.
fn store_or_truncate(f: &mut Function, at: u32, value: u32) {
    wasm!(f;
        LocalGet(at), I32Const(MAX_DIGITS), I32LtS,
        If(BlockType::Empty),
        LocalGet(at), LocalGet(value), I64Store8(mem8(DIGITS)),
        Else,
        LocalGet(value), I64Const(0), I64GtU,
        If(BlockType::Empty),
    );
    set_field_const(f, TRUNC, 1);
    wasm!(f; End, End);
}

/// Drop the trailing zero digits of the count in i32 local `num`, then
/// publish it.
fn trim_and_publish(f: &mut Function, num: u32) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(num), I32Eqz, BrIf(1),
        LocalGet(num), I32Const(1), I32Sub, I32Load8U(mem8(DIGITS)), BrIf(1),
    );
    bump(f, num, -1);
    wasm!(f; Br(0), End, End);
    set_field(f, NUM, num);
}

/// `__dec_left(shift: i32)`: multiply the buffer by `2^shift`.
pub(super) fn emit_dec_left() -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (4, ValType::I64)]);
    let (shift, new, num, read, write) = (0, 1, 2, 3, 4);
    let (pa, pb, i) = (5, 6, 7);
    let (n, q, rem, wide) = (8, 9, 10, 11);
    field(&mut f, NUM);
    wasm!(f;
        LocalTee(num), I32Eqz,
        If(BlockType::Empty), Return, End,
        LocalGet(shift), I64ExtendI32U, LocalSet(wide),
    );
    left_new_digits(&mut f, [shift, new, num, pa, pb, i]);
    wasm!(f;
        LocalGet(num), LocalSet(read),
        LocalGet(num), LocalGet(new), I32Add, LocalSet(write),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(read), I32Eqz, BrIf(1),
    );
    bump(&mut f, read, -1);
    bump(&mut f, write, -1);
    wasm!(f; LocalGet(n));
    digit(&mut f, read);
    wasm!(f; I64ExtendI32U, LocalGet(wide), I64Shl, I64Add, LocalSet(n));
    left_emit_digit(&mut f, write, [n, q, rem]);
    wasm!(f;
        Br(0),
        End,
        End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(n), I64Eqz, BrIf(1),
    );
    bump(&mut f, write, -1);
    left_emit_digit(&mut f, write, [n, q, rem]);
    wasm!(f;
        Br(0),
        End,
        End,
        LocalGet(num), LocalGet(new), I32Add, LocalSet(num),
        I32Const(MAX_DIGITS), LocalGet(num), LocalGet(num), I32Const(MAX_DIGITS), I32GtS, Select,
        LocalSet(num),
    );
    wasm!(f; I32Const(0));
    field(&mut f, DP);
    wasm!(f; LocalGet(new), I32Add, I32Store(mem32(DP)));
    trim_and_publish(&mut f, num);
    wasm!(f; End);
    f
}

/// i32 local `new` = how many digits a left shift adds: the table's delta,
/// less one when the digits compare below the power of five.
fn left_new_digits(f: &mut Function, [shift, new, num, pa, pb, i]: [u32; 6]) {
    let table = mem16(u64::from(abi::DEC_SHIFT_TABLE));
    wasm!(f;
        LocalGet(shift), I32Const(1), I32Shl, I32Load16U(table), LocalTee(pa),
        I32Const(11), I32ShrU, LocalSet(new),
        LocalGet(pa), I32Const(0x7FF), I32And, LocalSet(pa),
        LocalGet(shift), I32Const(1), I32Shl, I32Load16U(mem16(u64::from(abi::DEC_SHIFT_TABLE) + 2)),
        I32Const(0x7FF), I32And, LocalSet(pb),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pa), LocalGet(i), I32Add, LocalGet(pb), I32GeU, BrIf(1),
        // The digits ran out while equal: below the power of five.
        LocalGet(i), LocalGet(num), I32GeU,
        If(BlockType::Empty),
    );
    bump(f, new, -1);
    wasm!(f; Br(2), End);
    digit(f, i);
    wasm!(f;
        LocalGet(pa), LocalGet(i), I32Add, I32Load8U(mem8(u64::from(abi::DEC_POW5))),
        I32Ne,
        If(BlockType::Empty),
    );
    digit(f, i);
    wasm!(f;
        LocalGet(pa), LocalGet(i), I32Add, I32Load8U(mem8(u64::from(abi::DEC_POW5))),
        I32LtU,
        If(BlockType::Empty),
    );
    bump(f, new, -1);
    wasm!(f; End, Br(2), End);
    bump(f, i, 1);
    wasm!(f; Br(0), End, End);
}

/// Split i64 local `n` into its last decimal digit (stored at `write`) and
/// the carried quotient.
fn left_emit_digit(f: &mut Function, write: u32, [n, q, rem]: [u32; 3]) {
    wasm!(f;
        LocalGet(n), I64Const(10), I64DivU, LocalSet(q),
        LocalGet(n), LocalGet(q), I64Const(10), I64Mul, I64Sub, LocalSet(rem),
    );
    store_or_truncate(f, write, rem);
    wasm!(f; LocalGet(q), LocalSet(n));
}

/// `__dec_right(shift: i32)`: divide the buffer by `2^shift`.
pub(super) fn emit_dec_right() -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (3, ValType::I64)]);
    let (shift, read, write, num, new) = (0, 1, 2, 3, 4);
    let (n, wide, mask) = (5, 6, 7);
    field(&mut f, NUM);
    wasm!(f; LocalSet(num), LocalGet(shift), I64ExtendI32U, LocalSet(wide));
    right_prefix(&mut f, [read, num, n, wide]);
    // The point moves; below the range the value is exactly representable as 0.
    wasm!(f; I32Const(0));
    field(&mut f, DP);
    wasm!(f;
        LocalGet(read), I32Const(1), I32Sub, I32Sub, LocalTee(new), I32Store(mem32(DP)),
        LocalGet(new), I32Const(-DP_RANGE), I32LtS,
        If(BlockType::Empty),
    );
    set_field_const(&mut f, NUM, 0);
    set_field_const(&mut f, DP, 0);
    set_field_const(&mut f, TRUNC, 0);
    wasm!(f;
        Return,
        End,
        I64Const(1), LocalGet(wide), I64Shl, I64Const(1), I64Sub, LocalSet(mask),
    );
    right_digits(&mut f, [read, write, num, new, n, wide, mask]);
    right_tail(&mut f, [write, n, wide, mask]);
    wasm!(f; LocalGet(write), LocalSet(num));
    trim_and_publish(&mut f, num);
    wasm!(f; End);
    f
}

/// Read leading digits into i64 local `n` until the shift yields a digit;
/// returns from the helper when the buffer is exhausted with `n == 0`.
fn right_prefix(f: &mut Function, [read, num, n, wide]: [u32; 4]) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(n), LocalGet(wide), I64ShrU, I64Eqz, I32Eqz, BrIf(1),
        LocalGet(read), LocalGet(num), I32LtU,
        If(BlockType::Empty),
        LocalGet(n), I64Const(10), I64Mul,
    );
    digit(f, read);
    wasm!(f; I64ExtendI32U, I64Add, LocalSet(n));
    bump(f, read, 1);
    wasm!(f;
        Br(1),
        End,
        LocalGet(n), I64Eqz,
        If(BlockType::Empty), Return, End,
        // Past the digits the value continues with zeros.
        Loop(BlockType::Empty),
        LocalGet(n), I64Const(10), I64Mul, LocalSet(n),
    );
    bump(f, read, 1);
    wasm!(f;
        LocalGet(n), LocalGet(wide), I64ShrU, I64Eqz, BrIf(0),
        End,
        Br(1),
        End,
        End,
    );
}

/// Emit one quotient digit per remaining buffer digit.
fn right_digits(f: &mut Function, [read, write, num, new, n, wide, mask]: [u32; 7]) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(read), LocalGet(num), I32GeU, BrIf(1),
        LocalGet(n), LocalGet(wide), I64ShrU, I32WrapI64, LocalSet(new),
        LocalGet(n), LocalGet(mask), I64And, I64Const(10), I64Mul,
    );
    digit(f, read);
    wasm!(f;
        I64ExtendI32U, I64Add, LocalSet(n),
        LocalGet(write), LocalGet(new), I32Store8(mem8(DIGITS)),
    );
    bump(f, read, 1);
    bump(f, write, 1);
    wasm!(f; Br(0), End, End);
}

/// Emit the digits of the remainder, past the end of the input digits.
fn right_tail(f: &mut Function, [write, n, wide, mask]: [u32; 4]) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(n), I64Eqz, BrIf(1),
        LocalGet(write), I32Const(MAX_DIGITS), I32LtS,
        If(BlockType::Empty),
        LocalGet(write), LocalGet(n), LocalGet(wide), I64ShrU, I64Store8(mem8(DIGITS)),
    );
    bump(f, write, 1);
    wasm!(f;
        Else,
        LocalGet(n), LocalGet(wide), I64ShrU, I64Const(0), I64GtU,
        If(BlockType::Empty),
    );
    set_field_const(f, TRUNC, 1);
    wasm!(f;
        End,
        End,
        LocalGet(n), LocalGet(mask), I64And, I64Const(10), I64Mul, LocalSet(n),
        Br(0),
        End,
        End,
    );
}

/// `__digit_span(data, start, end, radix) -> i32`: the index past the run of
/// radix-`radix` digits at `start`.
pub(super) fn emit_digit_span(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (data, pos, end, radix, c, d) = (0, 1, 2, 3, 4, 5);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pos), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(data), LocalGet(pos), Call(h.num_unit()), LocalSet(c),
    );
    digit_value(&mut f, c, d);
    wasm!(f; LocalGet(d), LocalGet(radix), I32GeU, BrIf(1));
    bump(&mut f, pos, 1);
    wasm!(f; Br(0), End, End, LocalGet(pos), End);
    f
}

/// `__radix_pow2(data, start, end, bits) -> f64`: the exact value of the
/// validated digits of radix `2^bits`, rounded half to even at 53 bits.
pub(super) fn emit_radix_pow2(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (3, ValType::I64)]);
    let (data, idx, end, bits) = (0, 1, 2, 3);
    let (val, bit, sticky, dropped, extra, up, unit) = (4, 5, 6, 7, 8, 9, 10);
    let (window, mant, rem) = (11, 12, 13);
    radix_window(
        &mut f,
        h,
        [data, idx, end, bits, val, bit, sticky, dropped, unit],
        window,
    );
    wasm!(f;
        // At most 53 bits: nothing was dropped and the conversion is exact.
        LocalGet(window), I64Const(TWO_53), I64LtU,
        If(BlockType::Empty),
        LocalGet(window), F64ConvertI64U, Return,
        End,
        I32Const(64), LocalGet(window), I64Clz, I32WrapI64, I32Sub, I32Const(53), I32Sub,
        LocalSet(extra),
        LocalGet(window), LocalGet(extra), I64ExtendI32U, I64ShrU, LocalSet(mant),
        LocalGet(window),
        I64Const(1), LocalGet(extra), I64ExtendI32U, I64Shl, I64Const(1), I64Sub,
        I64And, LocalSet(rem),
    );
    radix_round(&mut f, [extra, up, sticky], [mant, rem]);
    wasm!(f; LocalGet(mant), LocalGet(up), I64ExtendI32U, I64Add, LocalTee(mant), I64Const(TWO_53), I64Eq);
    wasm!(f;
        If(BlockType::Empty),
        I64Const(TWO_52), LocalSet(mant),
    );
    bump(&mut f, extra, 1);
    wasm!(f;
        End,
        LocalGet(extra), LocalGet(dropped), I32Add, I32Const(52), I32Add, LocalTee(extra),
        I32Const(1023), I32GtS,
        If(BlockType::Empty),
        I64Const(INF_BITS), F64ReinterpretI64, Return,
        End,
        LocalGet(extra), I32Const(1023), I32Add, I64ExtendI32U, I64Const(52), I64Shl,
        LocalGet(mant), I64Const(TWO_52 - 1), I64And, I64Or,
        F64ReinterpretI64,
        End,
    );
    f
}

/// Shift the digits' bits into the u64 window; bits past it set `sticky` and
/// count in `dropped`. A zero window returns `0`.
fn radix_window(
    f: &mut Function,
    h: Helpers<'_>,
    [data, idx, end, bits, val, bit, sticky, dropped, unit]: [u32; 9],
    window: u32,
) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(idx), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(data), LocalGet(idx), Call(h.num_unit()), LocalSet(unit),
    );
    digit_value(f, unit, val);
    wasm!(f;
        LocalGet(bits), I32Const(1), I32Sub, LocalSet(bit),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(bit), I32Const(0), I32LtS, BrIf(1),
        LocalGet(window), I64Const(0), I64LtS,
        If(BlockType::Empty),
        LocalGet(sticky), LocalGet(val), LocalGet(bit), I32ShrU, I32Const(1), I32And, I32Or,
        LocalSet(sticky),
    );
    bump(f, dropped, 1);
    wasm!(f;
        Else,
        LocalGet(window), I64Const(1), I64Shl,
        LocalGet(val), LocalGet(bit), I32ShrU, I32Const(1), I32And, I64ExtendI32U,
        I64Or, LocalSet(window),
        End,
    );
    bump(f, bit, -1);
    wasm!(f; Br(0), End, End);
    bump(f, idx, 1);
    wasm!(f;
        Br(0),
        End,
        End,
        LocalGet(window), I64Eqz,
        If(BlockType::Empty),
        F64Const(0.0.into()), Return,
        End,
    );
}

/// i32 local `up` = whether the discarded `rem` rounds mantissa `m` up.
fn radix_round(f: &mut Function, [extra, up, sticky]: [u32; 3], [m, rem]: [u32; 2]) {
    let half = |f: &mut Function| {
        wasm!(f; I64Const(1), LocalGet(extra), I32Const(1), I32Sub, I64ExtendI32U, I64Shl);
    };
    wasm!(f; LocalGet(rem));
    half(f);
    wasm!(f; I64GtU, LocalGet(rem));
    half(f);
    wasm!(f;
        I64Eq,
        LocalGet(sticky), LocalGet(m), I32WrapI64, I32Const(1), I32And, I32Or,
        I32And, I32Or, LocalSet(up),
    );
}

/// `__radix_fold(data, start, end, radix) -> f64`: the digits accumulated in
/// `f64` (the approximation the specification permits for other radices).
pub(super) fn emit_radix_fold(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32), (1, ValType::F64)]);
    let (data, idx, end, radix, val, unit, acc) = (0, 1, 2, 3, 4, 5, 6);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(idx), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(data), LocalGet(idx), Call(h.num_unit()), LocalSet(unit),
    );
    digit_value(&mut f, unit, val);
    wasm!(f;
        LocalGet(acc), LocalGet(radix), F64ConvertI32U, F64Mul,
        LocalGet(val), F64ConvertI32U, F64Add, LocalSet(acc),
    );
    bump(&mut f, idx, 1);
    wasm!(f; Br(0), End, End, LocalGet(acc), End);
    f
}

/// `__string_to_number(s: i32) -> f64`: `StringToNumber`.
pub(super) fn emit_string_to_number(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32), (1, ValType::F64)]);
    let (s, data, pos, end, c, bits, val) = (0, 1, 2, 3, 4, 5, 6);
    set_data(&mut f, s, data);
    wasm!(f;
        LocalGet(s), I32Load(mem32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(end),
    );
    skip_leading_ws(&mut f, h, [data, pos, end]);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(end), LocalGet(pos), I32LeU, BrIf(1),
        LocalGet(data), LocalGet(end), I32Const(1), I32Sub, Call(h.num_unit()),
        Call(h.str_ws()), I32Eqz, BrIf(1),
    );
    bump(&mut f, end, -1);
    wasm!(f;
        Br(0), End, End,
        LocalGet(pos), LocalGet(end), I32Eq,
        If(BlockType::Empty), F64Const(0.0.into()), Return, End,
    );
    string_to_number_radix(&mut f, h, [data, pos, end, c, bits]);
    wasm!(f;
        LocalGet(data), LocalGet(pos), LocalGet(end), Call(h.dec_parse()), LocalSet(val),
    );
    field(&mut f, END_CELL);
    wasm!(f; LocalGet(end), I32Ne, If(BlockType::Empty));
    nan(&mut f);
    wasm!(f; Return, End, LocalGet(val), End);
    f
}

/// Return the value of a `0x` / `0o` / `0b` literal, NaN when malformed; any
/// other input falls through. Only the unsigned form is a non-decimal literal.
fn string_to_number_radix(f: &mut Function, h: Helpers<'_>, [data, pos, end, c, bits]: [u32; 5]) {
    wasm!(f;
        LocalGet(end), LocalGet(pos), I32Sub, I32Const(2), I32GeS,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(pos), Call(h.num_unit()), I32Const(48), I32Eq,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(pos), I32Const(1), I32Add, Call(h.num_unit()),
        I32Const(32), I32Or, LocalSet(c),
        LocalGet(c), I32Const(120), I32Eq, I32Const(2), I32Shl,
        LocalGet(c), I32Const(111), I32Eq, I32Const(3), I32Mul, I32Or,
        LocalGet(c), I32Const(98), I32Eq, I32Or,
        LocalTee(bits),
        If(BlockType::Empty),
    );
    bump(f, pos, 2);
    wasm!(f;
        LocalGet(pos), LocalGet(end), I32Eq,
        LocalGet(data), LocalGet(pos), LocalGet(end),
        I32Const(1), LocalGet(bits), I32Shl, Call(h.digit_span()), LocalGet(end), I32Ne,
        I32Or,
        If(BlockType::Empty),
    );
    nan(f);
    wasm!(f;
        Return,
        End,
        LocalGet(data), LocalGet(pos), LocalGet(end), LocalGet(bits), Call(h.radix_pow2()),
        Return,
        End,
        End,
        End,
    );
}

/// `__to_number_prim(v: i64) -> i64`: `ToNumber` of a non-symbol primitive.
pub(super) fn emit_to_number_prim(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    let v = 0;
    emit_is_number(&mut f, v);
    wasm!(f;
        If(BlockType::Empty), LocalGet(v), Return, End,
    );
    emit_is_string(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, Call(h.string_to_number()), I64ReinterpretF64, Return,
        End,
    );
    let one = 1.0_f64.to_bits().cast_signed();
    for (bits, number) in [(NULL_BITS, 0), (FALSE_BITS, 0), (TRUE_BITS, one)] {
        wasm!(f;
            LocalGet(v), I64Const(bits), I64Eq,
            If(BlockType::Empty), I64Const(number), Return, End,
        );
    }
    // `undefined` (and, as before, any other value) is NaN.
    wasm!(f; I64Const(QNAN_BITS), End);
    f
}

/// `__parse_float(v: i64) -> i64`: `parseFloat` of the string jsval `v`.
pub(super) fn emit_parse_float(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32)]);
    let (v, s, data, pos, end) = (0, 1, 2, 3, 4);
    wasm!(f;
        LocalGet(v), I32WrapI64, LocalTee(s),
        I32Load(mem32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(end),
    );
    set_data(&mut f, s, data);
    skip_leading_ws(&mut f, h, [data, pos, end]);
    wasm!(f;
        LocalGet(data), LocalGet(pos), LocalGet(end), Call(h.dec_parse()),
        I64ReinterpretF64,
        End,
    );
    f
}

/// `__parse_int(v: i64, radix: i32) -> i64`: `parseInt` of the string jsval
/// `v` with `R = radix` (already `ToInt32`).
pub(super) fn emit_parse_int(h: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (1, ValType::F64)]);
    let (jsv, radix, ptr, data, pos, end, neg, unit, span, val) = (0, 1, 2, 3, 4, 5, 6, 7, 8, 9);
    wasm!(f;
        LocalGet(jsv), I32WrapI64, LocalTee(ptr),
        I32Load(mem32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(end),
    );
    set_data(&mut f, ptr, data);
    skip_leading_ws(&mut f, h, [data, pos, end]);
    wasm!(f;
        LocalGet(pos), LocalGet(end), I32LtU,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(pos), Call(h.num_unit()), LocalTee(unit),
        I32Const(45), I32Eq, LocalTee(neg),
        LocalGet(unit), I32Const(43), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    bump(&mut f, pos, 1);
    wasm!(f; End, End);
    parse_int_radix(&mut f, h, [radix, data, pos, end, unit]);
    wasm!(f;
        LocalGet(data), LocalGet(pos), LocalGet(end), LocalGet(radix), Call(h.digit_span()),
        LocalTee(span), LocalGet(pos), I32Eq,
        If(BlockType::Empty),
        I64Const(QNAN_BITS), Return,
        End,
    );
    parse_int_value(&mut f, h, [radix, data, pos, span], val);
    wasm!(f;
        LocalGet(val), F64Neg, LocalGet(val), LocalGet(neg), Select,
        I64ReinterpretF64,
        End,
    );
    f
}

/// Resolve `R` and strip a `0x` prefix, returning NaN for an invalid radix.
fn parse_int_radix(f: &mut Function, h: Helpers<'_>, [radix, data, pos, end, c]: [u32; 5]) {
    // `c` = strip_prefix.
    wasm!(f;
        LocalGet(radix), I32Eqz, LocalTee(c),
        If(BlockType::Empty),
        I32Const(10), LocalSet(radix),
        Else,
        LocalGet(radix), I32Const(2), I32Sub, I32Const(34), I32GtU,
        If(BlockType::Empty), I64Const(QNAN_BITS), Return, End,
        LocalGet(radix), I32Const(16), I32Eq, LocalSet(c),
        End,
        LocalGet(c),
        LocalGet(end), LocalGet(pos), I32Sub, I32Const(2), I32GeS, I32And,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(pos), Call(h.num_unit()), I32Const(48), I32Eq,
        LocalGet(data), LocalGet(pos), I32Const(1), I32Add, Call(h.num_unit()),
        I32Const(32), I32Or, I32Const(120), I32Eq,
        I32And,
        If(BlockType::Empty),
    );
    bump(f, pos, 2);
    wasm!(f;
        I32Const(16), LocalSet(radix),
        End,
        End,
    );
}

/// f64 local `val` = the digits `pos..span` in radix `radix`.
fn parse_int_value(f: &mut Function, h: Helpers<'_>, [radix, data, pos, span]: [u32; 4], val: u32) {
    wasm!(f;
        LocalGet(radix), I32Const(10), I32Eq,
        If(BlockType::Result(ValType::F64)),
        LocalGet(data), LocalGet(pos), LocalGet(span), Call(h.dec_parse()),
        Else,
        LocalGet(radix), LocalGet(radix), I32Const(1), I32Sub, I32And, I32Eqz,
        If(BlockType::Result(ValType::F64)),
        LocalGet(data), LocalGet(pos), LocalGet(span), LocalGet(radix), I32Ctz,
        Call(h.radix_pow2()),
        Else,
        LocalGet(data), LocalGet(pos), LocalGet(span), LocalGet(radix), Call(h.radix_fold()),
        End,
        End,
        LocalSet(val),
    );
}

/// The static tables at [`abi::DEC_POW10`]: the 23 exact powers of ten, the
/// 65-entry left-shift table, the packed digits of `5^s`, and the step table.
pub(super) fn tables() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((abi::NUM_SCRATCH_ADDR - abi::DEC_POW10) as usize);
    let mut power = 1.0_f64;
    for _ in 0..23 {
        bytes.extend_from_slice(&power.to_le_bytes());
        power *= 10.0;
    }
    let (shift_table, pow5) = shift_tables();
    for entry in shift_table {
        bytes.extend_from_slice(&entry.to_le_bytes());
    }
    bytes.extend_from_slice(&pow5);
    bytes.extend_from_slice(&[
        0, 3, 6, 9, 13, 16, 19, 23, 26, 29, 33, 36, 39, 43, 46, 49, 53, 56, 59,
    ]);
    bytes
}

/// The left-shift table: entry `s` packs `digits(2^s) << 11` with the offset
/// of the digits of `5^s` in the second table.
fn shift_tables() -> ([u16; 65], Vec<u8>) {
    let mut table = [0_u16; 65];
    let mut pow5_digits: Vec<u8> = Vec::new();
    // Little-endian decimal digits of 5^s.
    let mut five = vec![1_u8];
    for s in 1..=64_u32 {
        let mut carry = 0_u8;
        for digit in &mut five {
            let next = *digit * 5 + carry;
            *digit = next % 10;
            carry = next / 10;
        }
        if carry > 0 {
            five.push(carry);
        }
        let delta = (1_u128 << s).to_string().len();
        let offset = pow5_digits.len();
        table[s as usize] = u16::try_from((delta << 11) | offset).unwrap_or(0);
        if s < 64 {
            pow5_digits.extend(five.iter().rev());
        }
    }
    (table, pow5_digits)
}

#[cfg(test)]
mod tests {
    use super::{abi, shift_tables, tables};

    #[test]
    fn shift_table_matches_the_published_layout() {
        let (table, pow5) = shift_tables();
        assert_eq!(table[0], 0);
        assert_eq!(table[1], 0x0800);
        assert_eq!(table[2], 0x0801);
        assert_eq!(table[3], 0x0803);
        assert_eq!(table[4], 0x1006);
        assert_eq!(table[7], 0x1812);
        assert_eq!(pow5.len(), abi::DEC_POW5_LEN as usize);
        assert_eq!(&pow5[..6], &[5, 2, 5, 1, 2, 5]);
    }

    #[test]
    fn tables_fill_the_reserved_area() {
        let size = (abi::DEC_POWERS + 19 - abi::DEC_POW10) as usize;
        assert_eq!(tables().len(), size);
    }
}
