//! The `Math` transcendentals as guest helpers: a hand port of the `libm`
//! crate's musl / FreeBSD msun algorithms to pure wasm, emitted once per
//! module, so results do not depend on the host.
//!
//! The port stays line-for-line comparable with its source through a small
//! typed expression layer: [`F`], [`I`] and [`L`] hold the instructions of an
//! `f64`, `i32` and `i64` expression, the `V*` types name locals, and the
//! operators build the same evaluation order as the Rust source. `cbrt` is
//! the crate's CORE-MATH routine; wasm has no `fma`, so its exact product
//! remainders use Dekker's product ([`mul_err`]).
//!
//! `rem_pio2_large` indexes arrays by computed subscripts, so it keeps its
//! tables and working arrays in a reserved static block (see [`STATIC_SIZE`]).

use std::ops::{Add, BitAnd, BitOr, BitXor, Div, Mul, Neg, Shl, Sub};

use wasm_encoder::{
    BlockType, ConstExpr, DataSection, Function, FunctionSection, Ieee64, Instruction, MemArg,
    TypeSection, ValType,
};

use super::{Helpers, MathOp};
use crate::abi;

type W = Instruction<'static>;

/// An `f64` expression.
#[derive(Clone)]
struct F(Vec<W>);
/// An `i32` expression.
#[derive(Clone)]
struct I(Vec<W>);
/// An `i64` expression.
#[derive(Clone)]
struct L(Vec<W>);
/// An `f64` local.
#[derive(Clone, Copy)]
struct VF(u32);
/// An `i32` local.
#[derive(Clone, Copy)]
struct VI(u32);
/// An `i64` local.
#[derive(Clone, Copy)]
struct VL(u32);

impl From<f64> for F {
    fn from(v: f64) -> Self {
        Self(vec![W::F64Const(Ieee64::from(v))])
    }
}
impl From<VF> for F {
    fn from(v: VF) -> Self {
        Self(vec![W::LocalGet(v.0)])
    }
}
impl From<i32> for I {
    fn from(v: i32) -> Self {
        Self(vec![W::I32Const(v)])
    }
}
impl From<VI> for I {
    fn from(v: VI) -> Self {
        Self(vec![W::LocalGet(v.0)])
    }
}
impl From<i64> for L {
    fn from(v: i64) -> Self {
        Self(vec![W::I64Const(v)])
    }
}
impl From<VL> for L {
    fn from(v: VL) -> Self {
        Self(vec![W::LocalGet(v.0)])
    }
}

/// The instructions of `a`, then `b`, then `op`.
fn bin(a: Vec<W>, b: Vec<W>, op: W) -> Vec<W> {
    let mut code = a;
    code.extend(b);
    code.push(op);
    code
}

/// The instructions of `a`, then `op`.
fn un(a: Vec<W>, op: W) -> Vec<W> {
    let mut code = a;
    code.push(op);
    code
}

macro_rules! binops {
    ($e:ident, $v:ident: $($tr:ident $m:ident $op:ident),*) => {$(
        impl<R: Into<$e>> $tr<R> for $e {
            type Output = $e;
            fn $m(self, r: R) -> $e {
                $e(bin(self.0, r.into().0, W::$op))
            }
        }
        impl<R: Into<$e>> $tr<R> for $v {
            type Output = $e;
            fn $m(self, r: R) -> $e {
                $e(bin($e::from(self).0, r.into().0, W::$op))
            }
        }
    )*};
}
binops!(F, VF: Add add F64Add, Sub sub F64Sub, Mul mul F64Mul, Div div F64Div);
binops!(I, VI: Add add I32Add, Sub sub I32Sub, Mul mul I32Mul, BitAnd bitand I32And,
    BitOr bitor I32Or, Shl shl I32Shl);
binops!(L, VL: Add add I64Add, BitAnd bitand I64And, BitOr bitor I64Or, BitXor bitxor I64Xor,
    Shl shl I64Shl);

macro_rules! left_float_ops {
    ($($tr:ident $m:ident $op:ident),*) => {$(
        impl $tr<F> for f64 {
            type Output = F;
            fn $m(self, r: F) -> F {
                F(bin(F::from(self).0, r.0, W::$op))
            }
        }
        impl $tr<VF> for f64 {
            type Output = F;
            fn $m(self, r: VF) -> F {
                F(bin(F::from(self).0, F::from(r).0, W::$op))
            }
        }
    )*};
}
left_float_ops!(Add add F64Add, Sub sub F64Sub, Mul mul F64Mul, Div div F64Div);

impl Neg for F {
    type Output = F;
    fn neg(self) -> F {
        F(un(self.0, W::F64Neg))
    }
}
impl Neg for VF {
    type Output = F;
    fn neg(self) -> F {
        -F::from(self)
    }
}

/// Comparisons and conversions of an `f64` expression.
trait Fe: Into<F> + Sized {
    fn cmp(self, r: impl Into<F>, op: W) -> I {
        I(bin(self.into().0, r.into().0, op))
    }
    fn lt(self, r: impl Into<F>) -> I {
        self.cmp(r, W::F64Lt)
    }
    fn gt(self, r: impl Into<F>) -> I {
        self.cmp(r, W::F64Gt)
    }
    fn ge(self, r: impl Into<F>) -> I {
        self.cmp(r, W::F64Ge)
    }
    fn eq(self, r: impl Into<F>) -> I {
        self.cmp(r, W::F64Eq)
    }
    fn ne(self, r: impl Into<F>) -> I {
        self.cmp(r, W::F64Ne)
    }
    fn fabs(self) -> F {
        F(un(self.into().0, W::F64Abs))
    }
    fn fsqrt(self) -> F {
        F(un(self.into().0, W::F64Sqrt))
    }
    fn ffloor(self) -> F {
        F(un(self.into().0, W::F64Floor))
    }
    fn sign_of(self, r: impl Into<F>) -> F {
        F(bin(self.into().0, r.into().0, W::F64Copysign))
    }
    /// `x as i32` (saturating).
    fn trunc_i(self) -> I {
        I(un(self.into().0, W::I32TruncSatF64S))
    }
    /// `x.to_bits()`.
    fn bits(self) -> L {
        L(un(self.into().0, W::I64ReinterpretF64))
    }
    /// The high word of `x`.
    fn hi(self) -> I {
        self.bits().shr_u(32).wrap()
    }
    /// The low word of `x`.
    fn lo(self) -> I {
        self.bits().wrap()
    }
    /// `x` with a zero low word.
    fn zero_lo(self) -> F {
        (self.bits() & (-1_i64 << 32)).reinterpret()
    }
}
impl Fe for F {}
impl Fe for VF {}

/// Comparisons and conversions of an `i32` expression.
trait Ie: Into<I> + Sized {
    fn cmp(self, r: impl Into<I>, op: W) -> I {
        I(bin(self.into().0, r.into().0, op))
    }
    fn eq(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32Eq)
    }
    fn ne(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32Ne)
    }
    fn lt_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32LtS)
    }
    fn le_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32LeS)
    }
    fn gt_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32GtS)
    }
    fn ge_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32GeS)
    }
    fn lt_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32LtU)
    }
    fn le_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32LeU)
    }
    fn gt_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32GtU)
    }
    fn ge_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32GeU)
    }
    fn eqz(self) -> I {
        I(un(self.into().0, W::I32Eqz))
    }
    fn shr_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32ShrU)
    }
    fn shr_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32ShrS)
    }
    fn div_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32DivU)
    }
    fn div_s(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32DivS)
    }
    fn rem_u(self, r: impl Into<I>) -> I {
        self.cmp(r, W::I32RemU)
    }
    /// `n as f64`.
    fn i2f(self) -> F {
        F(un(self.into().0, W::F64ConvertI32S))
    }
    /// `n as u32 as u64`.
    fn zext(self) -> L {
        L(un(self.into().0, W::I64ExtendI32U))
    }
    /// `n as i64`.
    fn sext(self) -> L {
        L(un(self.into().0, W::I64ExtendI32S))
    }
}
impl Ie for I {}
impl Ie for VI {}

/// Comparisons and conversions of an `i64` expression.
trait Le: Into<L> + Sized {
    fn cmp(self, r: impl Into<L>, op: W) -> I {
        I(bin(self.into().0, r.into().0, op))
    }
    fn le_u(self, r: impl Into<L>) -> I {
        self.cmp(r, W::I64LeU)
    }
    fn eqz(self) -> I {
        I(un(self.into().0, W::I64Eqz))
    }
    fn shr_u(self, r: impl Into<L>) -> L {
        L(bin(self.into().0, r.into().0, W::I64ShrU))
    }
    fn wrap(self) -> I {
        I(un(self.into().0, W::I32WrapI64))
    }
    /// `f64::from_bits(u)`.
    fn reinterpret(self) -> F {
        F(un(self.into().0, W::F64ReinterpretI64))
    }
    fn clz(self) -> L {
        L(un(self.into().0, W::I64Clz))
    }
}
impl Le for L {}
impl Le for VL {}

/// `if c { t } else { e }` without control flow (both arms are evaluated).
fn select(c: impl Into<I>, t: impl Into<F>, e: impl Into<F>) -> F {
    let mut code = t.into().0;
    code.extend(e.into().0);
    code.extend(c.into().0);
    code.push(W::Select);
    F(code)
}

/// `f(args..)` of the sibling helper `h`, relocated by [`B::finish`].
fn call(h: H, args: Vec<Vec<W>>) -> Vec<W> {
    let mut code: Vec<W> = args.into_iter().flatten().collect();
    code.push(W::Call(h as u32));
    code
}

fn call1(h: H, a: impl Into<F>) -> F {
    F(call(h, vec![a.into().0]))
}

fn call2(h: H, a: impl Into<F>, c: impl Into<F>) -> F {
    F(call(h, vec![a.into().0, c.into().0]))
}

fn scalbn(a: impl Into<F>, n: impl Into<I>) -> F {
    F(call(H::Scalbn, vec![a.into().0, n.into().0]))
}

fn mul_err_of(a: impl Into<F>, c: impl Into<F>, p: VF) -> F {
    F(call(H::MulErr, vec![a.into().0, c.into().0, F::from(p).0]))
}

fn k_sin(a: VF, c: VF, yflag: i32) -> F {
    F(call(
        H::KSin,
        vec![F::from(a).0, F::from(c).0, I::from(yflag).0],
    ))
}

fn k_cos(a: VF, c: VF) -> F {
    call2(H::KCos, a, c)
}

/// An `i32` load of element `idx` of the 4-byte table at `base`.
fn iload(base: i32, idx: impl Into<I>) -> I {
    let mut code = ((idx.into() << 2) + base).0;
    code.push(W::I32Load(mem(2)));
    I(code)
}

/// An `f64` load of element `idx` of the 8-byte table at `base`.
fn fload(base: i32, idx: impl Into<I>) -> F {
    let mut code = ((idx.into() << 3) + base).0;
    code.push(W::F64Load(mem(3)));
    F(code)
}

