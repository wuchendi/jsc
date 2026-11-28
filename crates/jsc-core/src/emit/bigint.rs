//! The `BigInt` runtime (DESIGN.md §6.10): block allocation and
//! canonicalization, equality, comparison and hashing, the conversions to and
//! from strings and numbers, `ToNumeric` / `ToBigInt`, and the `BigInt` arm of
//! loose equality.
//!
//! A `BigInt` is a [`abi::TYPE_BIGINT`] heap block: the sign in the header's
//! [`abi::BIGINT_FLAG_NEG`] bit, the magnitude as `len` little-endian `u32`
//! limbs from [`abi::BIGINT_DATA_OFFSET`]. Every block a jsval reaches is
//! canonical (nonzero top limb; `0n` is `len = 0` with the sign clear, the
//! static [`abi::BIGINT_ZERO`] when produced at run time) and immutable, so
//! equality and hashing read the stored words directly. Limb arithmetic uses
//! `i64` intermediates read as unsigned.
//!
//! Every helper is called only behind a `TYPE_BIGINT` type test (or from
//! another `BigInt` helper), so the tree shaker drops them from a program that
//! never holds a `BigInt`.

#[allow(clippy::wildcard_imports)]
use super::*;

use registry::Big;

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// `RangeError` of a result above [`abi::BIGINT_MAX_LIMBS`] limbs.
const MAX_SIZE: [&str; 2] = [
    "Maximum BigInt size exceeded",
    "RangeError: Maximum BigInt size exceeded",
];

/// The `TypeError` of `ToNumber(bigint)`.
pub(super) const BIGINT_TO_NUMBER: CalleeErrorText<'static> = [
    "Cannot convert a BigInt value to a number",
    "TypeError: Cannot convert a BigInt value to a number",
];

/// The `TypeError` of `JSON.stringify` reaching a `BigInt`.
pub(super) const BIGINT_JSON: CalleeErrorText<'static> = [
    "Do not know how to serialize a BigInt",
    "TypeError: Do not know how to serialize a BigInt",
];

/// The fragments of the `ToBigInt` errors: `Cannot convert <x> to a BigInt`.
const CONVERT_PREFIX: &str = "Cannot convert ";
/// See [`CONVERT_PREFIX`].
const CONVERT_SUFFIX: &str = " to a BigInt";
/// The suffix of a `BigInt`'s rendering (`1n`) in the console and error
/// messages.
pub(super) const SUFFIX: &str = "n";
/// The `.stack` prefix of a `SyntaxError`.
const SYNTAX_ERROR_STACK_PREFIX: &str = "SyntaxError: ";

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    [
        MAX_SIZE[0],
        MAX_SIZE[1],
        BIGINT_TO_NUMBER[0],
        BIGINT_TO_NUMBER[1],
        BIGINT_JSON[0],
        BIGINT_JSON[1],
        CONVERT_PREFIX,
        CONVERT_SUFFIX,
        SYNTAX_ERROR_STACK_PREFIX,
        TYPE_ERROR_STACK_PREFIX,
        "0",
        "bigint",
        SUFFIX,
    ]
    .into_iter()
}

/// The jsval bits of the static `0n`.
pub(super) const ZERO_BITS: i64 = HEAP_BITS | abi::BIGINT_ZERO as i64;

const TYPE: i32 = abi::TYPE_BIGINT as i32;
const NEG: i32 = abi::BIGINT_FLAG_NEG as i32;
const LEN: u64 = abi::BIGINT_LEN_OFFSET as u64;
const DATA: u64 = abi::BIGINT_DATA_OFFSET as u64;
const DATA_I32: i32 = abi::BIGINT_DATA_OFFSET.cast_signed();
const FLAGS: u64 = abi::HEADER_FLAGS_OFFSET as u64;
const RAW_HEADER: i32 = abi::TYPE_RAW as i32;

fn m8(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

/// Push the address of limb `i` (i32 local) of the block in i32 local `p`;
/// the limb itself is at offset [`DATA`] from it.
fn limb_addr(f: &mut impl Sink, p: u32, i: u32) {
    wasm!(f; LocalGet(p), LocalGet(i), I32Const(2), I32Shl, I32Add);
}

/// Push limb `i` of the block in `p` as an unsigned `i64`.
fn load_limb64(f: &mut impl Sink, p: u32, i: u32) {
    limb_addr(f, p, i);
    wasm!(f; I64Load32U(MemArg { offset: DATA, align: 2, memory_index: 0 }));
}

/// Push `1` if the jsval in `local` is a `BigInt` (a [`abi::TYPE_BIGINT`] heap
/// block), else `0`.
pub(super) fn emit_is_bigint(function: &mut impl Sink, local: u32) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(TYPE), I32Eq, Else, I32Const(0), End);
}

/// Leave the error `kind` with the static texts `[message, stack]` pending
/// (the helper's locals hold [`declare_error_scratch`] at `scratch`).
fn raise_static(
    f: &mut impl Sink,
    kind: ErrorKind,
    [message, stack]: [&str; 2],
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let error = IrNode::error_object(
        kind,
        Some(IrNode::Str(message.into())),
        IrNode::Str(stack.into()),
    );
    emit_node(&error, f, scratch, strings, helpers);
    emit_set_exc_value(f, helpers);
    wasm!(f;
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
    );
}

/// Leave the error `kind` pending whose message string jsval is in the
/// error-scratch local `scratch + 24` (`stack` = `stack_prefix` + message,
/// built into `scratch + 25`).
fn raise_dynamic(
    f: &mut impl Sink,
    kind: ErrorKind,
    stack_prefix: &str,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (msg, stack) = (scratch + 24, scratch + 25);
    wasm!(f;
        I32Const(static_key(strings, stack_prefix)),
        LocalGet(msg), I32WrapI64, Call(helpers.concat),
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(stack),
    );
    let error = IrNode::error_object(kind, Some(IrNode::Local(msg)), IrNode::Local(stack));
    emit_node(&error, f, scratch, strings, helpers);
    emit_set_exc_value(f, helpers);
    wasm!(f;
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
    );
}

/// `__bigint_new(len: i32, neg: i32) -> i32`: the only allocator of `BigInt`
/// blocks: a zeroed block of `len` limbs, the sign flag set when `neg`. A
/// `len` above [`abi::BIGINT_MAX_LIMBS`] throws `RangeError: Maximum BigInt
/// size exceeded`; that and an out-of-memory (left pending by `__alloc`)
/// return [`abi::BIGINT_ZERO`]: every caller tests `$exc_pending`.
pub(super) fn emit_new(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (len, neg, base, scratch) = (0, 1, 2, 3);
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32], &[RAW_I32]);
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    wasm!(f;
        LocalGet(len), I32Const(abi::BIGINT_MAX_LIMBS.cast_signed()), I32GtU,
        If(BlockType::Empty),
    );
    raise_static(
        &mut f,
        ErrorKind::RangeError,
        MAX_SIZE,
        scratch,
        strings,
        helpers,
    );
    wasm!(f;
        I32Const(abi::BIGINT_ZERO.cast_signed()), Return,
        End,
        LocalGet(len), I32Const(2), I32Shl, I32Const(DATA_I32), I32Add,
        Call(helpers.alloc), LocalTee(base), I32Eqz,
        If(BlockType::Empty), I32Const(abi::BIGINT_ZERO.cast_signed()), Return, End,
        LocalGet(base),
        I32Const(TYPE | (NEG << 16)), I32Const(TYPE), LocalGet(neg), Select,
        I32Store(m32(0)),
        LocalGet(base), LocalGet(len), I32Store(m32(LEN)),
        LocalGet(base),
        End,
    );
    finish_builder(f)
}

/// `__bigint_trim(p: i32) -> i64`: canonicalize the unpublished block `p`
/// (lower `len` past high zero limbs) and return its jsval, the static `0n`
/// for a zero magnitude. Leaf.
pub(super) fn emit_trim(leaves: &LeafSet) -> Function {
    let (p, len) = (0, 1);
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32]);
    wasm!(f;
        LocalGet(p), I32Load(m32(LEN)), LocalSet(len),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(len), I32Eqz, BrIf(1),
        LocalGet(p), LocalGet(len), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), BrIf(1),
        LocalGet(len), I32Const(1), I32Sub, LocalSet(len),
        Br(0),
        End, End,
        LocalGet(p), LocalGet(len), I32Store(m32(LEN)),
        LocalGet(len), I32Eqz,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(p), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        End,
    );
    finish_builder(f)
}

/// `__bigint_eq(a: i64, b: i64) -> i32`: `1` iff both jsvals are `BigInts` of
/// the same value (identical sign, `len` and limbs: canonical form), else
/// `0`. Leaf.
pub(super) fn emit_eq(leaves: &LeafSet) -> Function {
    let (a, b, pa, pb, len, i) = (0, 1, 2, 3, 4, 5);
    let mut f = helper_builder(
        leaves,
        &[ROOT_JS, ROOT_JS],
        &[RAW_I32, RAW_I32, RAW_I32, RAW_I32],
    );
    wasm!(f; LocalGet(a), LocalGet(b), I64Eq);
    emit_is_bigint(&mut f, a);
    wasm!(f; I32And, If(BlockType::Empty), I32Const(1), Return, End);
    emit_is_bigint(&mut f, a);
    emit_is_bigint(&mut f, b);
    wasm!(f;
        I32And, I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(a), I32WrapI64, LocalSet(pa),
        LocalGet(b), I32WrapI64, LocalSet(pb),
        LocalGet(pa), I32Load8U(m8(FLAGS)), LocalGet(pb), I32Load8U(m8(FLAGS)), I32Ne,
        LocalGet(pa), I32Load(m32(LEN)), LocalTee(len), LocalGet(pb), I32Load(m32(LEN)), I32Ne,
        I32Or, If(BlockType::Empty), I32Const(0), Return, End,
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
    );
    limb_addr(&mut f, pa, i);
    wasm!(f; I32Load(m32(DATA)));
    limb_addr(&mut f, pb, i);
    wasm!(f;
        I32Load(m32(DATA)), I32Ne, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        I32Const(1),
        End,
    );
    finish_builder(f)
}

/// `__bigint_cmp_mag(a: i32, b: i32) -> i32`: compare the magnitudes of the
/// `BigInt` blocks `a` and `b` (`-1` / `0` / `1`): length, then limbs from the
/// top. Leaf.
pub(super) fn emit_cmp_mag(leaves: &LeafSet) -> Function {
    let (a, b, la, lb, top_a, top_b) = (0, 1, 2, 3, 4, 5);
    let mut f = helper_builder(
        leaves,
        &[RAW_I32, RAW_I32],
        &[RAW_I32, RAW_I32, RAW_I32, RAW_I32],
    );
    wasm!(f;
        LocalGet(a), I32Load(m32(LEN)), LocalTee(la),
        LocalGet(b), I32Load(m32(LEN)), LocalTee(lb), I32Ne,
        If(BlockType::Empty),
        I32Const(1), I32Const(-1), LocalGet(la), LocalGet(lb), I32GtU, Select, Return,
        End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(la), I32Eqz, BrIf(1),
        LocalGet(la), I32Const(1), I32Sub, LocalSet(la),
    );
    limb_addr(&mut f, a, la);
    wasm!(f; I32Load(m32(DATA)), LocalSet(top_a));
    limb_addr(&mut f, b, la);
    wasm!(f;
        I32Load(m32(DATA)), LocalTee(top_b), LocalGet(top_a), I32Ne,
        If(BlockType::Empty),
        I32Const(1), I32Const(-1), LocalGet(top_a), LocalGet(top_b), I32GtU, Select, Return,
        End,
        Br(0),
        End, End,
        I32Const(0),
        End,
    );
    finish_builder(f)
}

/// `__bigint_cmp(a: i32, b: i32) -> i32`: the signed comparison of the
/// `BigInt` blocks `a` and `b` (`-1` / `0` / `1`). Leaf.
pub(super) fn emit_cmp(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (a, b, na) = (0, 1, 2);
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32], &[RAW_I32]);
    wasm!(f;
        LocalGet(a), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, LocalTee(na),
        LocalGet(b), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, I32Ne,
        If(BlockType::Empty),
        I32Const(-1), I32Const(1), LocalGet(na), Select, Return,
        End,
        LocalGet(a), LocalGet(b), Call(helpers.bigint(Big::CmpMag)),
        I32Const(-1), I32Const(1), LocalGet(na), Select,
        I32Mul,
        End,
    );
    finish_builder(f)
}

