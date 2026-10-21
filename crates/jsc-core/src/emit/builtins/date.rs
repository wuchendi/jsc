//! `%Date.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of a `Date.prototype` method on a non-Date receiver.
const NOT_DATE: CalleeErrorText<'static> = [
    "this is not a Date object.",
    "TypeError: this is not a Date object.",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 2] = [NOT_DATE[0], NOT_DATE[1]];

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

/// `Date.prototype.toString()`: the host's rendering of the Date.
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    this_date(function, frame, this, strings, helpers);
    wasm!(function; LocalGet(this), I32Const(CONVERT_OP_TO_STRING), Call(helpers.convert));
}

/// `Date.prototype.valueOf()`: the time value.
pub(super) fn emit_value_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    this_date(function, frame, this, strings, helpers);
    wasm!(function;
        LocalGet(this), I32WrapI64,
        F64Load(MemArg { offset: DATE_MS_OFFSET, align: 3, memory_index: 0 }),
        Call(helpers.box_f64),
    );
}
