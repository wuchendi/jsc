//! `%Date.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of a `Date.prototype` method on a non-Date receiver.
const NOT_DATE: CalleeErrorText<'static> = [
    "this is not a Date object.",
    "TypeError: this is not a Date object.",
];

/// The static strings of this family (the method texts included).
pub(super) const STRINGS: [&str; 8] = [
    NOT_DATE[0],
    NOT_DATE[1],
    INVALID_DATE,
    INVALID_TIME_VALUE[0],
    INVALID_TIME_VALUE[1],
    DATE_WEEKDAYS,
    DATE_MONTHS,
    DATE_TAIL,
];

/// Read `this` into `this` and throw unless it is a Date.
fn this_date(
    function: &mut Function,
    frame: Frame,
    this: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function;
        I32Const(TYPE_DATE), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, NOT_DATE, strings, helpers);
    wasm!(function; End);
}

/// The Date method `op`: its field of the Date receiver.
pub(super) fn emit_method(
    op: DateOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    this_date(function, frame, this, strings, helpers);
    let node = IrNode::DateField { op, recv: this };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    vec![to_json_object(frame), to_iso_string_call(frame)]
}

/// `ToObject(this)` over the frame's i64 temp 0 (throwing for a nullish
/// `this`).
fn to_json_object(frame: Frame) -> IrNode {
    IrNode::ToObject(Box::new(IrNode::NullishCheck {
        value: Box::new(IrNode::Local(frame.i64(0))),
        key: "toJSON".to_owned(),
    }))
}

/// `Invoke(O, "toISOString")` over the frame's i64 temp 0.
fn to_iso_string_call(frame: Frame) -> IrNode {
    IrNode::MethodCallUser {
        obj: Box::new(IrNode::Local(frame.i64(0))),
        key: Box::new(IrNode::Str("toISOString".to_owned())),
        args: vec![],
        obj_local: frame.i64(2),
        not_callable: CalleeError::not_a_function("toISOString"),
    }
}

/// `Date.prototype.toJSON(key)`: `O = ToObject(this)`; `null` when
/// `ToPrimitive(O, number)` is a non-finite number, else
/// `Invoke(O, "toISOString")`. Generic over any receiver.
pub(super) fn emit_to_json(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, tv) = (frame.i64(0), frame.i64(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_node(
        &to_json_object(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function;
        LocalSet(o),
        LocalGet(o), I32Const(HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(tv),
    );
    return_if_pending(function);
    emit_is_number(function, tv);
    wasm!(function;
        If(BlockType::Empty),
        // `tv - tv` is `0` exactly for a finite `tv`.
        LocalGet(tv), F64ReinterpretI64, LocalGet(tv), F64ReinterpretI64, F64Sub,
        F64Const(Ieee64::from(0.0)), F64Ne,
        If(BlockType::Empty), I64Const(NULL_BITS), Return, End,
        End,
    );
    emit_node(
        &to_iso_string_call(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}