fn mem(align: u32) -> MemArg {
    MemArg {
        offset: 0,
        align,
        memory_index: 0,
    }
}

/// One helper body under construction.
struct B {
    params: u32,
    locals: Vec<ValType>,
    code: Vec<W>,
}

impl B {
    fn new(params: u32) -> Self {
        Self {
            params,
            locals: Vec::new(),
            code: Vec::new(),
        }
    }

    fn local(&mut self, ty: ValType) -> u32 {
        self.locals.push(ty);
        self.params + u32::try_from(self.locals.len()).unwrap_or(0) - 1
    }

    fn emit(&mut self, code: Vec<W>) {
        self.code.extend(code);
    }

    /// `let v = e` for a fresh `f64` local.
    fn f(&mut self, e: impl Into<F>) -> VF {
        let v = VF(self.local(ValType::F64));
        self.sf(v, e);
        v
    }

    fn i(&mut self, e: impl Into<I>) -> VI {
        let v = VI(self.local(ValType::I32));
        self.si(v, e);
        v
    }

    fn l(&mut self, e: impl Into<L>) -> VL {
        let v = VL(self.local(ValType::I64));
        self.sl(v, e);
        v
    }

    fn sf(&mut self, v: VF, e: impl Into<F>) {
        self.emit(e.into().0);
        self.code.push(W::LocalSet(v.0));
    }

    fn si(&mut self, v: VI, e: impl Into<I>) {
        self.emit(e.into().0);
        self.code.push(W::LocalSet(v.0));
    }

    fn sl(&mut self, v: VL, e: impl Into<L>) {
        self.emit(e.into().0);
        self.code.push(W::LocalSet(v.0));
    }

    fn ret(&mut self, e: impl Into<F>) {
        self.emit(e.into().0);
        self.code.push(W::Return);
    }

    /// `return (n, y0, y1)`.
    fn ret3(&mut self, n: impl Into<I>, y0: impl Into<F>, y1: impl Into<F>) {
        self.emit(n.into().0);
        self.emit(y0.into().0);
        self.emit(y1.into().0);
        self.code.push(W::Return);
    }

    fn fsto(&mut self, base: i32, idx: impl Into<I>, e: impl Into<F>) {
        self.emit(((idx.into() << 3) + base).0);
        self.emit(e.into().0);
        self.code.push(W::F64Store(mem(3)));
    }

    fn isto(&mut self, base: i32, idx: impl Into<I>, e: impl Into<I>) {
        self.emit(((idx.into() << 2) + base).0);
        self.emit(e.into().0);
        self.code.push(W::I32Store(mem(2)));
    }

    fn if_(&mut self, c: impl Into<I>, then: impl FnOnce(&mut Self)) {
        self.emit(c.into().0);
        self.code.push(W::If(BlockType::Empty));
        then(self);
        self.code.push(W::End);
    }

    fn if_else(
        &mut self,
        c: impl Into<I>,
        then: impl FnOnce(&mut Self),
        otherwise: impl FnOnce(&mut Self),
    ) {
        self.emit(c.into().0);
        self.code.push(W::If(BlockType::Empty));
        then(self);
        self.code.push(W::Else);
        otherwise(self);
        self.code.push(W::End);
    }

    /// `while c { body }`.
    fn while_(&mut self, c: impl Into<I>, body: impl FnOnce(&mut Self)) {
        self.code.push(W::Block(BlockType::Empty));
        self.code.push(W::Loop(BlockType::Empty));
        self.emit(c.into().eqz().0);
        self.code.push(W::BrIf(1));
        body(self);
        self.code.push(W::Br(0));
        self.code.push(W::End);
        self.code.push(W::End);
    }

    /// `loop { body; if !c { break } }`.
    fn do_while(&mut self, body: impl FnOnce(&mut Self), c: impl Into<I>) {
        self.code.push(W::Loop(BlockType::Empty));
        body(self);
        self.emit(c.into().0);
        self.code.push(W::BrIf(0));
        self.code.push(W::End);
    }

    /// `for i in from..=to { body }`.
    fn up(&mut self, i: VI, from: impl Into<I>, to: impl Into<I>, body: impl FnOnce(&mut Self)) {
        self.si(i, from);
        self.while_(i.le_s(to), |b| {
            body(b);
            b.si(i, i + 1);
        });
    }

    /// `for i in (to..=from).rev() { body }`.
    fn down(&mut self, i: VI, from: impl Into<I>, to: impl Into<I>, body: impl FnOnce(&mut Self)) {
        self.si(i, from);
        self.while_(i.ge_s(to), |b| {
            body(b);
            b.si(i, i - 1);
        });
    }

    /// The finished body, with sibling calls relocated by `base`. Every path
    /// returns explicitly, so the end of the body is unreachable.
    fn finish(self, base: u32) -> Function {
        let mut function = Function::new_with_locals_types(self.locals);
        for ins in self.code {
            match ins {
                W::Call(h) => function.instruction(&W::Call(base + h)),
                other => function.instruction(&other),
            };
        }
        function.instruction(&W::Unreachable);
        function.instruction(&W::End);
        function
    }
}

/// The helpers, in function-index order after `_start`.
#[derive(Clone, Copy)]
pub(super) enum H {
    Scalbn,
    MulErr,
    AsinR,
    Exp,
    Expm1,
    Log,
    Log1p,
    Log2,
    Log10,
    Cbrt,
    KSin,
    KCos,
    KTan,
    RemMedium,
    RemLarge,
    RemPio2,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Expo2,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
}

use ValType::{F64, I32};

/// The `(params, results)` of every helper, in [`H`] order.
const TYPES: [(&[ValType], &[ValType]); 30] = [
    (&[F64, I32], &[F64]),           // __libm_scalbn(x, n)
    (&[F64, F64, F64], &[F64]),      // __libm_mul_err(a, b, p)
    (&[F64], &[F64]),                // __libm_asin_r(z)
    (&[F64], &[F64]),                // exp
    (&[F64], &[F64]),                // expm1
    (&[F64], &[F64]),                // log
    (&[F64], &[F64]),                // log1p
    (&[F64], &[F64]),                // log2
    (&[F64], &[F64]),                // log10
    (&[F64], &[F64]),                // cbrt
    (&[F64, F64, I32], &[F64]),      // __libm_k_sin(x, y, yflag)
    (&[F64, F64], &[F64]),           // __libm_k_cos(x, y)
    (&[F64, F64, I32], &[F64]),      // __libm_k_tan(x, y, odd)
    (&[F64, I32], &[I32, F64, F64]), // __libm_rem_medium(x, ix) -> (n, y0, y1)
    (&[I32, I32], &[I32, F64, F64]), // __libm_rem_large(nx, e0) -> (n, y0, y1)
    (&[F64], &[I32, F64, F64]),      // __libm_rem_pio2(x) -> (n, y0, y1)
    (&[F64], &[F64]),                // sin
    (&[F64], &[F64]),                // cos
    (&[F64], &[F64]),                // tan
    (&[F64], &[F64]),                // asin
    (&[F64], &[F64]),                // acos
    (&[F64], &[F64]),                // atan
    (&[F64, F64], &[F64]),           // atan2(y, x)
    (&[F64], &[F64]),                // __libm_expo2(x)
    (&[F64], &[F64]),                // sinh
    (&[F64], &[F64]),                // cosh
    (&[F64], &[F64]),                // tanh
    (&[F64], &[F64]),                // asinh
    (&[F64], &[F64]),                // acosh
    (&[F64], &[F64]),                // atanh
];

// The reserved static block, at `abi::STATIC_AREA_END`.
const TABLES_BASE: i32 = abi::STATIC_AREA_END.cast_signed();
const IPIO2_ADDR: i32 = TABLES_BASE;
const PIO2_ADDR: i32 = IPIO2_ADDR + 4 * 66;
const ATANHI_ADDR: i32 = PIO2_ADDR + 8 * 5;
const ATANLO_ADDR: i32 = ATANHI_ADDR + 8 * 4;
/// The working arrays of `rem_pio2_large` (zero-initialized), after the
/// tables: the argument chunks and the `f`, `fq`, `q` and `iq` arrays.
const X_ADDR: i32 = ATANLO_ADDR + 8 * 4;
const F_ADDR: i32 = X_ADDR + 8 * 3;
const FQ_ADDR: i32 = F_ADDR + 8 * 20;
const Q_ADDR: i32 = FQ_ADDR + 8 * 20;
const IQ_ADDR: i32 = Q_ADDR + 8 * 20;
const BLOCK_END: i32 = IQ_ADDR + 4 * 20;

/// The size in bytes of the block reserved after the fixed static area: the
/// tables, then the working arrays of `rem_pio2_large` (a leaf that holds no
/// state across calls). Keeps the string blocks after it 8-aligned.
pub(super) const STATIC_SIZE: i32 = BLOCK_END - TABLES_BASE;

/// `2/pi` in 24-bit chunks; 66 entries cover every `f64` exponent.
const IPIO2: [i32; 66] = [
    0xA2_F983, 0x6E_4E44, 0x15_29FC, 0x27_57D1, 0xF5_34DD, 0xC0_DB62, 0x95_993C, 0x43_9041,
    0xFE_5163, 0xAB_DEBB, 0xC5_61B7, 0x24_6E3A, 0x42_4DD2, 0xE0_0649, 0x2E_EA09, 0xD1_921C,
    0xFE_1DEB, 0x1C_B129, 0xA7_3EE8, 0x82_35F5, 0x2E_BB44, 0x84_E99C, 0x70_26B4, 0x5F_7E41,
    0x39_91D6, 0x39_8353, 0x39_F49C, 0x84_5F8B, 0xBD_F928, 0x3B_1FF8, 0x97_FFDE, 0x05_980F,
    0xEF_2F11, 0x8B_5A0A, 0x6D_1F6D, 0x36_7ECF, 0x27_CB09, 0xB7_4F46, 0x3F_669E, 0x5F_EA2D,
    0x75_27BA, 0xC7_EBE5, 0xF1_7B3D, 0x07_39F7, 0x8A_5292, 0xEA_6BFB, 0x5F_B11F, 0x8D_5D08,
    0x56_0330, 0x46_FC7B, 0x6B_ABF0, 0xCF_BC20, 0x9A_F436, 0x1D_A9E3, 0x91_615E, 0xE6_1B08,
    0x65_9985, 0x5F_14A0, 0x68_408D, 0xFF_D880, 0x4D_7327, 0x31_0606, 0x15_56CA, 0x73_A8C9,
    0x60_E27B, 0xC0_8C6B,
];

/// `pi/2` in 24-bit pieces, as far as `rem_pio2_large` reads it at `prec = 1`.
const PIO2: [u64; 5] = [
    0x3FF9_21FB_4000_0000,
    0x3E74_442D_0000_0000,
    0x3CF8_4698_8000_0000,
    0x3B78_CC51_6000_0000,
    0x39F0_1B83_8000_0000,
];

