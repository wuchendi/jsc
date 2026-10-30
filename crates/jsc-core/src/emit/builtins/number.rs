//! `%Number.prototype%` and `%Boolean.prototype%` methods: `thisNumberValue`
//! / `thisBooleanValue` of a primitive or its wrapper object.

#[allow(clippy::wildcard_imports)]
use super::*;

/// `thisNumberValue` / `thisBooleanValue` failures.
const NOT_NUMBER_TO_STRING: CalleeErrorText<'static> = [
    "Number.prototype.toString requires that 'this' be a Number",
    "TypeError: Number.prototype.toString requires that 'this' be a Number",
];
const NOT_NUMBER_VALUE_OF: CalleeErrorText<'static> = [
    "Number.prototype.valueOf requires that 'this' be a Number",
    "TypeError: Number.prototype.valueOf requires that 'this' be a Number",
];
const NOT_BOOLEAN_TO_STRING: CalleeErrorText<'static> = [
    "Boolean.prototype.toString requires that 'this' be a Boolean",
    "TypeError: Boolean.prototype.toString requires that 'this' be a Boolean",
];
const NOT_BOOLEAN_VALUE_OF: CalleeErrorText<'static> = [
    "Boolean.prototype.valueOf requires that 'this' be a Boolean",
    "TypeError: Boolean.prototype.valueOf requires that 'this' be a Boolean",
];

/// The `RangeError` of `Number.prototype.toString` with a radix outside
/// `2..=36`.
const BAD_RADIX: [&str; 2] = [
    "toString() radix must be between 2 and 36",
    "RangeError: toString() radix must be between 2 and 36",
];

/// The `Boolean.prototype.toString` results.
const TRUE_TEXT: &str = "true";
const FALSE_TEXT: &str = "false";

/// A `Number.prototype` digit-formatting method: its name (for the
/// receiver `TypeError`), host op and the `RangeError` text with the digit
/// range it accepts.
#[derive(Clone, Copy)]
pub(super) enum NumberFormat {
    Fixed,
    Exponential,
    Precision,
}

/// The receiver `TypeError` texts of the formatting methods.
const NOT_NUMBER_FORMAT: [CalleeErrorText<'static>; 4] = [
    [
        "Number.prototype.toFixed requires that 'this' be a Number",
        "TypeError: Number.prototype.toFixed requires that 'this' be a Number",
    ],
    [
        "Number.prototype.toExponential requires that 'this' be a Number",
        "TypeError: Number.prototype.toExponential requires that 'this' be a Number",
    ],
    [
        "Number.prototype.toPrecision requires that 'this' be a Number",
        "TypeError: Number.prototype.toPrecision requires that 'this' be a Number",
    ],
    [
        "Number.prototype.toLocaleString requires that 'this' be a Number",
        "TypeError: Number.prototype.toLocaleString requires that 'this' be a Number",
    ],
];

/// The digit-range `RangeError` texts of the formatting methods.
const BAD_DIGITS: [[&str; 2]; 3] = [
    [
        "toFixed() digits argument must be between 0 and 100",
        "RangeError: toFixed() digits argument must be between 0 and 100",
    ],
    [
        "toExponential() argument must be between 0 and 100",
        "RangeError: toExponential() argument must be between 0 and 100",
    ],
    [
        "toPrecision() argument must be between 1 and 100",
        "RangeError: toPrecision() argument must be between 1 and 100",
    ],
];

impl NumberFormat {
    /// The index of this method in the text tables.
    fn index(self) -> usize {
        match self {
            Self::Fixed => 0,
            Self::Exponential => 1,
            Self::Precision => 2,
        }
    }

    /// The host `__method` op.
    fn op(self) -> i32 {
        match self {
            Self::Fixed => abi::METHOD_OP_NUMBER_TO_FIXED,
            Self::Exponential => abi::METHOD_OP_NUMBER_TO_EXPONENTIAL,
            Self::Precision => abi::METHOD_OP_NUMBER_TO_PRECISION,
        }
    }

    /// The smallest accepted digit count (the largest is 100).
    fn min(self) -> f64 {
        match self {
            Self::Precision => 1.0,
            Self::Fixed | Self::Exponential => 0.0,
        }
    }
}

