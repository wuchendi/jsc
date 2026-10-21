//! `%String.prototype%` methods. A generic method takes
//! `ToString(RequireObjectCoercible(this))`, coerces its arguments in the guest
//! (a user `toString` / `valueOf` may run, and throw), then runs its host
//! `__method` op; `toString` / `valueOf` are `thisStringValue`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// How a method coerces one argument before its host op.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Coerce {
    /// Unused: passed as is.
    None,
    /// `ToString` (`undefined` becomes `"undefined"`).
    String,
    /// `ToString` unless `undefined` (which selects the host default).
    OptString,
    /// `ToNumber` unless `undefined` (which selects the host default).
    Number,
}

/// The coercions of `op`'s first two arguments.
fn coercions(op: StringOp) -> [Coerce; 2] {
    match op {
        StringOp::IndexOf | StringOp::Includes | StringOp::StartsWith | StringOp::EndsWith => {
            [Coerce::String, Coerce::Number]
        }
        StringOp::Slice | StringOp::Substring => [Coerce::Number, Coerce::Number],
        StringOp::CharAt | StringOp::Repeat => [Coerce::Number, Coerce::None],
        StringOp::PadStart | StringOp::PadEnd => [Coerce::Number, Coerce::OptString],
        StringOp::Split => [Coerce::OptString, Coerce::Number],
        StringOp::ToUpperCase
        | StringOp::ToLowerCase
        | StringOp::Trim
        | StringOp::TrimStart
        | StringOp::TrimEnd => [Coerce::None, Coerce::None],
    }
}

/// Whether the call-site arguments `args` of `op` are already of the types
/// its coercions produce (string / number literals; a missing argument is
/// `undefined`), so that on a string receiver the builtin is exactly its host
/// op: no coercion runs, and `repeat`'s count is a valid one.
pub(in crate::emit) fn host_ready_args(op: StringOp, args: &[IrNode]) -> bool {
    let ready = |coerce: Coerce, arg: Option<&IrNode>| match (coerce, arg) {
        (Coerce::None, _)
        | (Coerce::OptString | Coerce::Number, None)
        | (Coerce::String | Coerce::OptString, Some(IrNode::Str(_))) => true,
        (Coerce::Number, Some(IrNode::Num(n))) => {
            op != StringOp::Repeat || (*n >= 0.0 && n.is_finite())
        }
        _ => false,
    };
    let [first, second] = coercions(op);
    ready(first, args.first()) && ready(second, args.get(1))
}

/// The `TypeError` text of `String.prototype.<name>` on `null`/`undefined`.
macro_rules! nullish {
    ($name:literal) => {
        [
            concat!("String.prototype.", $name, " called on null or undefined"),
            concat!(
                "TypeError: String.prototype.",
                $name,
                " called on null or undefined"
            ),
        ]
    };
}

/// The `TypeError` of `op` on a `null`/`undefined` receiver.
fn nullish_text(op: StringOp) -> CalleeErrorText<'static> {
    match op {
        StringOp::IndexOf => nullish!("indexOf"),
        StringOp::Includes => nullish!("includes"),
        StringOp::Slice => nullish!("slice"),
        StringOp::ToUpperCase => nullish!("toUpperCase"),
        StringOp::ToLowerCase => nullish!("toLowerCase"),
        StringOp::CharAt => nullish!("charAt"),
        StringOp::Trim => nullish!("trim"),
        StringOp::TrimStart => nullish!("trimStart"),
        StringOp::TrimEnd => nullish!("trimEnd"),
        StringOp::Repeat => nullish!("repeat"),
        StringOp::StartsWith => nullish!("startsWith"),
        StringOp::EndsWith => nullish!("endsWith"),
        StringOp::PadStart => nullish!("padStart"),
        StringOp::PadEnd => nullish!("padEnd"),
        StringOp::Substring => nullish!("substring"),
        StringOp::Split => nullish!("split"),
    }
}

/// Every op of this family.
const OPS: [StringOp; 16] = [
    StringOp::IndexOf,
    StringOp::Includes,
    StringOp::Slice,
    StringOp::ToUpperCase,
    StringOp::ToLowerCase,
    StringOp::CharAt,
    StringOp::Trim,
    StringOp::TrimStart,
    StringOp::TrimEnd,
    StringOp::Repeat,
    StringOp::StartsWith,
    StringOp::EndsWith,
    StringOp::PadStart,
    StringOp::PadEnd,
    StringOp::Substring,
    StringOp::Split,
];