/// `__bigint_cmp_num(a: i32, x: f64) -> i32`: compare the `BigInt` block `a`
/// with the number `x` exactly, without allocating: `-1` / `0` / `1`, or `2`
/// when `x` is `NaN` (unordered). Leaf.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_cmp_num(leaves: &LeafSet) -> Function {
    let (a, num, sa, sx, len, mag, whole, mant, exp, i, ly, la, shift, t_bits) =
        (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13);
    let mut f = helper_builder(
        leaves,
        &[RAW_I32, RAW_F64],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_F64, RAW_F64, RAW_I64, RAW_I64, RAW_I32, RAW_I32,
            RAW_I32, RAW_I64, RAW_I64,
        ],
    );
    wasm!(f;
        LocalGet(num), LocalGet(num), F64Ne, If(BlockType::Empty), I32Const(2), Return, End,
        // Signs: sa, sx in {-1, 0, 1}.
        LocalGet(a), I32Load(m32(LEN)), LocalTee(len), I32Eqz,
        If(BlockType::Result(ValType::I32)),
        I32Const(0),
        Else,
        I32Const(-1), I32Const(1),
        LocalGet(a), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, Select,
        End,
        LocalSet(sa),
        LocalGet(num), F64Const(0.0.into()), F64Gt,
        LocalGet(num), F64Const(0.0.into()), F64Lt, I32Sub, LocalSet(sx),
        LocalGet(sa), LocalGet(sx), I32Ne,
        If(BlockType::Empty),
        I32Const(-1), I32Const(1), LocalGet(sa), LocalGet(sx), I32LtS, Select, Return,
        End,
        LocalGet(sa), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        // Same nonzero sign: compare |a| with mag = |num|; the result is sa times their order.
        LocalGet(num), F64Abs, LocalTee(mag),
        F64Const(f64::INFINITY.into()), F64Eq,
        If(BlockType::Empty), I32Const(0), LocalGet(sa), I32Sub, Return, End,
        LocalGet(mag), F64Floor, LocalSet(whole),
        LocalGet(mag), F64Const(18_446_744_073_709_551_616.0.into()), F64Lt,
        If(BlockType::Empty),
        LocalGet(len), I32Const(2), I32GtU,
        If(BlockType::Empty), LocalGet(sa), Return, End,
        // |a| as u64 against trunc(mag) as u64.
        LocalGet(a), I64Load32U(MemArg { offset: DATA, align: 2, memory_index: 0 }),
        LocalGet(len), I32Const(2), I32Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(a), I64Load32U(MemArg { offset: DATA + 4, align: 2, memory_index: 0 }),
        I64Const(32), I64Shl,
        Else, I64Const(0), End,
        I64Or, LocalTee(mant),
        LocalGet(whole), I64TruncF64U, LocalTee(exp), I64Ne,
        If(BlockType::Empty),
        LocalGet(sa), I32Const(0), LocalGet(sa), I32Sub,
        LocalGet(mant), LocalGet(exp), I64GtU, Select, Return,
        End,
        // Equal integer parts: a fractional mag is the larger.
        I32Const(0), LocalGet(sa), I32Sub, I32Const(0), LocalGet(mag), LocalGet(whole), F64Ne, Select,
        Return,
        End,
        // mag >= 2^64 is an integer mant * 2^exp with a 53-bit mant and exp >= 12.
        LocalGet(mag), I64ReinterpretF64, LocalTee(mant), I64Const(52), I64ShrU,
        I64Const(1075), I64Sub, LocalSet(exp),
        LocalGet(mant), I64Const(0x000F_FFFF_FFFF_FFFF), I64And,
        I64Const(0x0010_0000_0000_0000), I64Or, LocalSet(mant),
        // Bit lengths: 53 + exp against bitlen(a).
        LocalGet(exp), I32WrapI64, I32Const(53), I32Add, LocalSet(ly),
        LocalGet(len), I32Const(5), I32Shl,
        LocalGet(a), LocalGet(len), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), I32Clz, I32Sub, LocalTee(la),
        LocalGet(ly), I32Ne,
        If(BlockType::Empty),
        LocalGet(sa), I32Const(0), LocalGet(sa), I32Sub,
        LocalGet(la), LocalGet(ly), I32GtU, Select, Return,
        End,
        // Limb by limb from the top: limb i of mag is (mant << exp) >> 32i.
        LocalGet(len), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), I32Eqz, BrIf(1),
        LocalGet(i), I32Const(1), I32Sub, LocalSet(i),
        // shift = 32i - exp; limb = shift >= 0 ? mant >> shift : mant << -shift
        // (0 once the shift reaches 64).
        LocalGet(i), I64ExtendI32U, I64Const(5), I64Shl, LocalGet(exp), I64Sub, LocalTee(shift),
        I64Const(0), I64GeS,
        If(BlockType::Result(ValType::I64)),
        LocalGet(mant), LocalGet(shift), I64ShrU, I64Const(0),
        LocalGet(shift), I64Const(64), I64LtS, Select,
        Else,
        LocalGet(mant), I64Const(0), LocalGet(shift), I64Sub, I64Shl, I64Const(0),
        I64Const(0), LocalGet(shift), I64Sub, I64Const(64), I64LtS, Select,
        End,
        I64Const(0xFFFF_FFFF), I64And, LocalSet(shift),
    );
    load_limb64(&mut f, a, i);
    wasm!(f;
        LocalTee(t_bits), LocalGet(shift), I64Ne,
        If(BlockType::Empty),
        LocalGet(sa), I32Const(0), LocalGet(sa), I32Sub,
        LocalGet(t_bits), LocalGet(shift), I64GtU, Select, Return,
        End,
        Br(0),
        End, End,
        I32Const(0),
        End,
    );
    finish_builder(f)
}

/// `__bigint_hash(p: i32) -> i32`: FNV-1a over the sign byte, `len` and the
/// limbs of the `BigInt` block `p` (word by word), so equal values hash alike
/// whether static or heap blocks. Leaf.
pub(super) fn emit_hash(leaves: &LeafSet) -> Function {
    let (p, h, len, i) = (0, 1, 2, 3);
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32, RAW_I32, RAW_I32]);
    let prime = 0x0100_0193;
    wasm!(f;
        I32Const(0x811C_9DC5_u32.cast_signed()),
        LocalGet(p), I32Load8U(m8(FLAGS)), I32Xor, I32Const(prime), I32Mul,
        LocalGet(p), I32Load(m32(LEN)), LocalTee(len), I32Xor, I32Const(prime), I32Mul,
        LocalSet(h),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(h),
    );
    limb_addr(&mut f, p, i);
    wasm!(f;
        I32Load(m32(DATA)), I32Xor, I32Const(prime), I32Mul, LocalSet(h),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(h),
        End,
    );
    finish_builder(f)
}

/// Push the digit character of the value in i32 local `d` (`0-9a-z`).
fn digit_char(f: &mut impl Sink, d: u32) {
    wasm!(f;
        LocalGet(d), I32Const(48), I32Add,
        I32Const(39), I32Const(0), LocalGet(d), I32Const(9), I32GtU, Select,
        I32Add,
    );
}

/// `__bigint_to_string(v: i64, radix: i32) -> i32`: the digits of the
/// `BigInt` `v` in `radix` (`2..=36`, lowercase), with a leading `-` for a
/// negative value, as a fresh string base (`"0"` for `0n`). The magnitude is
/// copied into an unpublished scratch block and divided in place by the
/// largest `radix^k <= 2^32 - 1`, collecting `k`-digit chunks (O(n²)); the
/// string is then filled from the least significant chunk (padded to `k`
/// digits) up to the most significant one (unpadded). Out of memory returns
/// the static empty string with the `RangeError` pending.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_to_string(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let (v, radix) = (0, 1);
    let (src, len, big, per_chunk, scr, chunks, count, used, i, rem, wide, out, pos, chunk, digit) =
        (2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
    let mut f = helper_builder(
        leaves,
        &[ROOT_JS, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I64, RAW_I32, ROOT_BASE, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32,
            RAW_I64, RAW_I64, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32,
        ],
    );
    let oom = runtime::OOM_RETURN_EMPTY_STR;
    let raw_block = |f: &mut FnBuilder<'_>, size: &dyn Fn(&mut FnBuilder<'_>), base: u32| {
        size(f);
        wasm!(f; Call(helpers.alloc), LocalSet(base));
        emit_oom_check(f, base, &oom);
        wasm!(f; LocalGet(base), I32Const(RAW_HEADER), I32Store(m32(0)));
    };
    wasm!(f;
        LocalGet(v), I32WrapI64, LocalTee(src), I32Load(m32(LEN)), LocalTee(len), I32Eqz,
        If(BlockType::Empty), I32Const(static_key(strings, "0")), Return, End,
        // big = radix^per_chunk, the largest power not above 2^32 - 1.
        LocalGet(radix), I64ExtendI32U, LocalSet(big),
        I32Const(1), LocalSet(per_chunk),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(big), LocalGet(radix), I64ExtendI32U, I64Mul, LocalTee(wide),
        I64Const(0xFFFF_FFFF), I64GtU, BrIf(1),
        LocalGet(wide), LocalSet(big),
        LocalGet(per_chunk), I32Const(1), I32Add, LocalSet(per_chunk),
        Br(0),
        End, End,
    );
    raw_block(
        &mut f,
        &|f| wasm!(f; LocalGet(len), I32Const(2), I32Shl, I32Const(DATA_I32), I32Add),
        scr,
    );
    wasm!(f;
        LocalGet(scr), LocalGet(len), I32Store(m32(LEN)),
        LocalGet(scr), I32Const(DATA_I32), I32Add,
        LocalGet(src), I32Const(DATA_I32), I32Add,
        LocalGet(len), I32Const(2), I32Shl,
        MemoryCopy { src_mem: 0, dst_mem: 0 },
    );
    // At most 2·len + 1 chunks: radix^per_chunk > 2^26 for every radix.
    raw_block(
        &mut f,
        &|f| wasm!(f; LocalGet(len), I32Const(3), I32Shl, I32Const(DATA_I32 + 8), I32Add),
        chunks,
    );
    wasm!(f;
        LocalGet(len), LocalSet(used),
        I32Const(0), LocalSet(count),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(used), I32Eqz, BrIf(1),
        // One short division of the scratch magnitude by big, from the top.
        I64Const(0), LocalSet(rem),
        LocalGet(used), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), I32Eqz, BrIf(1),
        LocalGet(i), I32Const(1), I32Sub, LocalSet(i),
        LocalGet(rem), I64Const(32), I64Shl,
    );
    load_limb64(&mut f, scr, i);
    wasm!(f; I64Or, LocalSet(wide));
    limb_addr(&mut f, scr, i);
    wasm!(f;
        LocalGet(wide), LocalGet(big), I64DivU, I32WrapI64, I32Store(m32(DATA)),
        LocalGet(wide), LocalGet(big), I64RemU, LocalSet(rem),
        Br(0),
        End, End,
        LocalGet(chunks), LocalGet(count), I32Const(2), I32Shl, I32Add,
        LocalGet(rem), I32WrapI64, I32Store(m32(DATA)),
        LocalGet(count), I32Const(1), I32Add, LocalSet(count),
        // Drop the high zero limbs.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(used), I32Eqz, BrIf(1),
        LocalGet(scr), LocalGet(used), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), BrIf(1),
        LocalGet(used), I32Const(1), I32Sub, LocalSet(used),
        Br(0),
        End, End,
        Br(0),
        End, End,
        // The length: the top chunk's digits, per_chunk per lower chunk, the sign.
        LocalGet(chunks), LocalGet(count), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), LocalSet(chunk),
        I32Const(0), LocalSet(pos),
        Loop(BlockType::Empty),
        LocalGet(pos), I32Const(1), I32Add, LocalSet(pos),
        LocalGet(chunk), LocalGet(radix), I32DivU, LocalTee(chunk), BrIf(0),
        End,
        LocalGet(pos),
        LocalGet(count), I32Const(1), I32Sub, LocalGet(per_chunk), I32Mul, I32Add,
        LocalGet(src), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, I32Add,
        LocalTee(pos), I32Const(0), Call(helpers.new_string), LocalTee(out), I32Eqz,
        If(BlockType::Empty),
    );
    for instruction in &oom {
        f.instruction(instruction);
    }
    wasm!(f;
        End,
        // The lower chunks, per_chunk digits each, from the end.
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(count), I32Const(1), I32Sub, I32GeU, BrIf(1),
        LocalGet(chunks), LocalGet(i), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA)), LocalSet(chunk),
        LocalGet(per_chunk), LocalSet(used),
        Loop(BlockType::Empty),
        LocalGet(pos), I32Const(1), I32Sub, LocalSet(pos),
        LocalGet(chunk), LocalGet(radix), I32RemU, LocalSet(digit),
        LocalGet(out), LocalGet(pos), I32Add,
    );
    digit_char(&mut f, digit);
    wasm!(f;
        I32Store8(m8(u64::from(abi::STR_DATA_OFFSET))),
        LocalGet(chunk), LocalGet(radix), I32DivU, LocalSet(chunk),
        LocalGet(used), I32Const(1), I32Sub, LocalTee(used), BrIf(0),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        // The top chunk, unpadded.
        LocalGet(chunks), LocalGet(count), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), LocalSet(chunk),
        Loop(BlockType::Empty),
        LocalGet(pos), I32Const(1), I32Sub, LocalSet(pos),
        LocalGet(chunk), LocalGet(radix), I32RemU, LocalSet(digit),
        LocalGet(out), LocalGet(pos), I32Add,
    );
    digit_char(&mut f, digit);
    wasm!(f;
        I32Store8(m8(u64::from(abi::STR_DATA_OFFSET))),
        LocalGet(chunk), LocalGet(radix), I32DivU, LocalTee(chunk), BrIf(0),
        End,
        LocalGet(pos),
        If(BlockType::Empty),
        LocalGet(out), I32Const(i32::from(b'-')), I32Store8(m8(u64::from(abi::STR_DATA_OFFSET))),
        End,
        // `v` (and so the block `src` reads) stays rooted to the end.
        LocalGet(v), Drop,
        LocalGet(out),
        End,
    );
    finish_builder(f)
}

