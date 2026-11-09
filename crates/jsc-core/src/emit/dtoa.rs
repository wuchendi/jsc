//! The guest `Number` to string conversions: Ryu shortest digits with the
//! size-optimized tables (`d2s_small_table`), the ECMAScript `Number::toString`
//! layout, the exact-decimal `toFixed` / `toExponential` / `toPrecision`, the
//! radix `toString` (V8's `DoubleToRadixCString`), and the primitive
//! `ToString` built on them.
//!
//! Everything is composed in the static [`abi::NUM_SCRATCH_ADDR`] buffer and
//! copied into one Latin-1 `__new_string` at the end. No user code runs while
//! it is in use, so the buffer is never re-entered.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// The number of helpers [`emit_helpers`] returns; their indices are
/// consecutive from [`Helpers::dtoa`].
pub(super) const HELPER_COUNT: u32 = 12;

/// The static scratch area and its regions.
const SCRATCH: i32 = abi::NUM_SCRATCH_ADDR.cast_signed();
const SCRATCH_END: i32 = SCRATCH + abi::NUM_SCRATCH_SIZE.cast_signed();
/// The little-endian `u32` bignum limbs (at most 80 are live).
const LIMBS: i32 = SCRATCH;
const LIMBS_BYTES: i32 = 512;
/// The decimal digit buffer, filled from its end.
const DIGITS: i32 = LIMBS + LIMBS_BYTES;
const DIGITS_BYTES: i32 = 800;
/// The end of the shortest digits, written backwards at the digit buffer's
/// start (at most 17 digits).
const SHORT_END: i32 = DIGITS + 32;
/// The rounded digits of the exact formats (at most 122 are live), and the
/// output text of the shortest format.
const ROUNDED: i32 = DIGITS + DIGITS_BYTES;
/// The output text of the exact formats.
const TEXT: i32 = ROUNDED + 300;

/// The `__num_format` modes.
pub(super) const MODE_FIXED: i32 = 0;
pub(super) const MODE_EXPONENTIAL: i32 = 1;
pub(super) const MODE_PRECISION: i32 = 2;
pub(super) const MODE_SHORTEST_EXPONENTIAL: i32 = 3;

/// The IEEE 754 fields.
const FRACTION_MASK: i64 = (1 << 52) - 1;
const HIDDEN_BIT: i64 = 1 << 52;
const INFINITY_BITS: i64 = 0x7FF0_0000_0000_0000;
const LOW_32: i64 = 0xFFFF_FFFF;

/// ASCII bytes of the layouts.
const ZERO: i32 = 48;
const FIVE: i32 = 53;
const NINE: i32 = 57;
const DOT: i32 = 46;
const MINUS: i32 = 45;
const PLUS: i32 = 43;
const LOWER_E: i32 = 101;

/// `5^26` steps between the `*_SPLIT` table entries.
const POW5_STEP: i32 = 26;

/// The static strings of this family.
const NAN_TEXT: &str = "NaN";
const INFINITY_TEXT: &str = "Infinity";
const MINUS_INFINITY_TEXT: &str = "-Infinity";
const ZERO_TEXT: &str = "0";
const TRUE_TEXT: &str = "true";
const FALSE_TEXT: &str = "false";
const NULL_TEXT: &str = "null";
const UNDEFINED_TEXT: &str = "undefined";
const OBJECT_TEXT: &str = "[object Object]";

/// See [`NAN_TEXT`].
pub(super) const STRINGS: [&str; 9] = [
    NAN_TEXT,
    INFINITY_TEXT,
    MINUS_INFINITY_TEXT,
    ZERO_TEXT,
    TRUE_TEXT,
    FALSE_TEXT,
    NULL_TEXT,
    UNDEFINED_TEXT,
    OBJECT_TEXT,
];

// The Ryu tables: `5^i`, the 125-bit `5^(26b)` and `2^k / 5^(26b)` split into
// (low, high) words, and the 2-bit corrections of the products of two entries.
// Generated from exact big-integer arithmetic, as upstream's
// `d2s_small_table.h`.
const POW5: [u64; 26] = [
    0x1,
    0x5,
    0x19,
    0x7d,
    0x271,
    0xc35,
    0x3d09,
    0x0001_312d,
    0x0005_f5e1,
    0x001d_cd65,
    0x0095_02f9,
    0x02e9_0edd,
    0x0e8d_4a51,
    0x48c2_7395,
    0x0001_6bcc_41e9,
    0x0007_1afd_498d,
    0x0023_86f2_6fc1,
    0x00b1_a2bc_2ec5,
    0x0378_2dac_e9d9,
    0x1158_e460_913d,
    0x56bc_75e2_d631,
    0x0001_b1ae_4d6e_2ef5,
    0x0008_7867_8326_eac9,
    0x002a_5a05_8fc2_95ed,
    0x00d3_c21b_cecc_eda1,
    0x0422_ca8b_0a00_a425,
];
const POW5_SPLIT: [[u64; 2]; 13] = [
    [0x0, 0x1000_0000_0000_0000],
    [0x0, 0x14ad_f4b7_3203_34b9],
    [0x0e54_9208_b31a_db10, 0x1aba_4714_957d_300d],
    [0x6dc6_ad26_4d8f_0866, 0x1145_b7e2_85bf_98f5],
    [0xeb1d_bd92_3d85_96ca, 0x1652_efdc_6018_a1fc],
    [0xb4c1_b80b_22ae_923c, 0x1cda_6205_5b2d_9d83],
    [0x5bb2_8b4e_8f7e_4c30, 0x12a5_568b_9f52_f416],
    [0xf08a_ed43_7682_d4fb, 0x1819_6515_31f9_e78f],
    [0xb4ee_134a_d99b_f150, 0x1f25_c186_a6f0_4c28],
    [0x1649_9ecb_70c2_5f03, 0x1420_eb44_9c88_42e6],
    [0x85a5_6ead_3608_65b0, 0x1a03_fde2_14ca_f085],
    [0x093d_b1d5_7999_890b, 0x10cf_eb35_3a97_dad8],
    [0xcf38_bb73_5e3f_36ac, 0x15ba_af44_fa52_673e],
];
const POW5_INV_SPLIT: [[u64; 2]; 15] = [
    [0x1, 0x2000_0000_0000_0000],
    [0x52a6_c95f_c065_5034, 0x18c2_40c4_aecb_13bb],
    [0x7ca8_d500_71df_c806, 0x1327_fc58_da0f_6ff5],
    [0x6520_247d_3556_476e, 0x1da4_8ce4_68e7_c702],
    [0x6139_cdd7_6802_e6e9, 0x16ef_5b40_c2fc_7779],
    [0xf951_a7ff_43de_8c79, 0x11be_bdf5_78b2_f391],
    [0x7be8_bee8_d6e9_57e8, 0x1b75_8d84_8fac_54b0],
    [0x8bd3_f9e9_99a4_23ea, 0x153e_da61_4071_a3b7],
    [0x0848_f973_cb3e_e3ce, 0x1070_1bd5_27b4_978c],
    [0x1532_85eb_b9ef_bfa2, 0x196f_bb9b_b44d_b44d],
    [0xadee_e7f8_6c07_b696, 0x13ae_3591_f5b4_d936],
    [0x4d68_6a4e_af18_2222, 0x1e74_404f_3daa_da91],
    [0x98c0_a106_e09e_bd9f, 0x1790_0ea4_fda7_c257],
    [0x8f20_e373_7149_7d0e, 0x123b_1405_76d8_20b2],
    [0xb043_1381_3474_3d85, 0x1c35_f427_5f7a_29ad],
];
const POW5_OFFSETS: [u32; 21] = [
    0x0,
    0x0,
    0x0,
    0x0,
    0x4000_0000,
    0x5969_5995,
    0x5554_5555,
    0x5655_5515,
    0x4115_0504,
    0x4055_5410,
    0x4455_5145,
    0x4450_4540,
    0x4555_5550,
    0x4000_4000,
    0x9644_0440,
    0x5556_5565,
    0x5445_4045,
    0x4015_4151,
    0x5555_9155,
    0x5140_5555,
    0x105,
];
const POW5_INV_OFFSETS: [u32; 22] = [
    0x5454_4554,
    0x0405_5545,
    0x1004_1000,
    0x0040_0414,
    0x4001_0000,
    0x4115_5555,
    0x454,
    0x0001_0044,
    0x4000_0000,
    0x4400_0041,
    0x5045_4450,
    0x5555_0054,
    0x5165_5554,
    0x4000_4000,
    0x0100_0001,
    0x0001_0500,
    0x5151_5411,
    0x0555_5554,
    0x5041_1500,
    0x4004_0000,
    0x0504_0110,
    0x0,
];

/// Byte offsets in the table image.
const POW5_AT: i32 = 0;
const POW5_SPLIT_AT: i32 = 26 * 8;
const POW5_INV_SPLIT_AT: i32 = POW5_SPLIT_AT + 13 * 16;
const POW5_OFFSETS_AT: i32 = POW5_INV_SPLIT_AT + 15 * 16;
const POW5_INV_OFFSETS_AT: i32 = POW5_OFFSETS_AT + 21 * 4;

