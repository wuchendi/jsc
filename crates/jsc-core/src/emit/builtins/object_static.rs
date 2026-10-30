//! The `Object` statics over property descriptors, integrity levels,
//! prototypes and own keys (DESIGN.md §6.3): `defineProperty`,
//! `defineProperties`, `getOwnPropertyDescriptor(s)`, `preventExtensions`,
//! `seal`, `freeze`, `isExtensible`, `isSealed`, `isFrozen`, `create`,
//! `getPrototypeOf`, `setPrototypeOf`, `getOwnPropertyNames`, `assign`,
//! `hasOwn`, `is`, `fromEntries`. The descriptor work runs in the
//! `__to_desc` / `__define_own` / `__own_desc` / `__set_integrity` /
//! `__test_integrity` / `__own_enum` / `__set_proto` helpers.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `Object.defineProperty` on a non-object.
const DEFINE_PROPERTY_NON_OBJECT: CalleeErrorText<'static> = [
    "Object.defineProperty called on non-object",
    "TypeError: Object.defineProperty called on non-object",
];

/// The `TypeError` of `Object.defineProperties` on a non-object.
const DEFINE_PROPERTIES_NON_OBJECT: CalleeErrorText<'static> = [
    "Object.defineProperties called on non-object",
    "TypeError: Object.defineProperties called on non-object",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    DEFINE_PROPERTY_NON_OBJECT
        .into_iter()
        .chain(DEFINE_PROPERTIES_NON_OBJECT)
        .chain(descriptor_strings())
}

fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn m64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// Emit the body of the `Object` static `op`.
pub(super) fn emit(
    op: ReflectOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        ReflectOp::DefineProperty => emit_define_property(function, frame, strings, helpers),
        ReflectOp::DefineProperties => emit_define_properties(function, frame, strings, helpers),
        ReflectOp::GetOwnPropertyDescriptor => {
            emit_get_own_property_descriptor(function, frame, strings, helpers);
        }
        ReflectOp::GetOwnPropertyDescriptors => {
            emit_get_own_property_descriptors(function, frame, strings, helpers);
        }
        ReflectOp::PreventExtensions => {
            emit_set_level(INTEGRITY_NON_EXTENSIBLE, function, frame, helpers);
        }
        ReflectOp::Seal => emit_set_level(INTEGRITY_SEALED, function, frame, helpers),
        ReflectOp::Freeze => emit_set_level(INTEGRITY_FROZEN, function, frame, helpers),
        ReflectOp::IsExtensible => {
            emit_test_level(INTEGRITY_NON_EXTENSIBLE, function, frame, helpers);
        }
        ReflectOp::IsSealed => emit_test_level(INTEGRITY_SEALED, function, frame, helpers),
        ReflectOp::IsFrozen => emit_test_level(INTEGRITY_FROZEN, function, frame, helpers),
        ReflectOp::Create => emit_create(function, frame, strings, helpers),
        ReflectOp::GetPrototypeOf => emit_get_prototype_of(function, frame, strings, helpers),
        ReflectOp::SetPrototypeOf => emit_set_prototype_of(function, frame, strings, helpers),
        ReflectOp::GetOwnPropertyNames => {
            emit_get_own_property_names(function, frame, strings, helpers);
        }
        ReflectOp::GetOwnPropertySymbols => {
            emit_get_own_property_symbols(function, frame, strings, helpers);
        }
        ReflectOp::Assign => emit_assign(function, frame, strings, helpers),
        ReflectOp::HasOwn => emit_has_own(function, frame, strings, helpers),
        ReflectOp::Is => emit_is(function, frame, helpers),
        ReflectOp::FromEntries => emit_from_entries(function, frame, strings, helpers),
    }
}

/// Throw `text` when the jsval in `local` is not an object.
fn require_object(
    function: &mut Function,
    frame: Frame,
    local: u32,
    text: CalleeErrorText<'static>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_object(function, local);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// `ToObject` of argument `i` into i64 `out` (`null` / `undefined` throw).
fn arg_to_object(
    function: &mut Function,
    frame: Frame,
    i: u32,
    out: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    frame.arg(function, i);
    wasm!(function; LocalSet(out));
    object::require_object_coercible(function, frame, out, strings, helpers);
    wasm!(function; LocalGet(out), Call(helpers.to_object()), LocalSet(out));
}

/// Push element `i` (an i32 local) of the array whose i32 base is in `arr`.
fn emit_element(function: &mut Function, arr: u32, i: u32, slot: u64) {
    wasm!(function; LocalGet(arr));
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(5), I32Shl, I32Add,
        I64Load(elem_slot(slot)),
    );
}

