//! Property descriptors and integrity levels (DESIGN.md §6.3) over the
//! property tables of §5.3, whose `attrs` already hold every descriptor
//! state:
//!
//! - `__to_desc` (`ToPropertyDescriptor`) reads a descriptor object into a
//!   descriptor record — four jsvals `[desc, value, get, set]` appended to an
//!   array, `desc` a number packing the `W`/`E`/`C` values in their `attrs`
//!   bits plus the `DESC_HAS_*` presence bits (an absent `value`/`get`/`set`
//!   is `undefined`);
//! - `__define_own` (`ValidateAndApplyPropertyDescriptor`) applies a record;
//! - `__own_desc` (`FromPropertyDescriptor` of `[[GetOwnProperty]]`) builds
//!   the descriptor object of an own property;
//! - `__set_integrity` / `__test_integrity` back `preventExtensions` / `seal`
//!   / `freeze` and `isExtensible` / `isSealed` / `isFrozen`;
//! - `__own_enum` tests for an own enumerable property (`Object.assign`,
//!   `propertyIsEnumerable`), and `__set_proto` is `[[SetPrototypeOf]]`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Descriptor-record presence bit: `writable` is present.
pub(in crate::emit) const DESC_HAS_WRITABLE: i32 = 0x100;
/// Descriptor-record presence bit: `enumerable` is present.
pub(in crate::emit) const DESC_HAS_ENUMERABLE: i32 = 0x200;
/// Descriptor-record presence bit: `configurable` is present.
pub(in crate::emit) const DESC_HAS_CONFIGURABLE: i32 = 0x400;
/// Descriptor-record presence bit: `value` is present.
pub(in crate::emit) const DESC_HAS_VALUE: i32 = 0x800;
/// Descriptor-record presence bit: `get` is present.
pub(in crate::emit) const DESC_HAS_GET: i32 = 0x1000;
/// Descriptor-record presence bit: `set` is present.
pub(in crate::emit) const DESC_HAS_SET: i32 = 0x2000;
/// Every presence bit.
const DESC_FIELDS: i32 = 0x3F00;
/// The `W`/`E`/`C` presence bits sit this far above their value bits.
const DESC_HAS_SHIFT: i32 = 8;
/// The complete data descriptor `CreateDataProperty` defines:
/// `{value, writable: true, enumerable: true, configurable: true}`.
pub(in crate::emit) const DESC_DATA_DEFAULT: i32 = DESC_HAS_WRITABLE
    | DESC_HAS_ENUMERABLE
    | DESC_HAS_CONFIGURABLE
    | DESC_HAS_VALUE
    | ATTRS_DEFAULT;

/// `__define_own` result: the property was defined or updated.
pub(in crate::emit) const DEFINE_OK: i32 = 1;
/// `__define_own` result: a non-configurable property rejected the update.
pub(in crate::emit) const DEFINE_REJECT: i32 = 0;
/// `__define_own` result: a new property on a non-extensible object.
pub(in crate::emit) const DEFINE_NOT_EXTENSIBLE: i32 = 3;

/// Integrity level: not extensible (`preventExtensions` / `isExtensible`).
pub(in crate::emit) const INTEGRITY_NON_EXTENSIBLE: i32 = 0;
/// Integrity level: sealed.
pub(in crate::emit) const INTEGRITY_SEALED: i32 = 1;
/// Integrity level: frozen.
pub(in crate::emit) const INTEGRITY_FROZEN: i32 = 2;

/// The `ToPropertyDescriptor` fields in read order: key, presence bit, and
/// the `attrs` bit a boolean field sets (`0` for `value` / `get` / `set`).
const FIELDS: [(&str, i32, i32); 6] = [
    ("enumerable", DESC_HAS_ENUMERABLE, ATTR_ENUMERABLE),
    ("configurable", DESC_HAS_CONFIGURABLE, ATTR_CONFIGURABLE),
    ("value", DESC_HAS_VALUE, 0),
    ("writable", DESC_HAS_WRITABLE, ATTR_WRITABLE),
    ("get", DESC_HAS_GET, 0),
    ("set", DESC_HAS_SET, 0),
];

/// How a non-callable `get` / `set` renders in its `TypeError`.
const OBJECT_TAG: &str = "#<Object>";

/// The static strings the descriptor helpers reference, plus every
/// `__prop_throw` fragment (these helpers and `__define` throw through it in
/// any program).
pub(in crate::emit) fn descriptor_strings() -> impl Iterator<Item = &'static str> {
    FIELDS
        .map(|(key, ..)| key)
        .into_iter()
        .chain([OBJECT_TAG, ""])
        .chain(PROP_ERROR_STRS)
        .chain(
            PROP_KEYED_MSGS
                .into_iter()
                .flat_map(|(_, prefix, suffix)| [prefix, suffix]),
        )
}