/// The absolute address of a table at byte offset `at`.
const fn table(at: i32) -> i32 {
    abi::RYU_TABLES_ADDR.cast_signed() + at
}

/// The static image of the Ryu tables, placed at [`abi::RYU_TABLES_ADDR`].
pub(super) fn tables() -> Vec<u8> {
    let mut image = Vec::with_capacity(abi::RYU_TABLES_SIZE as usize);
    image.extend(POW5.iter().flat_map(|v| v.to_le_bytes()));
    image.extend(POW5_SPLIT.iter().flatten().flat_map(|v| v.to_le_bytes()));
    image.extend(
        POW5_INV_SPLIT
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes()),
    );
    image.extend(POW5_OFFSETS.iter().flat_map(|v| v.to_le_bytes()));
    image.extend(POW5_INV_OFFSETS.iter().flat_map(|v| v.to_le_bytes()));
    image
}

/// A [`MemArg`] with no alignment promise at absolute address or offset `at`.
fn mem(at: i32) -> MemArg {
    MemArg {
        offset: u64::from(at.cast_unsigned()),
        align: 0,
        memory_index: 0,
    }
}

/// `memory.copy` in memory 0.
const COPY: Instruction<'static> = Instruction::MemoryCopy {
    src_mem: 0,
    dst_mem: 0,
};

/// The bodies of the helpers, in index order from [`Helpers::dtoa`].
pub(super) fn emit_helpers(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) -> [Function; HELPER_COUNT as usize] {
    [
        emit_umul128(),
        emit_pow5(helpers),
        emit_mul_shift(helpers),
        emit_multiple_of_pow5(),
        emit_d2d(helpers),
        emit_scratch_str(helpers),
        emit_num_to_str(strings, helpers),
        emit_dec_expand(),
        emit_num_format(helpers),
        emit_num_radix(helpers),
        emit_prim_str(strings, helpers),
        emit_msg_str(strings, helpers),
    ]
}

// --- Ryu ---------------------------------------------------------------------

/// `__umul128(a: i64, b: i64) -> (low: i64, high: i64)`: the full 128-bit
/// product from four multiplies of the 32-bit halves.
fn emit_umul128() -> Function {
    let (lhs, rhs, low_low, low_high, high_low, high_high, mid_one, mid_two) =
        (0, 1, 2, 3, 4, 5, 6, 7);
    let mut f = Function::new(vec![(6, ValType::I64)]);
    let low = |f: &mut Function, x: u32| wasm!(f; LocalGet(x), I64Const(LOW_32), I64And);
    let high = |f: &mut Function, x: u32| wasm!(f; LocalGet(x), I64Const(32), I64ShrU);
    low(&mut f, lhs);
    low(&mut f, rhs);
    wasm!(f; I64Mul, LocalSet(low_low));
    low(&mut f, lhs);
    high(&mut f, rhs);
    wasm!(f; I64Mul, LocalSet(low_high));
    high(&mut f, lhs);
    low(&mut f, rhs);
    wasm!(f; I64Mul, LocalSet(high_low));
    high(&mut f, lhs);
    high(&mut f, rhs);
    wasm!(f; I64Mul, LocalSet(high_high), LocalGet(high_low));
    high(&mut f, low_low);
    wasm!(f; I64Add, LocalSet(mid_one), LocalGet(low_high));
    low(&mut f, mid_one);
    wasm!(f; I64Add, LocalSet(mid_two), LocalGet(mid_two), I64Const(32), I64Shl);
    low(&mut f, low_low);
    wasm!(f; I64Or, LocalGet(high_high));
    high(&mut f, mid_one);
    wasm!(f; I64Add);
    high(&mut f, mid_two);
    wasm!(f; I64Add, End);
    f
}

/// Push `pow5bits(local)`: the bit length of `5^local`.
fn pow5bits(f: &mut Function, local: u32) {
    wasm!(f;
        LocalGet(local), I32Const(1_217_359), I32Mul, I32Const(19), I32ShrU,
        I32Const(1), I32Add,
    );
}

/// Push `(hi:lo) >> dist` truncated to 64 bits (`dist`, an i64 local, in
/// `1..64`).
fn shift_right_128(f: &mut Function, lo: u32, hi: u32, dist: u32) {
    wasm!(f;
        LocalGet(hi), I64Const(64), LocalGet(dist), I64Sub, I64Shl,
        LocalGet(lo), LocalGet(dist), I64ShrU, I64Or,
    );
}

/// Set `sum = carry_in + add` and bump `high` on the 64-bit overflow.
fn add_with_carry(f: &mut Function, sum: u32, carry_in: u32, add: u32, high: u32) {
    wasm!(f;
        LocalGet(carry_in), LocalGet(add), I64Add, LocalTee(sum), LocalGet(carry_in), I64LtU,
        If(BlockType::Empty), LocalGet(high), I64Const(1), I64Add, LocalSet(high), End,
    );
}

/// `__ryu_pow5(i: i32, inv: i32) -> (low: i64, high: i64)`: the 125-bit
/// `5^i` (`double_computePow5`), or with `inv` the inverse
/// `2^k / 5^i + 1` (`double_computeInvPow5`), from a table entry times
/// `5^offset` and its 2-bit correction.
fn emit_pow5(helpers: Helpers<'_>) -> Function {
    let (i, inv) = (0, 1);
    let (base2, offset, bits_delta, addr) = (2, 3, 4, 5);
    let (mul_low, mul_high, factor, prod_low, carry_low, prod_high, carry_high, sum, delta) =
        (6, 7, 8, 9, 10, 11, 12, 13, 14);
    let mut f = Function::new(vec![(4, ValType::I32), (9, ValType::I64)]);
    let umul = helpers.umul128();
    wasm!(f;
        // base = (i + (inv ? 25 : 0)) / 26; the table entry; |base * 26 - i|.
        LocalGet(i), I32Const(POW5_STEP - 1), LocalGet(inv), I32Mul, I32Add,
        I32Const(POW5_STEP), I32DivU, LocalTee(base2),
        I32Const(4), I32Shl,
        I32Const(table(POW5_INV_SPLIT_AT)), I32Const(table(POW5_SPLIT_AT)), LocalGet(inv), Select,
        I32Add, LocalSet(addr),
        LocalGet(base2), I32Const(POW5_STEP), I32Mul, LocalSet(base2),
        LocalGet(base2), LocalGet(i), I32Sub, LocalSet(offset),
        I32Const(0), LocalGet(offset), I32Sub, LocalGet(offset),
        LocalGet(offset), I32Const(0), I32LtS, Select, LocalSet(offset),
        LocalGet(addr), I64Load(mem(0)), LocalSet(mul_low),
        LocalGet(addr), I64Load(mem(8)), LocalSet(mul_high),
        LocalGet(offset), I32Eqz, If(BlockType::Empty),
        LocalGet(mul_low), LocalGet(mul_high), Return,
        End,
        LocalGet(offset), I32Const(3), I32Shl, I64Load(mem(table(POW5_AT))), LocalSet(factor),
        LocalGet(factor), LocalGet(mul_high), Call(umul), LocalSet(prod_high), LocalSet(carry_high),
        LocalGet(factor), LocalGet(mul_low), LocalGet(inv), I64ExtendI32U, I64Sub,
        Call(umul), LocalSet(carry_low), LocalSet(prod_low),
    );
    add_with_carry(&mut f, sum, carry_low, carry_high, prod_high);
    // The bit-length difference of `5^i` and the table's power.
    pow5bits(&mut f, i);
    pow5bits(&mut f, base2);
    wasm!(f;
        I32Sub, LocalSet(bits_delta),
        I32Const(0), LocalGet(bits_delta), I32Sub, LocalGet(bits_delta), LocalGet(inv), Select,
        I64ExtendI32U, LocalSet(delta),
    );
    shift_right_128(&mut f, prod_low, sum, delta);
    wasm!(f;
        LocalGet(i), I32Const(16), I32DivU, I32Const(2), I32Shl,
        I32Const(table(POW5_INV_OFFSETS_AT)), I32Const(table(POW5_OFFSETS_AT)), LocalGet(inv),
        Select, I32Add, I32Load(mem(0)),
        LocalGet(i), I32Const(15), I32And, I32Const(1), I32Shl, I32ShrU,
        I32Const(3), I32And, LocalGet(inv), I32Add, I64ExtendI32U, I64Add,
    );
    shift_right_128(&mut f, sum, prod_high, delta);
    wasm!(f; End);
    f
}

/// `__ryu_mul_shift(m: i64, low: i64, high: i64, j: i32) -> i64`: `mulShift64`,
/// `(m * (high:low)) >> j` for `j` in `65..128`.
fn emit_mul_shift(helpers: Helpers<'_>) -> Function {
    let (factor, mul_low, mul_high, shift) = (0, 1, 2, 3);
    let (carry_high, prod_high, carry_low, sum, dist) = (4, 5, 6, 7, 8);
    let mut f = Function::new(vec![(5, ValType::I64)]);
    let umul = helpers.umul128();
    wasm!(f;
        LocalGet(factor), LocalGet(mul_high), Call(umul), LocalSet(prod_high), LocalSet(carry_high),
        LocalGet(factor), LocalGet(mul_low), Call(umul), LocalSet(carry_low), Drop,
    );
    add_with_carry(&mut f, sum, carry_low, carry_high, prod_high);
    wasm!(f; LocalGet(shift), I32Const(64), I32Sub, I64ExtendI32U, LocalSet(dist));
    shift_right_128(&mut f, sum, prod_high, dist);
    wasm!(f; End);
    f
}

