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

// --- Math.sumPrecise ---------------------------------------------------------

/// The `TypeError` of `Math.sumPrecise` over a non-number value.
const SUM_NOT_NUMBER: CalleeErrorText<'static> = [
    "Math.sumPrecise requires an iterable of numbers",
    "TypeError: Math.sumPrecise requires an iterable of numbers",
];

/// The static strings of `Math.sumPrecise`.
pub(super) const SUM_STRINGS: [&str; 2] = SUM_NOT_NUMBER;

/// The exact accumulator of `Math.sumPrecise`: a two's-complement fixed-point
/// integer in units of `2^-1074` (the least subnormal), one 32-bit limb per
/// i64 slot, least significant first. [`SUM_PAD`] zero slots below the
/// value let the rounding read two limbs under the leading one unchecked;
/// the [`SUM_TOP`] slot holds the sign. A finite double spans bits
/// `0..2098`, and `2^53` of them fit in `2151` bits, below the sign slot.
const SUM_SLOTS: i32 = 72;
/// The zero slots below the value.
const SUM_PAD: i32 = 2;
/// The sign slot (the last one).
const SUM_TOP: i32 = SUM_SLOTS - 1;
/// A 32-bit limb.
const LIMB_MASK: i64 = 0xFFFF_FFFF;

/// `sumPrecise` states (the proposal's `state`, `finite` meaning "a finite
/// value other than `-0` was seen").
const SUM_MINUS_ZERO: i32 = 0;
const SUM_FINITE: i32 = 1;
const SUM_PLUS_INF: i32 = 2;
const SUM_MINUS_INF: i32 = 3;
const SUM_NAN: i32 = 4;