/// `__string_to_bigint(s: i32) -> i64`: `StringToBigInt` of the string block
/// `s` (DESIGN.md §6.10.3), or `undefined` when it is not a
/// `StringIntegerLiteral`: JS whitespace trimmed at both ends (empty → `0n`),
/// then `0x` / `0o` / `0b` and at least one digit of that radix, or an
/// optional sign and at least one decimal digit; nothing else. The digits are
/// folded into a block sized for the worst case by an in-place multiply-add
/// of `k`-digit chunks (`radix^k <= 2^32 - 1`), then trimmed. A result above
/// the size limit throws the `RangeError` of `__bigint_new`.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_from_string(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let s = 0;
    let (data, pos, end, radix, neg, start, ch, digit, p, used, mul, chunk, cnt, i, wide, carry) =
        (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, ROOT_BASE,
            RAW_I32, RAW_I64, RAW_I64, RAW_I32, RAW_I32, RAW_I64, RAW_I64,
        ],
    );
    let fail = |f: &mut FnBuilder<'_>| wasm!(f; I64Const(UNDEFINED_BITS), Return);
    let unit = |f: &mut FnBuilder<'_>, at: u32| {
        wasm!(f; LocalGet(data), LocalGet(at), Call(helpers.num_unit()));
    };
    // data: the tagged unit pointer (bit 0 = two-byte).
    wasm!(f;
        LocalGet(s), I32Const(DATA_I32), I32Add,
        LocalGet(s), I32Load8U(m8(FLAGS)),
        I32Const(i32::from(abi::STR_FLAG_TWO_BYTE)), I32And, I32Const(0), I32Ne,
        I32Or, LocalSet(data),
        LocalGet(s), I32Load(m32(u64::from(abi::STR_LEN_OFFSET))), LocalSet(end),
        I32Const(0), LocalSet(pos),
        // Trim both ends.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pos), LocalGet(end), I32GeU, BrIf(1),
    );
    unit(&mut f, pos);
    wasm!(f;
        Call(helpers.str_ws()), I32Eqz, BrIf(1),
        LocalGet(pos), I32Const(1), I32Add, LocalSet(pos),
        Br(0),
        End, End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(end), LocalGet(pos), I32LeU, BrIf(1),
        LocalGet(end), I32Const(1), I32Sub, LocalSet(i),
    );
    unit(&mut f, i);
    wasm!(f;
        Call(helpers.str_ws()), I32Eqz, BrIf(1),
        LocalGet(i), LocalSet(end),
        Br(0),
        End, End,
        LocalGet(pos), LocalGet(end), I32Eq,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        I32Const(10), LocalSet(radix),
        I32Const(0), LocalSet(neg),
    );
    unit(&mut f, pos);
    wasm!(f;
        LocalTee(ch), I32Const(48), I32Eq,
        LocalGet(end), LocalGet(pos), I32Sub, I32Const(2), I32GeU, I32And,
        If(BlockType::Empty),
        LocalGet(pos), I32Const(1), I32Add, LocalSet(i),
    );
    unit(&mut f, i);
    wasm!(f;
        I32Const(32), I32Or, LocalTee(ch),
        I32Const(i32::from(b'x')), I32Eq,
        If(BlockType::Empty), I32Const(16), LocalSet(radix), End,
        LocalGet(ch), I32Const(i32::from(b'o')), I32Eq,
        If(BlockType::Empty), I32Const(8), LocalSet(radix), End,
        LocalGet(ch), I32Const(i32::from(b'b')), I32Eq,
        If(BlockType::Empty), I32Const(2), LocalSet(radix), End,
        LocalGet(radix), I32Const(10), I32Ne,
        If(BlockType::Empty), LocalGet(pos), I32Const(2), I32Add, LocalSet(pos), End,
        End,
        LocalGet(radix), I32Const(10), I32Eq,
        If(BlockType::Empty),
    );
    unit(&mut f, pos);
    wasm!(f;
        LocalTee(ch), I32Const(i32::from(b'-')), I32Eq,
        LocalGet(ch), I32Const(i32::from(b'+')), I32Eq, I32Or,
        If(BlockType::Empty),
        LocalGet(ch), I32Const(i32::from(b'-')), I32Eq, LocalSet(neg),
        LocalGet(pos), I32Const(1), I32Add, LocalSet(pos),
        End,
        End,
        LocalGet(pos), LocalGet(end), I32Eq,
        If(BlockType::Empty),
    );
    fail(&mut f);
    wasm!(f;
        End,
        // Validate every digit first.
        LocalGet(pos), LocalTee(start), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(end), I32GeU, BrIf(1),
    );
    unit(&mut f, i);
    wasm!(f;
        I32Const(48), I32Sub, LocalTee(digit), I32Const(10), I32GeU,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(i), Call(helpers.num_unit()),
        I32Const(32), I32Or, I32Const(87), I32Sub, LocalTee(digit),
        I32Const(10), I32LtU,
        If(BlockType::Empty), I32Const(99), LocalSet(digit), End,
        End,
        LocalGet(digit), LocalGet(radix), I32GeU,
        If(BlockType::Empty),
    );
    fail(&mut f);
    wasm!(f;
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        // A block of ceil(digits · 4 / 32) + 1 limbs bounds every radix.
        LocalGet(end), LocalGet(start), I32Sub, I32Const(3), I32ShrU, I32Const(2), I32Add,
        LocalGet(neg), Call(helpers.bigint(Big::New)), LocalSet(p),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        I32Const(0), LocalSet(used),
        I64Const(1), LocalSet(mul),
        I64Const(0), LocalSet(chunk),
        I32Const(0), LocalSet(cnt),
        LocalGet(start), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    // Fold (mul, chunk) into the limbs: acc = acc · mul + chunk.
    let fold = |f: &mut FnBuilder<'_>| {
        wasm!(f;
            LocalGet(chunk), LocalSet(carry),
            I32Const(0), LocalSet(cnt),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(cnt), LocalGet(used), I32GeU, BrIf(1),
        );
        load_limb64(f, p, cnt);
        wasm!(f; LocalGet(mul), I64Mul, LocalGet(carry), I64Add, LocalSet(wide));
        limb_addr(f, p, cnt);
        wasm!(f;
            LocalGet(wide), I64Store32(MemArg { offset: DATA, align: 2, memory_index: 0 }),
            LocalGet(wide), I64Const(32), I64ShrU, LocalSet(carry),
            LocalGet(cnt), I32Const(1), I32Add, LocalSet(cnt),
            Br(0),
            End, End,
            LocalGet(carry), I64Eqz, I32Eqz,
            If(BlockType::Empty),
        );
        limb_addr(f, p, used);
        wasm!(f;
            LocalGet(carry), I64Store32(MemArg { offset: DATA, align: 2, memory_index: 0 }),
            LocalGet(used), I32Const(1), I32Add, LocalSet(used),
            End,
            I64Const(1), LocalSet(mul),
            I64Const(0), LocalSet(chunk),
        );
    };
    wasm!(f; LocalGet(i), LocalGet(end), I32GeU, BrIf(1));
    unit(&mut f, i);
    wasm!(f;
        I32Const(48), I32Sub, LocalTee(digit), I32Const(10), I32GeU,
        If(BlockType::Empty),
        LocalGet(data), LocalGet(i), Call(helpers.num_unit()),
        I32Const(32), I32Or, I32Const(87), I32Sub, LocalSet(digit),
        End,
        LocalGet(chunk), LocalGet(radix), I64ExtendI32U, I64Mul,
        LocalGet(digit), I64ExtendI32U, I64Add, LocalSet(chunk),
        LocalGet(mul), LocalGet(radix), I64ExtendI32U, I64Mul, LocalTee(mul),
        LocalGet(radix), I64ExtendI32U, I64Mul, I64Const(0xFFFF_FFFF), I64GtU,
        If(BlockType::Empty),
    );
    fold(&mut f);
    wasm!(f;
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(mul), I64Const(1), I64Ne,
        If(BlockType::Empty),
    );
    fold(&mut f);
    wasm!(f;
        End,
        LocalGet(p), LocalGet(used), I32Store(m32(LEN)),
        // `s` (read through `data`) stays rooted to the end.
        LocalGet(s), Drop,
        LocalGet(p), Call(helpers.bigint(Big::Trim)),
        End,
    );
    finish_builder(f)
}

/// `__bigint_to_f64(p: i32) -> f64`: the Number value of the `BigInt` block
/// `p`, correctly rounded (ties to even): up to 64 bits convert directly;
/// otherwise the top 64 bits, with bit 0 or-ed with "any lower bit set",
/// convert (wasm rounds to nearest even at bit 11) and scale by
/// `2^(bitlen - 64)` (`Infinity` past the exponent range). Leaf.
pub(super) fn emit_to_f64(leaves: &LeafSet) -> Function {
    let (p, len, shift, word, bit, top, i, res) = (0, 1, 2, 3, 4, 5, 6, 7);
    let mut f = helper_builder(
        leaves,
        &[RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I64, RAW_I32, RAW_F64,
        ],
    );
    let limb = |f: &mut FnBuilder<'_>, extra: u64| {
        wasm!(f;
            LocalGet(p), LocalGet(word), I32Const(2), I32Shl, I32Add,
            I64Load32U(MemArg { offset: DATA + extra, align: 2, memory_index: 0 }),
        );
    };
    wasm!(f;
        LocalGet(p), I32Load(m32(LEN)), LocalTee(len), I32Const(2), I32LeU,
        If(BlockType::Empty),
        LocalGet(len), I32Eqz,
        If(BlockType::Empty), F64Const(0.0.into()), Return, End,
        LocalGet(p), I64Load32U(MemArg { offset: DATA, align: 2, memory_index: 0 }),
        LocalGet(len), I32Const(2), I32Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(p), I64Load32U(MemArg { offset: DATA + 4, align: 2, memory_index: 0 }),
        I64Const(32), I64Shl,
        Else, I64Const(0), End,
        I64Or, F64ConvertI64U, LocalSet(res),
        Else,
        // shift = bitlen - 64 >= 1; the top 64 bits start at bit shift.
        LocalGet(len), I32Const(5), I32Shl,
        LocalGet(p), LocalGet(len), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), I32Clz, I32Sub, I32Const(64), I32Sub, LocalTee(shift),
        I32Const(5), I32ShrU, LocalSet(word),
        LocalGet(shift), I32Const(31), I32And, LocalSet(bit),
    );
    limb(&mut f, 4);
    wasm!(f; I64Const(32), I64Shl);
    limb(&mut f, 0);
    wasm!(f;
        I64Or, LocalGet(bit), I64ExtendI32U, I64ShrU, LocalSet(top),
        LocalGet(bit),
        If(BlockType::Empty),
    );
    limb(&mut f, 8);
    wasm!(f;
        I64Const(64), LocalGet(bit), I64ExtendI32U, I64Sub, I64Shl,
        LocalGet(top), I64Or, LocalSet(top),
        // Sticky: the bits of limb word below bit.
    );
    limb(&mut f, 0);
    wasm!(f;
        I64Const(1), LocalGet(bit), I64ExtendI32U, I64Shl, I64Const(1), I64Sub, I64And,
        I64Const(0), I64Ne, I64ExtendI32U, LocalGet(top), I64Or, LocalSet(top),
        End,
        // Sticky: any nonzero limb below word.
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(word), I32GeU, BrIf(1),
        LocalGet(p), LocalGet(i), I32Const(2), I32Shl, I32Add, I32Load(m32(DATA)),
        If(BlockType::Empty), LocalGet(top), I64Const(1), I64Or, LocalSet(top), Br(2), End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(shift), I32Const(1023), I32GtU,
        If(BlockType::Result(ValType::F64)),
        F64Const(f64::INFINITY.into()),
        Else,
        LocalGet(top), F64ConvertI64U,
        LocalGet(shift), I64ExtendI32U, I64Const(1023), I64Add, I64Const(52), I64Shl,
        F64ReinterpretI64, F64Mul,
        End,
        LocalSet(res),
        End,
        LocalGet(res), F64Neg, LocalGet(res),
        LocalGet(p), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, Select,
        End,
    );
    finish_builder(f)
}