/// `__ryu_mp5(value: i64, p: i32) -> i32`: whether the nonzero `value` is a
/// multiple of `5^p`.
fn emit_multiple_of_pow5() -> Function {
    let (value, power, count) = (0, 1, 2);
    let mut f = Function::new(vec![(1, ValType::I32)]);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(value), I64Const(5), I64RemU, I64Const(0), I64Ne, BrIf(1),
        LocalGet(value), I64Const(5), I64DivU, LocalSet(value),
        LocalGet(count), I32Const(1), I32Add, LocalSet(count),
        Br(0),
        End, End,
        LocalGet(count), LocalGet(power), I32GeU,
        End,
    );
    f
}

/// The locals of [`emit_d2d`].
#[derive(Clone, Copy)]
struct D2d {
    mantissa: u32,
    m2: u32,
    mv: u32,
    vr: u32,
    vp: u32,
    vm: u32,
    pow_low: u32,
    pow_high: u32,
    vp_next: u32,
    vm_next: u32,
    vr_next: u32,
    biased: u32,
    e2: u32,
    even: u32,
    mm_shift: u32,
    e10: u32,
    q: u32,
    shift: u32,
    vm_zeros: u32,
    vr_zeros: u32,
    removed: u32,
    last: u32,
    vm_digit: u32,
    vr_digit: u32,
}

/// The locals of [`emit_d2d`]: `bits` is parameter 0.
const D2D: D2d = D2d {
    mantissa: 1,
    m2: 2,
    mv: 3,
    vr: 4,
    vp: 5,
    vm: 6,
    pow_low: 7,
    pow_high: 8,
    vp_next: 9,
    vm_next: 10,
    vr_next: 11,
    biased: 12,
    e2: 13,
    even: 14,
    mm_shift: 15,
    e10: 16,
    q: 17,
    shift: 18,
    vm_zeros: 19,
    vr_zeros: 20,
    removed: 21,
    last: 22,
    vm_digit: 23,
    vr_digit: 24,
};

/// `__ryu_d2d(bits: i64) -> (digits: i64, exp10: i32)`: the shortest
/// round-tripping decimal `digits * 10^exp10` of the finite, positive, nonzero
/// double `bits`, closest to the exact value and ties to even (Ryu's `d2d`
/// general loop, which the upstream common-case path only speeds up).
fn emit_d2d(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(11, ValType::I64), (13, ValType::I32)]);
    d2d_decode(&mut f);
    wasm!(f; LocalGet(D2D.e2), I32Const(0), I32GeS, If(BlockType::Empty));
    d2d_positive_exponent(&mut f, helpers);
    wasm!(f; Else);
    d2d_negative_exponent(&mut f, helpers);
    wasm!(f; End);
    d2d_shorten(&mut f);
    d2d_output(&mut f);
    f
}

/// Step 1 and 2: decode `bits` and compute `mv` and `mm_shift`.
fn d2d_decode(f: &mut Function) {
    let bits = 0;
    let D2d {
        mantissa,
        m2,
        mv,
        biased,
        e2,
        even,
        mm_shift,
        ..
    } = D2D;
    wasm!(f;
        LocalGet(bits), I64Const(FRACTION_MASK), I64And, LocalSet(mantissa),
        LocalGet(bits), I64Const(52), I64ShrU, I64Const(0x7FF), I64And, I32WrapI64, LocalSet(biased),
        LocalGet(biased), I32Eqz, If(BlockType::Empty),
        I32Const(-1076), LocalSet(e2), LocalGet(mantissa), LocalSet(m2),
        Else,
        LocalGet(biased), I32Const(1077), I32Sub, LocalSet(e2),
        LocalGet(mantissa), I64Const(HIDDEN_BIT), I64Or, LocalSet(m2),
        End,
        LocalGet(m2), I64Const(1), I64And, I64Eqz, LocalSet(even),
        LocalGet(m2), I64Const(2), I64Shl, LocalSet(mv),
        LocalGet(mantissa), I64Const(0), I64Ne, LocalGet(biased), I32Const(1), I32LeU, I32Or,
        LocalSet(mm_shift),
    );
}

/// `vp`, `vm` and `vr`: `mulShiftAll64` by the power in `pow_low`/`pow_high`.
fn d2d_mul_all(f: &mut Function, helpers: Helpers<'_>) {
    let D2d {
        mv,
        vr,
        vp,
        vm,
        pow_low,
        pow_high,
        mm_shift,
        shift,
        ..
    } = D2D;
    let mul_shift = helpers.mul_shift();
    wasm!(f;
        LocalGet(mv), I64Const(2), I64Add,
        LocalGet(pow_low), LocalGet(pow_high), LocalGet(shift), Call(mul_shift), LocalSet(vp),
        LocalGet(mv), I64Const(1), I64Sub, LocalGet(mm_shift), I64ExtendI32U, I64Sub,
        LocalGet(pow_low), LocalGet(pow_high), LocalGet(shift), Call(mul_shift), LocalSet(vm),
        LocalGet(mv),
        LocalGet(pow_low), LocalGet(pow_high), LocalGet(shift), Call(mul_shift), LocalSet(vr),
    );
}

/// Step 3 for `e2 >= 0`.
fn d2d_positive_exponent(f: &mut Function, helpers: Helpers<'_>) {
    let D2d {
        mv,
        vp,
        pow_low,
        pow_high,
        e2,
        even,
        mm_shift,
        e10,
        q,
        shift,
        vm_zeros,
        vr_zeros,
        ..
    } = D2D;
    let (pow5, mp5) = (helpers.ryu_pow5(), helpers.multiple_of_pow5());
    wasm!(f;
        LocalGet(e2), I32Const(78_913), I32Mul, I32Const(18), I32ShrU,
        LocalGet(e2), I32Const(3), I32GtS, I32Sub, LocalTee(q), LocalSet(e10),
    );
    pow5bits(f, q);
    wasm!(f;
        I32Const(124), I32Add, LocalGet(q), I32Add, LocalGet(e2), I32Sub, LocalSet(shift),
        LocalGet(q), I32Const(1), Call(pow5), LocalSet(pow_high), LocalSet(pow_low),
    );
    d2d_mul_all(f, helpers);
    wasm!(f;
        LocalGet(q), I32Const(21), I32LeU, If(BlockType::Empty),
        LocalGet(mv), I64Const(5), I64RemU, I64Eqz, If(BlockType::Empty),
        LocalGet(mv), LocalGet(q), Call(mp5), LocalSet(vr_zeros),
        Else,
        LocalGet(even), If(BlockType::Empty),
        LocalGet(mv), I64Const(1), I64Sub, LocalGet(mm_shift), I64ExtendI32U, I64Sub,
        LocalGet(q), Call(mp5), LocalSet(vm_zeros),
        Else,
        LocalGet(vp), LocalGet(mv), I64Const(2), I64Add, LocalGet(q), Call(mp5), I64ExtendI32U,
        I64Sub, LocalSet(vp),
        End,
        End,
        End,
    );
}

/// Step 3 for `e2 < 0`.
fn d2d_negative_exponent(f: &mut Function, helpers: Helpers<'_>) {
    let D2d {
        mv,
        vp,
        pow_low,
        pow_high,
        e2,
        even,
        mm_shift,
        e10,
        q,
        shift,
        vm_zeros,
        vr_zeros,
        ..
    } = D2D;
    let pow5 = helpers.ryu_pow5();
    wasm!(f;
        I32Const(0), LocalGet(e2), I32Sub, LocalTee(shift),
        I32Const(732_923), I32Mul, I32Const(20), I32ShrU,
        LocalGet(shift), I32Const(1), I32GtS, I32Sub, LocalTee(q),
        LocalGet(e2), I32Add, LocalSet(e10),
        // The power index `-e2 - q` is `shift` until the `j` below replaces it.
        LocalGet(shift), LocalGet(q), I32Sub, LocalTee(shift),
        I32Const(0), Call(pow5), LocalSet(pow_high), LocalSet(pow_low),
        LocalGet(q),
    );
    pow5bits(f, shift);
    wasm!(f; I32Const(125), I32Sub, I32Sub, LocalSet(shift));
    d2d_mul_all(f, helpers);
    wasm!(f;
        LocalGet(q), I32Const(1), I32LeU, If(BlockType::Empty),
        I32Const(1), LocalSet(vr_zeros),
        LocalGet(even), If(BlockType::Empty),
        LocalGet(mm_shift), LocalSet(vm_zeros),
        Else,
        LocalGet(vp), I64Const(1), I64Sub, LocalSet(vp),
        End,
        Else,
        LocalGet(q), I32Const(63), I32LtU, If(BlockType::Empty),
        LocalGet(mv), I64Const(1), LocalGet(q), I64ExtendI32U, I64Shl, I64Const(1), I64Sub,
        I64And, I64Eqz, LocalSet(vr_zeros),
        End,
        End,
    );
}