/// Push `1` when `local & mask` is nonzero, else `0`.
fn emit_flag(function: &mut Function, local: u32, mask: i32) {
    wasm!(function; LocalGet(local), I32Const(mask), I32And, I32Const(0), I32Ne);
}

/// Push `SameValue` of the jsvals in i64 locals `a` and `b` as an i32: two
/// numbers by bits (so `+0` and `-0` differ) or both `NaN`; anything else
/// by `__same_value_zero` (strings by content, the rest by identity).
pub(in crate::emit) fn emit_same_value(
    function: &mut Function,
    a: u32,
    b: u32,
    helpers: Helpers<'_>,
) {
    emit_is_number(function, a);
    emit_is_number(function, b);
    wasm!(function;
        I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(a), LocalGet(b), I64Eq,
        LocalGet(a), F64ReinterpretI64, LocalGet(a), F64ReinterpretI64, F64Ne,
        LocalGet(b), F64ReinterpretI64, LocalGet(b), F64ReinterpretI64, F64Ne,
        I32And, I32Or,
        Else,
        LocalGet(a), LocalGet(b), Call(helpers.same_value_zero),
        End,
    );
}

/// Push a fresh `TYPE_ACCESSOR` block (as a heap jsval) whose getter and
/// setter are pushed by `getter` / `setter`; `b` is i32 scratch.
fn emit_accessor_block(
    function: &mut Function,
    b: u32,
    getter: impl FnOnce(&mut Function),
    setter: impl FnOnce(&mut Function),
    helpers: Helpers<'_>,
) {
    wasm!(function;
        I32Const(ACCESSOR_BLOCK_SIZE), Call(helpers.alloc), LocalTee(b),
        I32Const(TYPE_ACCESSOR), I32Store8(m8(0)),
        LocalGet(b),
    );
    getter(function);
    wasm!(function; I64Store(m64(ACCESSOR_GET_OFFSET)), LocalGet(b));
    setter(function);
    wasm!(function; I64Store(m64(ACCESSOR_SET_OFFSET)), LocalGet(b));
    emit_box_heap(function);
}

