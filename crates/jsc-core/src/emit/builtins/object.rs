//! `%Object.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `ToObject(undefined | null)`.
const NULLISH_TO_OBJECT: CalleeErrorText<'static> = [
    "Cannot convert undefined or null to object",
    "TypeError: Cannot convert undefined or null to object",
];

/// The `Object.prototype.toString` results, by builtin tag.
const TAG_UNDEFINED: &str = "[object Undefined]";
const TAG_NULL: &str = "[object Null]";
const TAG_BOOLEAN: &str = "[object Boolean]";
const TAG_NUMBER: &str = "[object Number]";
const TAG_STRING: &str = "[object String]";
const TAG_ARRAY: &str = "[object Array]";
const TAG_FUNCTION: &str = "[object Function]";
const TAG_DATE: &str = "[object Date]";
const TAG_ERROR: &str = "[object Error]";
const TAG_ARGUMENTS: &str = "[object Arguments]";
const TAG_OBJECT: &str = "[object Object]";

/// The static strings of this family.
pub(super) const STRINGS: [&str; 13] = [
    NULLISH_TO_OBJECT[0],
    NULLISH_TO_OBJECT[1],
    TAG_UNDEFINED,
    TAG_NULL,
    TAG_BOOLEAN,
    TAG_NUMBER,
    TAG_STRING,
    TAG_ARRAY,
    TAG_FUNCTION,
    TAG_DATE,
    TAG_ERROR,
    TAG_ARGUMENTS,
    TAG_OBJECT,
];

/// Throw the `ToObject` `TypeError` when the `this` value in `this` is
/// `undefined` or `null`.
fn require_object_coercible(
    function: &mut Function,
    frame: Frame,
    this: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_is_nullish(function, this);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, NULLISH_TO_OBJECT, strings, helpers);
    wasm!(function; End);
}

/// `Object.prototype.hasOwnProperty(V)`: `ToPropertyKey(V)`, then
/// `ToObject(this)` (a primitive other than a string has no own properties),
/// then whether the own property exists (`__has_own`).
pub(super) fn emit_has_own_property(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, key) = (frame.i64(0), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; I32Const(0), Call(helpers.to_key), LocalSet(key));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function;
        LocalGet(key), LocalGet(this), Call(helpers.has_own()),
        If(BlockType::Result(ValType::I64)), I64Const(TRUE_BITS), Else, I64Const(FALSE_BITS), End,
    );
}

/// `Object.prototype.toString()`: `"[object <tag>]"` with the builtin tag of
/// `this` (`Symbol.toStringTag` is phase 3).
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
) {
    let (this, kind) = (frame.i64(0), frame.i32(0));
    let tag_if = |function: &mut Function, tag: &str| {
        wasm!(function; If(BlockType::Empty));
        push_str(function, strings, tag);
        wasm!(function; Return, End);
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalTee(this), I64Const(UNDEFINED_BITS), I64Eq);
    tag_if(function, TAG_UNDEFINED);
    wasm!(function; LocalGet(this), I64Const(NULL_BITS), I64Eq);
    tag_if(function, TAG_NULL);
    wasm!(function;
        LocalGet(this), I64Const(TRUE_BITS), I64Eq,
        LocalGet(this), I64Const(FALSE_BITS), I64Eq, I32Or,
    );
    tag_if(function, TAG_BOOLEAN);
    emit_is_number(function, this);
    tag_if(function, TAG_NUMBER);
    emit_load_type_byte(function, this);
    wasm!(function; LocalTee(kind), I32Const(TYPE_STRING), I32Eq);
    tag_if(function, TAG_STRING);
    for (type_byte, tag) in [
        (TYPE_ARRAY, TAG_ARRAY),
        (TYPE_FUNCTION, TAG_FUNCTION),
        (TYPE_DATE, TAG_DATE),
    ] {
        wasm!(function; LocalGet(kind), I32Const(type_byte), I32Eq);
        tag_if(function, tag);
    }
    wasm!(function;
        LocalGet(kind), I32Const(TYPE_OBJECT), I32Eq,
        If(BlockType::Empty),
        LocalGet(this), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(abi::HEADER_CLASS_OFFSET), align: 0, memory_index: 0 }),
        LocalSet(kind),
        LocalGet(kind), I32Const(i32::from(abi::CLASS_ERROR)), I32Eq,
    );
    tag_if(function, TAG_ERROR);
    wasm!(function; LocalGet(kind), I32Const(i32::from(abi::CLASS_ARGUMENTS)), I32Eq);
    tag_if(function, TAG_ARGUMENTS);
    wasm!(function; End);
    push_str(function, strings, TAG_OBJECT);
}

/// `Object.prototype.valueOf()`: `ToObject(this)` (a primitive stays as is
/// until the wrapper objects of S5).
pub(super) fn emit_value_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function; LocalGet(this));
}