const ATANHI: [u64; 4] = [
    0x3fdd_ac67_0561_bb4f,
    0x3fe9_21fb_5444_2d18,
    0x3fef_730b_d281_f69b,
    0x3ff9_21fb_5444_2d18,
];
const ATANLO: [u64; 4] = [
    0x3c7a_2b7f_222f_65e2,
    0x3c81_a626_3314_5c07,
    0x3c70_0788_7af0_cbbd,
    0x3c91_a626_3314_5c07,
];

/// Append the constant tables of the reserved block to the data section.
pub(super) fn emit_data(data: &mut DataSection) {
    let mut bytes = Vec::new();
    for v in IPIO2 {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for v in PIO2.iter().chain(&ATANHI).chain(&ATANLO) {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    data.active(0, &ConstExpr::i32_const(TABLES_BASE), bytes);
}

/// Declare every helper's type and function-section entry.
pub(super) fn register(types: &mut TypeSection, functions: &mut FunctionSection) {
    for (params, results) in TYPES {
        let index = types.len();
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        functions.function(index);
    }
}

impl Helpers<'_> {
    /// The wasm function index of libm helper `h`; the helpers follow `_start`.
    pub(super) fn libm(self, h: H) -> u32 {
        self.start() + 1 + h as u32
    }
}

/// The helper implementing the unary `Math` method `op`, or `None`.
pub(super) fn unary_helper(op: MathOp) -> Option<H> {
    Some(match op {
        MathOp::Cbrt => H::Cbrt,
        MathOp::Log => H::Log,
        MathOp::Log2 => H::Log2,
        MathOp::Log10 => H::Log10,
        MathOp::Log1p => H::Log1p,
        MathOp::Exp => H::Exp,
        MathOp::Expm1 => H::Expm1,
        MathOp::Sin => H::Sin,
        MathOp::Cos => H::Cos,
        MathOp::Tan => H::Tan,
        MathOp::Asin => H::Asin,
        MathOp::Acos => H::Acos,
        MathOp::Atan => H::Atan,
        MathOp::Sinh => H::Sinh,
        MathOp::Cosh => H::Cosh,
        MathOp::Tanh => H::Tanh,
        MathOp::Asinh => H::Asinh,
        MathOp::Acosh => H::Acosh,
        MathOp::Atanh => H::Atanh,
        _ => return None,
    })
}

/// Every helper body, in [`H`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>) -> Vec<Function> {
    let bodies: [fn() -> B; 30] = [
        emit_scalbn,
        emit_mul_err,
        emit_asin_r,
        emit_exp,
        emit_expm1,
        emit_log,
        emit_log1p,
        emit_log2,
        emit_log10,
        emit_cbrt,
        emit_k_sin,
        emit_k_cos,
        emit_k_tan,
        emit_rem_medium,
        emit_rem_large,
        emit_rem_pio2,
        emit_sin,
        emit_cos,
        emit_tan,
        emit_asin,
        emit_acos,
        emit_atan,
        emit_atan2,
        emit_expo2,
        emit_sinh,
        emit_cosh,
        emit_tanh,
        emit_asinh,
        emit_acosh,
        emit_atanh,
    ];
    let base = helpers.libm(H::Scalbn);
    bodies.iter().map(|body| body().finish(base)).collect()
}

/// `f64` from a bit pattern, for the constants below.
const fn c(bits: u64) -> f64 {
    f64::from_bits(bits)
}

/// A high-word constant as the `i32` wasm compares.
const fn u(v: u32) -> i32 {
    v.cast_signed()
}

const X1P1023: f64 = c(0x7fe0_0000_0000_0000);
const X1P54: f64 = c(0x4350_0000_0000_0000);
const X1P24: f64 = c(0x4170_0000_0000_0000);
const X1P_24: f64 = c(0x3e70_0000_0000_0000);
const LOW_WORD: i64 = 0xffff_ffff;

const LN2_HI: f64 = c(0x3fe6_2e42_fee0_0000);
const LN2_LO: f64 = c(0x3dea_39ef_3579_3c76);
const LN2: f64 = c(0x3fe6_2e42_fefa_39ef);
const INVLN2: f64 = c(0x3ff7_1547_652b_82fe);

/// `scalbn(x, n)` (`generic/scalbn.rs`).
fn emit_scalbn() -> B {
    const EXP_MAX: i32 = 1023;
    const EXP_MIN: i32 = -1022;
    /// `2^-1022 * 2^53`.
    const MUL: f64 = c(0x0360_0000_0000_0000);
    /// `-EXP_MIN - 53`.
    const ADD: i32 = 969;
    let mut b = B::new(2);
    let (x, nv) = (VF(0), VI(1));
    b.if_else(
        nv.gt_s(EXP_MAX),
        |b| {
            b.sf(x, x * X1P1023);
            b.si(nv, nv - EXP_MAX);
            b.if_(nv.gt_s(EXP_MAX), |b| {
                b.sf(x, x * X1P1023);
                b.si(nv, nv - EXP_MAX);
                b.if_(nv.gt_s(EXP_MAX), |b| b.si(nv, EXP_MAX));
            });
        },
        |b| {
            b.if_(nv.lt_s(EXP_MIN), |b| {
                b.sf(x, x * MUL);
                b.si(nv, nv + ADD);
                b.if_(nv.lt_s(EXP_MIN), |b| {
                    b.sf(x, x * MUL);
                    b.si(nv, nv + ADD);
                    b.if_(nv.lt_s(EXP_MIN), |b| b.si(nv, EXP_MIN));
                });
            });
        },
    );
    b.ret(x * ((nv + 1023).sext() << 52).reinterpret());
    b
}

/// `a * c - p` exactly, for `p == fl(a * c)` far from overflow (Dekker).
fn emit_mul_err() -> B {
    /// `2^27 + 1`.
    const SPLIT: f64 = c(0x41a0_0000_0200_0000);
    let mut b = B::new(3);
    let (lhs, rhs, pv) = (VF(0), VF(1), VF(2));
    let tmp = b.f(SPLIT * lhs);
    let lhs_hi = b.f(tmp - (tmp - lhs));
    let lhs_lo = b.f(lhs - lhs_hi);
    b.sf(tmp, SPLIT * rhs);
    let rhs_hi = b.f(tmp - (tmp - rhs));
    let rhs_lo = b.f(rhs - rhs_hi);
    b.ret(lhs_hi * rhs_hi - pv + lhs_hi * rhs_lo + lhs_lo * rhs_hi + lhs_lo * rhs_lo);
    b
}

const P_S0: f64 = c(0x3fc5_5555_5555_5555);
const P_S1: f64 = c(0xbfd4_d612_03eb_6f7d);
const P_S2: f64 = c(0x3fc9_c155_0e88_4455);
const P_S3: f64 = c(0xbfa4_8228_b568_8f3b);
const P_S4: f64 = c(0x3f49_efe0_7501_b288);
const P_S5: f64 = c(0x3f02_3de1_0dfd_f709);
const Q_S1: f64 = c(0xc003_3a27_1c8a_2d4b);
const Q_S2: f64 = c(0x4000_2ae5_9c59_8ac8);
const Q_S3: f64 = c(0xbfe6_066c_1b8d_0159);
const Q_S4: f64 = c(0x3fb3_b8c5_b12e_9282);
const PIO2_HI: f64 = c(0x3ff9_21fb_5444_2d18);
const PIO2_LO: f64 = c(0x3c91_a626_3314_5c07);

/// The rational kernel shared by `asin` and `acos`.
fn emit_asin_r() -> B {
    let mut b = B::new(1);
    let zv = VF(0);
    let pv = b.f(zv * (P_S0 + zv * (P_S1 + zv * (P_S2 + zv * (P_S3 + zv * (P_S4 + zv * P_S5))))));
    let qv = b.f(1.0 + zv * (Q_S1 + zv * (Q_S2 + zv * (Q_S3 + zv * Q_S4))));
    b.ret(pv / qv);
    b
}

/// `exp(x)`.
fn emit_exp() -> B {
    const P1: f64 = c(0x3fc5_5555_5555_553e);
    const P2: f64 = c(0xbf66_c16c_16be_bd93);
    const P3: f64 = c(0x3f11_566a_af25_de2c);
    const P4: f64 = c(0xbebb_bd41_c5d2_6bf1);
    const P5: f64 = c(0x3e66_3769_72be_a4d0);
    const OVERFLOW: f64 = c(0x4086_2e42_fefa_39ef);
    const ZERO_BELOW: f64 = c(0xc087_4910_d52d_3051);
    let mut b = B::new(1);
    let x = VF(0);
    let top = b.i(x.hi());
    let sign = b.i(top.shr_u(31));
    let hx = b.i(top & 0x7fff_ffff);
    b.if_(hx.ge_u(0x4086_232b), |b| {
        b.if_(x.ne(x), |b| b.ret(x));
        b.if_(x.gt(OVERFLOW), |b| b.ret(x * X1P1023));
        b.if_(x.lt(ZERO_BELOW), |b| b.ret(0.0));
    });
    let kv = b.i(0);
    let hi = b.f(0.0);
    let lo = b.f(0.0);
    b.if_else(
        hx.gt_u(0x3fd6_2e42),
        |b| {
            b.if_else(
                hx.ge_u(0x3ff0_a2b2),
                |b| b.si(kv, (INVLN2 * x + select(sign, -0.5, 0.5)).trunc_i()),
                |b| b.si(kv, I::from(1) - sign - sign),
            );
            b.sf(hi, x - kv.i2f() * LN2_HI);
            b.sf(lo, kv.i2f() * LN2_LO);
            b.sf(x, hi - lo);
        },
        |b| {
            b.if_else(hx.gt_u(0x3e30_0000), |b| b.sf(hi, x), |b| b.ret(1.0 + x));
        },
    );
    let xx = b.f(x * x);
    let cc = b.f(x - xx * (P1 + xx * (P2 + xx * (P3 + xx * (P4 + xx * P5)))));
    let yv = b.f(1.0 + (x * cc / (2.0 - cc) - lo + hi));
    b.if_(kv.eqz(), |b| b.ret(yv));
    b.ret(scalbn(yv, kv));
    b
}