/// `DefinePropertyOrThrow(o, key, record j)`: apply descriptor record `j` (an
/// i32 local) of the `__to_desc` list in i32 `list` to the own property `key`
/// (an interned i32 local) of the object in i64 `o`, returning from the body
/// with the `TypeError` pending when it is rejected, or with the exception an
/// array `length` left pending (`r` is i32 scratch).
fn emit_apply(
    function: &mut Function,
    o: u32,
    key: u32,
    list: u32,
    j: u32,
    r: u32,
    helpers: Helpers<'_>,
) {
    define_or_throw(
        function,
        o,
        key,
        r,
        |function| {
            emit_element(function, list, j, 0);
            wasm!(function; F64ReinterpretI64, I32TruncSatF64U);
            for slot in 1..4 {
                emit_element(function, list, j, slot);
            }
        },
        helpers,
    );
}

/// `DefinePropertyOrThrow(o, key, record)`: `__define_own` of the descriptor
/// record `record` pushes (`desc: i32, value, get, set: i64`) on the own
/// property `key` (an interned i32 local) of the object in i64 `o`; a
/// rejection returns from the body with its `TypeError` pending, as does an
/// exception an array `length` left pending (`r` is i32 scratch).
pub(super) fn define_or_throw(
    function: &mut Function,
    o: u32,
    key: u32,
    r: u32,
    record: impl FnOnce(&mut Function),
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(o), I32WrapI64, LocalGet(key));
    record(function);
    wasm!(function;
        Call(helpers.define_own()), LocalTee(r), I32Const(DEFINE_OK), I32Ne,
        GlobalGet(EXC_PENDING_GLOBAL), I32Or,
        If(BlockType::Empty),
        GlobalGet(EXC_PENDING_GLOBAL), I32Eqz,
        If(BlockType::Empty),
        LocalGet(o),
        LocalGet(key), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        I32Const(PROP_THROW_DEFINE_NOT_EXTENSIBLE), I32Const(PROP_THROW_REDEFINE),
        LocalGet(r), I32Const(DEFINE_NOT_EXTENSIBLE), I32Eq, Select,
        Call(helpers.prop_throw),
        End,
        I64Const(UNDEFINED_BITS), Return,
        End,
    );
}

/// `Object.defineProperty(O, P, Attributes)`: `O` must be an object; then
/// `ToPropertyKey(P)`, `ToPropertyDescriptor(Attributes)`, and
/// `DefinePropertyOrThrow`; returns `O`.
fn emit_define_property(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let o = frame.i64(0);
    let (key, list, j, r) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(o));
    require_object(
        function,
        frame,
        o,
        DEFINE_PROPERTY_NON_OBJECT,
        strings,
        helpers,
    );
    frame.arg(function, 1);
    wasm!(function; I32Const(1), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    wasm!(function; I32Const(0), Call(helpers.new_array()), LocalSet(list));
    frame.arg(function, 2);
    wasm!(function; LocalGet(list), Call(helpers.to_desc()));
    return_if_pending(function);
    wasm!(function; I32Const(0), LocalSet(j));
    emit_apply(function, o, key, list, j, r, helpers);
    wasm!(function; LocalGet(o));
}

/// `Object.defineProperties(O, Properties)`: `O` must be an object; every
/// descriptor of `ToObject(Properties)`'s own enumerable keys is read first
/// (`Get` + `ToPropertyDescriptor`, in own-key order), then each is applied
/// in order; returns `O`.
fn emit_define_properties(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let o = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(o));
    require_object(
        function,
        frame,
        o,
        DEFINE_PROPERTIES_NON_OBJECT,
        strings,
        helpers,
    );
    object_define_properties(function, frame, o, strings, helpers);
    wasm!(function; LocalGet(o));
}