/// `__bigint_from_f64(x: f64) -> i64`: `NumberToBigInt` of the integral
/// finite number `x` (the caller checked `IsIntegralNumber`): below `2^64`
/// through `i64.trunc_f64_u`, else the 53-bit mantissa shifted left by
/// `exponent - 52`.
pub(super) fn emit_from_f64(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (x, mag, bits, exp, p, word, bit) = (0, 1, 2, 3, 4, 5, 6);
    let mut f = helper_builder(
        leaves,
        &[RAW_F64],
        &[RAW_F64, RAW_I64, RAW_I32, RAW_I32, RAW_I32, RAW_I32],
    );
    let store64 = |f: &mut FnBuilder<'_>, extra: u64| {
        wasm!(f;
            LocalGet(p), LocalGet(word), I32Const(2), I32Shl, I32Add,
            LocalGet(bits), I64Store(MemArg { offset: DATA + extra, align: 2, memory_index: 0 }),
        );
    };
    wasm!(f;
        LocalGet(x), F64Const(0.0.into()), F64Eq,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(x), F64Abs, LocalTee(mag),
        F64Const(18_446_744_073_709_551_616.0.into()), F64Lt,
        If(BlockType::Empty),
        I32Const(2), LocalGet(x), F64Const(0.0.into()), F64Lt,
        Call(helpers.bigint(Big::New)), LocalSet(p),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(mag), I64TruncF64U, LocalSet(bits),
        I32Const(0), LocalSet(word),
    );
    store64(&mut f, 0);
    wasm!(f;
        LocalGet(p), Call(helpers.bigint(Big::Trim)), Return,
        End,
        LocalGet(mag), I64ReinterpretF64, LocalTee(bits), I64Const(52), I64ShrU, I32WrapI64,
        I32Const(1075), I32Sub, LocalTee(exp),
        I32Const(5), I32ShrU, LocalSet(word),
        LocalGet(exp), I32Const(31), I32And, LocalSet(bit),
        LocalGet(word), I32Const(3), I32Add, LocalGet(x), F64Const(0.0.into()), F64Lt,
        Call(helpers.bigint(Big::New)), LocalSet(p),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(bits), I64Const(0x000F_FFFF_FFFF_FFFF), I64And,
        I64Const(0x0010_0000_0000_0000), I64Or, LocalSet(bits),
        // Limbs word, word + 1 take bits << bit; limb word + 2 the bits shifted past 64.
        LocalGet(p), LocalGet(word), I32Const(2), I32Shl, I32Add,
        LocalGet(bits), I64Const(64), LocalGet(bit), I64ExtendI32U, I64Sub, I64ShrU,
        I64Const(0), LocalGet(bit), Select,
        I64Store32(MemArg { offset: DATA + 8, align: 2, memory_index: 0 }),
        LocalGet(bits), LocalGet(bit), I64ExtendI32U, I64Shl, LocalSet(bits),
    );
    store64(&mut f, 0);
    wasm!(f; LocalGet(p), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// `__to_numeric(v: i64) -> i64`: `ToNumeric`: `ToPrimitive(v, number)`,
/// then a `BigInt` is returned as is and anything else goes through
/// `__to_number` (a symbol throws). A throw stays pending (`NaN`).
pub(super) fn emit_to_numeric(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let v = 0;
    let mut f = helper_builder(leaves, &[ROOT_JS], &[]);
    emit_is_object(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32Const(convert::HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(v),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(QNAN_BITS), Return, End,
        End,
    );
    emit_is_bigint(&mut f, v);
    wasm!(f;
        If(BlockType::Empty), LocalGet(v), Return, End,
        LocalGet(v), Call(helpers.to_number),
        End,
    );
    finish_builder(f)
}

/// `__to_bigint(v: i64) -> i64`: `ToBigInt`: `ToPrimitive(v, number)`, then
/// a `BigInt` is itself, `true` / `false` are `1n` / `0n`, a string goes
/// through `StringToBigInt` (failure: the `SyntaxError` "Cannot convert
/// <s> to a `BigInt`"), and `undefined`, `null`, a number or a symbol throw
/// the `TypeError` "Cannot convert <x> to a `BigInt`". A throw stays pending
/// (`0n`).
pub(super) fn emit_to_bigint(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let (v, r, scratch) = (0, 1, 2);
    let msg = scratch + 24;
    let mut f = helper_builder(leaves, &[ROOT_JS], &[RAW_I32]);
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    emit_is_object(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32Const(convert::HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(v),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        End,
    );
    emit_is_bigint(&mut f, v);
    wasm!(f;
        If(BlockType::Empty), LocalGet(v), Return, End,
        LocalGet(v), I64Const(FALSE_BITS), I64Eq,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(v), I64Const(TRUE_BITS), I64Eq,
        If(BlockType::Empty),
        I32Const(1), I32Const(0), Call(helpers.bigint(Big::New)), LocalTee(r),
        I32Const(1), I32Store(m32(DATA)),
        LocalGet(r), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Return,
        End,
    );
    let message = |f: &mut FnBuilder<'_>, text: &dyn Fn(&mut FnBuilder<'_>)| {
        wasm!(f; I32Const(static_key(strings, CONVERT_PREFIX)));
        text(f);
        wasm!(f;
            Call(helpers.concat),
            I32Const(static_key(strings, CONVERT_SUFFIX)), Call(helpers.concat),
            I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(msg),
        );
    };
    emit_is_string(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, Call(helpers.bigint(Big::FromString)), LocalTee(scratch),
        I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty), LocalGet(scratch), Return, End,
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
    );
    message(&mut f, &|f| wasm!(f; LocalGet(v), I32WrapI64));
    raise_dynamic(
        &mut f,
        ErrorKind::SyntaxError,
        SYNTAX_ERROR_STACK_PREFIX,
        scratch,
        strings,
        helpers,
    );
    wasm!(f; I64Const(ZERO_BITS), Return, End);
    message(&mut f, &|f| wasm!(f; LocalGet(v), Call(helpers.msg_str())));
    raise_dynamic(
        &mut f,
        ErrorKind::TypeError,
        TYPE_ERROR_STACK_PREFIX,
        scratch,
        strings,
        helpers,
    );
    wasm!(f; I64Const(ZERO_BITS), End);
    finish_builder(f)
}

/// `__bigint_loose_eq(a: i64, b: i64) -> i32`: `a == b` once either operand
/// is a `BigInt` and neither is an object (`__loose_eq` converted those, and
/// booleans to numbers): two `BigInts` compare by value; a `BigInt` and a number
/// exactly (`NaN` and the infinities are unequal); a `BigInt` and a string
/// through `StringToBigInt` (an unparsable string is unequal); anything else
/// is unequal.
pub(super) fn emit_loose_eq(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (a, b, big, other) = (0, 1, 2, 3);
    let mut f = helper_builder(leaves, &[ROOT_JS, ROOT_JS], &[ROOT_JS, ROOT_JS]);
    emit_is_bigint(&mut f, a);
    emit_is_bigint(&mut f, b);
    wasm!(f;
        I32And,
        If(BlockType::Empty),
        LocalGet(a), LocalGet(b), Call(helpers.bigint(Big::Eq)), Return,
        End,
    );
    emit_is_bigint(&mut f, a);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(a), LocalSet(big), LocalGet(b), LocalSet(other),
        Else,
        LocalGet(b), LocalSet(big), LocalGet(a), LocalSet(other),
        End,
    );
    emit_is_number(&mut f, other);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(big), I32WrapI64, LocalGet(other), F64ReinterpretI64,
        Call(helpers.bigint(Big::CmpNum)), I32Eqz, Return,
        End,
    );
    emit_is_string(&mut f, other);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(other), I32WrapI64, Call(helpers.bigint(Big::FromString)), LocalSet(other),
        // A size RangeError while parsing a huge string: not equal.
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
        LocalGet(big), LocalGet(other), Call(helpers.bigint(Big::Eq)), Return,
        End,
        I32Const(0),
        End,
    );
    finish_builder(f)
}

// ---------------------------------------------------------------------------
// Operators (DESIGN.md §6.10.4, §6.10.5): the arithmetic kernels and the
// `ToNumeric` dispatch of the numeric operators.
// ---------------------------------------------------------------------------

/// `__numeric_op` / `__numeric_unary` operator codes (DESIGN.md §6.10.5).
pub(super) const NUMOP_ADD: i32 = 0;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_SUB: i32 = 1;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_MUL: i32 = 2;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_DIV: i32 = 3;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_MOD: i32 = 4;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_EXP: i32 = 5;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_AND: i32 = 6;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_OR: i32 = 7;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_XOR: i32 = 8;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_SHL: i32 = 9;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_SHR: i32 = 10;
/// See [`NUMOP_ADD`].
pub(super) const NUMOP_USHR: i32 = 11;
/// The unary `-` code of `__numeric_unary`.
pub(super) const NUMOP_NEG: i32 = 12;
/// The unary `~` code of `__numeric_unary`.
pub(super) const NUMOP_NOT: i32 = 13;

/// The `TypeError` of an operator mixing a `BigInt` with a number.
const MIX: CalleeErrorText<'static> = [
    "Cannot mix BigInt and other types, use explicit conversions",
    "TypeError: Cannot mix BigInt and other types, use explicit conversions",
];
/// The `TypeError` of `>>>` on two `BigInts`.
const USHR: CalleeErrorText<'static> = [
    "BigInts have no unsigned right shift, use >> instead",
    "TypeError: BigInts have no unsigned right shift, use >> instead",
];
/// The `RangeError` of a `BigInt` division or remainder by `0n`.
const DIV_ZERO: [&str; 2] = ["Division by zero", "RangeError: Division by zero"];
/// The `RangeError` of a negative `BigInt` exponent.
const NEG_EXP: [&str; 2] = [
    "Exponent must be non-negative",
    "RangeError: Exponent must be non-negative",
];

/// The static strings of the operator helpers.
pub(super) fn op_strings() -> impl Iterator<Item = &'static str> {
    [
        MIX[0],
        MIX[1],
        USHR[0],
        USHR[1],
        DIV_ZERO[0],
        DIV_ZERO[1],
        NEG_EXP[0],
        NEG_EXP[1],
    ]
    .into_iter()
}

/// The `2^32` limb base.
const BASE: i64 = 1 << 32;
/// The low-limb mask.
const MASK: i64 = 0xFFFF_FFFF;

/// Push `1` when the `BigInt` block in i32 local `p` is negative, else `0`.
fn sign(f: &mut impl Sink, p: u32) {
    wasm!(f; LocalGet(p), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, I32Const(0), I32Ne);
}

/// Push the limb count of the block in i32 local `p`.
fn len_of(f: &mut impl Sink, p: u32) {
    wasm!(f; LocalGet(p), I32Load(m32(LEN)));
}

/// Store the low 32 bits of the `i64` that `value` pushes as limb `i` of the
/// block in `p`.
fn store_limb(f: &mut FnBuilder<'_>, p: u32, i: u32, value: &dyn Fn(&mut FnBuilder<'_>)) {
    limb_addr(f, p, i);
    value(f);
    wasm!(f; I64Store32(m32(DATA)));
}

/// Push limb `i` of `p` as an unsigned `i64`, or `0` when `i >= len` (i32
/// local `len`).
fn load_limb_or0(f: &mut impl Sink, p: u32, i: u32, len: u32) {
    wasm!(f; LocalGet(i), LocalGet(len), I32LtU, If(BlockType::Result(ValType::I64)));
    load_limb64(f, p, i);
    wasm!(f; Else, I64Const(0), End);
}