/// `expm1(x)`.
fn emit_expm1() -> B {
    const O_THRESHOLD: f64 = c(0x4086_2e42_fefa_39ef);
    let mut b = B::new(1);
    let x = VF(0);
    let top = b.i(x.hi());
    let hx = b.i(top & 0x7fff_ffff);
    let sign = b.i(top.shr_u(31));
    b.if_(hx.ge_u(0x4043_687a), |b| {
        b.if_(x.ne(x), |b| b.ret(x));
        b.if_(sign, |b| b.ret(-1.0));
        b.if_(x.gt(O_THRESHOLD), |b| b.ret(x * X1P1023));
    });
    let (hi, lo, cc) = (b.f(0.0), b.f(0.0), b.f(0.0));
    let kv = b.i(0);
    b.if_else(
        hx.gt_u(0x3fd6_2e42),
        |b| {
            b.if_else(
                hx.lt_u(0x3ff0_a2b2),
                |b| {
                    b.if_else(
                        sign.eqz(),
                        |b| {
                            b.sf(hi, x - LN2_HI);
                            b.sf(lo, LN2_LO);
                            b.si(kv, 1);
                        },
                        |b| {
                            b.sf(hi, x + LN2_HI);
                            b.sf(lo, -LN2_LO);
                            b.si(kv, -1);
                        },
                    );
                },
                |b| {
                    b.si(kv, (INVLN2 * x + select(sign, -0.5, 0.5)).trunc_i());
                    b.sf(hi, x - kv.i2f() * LN2_HI);
                    b.sf(lo, kv.i2f() * LN2_LO);
                },
            );
            b.sf(x, hi - lo);
            b.sf(cc, hi - x - lo);
        },
        |b| b.if_(hx.lt_u(0x3c90_0000), |b| b.ret(x)),
    );
    expm1_tail(&mut b, x, cc, kv);
    b
}

/// The primary-range polynomial and `2^k` reconstruction of `expm1`.
fn expm1_tail(b: &mut B, x: VF, cc: VF, kv: VI) {
    const Q1: f64 = c(0xbfa1_1111_1111_10f4);
    const Q2: f64 = c(0x3f5a_01a0_19fe_5585);
    const Q3: f64 = c(0xbf14_ce19_9eaa_dbb7);
    const Q4: f64 = c(0x3ed0_cfca_86e6_5239);
    const Q5: f64 = c(0xbe8a_fdb7_6e09_c32d);
    let hfx = b.f(0.5 * x);
    let hxs = b.f(x * hfx);
    let r1 = b.f(1.0 + hxs * (Q1 + hxs * (Q2 + hxs * (Q3 + hxs * (Q4 + hxs * Q5)))));
    let tv = b.f(3.0 - r1 * hfx);
    let ev = b.f(hxs * ((r1 - tv) / (6.0 - x * tv)));
    b.if_(kv.eqz(), |b| b.ret(x - (x * ev - hxs)));
    b.sf(ev, x * (ev - cc) - cc);
    b.sf(ev, ev - hxs);
    b.if_(kv.eq(-1), |b| b.ret(0.5 * (x - ev) - 0.5));
    b.if_(kv.eq(1), |b| {
        b.if_(x.lt(-0.25), |b| b.ret(-2.0 * (ev - (x + 0.5))));
        b.ret(1.0 + 2.0 * (x - ev));
    });
    let twopk = b.f(((kv + 0x3ff).sext() << 52).reinterpret());
    let yv = b.f(0.0);
    b.if_(kv.lt_s(0) | kv.gt_s(56), |b| {
        b.sf(yv, x - ev + 1.0);
        b.if_else(
            kv.eq(1024),
            |b| b.sf(yv, yv * 2.0 * X1P1023),
            |b| b.sf(yv, yv * twopk),
        );
        b.ret(yv - 1.0);
    });
    let uf = b.f(((I::from(0x3ff) - kv).sext() << 52).reinterpret());
    b.if_else(
        kv.lt_s(20),
        |b| b.sf(yv, (x - ev + (1.0 - uf)) * twopk),
        |b| b.sf(yv, (x - (ev + uf) + 1.0) * twopk),
    );
    b.ret(yv);
}

const LG1: f64 = c(0x3fe5_5555_5555_5593);
const LG2: f64 = c(0x3fd9_9999_9997_fa04);
const LG3: f64 = c(0x3fd2_4924_9422_9359);
const LG4: f64 = c(0x3fcc_71c5_1d8e_78af);
const LG5: f64 = c(0x3fc7_4664_96cb_03de);
const LG6: f64 = c(0x3fc3_9a09_d078_c69f);
const LG7: f64 = c(0x3fc2_f112_df3e_5244);

/// The special cases and reduction shared by `log`, `log2` and `log10`:
/// `f` in `[sqrt(2)/2 - 1, sqrt(2) - 1]` and the exponent `k`.
fn log_reduce(b: &mut B, x: VF) -> (VF, VI) {
    let kv = b.i(0);
    let ui = b.l(x.bits());
    let hx = b.i(ui.shr_u(32).wrap());
    b.if_else(
        hx.lt_u(0x0010_0000) | hx.shr_u(31),
        |b| {
            b.if_((ui << 1).eqz(), |b| b.ret(-1.0 / (x * x)));
            b.if_(hx.shr_u(31), |b| b.ret(self_sub(x) / 0.0));
            b.si(kv, kv - 54);
            b.sf(x, x * X1P54);
            b.sl(ui, x.bits());
            b.si(hx, ui.shr_u(32).wrap());
        },
        |b| {
            b.if_(hx.ge_u(0x7ff0_0000), |b| b.ret(x));
            b.if_(hx.eq(0x3ff0_0000) & (ui << 32).eqz(), |b| b.ret(0.0));
        },
    );
    b.si(hx, hx + (0x3ff0_0000 - 0x3fe6_a09e));
    b.si(kv, kv + (hx.shr_u(20) - 0x3ff));
    b.si(hx, (hx & 0x000f_ffff) + 0x3fe6_a09e);
    b.sl(ui, (hx.zext() << 32) | (ui & LOW_WORD));
    let fv = b.f(ui.reinterpret() - 1.0);
    (fv, kv)
}

/// The shared `log(1+f)` polynomial: `(hfsq, s, r)`.
fn log_poly(b: &mut B, fv: VF) -> (VF, VF, VF) {
    let hfsq = b.f(0.5 * fv * fv);
    let sv = b.f(fv / (2.0 + fv));
    let zv = b.f(sv * sv);
    let wv = b.f(zv * zv);
    let t1 = b.f(wv * (LG2 + wv * (LG4 + wv * LG6)));
    let t2 = b.f(zv * (LG1 + wv * (LG3 + wv * (LG5 + wv * LG7))));
    let rv = b.f(t2 + t1);
    (hfsq, sv, rv)
}

/// `log(x)`.
fn emit_log() -> B {
    let mut b = B::new(1);
    let (fv, kv) = log_reduce(&mut b, VF(0));
    let (hfsq, sv, rv) = log_poly(&mut b, fv);
    let dk = b.f(kv.i2f());
    b.ret(sv * (hfsq + rv) + dk * LN2_LO - hfsq + fv + dk * LN2_HI);
    b
}

/// `log1p(x)`.
fn emit_log1p() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let hx = b.i(x.hi());
    let kv = b.i(1);
    let (fv, cc) = (b.f(0.0), b.f(0.0));
    b.if_else(
        hx.lt_u(0x3fda_827a) | hx.shr_u(31),
        |b| {
            b.if_(hx.ge_u(u(0xbff0_0000)), |b| {
                b.if_(x.eq(-1.0), |b| b.ret(x / 0.0));
                b.ret(self_sub(x) / 0.0);
            });
            b.if_((hx << 1).lt_u(0x3ca0_0000 << 1), |b| b.ret(x));
            b.if_(hx.le_u(u(0xbfd2_bec4)), |b| {
                b.si(kv, 0);
                b.sf(fv, x);
            });
        },
        |b| b.if_(hx.ge_u(0x7ff0_0000), |b| b.ret(x)),
    );
    b.if_(kv.gt_s(0), |b| {
        let ui = b.l((1.0 + x).bits());
        let hu = b.i(ui.shr_u(32).wrap() + (0x3ff0_0000 - 0x3fe6_a09e));
        b.si(kv, hu.shr_u(20) - 0x3ff);
        b.if_(kv.lt_s(54), |b| {
            b.if_else(
                kv.ge_s(2),
                |b| b.sf(cc, 1.0 - (ui.reinterpret() - x)),
                |b| b.sf(cc, x - (ui.reinterpret() - 1.0)),
            );
            b.sf(cc, cc / ui.reinterpret());
        });
        b.si(hu, (hu & 0x000f_ffff) + 0x3fe6_a09e);
        b.sl(ui, (hu.zext() << 32) | (ui & LOW_WORD));
        b.sf(fv, ui.reinterpret() - 1.0);
    });
    let (hfsq, sv, rv) = log_poly(&mut b, fv);
    let dk = b.f(kv.i2f());
    b.ret(sv * (hfsq + rv) + (dk * LN2_LO + cc) - hfsq + fv + dk * LN2_HI);
    b
}

/// `hi` and `lo` with `hi + lo == f - hfsq + s*(hfsq+r)` for `log2`/`log10`.
fn log_hi_lo(b: &mut B, fv: VF) -> (VF, VF) {
    let (hfsq, sv, rv) = log_poly(b, fv);
    let hi = b.f((fv - hfsq).zero_lo());
    let lo = b.f(fv - hi - hfsq + sv * (hfsq + rv));
    (hi, lo)
}

/// The extra-precision final add of `log2`/`log10` (`spadd`).
fn spadd_ret(b: &mut B, yv: VF, val_hi: VF, val_lo: VF) {
    let wv = b.f(yv + val_hi);
    b.sf(val_lo, val_lo + (yv - wv + val_hi));
    b.ret(val_lo + wv);
}

/// `log2(x)`.
fn emit_log2() -> B {
    const IVLN2HI: f64 = c(0x3ff7_1547_6520_0000);
    const IVLN2LO: f64 = c(0x3de7_05fc_2eef_a200);
    let mut b = B::new(1);
    let (fv, kv) = log_reduce(&mut b, VF(0));
    let (hi, lo) = log_hi_lo(&mut b, fv);
    let val_hi = b.f(hi * IVLN2HI);
    let val_lo = b.f((lo + hi) * IVLN2LO + lo * IVLN2HI);
    let yv = b.f(kv.i2f());
    spadd_ret(&mut b, yv, val_hi, val_lo);
    b
}

/// `log10(x)`.
fn emit_log10() -> B {
    const IVLN10HI: f64 = c(0x3fdb_cb7b_1520_0000);
    const IVLN10LO: f64 = c(0x3dbb_9438_ca9a_add5);
    const LOG10_2HI: f64 = c(0x3fd3_4413_509f_6000);
    const LOG10_2LO: f64 = c(0x3d59_fef3_11f1_2b36);
    let mut b = B::new(1);
    let (fv, kv) = log_reduce(&mut b, VF(0));
    let (hi, lo) = log_hi_lo(&mut b, fv);
    let val_hi = b.f(hi * IVLN10HI);
    let dk = b.f(kv.i2f());
    let yv = b.f(dk * LOG10_2HI);
    let val_lo = b.f(dk * LOG10_2LO + (lo + hi) * IVLN10LO + lo * IVLN10HI);
    spadd_ret(&mut b, yv, val_hi, val_lo);
    b
}