/// `ObjectDefineProperties(O, Properties)` of the object in i64 `o` (not
/// i64 temps `1` / `2`, nor any i32 temp) and argument 1: every descriptor of
/// `ToObject(Properties)`'s own keys (strings, then symbols) that is an own
/// enumerable property when reached is read first (`Get` +
/// `ToPropertyDescriptor`, in own-key order; the read keys are compacted to
/// the front of the key list), then each is applied in order; a throw returns
/// from the body.
fn object_define_properties(
    function: &mut Function,
    frame: Frame,
    o: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (props, attrs) = (frame.i64(1), frame.i64(2));
    let (ks, n, i, key) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (list, result) = (frame.x32(0), frame.x32(1));
    arg_to_object(function, frame, 1, props, strings, helpers);
    wasm!(function;
        LocalGet(props), I32Const(OWN_ALL_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(ks),
        I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
        I32Const(0), Call(helpers.new_array()), LocalSet(list),
    );
    // key = the interned key `ks[i]`.
    let key_at = |function: &mut Function| {
        wasm!(function; LocalGet(ks));
        emit_array_elems(function);
        wasm!(function;
            LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)),
            I32Const(1), Call(helpers.to_key), LocalSet(key),
        );
    };
    let each = |function: &mut Function, body: &dyn Fn(&mut Function)| {
        wasm!(function;
            I32Const(0), LocalSet(i),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        );
        key_at(function);
        body(function);
        wasm!(function;
            LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            Br(0),
            End, End,
        );
    };
    // `result` counts the read keys until the apply pass reuses it as scratch.
    wasm!(function; I32Const(0), LocalSet(result));
    each(function, &|function| {
        wasm!(function;
            LocalGet(props), LocalGet(key), Call(helpers.own_enum()),
            If(BlockType::Empty),
            LocalGet(props), LocalGet(key), Call(helpers.get), LocalSet(attrs),
        );
        return_if_pending(function);
        wasm!(function; LocalGet(attrs), LocalGet(list), Call(helpers.to_desc()));
        return_if_pending(function);
        wasm!(function; LocalGet(ks));
        emit_array_elems(function);
        wasm!(function;
            LocalGet(result), I32Const(3), I32Shl, I32Add,
            LocalGet(key), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, I64Store(elem_slot(0)),
            LocalGet(result), I32Const(1), I32Add, LocalSet(result),
            End,
        );
    });
    wasm!(function; LocalGet(result), LocalSet(n));
    each(function, &|function| {
        emit_apply(function, o, key, list, i, result, helpers);
    });
}

/// `Object.getOwnPropertyDescriptor(O, P)`: `ToObject(O)`, `ToPropertyKey(P)`,
/// then `FromPropertyDescriptor` of the own property (`undefined` when
/// absent).
fn emit_get_own_property_descriptor(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, key) = (frame.i64(0), frame.i32(0));
    arg_to_object(function, frame, 0, v, strings, helpers);
    frame.arg(function, 1);
    wasm!(function; I32Const(0), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    wasm!(function; LocalGet(v), LocalGet(key), Call(helpers.own_desc()));
}