/// `for (i = 0; i < bound; i++) body`, `bound` an i32 local.
fn for_up(f: &mut FnBuilder<'_>, i: u32, bound: u32, body: &dyn Fn(&mut FnBuilder<'_>)) {
    wasm!(f;
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(bound), I32GeU, BrIf(1),
    );
    body(f);
    wasm!(f;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
}

/// Return `BIGINT_ZERO` when the last allocation left an exception pending.
fn zero_if_pending(f: &mut impl Sink) {
    wasm!(f;
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
    );
}

/// Push the jsval of the block base in i32 local `p`.
fn jsval_of(f: &mut impl Sink, p: u32) {
    wasm!(f; LocalGet(p), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Allocate the one-limb `BigInt` `±1` (sign `neg`, an i32 constant) into
/// i32 local `r` and return its jsval.
fn return_unit(f: &mut impl Sink, r: u32, neg: i32, helpers: Helpers<'_>) {
    wasm!(f; I32Const(1), I32Const(neg), Call(helpers.bigint(Big::New)), LocalSet(r));
    zero_if_pending(f);
    wasm!(f; LocalGet(r), I32Const(1), I32Store(m32(DATA)));
    jsval_of(f, r);
    wasm!(f; Return);
}

/// `__bigint_add(a: i32, b: i32) -> i64` (`sub` false) and
/// `__bigint_sub(a: i32, b: i32) -> i64` (`sub` true): `a ± b` of two
/// `BigInt` blocks. Equal effective signs add the magnitudes (with carry,
/// `max(len) + 1` limbs); different ones subtract the smaller magnitude from
/// the larger, which gives the sign.
pub(super) fn emit_add_sub(sub: bool, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (lhs, rhs) = (0, 1);
    let (na, nb, big, small, lx, ly, res, idx, tmp, cval) = (2, 3, 4, 5, 6, 7, 8, 9, 10, 11);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I64,
            RAW_I64,
        ],
    );
    sign(&mut f, lhs);
    wasm!(f; LocalSet(na));
    sign(&mut f, rhs);
    wasm!(f; I32Const(i32::from(sub)), I32Xor, LocalSet(nb));
    // x: the operand with the larger magnitude (any of the two for an add).
    wasm!(f; LocalGet(na), LocalGet(nb), I32Eq, If(BlockType::Empty));
    len_of(&mut f, lhs);
    len_of(&mut f, rhs);
    wasm!(f;
        I32GeU,
        If(BlockType::Empty),
        LocalGet(lhs), LocalSet(big), LocalGet(rhs), LocalSet(small),
        Else,
        LocalGet(rhs), LocalSet(big), LocalGet(lhs), LocalSet(small),
        End,
    );
    len_of(&mut f, big);
    wasm!(f; LocalSet(lx));
    len_of(&mut f, small);
    wasm!(f;
        LocalSet(ly),
        LocalGet(lx), I32Const(1), I32Add, LocalGet(na), Call(helpers.bigint(Big::New)),
        LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f; I64Const(0), LocalSet(cval));
    for_up(&mut f, idx, lx, &|f| {
        load_limb64(f, big, idx);
        load_limb_or0(f, small, idx, ly);
        wasm!(f; I64Add, LocalGet(cval), I64Add, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(cval));
    });
    store_limb(&mut f, res, lx, &|f| wasm!(f; LocalGet(cval)));
    wasm!(f; LocalGet(res), Call(helpers.bigint(Big::Trim)), Return, End);
    // Different signs: |x| - |y| with |x| >= |y|, the sign of x.
    wasm!(f;
        LocalGet(lhs), LocalGet(rhs), Call(helpers.bigint(Big::CmpMag)), LocalTee(idx), I32Eqz,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(idx), I32Const(0), I32GtS,
        If(BlockType::Empty),
        LocalGet(lhs), LocalSet(big), LocalGet(rhs), LocalSet(small),
        Else,
        LocalGet(rhs), LocalSet(big), LocalGet(lhs), LocalSet(small), LocalGet(nb), LocalSet(na),
        End,
    );
    len_of(&mut f, big);
    wasm!(f; LocalSet(lx));
    len_of(&mut f, small);
    wasm!(f;
        LocalSet(ly),
        LocalGet(lx), LocalGet(na), Call(helpers.bigint(Big::New)), LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f; I64Const(0), LocalSet(cval));
    for_up(&mut f, idx, lx, &|f| {
        load_limb64(f, big, idx);
        load_limb_or0(f, small, idx, ly);
        wasm!(f; I64Sub, LocalGet(cval), I64Sub, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(0), I64LtS, I64ExtendI32U, LocalSet(cval));
    });
    wasm!(f; LocalGet(res), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// `__bigint_mul(a: i32, b: i32) -> i64`: schoolbook multiplication into
/// `len_a + len_b` limbs (every partial `r + a_i·b_j + carry` fits a `u64`);
/// a zero operand is `0n`.
pub(super) fn emit_mul(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (lhs, rhs) = (0, 1);
    let (la, lb, res, idx, jdx, acc, ai, cval, tmp) = (2, 3, 4, 5, 6, 7, 8, 9, 10);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I64, RAW_I64, RAW_I64,
        ],
    );
    len_of(&mut f, lhs);
    wasm!(f; LocalTee(la), I32Eqz);
    len_of(&mut f, rhs);
    wasm!(f;
        LocalTee(lb), I32Eqz, I32Or,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(la), LocalGet(lb), I32Add,
    );
    sign(&mut f, lhs);
    sign(&mut f, rhs);
    wasm!(f; I32Xor, Call(helpers.bigint(Big::New)), LocalSet(res));
    zero_if_pending(&mut f);
    for_up(&mut f, idx, la, &|f| {
        load_limb64(f, lhs, idx);
        wasm!(f; LocalSet(ai), I64Const(0), LocalSet(cval));
        for_up(f, jdx, lb, &|f| {
            wasm!(f; LocalGet(idx), LocalGet(jdx), I32Add, LocalSet(acc));
            load_limb64(f, res, acc);
            wasm!(f; LocalGet(ai));
            load_limb64(f, rhs, jdx);
            wasm!(f; I64Mul, I64Add, LocalGet(cval), I64Add, LocalSet(tmp));
            store_limb(f, res, acc, &|f| wasm!(f; LocalGet(tmp)));
            wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(cval));
        });
        wasm!(f; LocalGet(idx), LocalGet(lb), I32Add, LocalSet(acc));
        store_limb(f, res, acc, &|f| wasm!(f; LocalGet(cval)));
    });
    wasm!(f; LocalGet(res), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// `__bigint_divmod(a: i32, b: i32, want_rem: i32) -> i64`: the quotient
/// (truncated toward zero, sign `sa ^ sb`) or, when `want_rem`, the
/// remainder (the dividend's sign) of two `BigInt` blocks. `b = 0n` throws
/// `RangeError: Division by zero`; `|a| < |b|` is `0n` / `a`; a one-limb
/// divisor takes the short division; otherwise Knuth's Algorithm D over
/// copies normalized so the divisor's top bit is set (`q̂` from the top two
/// limbs with the two-limb correction, multiply-subtract, add back on
/// borrow), the remainder shifted back.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_divmod(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let (lhs, rhs, want_rem) = (0, 1, 2);
    let (la, lb, m_len, norm, un, vn, quot, jdx, idx, k32) = (3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
    let (num, qhat, rhat, prod, tmp, acc, vtop, vnext, rem) = (13, 14, 15, 16, 17, 18, 19, 20, 21);
    let scratch = 22;
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, ROOT_BASE, ROOT_BASE, RAW_I32, RAW_I32, RAW_I32,
            RAW_I32, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64,
            RAW_I64,
        ],
    );
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    len_of(&mut f, rhs);
    wasm!(f; LocalTee(lb), I32Eqz, If(BlockType::Empty));
    raise_static(
        &mut f,
        ErrorKind::RangeError,
        DIV_ZERO,
        scratch,
        strings,
        helpers,
    );
    wasm!(f; I64Const(ZERO_BITS), Return, End);
    len_of(&mut f, lhs);
    wasm!(f;
        LocalSet(la),
        LocalGet(lhs), LocalGet(rhs), Call(helpers.bigint(Big::CmpMag)), I32Const(0), I32LtS,
        If(BlockType::Empty),
        LocalGet(want_rem), If(BlockType::Empty),
    );
    jsval_of(&mut f, lhs);
    wasm!(f; Return, End, I64Const(ZERO_BITS), Return, End);
    // Short division by the single limb d (vtop).
    wasm!(f; LocalGet(lb), I32Const(1), I32Eq, If(BlockType::Empty));
    wasm!(f; LocalGet(rhs), I64Load32U(m32(DATA)));
    wasm!(f; LocalSet(vtop), LocalGet(want_rem), I32Eqz, If(BlockType::Empty), LocalGet(la));
    sign(&mut f, lhs);
    sign(&mut f, rhs);
    wasm!(f; I32Xor, Call(helpers.bigint(Big::New)), LocalSet(quot));
    zero_if_pending(&mut f);
    wasm!(f;
        End,
        I64Const(0), LocalSet(rem),
        LocalGet(la), LocalSet(idx),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(idx), I32Eqz, BrIf(1),
        LocalGet(idx), I32Const(1), I32Sub, LocalSet(idx),
        LocalGet(rem), I64Const(32), I64Shl,
    );
    load_limb64(&mut f, lhs, idx);
    wasm!(f; I64Or, LocalSet(num), LocalGet(want_rem), I32Eqz, If(BlockType::Empty));
    store_limb(
        &mut f,
        quot,
        idx,
        &|f| wasm!(f; LocalGet(num), LocalGet(vtop), I64DivU),
    );
    wasm!(f;
        End,
        LocalGet(num), LocalGet(vtop), I64RemU, LocalSet(rem),
        Br(0),
        End, End,
        LocalGet(want_rem), I32Eqz,
        If(BlockType::Empty), LocalGet(quot), Call(helpers.bigint(Big::Trim)), Return, End,
        LocalGet(rem), I64Eqz, If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        I32Const(1),
    );
    sign(&mut f, lhs);
    wasm!(f; Call(helpers.bigint(Big::New)), LocalSet(quot));
    zero_if_pending(&mut f);
    wasm!(f; LocalGet(quot), LocalGet(rem), I64Store32(m32(DATA)));
    jsval_of(&mut f, quot);
    wasm!(f; Return, End);
    // Algorithm D: n = lb >= 2 limbs, m = la - lb.
    wasm!(f; LocalGet(la), LocalGet(lb), I32Sub, LocalSet(m_len));
    wasm!(f;
        LocalGet(rhs), LocalGet(lb), I32Const(2), I32Shl, I32Add,
        I32Load(m32(DATA - 4)), I32Clz, LocalSet(norm),
    );
    // Normalize: limb i of x << s is the high half of (x_i:x_{i-1}) << s.
    let normalize = |f: &mut FnBuilder<'_>, src: u32, len: u32, dst: u32, count: u32| {
        wasm!(f; LocalGet(count), I32Const(0), Call(helpers.bigint(Big::New)), LocalSet(dst));
        zero_if_pending(f);
        for_up(f, idx, count, &|f| {
            store_limb(f, dst, idx, &|f| {
                load_limb_or0(f, src, idx, len);
                wasm!(f; I64Const(32), I64Shl);
                wasm!(f; LocalGet(idx), If(BlockType::Result(ValType::I64)));
                wasm!(f; LocalGet(idx), I32Const(1), I32Sub, LocalSet(k32));
                load_limb64(f, src, k32);
                wasm!(f;
                    Else, I64Const(0), End,
                    I64Or, LocalGet(norm), I64ExtendI32U, I64Shl, I64Const(32), I64ShrU,
                );
            });
        });
    };
    wasm!(f; LocalGet(la), I32Const(1), I32Add, LocalSet(jdx));
    normalize(&mut f, lhs, la, un, jdx);
    normalize(&mut f, rhs, lb, vn, lb);
    wasm!(f; LocalGet(want_rem), I32Eqz, If(BlockType::Empty), LocalGet(m_len), I32Const(1), I32Add);
    sign(&mut f, lhs);
    sign(&mut f, rhs);
    wasm!(f; I32Xor, Call(helpers.bigint(Big::New)), LocalSet(quot));
    zero_if_pending(&mut f);
    wasm!(f; End, LocalGet(lb), I32Const(1), I32Sub, LocalSet(k32));
    load_limb64(&mut f, vn, k32);
    wasm!(f; LocalSet(vtop), LocalGet(lb), I32Const(2), I32Sub, LocalSet(k32));
    load_limb64(&mut f, vn, k32);
    wasm!(f;
        LocalSet(vnext),
        LocalGet(m_len), I32Const(1), I32Add, LocalSet(jdx),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(jdx), I32Eqz, BrIf(1),
        LocalGet(jdx), I32Const(1), I32Sub, LocalSet(jdx),
        // num = u[j+n]:u[j+n-1]; q̂ = num / vtop, r̂ = num % vtop.
        LocalGet(jdx), LocalGet(lb), I32Add, LocalSet(k32),
    );
    load_limb64(&mut f, un, k32);
    wasm!(f; I64Const(32), I64Shl, LocalGet(k32), I32Const(1), I32Sub, LocalSet(k32));
    load_limb64(&mut f, un, k32);
    wasm!(f;
        I64Or, LocalTee(num), LocalGet(vtop), I64DivU, LocalSet(qhat),
        LocalGet(num), LocalGet(vtop), I64RemU, LocalSet(rhat),
        // While q̂ >= B or q̂·v[n-2] > r̂:u[j+n-2]: q̂ -= 1, r̂ += vtop (stop once r̂ >= B).
        LocalGet(k32), I32Const(1), I32Sub, LocalSet(k32),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(qhat), I64Const(BASE), I64GeU,
        LocalGet(qhat), LocalGet(vnext), I64Mul,
        LocalGet(rhat), I64Const(32), I64Shl,
    );
    load_limb64(&mut f, un, k32);
    wasm!(f;
        I64Or, I64GtU, I32Or, I32Eqz, BrIf(1),
        LocalGet(qhat), I64Const(1), I64Sub, LocalSet(qhat),
        LocalGet(rhat), LocalGet(vtop), I64Add, LocalTee(rhat),
        I64Const(BASE), I64LtU, BrIf(0),
        End, End,
        // u[j..j+n] -= q̂·v.
        I64Const(0), LocalSet(acc),
    );
    for_up(&mut f, idx, lb, &|f| {
        wasm!(f; LocalGet(qhat));
        load_limb64(f, vn, idx);
        wasm!(f; I64Mul, LocalSet(prod), LocalGet(idx), LocalGet(jdx), I32Add, LocalSet(k32));
        load_limb64(f, un, k32);
        wasm!(f;
            LocalGet(acc), I64Sub, LocalGet(prod), I64Const(MASK), I64And, I64Sub, LocalSet(tmp),
        );
        store_limb(f, un, k32, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f;
            LocalGet(prod), I64Const(32), I64ShrU, LocalGet(tmp), I64Const(32), I64ShrS, I64Sub,
            LocalSet(acc),
        );
    });
    wasm!(f; LocalGet(jdx), LocalGet(lb), I32Add, LocalSet(k32));
    load_limb64(&mut f, un, k32);
    wasm!(f; LocalGet(acc), I64Sub, LocalSet(tmp));
    store_limb(&mut f, un, k32, &|f| wasm!(f; LocalGet(tmp)));
    // Borrow: q̂ was one too large; add v back.
    wasm!(f;
        LocalGet(tmp), I64Const(0), I64LtS,
        If(BlockType::Empty),
        LocalGet(qhat), I64Const(1), I64Sub, LocalSet(qhat),
        I64Const(0), LocalSet(acc),
    );
    for_up(&mut f, idx, lb, &|f| {
        wasm!(f; LocalGet(idx), LocalGet(jdx), I32Add, LocalSet(k32));
        load_limb64(f, un, k32);
        load_limb64(f, vn, idx);
        wasm!(f; I64Add, LocalGet(acc), I64Add, LocalSet(tmp));
        store_limb(f, un, k32, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(acc));
    });
    wasm!(f; LocalGet(jdx), LocalGet(lb), I32Add, LocalSet(k32));
    store_limb(&mut f, un, k32, &|f| {
        load_limb64(f, un, k32);
        wasm!(f; LocalGet(acc), I64Add);
    });
    wasm!(f; End, LocalGet(want_rem), I32Eqz, If(BlockType::Empty));
    store_limb(&mut f, quot, jdx, &|f| wasm!(f; LocalGet(qhat)));
    wasm!(f;
        End,
        Br(0),
        End, End,
        LocalGet(want_rem), I32Eqz,
        If(BlockType::Empty), LocalGet(quot), Call(helpers.bigint(Big::Trim)), Return, End,
        // The remainder: u[0..n] >> s.
        LocalGet(lb),
    );
    sign(&mut f, lhs);
    wasm!(f; Call(helpers.bigint(Big::New)), LocalSet(quot));
    zero_if_pending(&mut f);
    for_up(&mut f, idx, lb, &|f| {
        store_limb(f, quot, idx, &|f| {
            wasm!(f; LocalGet(idx), I32Const(1), I32Add, LocalSet(k32));
            load_limb64(f, un, k32);
            wasm!(f; I64Const(32), I64Shl);
            load_limb64(f, un, idx);
            wasm!(f; I64Or, LocalGet(norm), I64ExtendI32U, I64ShrU);
        });
    });
    wasm!(f;
        // `u` stays rooted until the remainder is read.
        LocalGet(un), Drop,
        LocalGet(quot), Call(helpers.bigint(Big::Trim)),
        End,
    );
    finish_builder(f)
}