const CBRT_U0: f64 = c(0x3fd5_5555_5555_5555);
const X1P_75: f64 = c(0x3b40_0000_0000_0000);
const X1P_98: f64 = c(0x39d0_0000_0000_0000);
/// `2^-53` and `2^-53 + 2^-52`: `off[Nearest]` and `off[Nearest] + 2^-52`.
const CBRT_OFF0: f64 = c(0x3ca0_0000_0000_0000);
const CBRT_OFF1: f64 = c(0x3cb8_0000_0000_0000);
const SIG_MASK: i64 = 0x000f_ffff_ffff_ffff;

/// `cbrt(x)`, rounding to nearest (CORE-MATH, as in `libm` 0.2.16).
fn emit_cbrt() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let hx = b.l(x.bits());
    let mant = b.l(hx & SIG_MASK);
    let sign = b.l(hx.shr_u(63));
    let ev = b.i(hx.shr_u(52).wrap() & 0x7ff);
    b.if_(((ev + 1) & 0x7ff).lt_u(2), |b| {
        let ix = b.l(hx & i64::MAX);
        b.if_(ev.eq(0x7ff) | ix.eqz(), |b| b.ret(x + x));
        let nz = b.l(ix.clz() + -11_i64);
        b.sl(mant, (mant << nz) & SIG_MASK);
        b.si(ev, ev - (nz.wrap() - 1));
    });
    b.si(ev, ev + 3072);
    let cvt1 = b.l(mant | (0x3ff_i64 << 52));
    let et = b.i(ev.div_u(3));
    let it = b.i(ev.rem_u(3));
    let zz = b.f(((cvt1 + (it.zext() << 52)) | (sign << 63)).reinterpret());
    let zv = b.f(cvt1.reinterpret());
    cbrt_finish(
        &mut b,
        CbrtArgs {
            zz,
            zv,
            et,
            it,
            sign,
        },
    );
    b
}

#[derive(Clone, Copy)]
struct CbrtArgs {
    zz: VF,
    zv: VF,
    et: VI,
    it: VI,
    sign: VL,
}

/// `2^(it/3)` with the sign bit of `sign`.
fn cbrt_escale(it: VI, sign: VL) -> F {
    const ESCALE1: u64 = 0x3ff4_28a2_f98d_728b;
    const ESCALE2: u64 = 0x3ff9_65fe_a53d_6e3d;
    let scale = select(
        it.eq(2),
        f64::from_bits(ESCALE2),
        select(it.eq(1), f64::from_bits(ESCALE1), 1.0),
    );
    (scale.bits() | (sign << 63)).reinterpret()
}

/// The Newton iterations and final rounding of `cbrt`.
fn cbrt_finish(b: &mut B, args: CbrtArgs) {
    const C0: f64 = c(0x3fe1_b0ba_bccf_ef9c);
    const C1: f64 = c(0x3fe2_c9a3_e94d_1da5);
    const C2: f64 = c(0xbfc4_dc30_b1a1_ddba);
    const C3: f64 = c(0x3f97_a8d3_e4ec_9b07);
    const U1: f64 = c(0x3fcc_71c7_1c71_c71c);
    let CbrtArgs {
        zz,
        zv,
        et,
        it,
        sign,
    } = args;
    let rv = b.f(1.0 / zv);
    // `r * rsc[(it << 1) | sign]`, `rsc[i] = (-1)^(i & 1) * 2^-(i >> 1)`.
    let rr = b.f(rv * (((I::from(0x3ff) - it).zext() << 52) | (sign << 63)).reinterpret());
    let z2 = b.f(zv * zv);
    let c0 = b.f(C0 + zv * C1);
    let c2 = b.f(C2 + zv * C3);
    let yv = b.f(c0 + z2 * c2);
    let y2 = b.f(yv * yv);
    let hv = b.f(y2 * (yv * rv) - 1.0);
    b.sf(yv, yv - hv * yv * (CBRT_U0 - U1 * hv));
    b.sf(yv, yv * cbrt_escale(it, sign));
    b.sf(y2, yv * yv);
    let y2l = b.f(mul_err_of(yv, yv, y2));
    let y3 = b.f(y2 * yv);
    let y3l = b.f(mul_err_of(yv, y2, y3) + yv * y2l);
    b.sf(hv, (y3 - zz + y3l) * rr);
    let dy = b.f(hv * (yv * CBRT_U0));
    let y1 = b.f(yv - dy);
    b.sf(dy, yv - y1 - dy);
    let ady = b.f(dy.fabs());
    let ady0 = b.f((ady - CBRT_OFF0).fabs());
    let ady1 = b.f((ady - CBRT_OFF1).fabs());
    b.if_(ady0.lt(X1P_75) | ady1.lt(X1P_75), |b| {
        b.sf(y2, y1 * y1);
        b.sf(y2l, mul_err_of(y1, y1, y2));
        b.sf(y3, y2 * y1);
        b.sf(y3l, mul_err_of(y1, y2, y3) + y1 * y2l);
        b.sf(hv, (y3 - zz + y3l) * rr);
        b.sf(dy, hv * (y1 * CBRT_U0));
        b.sf(yv, y1 - dy);
        b.sf(dy, y1 - yv - dy);
        b.sf(y1, yv);
        b.sf(ady, dy.fabs());
        b.sf(ady0, (ady - CBRT_OFF0).fabs());
        b.sf(ady1, (ady - CBRT_OFF1).fabs());
        b.if_(ady0.lt(X1P_98) | ady1.lt(X1P_98), |b| {
            let azz = b.f(zz.fabs());
            b.if_(azz.eq(c(0x4009_b782_23aa_307c)), |b| {
                b.sf(y1, F::from(c(0x3ff7_9d15_d0e8_d59c)).sign_of(zz));
            });
            b.if_(azz.eq(c(0x401a_202b_fc89_ddff)), |b| {
                b.sf(y1, F::from(c(0x3ffd_e87a_a837_820f)).sign_of(zz));
            });
        });
    });
    cbrt_round(b, y1, dy, zz, et);
}

/// Apply the exponent and repair the exact and midway cases of `cbrt`.
fn cbrt_round(b: &mut B, y1: VF, dy: VF, zz: VF, et: VI) {
    const X1P_60: f64 = c(0x3c30_0000_0000_0000);
    const TOP48: i64 = -0x1_0000;
    let cvt3 = b.l(y1.bits() + ((et - 342 - 1023).zext() << 52));
    let m0 = b.l(cvt3 << 30);
    let m1 = b.l(m0.shr_u(63));
    b.if_((m0 ^ m1).le_u(1_i64 << 30), |b| {
        let cvt4 = b.l((y1.bits() + (164_i64 << 15)) & TOP48);
        b.if_(
            (cvt4.reinterpret() - y1 - dy).fabs().lt(X1P_60) | zz.fabs().eq(1.0),
            |b| b.sl(cvt3, (cvt3 + (1_i64 << 15)) & TOP48),
        );
    });
    b.ret(cvt3.reinterpret());
}

/// `__sin(x, y, yflag)`.
fn emit_k_sin() -> B {
    const S1: f64 = c(0xbfc5_5555_5555_5549);
    const S2: f64 = c(0x3f81_1111_1110_f8a6);
    const S3: f64 = c(0xbf2a_01a0_19c1_61d5);
    const S4: f64 = c(0x3ec7_1de3_57b1_fe7d);
    const S5: f64 = c(0xbe5a_e5e6_8a2b_9ceb);
    const S6: f64 = c(0x3de5_d93a_5acf_d57c);
    let mut b = B::new(3);
    let (x, yv, yflag) = (VF(0), VF(1), VI(2));
    let zv = b.f(x * x);
    let wv = b.f(zv * zv);
    let rv = b.f(S2 + zv * (S3 + zv * S4) + zv * wv * (S5 + zv * S6));
    let vv = b.f(zv * x);
    b.if_(yflag.eqz(), |b| b.ret(x + vv * (S1 + zv * rv)));
    b.ret(x - (zv * (0.5 * yv - vv * rv) - yv - vv * S1));
    b
}

/// `__cos(x, y)`.
fn emit_k_cos() -> B {
    const C1: f64 = c(0x3fa5_5555_5555_554c);
    const C2: f64 = c(0xbf56_c16c_16c1_5177);
    const C3: f64 = c(0x3efa_01a0_19cb_1590);
    const C4: f64 = c(0xbe92_7e4f_809c_52ad);
    const C5: f64 = c(0x3e21_ee9e_bdb4_b1c4);
    const C6: f64 = c(0xbda8_fae9_be88_38d4);
    let mut b = B::new(2);
    let (x, yv) = (VF(0), VF(1));
    let zv = b.f(x * x);
    let wv = b.f(zv * zv);
    let rv = b.f(zv * (C1 + zv * (C2 + zv * C3)) + wv * wv * (C4 + zv * (C5 + zv * C6)));
    let hz = b.f(0.5 * zv);
    b.sf(wv, 1.0 - hz);
    b.ret(wv + (1.0 - wv - hz + (zv * rv - x * yv)));
    b
}

/// `__tan(x, y, odd)`.
fn emit_k_tan() -> B {
    const T: [f64; 13] = [
        c(0x3fd5_5555_5555_5563),
        c(0x3fc1_1111_1110_fe7a),
        c(0x3fab_a1ba_1bb3_41fe),
        c(0x3f96_64f4_8406_d637),
        c(0x3f82_26e3_e96e_8493),
        c(0x3f6d_6d22_c956_0328),
        c(0x3f57_dbc8_fee0_8315),
        c(0x3f43_44d8_f2f2_6501),
        c(0x3f30_26f7_1a8d_1068),
        c(0x3f14_7e88_a037_92a6),
        c(0x3f12_b80f_32f0_a7e9),
        c(0xbef3_75cb_db60_5373),
        c(0x3efb_2a70_74bf_7ad4),
    ];
    const PIO4: f64 = c(0x3fe9_21fb_5444_2d18);
    const PIO4_LO: f64 = c(0x3c81_a626_3314_5c07);
    let mut b = B::new(3);
    let (x, yv, odd) = (VF(0), VF(1), VI(2));
    let hx = b.i(x.hi());
    let big = b.i((hx & 0x7fff_ffff).ge_u(0x3fe5_9428));
    let sign = b.i(hx.shr_u(31));
    b.if_(big, |b| {
        b.if_(sign, |b| {
            b.sf(x, -x);
            b.sf(yv, -yv);
        });
        b.sf(x, PIO4 - x + (PIO4_LO - yv));
        b.sf(yv, 0.0);
    });
    let zv = b.f(x * x);
    let wv = b.f(zv * zv);
    let rv = b.f(T[1] + wv * (T[3] + wv * (T[5] + wv * (T[7] + wv * (T[9] + wv * T[11])))));
    let vv = b.f(zv * (T[2] + wv * (T[4] + wv * (T[6] + wv * (T[8] + wv * (T[10] + wv * T[12]))))));
    let sv = b.f(zv * x);
    b.sf(rv, yv + zv * (sv * (rv + vv) + yv) + sv * T[0]);
    b.sf(wv, x + rv);
    b.if_(big, |b| {
        b.sf(sv, 1.0 - 2.0 * odd.i2f());
        b.sf(vv, sv - 2.0 * (x + (rv - wv * wv / (wv + sv))));
        b.ret(select(sign, -vv, vv));
    });
    b.if_(odd.eqz(), |b| b.ret(wv));
    let w0 = b.f(wv.zero_lo());
    b.sf(vv, rv - (w0 - x));
    let av = b.f(-1.0 / wv);
    let a0 = b.f(av.zero_lo());
    b.ret(a0 + av * (1.0 + a0 * w0 + a0 * vv));
    b
}