/// Push `value - 10 * next` (the digit `value` loses) as i32.
fn low_digit(f: &mut Function, value: u32, next: u32) {
    wasm!(f;
        LocalGet(value), LocalGet(next), I64Const(10), I64Mul, I64Sub, I32WrapI64,
    );
}

/// Commit one removed digit: update `vr_zeros` and `last` from `vr_digit` and
/// move `vr`/`vp`/`vm` to their `*_next` values.
fn d2d_commit(f: &mut Function) {
    let D2d {
        vr,
        vp,
        vm,
        vp_next,
        vm_next,
        vr_next,
        vr_zeros,
        removed,
        last,
        vr_digit,
        ..
    } = D2D;
    wasm!(f;
        LocalGet(vr_zeros), LocalGet(last), I32Eqz, I32And, LocalSet(vr_zeros),
        LocalGet(vr_digit), LocalSet(last),
        LocalGet(vr_next), LocalSet(vr),
        LocalGet(vp_next), LocalSet(vp),
        LocalGet(vm_next), LocalSet(vm),
        LocalGet(removed), I32Const(1), I32Add, LocalSet(removed),
    );
}

/// Step 4: remove digits while the interval still holds a shorter decimal.
fn d2d_shorten(f: &mut Function) {
    let D2d {
        vr,
        vp,
        vm,
        vp_next,
        vm_next,
        vr_next,
        vm_zeros,
        vm_digit,
        vr_digit,
        ..
    } = D2D;
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(vp), I64Const(10), I64DivU, LocalTee(vp_next),
        LocalGet(vm), I64Const(10), I64DivU, LocalTee(vm_next),
        I64LeU, BrIf(1),
    );
    low_digit(f, vm, vm_next);
    wasm!(f;
        LocalSet(vm_digit),
        LocalGet(vr), I64Const(10), I64DivU, LocalSet(vr_next),
    );
    low_digit(f, vr, vr_next);
    wasm!(f;
        LocalSet(vr_digit),
        LocalGet(vm_zeros), LocalGet(vm_digit), I32Eqz, I32And, LocalSet(vm_zeros),
    );
    d2d_commit(f);
    wasm!(f;
        Br(0),
        End, End,
        // The lower bound itself is a shorter decimal: keep removing zeros.
        LocalGet(vm_zeros), If(BlockType::Empty),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(vm), I64Const(10), I64DivU, LocalSet(vm_next),
    );
    low_digit(f, vm, vm_next);
    wasm!(f;
        BrIf(1),
        LocalGet(vp), I64Const(10), I64DivU, LocalSet(vp_next),
        LocalGet(vr), I64Const(10), I64DivU, LocalSet(vr_next),
    );
    low_digit(f, vr, vr_next);
    wasm!(f; LocalSet(vr_digit));
    d2d_commit(f);
    wasm!(f; Br(0), End, End, End);
}

/// Push the results: `vr` rounded by the removed digits, and `e10 + removed`.
fn d2d_output(f: &mut Function) {
    let D2d {
        vr,
        vm,
        even,
        e10,
        vm_zeros,
        vr_zeros,
        removed,
        last,
        ..
    } = D2D;
    wasm!(f;
        // An exact `..50..0` rounds to even.
        LocalGet(vr_zeros), LocalGet(last), I32Const(5), I32Eq, I32And,
        LocalGet(vr), I64Const(1), I64And, I64Eqz, I32And,
        If(BlockType::Empty), I32Const(4), LocalSet(last), End,
        LocalGet(vr),
        LocalGet(vr), LocalGet(vm), I64Eq,
        LocalGet(even), I32Eqz, LocalGet(vm_zeros), I32Eqz, I32Or, I32And,
        LocalGet(last), I32Const(5), I32GeU, I32Or,
        I64ExtendI32U, I64Add,
        LocalGet(e10), LocalGet(removed), I32Add,
        End,
    );
}

// --- Text building -----------------------------------------------------------

/// Store `byte` at the cursor `p` and advance it.
fn put(f: &mut Function, p: u32, byte: i32) {
    wasm!(f;
        LocalGet(p), I32Const(byte), I32Store8(mem(0)),
        LocalGet(p), I32Const(1), I32Add, LocalSet(p),
    );
}

/// Copy `len` bytes from `src` to the cursor `p` and advance it.
fn put_copy(f: &mut Function, p: u32, src: impl Fn(&mut Function), len: impl Fn(&mut Function)) {
    wasm!(f; LocalGet(p));
    src(f);
    len(f);
    f.instruction(&COPY);
    wasm!(f; LocalGet(p));
    len(f);
    wasm!(f; I32Add, LocalSet(p));
}

/// Write `len` `'0'` bytes at the cursor `p` and advance it.
fn put_zeros(f: &mut Function, p: u32, len: impl Fn(&mut Function)) {
    wasm!(f; LocalGet(p), I32Const(ZERO));
    len(f);
    wasm!(f; MemoryFill(0), LocalGet(p));
    len(f);
    wasm!(f; I32Add, LocalSet(p));
}

/// Store `'0' + digit` (pushed by `digit`) at the cursor `p` and advance it.
fn put_digit(f: &mut Function, p: u32, digit: impl Fn(&mut Function)) {
    wasm!(f; LocalGet(p));
    digit(f);
    wasm!(f;
        I32Const(ZERO), I32Add, I32Store8(mem(0)),
        LocalGet(p), I32Const(1), I32Add, LocalSet(p),
    );
}

/// Write `e±k` for the exponent in i32 local `exp` (clobbered).
fn put_exponent(f: &mut Function, p: u32, exp: u32) {
    put(f, p, LOWER_E);
    wasm!(f; LocalGet(exp), I32Const(0), I32LtS, If(BlockType::Empty));
    put(f, p, MINUS);
    wasm!(f; I32Const(0), LocalGet(exp), I32Sub, LocalSet(exp), Else);
    put(f, p, PLUS);
    wasm!(f; End, LocalGet(exp), I32Const(100), I32GeU, If(BlockType::Empty));
    put_digit(f, p, |f| wasm!(f; LocalGet(exp), I32Const(100), I32DivU));
    wasm!(f; End, LocalGet(exp), I32Const(10), I32GeU, If(BlockType::Empty));
    put_digit(
        f,
        p,
        |f| wasm!(f; LocalGet(exp), I32Const(10), I32DivU, I32Const(10), I32RemU),
    );
    wasm!(f; End);
    put_digit(f, p, |f| wasm!(f; LocalGet(exp), I32Const(10), I32RemU));
}

/// Write the decimal digits of the i64 local `digits` (clobbered) backwards,
/// ending at `end`: `ptr` is their start and `count` their number.
fn write_digits(f: &mut Function, digits: u32, end: i32, ptr: u32, count: u32) {
    wasm!(f;
        I32Const(end), LocalSet(ptr),
        Loop(BlockType::Empty),
        LocalGet(ptr), I32Const(1), I32Sub, LocalTee(ptr),
        LocalGet(digits), I64Const(10), I64RemU, I32WrapI64, I32Const(ZERO), I32Add,
        I32Store8(mem(0)),
        LocalGet(digits), I64Const(10), I64DivU, LocalTee(digits), I64Const(0), I64Ne, BrIf(0),
        End,
        I32Const(end), LocalGet(ptr), I32Sub, LocalSet(count),
    );
}

/// `__scratch_str(ptr: i32, len: i32) -> i32`: a fresh Latin-1 string of the
/// `len` bytes at `ptr`.
fn emit_scratch_str(helpers: Helpers<'_>) -> Function {
    let (ptr, len, string) = (0, 1, 2);
    let mut f = Function::new(vec![(1, ValType::I32)]);
    wasm!(f;
        LocalGet(len), I32Const(0), Call(helpers.new_string), LocalTee(string),
        I32Const(STR_DATA_OFFSET), I32Add, LocalGet(ptr), LocalGet(len),
    );
    f.instruction(&COPY);
    wasm!(f; LocalGet(string), End);
    f
}

// --- ToString ----------------------------------------------------------------

/// `__num_to_str(bits: i64) -> i32`: `Number::toString(x, 10)` as a string
/// base.
fn emit_num_to_str(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let bits = 0;
    let digits = 1;
    let (exp10, count, point, ptr, p) = (2, 3, 4, 5, 6);
    let mut f = Function::new(vec![(1, ValType::I64), (5, ValType::I32)]);
    let at = |text: &str| static_key(strings, text);
    wasm!(f;
        LocalGet(bits), I64Const(INFINITY_BITS), I64And, I64Const(INFINITY_BITS), I64Eq,
        If(BlockType::Empty),
        LocalGet(bits), I64Const(FRACTION_MASK), I64And, I64Const(0), I64Ne,
        If(BlockType::Result(ValType::I32)),
        I32Const(at(NAN_TEXT)),
        Else,
        I32Const(at(MINUS_INFINITY_TEXT)), I32Const(at(INFINITY_TEXT)),
        LocalGet(bits), I64Const(0), I64LtS, Select,
        End,
        Return,
        End,
        LocalGet(bits), I64Const(1), I64Shl, I64Eqz, If(BlockType::Empty),
        I32Const(at(ZERO_TEXT)), Return,
        End,
        LocalGet(bits), I64Const(i64::MAX), I64And, Call(helpers.ryu_d2d()),
        LocalSet(exp10), LocalSet(digits),
    );
    write_digits(&mut f, digits, SHORT_END, ptr, count);
    wasm!(f;
        LocalGet(exp10), LocalGet(count), I32Add, LocalSet(point),
        I32Const(ROUNDED), LocalSet(p),
        LocalGet(bits), I64Const(0), I64LtS, If(BlockType::Empty),
    );
    put(&mut f, p, MINUS);
    wasm!(f; End);
    shortest_layout(&mut f, ptr, count, point, p);
    wasm!(f;
        I32Const(ROUNDED), LocalGet(p), I32Const(ROUNDED), I32Sub, Call(helpers.scratch_str()),
        End,
    );
    f
}