/// `__bigint_pow(a: i32, b: i32) -> i64`: `a ** b` of two `BigInt` blocks.
/// `b < 0n` throws `RangeError: Exponent must be non-negative`; `b = 0n` is
/// `1n`; a base `0n` / `1n` is itself, `-1n` alternates by the parity of `b`;
/// otherwise `b` must fit a `u32` and `bitlen(a)·b <= 2^24` (else the size
/// `RangeError`), then square-and-multiply through `__bigint_mul`.
pub(super) fn emit_pow(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (lhs, rhs) = (0, 1);
    let (e, res, base, la, scratch) = (2, 3, 4, 5, 6);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE],
        &[RAW_I32, ROOT_BASE, ROOT_BASE, RAW_I32],
    );
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    let raise = |f: &mut FnBuilder<'_>, text: [&str; 2]| {
        raise_static(f, ErrorKind::RangeError, text, scratch, strings, helpers);
        wasm!(f; I64Const(ZERO_BITS), Return);
    };
    sign(&mut f, rhs);
    wasm!(f; If(BlockType::Empty));
    raise(&mut f, NEG_EXP);
    wasm!(f; End);
    len_of(&mut f, rhs);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    return_unit(&mut f, res, 0, helpers);
    wasm!(f; End);
    len_of(&mut f, lhs);
    wasm!(f;
        LocalTee(la), I32Eqz, If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        // |a| = 1: 1n is itself, -1n is itself for an odd b, else 1n.
        LocalGet(la), I32Const(1), I32Eq,
        LocalGet(lhs), I32Load(m32(DATA)), I32Const(1), I32Eq, I32And,
        If(BlockType::Empty),
    );
    sign(&mut f, lhs);
    wasm!(f; I32Eqz, LocalGet(rhs), I32Load(m32(DATA)), I32Const(1), I32And, I32Or, If(BlockType::Empty));
    jsval_of(&mut f, lhs);
    wasm!(f; Return, End);
    return_unit(&mut f, res, 0, helpers);
    wasm!(f; End);
    len_of(&mut f, rhs);
    wasm!(f; I32Const(1), I32GtU, If(BlockType::Empty));
    raise(&mut f, MAX_SIZE);
    wasm!(f;
        End,
        LocalGet(rhs), I32Load(m32(DATA)), LocalSet(e),
        // bitlen(a) · e against 2^24.
        LocalGet(la), I32Const(5), I32Shl,
        LocalGet(lhs), LocalGet(la), I32Const(2), I32Shl, I32Add, I32Load(m32(DATA - 4)), I32Clz,
        I32Sub, I64ExtendI32U, LocalGet(e), I64ExtendI32U, I64Mul,
        I64Const(1 << 24), I64GtU,
        If(BlockType::Empty),
    );
    raise(&mut f, MAX_SIZE);
    wasm!(f;
        End,
        I32Const(1), I32Const(0), Call(helpers.bigint(Big::New)), LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f;
        LocalGet(res), I32Const(1), I32Store(m32(DATA)),
        LocalGet(lhs), LocalSet(base),
        Loop(BlockType::Empty),
        LocalGet(e), I32Const(1), I32And,
        If(BlockType::Empty),
        LocalGet(res), LocalGet(base), Call(helpers.bigint(Big::Mul)), I32WrapI64, LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f;
        End,
        LocalGet(e), I32Const(1), I32ShrU, LocalTee(e),
        If(BlockType::Empty),
        LocalGet(base), LocalGet(base), Call(helpers.bigint(Big::Mul)), I32WrapI64,
        LocalSet(base),
    );
    zero_if_pending(&mut f);
    wasm!(f; Br(1), End, End);
    jsval_of(&mut f, res);
    wasm!(f; End);
    finish_builder(f)
}

/// `__bigint_neg(a: i32) -> i64`: `-a` (a copy with the sign flipped; `0n`
/// is itself).
pub(super) fn emit_neg(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let lhs = 0;
    let (len, res) = (1, 2);
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[RAW_I32, RAW_I32]);
    len_of(&mut f, lhs);
    wasm!(f; LocalTee(len), I32Eqz, If(BlockType::Empty));
    jsval_of(&mut f, lhs);
    wasm!(f; Return, End, LocalGet(len));
    sign(&mut f, lhs);
    wasm!(f; I32Eqz, Call(helpers.bigint(Big::New)), LocalSet(res));
    zero_if_pending(&mut f);
    wasm!(f;
        LocalGet(res), I32Const(DATA_I32), I32Add,
        LocalGet(lhs), I32Const(DATA_I32), I32Add,
        LocalGet(len), I32Const(2), I32Shl,
        MemoryCopy { src_mem: 0, dst_mem: 0 },
    );
    jsval_of(&mut f, res);
    wasm!(f; End);
    finish_builder(f)
}

/// `__bigint_mag_step(p: i32, inc: i32, neg: i32) -> i64`: the `BigInt`
/// with magnitude `|p| + 1` (`inc`) or `|p| - 1` (`|p| > 0`) and sign `neg`:
/// the kernel of `__bigint_add_small` and `__bigint_not`.
pub(super) fn emit_mag_step(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (src, inc, neg) = (0, 1, 2);
    let (len, res, idx, tmp) = (3, 4, 5, 6);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, RAW_I32, RAW_I32],
        &[RAW_I32, RAW_I32, RAW_I32, RAW_I64],
    );
    len_of(&mut f, src);
    wasm!(f;
        LocalTee(len), LocalGet(inc), I32Add, LocalGet(neg), Call(helpers.bigint(Big::New)),
        LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f; I64Const(1), LocalSet(tmp), LocalGet(inc), If(BlockType::Empty));
    for_up(&mut f, idx, len, &|f| {
        load_limb64(f, src, idx);
        wasm!(f; LocalGet(tmp), I64Add, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(tmp));
    });
    store_limb(&mut f, res, len, &|f| wasm!(f; LocalGet(tmp)));
    wasm!(f; Else);
    for_up(&mut f, idx, len, &|f| {
        load_limb64(f, src, idx);
        wasm!(f; LocalGet(tmp), I64Sub, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(0), I64LtS, I64ExtendI32U, LocalSet(tmp));
    });
    wasm!(f; End, LocalGet(res), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// `__bigint_add_small(v: i64, d: i32) -> i64`: `v + d` for the `BigInt` `v`
/// and `d = ±1` (`++` / `--`).
pub(super) fn emit_add_small(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (vn, d) = (0, 1);
    let (src, neg) = (2, 3);
    let mut f = helper_builder(leaves, &[ROOT_JS, RAW_I32], &[RAW_I32, RAW_I32]);
    wasm!(f; LocalGet(vn), I32WrapI64, LocalSet(src));
    sign(&mut f, src);
    wasm!(f; LocalSet(neg));
    len_of(&mut f, src);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(src), I32Const(1), LocalGet(d), I32Const(0), I32LtS,
        Call(helpers.bigint(Big::MagStep)), Return,
        End,
        // The magnitude grows when d has the value's sign.
        LocalGet(src), LocalGet(neg), LocalGet(d), I32Const(0), I32LtS, I32Eq, LocalGet(neg),
        Call(helpers.bigint(Big::MagStep)),
        End,
    );
    finish_builder(f)
}

/// `__bigint_not(a: i32) -> i64`: `~a = -a - 1n`: `-(|a| + 1)` for a
/// non-negative `a`, `|a| - 1` for a negative one.
pub(super) fn emit_not(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let lhs = 0;
    let mut f = helper_builder(leaves, &[ROOT_BASE], &[]);
    wasm!(f; LocalGet(lhs));
    sign(&mut f, lhs);
    wasm!(f; I32Eqz);
    sign(&mut f, lhs);
    wasm!(f; I32Eqz, Call(helpers.bigint(Big::MagStep)), End);
    finish_builder(f)
}

/// The `__bigint_bitop` operation codes.
const BITOP_AND: i32 = 0;
/// See [`BITOP_AND`].
const BITOP_OR: i32 = 1;
/// See [`BITOP_AND`].
const BITOP_XOR: i32 = 2;

