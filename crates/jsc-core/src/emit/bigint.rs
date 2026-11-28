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
    ]
}