/// The spec layout of `count` digits at `ptr` with the decimal point at
/// `point` (clobbered): fixed for `-6 < point <= 21`, else exponential.
fn shortest_layout(f: &mut Function, ptr: u32, count: u32, point: u32, p: u32) {
    let digits = |f: &mut Function| wasm!(f; LocalGet(ptr));
    let all = |f: &mut Function| wasm!(f; LocalGet(count));
    let head = |f: &mut Function| wasm!(f; LocalGet(point));
    let tail_src = |f: &mut Function| wasm!(f; LocalGet(ptr), LocalGet(point), I32Add);
    let tail = |f: &mut Function| wasm!(f; LocalGet(count), LocalGet(point), I32Sub);
    let one = |f: &mut Function| wasm!(f; I32Const(1));
    let rest_src = |f: &mut Function| wasm!(f; LocalGet(ptr), I32Const(1), I32Add);
    let rest = |f: &mut Function| wasm!(f; LocalGet(count), I32Const(1), I32Sub);
    wasm!(f;
        LocalGet(point), I32Const(1), I32Sub, I32Const(21), I32LtU, If(BlockType::Empty),
        LocalGet(point), LocalGet(count), I32GeS, If(BlockType::Empty),
    );
    put_copy(f, p, digits, all);
    put_zeros(f, p, |f| wasm!(f; LocalGet(point), LocalGet(count), I32Sub));
    wasm!(f; Else);
    put_copy(f, p, digits, head);
    put(f, p, DOT);
    put_copy(f, p, tail_src, tail);
    wasm!(f;
        End,
        Else,
        LocalGet(point), I32Const(5), I32Add, I32Const(6), I32LtU, If(BlockType::Empty),
    );
    put(f, p, ZERO);
    put(f, p, DOT);
    put_zeros(f, p, |f| wasm!(f; I32Const(0), LocalGet(point), I32Sub));
    put_copy(f, p, digits, all);
    wasm!(f; Else);
    put_copy(f, p, digits, one);
    wasm!(f; LocalGet(count), I32Const(1), I32GtU, If(BlockType::Empty));
    put(f, p, DOT);
    put_copy(f, p, rest_src, rest);
    wasm!(f; End, LocalGet(point), I32Const(1), I32Sub, LocalSet(point));
    put_exponent(f, p, point);
    wasm!(f; End, End);
}

/// `__prim_str(v: i64) -> i32`: `ToString` of a number, boolean, `null` or
/// `undefined`.
fn emit_prim_str(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let v = 0;
    let mut f = Function::new(vec![]);
    let at = |text: &str| static_key(strings, text);
    emit_is_number(&mut f, v);
    wasm!(f;
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), Call(helpers.num_to_str()),
        Else,
        I32Const(at(UNDEFINED_TEXT)),
    );
    for (bits, text) in [
        (NULL_BITS, NULL_TEXT),
        (FALSE_BITS, FALSE_TEXT),
        (TRUE_BITS, TRUE_TEXT),
    ] {
        wasm!(f; I32Const(at(text)), LocalGet(v), I64Const(bits), I64Ne, Select);
    }
    wasm!(f; End, End);
    f
}

/// `__msg_str(v: i64) -> i32`: the text of any value in an error message: like
/// `ToString`, but a symbol is its `SymbolDescriptiveString` and an object
/// stays unconverted (`[object Object]`), so it never throws or runs user code.
fn emit_msg_str(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let v = 0;
    let mut f = Function::new(vec![(1, ValType::I64)]);
    emit_symbol_descriptive_string(&mut f, v, strings, helpers);
    emit_is_string(&mut f, v);
    wasm!(f; If(BlockType::Result(ValType::I32)), LocalGet(v), I32WrapI64, Else);
    emit_is_object(&mut f, v);
    wasm!(f;
        If(BlockType::Result(ValType::I32)),
        I32Const(static_key(strings, OBJECT_TEXT)),
        Else,
        LocalGet(v), Call(helpers.prim_str()),
        End,
        End,
        End,
    );
    f
}

// --- Exact decimal expansion -------------------------------------------------

/// The address of limb `i` (an i32 local) is pushed; load or store at
/// `mem(LIMBS)`.
fn limb_offset(f: &mut Function, i: u32) {
    wasm!(f; LocalGet(i), I32Const(2), I32Shl);
}

/// Drop the zero limbs at the top of the `n` live limbs.
fn trim_limbs(f: &mut Function, n: u32) {
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(n), I32Eqz, BrIf(1),
        LocalGet(n), I32Const(1), I32Sub, I32Const(2), I32Shl, I32Load(mem(LIMBS)),
        BrIf(1),
        LocalGet(n), I32Const(1), I32Sub, LocalSet(n),
        Br(0),
        End, End,
    );
}

/// The locals of [`emit_dec_expand`]: `bits` is parameter 0.
#[derive(Clone, Copy)]
struct Expand {
    significand: u32,
    carry: u32,
    chunk: u32,
    exp2: u32,
    ptr: u32,
    limbs: u32,
    i: u32,
    left: u32,
    count: u32,
}

/// See [`Expand`].
const EXPAND: Expand = Expand {
    significand: 1,
    carry: 2,
    chunk: 3,
    exp2: 4,
    ptr: 5,
    limbs: 6,
    i: 7,
    left: 8,
    count: 9,
};

/// `__dec_expand(bits: i64) -> (ptr: i32, count: i32, point: i32)`: the exact
/// significant decimal digits of the finite, positive, nonzero double `bits`
/// (no leading or trailing zeros) with `x = 0.digits * 10^point`: `m << e` as
/// a bignum, or `m * 5^-e` with the point `-e` digits from the right, printed
/// by repeated division by `10^9`.
fn emit_dec_expand() -> Function {
    let mut f = Function::new(vec![(3, ValType::I64), (6, ValType::I32)]);
    expand_load(&mut f);
    expand_digits(&mut f);
    expand_finish(&mut f);
    f
}

/// Decode `bits` into `significand * 2^exp2` and build the bignum in the limbs.
fn expand_load(f: &mut Function) {
    let bits = 0;
    let Expand {
        significand,
        exp2,
        limbs,
        ..
    } = EXPAND;
    wasm!(f;
        LocalGet(bits), I64Const(52), I64ShrU, I32WrapI64, LocalTee(exp2),
        If(BlockType::Empty),
        LocalGet(exp2), I32Const(1075), I32Sub, LocalSet(exp2),
        LocalGet(bits), I64Const(FRACTION_MASK), I64And, I64Const(HIDDEN_BIT), I64Or,
        LocalSet(significand),
        Else,
        I32Const(-1074), LocalSet(exp2),
        LocalGet(bits), I64Const(FRACTION_MASK), I64And, LocalSet(significand),
        End,
        I32Const(LIMBS), I32Const(0), I32Const(LIMBS_BYTES), MemoryFill(0),
        LocalGet(exp2), I32Const(0), I32GeS, If(BlockType::Empty),
    );
    expand_shift(f);
    wasm!(f; Else);
    expand_times_pow5(f);
    wasm!(f; End);
    trim_limbs(f, limbs);
}

