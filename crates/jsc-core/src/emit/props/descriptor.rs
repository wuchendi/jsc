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
/// `__define_own` result: an array cannot take the descriptor yet (its
/// `length`, or non-default attributes on an index).
pub(in crate::emit) const DEFINE_UNSUPPORTED: i32 = 2;
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

/// The integrity-level names an array's `TypeError` reports.
const INTEGRITY_NAMES: [&str; 3] = ["preventExtensions", "seal", "freeze"];

/// The static strings the descriptor helpers reference, plus every
/// `__prop_throw` fragment (these helpers and `__define` throw through it in
/// any program).
pub(in crate::emit) fn descriptor_strings() -> impl Iterator<Item = &'static str> {
    FIELDS
        .map(|(key, ..)| key)
        .into_iter()
        .chain([OBJECT_TAG, ""])
        .chain(INTEGRITY_NAMES)
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
/// interned key) of the object-family block `o`. Never throws; returns
/// [`DEFINE_OK`], [`DEFINE_REJECT`], [`DEFINE_NOT_EXTENSIBLE`], or
/// [`DEFINE_UNSUPPORTED`] for an array's `length` or an array index whose
/// resulting attributes are not the default ones (an index with default
/// attributes is stored through `__array_set`).
///
/// A new property takes its absent fields as `false` / `undefined`. An
/// existing one keeps its absent fields; a kind change (data <-> accessor)
/// keeps `E` and `C` and resets the rest. A non-configurable property rejects
/// becoming configurable, an `enumerable` flip, a kind change, and — when
/// non-writable — becoming writable or a value that is not `SameValue`; an
/// accessor rejects a `get` / `set` that is not `SameValue`. An update sets
/// the table's `HAS_NON_ENUM` flag and its dirty bit.
#[allow(clippy::too_many_lines)] // one straight-line helper body
pub(in crate::emit) fn emit_define_own(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (1, ValType::I64)]);
    let (o, key, desc, value, get, set) = (0, 1, 2, 3, 4, 5);
    let (e, a, m, t, i, b, p) = (6, 7, 8, 9, 10, 11, 12);
    let x = 13;
    let undefined = |f: &mut Function| wasm!(f; I64Const(UNDEFINED_BITS));
    let accessor_desc = DESC_HAS_GET | DESC_HAS_SET;
    let data_desc = DESC_HAS_VALUE | DESC_HAS_WRITABLE;

    // An array: `length` and accessor indices are unsupported; an index with
    // default resulting attributes is an element store.
    wasm!(f;
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And, LocalSet(i),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
    );
    emit_flag(&mut f, desc, accessor_desc);
    wasm!(f;
        LocalGet(i), I32Const(0), I32Ne, I32And, I32Or,
        If(BlockType::Empty), I32Const(DEFINE_UNSUPPORTED), Return, End,
        LocalGet(i),
        If(BlockType::Empty),
        // b = the element exists.
        LocalGet(key), Call(helpers.index_value), LocalTee(i),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32LtU,
        If(BlockType::Result(ValType::I32)),
    );
    emit_elem_load(&mut f, o, i, helpers.sparse_get());
    wasm!(f;
        I64Const(HOLE_BITS), I64Ne,
        Else, I32Const(0), End,
        LocalSet(b),
        // Every present W/E/C field must be true, and a new element needs all
        // three.
        LocalGet(desc), I32Const(DESC_HAS_SHIFT), I32ShrU, I32Const(7), I32And, LocalTee(m),
        LocalGet(desc), I32And, LocalGet(m), I32Ne,
        LocalGet(m), I32Const(7), I32Ne, LocalGet(b), I32Eqz, I32And,
        I32Or,
        If(BlockType::Empty), I32Const(DEFINE_UNSUPPORTED), Return, End,
        LocalGet(desc), I32Const(DESC_HAS_VALUE), I32And, LocalGet(b), I32Eqz, I32Or,
        If(BlockType::Empty),
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalGet(key));
    emit_box_heap(&mut f);
    wasm!(f;
        LocalGet(value), Call(helpers.array_set()),
        End,
        I32Const(DEFINE_OK), Return,
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
pub(in crate::emit) fn emit_to_desc(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
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
/// array's `length` is writable, non-enumerable, non-configurable, and its
/// elements (holes are absent) have the default attributes; a `LAZY`
/// function is materialized by the lookup.
pub(in crate::emit) fn emit_own_desc(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
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
        I32Const(ATTR_WRITABLE), LocalSet(a),
        Br(2),
        End,
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(key), Call(helpers.index_value), LocalTee(i),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
    );
    emit_elem_load(&mut f, o, i, helpers.sparse_get());
    wasm!(f;
        LocalTee(v), I64Const(HOLE_BITS), I64Eq,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        I32Const(ATTRS_DEFAULT), LocalSet(a),
        Br(2),
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
/// materialized first; a watched table sets its dirty bit. An array throws a
/// `TypeError` (left pending, nothing changed): its exotic `length` and
/// elements cannot take an integrity level yet.
pub(in crate::emit) fn emit_set_integrity(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32)]);
    let (obj, level) = (0, 1);
    let (o, p, e, end, a) = (2, 3, 4, 5, 6);
    let name = |i: usize| {
        Instruction::I64Const(HEAP_BITS | i64::from(static_key(strings, INTEGRITY_NAMES[i])))
    };
    wasm!(f;
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(obj),
        name(2), name(1), LocalGet(level), I32Const(INTEGRITY_FROZEN), I32Eq, Select,
        name(0), LocalGet(level), Select,
        I32Const(PROP_THROW_ARRAY_INTEGRITY), Call(helpers.prop_throw), Return,
        End,
    );
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
        I32Store8(m8(FLAGS)),
        End,
    );
    f
}

/// `__test_integrity(obj: i64, level: i32) -> i32`: for
/// [`INTEGRITY_NON_EXTENSIBLE`], `IsExtensible(obj)`; otherwise
/// `TestIntegrityLevel` — the object is not extensible and every own property
/// is non-configurable (and, [`INTEGRITY_FROZEN`], every own data property
/// non-writable).
pub(in crate::emit) fn emit_test_integrity() -> Function {
    let mut f = Function::new(vec![(5, ValType::I32)]);
    let (obj, level) = (0, 1);
    let (o, p, e, end, a) = (2, 3, 4, 5, 6);
    wasm!(f;
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        I32Eqz,
        If(BlockType::Empty), LocalGet(level), I32Eqz, Return, End,
        LocalGet(level), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
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
/// `length` is not enumerable and its present elements (holes are absent)
/// are; any other key is its table entry's `E` (a `LAZY` function is
/// materialized by the lookup).
pub(in crate::emit) fn emit_own_enum(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (obj, key, o, i) = (0, 1, 2, 3);
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
    emit_elem_load(&mut f, o, i, helpers.sparse_get());
    wasm!(f;
        I64Const(HOLE_BITS), I64Ne, Return,
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
