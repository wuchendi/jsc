//! `BigInt`, `BigInt.asIntN` / `BigInt.asUintN` and the `%BigInt.prototype%`
//! methods (DESIGN.md §6.10.7) over the `BigInt` runtime of `emit/bigint.rs`
//! (re-exported here for the sibling bodies that test for a `BigInt`).

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::BigIntOp;
use crate::emit::registry::Big;

pub(super) use crate::emit::bigint::{BIGINT_JSON, emit_is_bigint};

/// `new BigInt()`: `BigInt` has `[[Construct]]` only for `IsConstructor`.
const NOT_A_CONSTRUCTOR: CalleeErrorText<'static> = [
    "BigInt is not a constructor",
    "TypeError: BigInt is not a constructor",
];

/// `thisBigIntValue` failures.
const NOT_BIGINT_TO_STRING: CalleeErrorText<'static> = [
    "BigInt.prototype.toString requires that 'this' be a BigInt",
    "TypeError: BigInt.prototype.toString requires that 'this' be a BigInt",
];
const NOT_BIGINT_TO_LOCALE_STRING: CalleeErrorText<'static> = [
    "BigInt.prototype.toLocaleString requires that 'this' be a BigInt",
    "TypeError: BigInt.prototype.toLocaleString requires that 'this' be a BigInt",
];
const NOT_BIGINT_VALUE_OF: CalleeErrorText<'static> = [
    "BigInt.prototype.valueOf requires that 'this' be a BigInt",
    "TypeError: BigInt.prototype.valueOf requires that 'this' be a BigInt",
];

/// The `RangeError` of `ToIndex` (`asIntN` / `asUintN` bits).
const BAD_INDEX: [&str; 2] = [
    "Invalid value: not (convertible to) a safe integer",
    "RangeError: Invalid value: not (convertible to) a safe integer",
];

/// The pieces of the `NumberToBigInt` `RangeError`: `The number <x> cannot be
/// converted to a BigInt because it is not an integer`.
const NOT_INTEGER_PREFIX: &str = "The number ";
const NOT_INTEGER_SUFFIX: &str = " cannot be converted to a BigInt because it is not an integer";
const RANGE_ERROR_STACK_PREFIX: &str = "RangeError: ";

/// Every static string the bodies reference.
pub(super) const STRINGS: [&str; 15] = [
    NOT_A_CONSTRUCTOR[0],
    NOT_A_CONSTRUCTOR[1],
    NOT_BIGINT_TO_STRING[0],
    NOT_BIGINT_TO_STRING[1],
    NOT_BIGINT_TO_LOCALE_STRING[0],
    NOT_BIGINT_TO_LOCALE_STRING[1],
    NOT_BIGINT_VALUE_OF[0],
    NOT_BIGINT_VALUE_OF[1],
    BAD_INDEX[0],
    BAD_INDEX[1],
    number::BAD_RADIX[0],
    number::BAD_RADIX[1],
    NOT_INTEGER_PREFIX,
    NOT_INTEGER_SUFFIX,
    RANGE_ERROR_STACK_PREFIX,
];

/// Throw the static `RangeError` `[message, stack]` and return.
fn throw_range_error(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    [message, stack]: [&str; 2],
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let error = IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(message.into())),
        IrNode::Str(stack.into()),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
}

/// `BigInt(value)`: `new BigInt()` throws first; then `prim =
/// ToPrimitive(value, number)`, a number goes through `NumberToBigInt`
/// (`RangeError` for `NaN`, `±Infinity` and non-integers) and anything else
/// through `ToBigInt`.
pub(super) fn emit_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, message) = (frame.i64(0), frame.i64(1));
    emit_is_construct_call(function, frame.argc());
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, NOT_A_CONSTRUCTOR, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    emit_is_object(function, v);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(v), I32Const(convert::HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(v),
    );
    return_if_pending(function);
    wasm!(function; End);
    emit_is_number(function, v);
    wasm!(function;
        If(BlockType::Empty),
        // `x - trunc(x)` is `0` exactly for the finite integers.
        LocalGet(v), F64ReinterpretI64, LocalGet(v), F64ReinterpretI64, F64Trunc, F64Sub,
        F64Const(Ieee64::from(0.0)), F64Ne,
        If(BlockType::Empty),
        I32Const(static_key(strings, NOT_INTEGER_PREFIX)),
        LocalGet(v), Call(helpers.to_str()), Call(helpers.concat),
        I32Const(static_key(strings, NOT_INTEGER_SUFFIX)), Call(helpers.concat),
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(message),
    );
    let stack = IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Str(RANGE_ERROR_STACK_PREFIX.into())),
        rhs: Box::new(IrNode::Local(message)),
    };
    let error = IrNode::error_object(ErrorKind::RangeError, Some(IrNode::Local(message)), stack);
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
    wasm!(function;
        End,
        LocalGet(v), F64ReinterpretI64, Call(helpers.bigint(Big::FromF64)), Return,
        End,
        LocalGet(v), Call(helpers.bigint(Big::ToBigInt)),
    );
}

