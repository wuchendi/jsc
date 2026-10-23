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
const TAG_MAP: &str = "[object Map]";
const TAG_SET: &str = "[object Set]";
const TAG_PROMISE: &str = "[object Promise]";
const TAG_GENERATOR: &str = "[object Generator]";
const TAG_OBJECT: &str = "[object Object]";

/// The `TypeError` of `toLocaleString` when `this.toString` is not callable.
const TO_STRING_NOT_CALLABLE: CalleeErrorText<'static> = [
    "toString is not a function",
    "TypeError: toString is not a function",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 19] = [
    NULLISH_TO_OBJECT[0],
    NULLISH_TO_OBJECT[1],
    TO_STRING_NOT_CALLABLE[0],
    TO_STRING_NOT_CALLABLE[1],
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
    TAG_MAP,
    TAG_SET,
    TAG_PROMISE,
    TAG_GENERATOR,
];

/// Throw the `ToObject` `TypeError` when the `this` value in `this` is
/// `undefined` or `null`.
pub(super) fn require_object_coercible(
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
/// `this`, Map/Set/Promise/Generator by kind (`Symbol.toStringTag` is phase 3).
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
        // The builtin `@@toStringTag`s of these kinds' prototypes (phase 3
        // makes them real properties).
        (TYPE_MAP, TAG_MAP),
        (TYPE_SET, TAG_SET),
        (i32::from(abi::TYPE_PROMISE), TAG_PROMISE),
        (TYPE_GENERATOR, TAG_GENERATOR),
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
    for (class, tag) in [
        (abi::CLASS_ARGUMENTS, TAG_ARGUMENTS),
        (abi::CLASS_BOOLEAN, TAG_BOOLEAN),
        (abi::CLASS_NUMBER, TAG_NUMBER),
        (abi::CLASS_STRING, TAG_STRING),
    ] {
        wasm!(function; LocalGet(kind), I32Const(i32::from(class)), I32Eq);
        tag_if(function, tag);
    }
    wasm!(function; End);
    push_str(function, strings, TAG_OBJECT);
}

/// `Object.prototype.valueOf()`: `ToObject(this)`.
pub(super) fn emit_value_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function; LocalGet(this), Call(helpers.to_object()));
}

/// `Object.prototype.isPrototypeOf(V)`: `false` for a non-object `V`; else
/// `ToObject(this)` and whether it is on `V`'s `proto` chain.
pub(super) fn emit_is_prototype_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, this) = (frame.i64(0), frame.i64(1));
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    emit_is_object(function, v);
    wasm!(function;
        I32Eqz, If(BlockType::Empty), I64Const(FALSE_BITS), Return, End,
        GlobalGet(THIS_GLOBAL), LocalSet(this),
    );
    require_object_coercible(function, frame, this, strings, helpers);
    let proto = MemArg {
        offset: OBJ_PROTO_OFFSET,
        align: 3,
        memory_index: 0,
    };
    wasm!(function;
        LocalGet(this), Call(helpers.to_object()), LocalSet(this),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(v), I32WrapI64, I64Load(proto), LocalTee(v), I64Const(NULL_BITS), I64Eq, BrIf(1),
        LocalGet(v), LocalGet(this), I64Eq,
        If(BlockType::Empty), I64Const(TRUE_BITS), Return, End,
        Br(0),
        End, End,
        I64Const(FALSE_BITS),
    );
}

/// `Object.prototype.propertyIsEnumerable(V)`: `ToPropertyKey(V)`, then
/// `ToObject(this)`, then whether it has an own enumerable property `V`
/// (`__own_enum`).
pub(super) fn emit_property_is_enumerable(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, key) = (frame.i64(0), frame.i32(0));
    frame.arg(function, 0);
    wasm!(function; I32Const(0), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function;
        LocalGet(this), Call(helpers.to_object()), LocalGet(key), Call(helpers.own_enum()),
        If(BlockType::Result(ValType::I64)), I64Const(TRUE_BITS), Else, I64Const(FALSE_BITS), End,
    );
}

/// `Object.prototype.toLocaleString()`: `Invoke(this, "toString")` (a
/// nullish `this` throws; a primitive's `toString` is looked up on its realm
/// prototype and called with the primitive itself).
pub(super) fn emit_to_locale_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, method) = (frame.i64(0), frame.i64(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function;
        LocalGet(this), I32Const(static_key(strings, "toString")), Call(helpers.get), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, TO_STRING_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    for _ in 0..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    invoke(function, method, |function| emit_argc(0, function), helpers);
}