/// `Object.getOwnPropertyDescriptors(O)`: a fresh object mapping each own key
/// of `ToObject(O)` (own-key order: string keys, an array's `length` after
/// its indices, then symbol keys) to its descriptor object.
fn emit_get_own_property_descriptors(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, kv, d) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (res, ks, n, i) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (key, length) = (frame.x32(0), frame.x32(1));
    arg_to_object(function, frame, 0, v, strings, helpers);
    wasm!(function; LocalGet(v), I32WrapI64, LocalSet(key));
    emit_materialize(function, key, helpers);
    wasm!(function;
        I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalSet(res),
    );
    emit_object_prefix(
        function,
        res,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    // length = 1 while an array's `length` is still to come.
    emit_load_type_byte(function, v);
    wasm!(function;
        I32Const(TYPE_ARRAY), I32Eq, LocalSet(length),
        LocalGet(v), I32Const(OWN_ALL_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(ks),
        I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
    );
    // Define res[key] = the descriptor of the interned key pushed by `push`.
    let add = |function: &mut Function, push: &dyn Fn(&mut Function)| {
        wasm!(function; LocalGet(v));
        push(function);
        wasm!(function;
            Call(helpers.own_desc()), LocalTee(d), I64Const(UNDEFINED_BITS), I64Ne,
            If(BlockType::Empty),
            LocalGet(res),
        );
        push(function);
        wasm!(function;
            LocalGet(d), I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
            End,
        );
    };
    let length_key = static_key(strings, INDEX_LENGTH_KEY);
    wasm!(function;
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(ks),
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), LocalSet(kv),
        LocalGet(length), LocalGet(kv), I32WrapI64, Call(helpers.index_value), I32Const(-1), I32Eq,
        I32And,
        If(BlockType::Empty),
    );
    add(function, &|function| wasm!(function; I32Const(length_key)));
    wasm!(function;
        I32Const(0), LocalSet(length),
        End,
        LocalGet(kv), I32Const(1), Call(helpers.to_key), LocalSet(key),
    );
    add(function, &|function| wasm!(function; LocalGet(key)));
    wasm!(function;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(length),
        If(BlockType::Empty),
    );
    add(function, &|function| wasm!(function; I32Const(length_key)));
    wasm!(function;
        End,
        LocalGet(res), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// `Object.preventExtensions` / `seal` / `freeze` (`level`): an object takes
/// the integrity level (`__set_integrity`); any other value is returned as is.
fn emit_set_level(level: i32, function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let v = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    emit_is_object(function, v);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(v), I32Const(level), Call(helpers.set_integrity()),
    );
    return_if_pending(function);
    wasm!(function; End, LocalGet(v));
}

/// `Object.isExtensible` / `isSealed` / `isFrozen` (`level`): an object is
/// tested (`__test_integrity`); any other value is not extensible, so it is
/// sealed and frozen.
fn emit_test_level(level: i32, function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let v = frame.i64(0);
    let primitive = if level == INTEGRITY_NON_EXTENSIBLE {
        FALSE_BITS
    } else {
        TRUE_BITS
    };
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    emit_is_object(function, v);
    wasm!(function;
        If(BlockType::Result(ValType::I64)),
        I64Const(TRUE_BITS), I64Const(FALSE_BITS),
        LocalGet(v), I32Const(level), Call(helpers.test_integrity()), Select,
        Else, I64Const(primitive),
        End,
    );
}

/// Raise the keyed `__prop_throw` `TypeError` of the mode pushed by `mode`
/// over the key jsval pushed by `key`, and return from the body.
pub(super) fn prop_throw(
    function: &mut Function,
    key: impl FnOnce(&mut Function),
    mode: impl FnOnce(&mut Function),
    helpers: Helpers<'_>,
) {
    wasm!(function; I64Const(UNDEFINED_BITS));
    key(function);
    mode(function);
    wasm!(function; Call(helpers.prop_throw), I64Const(UNDEFINED_BITS), Return);
}

/// Throw `Object prototype may only be an Object or null: <v>` unless the
/// jsval in `proto` is an object or `null`.
fn require_proto(function: &mut Function, proto: u32, helpers: Helpers<'_>) {
    emit_is_object(function, proto);
    wasm!(function;
        LocalGet(proto), I64Const(NULL_BITS), I64Eq, I32Or, I32Eqz,
        If(BlockType::Empty),
    );
    prop_throw(
        function,
        |function| wasm!(function; LocalGet(proto)),
        |function| wasm!(function; I32Const(PROP_THROW_PROTO_TYPE)),
        helpers,
    );
    wasm!(function; End);
}

/// `Object.create(O, Properties)`: `O` must be an object or `null`; a fresh
/// ordinary object with `proto` `O`, then `ObjectDefineProperties` when
/// `Properties` is not `undefined`.
fn emit_create(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (proto, o, base) = (frame.i64(0), frame.i64(3), frame.i32(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(proto));
    require_proto(function, proto, helpers);
    wasm!(function;
        I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalTee(base),
        I64Const(i64::from(TYPE_OBJECT)), I64Store(m64(0)),
        LocalGet(base), LocalGet(proto), I64Store(m64(OBJ_PROTO_OFFSET)),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(o),
    );
    frame.arg(function, 1);
    wasm!(function; I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    object_define_properties(function, frame, o, strings, helpers);
    wasm!(function; End, LocalGet(o));
}

/// `Object.getPrototypeOf(O)`: the `proto` of `ToObject(O)` (a primitive's
/// realm prototype).
fn emit_get_prototype_of(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let v = frame.i64(0);
    arg_to_object(function, frame, 0, v, strings, helpers);
    wasm!(function; LocalGet(v), I32WrapI64, I64Load(m64(OBJ_PROTO_OFFSET)));
}

/// `Object.setPrototypeOf(O, proto)`: `RequireObjectCoercible(O)`, `proto`
/// must be an object or `null`; a primitive `O` is returned as is, an object
/// takes `[[SetPrototypeOf]]` (`__set_proto`; a rejection throws); returns
/// `O`.
fn emit_set_prototype_of(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (o, proto, r) = (frame.i64(0), frame.i64(1), frame.i32(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(o));
    object::require_object_coercible(function, frame, o, strings, helpers);
    frame.arg(function, 1);
    wasm!(function; LocalSet(proto));
    require_proto(function, proto, helpers);
    emit_is_object(function, o);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(o), I32WrapI64, LocalGet(proto), Call(helpers.set_proto()), LocalTee(r),
        If(BlockType::Empty),
    );
    prop_throw(
        function,
        |function| push_str(function, strings, ""),
        |function| wasm!(function; LocalGet(r)),
        helpers,
    );
    wasm!(function; End, End, LocalGet(o));
}

/// `Object.getOwnPropertyNames(O)`: every own string key of `ToObject(O)`,
/// enumerable or not, in own-key order (a `LAZY` function is materialized
/// first; an array's `length` follows its indices).
fn emit_get_own_property_names(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, kv) = (frame.i64(0), frame.i64(1));
    let (res, ks, n, i) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (key, length) = (frame.x32(0), frame.x32(1));
    arg_to_object(function, frame, 0, v, strings, helpers);
    wasm!(function; LocalGet(v), I32WrapI64, LocalSet(key));
    emit_materialize(function, key, helpers);
    wasm!(function; LocalGet(v), I32Const(OWN_ALL), Call(helpers.own_keys), LocalSet(kv));
    emit_load_type_byte(function, v);
    wasm!(function;
        I32Const(TYPE_ARRAY), I32Ne, If(BlockType::Empty), LocalGet(kv), Return, End,
        LocalGet(kv), I32WrapI64, LocalTee(ks), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
        I32Const(0), Call(helpers.new_array()), LocalSet(res),
        I32Const(1), LocalSet(length),
    );
    // Push `length` (once) onto `res`.
    let push_length = |function: &mut Function| {
        wasm!(function; LocalGet(res));
        push_str(function, strings, INDEX_LENGTH_KEY);
        wasm!(function; Call(helpers.array_push), Drop, I32Const(0), LocalSet(length));
    };
    wasm!(function;
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(ks),
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), LocalSet(kv),
        LocalGet(length), LocalGet(kv), I32WrapI64, Call(helpers.index_value), I32Const(-1), I32Eq,
        I32And,
        If(BlockType::Empty),
    );
    push_length(function);
    wasm!(function;
        End,
        LocalGet(res), LocalGet(kv), Call(helpers.array_push), Drop,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(length),
        If(BlockType::Empty),
    );
    push_length(function);
    wasm!(function;
        End,
        LocalGet(res), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// `Object.getOwnPropertySymbols(O)`: every own symbol key of `ToObject(O)`,
/// enumerable or not, in creation order.
fn emit_get_own_property_symbols(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (v, key) = (frame.i64(0), frame.i32(0));
    arg_to_object(function, frame, 0, v, strings, helpers);
    wasm!(function; LocalGet(v), I32WrapI64, LocalSet(key));
    emit_materialize(function, key, helpers);
    wasm!(function; LocalGet(v), I32Const(OWN_SYMBOLS), Call(helpers.own_keys));
}

/// `Object.assign(target, ...sources)`: `ToObject(target)`; each non-nullish
/// source is `ToObject`'d and each of its own keys (own-key order: strings,
/// then symbols) that is
/// an own enumerable property when reached (`__own_enum`) is read with
/// `[[Get]]` and written to the target with a strict `[[Set]]` (a throw
/// returns); returns the target.
fn emit_assign(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (to, from, kv, v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (n, i, ks, len) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (at, spread) = (frame.x32(0), frame.x64(0));
    // The spread argument array, before a getter's spread call republishes it.
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(spread));
    arg_to_object(function, frame, 0, to, strings, helpers);
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U, LocalSet(n),
        I32Const(1), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    arg_at(function, frame, i, spread, from);
    emit_is_nullish(function, from);
    wasm!(function;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(from), Call(helpers.to_object()), LocalTee(from),
        I32Const(OWN_ALL_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(ks),
        I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(len),
        I32Const(0), LocalSet(at),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(ks),
    );
    emit_array_elems(function);
    // kv = the interned key `ks[at]`, as a string or symbol jsval.
    wasm!(function;
        LocalGet(at), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)),
        I32Const(1), Call(helpers.to_key), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(kv),
        LocalGet(from), LocalGet(kv), I32WrapI64, Call(helpers.own_enum()),
        If(BlockType::Empty),
        LocalGet(from), LocalGet(kv), I32WrapI64, Call(helpers.get), LocalSet(v),
    );
    return_if_pending(function);
    let store = IrNode::IndexSet {
        obj: Box::new(IrNode::Local(to)),
        index: Box::new(IrNode::Local(kv)),
        value: Box::new(IrNode::Local(v)),
    };
    emit_drop(function, frame, &store, strings, helpers);
    wasm!(function;
        End,
        LocalGet(at), I32Const(1), I32Add, LocalSet(at),
        Br(0),
        End, End,
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(to),
    );
}

/// `Object.hasOwn(O, P)`: `ToObject(O)`, then `ToPropertyKey(P)`, then
/// whether the own property exists (`__has_own`).
fn emit_has_own(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (v, key) = (frame.i64(0), frame.i32(0));
    arg_to_object(function, frame, 0, v, strings, helpers);
    frame.arg(function, 1);
    wasm!(function; I32Const(0), Call(helpers.to_key), LocalSet(key));
    return_if_pending(function);
    wasm!(function;
        LocalGet(key), LocalGet(v), Call(helpers.has_own()),
        If(BlockType::Result(ValType::I64)), I64Const(TRUE_BITS), Else, I64Const(FALSE_BITS), End,
    );
}

/// `Object.is(a, b)`: `SameValue` (`NaN` is `NaN`, `+0` is not `-0`).
fn emit_is(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let (a, b) = (frame.i64(0), frame.i64(1));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a));
    frame.arg(function, 1);
    wasm!(function; LocalSet(b), I64Const(TRUE_BITS), I64Const(FALSE_BITS));
    emit_same_value(function, a, b, helpers);
    wasm!(function; Select);
}