/// Emit the body of the `BigInt` static or `%BigInt.prototype%` method `op`.
pub(super) fn emit(
    op: BigIntOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        BigIntOp::AsIntN | BigIntOp::AsUintN => {
            emit_as_n(op == BigIntOp::AsIntN, function, frame, strings, helpers);
        }
        BigIntOp::ToString => emit_to_string(function, frame, strings, helpers),
        BigIntOp::ToLocaleString | BigIntOp::ValueOf => {
            let x = frame.i64(0);
            let text = if op == BigIntOp::ValueOf {
                NOT_BIGINT_VALUE_OF
            } else {
                NOT_BIGINT_TO_LOCALE_STRING
            };
            this_bigint(function, frame, x, text, strings, helpers);
            if op == BigIntOp::ValueOf {
                wasm!(function; LocalGet(x));
            } else {
                // No locale data: the `toString()` rendering.
                wasm!(function;
                    LocalGet(x), I32Const(10), Call(helpers.bigint(Big::ToString)),
                    I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
                );
            }
        }
    }
}

/// `thisBigIntValue(this)` into i64 `out`: a `BigInt`, or the
/// `[[BigIntData]]` of a `CLASS_BIGINT` wrapper, else throw `text`.
fn this_bigint(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    out: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    this_primitive(
        function,
        frame,
        out,
        abi::CLASS_BIGINT,
        |f, l| emit_is_bigint(f, l),
        text,
        strings,
        helpers,
    );
}

/// `ToIntegerOrInfinity` of the jsval in i64 `v` into f64 `out` (`ToNumber`
/// may throw: return then; `NaN` and `-0` are `+0`).
fn to_integer_or_infinity(function: &mut FnBuilder<'_>, v: u32, out: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(v), Call(helpers.to_number), LocalSet(v));
    return_if_pending(function);
    wasm!(function;
        LocalGet(v), F64ReinterpretI64, F64Trunc, LocalTee(out),
        LocalGet(out), F64Eq,
        If(BlockType::Empty),
        LocalGet(out), F64Const(Ieee64::from(0.0)), F64Add, LocalSet(out),
        Else,
        F64Const(Ieee64::from(0.0)), LocalSet(out),
        End,
    );
}

/// `BigInt.asIntN(bits, bigint)` / `BigInt.asUintN(bits, bigint)`:
/// `bits = ToIndex(bits)` (`RangeError` below `0` or above `2^53 - 1`), then
/// `bigint = ToBigInt(bigint)`, then `__bigint_as_n`.
fn emit_as_n(
    signed: bool,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let frame = frame.snapshot(function, 2);
    let (bits_v, big) = (frame.i64(0), frame.i64(1));
    let bits = frame.f64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(bits_v));
    to_integer_or_infinity(function, bits_v, bits, helpers);
    wasm!(function;
        LocalGet(bits), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(bits), F64Const(Ieee64::from(9_007_199_254_740_991.0)), F64Gt, I32Or,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, BAD_INDEX, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 1);
    wasm!(function; Call(helpers.bigint(Big::ToBigInt)), LocalSet(big));
    return_if_pending(function);
    wasm!(function;
        LocalGet(bits), LocalGet(big), I32Const(i32::from(signed)),
        Call(helpers.bigint(Big::AsN)),
    );
}

/// `BigInt.prototype.toString(radix)`: `thisBigIntValue(this)`, then the
/// radix (`10` when `undefined`, else `ToIntegerOrInfinity`, a `RangeError`
/// outside `2..=36`), then `__bigint_to_string`.
fn emit_to_string(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (x, radix_v) = (frame.i64(0), frame.i64(1));
    let radix = frame.f64(0);
    this_bigint(function, frame, x, NOT_BIGINT_TO_STRING, strings, helpers);
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(radix_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
        F64Const(Ieee64::from(10.0)), LocalSet(radix),
        Else,
    );
    to_integer_or_infinity(function, radix_v, radix, helpers);
    wasm!(function;
        End,
        LocalGet(radix), F64Const(Ieee64::from(2.0)), F64Ge,
        LocalGet(radix), F64Const(Ieee64::from(36.0)), F64Le, I32And, I32Eqz,
        If(BlockType::Empty),
    );
    throw_range_error(function, frame, number::BAD_RADIX, strings, helpers);
    wasm!(function;
        End,
        LocalGet(x), LocalGet(radix), I32TruncF64U, Call(helpers.bigint(Big::ToString)),
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