const INV_PIO2: f64 = c(0x3fe4_5f30_6dc9_c883);
const TO_INT: f64 = c(0x4338_0000_0000_0000);
const PIO2_1: f64 = c(0x3ff9_21fb_5440_0000);
const PIO2_1T: f64 = c(0x3dd0_b461_1a62_6331);
const PIO2_2: f64 = c(0x3dd0_b461_1a60_0000);
const PIO2_2T: f64 = c(0x3ba3_198a_2e03_7073);
const PIO2_3: f64 = c(0x3ba3_198a_2e00_0000);
const PIO2_3T: f64 = c(0x397b_839a_2520_49c1);

/// The biased exponent of `v`.
fn exp_of(vv: VF) -> I {
    vv.bits().shr_u(52).wrap() & 0x7ff
}

/// `rem_pio2`'s medium path for `|x| < 2^20 * pi/2`.
fn emit_rem_medium() -> B {
    let mut b = B::new(2);
    let (x, ix) = (VF(0), VI(1));
    let tmp = b.f(x * INV_PIO2 + TO_INT);
    let f_n = b.f(tmp - TO_INT);
    let nv = b.i(f_n.trunc_i());
    let rv = b.f(x - f_n * PIO2_1);
    let wv = b.f(f_n * PIO2_1T);
    let y0 = b.f(rv - wv);
    let ex = b.i(ix.shr_u(20));
    let tv = b.f(0.0);
    b.if_((ex - exp_of(y0)).gt_s(16), |b| {
        b.sf(tv, rv);
        b.sf(wv, f_n * PIO2_2);
        b.sf(rv, tv - wv);
        b.sf(wv, f_n * PIO2_2T - (tv - rv - wv));
        b.sf(y0, rv - wv);
        b.if_((ex - exp_of(y0)).gt_s(49), |b| {
            b.sf(tv, rv);
            b.sf(wv, f_n * PIO2_3);
            b.sf(rv, tv - wv);
            b.sf(wv, f_n * PIO2_3T - (tv - rv - wv));
            b.sf(y0, rv - wv);
        });
    });
    let y1 = b.f(rv - y0 - wv);
    b.ret3(nv, y0, y1);
    b
}

/// The `prec = 1` Payne-Hanek state of `rem_pio2_large`.
#[derive(Clone, Copy)]
struct Large {
    jx: VI,
    jv: VI,
    q0: VI,
    jz: VI,
    zv: VF,
    nv: VI,
    ih: VI,
    i: VI,
    j: VI,
    fw: VF,
}

const JK: i32 = 4;

/// `q[i] = sum(x[j] * f[jx + i - j])`, `j` in `0..=jx`.
fn large_sum(b: &mut B, st: Large, i: VI) {
    b.sf(st.fw, 0.0);
    b.up(st.j, 0, st.jx, |b| {
        b.sf(
            st.fw,
            st.fw + fload(X_ADDR, st.j) * fload(F_ADDR, st.jx + i - st.j),
        );
    });
    b.fsto(Q_ADDR, i, st.fw);
}

/// Distill `q[]` into `iq[]`, then compute `n`, `z` and `ih`.
fn large_distill(b: &mut B, st: Large) {
    b.si(st.i, 0);
    b.sf(st.zv, fload(Q_ADDR, st.jz));
    b.si(st.j, st.jz);
    b.while_(st.j.ge_s(1), |b| {
        b.sf(st.fw, (X1P_24 * st.zv).trunc_i().i2f());
        b.isto(IQ_ADDR, st.i, (st.zv - X1P24 * st.fw).trunc_i());
        b.sf(st.zv, fload(Q_ADDR, st.j - 1) + st.fw);
        b.si(st.i, st.i + 1);
        b.si(st.j, st.j - 1);
    });
    b.sf(st.zv, scalbn(st.zv, st.q0));
    b.sf(st.zv, st.zv - 8.0 * (st.zv * 0.125).ffloor());
    b.si(st.nv, st.zv.trunc_i());
    b.sf(st.zv, st.zv - st.nv.i2f());
    b.si(st.ih, 0);
    let top = st.jz - 1;
    b.if_else(
        st.q0.gt_s(0),
        |b| {
            b.si(st.i, iload(IQ_ADDR, top.clone()).shr_s(I::from(24) - st.q0));
            b.si(st.nv, st.nv + st.i);
            b.isto(
                IQ_ADDR,
                top.clone(),
                iload(IQ_ADDR, top.clone()) - (st.i << (I::from(24) - st.q0)),
            );
            b.si(
                st.ih,
                iload(IQ_ADDR, top.clone()).shr_s(I::from(23) - st.q0),
            );
        },
        |b| {
            b.if_else(
                st.q0.eqz(),
                |b| b.si(st.ih, iload(IQ_ADDR, top.clone()).shr_s(23)),
                |b| b.if_(st.zv.ge(0.5), |b| b.si(st.ih, 2)),
            );
        },
    );
}

/// `ih > 0`: replace the fraction `q` by `1 - q`.
fn large_negate(b: &mut B, st: Large, carry: VI) {
    b.si(st.nv, st.nv + 1);
    b.si(carry, 0);
    b.si(st.i, 0);
    b.while_(st.i.lt_s(st.jz), |b| {
        b.si(st.j, iload(IQ_ADDR, st.i));
        b.if_else(
            carry.eqz(),
            |b| {
                b.if_(st.j, |b| {
                    b.si(carry, 1);
                    b.isto(IQ_ADDR, st.i, I::from(0x0100_0000) - st.j);
                });
            },
            |b| b.isto(IQ_ADDR, st.i, I::from(0x00ff_ffff) - st.j),
        );
        b.si(st.i, st.i + 1);
    });
    let top = st.jz - 1;
    b.if_(st.q0.eq(1), |b| {
        b.isto(
            IQ_ADDR,
            top.clone(),
            iload(IQ_ADDR, top.clone()) & 0x007f_ffff,
        );
    });
    b.if_(st.q0.eq(2), |b| {
        b.isto(
            IQ_ADDR,
            top.clone(),
            iload(IQ_ADDR, top.clone()) & 0x003f_ffff,
        );
    });
    b.if_(st.ih.eq(2), |b| {
        b.sf(st.zv, 1.0 - st.zv);
        b.if_(carry, |b| b.sf(st.zv, st.zv - scalbn(1.0, st.q0)));
    });
}

/// `z == 0` and every low chunk zero: extend `q` by the terms needed.
fn large_recompute(b: &mut B, st: Large, kv: VI, again: VI) {
    b.si(st.j, 0);
    b.down(st.i, st.jz - 1, JK, |b| {
        b.si(st.j, st.j | iload(IQ_ADDR, st.i));
    });
    b.if_(st.j.eqz(), |b| {
        b.si(kv, 1);
        b.while_(iload(IQ_ADDR, I::from(JK) - kv).eqz(), |b| b.si(kv, kv + 1));
        b.si(st.i, st.jz + 1);
        b.while_(st.i.le_s(st.jz + kv), |b| {
            b.fsto(F_ADDR, st.jx + st.i, iload(IPIO2_ADDR, st.jv + st.i).i2f());
            large_sum(b, st, st.i);
            b.si(st.i, st.i + 1);
        });
        b.si(st.jz, st.jz + kv);
        b.si(again, 1);
    });
}

/// Chop zero terms (or split `z` into a final chunk), then rebuild `fq[]`.
fn large_finish(b: &mut B, st: Large, kv: VI) {
    b.if_else(
        st.zv.eq(0.0),
        |b| {
            b.si(st.jz, st.jz - 1);
            b.si(st.q0, st.q0 - 24);
            b.while_(iload(IQ_ADDR, st.jz).eqz(), |b| {
                b.si(st.jz, st.jz - 1);
                b.si(st.q0, st.q0 - 24);
            });
        },
        |b| {
            b.sf(st.zv, scalbn(st.zv, I::from(0) - st.q0));
            b.if_else(
                st.zv.ge(X1P24),
                |b| {
                    b.sf(st.fw, (X1P_24 * st.zv).trunc_i().i2f());
                    b.isto(IQ_ADDR, st.jz, (st.zv - X1P24 * st.fw).trunc_i());
                    b.si(st.jz, st.jz + 1);
                    b.si(st.q0, st.q0 + 24);
                    b.isto(IQ_ADDR, st.jz, st.fw.trunc_i());
                },
                |b| b.isto(IQ_ADDR, st.jz, st.zv.trunc_i()),
            );
        },
    );
    b.sf(st.fw, scalbn(1.0, st.q0));
    b.down(st.i, st.jz, 0, |b| {
        b.fsto(Q_ADDR, st.i, st.fw * iload(IQ_ADDR, st.i).i2f());
        b.sf(st.fw, st.fw * X1P_24);
    });
    b.down(st.i, st.jz, 0, |b| {
        b.sf(st.fw, 0.0);
        b.si(kv, 0);
        b.while_(kv.le_s(JK) & kv.le_s(st.jz - st.i), |b| {
            b.sf(
                st.fw,
                st.fw + fload(PIO2_ADDR, kv) * fload(Q_ADDR, st.i + kv),
            );
            b.si(kv, kv + 1);
        });
        b.fsto(FQ_ADDR, st.jz - st.i, st.fw);
    });
}

/// `rem_pio2_large` at `prec = 1`, over the `nx` chunks at `X_ADDR`.
fn emit_rem_large() -> B {
    let mut b = B::new(2);
    let (nx, e0) = (VI(0), VI(1));
    let jx = b.i(nx - 1);
    let jv = b.i((e0 - 3).div_s(24));
    b.if_(jv.lt_s(0), |b| b.si(jv, 0));
    let st = Large {
        jx,
        jv,
        q0: b.i(e0 - (jv + 1) * 24),
        jz: b.i(JK),
        zv: b.f(0.0),
        nv: b.i(0),
        ih: b.i(0),
        i: b.i(0),
        j: b.i(jv - jx),
        fw: b.f(0.0),
    };
    b.up(st.i, 0, jx + JK, |b| {
        b.fsto(F_ADDR, st.i, 0.0);
        b.if_(st.j.ge_s(0), |b| {
            b.fsto(F_ADDR, st.i, iload(IPIO2_ADDR, st.j).i2f());
        });
        b.si(st.j, st.j + 1);
    });
    b.up(st.i, 0, JK, |b| large_sum(b, st, st.i));
    let (carry, kv, again) = (b.i(0), b.i(0), b.i(0));
    b.do_while(
        |b| {
            b.si(again, 0);
            large_distill(b, st);
            b.if_(st.ih.gt_s(0), |b| large_negate(b, st, carry));
            b.if_(st.zv.eq(0.0), |b| large_recompute(b, st, kv, again));
        },
        again,
    );
    large_finish(&mut b, st, kv);
    b.sf(st.fw, 0.0);
    b.down(st.i, st.jz, 0, |b| {
        b.sf(st.fw, st.fw + fload(FQ_ADDR, st.i));
    });
    let y0 = b.f(select(st.ih, -st.fw, st.fw));
    b.sf(st.fw, fload(FQ_ADDR, 0) - st.fw);
    b.up(st.i, 1, st.jz, |b| {
        b.sf(st.fw, st.fw + fload(FQ_ADDR, st.i));
    });
    let y1 = b.f(select(st.ih, -st.fw, st.fw));
    b.ret3(st.nv & 7, y0, y1);
    b
}