/// `thisStringValue` failures.
const NOT_STRING_TO_STRING: CalleeErrorText<'static> = [
    "String.prototype.toString requires that 'this' be a String",
    "TypeError: String.prototype.toString requires that 'this' be a String",
];
const NOT_STRING_VALUE_OF: CalleeErrorText<'static> = [
    "String.prototype.valueOf requires that 'this' be a String",
    "TypeError: String.prototype.valueOf requires that 'this' be a String",
];

/// The `RangeError` text prefixes of `repeat` with a negative or infinite
/// count (the count follows).
const REPEAT_COUNT: [&str; 2] = ["Invalid count value: ", "RangeError: Invalid count value: "];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    OPS.into_iter()
        .flat_map(nullish_text)
        .chain(NOT_STRING_TO_STRING)
        .chain(NOT_STRING_VALUE_OF)
        .chain(REPEAT_COUNT)
}

/// `String.prototype.toString()` / `valueOf()`: `thisStringValue(this)`.
pub(super) fn emit_this_value(
    value_of: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let text = if value_of {
        NOT_STRING_VALUE_OF
    } else {
        NOT_STRING_TO_STRING
    };
    let s = frame.i64(0);
    this_primitive(
        function,
        frame,
        s,
        abi::CLASS_STRING,
        emit_is_string,
        text,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(s));
}

/// The generic `String.prototype.<op>(a0, a1)`: `ToString(this)` (a
/// `null`/`undefined` receiver throws), the argument coercions of
/// [`coercions`] in order, `repeat`'s count check, then the host op.
pub(super) fn emit_method(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, a0, a1) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let count = frame.f64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(s));
    is_string(function, s);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_nullish(function, s);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, nullish_text(op), strings, helpers);
    wasm!(function; End);
    to_string(function, s, helpers);
    wasm!(function; End);
    for (i, (coerce, out)) in (0..).zip(coercions(op).into_iter().zip([a0, a1])) {
        frame.arg(function, i);
        wasm!(function; LocalSet(out));
        // Each coercion is skipped for a value already of its type.
        let coerce_string = |function: &mut Function| {
            is_string(function, out);
            wasm!(function; I32Eqz, If(BlockType::Empty));
            to_string(function, out, helpers);
            wasm!(function; End);
        };
        match coerce {
            Coerce::None => {}
            Coerce::String => coerce_string(function),
            Coerce::OptString | Coerce::Number => {
                wasm!(function;
                    LocalGet(out), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty),
                );
                if coerce == Coerce::Number {
                    emit_is_number(function, out);
                    wasm!(function;
                        I32Eqz, If(BlockType::Empty),
                        LocalGet(out), Call(helpers.to_number), LocalSet(out),
                    );
                    return_if_pending(function);
                    wasm!(function; End);
                } else {
                    coerce_string(function);
                }
                wasm!(function; End);
            }
        }
    }
    if op == StringOp::Repeat {
        // `ToIntegerOrInfinity(count)` below 0 or `+Infinity` is a RangeError.
        wasm!(function;
            LocalGet(a0), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty),
            LocalGet(a0), F64ReinterpretI64, F64Trunc, LocalTee(count),
            F64Const(Ieee64::from(0.0)), F64Lt,
            LocalGet(count), F64Const(Ieee64::from(f64::INFINITY)), F64Eq, I32Or,
            If(BlockType::Empty),
        );
        let text = |prefix: &str| IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str(prefix.to_owned())),
            rhs: Box::new(IrNode::Local(a0)),
        };
        let error = IrNode::error_object(
            ErrorKind::RangeError,
            Some(text(REPEAT_COUNT[0])),
            text(REPEAT_COUNT[1]),
        );
        emit_throw_error(&error, function, frame.scratch(), strings, helpers);
        wasm!(function; End, End);
    }
    wasm!(function;
        LocalGet(s), I32Const(op.flag()), LocalGet(a0), LocalGet(a1), Call(helpers.method),
    );
}

/// Push `1` when the jsval in `local` is a string: the heap tag in its top 16
/// bits, then the kind byte.
fn is_string(function: &mut Function, local: u32) {
    wasm!(function;
        LocalGet(local), I64Const(48), I64ShrU, I64Const(HEAP_BITS >> 48), I64Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64,
        I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }),
        I32Const(TYPE_STRING), I32Eq,
        Else, I32Const(0), End,
    );
}

/// Replace the jsval in i64 local `v` by its `ToString` (a heap string), returning when a
/// user `toString` throws.
pub(super) fn to_string(function: &mut Function, v: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(v), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(v),
    );
    return_if_pending(function);
}