/// The static strings of this family.
pub(super) const STRINGS: [&str; 26] = [
    NOT_NUMBER_TO_STRING[0],
    NOT_NUMBER_TO_STRING[1],
    NOT_NUMBER_VALUE_OF[0],
    NOT_NUMBER_VALUE_OF[1],
    NOT_BOOLEAN_TO_STRING[0],
    NOT_BOOLEAN_TO_STRING[1],
    NOT_BOOLEAN_VALUE_OF[0],
    NOT_BOOLEAN_VALUE_OF[1],
    BAD_RADIX[0],
    BAD_RADIX[1],
    TRUE_TEXT,
    FALSE_TEXT,
    NOT_NUMBER_FORMAT[0][0],
    NOT_NUMBER_FORMAT[0][1],
    NOT_NUMBER_FORMAT[1][0],
    NOT_NUMBER_FORMAT[1][1],
    NOT_NUMBER_FORMAT[2][0],
    NOT_NUMBER_FORMAT[2][1],
    NOT_NUMBER_FORMAT[3][0],
    NOT_NUMBER_FORMAT[3][1],
    BAD_DIGITS[0][0],
    BAD_DIGITS[0][1],
    BAD_DIGITS[1][0],
    BAD_DIGITS[1][1],
    BAD_DIGITS[2][0],
    BAD_DIGITS[2][1],
];

/// Push `1` when the jsval in `local` is a boolean.
fn is_boolean(function: &mut Function, local: u32) {
    wasm!(function;
        LocalGet(local), I64Const(TRUE_BITS), I64Eq,
        LocalGet(local), I64Const(FALSE_BITS), I64Eq, I32Or,
    );
}

/// `Number.prototype.valueOf()`: `thisNumberValue(this)`.
pub(super) fn emit_number_value_of(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let x = frame.i64(0);
    let text = NOT_NUMBER_VALUE_OF;
    this_primitive(
        function,
        frame,
        x,
        abi::CLASS_NUMBER,
        emit_is_number,
        text,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(x));
}

/// `Number.prototype.toString(radix)`: `thisNumberValue(this)`, then the
/// radix (`10` when `undefined`, else `ToIntegerOrInfinity`, a `RangeError`
/// outside `2..=36`); radix 10 is `ToString`, any other the host's radix
/// rendering.
pub(super) fn emit_number_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (x, radix_v) = (frame.i64(0), frame.i64(1));
    let radix = frame.f64(0);
    let text = NOT_NUMBER_TO_STRING;
    this_primitive(
        function,
        frame,
        x,
        abi::CLASS_NUMBER,
        emit_is_number,
        text,
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(radix_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
        F64Const(Ieee64::from(10.0)), LocalSet(radix),
        Else,
        LocalGet(radix_v), Call(helpers.to_number), LocalSet(radix_v),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(radix_v), F64ReinterpretI64, F64Trunc, LocalSet(radix),
        End,
        LocalGet(radix), F64Const(Ieee64::from(2.0)), F64Ge,
        LocalGet(radix), F64Const(Ieee64::from(36.0)), F64Le, I32And, I32Eqz,
        If(BlockType::Empty),
    );
    let error = IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(BAD_RADIX[0].into())),
        IrNode::Str(BAD_RADIX[1].into()),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
    wasm!(function;
        End,
        LocalGet(radix), F64Const(Ieee64::from(10.0)), F64Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(x), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        Else,
        LocalGet(x), I32Const(abi::METHOD_OP_NUMBER_TO_STRING),
        LocalGet(radix), I64ReinterpretF64, I64Const(UNDEFINED_BITS), Call(helpers.method),
        End,
    );
}

/// Return `ToString` of the number jsval in `x`.
fn return_to_string(function: &mut Function, x: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(x), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Return,
    );
}