/// `Object.fromEntries(iterable)`: `RequireObjectCoercible`, then each value
/// its iterator record yields (DESIGN.md §7.8; `<x> is not
/// iterable` without one) must be an object whose `0` / `1` become a data
/// property (`ToPropertyKey(entry[0])`, `CreateDataPropertyOrThrow`) of a
/// fresh ordinary object. A non-object entry or a throwing entry read or key
/// conversion closes the iterator.
fn emit_from_entries(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (src, item, kv, v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (res, key) = (frame.i32(0), frame.i32(3));
    let rec = iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    frame.arg(function, 0);
    wasm!(function; LocalSet(src));
    object::require_object_coercible(function, frame, src, strings, helpers);
    emit_get_iterator(
        &IrNode::Local(src),
        &rec,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalSet(res));
    emit_object_prefix(
        function,
        res,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    iter::guarded(function, helpers, |function| {
        wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
        enter_frame(helpers);
        enter_frame(helpers);
        emit_iterator_step(&rec, function, scratch, strings, helpers);
        wasm!(function; I32Eqz, BrIf(1));
        emit_iterator_value(&rec, function, scratch, strings, helpers);
        wasm!(function; LocalSet(item));
        emit_is_object(function, item);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        enter_frame(helpers);
        emit_drop(
            function,
            frame,
            &map_set::not_an_entry(item),
            strings,
            helpers,
        );
        leave_frame(helpers);
        wasm!(function; End);
        for (index, out) in [("0", kv), ("1", v)] {
            wasm!(function; LocalGet(item), I32Const(static_key(strings, index)), Call(helpers.get), LocalSet(out));
            emit_propagate_if_pending(function, helpers);
        }
        wasm!(function; LocalGet(kv), I32Const(1), Call(helpers.to_key), LocalSet(key));
        emit_propagate_if_pending(function, helpers);
        wasm!(function;
            LocalGet(res), LocalGet(key), LocalGet(v), I32Const(abi::ATTRS_DEFAULT.cast_signed()),
            Call(helpers.define),
        );
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
        LocalGet(res), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