/// The i64 memory operand of slot byte offset `offset`.
fn slot(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// `Math.sumPrecise(items)`: `GetIterator(items)`, then every value must be
/// a number (else close and `TypeError`); `NaN` and `+∞`/`-∞` (both: `NaN`)
/// dominate, `-0` is skipped, and the finite values are summed exactly
/// ([`add_exact`]) and rounded once ([`round_exact`]). No value (or only
/// `-0`) yields `-0`.
pub(super) fn emit_sum_precise(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (items, v) = (frame.i64(0), frame.i64(1));
    let (acc, state) = (frame.i32(0), frame.i32(1));
    let temps = [frame.i64(2), frame.i64(3), frame.x64(0), frame.x64(1)];
    let (cursor, bound) = (frame.i32(2), frame.i32(3));
    let rec = iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    frame.arg(function, 0);
    wasm!(function; LocalSet(items));
    emit_get_iterator(
        &IrNode::Local(items),
        &rec,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        I32Const(SUM_SLOTS * 8), Call(helpers.alloc), LocalTee(acc),
        I32Const(0), I32Const(SUM_SLOTS * 8), MemoryFill(0),
        I32Const(SUM_MINUS_ZERO), LocalSet(state),
    );
    iter::guarded(function, helpers, |function| {
        wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
        enter_frame(helpers);
        enter_frame(helpers);
        emit_iterator_step(&rec, function, scratch, strings, helpers);
        wasm!(function; I32Eqz, BrIf(1));
        emit_iterator_value(&rec, function, scratch, strings, helpers);
        wasm!(function; LocalSet(v));
        emit_is_number(function, v);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        enter_frame(helpers);
        throw_type_error(function, frame, SUM_NOT_NUMBER, strings, helpers);
        leave_frame(helpers);
        let inf = f64::INFINITY.to_bits().cast_signed();
        let minus_inf = f64::NEG_INFINITY.to_bits().cast_signed();
        let minus_zero = (-0.0f64).to_bits().cast_signed();
        wasm!(function;
            End,
            LocalGet(state), I32Const(SUM_NAN), I32Ne, If(BlockType::Empty),
            LocalGet(v), F64ReinterpretI64, LocalGet(v), F64ReinterpretI64, F64Ne,
            If(BlockType::Empty), I32Const(SUM_NAN), LocalSet(state), Else,
            LocalGet(v), I64Const(inf), I64Eq,
            If(BlockType::Empty),
            I32Const(SUM_NAN), I32Const(SUM_PLUS_INF),
            LocalGet(state), I32Const(SUM_MINUS_INF), I32Eq, Select, LocalSet(state),
            Else,
            LocalGet(v), I64Const(minus_inf), I64Eq,
            If(BlockType::Empty),
            I32Const(SUM_NAN), I32Const(SUM_MINUS_INF),
            LocalGet(state), I32Const(SUM_PLUS_INF), I32Eq, Select, LocalSet(state),
            Else,
            LocalGet(v), I64Const(minus_zero), I64Ne,
            LocalGet(state), I32Const(SUM_PLUS_INF), I32LtU, I32And,
            If(BlockType::Empty), I32Const(SUM_FINITE), LocalSet(state),
        );
        add_exact(function, acc, v, [cursor, bound], temps);
        wasm!(function; End, End, End, End, End);
        leave_frame(helpers);
        leave_frame(helpers);
        wasm!(function; Br(0), End, End);
    });
    wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    emit_iterator_close(
        &rec,
        CloseMode::Pending,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(state), I32Const(SUM_FINITE), I32Eq, If(BlockType::Result(ValType::F64)),
    );
    round_exact(function, acc, [cursor, bound, frame.x32(0)], temps);
    wasm!(function;
        F64ReinterpretI64,
        Else,
        F64Const(Ieee64::from(f64::INFINITY)), F64Const(Ieee64::from(f64::NEG_INFINITY)),
        LocalGet(state), I32Const(SUM_PLUS_INF), I32Eq, Select,
        F64Const(Ieee64::from(-0.0)), F64Const(Ieee64::from(f64::NAN)),
        LocalGet(state), I32Const(SUM_MINUS_ZERO), I32Eq, Select,
        LocalGet(state), I32Const(SUM_PLUS_INF), I32GeU, LocalGet(state), I32Const(SUM_MINUS_INF),
        I32LeU, I32And, Select,
        End,
    );
    emit_box_f64(function, helpers);
}

/// Add the finite number (other than `-0`) whose bits are in i64 `x` to the
/// accumulator at i32 `acc` exactly: its 53-bit significand `m` at bit `p`
/// (`p = E - 1` for a normal, `0` for a subnormal) lands as three 32-bit
/// digits (negated for a negative `x`) on the slots from `p / 32`, then the
/// carries ripple up (to the sign slot at most).
fn add_exact(
    function: &mut Function,
    acc: u32,
    bits: u32,
    [bit, at]: [u32; 2],
    [hi, lo, sign, sum]: [u32; 4],
) {
    let frac = (1i64 << 52) - 1;
    wasm!(function;
        LocalGet(bits), I64Const(52), I64ShrU, I64Const(0x7FF), I64And, I32WrapI64, LocalSet(bit),
        LocalGet(bits), I64Const(frac), I64And, LocalSet(hi),
        LocalGet(bit), If(BlockType::Empty),
        LocalGet(hi), I64Const(1i64 << 52), I64Or, LocalSet(hi),
        LocalGet(bit), I32Const(1), I32Sub, LocalSet(bit),
        End,
        LocalGet(bits), I64Const(63), I64ShrS, LocalSet(sign),
        LocalGet(hi), I64Const(LIMB_MASK), I64And,
        LocalGet(bit), I32Const(31), I32And, I64ExtendI32U, I64Shl, LocalSet(lo),
        LocalGet(hi), I64Const(32), I64ShrU,
        LocalGet(bit), I32Const(31), I32And, I64ExtendI32U, I64Shl, LocalSet(hi),
        LocalGet(acc), LocalGet(bit), I32Const(5), I32ShrU, I32Const(SUM_PAD), I32Add,
        I32Const(3), I32Shl, I32Add, LocalSet(at),
    );
    // The three digits: `lo` low half, `lo` high half + `hi` low half, `hi`
    // high half; each `(d ^ s) - s` (negated when `x < 0`).
    let digits: [&dyn Fn(&mut Function); 3] = [
        &|function: &mut Function| wasm!(function; LocalGet(lo), I64Const(LIMB_MASK), I64And),
        &|function: &mut Function| {
            wasm!(function;
                LocalGet(lo), I64Const(32), I64ShrU, LocalGet(hi), I64Const(LIMB_MASK), I64And, I64Add,
            );
        },
        &|function: &mut Function| wasm!(function; LocalGet(hi), I64Const(32), I64ShrU),
    ];
    for (k, digit) in (0u64..).zip(digits) {
        wasm!(function; LocalGet(at), LocalGet(at), I64Load(slot(8 * k)));
        digit(function);
        wasm!(function; LocalGet(sign), I64Xor, LocalGet(sign), I64Sub, I64Add, I64Store(slot(8 * k)));
    }
    // Ripple: past the three digits with no carry left, or at the sign slot
    // (which keeps the sign, unmasked).
    wasm!(function;
        LocalGet(at), I32Const(24), I32Add, LocalSet(bit),
        I64Const(0), LocalSet(sign),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(acc), I32Const(SUM_TOP * 8), I32Add, I32Eq,
        If(BlockType::Empty),
        LocalGet(at), LocalGet(at), I64Load(slot(0)), LocalGet(sign), I64Add, I64Store(slot(0)),
        Br(2),
        End,
        LocalGet(at), I64Load(slot(0)), LocalGet(sign), I64Add, LocalSet(sum),
        LocalGet(at), LocalGet(sum), I64Const(LIMB_MASK), I64And, I64Store(slot(0)),
        LocalGet(sum), I64Const(32), I64ShrS, LocalSet(sign),
        LocalGet(at), I32Const(8), I32Add, LocalTee(at),
        LocalGet(bit), I32GeU, LocalGet(sign), I64Eqz, I32And, BrIf(1),
        Br(0),
        End, End,
    );
}

/// Push the bits of the double nearest (ties to even) to the accumulator at
/// i32 `acc`: negate a negative value, find the leading slot `H` and the
/// bit length `L`; `L <= 53` is exact (the integer is the subnormal /
/// smallest-normal bit pattern itself), else the 64 bits under the leading
/// one give the significand `M`, its round bit and part of the sticky bits
/// (the rest: the slots below), and the result is `(L - 53) << 52 + M`
/// (a carry out of `M` bumps the exponent; at the top, `+∞`).
fn round_exact(
    function: &mut Function,
    acc: u32,
    [at, width, neg]: [u32; 3],
    [window, len, carry, sticky]: [u32; 4],
) {
    let top = u64::try_from(SUM_TOP * 8).unwrap_or(0);
    let inf = f64::INFINITY.to_bits().cast_signed();
    wasm!(function;
        LocalGet(acc), I64Load(slot(top)), I64Const(0), I64LtS, LocalTee(neg),
        If(BlockType::Empty),
        LocalGet(acc), LocalSet(at), I64Const(0), LocalSet(carry),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(carry), LocalGet(at), I64Load(slot(0)), I64Sub, LocalSet(len),
        LocalGet(at), LocalGet(len), I64Const(LIMB_MASK), I64And, I64Store(slot(0)),
        LocalGet(len), I64Const(32), I64ShrS, LocalSet(carry),
        LocalGet(at), LocalGet(acc), I32Const(SUM_TOP * 8), I32Add, I32Eq, BrIf(1),
        LocalGet(at), I32Const(8), I32Add, LocalSet(at),
        Br(0),
        End, End,
        End,
        // The leading slot (`H`); none above the padding: `+0`.
        LocalGet(acc), I32Const(SUM_TOP * 8), I32Add, LocalSet(at),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(acc), I32Const(SUM_PAD * 8), I32Add, I32LtU, BrIf(1),
        LocalGet(at), I64Load(slot(0)), I64Const(0), I64Ne, BrIf(1),
        LocalGet(at), I32Const(8), I32Sub, LocalSet(at),
        Br(0),
        End, End,
        LocalGet(at), LocalGet(acc), I32Const(SUM_PAD * 8), I32Add, I32LtU,
        If(BlockType::Result(ValType::I64)), I64Const(0), Else,
        // `b`: the leading slot's bit length; `L` in `t`.
        I32Const(64), LocalGet(at), I64Load(slot(0)), I64Clz, I32WrapI64, I32Sub, LocalSet(width),
        LocalGet(at), LocalGet(acc), I32Sub, I32Const(3), I32ShrU, I32Const(SUM_PAD), I32Sub,
        I32Const(5), I32Shl, LocalGet(width), I32Add, I64ExtendI32U, LocalTee(len),
        I64Const(53), I64LeU,
        If(BlockType::Result(ValType::I64)),
        LocalGet(acc), I64Load(slot(8 * 2)),
        LocalGet(acc), I64Load(slot(8 * 3)), I64Const(32), I64Shl, I64Or,
        Else,
        // The 64 bits under the leading one.
        LocalGet(at), I64Load(slot(0)), I64Const(64), LocalGet(width), I64ExtendI32U, I64Sub, I64Shl,
        LocalGet(at), I32Const(8), I32Sub, I64Load(slot(0)),
        I64Const(32), LocalGet(width), I64ExtendI32U, I64Sub, I64Shl, I64Or,
        LocalGet(at), I32Const(16), I32Sub, I64Load(slot(0)), LocalGet(width), I64ExtendI32U, I64ShrU,
        I64Or, LocalSet(window),
        // Sticky: the 10 bits under the round bit, the low `b` bits of the
        // slot two under `H`, and every slot below it.
        LocalGet(window), I64Const(0x3FF), I64And,
        LocalGet(at), I32Const(16), I32Sub, I64Load(slot(0)),
        I64Const(1), LocalGet(width), I64ExtendI32U, I64Shl, I64Const(1), I64Sub, I64And,
        I64Or, LocalSet(sticky),
        LocalGet(at), I32Const(24), I32Sub, LocalSet(width),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(width), LocalGet(acc), I32LtU, BrIf(1),
        LocalGet(sticky), LocalGet(width), I64Load(slot(0)), I64Or, LocalSet(sticky),
        LocalGet(width), I32Const(8), I32Sub, LocalSet(width),
        Br(0),
        End, End,
        // `M` (53 bits), rounded to nearest, ties to even.
        LocalGet(window), I64Const(10), I64ShrU, I64Const(1), I64And, I32WrapI64,
        LocalGet(sticky), I64Const(0), I64Ne,
        LocalGet(window), I64Const(11), I64ShrU, I64Const(1), I64And, I32WrapI64, I32Or, I32And,
        I64ExtendI32U, LocalGet(window), I64Const(11), I64ShrU, I64Add,
        LocalGet(len), I64Const(53), I64Sub, I64Const(52), I64Shl, I64Add, LocalTee(window),
        I64Const(inf), LocalGet(window), I64Const(inf), I64LtU, Select,
        End,
        End,
        I64Const(i64::MIN), I64Const(0), LocalGet(neg), Select, I64Or,
    );
}