/// `Number.prototype.toLocaleString()`: `thisNumberValue(this)`, then
/// `ToString` (no locale data: the `toString()` rendering).
pub(super) fn emit_number_to_locale_string(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let x = frame.i64(0);
    let text = NOT_NUMBER_FORMAT[3];
    this_primitive(
        function,
        frame,
        x,
        abi::CLASS_NUMBER,
        emit_is_number,
        text,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(x), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `Number.prototype.toFixed(f)` / `toExponential(f)` / `toPrecision(p)`:
/// `thisNumberValue(this)`, the digit count `ToIntegerOrInfinity(arg)` (a
/// `toPrecision` with `undefined` is `ToString(x)`), then in spec order the
/// non-finite receiver's `ToString` and the digit-range `RangeError`
/// (`toFixed` checks the range first); the finite digits render on the host,
/// which gets `undefined` for a `toExponential` without digits.
pub(super) fn emit_number_format(
    format: NumberFormat,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (x, arg) = (frame.i64(0), frame.i64(1));
    let digits = frame.f64(0);
    let text = NOT_NUMBER_FORMAT[format.index()];
    this_primitive(
        function,
        frame,
        x,
        abi::CLASS_NUMBER,
        emit_is_number,
        text,
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    if matches!(format, NumberFormat::Precision) {
        wasm!(function; LocalGet(arg), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
        return_to_string(function, x, helpers);
        wasm!(function; End);
    }
    // `ToIntegerOrInfinity`: `trunc`, with NaN as 0.
    wasm!(function; LocalGet(arg), Call(helpers.to_number), LocalSet(frame.i64(2)));
    return_if_pending(function);
    wasm!(function;
        LocalGet(frame.i64(2)), F64ReinterpretI64, F64Trunc, LocalTee(digits),
        F64Const(Ieee64::from(0.0)),
        LocalGet(digits), LocalGet(digits), F64Eq,
        Select, LocalSet(digits),
    );
    let check_range = |function: &mut Function| {
        wasm!(function;
            LocalGet(digits), F64Const(Ieee64::from(format.min())), F64Ge,
            LocalGet(digits), F64Const(Ieee64::from(100.0)), F64Le, I32And, I32Eqz,
            If(BlockType::Empty),
        );
        let [message, stack] = BAD_DIGITS[format.index()];
        let error = IrNode::error_object(
            ErrorKind::RangeError,
            Some(IrNode::Str(message.into())),
            IrNode::Str(stack.into()),
        );
        emit_throw_error(&error, function, frame.scratch(), strings, helpers);
        wasm!(function; End);
    };
    if matches!(format, NumberFormat::Fixed) {
        check_range(function);
    }
    // A non-finite receiver is its `ToString`.
    wasm!(function;
        LocalGet(x), F64ReinterpretI64, LocalGet(x), F64ReinterpretI64, F64Sub,
        F64Const(Ieee64::from(0.0)), F64Eq, I32Eqz, If(BlockType::Empty),
    );
    return_to_string(function, x, helpers);
    wasm!(function; End);
    if !matches!(format, NumberFormat::Fixed) {
        check_range(function);
    }
    wasm!(function; LocalGet(x), I32Const(format.op()));
    if matches!(format, NumberFormat::Exponential) {
        wasm!(function;
            LocalGet(digits), I64ReinterpretF64, I64Const(UNDEFINED_BITS),
            LocalGet(arg), I64Const(UNDEFINED_BITS), I64Ne, Select,
        );
    } else {
        wasm!(function; LocalGet(digits), I64ReinterpretF64);
    }
    wasm!(function; I64Const(UNDEFINED_BITS), Call(helpers.method));
}

/// `Boolean.prototype.toString()` / `valueOf()`: `thisBooleanValue(this)`,
/// as `"true"`/`"false"` for `toString`.
pub(super) fn emit_boolean(
    value_of: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let b = frame.i64(0);
    let text = if value_of {
        NOT_BOOLEAN_VALUE_OF
    } else {
        NOT_BOOLEAN_TO_STRING
    };
    this_primitive(
        function,
        frame,
        b,
        abi::CLASS_BOOLEAN,
        is_boolean,
        text,
        strings,
        helpers,
    );
    if value_of {
        wasm!(function; LocalGet(b));
        return;
    }
    wasm!(function; LocalGet(b), I64Const(TRUE_BITS), I64Eq, If(BlockType::Result(ValType::I64)));
    push_str(function, strings, TRUE_TEXT);
    wasm!(function; Else);
    push_str(function, strings, FALSE_TEXT);
    wasm!(function; End);
}