/// The bignum `significand << exp2` for `exp2 >= 0`.
fn expand_shift(f: &mut Function) {
    let Expand {
        significand,
        carry,
        chunk,
        exp2,
        limbs,
        i,
        ..
    } = EXPAND;
    wasm!(f;
        LocalGet(exp2), I32Const(5), I32ShrU, LocalTee(i), I32Const(3), I32Add, LocalSet(limbs),
        LocalGet(exp2), I32Const(31), I32And, I64ExtendI32U, LocalSet(chunk),
        LocalGet(significand), I64Const(LOW_32), I64And, LocalGet(chunk), I64Shl, LocalSet(carry),
    );
    limb_offset(f, i);
    wasm!(f;
        LocalGet(carry), I64Store32(mem(LIMBS)),
        LocalGet(significand), I64Const(32), I64ShrU, LocalGet(chunk), I64Shl,
        LocalGet(carry), I64Const(32), I64ShrU, I64Add, LocalSet(carry),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
    );
    limb_offset(f, i);
    wasm!(f;
        LocalGet(carry), I64Store32(mem(LIMBS)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
    );
    limb_offset(f, i);
    wasm!(f; LocalGet(carry), I64Const(32), I64ShrU, I64Store32(mem(LIMBS)));
}

/// The bignum `significand * 5^-exp2` for `exp2 < 0`, by steps of at most
/// `5^13` (so a limb product plus carry fits 64 bits).
fn expand_times_pow5(f: &mut Function) {
    let Expand {
        significand,
        carry,
        chunk,
        exp2,
        limbs,
        i,
        left,
        ..
    } = EXPAND;
    wasm!(f;
        I32Const(0), LocalGet(significand), I64Store32(mem(LIMBS)),
        I32Const(4), LocalGet(significand), I64Const(32), I64ShrU, I64Store32(mem(LIMBS)),
        I32Const(2), LocalSet(limbs),
        I32Const(0), LocalGet(exp2), I32Sub, LocalSet(left),
        Loop(BlockType::Empty),
        I64Const(0), LocalSet(carry),
        I32Const(0), LocalSet(i),
        // The factor `5^min(left, 13)`.
        LocalGet(left), I32Const(13), LocalGet(left), I32Const(13), I32LtU, Select,
        I32Const(3), I32Shl, I64Load(mem(table(POW5_AT))), LocalSet(chunk),
        Loop(BlockType::Empty),
    );
    limb_offset(f, i);
    wasm!(f; I64Load32U(mem(LIMBS)), LocalGet(chunk), I64Mul, LocalGet(carry), I64Add, LocalSet(carry));
    limb_offset(f, i);
    wasm!(f;
        LocalGet(carry), I64Store32(mem(LIMBS)),
        LocalGet(carry), I64Const(32), I64ShrU, LocalSet(carry),
        LocalGet(i), I32Const(1), I32Add, LocalTee(i),
        LocalGet(limbs), I32LtU, BrIf(0),
        End,
        LocalGet(carry), I64Eqz, I32Eqz, If(BlockType::Empty),
    );
    limb_offset(f, limbs);
    wasm!(f;
        LocalGet(carry), I64Store32(mem(LIMBS)),
        LocalGet(limbs), I32Const(1), I32Add, LocalSet(limbs),
        End,
        LocalGet(left), I32Const(13), I32Sub, LocalTee(left), I32Const(0), I32GtS, BrIf(0),
        End,
    );
}

/// Print the limbs as nine-digit groups ending at the digit buffer's end.
fn expand_digits(f: &mut Function) {
    let Expand {
        carry,
        chunk,
        ptr,
        limbs,
        i,
        left,
        ..
    } = EXPAND;
    wasm!(f;
        I32Const(DIGITS + DIGITS_BYTES), LocalSet(ptr),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(limbs), I32Eqz, BrIf(1),
        I64Const(0), LocalSet(chunk),
        LocalGet(limbs), LocalSet(i),
        Loop(BlockType::Empty),
        LocalGet(i), I32Const(1), I32Sub, LocalSet(i),
        LocalGet(chunk), I64Const(32), I64Shl,
    );
    limb_offset(f, i);
    wasm!(f; I64Load32U(mem(LIMBS)), I64Or, LocalSet(carry));
    limb_offset(f, i);
    wasm!(f;
        LocalGet(carry), I64Const(1_000_000_000), I64DivU, I64Store32(mem(LIMBS)),
        LocalGet(carry), I64Const(1_000_000_000), I64RemU, LocalSet(chunk),
        LocalGet(i), BrIf(0),
        End,
        I32Const(9), LocalSet(left),
        Loop(BlockType::Empty),
        LocalGet(ptr), I32Const(1), I32Sub, LocalTee(ptr),
        LocalGet(chunk), I64Const(10), I64RemU, I32WrapI64, I32Const(ZERO), I32Add,
        I32Store8(mem(0)),
        LocalGet(chunk), I64Const(10), I64DivU, LocalSet(chunk),
        LocalGet(left), I32Const(1), I32Sub, LocalTee(left), BrIf(0),
        End,
    );
    trim_limbs(f, limbs);
    wasm!(f; Br(0), End, End);
}

/// Strip the zeros at both ends and push `(ptr, count, point)`.
fn expand_finish(f: &mut Function) {
    let Expand {
        exp2, ptr, count, ..
    } = EXPAND;
    let end = DIGITS + DIGITS_BYTES;
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(ptr), I32Load8U(mem(0)), I32Const(ZERO), I32Ne, BrIf(1),
        LocalGet(ptr), I32Const(1), I32Add, LocalSet(ptr),
        Br(0),
        End, End,
        I32Const(end), LocalGet(ptr), I32Sub, LocalSet(count),
        // The point: `count` integer digits, less the `-exp2` fraction digits.
        LocalGet(ptr),
        I32Const(end), LocalGet(ptr), I32Sub,
        LocalGet(exp2), I32Const(0), LocalGet(exp2), I32Const(0), I32LtS, Select,
        I32Add, LocalSet(exp2),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(ptr), LocalGet(count), I32Add, I32Const(1), I32Sub, I32Load8U(mem(0)),
        I32Const(ZERO), I32Ne, BrIf(1),
        LocalGet(count), I32Const(1), I32Sub, LocalSet(count),
        Br(0),
        End, End,
        LocalGet(count), LocalGet(exp2),
        End,
    );
}

// --- toFixed / toExponential / toPrecision -----------------------------------

/// The locals of [`emit_num_format`]; `bits`, `mode` and `f` are the
/// parameters 0..=2.
#[derive(Clone, Copy)]
struct Format {
    magnitude: u32,
    shortest: u32,
    negative: u32,
    ptr: u32,
    count: u32,
    point: u32,
    keep: u32,
    rounded: u32,
    rounded_point: u32,
    i: u32,
    p: u32,
    exp: u32,
    length: u32,
    zeros: u32,
    int_len: u32,
}

/// See [`Format`].
const FORMAT: Format = Format {
    magnitude: 3,
    shortest: 4,
    negative: 5,
    ptr: 6,
    count: 7,
    point: 8,
    keep: 9,
    rounded: 10,
    rounded_point: 11,
    i: 12,
    p: 13,
    exp: 14,
    length: 15,
    zeros: 16,
    int_len: 17,
};

/// `__num_format(bits: i64, mode: i32, f: i32) -> i32`: `toFixed(f)`,
/// `toExponential(f)`, `toPrecision(f)` or `toExponential()` (the `MODE_*`) of
/// the finite double `bits`, with `f` in the method's range.
fn emit_num_format(helpers: Helpers<'_>) -> Function {
    let (bits, mode) = (0, 1);
    let Format {
        magnitude,
        negative,
        p,
        ..
    } = FORMAT;
    let mut f = Function::new(vec![(2, ValType::I64), (13, ValType::I32)]);
    wasm!(f;
        LocalGet(bits), I64Const(i64::MAX), I64And, LocalSet(magnitude),
        LocalGet(mode), I32Const(MODE_FIXED), I32Eq,
        LocalGet(magnitude), F64ReinterpretI64, F64Const(Ieee64::from(1e21)), F64Ge, I32And,
        If(BlockType::Empty),
        LocalGet(bits), Call(helpers.num_to_str()), Return,
        End,
        LocalGet(bits), I64Const(0), I64LtS, LocalGet(magnitude), I64Const(0), I64Ne, I32And,
        LocalSet(negative),
    );
    format_digits(&mut f, helpers);
    wasm!(f; I32Const(TEXT), LocalSet(p), LocalGet(negative), If(BlockType::Empty));
    put(&mut f, p, MINUS);
    wasm!(f;
        End,
        LocalGet(mode), I32Const(MODE_FIXED), I32Eq, If(BlockType::Empty),
    );
    format_fixed(&mut f);
    wasm!(f; Else, LocalGet(mode), I32Const(MODE_PRECISION), I32Eq, If(BlockType::Empty));
    format_precision(&mut f);
    wasm!(f; Else);
    format_exponential(&mut f);
    wasm!(f;
        End,
        End,
        I32Const(TEXT), LocalGet(p), I32Const(TEXT), I32Sub, Call(helpers.scratch_str()),
        End,
    );
    f
}

