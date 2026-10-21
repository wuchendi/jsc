//! `%Error.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `Error.prototype.toString` on a non-object receiver.
const NOT_OBJECT: CalleeErrorText<'static> = [
    "Error.prototype.toString called on a non-object",
    "TypeError: Error.prototype.toString called on a non-object",
];

/// The separator between a non-empty name and message.
const SEPARATOR: &str = ": ";

/// The static strings of this family.
pub(super) const STRINGS: [&str; 3] = [NOT_OBJECT[0], NOT_OBJECT[1], SEPARATOR];

/// Read `this[key]` into i32 local `out` as a string base: `fallback` when it
/// is `undefined`, else `ToString` of it.
#[allow(clippy::too_many_arguments)]
fn string_field(
    function: &mut Function,
    this: u32,
    value: u32,
    out: u32,
    key: &str,
    fallback: &str,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(this), I32Const(static_key(strings, key)), Call(helpers.get), LocalSet(value),
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(value), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)),
        I32Const(static_key(strings, fallback)),
        Else,
    );
    wasm!(function; LocalGet(value), Call(helpers.to_str()), End, LocalSet(out));
    return_if_pending(function);
}

/// `Error.prototype.toString()`: `name` (default `"Error"`) and `message`
/// (default `""`) joined by `": "`, either alone when the other is empty.
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, value) = (frame.i64(0), frame.i64(1));
    let (name, message) = (frame.i32(0), frame.i32(1));
    let len = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_object(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NOT_OBJECT, strings, helpers);
    wasm!(function; End);
    string_field(
        function, this, value, name, "name", "Error", strings, helpers,
    );
    string_field(
        function, this, value, message, "message", "", strings, helpers,
    );
    wasm!(function;
        LocalGet(name), I32Load(len), I32Eqz,
        If(BlockType::Result(ValType::I32)),
        LocalGet(message),
        Else,
        LocalGet(message), I32Load(len), I32Eqz,
        If(BlockType::Result(ValType::I32)),
        LocalGet(name),
        Else,
        LocalGet(name), I32Const(static_key(strings, SEPARATOR)), Call(helpers.concat),
        LocalGet(message), Call(helpers.concat),
        End,
        End,
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
