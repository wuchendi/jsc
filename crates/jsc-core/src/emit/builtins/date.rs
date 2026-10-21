//! `%Date.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of a `Date.prototype` method on a non-Date receiver.
const NOT_DATE: CalleeErrorText<'static> = [
    "this is not a Date object.",
    "TypeError: this is not a Date object.",
];

/// The static strings of this family (the method texts included).
pub(super) const STRINGS: [&str; 6] = [
    NOT_DATE[0],
    NOT_DATE[1],
    INVALID_DATE,
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