/// Fill [`ROUNDED`] with the digits to print: sets `rounded` (their number)
/// and `rounded_point` (`x = 0.digits * 10^rounded_point`).
fn format_digits(f: &mut Function, helpers: Helpers<'_>) {
    let (mode, digits_arg) = (1, 2);
    let Format {
        magnitude,
        shortest,
        ptr,
        count,
        point,
        keep,
        rounded,
        rounded_point,
        ..
    } = FORMAT;
    wasm!(f; LocalGet(magnitude), I64Eqz, If(BlockType::Empty));
    // Zero: `f + 1` / `p` / one digit (`toFixed` pads below).
    wasm!(f;
        LocalGet(digits_arg), I32Const(1), I32Add, I32Const(1),
        LocalGet(mode), I32Const(MODE_EXPONENTIAL), I32Eq, Select,
        LocalGet(digits_arg), LocalGet(mode), I32Const(MODE_PRECISION), I32Ne, Select,
        LocalTee(rounded), LocalSet(keep),
        I32Const(ROUNDED), I32Const(ZERO), LocalGet(keep), MemoryFill(0),
        I32Const(1), LocalGet(digits_arg), I32Sub, I32Const(1),
        LocalGet(mode), I32Const(MODE_FIXED), I32Eq, Select, LocalSet(rounded_point),
        Else,
        LocalGet(mode), I32Const(MODE_SHORTEST_EXPONENTIAL), I32Eq, If(BlockType::Empty),
        LocalGet(magnitude), Call(helpers.ryu_d2d()), LocalSet(point), LocalSet(shortest),
    );
    write_digits(f, shortest, SHORT_END, ptr, count);
    wasm!(f;
        I32Const(ROUNDED), LocalGet(ptr), LocalGet(count),
    );
    f.instruction(&COPY);
    wasm!(f;
        LocalGet(count), LocalSet(rounded),
        LocalGet(point), LocalGet(count), I32Add, LocalSet(rounded_point),
        Else,
        LocalGet(magnitude), Call(helpers.dec_expand()),
        LocalSet(point), LocalSet(count), LocalSet(ptr),
        // keep: toFixed `point + f`, toExponential `f + 1`, toPrecision `p`.
        LocalGet(point), LocalGet(digits_arg), I32Add,
        LocalGet(digits_arg), I32Const(1), I32Add,
        LocalGet(digits_arg),
        LocalGet(mode), I32Const(MODE_EXPONENTIAL), I32Eq, Select,
        LocalGet(mode), I32Const(MODE_FIXED), I32Eq, Select,
        LocalSet(keep),
    );
    round_digits(f);
    wasm!(f; End, End);
}

/// Round the `count` digits at `ptr` to `keep` significant digits, ties to the
/// larger `n`, into [`ROUNDED`] (`keep < 0` rounds to no digits at all).
fn round_digits(f: &mut Function) {
    let Format {
        ptr,
        count,
        point,
        keep,
        rounded,
        rounded_point,
        i,
        ..
    } = FORMAT;
    wasm!(f;
        LocalGet(point), LocalSet(rounded_point),
        I32Const(0), LocalSet(rounded),
        LocalGet(keep), I32Const(0), I32GeS, If(BlockType::Empty),
        LocalGet(keep), LocalSet(rounded),
        // The kept digits, `min(keep, count)` of them, then zeros.
        LocalGet(keep), LocalGet(count), LocalGet(keep), LocalGet(count), I32LtS, Select, LocalSet(i),
        I32Const(ROUNDED), LocalGet(ptr), LocalGet(i),
    );
    f.instruction(&COPY);
    wasm!(f;
        I32Const(ROUNDED), LocalGet(i), I32Add, I32Const(ZERO),
        LocalGet(keep), LocalGet(i), I32Sub, MemoryFill(0),
        LocalGet(keep), LocalGet(count), I32LtS, If(BlockType::Empty),
        LocalGet(ptr), LocalGet(keep), I32Add, I32Load8U(mem(0)), I32Const(FIVE), I32GeU,
        If(BlockType::Empty),
    );
    round_up(f);
    wasm!(f; End, End, End);
}

/// Add one unit in the last of the `keep` digits at [`ROUNDED`]; a carry out
/// of all of them yields `1` followed by zeros and bumps the point.
fn round_up(f: &mut Function) {
    let Format {
        point,
        keep,
        rounded,
        rounded_point,
        i,
        ..
    } = FORMAT;
    wasm!(f;
        LocalGet(keep), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), I32Eqz, If(BlockType::Empty),
        I32Const(ROUNDED), I32Const(ZERO + 1), I32Store8(mem(0)),
        LocalGet(keep), I32Const(1), LocalGet(keep), I32Const(1), I32GeS, Select,
        LocalSet(rounded),
        LocalGet(point), I32Const(1), I32Add, LocalSet(rounded_point),
        Br(2),
        End,
        LocalGet(i), I32Const(1), I32Sub, LocalTee(i), I32Load8U(mem(ROUNDED)),
        I32Const(NINE), I32Eq, If(BlockType::Empty),
        LocalGet(i), I32Const(ZERO), I32Store8(mem(ROUNDED)),
        Br(1),
        Else,
        LocalGet(i), LocalGet(i), I32Load8U(mem(ROUNDED)), I32Const(1), I32Add,
        I32Store8(mem(ROUNDED)),
        Br(2),
        End,
        End, End,
    );
}

/// The `toFixed` text at the cursor from the rounded digits.
fn format_fixed(f: &mut Function) {
    let digits_arg = 2;
    let Format {
        magnitude,
        rounded,
        rounded_point,
        p,
        length,
        zeros,
        int_len,
        ..
    } = FORMAT;
    wasm!(f;
        LocalGet(rounded), LocalSet(length),
        // Pad to `rounded_point + f` digits.
        LocalGet(magnitude), I64Eqz, I32Eqz,
        LocalGet(length), LocalGet(rounded_point), LocalGet(digits_arg), I32Add, I32LtS, I32And,
        If(BlockType::Empty),
        I32Const(ROUNDED), LocalGet(length), I32Add, I32Const(ZERO),
        LocalGet(rounded_point), LocalGet(digits_arg), I32Add, LocalGet(length), I32Sub,
        MemoryFill(0),
        LocalGet(rounded_point), LocalGet(digits_arg), I32Add, LocalSet(length),
        End,
        LocalGet(length), I32Eqz, If(BlockType::Empty),
        I32Const(ROUNDED), I32Const(ZERO), I32Store8(mem(0)),
        I32Const(1), LocalSet(length),
        End,
        LocalGet(digits_arg), I32Eqz, If(BlockType::Empty),
    );
    let text = |f: &mut Function| wasm!(f; I32Const(ROUNDED));
    put_copy(f, p, text, |f| wasm!(f; LocalGet(length)));
    wasm!(f;
        Else,
        LocalGet(length), LocalGet(digits_arg), I32LeS, If(BlockType::Empty),
        LocalGet(digits_arg), I32Const(1), I32Add, LocalGet(length), I32Sub, LocalSet(zeros),
        I32Const(ROUNDED), LocalGet(zeros), I32Add, I32Const(ROUNDED), LocalGet(length),
    );
    f.instruction(&COPY);
    wasm!(f;
        I32Const(ROUNDED), I32Const(ZERO), LocalGet(zeros), MemoryFill(0),
        LocalGet(length), LocalGet(zeros), I32Add, LocalSet(length),
        End,
        LocalGet(length), LocalGet(digits_arg), I32Sub, LocalSet(int_len),
    );
    put_copy(f, p, text, |f| wasm!(f; LocalGet(int_len)));
    put(f, p, DOT);
    put_copy(
        f,
        p,
        |f| wasm!(f; I32Const(ROUNDED), LocalGet(int_len), I32Add),
        |f| wasm!(f; LocalGet(digits_arg)),
    );
    wasm!(f; End);
}

/// `d.ddd` + `e±k` at the cursor from the rounded digits and `exp`.
fn format_exponential_text(f: &mut Function) {
    let Format {
        rounded, p, exp, ..
    } = FORMAT;
    put_copy(
        f,
        p,
        |f| wasm!(f; I32Const(ROUNDED)),
        |f| wasm!(f; I32Const(1)),
    );
    wasm!(f; LocalGet(rounded), I32Const(1), I32GtU, If(BlockType::Empty));
    put(f, p, DOT);
    put_copy(
        f,
        p,
        |f| wasm!(f; I32Const(ROUNDED + 1)),
        |f| wasm!(f; LocalGet(rounded), I32Const(1), I32Sub),
    );
    wasm!(f; End);
    put_exponent(f, p, exp);
}

/// The `toExponential` text.
fn format_exponential(f: &mut Function) {
    let Format {
        rounded_point, exp, ..
    } = FORMAT;
    wasm!(f; LocalGet(rounded_point), I32Const(1), I32Sub, LocalSet(exp));
    format_exponential_text(f);
}

/// The `toPrecision` text: exponential for `e < -6 || e >= p`, else fixed.
fn format_precision(f: &mut Function) {
    let precision = 2;
    let Format {
        rounded,
        rounded_point,
        p,
        exp,
        ..
    } = FORMAT;
    let text = |f: &mut Function| wasm!(f; I32Const(ROUNDED));
    let all = |f: &mut Function| wasm!(f; LocalGet(rounded));
    wasm!(f;
        LocalGet(rounded_point), I32Const(1), I32Sub, LocalTee(exp), I32Const(-6), I32LtS,
        LocalGet(exp), LocalGet(precision), I32GeS, I32Or, If(BlockType::Empty),
    );
    format_exponential_text(f);
    wasm!(f;
        Else,
        LocalGet(exp), LocalGet(precision), I32Const(1), I32Sub, I32Eq, If(BlockType::Empty),
    );
    put_copy(f, p, text, all);
    wasm!(f; Else, LocalGet(exp), I32Const(0), I32GeS, If(BlockType::Empty));
    put_copy(f, p, text, |f| wasm!(f; LocalGet(exp), I32Const(1), I32Add));
    put(f, p, DOT);
    put_copy(
        f,
        p,
        |f| wasm!(f; I32Const(ROUNDED + 1), LocalGet(exp), I32Add),
        |f| wasm!(f; LocalGet(rounded), LocalGet(exp), I32Const(1), I32Add, I32Sub),
    );
    wasm!(f; Else);
    put(f, p, ZERO);
    put(f, p, DOT);
    put_zeros(f, p, |f| wasm!(f; I32Const(-1), LocalGet(exp), I32Sub));
    put_copy(f, p, text, all);
    wasm!(f; End, End, End);
}