/// `__define_own(o: i32, key: i32, desc: i32, value: i64, get: i64, set: i64)
/// -> i32`: `ValidateAndApplyPropertyDescriptor` (ECMA-262 10.1.6.3) of the
/// descriptor record `(desc, value, get, set)` on the own property `key` (an
/// interned key) of the object-family block `o`. Returns [`DEFINE_OK`],
/// [`DEFINE_REJECT`], or [`DEFINE_NOT_EXTENSIBLE`]; only an array's `length`
/// may leave an exception pending (its `ToNumber` / `RangeError`, or the
/// `TypeError` of a shrink stopped by a non-configurable element).
///
/// An array's `length` is `ArraySetLength` (10.4.2.4): a value is first
/// converted by `ToUint32` and `ToNumber` (two conversions), and a mismatch is
/// the `RangeError` of `__array_set_length` (raised with the read-only flag
/// briefly cleared); then `configurable` / `enumerable` `true` and accessor
/// fields reject; a read-only `length` rejects `writable: true` and any other
/// value; a value goes through `__array_set_length`; `writable: false` then
/// makes it read-only (after putting the array in dictionary mode). An array index whose resulting
/// attributes are the default ones on a non-`SPARSE` array is an element store
/// (`__array_set`); any other index puts the array in dictionary mode, where
/// it is an ordinary table property (a new index at or past `length` rejects
/// on a read-only `length` and raises `length` otherwise).
///
/// A new property takes its absent fields as `false` / `undefined`. An
/// existing one keeps its absent fields; a kind change (data <-> accessor)
/// keeps `E` and `C` and resets the rest. A non-configurable property rejects
/// becoming configurable, an `enumerable` flip, a kind change, and — when
/// non-writable — becoming writable or a value that is not `SameValue`; an
/// accessor rejects a `get` / `set` that is not `SameValue`. An update sets
/// the table's `HAS_NON_ENUM` flag and its dirty bit.
#[allow(clippy::too_many_lines)] // one straight-line helper body
pub(in crate::emit) fn emit_define_own(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (2, ValType::I64)]);
    let (o, key, desc, value, get, set) = (0, 1, 2, 3, 4, 5);
    let (e, a, m, t, i, b, p) = (6, 7, 8, 9, 10, 11, 12);
    let (x, y) = (13, 14);
    let undefined = |f: &mut Function| wasm!(f; I64Const(UNDEFINED_BITS));
    let accessor_desc = DESC_HAS_GET | DESC_HAS_SET;
    let data_desc = DESC_HAS_VALUE | DESC_HAS_WRITABLE;
    // Push 1 when the boolean field `bit` is present and true.
    let present_true = |f: &mut Function, bit: i32| {
        let both = (bit << DESC_HAS_SHIFT) | bit;
        wasm!(f; LocalGet(desc), I32Const(both), I32And, I32Const(both), I32Eq);
    };
    let flag = |f: &mut Function, bit: i32| {
        wasm!(f; LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(bit), I32And);
    };

    wasm!(f;
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty),
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And,
        If(BlockType::Empty),
    );
    // `length` value: x = ToNumber(value) (for `ToUint32`), y = ToNumber(value);
    // `value` becomes the number `ToUint32(x)`, which must equal `y`.
    emit_is_number(&mut f, value);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(value), LocalSet(x), LocalGet(value), LocalSet(y),
        Else,
        LocalGet(value), Call(helpers.to_number), LocalSet(x),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        LocalGet(value), Call(helpers.to_number), LocalSet(y),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        End,
    );
    // ToUint32(x) = t - 2^32 * floor(t / 2^32), t = trunc(x) (NaN / infinities
    // saturate to 0).
    wasm!(f;
        LocalGet(x), F64ReinterpretI64, F64Trunc,
        LocalGet(x), F64ReinterpretI64, F64Trunc, F64Const(Ieee64::from(1.0 / 4_294_967_296.0)), F64Mul,
        F64Floor, F64Const(Ieee64::from(4_294_967_296.0)), F64Mul, F64Sub,
        I32TruncSatF64U, F64ConvertI32U, I64ReinterpretF64, LocalTee(value),
        F64ReinterpretI64, LocalGet(y), F64ReinterpretI64, F64Ne,
        If(BlockType::Empty),
        // NaN is always an invalid length: `__array_set_length` raises the
        // `RangeError` and changes nothing.
        LocalGet(o), I32Load8U(m8(FLAGS)), LocalSet(t),
        LocalGet(o), LocalGet(t), I32Const(!ARRAY_LENGTH_READONLY), I32And, I32Store8(m8(FLAGS)),
        LocalGet(o), I64Const(QNAN_BITS), Call(helpers.array_set_length()),
        LocalGet(o), LocalGet(t), I32Store8(m8(FLAGS)),
        I32Const(DEFINE_REJECT), Return,
        End,
        End,
    );
    // `length`: accessor fields, configurable or enumerable `true` reject.
    emit_flag(&mut f, desc, accessor_desc);
    present_true(&mut f, ATTR_CONFIGURABLE);
    wasm!(f; I32Or);
    present_true(&mut f, ATTR_ENUMERABLE);
    wasm!(f; I32Or, If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End);
    flag(&mut f, ARRAY_LENGTH_READONLY);
    wasm!(f; If(BlockType::Empty));
    // Read-only: no `writable: true`, and a value must equal the length.
    present_true(&mut f, ATTR_WRITABLE);
    wasm!(f;
        If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And,
        If(BlockType::Empty),
    );
    wasm!(f;
        LocalGet(value), F64ReinterpretI64, LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), F64ConvertI32U, F64Ne,
        If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        End,
        I32Const(DEFINE_OK), Return,
        End,
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(value), Call(helpers.array_set_length()),
        End,
        // `writable: false` applies even after a shrink stopped early.
        LocalGet(desc), I32Const(DESC_HAS_WRITABLE | ATTR_WRITABLE), I32And, I32Const(DESC_HAS_WRITABLE), I32Eq,
        If(BlockType::Empty),
    );
    emit_dictionize(&mut f, o, t, m, p, helpers);
    wasm!(f;
        LocalGet(o), LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_LENGTH_READONLY), I32Or,
        I32Store8(m8(FLAGS)),
        End,
        I32Const(DEFINE_REJECT), I32Const(DEFINE_OK), GlobalGet(EXC_PENDING_GLOBAL), Select, Return,
        End,
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(key), Call(helpers.index_value), LocalSet(i),
    );
    flag(&mut f, ARRAY_SPARSE);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        Block(BlockType::Empty),
    );
    // A dense array takes a default-attribute index as an element store.
    emit_flag(&mut f, desc, accessor_desc);
    wasm!(f;
        BrIf(0),
        // b = the element exists.
        LocalGet(i), LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32LtU,
        If(BlockType::Result(ValType::I32)),
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), I64Const(HOLE_BITS), I64Ne,
        Else, I32Const(0), End,
        LocalSet(b),
        // Every present W/E/C field must be true, and a new element needs all
        // three.
        LocalGet(desc), I32Const(DESC_HAS_SHIFT), I32ShrU, I32Const(7), I32And, LocalTee(m),
        LocalGet(desc), I32And, LocalGet(m), I32Ne,
        LocalGet(m), I32Const(7), I32Ne, LocalGet(b), I32Eqz, I32And,
        I32Or,
        BrIf(0),
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And, LocalGet(b), I32Eqz, I32Or,
        If(BlockType::Empty),
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f;
        LocalGet(value), Call(helpers.array_define()),
        End,
        I32Const(DEFINE_OK), Return,
        End,
        End,
    );
    // Any other index: dictionary mode, then an ordinary property; a new one
    // past the length needs a writable `length` and an extensible array.
    emit_dictionize(&mut f, o, t, m, p, helpers);
    wasm!(f;
        LocalGet(i), LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        If(BlockType::Empty),
    );
    flag(&mut f, ARRAY_LENGTH_READONLY);
    wasm!(f; If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End);
    flag(&mut f, NOT_EXTENSIBLE);
    wasm!(f;
        If(BlockType::Empty), I32Const(DEFINE_NOT_EXTENSIBLE), Return, End,
        LocalGet(o), LocalGet(i), I32Const(1), I32Add, I32Store(m32(ARRAY_LEN_OFFSET)),
        End,
        End,
        End,
    );

    // A new property: absent fields default to false / undefined.
    wasm!(f;
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(e), I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        If(BlockType::Empty), I32Const(DEFINE_NOT_EXTENSIBLE), Return, End,
        LocalGet(o), LocalGet(key),
        LocalGet(desc), I32Const(accessor_desc), I32And,
        If(BlockType::Result(ValType::I64)),
    );
    emit_accessor_block(
        &mut f,
        m,
        |f| wasm!(f; LocalGet(get)),
        |f| wasm!(f; LocalGet(set)),
        helpers,
    );
    wasm!(f;
        LocalGet(desc), I32Const(ATTR_ENUMERABLE | ATTR_CONFIGURABLE), I32And,
        I32Const(ATTR_ACCESSOR), I32Or, LocalSet(m),
        Else,
        LocalGet(value),
        LocalGet(desc), I32Const(ATTRS_DEFAULT), I32And, LocalSet(m),
        End,
        LocalGet(m), Call(helpers.define),
        I32Const(DEFINE_OK), Return,
        End,
        // An existing property: an empty descriptor changes nothing.
        LocalGet(e), I32Load(m32(E_ATTRS)), LocalSet(a),
        LocalGet(desc), I32Const(DESC_FIELDS), I32And, I32Eqz,
        If(BlockType::Empty), I32Const(DEFINE_OK), Return, End,
    );
    // i = accessor descriptor, b = data descriptor, t = accessor property.
    emit_flag(&mut f, desc, accessor_desc);
    wasm!(f; LocalSet(i));
    emit_flag(&mut f, desc, data_desc);
    wasm!(f; LocalSet(b));
    emit_flag(&mut f, a, ATTR_ACCESSOR);
    wasm!(f;
        LocalSet(t),
        LocalGet(a), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz,
        If(BlockType::Empty),
        // Non-configurable: no `configurable: true`, no `enumerable` flip, no
        // kind change.
        LocalGet(desc), I32Const(DESC_HAS_CONFIGURABLE | ATTR_CONFIGURABLE), I32And,
        I32Const(DESC_HAS_CONFIGURABLE | ATTR_CONFIGURABLE), I32Eq,
    );
    emit_flag(&mut f, desc, DESC_HAS_ENUMERABLE);
    wasm!(f;
        LocalGet(desc), LocalGet(a), I32Xor, I32Const(ATTR_ENUMERABLE), I32And, I32Const(0), I32Ne,
        I32And, I32Or,
        LocalGet(i), LocalGet(t), I32Eqz, I32And, I32Or,
        LocalGet(b), LocalGet(t), I32And, I32Or,
        If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        LocalGet(t),
        If(BlockType::Empty),
    );
    // An accessor keeps its exact getter and setter.
    for (present, offset, half) in [
        (DESC_HAS_GET, ACCESSOR_GET_OFFSET, get),
        (DESC_HAS_SET, ACCESSOR_SET_OFFSET, set),
    ] {
        wasm!(f;
            LocalGet(desc), I32Const(present), I32And,
            If(BlockType::Empty),
            LocalGet(e), I64Load(m64(E_VALUE)), I32WrapI64, I64Load(m64(offset)), LocalSet(x),
        );
        emit_same_value(&mut f, half, x, helpers);
        wasm!(f;
            I32Eqz, If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
            End,
        );
    }
    wasm!(f;
        Else,
        // A non-writable data property keeps its value and stays read-only.
        LocalGet(a), I32Const(ATTR_WRITABLE), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(desc), I32Const(DESC_HAS_WRITABLE | ATTR_WRITABLE), I32And,
        I32Const(DESC_HAS_WRITABLE | ATTR_WRITABLE), I32Eq,
        If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And,
        If(BlockType::Empty),
        LocalGet(e), I64Load(m64(E_VALUE)), LocalSet(x),
    );
    emit_same_value(&mut f, value, x, helpers);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I32Const(DEFINE_REJECT), Return, End,
        End,
        End,
        End,
        End,
        // Apply. A kind change keeps E and C and resets the rest.
        LocalGet(i), LocalGet(t), I32Eqz, I32And,
        If(BlockType::Empty),
        LocalGet(a), I32Const(ATTR_ENUMERABLE | ATTR_CONFIGURABLE), I32And,
        I32Const(ATTR_ACCESSOR), I32Or, LocalSet(a),
        LocalGet(e),
    );
    emit_accessor_block(&mut f, m, undefined, undefined, helpers);
    wasm!(f;
        I64Store(m64(E_VALUE)),
        End,
        LocalGet(b), LocalGet(t), I32And,
        If(BlockType::Empty),
        LocalGet(a), I32Const(ATTR_ENUMERABLE | ATTR_CONFIGURABLE), I32And, LocalSet(a),
        LocalGet(e), I64Const(UNDEFINED_BITS), I64Store(m64(E_VALUE)),
        End,
        LocalGet(a), I32Const(ATTR_ACCESSOR), I32And,
        If(BlockType::Empty),
        // An accessor's halves: a fresh block with the present ones replaced.
        LocalGet(i),
        If(BlockType::Empty),
        LocalGet(e), I64Load(m64(E_VALUE)), I32WrapI64, LocalSet(p),
        LocalGet(e),
    );
    let half = |present: i32, offset: u64, replacement: u32| {
        move |f: &mut Function| {
            wasm!(f;
                LocalGet(replacement),
                LocalGet(p), I64Load(m64(offset)),
                LocalGet(desc), I32Const(present), I32And,
                Select,
            );
        }
    };
    emit_accessor_block(
        &mut f,
        m,
        half(DESC_HAS_GET, ACCESSOR_GET_OFFSET, get),
        half(DESC_HAS_SET, ACCESSOR_SET_OFFSET, set),
        helpers,
    );
    wasm!(f;
        I64Store(m64(E_VALUE)),
        End,
        Else,
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And,
        If(BlockType::Empty), LocalGet(e), LocalGet(value), I64Store(m64(E_VALUE)), End,
        End,
        // attrs = (attrs & !present) | (desc & present) over W/E/C.
        LocalGet(desc), I32Const(DESC_HAS_SHIFT), I32ShrU, I32Const(7), I32And, LocalTee(m),
        I32Const(-1), I32Xor, LocalGet(a), I32And,
        LocalGet(desc), LocalGet(m), I32And, I32Or, LocalSet(a),
        LocalGet(e), LocalGet(a), I32Store(m32(E_ATTRS)),
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalSet(p),
        LocalGet(a), I32Const(ATTR_ENUMERABLE), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(p), LocalGet(p), I32Load8U(m8(P_TFLAGS)), I32Const(HAS_NON_ENUM), I32Or,
        I32Store8(m8(P_TFLAGS)),
        End,
    );
    if helpers.dirty_tracking {
        emit_mark_dirty(&mut f, p);
    }
    wasm!(f; I32Const(DEFINE_OK), End);
    f
}