/// `return medium(x, ix)`.
fn ret_medium(b: &mut B, x: VF, ix: VI) {
    b.emit(call(H::RemMedium, vec![F::from(x).0, I::from(ix).0]));
    b.code.push(W::Return);
}

/// The one-round reduction by `m * pi/2`, for `|x|` near `m * pi/2`.
fn ret_multiple(b: &mut B, x: VF, sign: VI, mv: i32) {
    let hi = f64::from(mv) * PIO2_1;
    let lo = f64::from(mv) * PIO2_1T;
    b.if_(sign.eqz(), |b| {
        let zv = b.f(x - hi);
        let y0 = b.f(zv - lo);
        b.ret3(mv, y0, zv - y0 - lo);
    });
    let zv = b.f(x + hi);
    let y0 = b.f(zv + lo);
    b.ret3(-mv, y0, zv - y0 + lo);
}

/// `rem_pio2(x)`; callers have already filtered out infinities and NaN.
fn emit_rem_pio2() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let top = b.i(x.hi());
    let sign = b.i(top.shr_u(31));
    let ix = b.i(top & 0x7fff_ffff);
    b.if_(ix.le_u(0x400f_6a7a), |b| {
        b.if_((ix & 0x000f_ffff).eq(0x0009_21fb), |b| ret_medium(b, x, ix));
        b.if_(ix.le_u(0x4002_d97c), |b| ret_multiple(b, x, sign, 1));
        ret_multiple(b, x, sign, 2);
    });
    b.if_(ix.le_u(0x401c_463b), |b| {
        b.if_(ix.le_u(0x4015_fdbc), |b| {
            b.if_(ix.eq(0x4012_d97c), |b| ret_medium(b, x, ix));
            ret_multiple(b, x, sign, 3);
        });
        b.if_(ix.eq(0x4019_21fb), |b| ret_medium(b, x, ix));
        ret_multiple(b, x, sign, 4);
    });
    b.if_(ix.lt_u(0x4139_21fb), |b| ret_medium(b, x, ix));
    let zv = b.f(((x.bits() & SIG_MASK) | ((0x3ff_i64 + 23) << 52)).reinterpret());
    let chunk = b.f(zv.trunc_i().i2f());
    b.fsto(X_ADDR, 0, chunk);
    b.sf(zv, (zv - chunk) * X1P24);
    b.sf(chunk, zv.trunc_i().i2f());
    b.fsto(X_ADDR, 1, chunk);
    b.sf(zv, (zv - chunk) * X1P24);
    b.fsto(X_ADDR, 2, zv);
    let last = b.i(2);
    b.while_(last.ne(0) & fload(X_ADDR, last).eq(0.0), |b| {
        b.si(last, last - 1);
    });
    b.emit(call(
        H::RemLarge,
        vec![(last + 1).0, (ix.shr_s(20) - (0x3ff + 23)).0],
    ));
    let (y1, y0) = (VF(b.local(F64)), VF(b.local(F64)));
    let nv = VI(b.local(I32));
    b.code.push(W::LocalSet(y1.0));
    b.code.push(W::LocalSet(y0.0));
    b.code.push(W::LocalSet(nv.0));
    b.if_(sign, |b| b.ret3(I::from(0) - nv, -y0, -y1));
    b.ret3(nv, y0, y1);
    b
}

/// `let (n, y0, y1) = rem_pio2(x)`, with `n & 3` in the returned local.
fn rem_pio2_of(b: &mut B, x: VF) -> (VI, VF, VF) {
    b.emit(call(H::RemPio2, vec![F::from(x).0]));
    let (y1, y0) = (VF(b.local(F64)), VF(b.local(F64)));
    let nv = VI(b.local(I32));
    b.code.push(W::LocalSet(y1.0));
    b.code.push(W::LocalSet(y0.0));
    b.code.push(W::LocalSet(nv.0));
    b.si(nv, nv & 3);
    (nv, y0, y1)
}

/// `sin(x)`.
fn emit_sin() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ix = b.i(x.hi() & 0x7fff_ffff);
    let zero = b.f(0.0);
    b.if_(ix.le_u(0x3fe9_21fb), |b| {
        b.if_(ix.lt_u(0x3e50_0000), |b| b.ret(x));
        b.ret(k_sin(x, zero, 0));
    });
    b.if_(ix.ge_u(0x7ff0_0000), |b| b.ret(self_sub(x)));
    let (nv, y0, y1) = rem_pio2_of(&mut b, x);
    b.if_(nv.eqz(), |b| b.ret(k_sin(y0, y1, 1)));
    b.if_(nv.eq(1), |b| b.ret(k_cos(y0, y1)));
    b.if_(nv.eq(2), |b| b.ret(-k_sin(y0, y1, 1)));
    b.ret(-k_cos(y0, y1));
    b
}

/// `cos(x)`.
fn emit_cos() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ix = b.i(x.hi() & 0x7fff_ffff);
    b.if_(ix.le_u(0x3fe9_21fb), |b| {
        b.if_(ix.lt_u(0x3e46_a09e), |b| b.ret(1.0));
        b.ret(call2(H::KCos, x, 0.0));
    });
    b.if_(ix.ge_u(0x7ff0_0000), |b| b.ret(self_sub(x)));
    let (nv, y0, y1) = rem_pio2_of(&mut b, x);
    b.if_(nv.eqz(), |b| b.ret(k_cos(y0, y1)));
    b.if_(nv.eq(1), |b| b.ret(-k_sin(y0, y1, 1)));
    b.if_(nv.eq(2), |b| b.ret(-k_cos(y0, y1)));
    b.ret(k_sin(y0, y1, 1));
    b
}

/// `tan(x)`.
fn emit_tan() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ix = b.i(x.hi() & 0x7fff_ffff);
    let k_tan = |av: F, yv: F, odd: I| F(call(H::KTan, vec![av.0, yv.0, odd.0]));
    b.if_(ix.le_u(0x3fe9_21fb), |b| {
        b.if_(ix.lt_u(0x3e40_0000), |b| b.ret(x));
        b.ret(k_tan(x.into(), 0.0.into(), 0.into()));
    });
    b.if_(ix.ge_u(0x7ff0_0000), |b| b.ret(self_sub(x)));
    let (nv, y0, y1) = rem_pio2_of(&mut b, x);
    b.ret(k_tan(y0.into(), y1.into(), nv & 1));
    b
}

/// `x - x`: zero for a finite `x`, NaN otherwise.
fn self_sub(x: VF) -> F {
    F(bin(F::from(x).0, F::from(x).0, W::F64Sub))
}

/// `2^-120`.
const X1P_120: f64 = c(0x3870_0000_0000_0000);

/// `asin(x)`.
fn emit_asin() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let hx = b.i(x.hi());
    let ix = b.i(hx & 0x7fff_ffff);
    b.if_(ix.ge_u(0x3ff0_0000), |b| {
        b.if_(((ix - 0x3ff0_0000) | x.lo()).eqz(), |b| {
            b.ret(x * PIO2_HI + X1P_120);
        });
        b.ret(0.0 / self_sub(x));
    });
    b.if_(ix.lt_u(0x3fe0_0000), |b| {
        b.if_(ix.ge_u(0x0010_0000) & ix.lt_u(0x3e50_0000), |b| b.ret(x));
        b.ret(x + x * call1(H::AsinR, x * x));
    });
    let zv = b.f((1.0 - x.fabs()) * 0.5);
    let sv = b.f(zv.fsqrt());
    let rv = b.f(call1(H::AsinR, zv));
    b.if_else(
        ix.ge_u(0x3fef_3333),
        |b| b.sf(x, PIO2_HI - (2.0 * (sv + sv * rv) - PIO2_LO)),
        |b| {
            let fv = b.f(sv.zero_lo());
            let cc = b.f((zv - fv * fv) / (sv + fv));
            b.sf(
                x,
                0.5 * PIO2_HI - (2.0 * sv * rv - (PIO2_LO - 2.0 * cc) - (0.5 * PIO2_HI - 2.0 * fv)),
            );
        },
    );
    b.ret(select(hx.shr_u(31), -x, x));
    b
}

/// `acos(x)`.
fn emit_acos() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let hx = b.i(x.hi());
    let ix = b.i(hx & 0x7fff_ffff);
    b.if_(ix.ge_u(0x3ff0_0000), |b| {
        b.if_(((ix - 0x3ff0_0000) | x.lo()).eqz(), |b| {
            b.if_(hx.shr_u(31), |b| b.ret(2.0 * PIO2_HI + X1P_120));
            b.ret(0.0);
        });
        b.ret(0.0 / self_sub(x));
    });
    b.if_(ix.lt_u(0x3fe0_0000), |b| {
        b.if_(ix.le_u(0x3c60_0000), |b| b.ret(PIO2_HI + X1P_120));
        b.ret(PIO2_HI - (x - (PIO2_LO - x * call1(H::AsinR, x * x))));
    });
    b.if_(hx.shr_u(31), |b| {
        let zv = b.f((1.0 + x) * 0.5);
        let sv = b.f(zv.fsqrt());
        let wv = b.f(call1(H::AsinR, zv) * sv - PIO2_LO);
        b.ret(2.0 * (PIO2_HI - (sv + wv)));
    });
    let zv = b.f((1.0 - x) * 0.5);
    let sv = b.f(zv.fsqrt());
    let df = b.f(sv.zero_lo());
    let cc = b.f((zv - df * df) / (sv + df));
    let wv = b.f(call1(H::AsinR, zv) * sv + cc);
    b.ret(2.0 * (df + wv));
    b
}

const AT: [f64; 11] = [
    c(0x3fd5_5555_5555_550d),
    c(0xbfc9_9999_9998_ebc4),
    c(0x3fc2_4924_9200_83ff),
    c(0xbfbc_71c6_fe23_1671),
    c(0x3fb7_45cd_c54c_206e),
    c(0xbfb3_b0f2_af74_9a6d),
    c(0x3fb1_0d66_a0d0_3d51),
    c(0xbfad_de2d_52de_fd9a),
    c(0x3fa9_7b4b_2476_0deb),
    c(0xbfa2_b444_2c6a_6c2f),
    c(0x3f90_ad3a_e322_da11),
];

