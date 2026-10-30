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
/// The fragments around a `@@toStringTag` string.
const TAG_OPEN: &str = "[object ";
const TAG_CLOSE: &str = "]";

/// The `TypeError` of `toLocaleString` when `this.toString` is not callable.
const TO_STRING_NOT_CALLABLE: CalleeErrorText<'static> = [
    "toString is not a function",
    "TypeError: toString is not a function",
];

/// The `TypeError`s of `__defineGetter__` / `__defineSetter__` with a
/// non-callable accessor.
const GETTER_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Object.prototype.__defineGetter__: Expecting function",
    "TypeError: Object.prototype.__defineGetter__: Expecting function",
];
const SETTER_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Object.prototype.__defineSetter__: Expecting function",
    "TypeError: Object.prototype.__defineSetter__: Expecting function",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 21] = [
    NULLISH_TO_OBJECT[0],
    NULLISH_TO_OBJECT[1],
    TO_STRING_NOT_CALLABLE[0],
    TO_STRING_NOT_CALLABLE[1],
    GETTER_NOT_CALLABLE[0],
    GETTER_NOT_CALLABLE[1],
    SETTER_NOT_CALLABLE[0],
    SETTER_NOT_CALLABLE[1],
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
    TAG_OPEN,
    TAG_CLOSE,
];

/// Throw the `ToObject` `TypeError` when the `this` value in `this` is
/// `undefined` or `null`.
pub(super) fn require_object_coercible(
    function: &mut Function,
    frame: Frame,
    this: u32,
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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

/// `Object.prototype.toString()`: `"[object Undefined]"` /
/// `"[object Null]"`; else `O = ToObject(this)` and `"[object <tag>]"` with
/// `Get(O, @@toStringTag)` (`__symbol_get`) when it is a string, else the
/// builtin tag of `O` (Array, Function, Date, Error, Arguments, Boolean,
/// Number, String, else Object).
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, kind, tag) = (frame.i64(0), frame.i32(0), frame.i64(1));
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
        LocalGet(this), Call(helpers.to_object()), LocalTee(this),
        I32Const(realm::symbol_addr(WellKnown::ToStringTag, helpers)),
        Call(helpers.symbol_get()), LocalSet(tag),
    );
    return_if_pending(function);
    emit_is_string(function, tag);
    wasm!(function; If(BlockType::Empty));
    let tagged = IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str(TAG_OPEN.into())),
            rhs: Box::new(IrNode::Local(tag)),
        }),
        rhs: Box::new(IrNode::Str(TAG_CLOSE.into())),
    };
    emit_node(&tagged, function, frame.scratch(), strings, helpers);
    wasm!(function; Return, End);
    emit_load_type_byte(function, this);
    wasm!(function; LocalSet(kind));
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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

/// `Object.prototype.__defineGetter__(P, getter)` / `__defineSetter__(P,
/// setter)` (Annex B.2.2.2-3): `ToObject(this)`, the accessor must be
/// callable, `ToPropertyKey(P)`, then `DefinePropertyOrThrow` of `{[[Get]] |
/// [[Set]]: accessor, enumerable: true, configurable: true}`.
pub(super) fn emit_define_accessor(
    set: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (o, accessor) = (frame.i64(0), frame.i64(1));
    let (key, r) = (frame.i32(0), frame.i32(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    require_object_coercible(function, frame, o, strings, helpers);
    wasm!(function; LocalGet(o), Call(helpers.to_object()), LocalSet(o));
    frame.arg(function, 1);
    wasm!(function; LocalSet(accessor));
    emit_is_function(function, accessor);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    let text = if set {
        SETTER_NOT_CALLABLE
    } else {
        GETTER_NOT_CALLABLE
    };
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 0);
    wasm!(function; I32Const(1), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    let half = if set { DESC_HAS_SET } else { DESC_HAS_GET };
    let desc = half
        | DESC_HAS_ENUMERABLE
        | DESC_HAS_CONFIGURABLE
        | abi::ATTR_ENUMERABLE.cast_signed()
        | abi::ATTR_CONFIGURABLE.cast_signed();
    object_static::define_or_throw(
        function,
        o,
        key,
        r,
        |function| {
            wasm!(function; I32Const(desc), I64Const(UNDEFINED_BITS));
            for present in [!set, set] {
                if present {
                    wasm!(function; LocalGet(accessor));
                } else {
                    wasm!(function; I64Const(UNDEFINED_BITS));
                }
            }
        },
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// `Object.prototype.__lookupGetter__(P)` / `__lookupSetter__(P)` (Annex
/// B.2.2.4-5): `ToObject(this)`, `ToPropertyKey(P)`, then the first own
/// property `P` up the `proto` chain decides: an accessor's `get` / `set`,
/// else (a data property or none) `undefined`.
pub(super) fn emit_lookup_accessor(
    set: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (o, desc) = (frame.i64(0), frame.i64(1));
    let key = frame.i32(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    require_object_coercible(function, frame, o, strings, helpers);
    wasm!(function; LocalGet(o), Call(helpers.to_object()), LocalSet(o));
    frame.arg(function, 0);
    wasm!(function; I32Const(0), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    let half = static_key(strings, if set { "set" } else { "get" });
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(o), LocalGet(key), Call(helpers.own_desc()), LocalTee(desc),
        I64Const(UNDEFINED_BITS), I64Ne,
        // A data descriptor has no own `get` / `set` (one inherited from
        // `%Object.prototype%` must not be read).
        If(BlockType::Empty),
        I32Const(half), LocalGet(desc), Call(helpers.has_own()),
        If(BlockType::Result(ValType::I64)),
        LocalGet(desc), I32Const(half), Call(helpers.get),
        Else, I64Const(UNDEFINED_BITS), End,
        Return,
        End,
        LocalGet(o), I32WrapI64, I64Load(MemArg { offset: OBJ_PROTO_OFFSET, align: 3, memory_index: 0 }),
        LocalTee(o), I64Const(NULL_BITS), I64Eq, BrIf(1),
        Br(0),
        End, End,
        I64Const(UNDEFINED_BITS),
    );
}

/// The getter of `Object.prototype.__proto__`: the `proto` of
/// `ToObject(this)`.
pub(super) fn emit_proto_get(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function;
        LocalGet(this), Call(helpers.to_object()), I32WrapI64,
        I64Load(MemArg { offset: OBJ_PROTO_OFFSET, align: 3, memory_index: 0 }),
    );
}

/// The setter of `Object.prototype.__proto__`: `RequireObjectCoercible(this)`;
/// a value that is neither an object nor `null`, or a primitive `this`, does
/// nothing; else `[[SetPrototypeOf]]` (`__set_proto`; a rejection throws).
/// Returns `undefined`.
pub(super) fn emit_proto_set(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, proto, r) = (frame.i64(0), frame.i64(1), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    require_object_coercible(function, frame, this, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(proto));
    emit_is_object(function, proto);
    wasm!(function; LocalGet(proto), I64Const(NULL_BITS), I64Eq, I32Or);
    emit_is_object(function, this);
    wasm!(function;
        I32And,
        If(BlockType::Empty),
        LocalGet(this), I32WrapI64, LocalGet(proto), Call(helpers.set_proto()), LocalTee(r),
        If(BlockType::Empty),
    );
    object_static::prop_throw(
        function,
        |function| push_str(function, strings, ""),
        |function| wasm!(function; LocalGet(r)),
        helpers,
    );
    wasm!(function; End, End, I64Const(UNDEFINED_BITS));
}