/// `__to_desc(attrs: i64, list: i32)`: `ToPropertyDescriptor` (ECMA-262
/// 6.2.6.5) of `attrs`, appending its record `[desc, value, get, set]` to the
/// array `list`. A non-object `attrs` throws; each field is read in spec
/// order through `__has` + `__get` (so inherited fields and getters count,
/// and a throwing getter propagates); a `get` / `set` that is neither
/// callable nor `undefined` throws, and so does mixing `get` / `set` with
/// `value` / `writable`. A throw leaves the exception pending and appends
/// nothing.
pub(in crate::emit) fn emit_to_desc(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(1, ValType::I32), (5, ValType::I64)]);
    let (attrs, list, desc) = (0, 1, 2);
    let (v, value, get, set, scratch) = (3, 4, 5, 6, 7);
    let throw = |f: &mut Function, mode: i32| {
        wasm!(f; I32Const(mode), Call(helpers.prop_throw), Return);
    };
    emit_is_object(&mut f, attrs);
    wasm!(f; I32Eqz, If(BlockType::Empty), LocalGet(attrs), LocalGet(attrs));
    throw(&mut f, PROP_THROW_DESC_NOT_OBJECT);
    wasm!(f;
        End,
        I64Const(UNDEFINED_BITS), LocalSet(value),
        I64Const(UNDEFINED_BITS), LocalSet(get),
        I64Const(UNDEFINED_BITS), LocalSet(set),
    );
    for (name, present, bit) in FIELDS {
        let key = static_key(strings, name);
        wasm!(f;
            I32Const(key), LocalGet(attrs), Call(helpers.has),
            If(BlockType::Empty),
            LocalGet(attrs), I32Const(key), Call(helpers.get), LocalSet(v),
            GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), Return, End,
            LocalGet(desc), I32Const(present), I32Or, LocalSet(desc),
        );
        match name {
            "value" => wasm!(f; LocalGet(v), LocalSet(value)),
            "get" | "set" => {
                // Neither callable nor undefined: `<kind> must be a function: v`.
                emit_is_function(&mut f, v);
                wasm!(f;
                    I32Eqz, LocalGet(v), I64Const(UNDEFINED_BITS), I64Ne, I32And,
                    If(BlockType::Empty),
                    LocalGet(attrs),
                    I64Const(HEAP_BITS | i64::from(static_key(strings, OBJECT_TAG))), LocalGet(v),
                );
                emit_is_object(&mut f, v);
                wasm!(f; Select);
                let (mode, slot) = if name == "get" {
                    (PROP_THROW_GETTER, get)
                } else {
                    (PROP_THROW_SETTER, set)
                };
                throw(&mut f, mode);
                wasm!(f; End, LocalGet(v), LocalSet(slot));
            }
            _ => {
                emit_cond(&IrNode::Local(v), &mut f, scratch, strings, helpers);
                wasm!(f;
                    If(BlockType::Empty),
                    LocalGet(desc), I32Const(bit), I32Or, LocalSet(desc),
                    End,
                );
            }
        }
        wasm!(f; End);
    }
    emit_flag(&mut f, desc, DESC_HAS_GET | DESC_HAS_SET);
    emit_flag(&mut f, desc, DESC_HAS_VALUE | DESC_HAS_WRITABLE);
    wasm!(f;
        I32And,
        If(BlockType::Empty),
        LocalGet(attrs), I64Const(HEAP_BITS | i64::from(static_key(strings, ""))),
    );
    throw(&mut f, PROP_THROW_DESC_MIXED);
    wasm!(f;
        End,
        LocalGet(list), LocalGet(desc), F64ConvertI32U, I64ReinterpretF64,
        Call(helpers.array_push), Drop,
    );
    for slot in [value, get, set] {
        wasm!(f; LocalGet(list), LocalGet(slot), Call(helpers.array_push), Drop);
    }
    wasm!(f; End);
    f
}