/// `__bigint_bitop(a: i32, b: i32, op: i32) -> i64`: `a & b`, `a | b` or
/// `a ^ b` ([`BITOP_AND`] ..) on infinite two's complement. Over `max(len) +
/// 1` limbs, a negative operand's limbs are `~(|x| - 1)` (a running borrow;
/// all ones past its length); the result is negative iff the op of the signs
/// is, and then converted back with `|r| = ~r + 1`.
pub(super) fn emit_bitop(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (lhs, rhs, op) = (0, 1, 2);
    let (la, lb, width, res, idx, na, nb, rn, ta, tb) = (3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
    let (ba, bb, tmp) = (13, 14, 15);
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32,
            RAW_I32, RAW_I64, RAW_I64, RAW_I64,
        ],
    );
    // Push `op(x, y)` of the i32 values in locals x, y.
    let apply = |f: &mut FnBuilder<'_>, big: u32, small: u32| {
        wasm!(f;
            LocalGet(big), LocalGet(small), I32And,
            LocalGet(big), LocalGet(small), I32Or,
            LocalGet(big), LocalGet(small), I32Xor,
            LocalGet(op), I32Const(BITOP_OR), I32Eq, Select,
            LocalGet(op), I32Const(BITOP_AND), I32Eq, Select,
        );
    };
    sign(&mut f, lhs);
    wasm!(f; LocalSet(na));
    sign(&mut f, rhs);
    wasm!(f; LocalSet(nb));
    apply(&mut f, na, nb);
    wasm!(f; LocalSet(rn));
    len_of(&mut f, lhs);
    wasm!(f; LocalTee(la));
    len_of(&mut f, rhs);
    wasm!(f;
        LocalTee(lb), LocalGet(la), LocalGet(lb), I32GtU, Select, I32Const(1), I32Add,
        LocalTee(width), LocalGet(rn), Call(helpers.bigint(Big::New)), LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f;
        LocalGet(na), I64ExtendI32U, LocalSet(ba),
        LocalGet(nb), I64ExtendI32U, LocalSet(bb),
    );
    // Limb i of x in two's complement: lo(x_i - borrow) ^ (neg ? -1 : 0).
    let twos = |f: &mut FnBuilder<'_>, big: u32, lx: u32, nx: u32, bx: u32, tx: u32| {
        load_limb_or0(f, big, idx, lx);
        wasm!(f;
            LocalGet(bx), I64Sub, LocalTee(tmp), I64Const(0), I64LtS, I64ExtendI32U, LocalSet(bx),
            LocalGet(tmp), I32WrapI64, I32Const(0), LocalGet(nx), I32Sub, I32Xor, LocalSet(tx),
        );
    };
    for_up(&mut f, idx, width, &|f| {
        twos(f, lhs, la, na, ba, ta);
        twos(f, rhs, lb, nb, bb, tb);
        limb_addr(f, res, idx);
        apply(f, ta, tb);
        wasm!(f; I32Store(m32(DATA)));
    });
    wasm!(f; LocalGet(rn), If(BlockType::Empty), I64Const(1), LocalSet(tmp));
    for_up(&mut f, idx, width, &|f| {
        load_limb64(f, res, idx);
        wasm!(f; I64Const(MASK), I64Xor, LocalGet(tmp), I64Add, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(tmp));
    });
    wasm!(f; End, LocalGet(res), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// `__bigint_shift(a: i32, n: i32, right: i32) -> i64`: `a << n` (`right`
/// 0) or `a >> n` (`right` 1) of two `BigInt` blocks; a negative count
/// shifts the other way. Left: a count above `2^24` throws the size
/// `RangeError`, else a limb move plus a bit shift. Right (floor): a count
/// `>= bitlen(a)` is `0n` / `-1n`; else `|a| >> n`, plus one for a negative
/// `a` that lost a set bit, negated.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_shift(strings: &StrOffsets, helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (lhs, count, right) = (0, 1, 2);
    let (la, cval, word, bit, res, idx, len, sticky, acc) = (3, 4, 5, 6, 7, 8, 9, 10, 11);
    let tmp = 12;
    let scratch = 13;
    let mut f = helper_builder(
        leaves,
        &[ROOT_BASE, ROOT_BASE, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32,
            RAW_I64,
        ],
    );
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    len_of(&mut f, lhs);
    wasm!(f; LocalTee(la), I32Eqz, If(BlockType::Empty), I64Const(ZERO_BITS), Return, End);
    len_of(&mut f, count);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    jsval_of(&mut f, lhs);
    wasm!(f; Return, End);
    // A count of more than one limb is huge either way.
    len_of(&mut f, count);
    wasm!(f;
        I32Const(1), I32GtU, LocalSet(sticky),
        LocalGet(count), I32Load(m32(DATA)), LocalSet(cval),
        LocalGet(cval), I32Const(5), I32ShrU, LocalSet(word),
        LocalGet(cval), I32Const(31), I32And, LocalSet(bit),
        LocalGet(right),
    );
    sign(&mut f, count);
    wasm!(f; I32Xor, I32Eqz, If(BlockType::Empty));
    // Left.
    wasm!(f;
        LocalGet(sticky), LocalGet(cval), I32Const(1 << 24), I32GtU, I32Or,
        If(BlockType::Empty),
    );
    raise_static(
        &mut f,
        ErrorKind::RangeError,
        MAX_SIZE,
        scratch,
        strings,
        helpers,
    );
    wasm!(f;
        I64Const(ZERO_BITS), Return,
        End,
        LocalGet(la), LocalGet(word), I32Add, I32Const(1), I32Add,
    );
    sign(&mut f, lhs);
    wasm!(f;
        Call(helpers.bigint(Big::New)), LocalSet(res),
    );
    zero_if_pending(&mut f);
    wasm!(f; LocalGet(la), I32Const(1), I32Add, LocalSet(len));
    for_up(&mut f, idx, len, &|f| {
        wasm!(f; LocalGet(idx), LocalGet(word), I32Add, LocalSet(acc));
        store_limb(f, res, acc, &|f| {
            load_limb_or0(f, lhs, idx, la);
            wasm!(f; I64Const(32), I64Shl, LocalGet(idx), If(BlockType::Result(ValType::I64)));
            wasm!(f; LocalGet(idx), I32Const(1), I32Sub, LocalSet(acc));
            load_limb64(f, lhs, acc);
            wasm!(f;
                Else, I64Const(0), End,
                I64Or, LocalGet(bit), I64ExtendI32U, I64Shl, I64Const(32), I64ShrU,
            );
        });
    });
    wasm!(f; LocalGet(res), Call(helpers.bigint(Big::Trim)), Return, End);
    // Right: a count of at least bitlen(a) leaves 0n or -1n.
    wasm!(f;
        LocalGet(sticky),
        LocalGet(cval),
        LocalGet(la), I32Const(5), I32Shl,
        LocalGet(lhs), LocalGet(la), I32Const(2), I32Shl, I32Add, I32Load(m32(DATA - 4)), I32Clz,
        I32Sub, I32GeU, I32Or,
        If(BlockType::Empty),
    );
    sign(&mut f, lhs);
    wasm!(f; If(BlockType::Empty));
    return_unit(&mut f, res, 1, helpers);
    wasm!(f;
        End,
        I64Const(ZERO_BITS), Return,
        End,
        LocalGet(la), LocalGet(word), I32Sub, LocalSet(len),
        // sticky: a negative a loses a set bit (below limb word, or the low
        // bits of limb word).
        I32Const(0), LocalSet(sticky),
    );
    sign(&mut f, lhs);
    wasm!(f; If(BlockType::Empty));
    for_up(&mut f, idx, word, &|f| {
        limb_addr(f, lhs, idx);
        wasm!(f; I32Load(m32(DATA)), LocalGet(sticky), I32Or, LocalSet(sticky));
    });
    limb_addr(&mut f, lhs, word);
    wasm!(f;
        I32Load(m32(DATA)),
        I32Const(1), LocalGet(bit), I32Shl, I32Const(1), I32Sub, I32And,
        LocalGet(sticky), I32Or, I32Const(0), I32Ne, LocalSet(sticky),
        End,
        LocalGet(len), LocalGet(sticky), I32Add,
    );
    sign(&mut f, lhs);
    wasm!(f; Call(helpers.bigint(Big::New)), LocalSet(res));
    zero_if_pending(&mut f);
    for_up(&mut f, idx, len, &|f| {
        wasm!(f; LocalGet(idx), LocalGet(word), I32Add, LocalTee(acc), I32Const(1), I32Add, LocalSet(cval));
        store_limb(f, res, idx, &|f| {
            load_limb_or0(f, lhs, cval, la);
            wasm!(f; I64Const(32), I64Shl);
            load_limb64(f, lhs, acc);
            wasm!(f; I64Or, LocalGet(bit), I64ExtendI32U, I64ShrU);
        });
    });
    wasm!(f;
        LocalGet(sticky),
        If(BlockType::Empty),
        I64Const(1), LocalSet(tmp),
        LocalGet(len), I32Const(1), I32Add, LocalSet(len),
    );
    for_up(&mut f, idx, len, &|f| {
        load_limb64(f, res, idx);
        wasm!(f; LocalGet(tmp), I64Add, LocalSet(tmp));
        store_limb(f, res, idx, &|f| wasm!(f; LocalGet(tmp)));
        wasm!(f; LocalGet(tmp), I64Const(32), I64ShrU, LocalSet(tmp));
    });
    wasm!(f; End, LocalGet(res), Call(helpers.bigint(Big::Trim)), End);
    finish_builder(f)
}

/// The numeric binary operators by `__numeric_op` code.
const NUMOPS: [(i32, BinOp); 12] = [
    (NUMOP_ADD, BinOp::Add),
    (NUMOP_SUB, BinOp::Sub),
    (NUMOP_MUL, BinOp::Mul),
    (NUMOP_DIV, BinOp::Div),
    (NUMOP_MOD, BinOp::Mod),
    (NUMOP_EXP, BinOp::Exp),
    (NUMOP_AND, BinOp::BitAnd),
    (NUMOP_OR, BinOp::BitOr),
    (NUMOP_XOR, BinOp::BitXor),
    (NUMOP_SHL, BinOp::Shl),
    (NUMOP_SHR, BinOp::Shr),
    (NUMOP_USHR, BinOp::UShr),
];

/// `__numeric_op(a: i64, b: i64, op: i32) -> i64`: a numeric binary operator
/// (`NUMOP_*`) once an operand is not a number (DESIGN.md §6.10.5):
/// `ToNumeric(a)`, then `ToNumeric(b)` (each may throw); two numbers give the
/// Number result (the inline code of `emit/numeric.rs`), two `BigInts` the
/// kernel, a mix the `TypeError`. `>>>` on two `BigInts` throws. A throw
/// stays pending (`undefined`).
pub(super) fn emit_numeric_op(
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    leaves: &LeafSet,
) -> Function {
    let (a, b, op) = (0, 1, 2);
    let (pa, pb, s0, s1) = (3, 4, 5, 6);
    let pow = [7, 8, 9, 10, 11];
    let scratch = 12;
    let mut f = helper_builder(
        leaves,
        &[ROOT_JS, ROOT_JS, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64, RAW_I64,
        ],
    );
    declare_error_scratch(&mut f);
    helpers.unwind.borrow_mut().reset();
    for v in [a, b] {
        wasm!(f;
            LocalGet(v), Call(helpers.bigint(Big::ToNumeric)), LocalSet(v),
            GlobalGet(EXC_PENDING_GLOBAL),
            If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        );
    }
    emit_is_number(&mut f, a);
    emit_is_number(&mut f, b);
    wasm!(f; I32And, If(BlockType::Empty), LocalGet(a), LocalSet(s0), LocalGet(b), LocalSet(s1));
    for (code, binop) in NUMOPS {
        wasm!(f; LocalGet(op), I32Const(code), I32Eq, If(BlockType::Empty));
        if binop == BinOp::Add {
            wasm!(f;
                LocalGet(s0), F64ReinterpretI64, LocalGet(s1), F64ReinterpretI64, F64Add,
                Call(helpers.box_f64),
            );
        } else {
            emit_number_op(binop, &mut f, s0, s1, pow, helpers);
        }
        wasm!(f; Return, End);
    }
    wasm!(f; End);
    emit_is_bigint(&mut f, a);
    emit_is_bigint(&mut f, b);
    wasm!(f;
        I32And,
        If(BlockType::Empty),
        LocalGet(a), I32WrapI64, LocalSet(pa),
        LocalGet(b), I32WrapI64, LocalSet(pb),
    );
    let kernels: [(i32, Big, Option<i32>); 11] = [
        (NUMOP_ADD, Big::Add, None),
        (NUMOP_SUB, Big::Sub, None),
        (NUMOP_MUL, Big::Mul, None),
        (NUMOP_DIV, Big::DivMod, Some(0)),
        (NUMOP_MOD, Big::DivMod, Some(1)),
        (NUMOP_EXP, Big::Pow, None),
        (NUMOP_AND, Big::BitOp, Some(BITOP_AND)),
        (NUMOP_OR, Big::BitOp, Some(BITOP_OR)),
        (NUMOP_XOR, Big::BitOp, Some(BITOP_XOR)),
        (NUMOP_SHL, Big::Shift, Some(0)),
        (NUMOP_SHR, Big::Shift, Some(1)),
    ];
    for (code, kernel, extra) in kernels {
        wasm!(f; LocalGet(op), I32Const(code), I32Eq, If(BlockType::Empty), LocalGet(pa), LocalGet(pb));
        if let Some(extra) = extra {
            wasm!(f; I32Const(extra));
        }
        wasm!(f; Call(helpers.bigint(kernel)), Return, End);
    }
    emit_node(&callee_type_error(USHR), &mut f, scratch, strings, helpers);
    emit_set_exc_value(&mut f, helpers);
    wasm!(f;
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
        I64Const(UNDEFINED_BITS), Return,
        End,
    );
    emit_node(&callee_type_error(MIX), &mut f, scratch, strings, helpers);
    emit_set_exc_value(&mut f, helpers);
    wasm!(f;
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
        I64Const(UNDEFINED_BITS),
        End,
    );
    finish_builder(f)
}