// --- Radix toString ----------------------------------------------------------

/// The locals of [`emit_num_radix`]; `bits` and `radix` are parameters 0 and 1.
#[derive(Clone, Copy)]
struct Radix {
    integer: u32,
    fraction: u32,
    delta: u32,
    base: u32,
    remainder: u32,
    floor: u32,
    frac_len: u32,
    digit: u32,
    int_at: u32,
    string: u32,
    cursor: u32,
}

/// See [`Radix`].
const RADIX: Radix = Radix {
    integer: 2,
    fraction: 3,
    delta: 4,
    base: 5,
    remainder: 6,
    floor: 7,
    frac_len: 8,
    digit: 9,
    int_at: 10,
    string: 11,
    cursor: 12,
};

/// `__num_radix(bits: i64, radix: i32) -> i32`: `Number::toString(x, radix)`
/// for a radix other than 10 (V8's `DoubleToRadixCString`): the fraction digits
/// grow from the scratch start, the integer digits down from its end. They
/// cannot meet: a value with many integer digits has no fraction left.
fn emit_num_radix(helpers: Helpers<'_>) -> Function {
    let (bits, radix) = (0, 1);
    let Radix {
        integer,
        fraction,
        delta,
        base,
        ..
    } = RADIX;
    let mut f = Function::new(vec![(6, ValType::F64), (5, ValType::I32)]);
    wasm!(f;
        LocalGet(bits), I64Const(INFINITY_BITS), I64And, I64Const(INFINITY_BITS), I64Eq,
        LocalGet(bits), I64Const(1), I64Shl, I64Eqz, I32Or, If(BlockType::Empty),
        LocalGet(bits), Call(helpers.num_to_str()), Return,
        End,
        LocalGet(radix), F64ConvertI32U, LocalSet(base),
        LocalGet(bits), F64ReinterpretI64, F64Abs, F64Floor, LocalSet(integer),
        LocalGet(bits), F64ReinterpretI64, F64Abs, LocalGet(integer), F64Sub, LocalSet(fraction),
        // Half the distance to the next double, at least the least subnormal.
        F64Const(Ieee64::from(0.5)),
        LocalGet(bits), I64Const(i64::MAX), I64And, I64Const(1), I64Add, F64ReinterpretI64,
        LocalGet(bits), F64ReinterpretI64, F64Abs, F64Sub, F64Mul,
        F64Const(Ieee64::from(f64::from_bits(1))), F64Max, LocalSet(delta),
        LocalGet(fraction), LocalGet(delta), F64Ge, If(BlockType::Empty),
    );
    radix_fraction(&mut f);
    wasm!(f; End);
    radix_integer(&mut f);
    radix_finish(&mut f, helpers);
    f
}

/// The fraction digits, as values, at the scratch start.
fn radix_fraction(f: &mut Function) {
    let radix = 1;
    let Radix {
        integer,
        fraction,
        delta,
        base,
        frac_len,
        digit,
        ..
    } = RADIX;
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(fraction), LocalGet(base), F64Mul, LocalSet(fraction),
        LocalGet(delta), LocalGet(base), F64Mul, LocalSet(delta),
        LocalGet(frac_len),
        LocalGet(fraction), I32TruncF64U, LocalTee(digit),
        I32Store8(mem(SCRATCH)),
        LocalGet(frac_len), I32Const(1), I32Add, LocalSet(frac_len),
        LocalGet(fraction), LocalGet(digit), F64ConvertI32U, F64Sub, LocalSet(fraction),
        // Round to even at the end of the digits.
        LocalGet(fraction), F64Const(Ieee64::from(0.5)), F64Gt,
        LocalGet(fraction), F64Const(Ieee64::from(0.5)), F64Eq,
        LocalGet(digit), I32Const(1), I32And, I32And, I32Or,
        LocalGet(fraction), LocalGet(delta), F64Add, F64Const(Ieee64::from(1.0)), F64Gt, I32And,
        If(BlockType::Empty),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(frac_len), I32Eqz, If(BlockType::Empty),
        LocalGet(integer), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(integer),
        Br(2),
        End,
        LocalGet(frac_len), I32Const(1), I32Sub, LocalTee(frac_len), I32Load8U(mem(SCRATCH)),
        I32Const(1), I32Add, LocalTee(digit), LocalGet(radix), I32LtU, If(BlockType::Empty),
        LocalGet(frac_len), LocalGet(digit), I32Store8(mem(SCRATCH)),
        LocalGet(frac_len), I32Const(1), I32Add, LocalSet(frac_len),
        Br(2),
        End,
        Br(0),
        End, End,
        Br(2),
        End,
        LocalGet(fraction), LocalGet(delta), F64Lt, BrIf(1),
        Br(0),
        End, End,
    );
}

/// Push the ASCII radix digit of the i32 local `digit`.
fn radix_char(f: &mut Function, digit: u32) {
    wasm!(f;
        LocalGet(digit), I32Const(ZERO), I32Add,
        LocalGet(digit), I32Const(87), I32Add,
        LocalGet(digit), I32Const(10), I32LtU, Select,
    );
}

/// The integer digits, backwards from the scratch end; past `2^53` the
/// unrepresented low digits are zeros. `integer` is below `2^59` at the
/// modulo, so the exact remainder comes from its i64 value (plus the
/// fractional part a division may have left).
fn radix_integer(f: &mut Function) {
    let radix = 1;
    let Radix {
        integer,
        base,
        remainder,
        floor,
        digit,
        int_at,
        ..
    } = RADIX;
    wasm!(f;
        I32Const(SCRATCH_END), LocalSet(int_at),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(integer), LocalGet(base), F64Div,
        F64Const(Ieee64::from(9_007_199_254_740_992.0)), F64Ge, I32Eqz, BrIf(1),
        LocalGet(integer), LocalGet(base), F64Div, LocalSet(integer),
        LocalGet(int_at), I32Const(1), I32Sub, LocalTee(int_at), I32Const(ZERO),
        I32Store8(mem(0)),
        Br(0),
        End, End,
        Loop(BlockType::Empty),
        LocalGet(integer), F64Floor, LocalTee(floor), I64TruncF64U,
        LocalGet(radix), I64ExtendI32U, I64RemU, I32WrapI64, LocalSet(digit),
        LocalGet(digit), F64ConvertI32U, LocalGet(integer), LocalGet(floor), F64Sub, F64Add,
        LocalSet(remainder),
        LocalGet(int_at), I32Const(1), I32Sub, LocalTee(int_at),
    );
    radix_char(f, digit);
    wasm!(f;
        I32Store8(mem(0)),
        LocalGet(integer), LocalGet(remainder), F64Sub, LocalGet(base), F64Div, LocalTee(integer),
        F64Const(Ieee64::from(0.0)), F64Gt, BrIf(0),
        End,
    );
}

/// Map the fraction digits to ASCII and build the string.
fn radix_finish(f: &mut Function, helpers: Helpers<'_>) {
    let bits = 0;
    let Radix {
        frac_len,
        digit,
        int_at,
        string,
        cursor,
        ..
    } = RADIX;
    wasm!(f;
        I32Const(0), LocalSet(cursor),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(cursor), LocalGet(frac_len), I32GeU, BrIf(1),
        LocalGet(cursor), I32Load8U(mem(SCRATCH)), LocalSet(digit),
        LocalGet(cursor),
    );
    radix_char(f, digit);
    wasm!(f;
        I32Store8(mem(SCRATCH)),
        LocalGet(cursor), I32Const(1), I32Add, LocalSet(cursor),
        Br(0),
        End, End,
        // sign + integer digits + `.` + fraction digits.
        LocalGet(bits), I64Const(63), I64ShrU, I32WrapI64,
        I32Const(SCRATCH_END), LocalGet(int_at), I32Sub, I32Add,
        LocalGet(frac_len), I32Const(1), I32Add, I32Const(0), LocalGet(frac_len), Select, I32Add,
        I32Const(0), Call(helpers.new_string), LocalTee(string),
        I32Const(STR_DATA_OFFSET), I32Add, LocalSet(cursor),
        LocalGet(bits), I64Const(0), I64LtS, If(BlockType::Empty),
    );
    put(f, cursor, MINUS);
    wasm!(f; End);
    put_copy(
        f,
        cursor,
        |f| wasm!(f; LocalGet(int_at)),
        |f| wasm!(f; I32Const(SCRATCH_END), LocalGet(int_at), I32Sub),
    );
    wasm!(f; LocalGet(frac_len), If(BlockType::Empty));
    put(f, cursor, DOT);
    put_copy(
        f,
        cursor,
        |f| wasm!(f; I32Const(SCRATCH)),
        |f| wasm!(f; LocalGet(frac_len)),
    );
    wasm!(f; End, LocalGet(string), End);
}