/// `__own_desc(obj: i64, key: i32) -> i64`: `FromPropertyDescriptor` of the
/// own property `key` (an interned key, `0` = absent) of the object `obj`:
/// a fresh ordinary object with own enumerable `value`, `writable` or `get`,
/// `set`, then `enumerable`, `configurable`; `undefined` when absent. An
/// array's `length` is non-enumerable, non-configurable, and writable unless
/// `LENGTH_READONLY`; its store elements (holes are absent) have the default
/// attributes and its table elements their own; a `LAZY` function is
/// materialized by the lookup.
pub(in crate::emit) fn emit_own_desc(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32), (1, ValType::I64)]);
    let (obj, key) = (0, 1);
    let (o, e, i, d, a) = (2, 3, 4, 5, 6);
    let v = 7;
    wasm!(f;
        LocalGet(key), I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(obj), I32WrapI64, LocalSet(o),
        Block(BlockType::Empty),
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), F64ConvertI32U, I64ReinterpretF64, LocalSet(v),
        I32Const(0), I32Const(ATTR_WRITABLE),
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_LENGTH_READONLY), I32And,
        Select, LocalSet(a),
        Br(2),
        End,
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(key), Call(helpers.index_value), LocalTee(i),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
    // A store element; a table one falls through to the table lookup.
    emit_in_store(&mut f, o, i, a);
    wasm!(f; If(BlockType::Empty));
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)),
        LocalTee(v), I64Const(HOLE_BITS), I64Eq,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        I32Const(ATTRS_DEFAULT), LocalSet(a),
        Br(3),
        End,
        End,
        End,
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(e), I32Eqz,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(e), I32Load(m32(E_ATTRS)), LocalSet(a),
        LocalGet(e), I64Load(m64(E_VALUE)), LocalSet(v),
        End,
        I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalSet(d),
    );
    emit_object_prefix(
        &mut f,
        d,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    let field = |f: &mut Function, name: &str, push: &dyn Fn(&mut Function)| {
        wasm!(f; LocalGet(d), I32Const(static_key(strings, name)));
        push(f);
        wasm!(f; I32Const(ATTRS_DEFAULT), Call(helpers.define));
    };
    let boolean = |bit: i32| {
        move |f: &mut Function| {
            wasm!(f;
                I64Const(TRUE_BITS), I64Const(FALSE_BITS),
                LocalGet(a), I32Const(bit), I32And, Select,
            );
        }
    };
    wasm!(f;
        LocalGet(a), I32Const(ATTR_ACCESSOR), I32And,
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, LocalSet(i),
    );
    field(
        &mut f,
        "get",
        &|f| wasm!(f; LocalGet(i), I64Load(m64(ACCESSOR_GET_OFFSET))),
    );
    field(
        &mut f,
        "set",
        &|f| wasm!(f; LocalGet(i), I64Load(m64(ACCESSOR_SET_OFFSET))),
    );
    wasm!(f; Else);
    field(&mut f, "value", &|f| wasm!(f; LocalGet(v)));
    field(&mut f, "writable", &boolean(ATTR_WRITABLE));
    wasm!(f; End);
    field(&mut f, "enumerable", &boolean(ATTR_ENUMERABLE));
    field(&mut f, "configurable", &boolean(ATTR_CONFIGURABLE));
    wasm!(f; LocalGet(d));
    emit_box_heap(&mut f);
    wasm!(f; End);
    f
}