/// `atan(x)`.
fn emit_atan() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let top = b.i(x.hi());
    let sign = b.i(top.shr_u(31));
    let ix = b.i(top & 0x7fff_ffff);
    b.if_(ix.ge_u(0x4410_0000), |b| {
        b.if_(x.ne(x), |b| b.ret(x));
        b.ret(select(sign, -PIO2_HI, PIO2_HI));
    });
    let id = b.i(-1);
    b.if_else(
        ix.lt_u(0x3fdc_0000),
        |b| b.if_(ix.lt_u(0x3e40_0000), |b| b.ret(x)),
        |b| atan_reduce(b, x, ix, id),
    );
    let zv = b.f(x * x);
    let wv = b.f(zv * zv);
    let s1 =
        b.f(zv * (AT[0] + wv * (AT[2] + wv * (AT[4] + wv * (AT[6] + wv * (AT[8] + wv * AT[10]))))));
    let s2 = b.f(wv * (AT[1] + wv * (AT[3] + wv * (AT[5] + wv * (AT[7] + wv * AT[9])))));
    b.if_(id.lt_s(0), |b| b.ret(x - x * (s1 + s2)));
    b.sf(
        zv,
        fload(ATANHI_ADDR, id) - (x * (s1 + s2) - fload(ATANLO_ADDR, id) - x),
    );
    b.ret(select(sign, -zv, zv));
    b
}

/// The `|x| >= 7/16` argument reduction of `atan`.
fn atan_reduce(b: &mut B, x: VF, ix: VI, id: VI) {
    b.sf(x, x.fabs());
    b.if_else(
        ix.lt_u(0x3ff3_0000),
        |b| {
            b.if_else(
                ix.lt_u(0x3fe6_0000),
                |b| {
                    b.sf(x, (2.0 * x - 1.0) / (2.0 + x));
                    b.si(id, 0);
                },
                |b| {
                    b.sf(x, (x - 1.0) / (x + 1.0));
                    b.si(id, 1);
                },
            );
        },
        |b| {
            b.if_else(
                ix.lt_u(0x4003_8000),
                |b| {
                    b.sf(x, (x - 1.5) / (1.0 + 1.5 * x));
                    b.si(id, 2);
                },
                |b| {
                    b.sf(x, -1.0 / x);
                    b.si(id, 3);
                },
            );
        },
    );
}

const PI: f64 = c(0x4009_21fb_5444_2d18);
const PI_LO: f64 = c(0x3ca1_a626_3314_5c07);

/// `atan2(y, x)` for finite non-zero arguments with `m` the sign quadrant.
fn atan2_general(b: &mut B, yv: VF, x: VF, ix: VI, abs_hy: VI, mv: VI) {
    b.if_(
        (ix + (64 << 20)).lt_u(abs_hy) | abs_hy.eq(0x7ff0_0000),
        |b| {
            b.ret(select(mv & 1, -PI / 2.0, PI / 2.0));
        },
    );
    let zv = b.f(0.0);
    b.if_((mv & 2).eqz() | (abs_hy + (64 << 20)).ge_u(ix), |b| {
        b.sf(zv, call1(H::Atan, (yv / x).fabs()));
    });
    b.if_(mv.eqz(), |b| b.ret(zv));
    b.if_(mv.eq(1), |b| b.ret(-zv));
    b.if_(mv.eq(2), |b| b.ret(PI - (zv - PI_LO)));
    b.ret(zv - PI_LO - PI);
}

/// `atan2(y, x)`.
fn emit_atan2() -> B {
    let mut b = B::new(2);
    let (yv, x) = (VF(0), VF(1));
    b.if_(x.ne(x) | yv.ne(yv), |b| b.ret(x + yv));
    let hx = b.i(x.hi());
    let lx = b.i(x.lo());
    let hy = b.i(yv.hi());
    let ly = b.i(yv.lo());
    b.if_(((hx - 0x3ff0_0000) | lx).eqz(), |b| {
        b.ret(call1(H::Atan, yv));
    });
    let mv = b.i((hy.shr_u(31) & 1) | (hx.shr_u(30) & 2));
    let ix = b.i(hx & 0x7fff_ffff);
    let abs_hy = b.i(hy & 0x7fff_ffff);
    b.if_((abs_hy | ly).eqz(), |b| {
        b.if_(mv.lt_u(2), |b| b.ret(yv));
        b.ret(select(mv.eq(2), PI, -PI));
    });
    b.if_((ix | lx).eqz(), |b| {
        b.ret(select(mv & 1, -PI / 2.0, PI / 2.0));
    });
    b.if_(ix.eq(0x7ff0_0000), |b| {
        b.if_(abs_hy.eq(0x7ff0_0000), |b| {
            b.if_(mv.eqz(), |b| b.ret(PI / 4.0));
            b.if_(mv.eq(1), |b| b.ret(-PI / 4.0));
            b.if_(mv.eq(2), |b| b.ret(3.0 * PI / 4.0));
            b.ret(-3.0 * PI / 4.0);
        });
        b.if_(mv.eqz(), |b| b.ret(0.0));
        b.if_(mv.eq(1), |b| b.ret(-0.0));
        b.ret(select(mv.eq(2), PI, -PI));
    });
    atan2_general(&mut b, yv, x, ix, abs_hy, mv);
    b
}

/// `exp(x)/2` for `x >= log(DBL_MAX)` (`expo2` / `k_expo2`).
fn emit_expo2() -> B {
    const K_LN2: f64 = c(0x4096_2066_151a_dd8b);
    const SCALE: f64 = c(0x7fc0_0000_0000_0000);
    let mut b = B::new(1);
    let x = VF(0);
    b.ret(call1(H::Exp, x - K_LN2) * SCALE * SCALE);
    b
}

/// `sinh(x)`.
fn emit_sinh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let hv = b.f(select(x.bits().shr_u(63).wrap(), -0.5, 0.5));
    let absx = b.f(x.fabs());
    let wv = b.i(absx.hi());
    b.if_(wv.lt_u(0x4086_2e42), |b| {
        let tv = b.f(call1(H::Expm1, absx));
        b.if_(wv.lt_u(0x3ff0_0000), |b| {
            b.if_(wv.lt_u(0x3ff0_0000 - (26 << 20)), |b| b.ret(x));
            b.ret(hv * (2.0 * tv - tv * tv / (tv + 1.0)));
        });
        b.ret(hv * (tv + tv / (tv + 1.0)));
    });
    b.ret(2.0 * hv * call1(H::Expo2, absx));
    b
}

/// `cosh(x)`.
fn emit_cosh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    b.sf(x, x.fabs());
    let wv = b.i(x.hi());
    b.if_(wv.lt_u(0x3fe6_2e42), |b| {
        b.if_(wv.lt_u(0x3ff0_0000 - (26 << 20)), |b| b.ret(1.0));
        let tv = b.f(call1(H::Expm1, x));
        b.ret(1.0 + tv * tv / (2.0 * (1.0 + tv)));
    });
    b.if_(wv.lt_u(0x4086_2e42), |b| {
        let tv = b.f(call1(H::Exp, x));
        b.ret(0.5 * (tv + 1.0 / tv));
    });
    b.ret(call1(H::Expo2, x));
    b
}

/// `tanh(x)`.
fn emit_tanh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let sign = b.i(x.bits().shr_u(63).wrap());
    b.sf(x, x.fabs());
    let wv = b.i(x.hi());
    let tv = b.f(x);
    b.if_else(
        wv.gt_u(0x3fe1_93ea),
        |b| {
            b.if_else(
                wv.gt_u(0x4034_0000),
                |b| b.sf(tv, 1.0 - 0.0 / x),
                |b| {
                    b.sf(tv, call1(H::Expm1, 2.0 * x));
                    b.sf(tv, 1.0 - 2.0 / (tv + 2.0));
                },
            );
        },
        |b| {
            b.if_else(
                wv.gt_u(0x3fd0_58ae),
                |b| {
                    b.sf(tv, call1(H::Expm1, 2.0 * x));
                    b.sf(tv, tv / (tv + 2.0));
                },
                |b| {
                    b.if_(wv.ge_u(0x0010_0000), |b| {
                        b.sf(tv, call1(H::Expm1, -2.0 * x));
                        b.sf(tv, -tv / (tv + 2.0));
                    });
                },
            );
        },
    );
    b.ret(select(sign, -tv, tv));
    b
}

/// `asinh(x)`.
fn emit_asinh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ev = b.i(exp_of(x));
    let sign = b.i(x.bits().shr_u(63).wrap());
    b.sf(x, x.fabs());
    b.if_else(
        ev.ge_u(0x3ff + 26),
        |b| b.sf(x, call1(H::Log, x) + LN2),
        |b| {
            b.if_else(
                ev.ge_u(0x3ff + 1),
                |b| {
                    b.sf(
                        x,
                        call1(H::Log, 2.0 * x + 1.0 / ((x * x + 1.0).fsqrt() + x)),
                    );
                },
                |b| {
                    b.if_(ev.ge_u(0x3ff - 26), |b| {
                        b.sf(
                            x,
                            call1(H::Log1p, x + x * x / ((x * x + 1.0).fsqrt() + 1.0)),
                        );
                    });
                },
            );
        },
    );
    b.ret(select(sign, -x, x));
    b
}

/// `acosh(x)`.
fn emit_acosh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ev = b.i(exp_of(x));
    b.if_(ev.lt_u(0x3ff + 1), |b| {
        b.ret(call1(
            H::Log1p,
            x - 1.0 + ((x - 1.0) * (x - 1.0) + 2.0 * (x - 1.0)).fsqrt(),
        ));
    });
    b.if_(ev.lt_u(0x3ff + 26), |b| {
        b.ret(call1(H::Log, 2.0 * x - 1.0 / (x + (x * x - 1.0).fsqrt())));
    });
    b.ret(call1(H::Log, x) + LN2);
    b
}

/// `atanh(x)`.
fn emit_atanh() -> B {
    let mut b = B::new(1);
    let x = VF(0);
    let ev = b.i(exp_of(x));
    let sign = b.i(x.bits().shr_u(63).wrap());
    let yv = b.f(x.fabs());
    b.if_else(
        ev.lt_u(0x3ff - 1),
        |b| {
            b.if_(ev.ge_u(0x3ff - 32), |b| {
                b.sf(
                    yv,
                    0.5 * call1(H::Log1p, 2.0 * yv + 2.0 * yv * yv / (1.0 - yv)),
                );
            });
        },
        |b| b.sf(yv, 0.5 * call1(H::Log1p, 2.0 * (yv / (1.0 - yv)))),
    );
    b.ret(select(sign, -yv, yv));
    b
}
