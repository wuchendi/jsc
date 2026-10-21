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

/// The static strings of this family.
pub(super) const STRINGS: [&str; 12] = [
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
    strings: &HashMap<String, i32>,
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
    strings: &HashMap<String, i32>,
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
        Some(IrNode::Str(BAD_RADIX[0].to_owned())),
        IrNode::Str(BAD_RADIX[1].to_owned()),
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

/// `Boolean.prototype.toString()` / `valueOf()`: `thisBooleanValue(this)`,
/// as `"true"`/`"false"` for `toString`.
pub(super) fn emit_boolean(
    value_of: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
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