/// Walk the live entries of the property table in i32 local `p` (nonzero):
/// `body` runs with the entry address in i32 local `e` (`end` is i32
/// scratch).
fn emit_live_entries(
    function: &mut Function,
    p: u32,
    e: u32,
    end: u32,
    body: impl FnOnce(&mut Function),
) {
    wasm!(function;
        LocalGet(p), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(p), I32Add,
        I32Const(P_HEADER), I32Add, LocalSet(end),
        LocalGet(p), I32Const(P_HEADER), I32Add, LocalSet(e),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(e), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(e), I32Load(m32(E_KEY)),
        If(BlockType::Empty),
    );
    body(function);
    wasm!(function;
        End,
        LocalGet(e), I32Const(ENTRY), I32Add, LocalSet(e),
        Br(0),
        End, End,
    );
}

/// `__set_integrity(obj: i64, level: i32)`: `SetIntegrityLevel` of the
/// object `obj` — every own property becomes non-configurable
/// ([`INTEGRITY_SEALED`]) and every own data property also non-writable
/// ([`INTEGRITY_FROZEN`]), then the object becomes `NOT_EXTENSIBLE`
/// ([`INTEGRITY_NON_EXTENSIBLE`] does only that). A `LAZY` function is
/// materialized first; a watched table sets its dirty bit. An array is put in
/// dictionary mode first, so its elements are table entries the level
/// reaches, and a frozen array's `length` becomes read-only.
pub(in crate::emit) fn emit_set_integrity(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(6, ValType::I32)]);
    let (obj, level) = (0, 1);
    let (o, p, e, end, a, t) = (2, 3, 4, 5, 6, 7);
    wasm!(f;
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq, LocalTee(t),
        If(BlockType::Empty),
    );
    emit_dictionize(&mut f, o, e, end, a, helpers);
    wasm!(f; End);
    emit_materialize(&mut f, o, helpers);
    wasm!(f;
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(p),
        If(BlockType::Empty),
        LocalGet(level),
        If(BlockType::Empty),
    );
    emit_live_entries(&mut f, p, e, end, |f| {
        // attrs &= !C; a frozen data property also loses W.
        wasm!(f;
            LocalGet(e),
            LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(!ATTR_CONFIGURABLE), I32And, LocalTee(a),
            I32Const(!ATTR_WRITABLE), I32And,
            LocalGet(a),
            LocalGet(level), I32Const(INTEGRITY_FROZEN), I32Eq,
            LocalGet(a), I32Const(ATTR_ACCESSOR), I32And, I32Eqz, I32And,
            Select,
            I32Store(m32(E_ATTRS)),
        );
    });
    wasm!(f; End);
    if helpers.dirty_tracking {
        emit_mark_dirty(&mut f, p);
    }
    wasm!(f;
        End,
        LocalGet(o), LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32Or,
        I32Const(ARRAY_LENGTH_READONLY), I32Const(0),
        LocalGet(t), LocalGet(level), I32Const(INTEGRITY_FROZEN), I32Eq, I32And, Select,
        I32Or,
        I32Store8(m8(FLAGS)),
        End,
    );
    f
}