/// `__numeric_unary(v: i64, op: i32) -> i64`: unary `-` ([`NUMOP_NEG`]) or
/// `~` ([`NUMOP_NOT`]) once the operand is not a number: `ToNumeric(v)`
/// (may throw), then `-x` / `~ToInt32(x)` for a number, `__bigint_neg` /
/// `__bigint_not` for a `BigInt`. A throw stays pending (`undefined`).
pub(super) fn emit_numeric_unary(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (v, op) = (0, 1);
    let s0 = 2;
    let mut f = helper_builder(leaves, &[ROOT_JS, RAW_I32], &[RAW_I64]);
    wasm!(f;
        LocalGet(v), Call(helpers.bigint(Big::ToNumeric)), LocalSet(v),
        GlobalGet(EXC_PENDING_GLOBAL),
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
    emit_is_bigint(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(op), I32Const(NUMOP_NEG), I32Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(v), I32WrapI64, Call(helpers.bigint(Big::Neg)),
        Else,
        LocalGet(v), I32WrapI64, Call(helpers.bigint(Big::Not)),
        End,
        Return,
        End,
        LocalGet(v), LocalSet(s0),
        LocalGet(op), I32Const(NUMOP_NEG), I32Eq,
        If(BlockType::Empty),
    );
    emit_number_unary(true, &mut f, s0, helpers);
    wasm!(f; Return, End);
    emit_number_unary(false, &mut f, s0, helpers);
    wasm!(f; End);
    finish_builder(f)
}

/// The call graph of this family's helpers, for the leaf solver.
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    let big = |h| helpers.bigint(h);
    vec![
        // The shared equality and hashing helpers call the leaf kernels.
        (helpers.same_value_zero, vec![big(Big::Eq)]),
        (helpers.map_hash(), vec![big(Big::Hash)]),
        (big(Big::Trim), vec![]),
        (big(Big::Eq), vec![]),
        (big(Big::CmpMag), vec![]),
        (big(Big::Cmp), vec![big(Big::CmpMag)]),
        (big(Big::CmpNum), vec![]),
        (big(Big::Hash), vec![]),
        (big(Big::ToF64), vec![]),
        (big(Big::ToI64), vec![]),
    ]
}

// --- B3: the `BigInt` builtins' kernels and the Number / 64-bit boundaries ---

/// `__number_value(v: i64) -> i64`: the `Number(v)` conversion
/// (DESIGN.md §6.10.7): `ToNumeric(v)`, then a `BigInt` becomes its Number
/// value (`__bigint_to_f64`, exact round-half-even) and a number is returned.
/// A throw stays pending (`NaN`).
pub(super) fn emit_number_value(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let v = 0;
    let mut f = helper_builder(leaves, &[ROOT_JS], &[]);
    wasm!(f;
        LocalGet(v), Call(helpers.bigint(Big::ToNumeric)), LocalSet(v),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(QNAN_BITS), Return, End,
    );
    emit_is_bigint(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, Call(helpers.bigint(Big::ToF64)), I64ReinterpretF64, Return,
        End,
        LocalGet(v),
        End,
    );
    finish_builder(f)
}

/// The locals of `__bigint_as_n`: the parameters `bits` (f64), `v` (the
/// `BigInt` jsval) and `signed`, then the source block, its `len`, the result
/// width `w` and block, the limb index, the source sign, the top limb's bit
/// count, the current limb, the carry and `ceil(bits / 32)`.
mod as_n {
    pub(super) const BITS: u32 = 0;
    pub(super) const V: u32 = 1;
    pub(super) const SIGNED: u32 = 2;
    pub(super) const SRC: u32 = 3;
    pub(super) const LEN: u32 = 4;
    pub(super) const WIDTH: u32 = 5;
    pub(super) const DST: u32 = 6;
    pub(super) const IDX: u32 = 7;
    pub(super) const NEG: u32 = 8;
    pub(super) const TOP_BITS: u32 = 9;
    pub(super) const LIMB: u32 = 10;
    pub(super) const CARRY: u32 = 11;
    pub(super) const WIDTH_F: u32 = 12;
}

/// `limb = (limb ^ 0xFFFF_FFFF) + carry`, `carry = limb >> 32`, `limb &=
/// 0xFFFF_FFFF`: one step of a two's-complement negation (`__bigint_as_n`).
fn as_n_complement(f: &mut FnBuilder<'_>) {
    use as_n::{CARRY, LIMB};
    wasm!(f;
        LocalGet(LIMB), I64Const(0xFFFF_FFFF), I64Xor, LocalGet(CARRY), I64Add, LocalTee(LIMB),
        I64Const(32), I64ShrU, LocalSet(CARRY),
        LocalGet(LIMB), I64Const(0xFFFF_FFFF), I64And, LocalSet(LIMB),
    );
}

/// Store the low 32 bits of `limb` as limb `idx` of the result block.
fn as_n_store(f: &mut FnBuilder<'_>) {
    use as_n::{DST, IDX, LIMB};
    limb_addr(f, DST, IDX);
    wasm!(f; LocalGet(LIMB), I64Store32(m32(DATA)));
}

/// Run `body` for `idx` in `0..width` (`__bigint_as_n`).
fn as_n_each_limb(f: &mut FnBuilder<'_>, body: impl Fn(&mut FnBuilder<'_>)) {
    use as_n::{IDX, WIDTH};
    wasm!(f;
        I32Const(0), LocalSet(IDX),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(IDX), LocalGet(WIDTH), I32GeU, BrIf(1),
    );
    body(f);
    wasm!(f;
        LocalGet(IDX), I32Const(1), I32Add, LocalSet(IDX),
        Br(0),
        End, End,
    );
}

/// Mask the result's top limb (`width - 1`) to its low `top_bits` bits,
/// leaving it in `limb`.
fn as_n_mask_top(f: &mut FnBuilder<'_>) {
    use as_n::{DST, IDX, LIMB, TOP_BITS, WIDTH};
    wasm!(f; LocalGet(WIDTH), I32Const(1), I32Sub, LocalSet(IDX));
    load_limb64(f, DST, IDX);
    wasm!(f;
        I64Const(1), LocalGet(TOP_BITS), I64ExtendI32U, I64Shl, I64Const(1), I64Sub,
        I64And, LocalSet(LIMB),
    );
    as_n_store(f);
}

/// `__bigint_as_n(bits: f64, v: i64, signed: i32) -> i64`: `BigInt.asUintN`
/// (`signed = 0`) / `BigInt.asIntN` of the `BigInt` `v` and the integral
/// `bits` (`0 ..= 2^53 - 1`, already `ToIndex`ed): the low `bits` bits of
/// `v`'s infinite two's complement, read unsigned, minus `2^bits` for
/// `asIntN` when bit `bits - 1` is set. The result spans `w = ceil(bits /
/// 32)` limbs; when `w` exceeds `len + 1` every result but `asUintN` of a
/// negative value is `v` itself, and that one is size-checked by
/// `__bigint_new` (`RangeError` past the limit).
pub(super) fn emit_as_n(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    use as_n::{
        BITS, CARRY, DST, IDX, LEN as SRC_LEN, LIMB, NEG as SRC_NEG, SIGNED, SRC, TOP_BITS, V,
        WIDTH, WIDTH_F,
    };
    let mut f = helper_builder(
        leaves,
        &[RAW_F64, ROOT_JS, RAW_I32],
        &[
            RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I64, RAW_I64,
            RAW_F64,
        ],
    );
    wasm!(f;
        LocalGet(BITS), F64Const(0.0.into()), F64Eq,
        If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(V), I32WrapI64, LocalTee(SRC), I32Load(m32(LEN)), LocalTee(SRC_LEN),
        I32Eqz, If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(SRC), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And, I32Const(0), I32Ne,
        LocalSet(SRC_NEG),
        LocalGet(BITS), F64Const(32.0.into()), F64Div, F64Ceil, LocalTee(WIDTH_F),
        LocalGet(SRC_LEN), I32Const(1), I32Add, F64ConvertI32U, F64Gt,
        If(BlockType::Empty),
        // Wider than `v`: only `asUintN` of a negative value changes it.
        LocalGet(SRC_NEG), LocalGet(SIGNED), I32Eqz, I32And, I32Eqz,
        If(BlockType::Empty), LocalGet(V), Return, End,
        I32Const(abi::BIGINT_MAX_LIMBS.cast_signed() + 1),
        LocalGet(WIDTH_F), I32TruncSatF64U,
        LocalGet(WIDTH_F), F64Const(f64::from(abi::BIGINT_MAX_LIMBS).into()), F64Gt,
        Select, LocalSet(WIDTH),
        Else,
        LocalGet(WIDTH_F), I32TruncSatF64U, LocalSet(WIDTH),
        End,
        LocalGet(WIDTH), I32Const(0), Call(helpers.bigint(Big::New)), LocalSet(DST),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(V), I32WrapI64, LocalSet(SRC),
        // The bits of the top limb, `1 ..= 32`.
        LocalGet(BITS), LocalGet(WIDTH), I32Const(1), I32Sub, F64ConvertI32U,
        F64Const(32.0.into()), F64Mul, F64Sub, I32TruncSatF64U, LocalSet(TOP_BITS),
        I64Const(1), LocalSet(CARRY),
    );
    // The result = `v mod 2^(32w)`: the magnitude, two's-complemented when
    // negative.
    as_n_each_limb(&mut f, |f| {
        wasm!(f;
            LocalGet(IDX), LocalGet(SRC_LEN), I32LtU,
            If(BlockType::Result(ValType::I64)),
        );
        load_limb64(f, SRC, IDX);
        wasm!(f;
            Else, I64Const(0), End, LocalSet(LIMB),
            LocalGet(SRC_NEG), If(BlockType::Empty),
        );
        as_n_complement(f);
        wasm!(f; End);
        as_n_store(f);
    });
    as_n_mask_top(&mut f);
    // `asIntN` with bit `bits - 1` set: the value is `-(2^bits - result)`.
    wasm!(f;
        LocalGet(SIGNED),
        LocalGet(LIMB), LocalGet(TOP_BITS), I32Const(1), I32Sub, I64ExtendI32U, I64ShrU,
        I32WrapI64, I32Const(1), I32And, I32And,
        If(BlockType::Empty),
        I64Const(1), LocalSet(CARRY),
    );
    as_n_each_limb(&mut f, |f| {
        load_limb64(f, DST, IDX);
        wasm!(f; LocalSet(LIMB));
        as_n_complement(f);
        as_n_store(f);
    });
    as_n_mask_top(&mut f);
    wasm!(f;
        LocalGet(DST), I32Const(NEG), I32Store8(m8(FLAGS)),
        End,
        LocalGet(DST), Call(helpers.bigint(Big::Trim)),
        End,
    );
    finish_builder(f)
}

/// `__bigint_to_i64(p: i32) -> i64`: the low 64 bits of the two's-complement
/// value of the `BigInt` block `p` (`ToBigInt64` / `ToBigUint64` read as
/// `i64` / `u64`). Leaf.
pub(super) fn emit_to_i64(leaves: &LeafSet) -> Function {
    let (p, len, x) = (0, 1, 2);
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32, RAW_I64]);
    wasm!(f;
        LocalGet(p), I32Load(m32(LEN)), LocalTee(len),
        If(BlockType::Empty),
        LocalGet(p), I64Load32U(m32(DATA)), LocalSet(x),
        End,
        LocalGet(len), I32Const(1), I32GtU,
        If(BlockType::Empty),
        LocalGet(x), LocalGet(p), I64Load32U(m32(DATA + 4)), I64Const(32), I64Shl, I64Or,
        LocalSet(x),
        End,
        I64Const(0), LocalGet(x), I64Sub, LocalGet(x),
        LocalGet(p), I32Load8U(m8(FLAGS)), I32Const(NEG), I32And,
        Select,
        End,
    );
    finish_builder(f)
}

/// `__bigint_from_i64(v: i64, signed: i32) -> i64`: the `BigInt` of the 64
/// bits `v`, read as `i64` when `signed`, else as `u64`.
pub(super) fn emit_from_i64(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let (v, signed, neg, p) = (0, 1, 2, 3);
    let mut f = helper_builder(leaves, &[RAW_I64, RAW_I32], &[RAW_I32, RAW_I32]);
    wasm!(f;
        LocalGet(v), I64Eqz, If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(signed), LocalGet(v), I64Const(0), I64LtS, I32And, LocalTee(neg),
        If(BlockType::Empty), I64Const(0), LocalGet(v), I64Sub, LocalSet(v), End,
        I32Const(2), LocalGet(neg), Call(helpers.bigint(Big::New)), LocalSet(p),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I64Const(ZERO_BITS), Return, End,
        LocalGet(p), LocalGet(v), I64Store(MemArg { offset: DATA, align: 2, memory_index: 0 }),
        LocalGet(p), Call(helpers.bigint(Big::Trim)),
        End,
    );
    finish_builder(f)
}