/// `__test_integrity(obj: i64, level: i32) -> i32`: for
/// [`INTEGRITY_NON_EXTENSIBLE`], `IsExtensible(obj)`; otherwise
/// `TestIntegrityLevel` — the object is not extensible and every own property
/// is non-configurable (and, [`INTEGRITY_FROZEN`], every own data property
/// non-writable, an array's `length` included).
pub(in crate::emit) fn emit_test_integrity() -> Function {
    let mut f = Function::new(vec![(5, ValType::I32)]);
    let (obj, level) = (0, 1);
    let (o, p, e, end, a) = (2, 3, 4, 5, 6);
    wasm!(f;
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        I32Eqz,
        If(BlockType::Empty), LocalGet(level), I32Eqz, Return, End,
        LocalGet(level), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(level), I32Const(INTEGRITY_FROZEN), I32Eq,
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq, I32And,
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_LENGTH_READONLY), I32And, I32Eqz, I32And,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(p),
        If(BlockType::Empty),
    );
    emit_live_entries(&mut f, p, e, end, |f| {
        wasm!(f;
            LocalGet(e), I32Load(m32(E_ATTRS)), LocalTee(a), I32Const(ATTR_CONFIGURABLE), I32And,
            LocalGet(level), I32Const(INTEGRITY_FROZEN), I32Eq,
            LocalGet(a), I32Const(ATTR_ACCESSOR | ATTR_WRITABLE), I32And, I32Const(ATTR_WRITABLE), I32Eq,
            I32And, I32Or,
            If(BlockType::Empty), I32Const(0), Return, End,
        );
    });
    wasm!(f; End, I32Const(1), End);
    f
}

/// `__own_enum(obj: i64, key: i32) -> i32`: whether the object `obj` has an
/// own enumerable property `key` (an interned key, `0` = absent): an array's
/// `length` is not enumerable and its present store elements (holes are
/// absent) are; any other key — a `SPARSE` or dictionary-mode array's
/// elements included — is its table entry's `E` (a `LAZY` function is
/// materialized by the lookup).
pub(in crate::emit) fn emit_own_enum(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(3, ValType::I32)]);
    let (obj, key, o, i, a) = (0, 1, 2, 3, 4);
    wasm!(f;
        LocalGet(key), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(key), Call(helpers.index_value), LocalTee(i),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        If(BlockType::Empty), I32Const(0), Return, End,
    );
    // A store element has the default attributes; a table one (a `SPARSE`
    // or dictionary-mode array) falls through to its entry's attributes.
    emit_in_store(&mut f, o, i, a);
    wasm!(f; If(BlockType::Empty));
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), I64Const(HOLE_BITS), I64Ne, Return,
        End,
        End,
        End,
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(i),
        If(BlockType::Result(ValType::I32)),
        LocalGet(i), I32Load(m32(E_ATTRS)), I32Const(ATTR_ENUMERABLE), I32And, I32Const(0), I32Ne,
        Else, I32Const(0), End,
        End,
    );
    f
}

/// `__set_proto(o: i32, proto: i64) -> i32`: `OrdinarySetPrototypeOf`
/// (ECMA-262 10.1.2.1) of the object-family block `o` to `proto` (an object
/// or `null`). Returns `0` when the prototype is set (or already is
/// `proto`), else the `__prop_throw` mode of the rejection (nothing changed):
/// [`PROP_THROW_PROTO_IMMUTABLE`] for `%Object.prototype%` (an immutable
/// prototype exotic object), [`PROP_THROW_PROTO_NOT_EXTENSIBLE`] for a
/// non-extensible object, [`PROP_THROW_PROTO_CYCLE`] when `o` is on
/// `proto`'s chain. A watched intrinsic sets its dirty bit.
pub(in crate::emit) fn emit_set_proto(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(1, ValType::I64), (1, ValType::I32)]);
    let (o, proto, p, t) = (0, 1, 2, 3);
    let object_prototype = intrinsic_bits(Intrinsic::ObjectPrototype, helpers);
    wasm!(f;
        LocalGet(o), I64Load(m64(OBJ_PROTO_OFFSET)), LocalGet(proto), I64Eq,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(o), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, I64Const(object_prototype), I64Eq,
        If(BlockType::Empty), I32Const(PROP_THROW_PROTO_IMMUTABLE), Return, End,
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        If(BlockType::Empty), I32Const(PROP_THROW_PROTO_NOT_EXTENSIBLE), Return, End,
        LocalGet(proto), LocalSet(p),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), I64Const(NULL_BITS), I64Eq, BrIf(1),
        LocalGet(p), I32WrapI64, LocalGet(o), I32Eq,
        If(BlockType::Empty), I32Const(PROP_THROW_PROTO_CYCLE), Return, End,
        LocalGet(p), I32WrapI64, I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(p),
        Br(0),
        End, End,
        LocalGet(o), LocalGet(proto), I64Store(m64(OBJ_PROTO_OFFSET)),
    );
    if helpers.dirty_tracking {
        wasm!(f; LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(t), If(BlockType::Empty));
        emit_mark_dirty(&mut f, t);
        wasm!(f; End);
    }
    wasm!(f; I32Const(0), End);
    f
}
