//! `GetIterator` of the iteration sources walked by index
//! ([`IrNode::IterSource`]): for-of, `yield*`, spread and array
//! destructuring; and the builtin iterator objects (design/symbol-strings.md
//! §5.4): the `keys` / `values` / `entries` / `[@@iterator]` methods that
//! create them and the `next` of their prototypes.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::{IterFamily, IterKind, IterOp};

/// Offset of an iterator object's `[[IteratedObject]]` jsval (`undefined`
/// once exhausted), right after the object-family prefix.
const ITER_ITERATED_OFFSET: u32 = abi::OBJ_PREFIX_SIZE;
/// Offset of an iterator object's `[[NextIndex]]: u32` (an array index, a
/// string byte index, or a Map/Set backing slot index).
const ITER_INDEX_OFFSET: u32 = abi::OBJ_PREFIX_SIZE + 8;
/// Offset of an iterator object's `kind: u8` ([`kind_byte`]).
const ITER_KIND_OFFSET: u32 = abi::OBJ_PREFIX_SIZE + 12;
/// Byte size of an iterator object block.
const ITER_BLOCK_SIZE: i32 = 32;

/// The `kind` byte of an iterator of `family` yielding `kind` (a string
/// iterator yields code points; a Set's keys are its values).
fn kind_byte(family: IterFamily, kind: IterKind) -> i32 {
    match (family, kind) {
        (IterFamily::Array, IterKind::Keys) => 0,
        (IterFamily::Array, IterKind::Values) => 1,
        (IterFamily::Array, IterKind::Entries) => 2,
        (IterFamily::String, _) => 3,
        (IterFamily::Map, IterKind::Keys) => 4,
        (IterFamily::Map, IterKind::Values) => 5,
        (IterFamily::Map, IterKind::Entries) => 6,
        (IterFamily::Set, IterKind::Keys | IterKind::Values) => 7,
        (IterFamily::Set, IterKind::Entries) => 8,
    }
}

/// The receiver `TypeError` of every iterator prototype's `next`.
const NEXT_INCOMPATIBLE: CalleeErrorText<'static> = [
    "next method called on incompatible receiver",
    "TypeError: next method called on incompatible receiver",
];

/// The `null` / `undefined` receiver `TypeError` of
/// `String.prototype[@@iterator]`.
const STRING_NULLISH: CalleeErrorText<'static> = [
    "String.prototype[Symbol.iterator] called on null or undefined",
    "TypeError: String.prototype[Symbol.iterator] called on null or undefined",
];

/// The `null` / `undefined` receiver `TypeError` of
/// `Array.prototype.<name>`.
macro_rules! array_nullish {
    ($name:literal) => {
        [
            concat!("Array.prototype.", $name, " called on null or undefined"),
            concat!(
                "TypeError: Array.prototype.",
                $name,
                " called on null or undefined"
            ),
        ]
    };
}

/// The receiver `TypeError` of `<Map|Set>.prototype.<name>`.
macro_rules! incompatible {
    ($name:literal) => {
        [
            concat!("Method ", $name, " called on incompatible receiver"),
            concat!(
                "TypeError: Method ",
                $name,
                " called on incompatible receiver"
            ),
        ]
    };
}

/// The receiver `TypeError`s of the iterator-creating methods, one per
/// `(family, kind)` pair.
const CREATE_TEXTS: [(IterFamily, IterKind, CalleeErrorText<'static>); 8] = [
    (IterFamily::Array, IterKind::Keys, array_nullish!("keys")),
    (
        IterFamily::Array,
        IterKind::Values,
        array_nullish!("values"),
    ),
    (
        IterFamily::Array,
        IterKind::Entries,
        array_nullish!("entries"),
    ),
    (
        IterFamily::Map,
        IterKind::Keys,
        incompatible!("Map.prototype.keys"),
    ),
    (
        IterFamily::Map,
        IterKind::Values,
        incompatible!("Map.prototype.values"),
    ),
    (
        IterFamily::Map,
        IterKind::Entries,
        incompatible!("Map.prototype.entries"),
    ),
    (
        IterFamily::Set,
        IterKind::Values,
        incompatible!("Set.prototype.values"),
    ),
    (
        IterFamily::Set,
        IterKind::Entries,
        incompatible!("Set.prototype.entries"),
    ),
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    CREATE_TEXTS
        .into_iter()
        .flat_map(|(_, _, text)| text)
        .chain(NEXT_INCOMPATIBLE)
        .chain(STRING_NULLISH)
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let local = Frame { k }.i64(0);
    vec![
        iter_result(IrNode::Local(local), false),
        iter_result(IrNode::Undefined, true),
        IrNode::Array(vec![IrNode::Local(local), IrNode::Local(local)]),
    ]
}

/// `CreateIterResultObject(value, done)`.
fn iter_result(value: IrNode, done: bool) -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".to_owned()), value),
            (PropKey::Static("done".to_owned()), IrNode::Bool(done)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// Emit the body of the iteration builtin `op`.
pub(super) fn emit(
    op: IterOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match op {
        // `%IteratorPrototype%[@@iterator]` / `%AsyncIteratorPrototype%
        // [@@asyncIterator]`: `return this`.
        IterOp::IteratorSelf | IterOp::AsyncIteratorSelf => {
            wasm!(function; GlobalGet(THIS_GLOBAL));
        }
        IterOp::ArrayIter(kind) => emit_array_iter(kind, function, frame, strings, helpers),
        IterOp::CollectionIter { set, kind } => {
            emit_collection_iter(set, kind, function, frame, strings, helpers);
        }
        IterOp::StringIterator => emit_string_iter(function, frame, strings, helpers),
        IterOp::Next(family) => emit_next(family, function, frame, strings, helpers),
    }
}

/// Push a fresh `CLASS_ITERATOR` object over the jsval in i64 `target`
/// with `kind` and the `proto` of `family` (`obj` an i32 temp).
fn create(
    function: &mut Function,
    target: u32,
    obj: u32,
    family: IterFamily,
    kind: IterKind,
    helpers: Helpers<'_>,
) {
    let header = TYPE_OBJECT | (i32::from(abi::CLASS_ITERATOR) << 24);
    let proto = intrinsic_bits(Intrinsic::IterPrototype(family), helpers);
    wasm!(function;
        I32Const(ITER_BLOCK_SIZE), Call(helpers.alloc), LocalTee(obj),
        I32Const(header), I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(obj), I32Const(0),
        I32Store(MemArg { offset: u64::from(abi::OBJ_PROPS_OFFSET), align: 2, memory_index: 0 }),
        LocalGet(obj), I64Const(proto),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(obj), LocalGet(target),
        I64Store(MemArg { offset: u64::from(ITER_ITERATED_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(obj), I32Const(0),
        I32Store(MemArg { offset: u64::from(ITER_INDEX_OFFSET), align: 2, memory_index: 0 }),
        // The kind byte and its three pad bytes.
        LocalGet(obj), I32Const(kind_byte(family, kind)),
        I32Store(MemArg { offset: u64::from(ITER_KIND_OFFSET), align: 2, memory_index: 0 }),
        LocalGet(obj), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// The receiver `TypeError` of the method creating a `family` / `kind`
/// iterator.
fn create_text(family: IterFamily, kind: IterKind) -> CalleeErrorText<'static> {
    let kind = if family == IterFamily::Set && kind == IterKind::Keys {
        IterKind::Values
    } else {
        kind
    };
    CREATE_TEXTS
        .iter()
        .find_map(|&(f, k, text)| (f == family && k == kind).then_some(text))
        .unwrap_or(STRING_NULLISH)
}

/// `Array.prototype.keys` / `values` / `entries`: an Array Iterator over
/// `ToObject(this)`.
fn emit_array_iter(
    kind: IterKind,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, obj) = (frame.i64(0), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_is_nullish(function, o);
    wasm!(function; If(BlockType::Empty));
    let text = create_text(IterFamily::Array, kind);
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End, LocalGet(o), Call(helpers.to_object()), LocalSet(o));
    create(function, o, obj, IterFamily::Array, kind, helpers);
}

/// `Map.prototype.keys` / `values` / `entries`, `Set.prototype.values` /
/// `entries`: a Map / Set Iterator over `this` (`RequireInternalSlot`).
fn emit_collection_iter(
    set: bool,
    kind: IterKind,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, obj) = (frame.i64(0), frame.i32(0));
    let family = if set {
        IterFamily::Set
    } else {
        IterFamily::Map
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function;
        I32Const(if set { TYPE_SET } else { TYPE_MAP }), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, create_text(family, kind), strings, helpers);
    wasm!(function; End);
    create(function, this, obj, family, kind, helpers);
}

/// `String.prototype[@@iterator]`: a String Iterator over
/// `ToString(RequireObjectCoercible(this))`.
fn emit_string_iter(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, obj) = (frame.i64(0), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(s));
    emit_is_string(function, s);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_nullish(function, s);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, STRING_NULLISH, strings, helpers);
    wasm!(function; End);
    super::string::to_string(function, s, helpers);
    wasm!(function; End);
    create(
        function,
        s,
        obj,
        IterFamily::String,
        IterKind::Values,
        helpers,
    );
}

/// The `kind` byte range of `family`'s iterators.
fn kind_range(family: IterFamily) -> (i32, i32) {
    match family {
        IterFamily::Array => (0, 2),
        IterFamily::String => (3, 3),
        IterFamily::Map => (4, 6),
        IterFamily::Set => (7, 8),
    }
}

/// The locals of an iterator `next` body.
#[derive(Clone, Copy)]
struct Step {
    frame: Frame,
    /// The iterator object (i64).
    this: u32,
    /// Its `[[IteratedObject]]` (i64).
    target: u32,
    /// The step's value (i64).
    value: u32,
    /// An i64 temp.
    item: u32,
    /// The next index (i32).
    i: u32,
    /// The kind byte (i32).
    kind: u32,
    /// The iterated length (i32).
    len: u32,
    /// An i32 temp.
    at: u32,
}

impl Step {
    fn new(frame: Frame) -> Self {
        Self {
            frame,
            this: frame.i64(0),
            target: frame.i64(1),
            value: frame.i64(2),
            item: frame.i64(3),
            i: frame.i32(0),
            kind: frame.i32(1),
            len: frame.i32(2),
            at: frame.i32(3),
        }
    }

    /// Emit `node` in the body's scratch layout.
    fn emit(
        self,
        function: &mut Function,
        node: &IrNode,
        strings: &HashMap<String, i32>,
        helpers: Helpers<'_>,
    ) {
        emit_node(node, function, self.frame.scratch(), strings, helpers);
    }

    /// Exhaust the iterator: `[[IteratedObject]] = undefined`.
    fn exhaust(self, function: &mut Function) {
        wasm!(function;
            LocalGet(self.this), I32WrapI64, I64Const(UNDEFINED_BITS),
            I64Store(MemArg { offset: u64::from(ITER_ITERATED_OFFSET), align: 3, memory_index: 0 }),
        );
    }

    /// A pending throw exhausts the iterator and returns.
    fn bail_if_pending(self, function: &mut Function) {
        wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
        self.exhaust(function);
        wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    }

    /// Return the done result.
    fn done(self, function: &mut Function, strings: &HashMap<String, i32>, helpers: Helpers<'_>) {
        self.emit(
            function,
            &iter_result(IrNode::Undefined, true),
            strings,
            helpers,
        );
        wasm!(function; Return);
    }

    /// `i >= len`: exhaust and return the done result.
    fn end_at_len(
        self,
        function: &mut Function,
        strings: &HashMap<String, i32>,
        helpers: Helpers<'_>,
    ) {
        wasm!(function; LocalGet(self.i), LocalGet(self.len), I32GeU, If(BlockType::Empty));
        self.exhaust(function);
        self.done(function, strings, helpers);
        wasm!(function; End);
    }

    /// Store `i` as the next index.
    fn store_index(self, function: &mut Function) {
        wasm!(function;
            LocalGet(self.this), I32WrapI64, LocalGet(self.i),
            I32Store(MemArg { offset: u64::from(ITER_INDEX_OFFSET), align: 2, memory_index: 0 }),
        );
    }

    /// `[a, b]` of two i64 locals.
    fn pair(a: u32, b: u32) -> IrNode {
        IrNode::Array(vec![IrNode::Local(a), IrNode::Local(b)])
    }
}

/// The `next` of `%<family>IteratorPrototype%`: a receiver that is not an
/// iterator of the family throws; an exhausted iterator (its iterated value
/// `undefined`) returns `{ value: undefined, done: true }`, as does a step
/// past the end, which exhausts it; any other step returns `{ value, done:
/// false }` and advances. A throw while stepping (an array-like's `length`
/// or element getter) exhausts the iterator too, as the spec's closure
/// completes.
fn emit_next(
    family: IterFamily,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let st = Step::new(frame);
    let (lo, hi) = kind_range(family);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(st.this));
    emit_is_wrapper(function, st.this, abi::CLASS_ITERATOR);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(st.this), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(ITER_KIND_OFFSET), align: 0, memory_index: 0 }),
        LocalTee(st.kind), I32Const(lo), I32Sub, I32Const(hi - lo), I32LeU,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty),
    );
    throw_type_error(function, frame, NEXT_INCOMPATIBLE, strings, helpers);
    wasm!(function;
        End,
        LocalGet(st.this), I32WrapI64,
        I64Load(MemArg { offset: u64::from(ITER_ITERATED_OFFSET), align: 3, memory_index: 0 }),
        LocalTee(st.target), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
    );
    st.done(function, strings, helpers);
    wasm!(function;
        End,
        LocalGet(st.this), I32WrapI64,
        I32Load(MemArg { offset: u64::from(ITER_INDEX_OFFSET), align: 2, memory_index: 0 }),
        LocalSet(st.i),
    );
    match family {
        IterFamily::Array => step_array(function, st, strings, helpers),
        IterFamily::String => step_string(function, st, strings, helpers),
        IterFamily::Map | IterFamily::Set => {
            step_collection(family == IterFamily::Set, function, st, strings, helpers);
        }
    }
    st.emit(
        function,
        &iter_result(IrNode::Local(st.value), false),
        strings,
        helpers,
    );
}

/// An Array Iterator step into `value`: `LengthOfArrayLike` re-read per
/// step, then the index (keys), `Get(target, index)` (values) or both.
fn step_array(
    function: &mut Function,
    st: Step,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(st.target), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get),
    );
    st.bail_if_pending(function);
    wasm!(function; Call(helpers.to_number));
    st.bail_if_pending(function);
    wasm!(function; F64ReinterpretI64, I32TruncSatF64U, LocalSet(st.len));
    st.end_at_len(function, strings, helpers);
    wasm!(function;
        LocalGet(st.i), F64ConvertI32U, I64ReinterpretF64, LocalSet(st.item),
        LocalGet(st.i), I32Const(1), I32Add, LocalSet(st.i),
    );
    st.store_index(function);
    wasm!(function;
        LocalGet(st.kind), I32Eqz,
        If(BlockType::Empty), LocalGet(st.item), LocalSet(st.value),
        Else,
        LocalGet(st.target), LocalGet(st.item), I32Const(1), Call(helpers.to_key),
        Call(helpers.get), LocalSet(st.value),
    );
    st.bail_if_pending(function);
    wasm!(function; LocalGet(st.kind), I32Const(2), I32Eq, If(BlockType::Empty));
    st.emit(function, &Step::pair(st.item, st.value), strings, helpers);
    wasm!(function; LocalSet(st.value), End, End);
}

/// A String Iterator step into `value`: the next code point's string.
fn step_string(
    function: &mut Function,
    st: Step,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(st.target), I32WrapI64, LocalTee(st.at),
        I32Load(MemArg { offset: u64::from(abi::HEADER_SIZE), align: 2, memory_index: 0 }),
        LocalSet(st.len),
    );
    st.end_at_len(function, strings, helpers);
    emit_str_iter_next(function, st.at, st.i, st.len, st.kind, helpers);
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(st.value));
    st.store_index(function);
}

/// A Map / Set Iterator step into `value`: the next backing entry pair
/// (re-reading the live entry count, so appended entries are seen), as its
/// key, value or `[key, value]` (a Set's value is its key).
fn step_collection(
    set: bool,
    function: &mut Function,
    st: Step,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    map_set::entries_len(function, st.target);
    wasm!(function; LocalSet(st.len));
    st.end_at_len(function, strings, helpers);
    map_set::load_entry(function, st.target, st.i, st.at, st.value, st.item);
    wasm!(function; LocalGet(st.i), I32Const(2), I32Add, LocalSet(st.i));
    st.store_index(function);
    let (family, values, entry) = if set {
        (IterFamily::Set, None, Step::pair(st.value, st.value))
    } else {
        (
            IterFamily::Map,
            Some(IterKind::Values),
            Step::pair(st.value, st.item),
        )
    };
    // `value` holds the key: kept for keys and Set values.
    if let Some(values) = values {
        wasm!(function;
            LocalGet(st.kind), I32Const(kind_byte(family, values)), I32Eq,
            If(BlockType::Empty), LocalGet(st.item), LocalSet(st.value), End,
        );
    }
    wasm!(function;
        LocalGet(st.kind), I32Const(kind_byte(family, IterKind::Entries)), I32Eq,
        If(BlockType::Empty),
    );
    st.emit(function, &entry, strings, helpers);
    wasm!(function; LocalSet(st.value), End);
}

/// `__str_iter_next(s, i)` (design/symbol-strings.md §5.4) on the UTF-8
/// layout: push the fresh string of the code point starting at byte index
/// `i` of the string at i32 `s` (`len` its byte length, `i < len`) and
/// advance `i` past it. One UTF-8 sequence is one code point (no lone
/// surrogate exists before the UTF-16 flip); the sequence length comes from
/// its lead byte, clamped to the string's end. `k` is an i32 temp.
fn emit_str_iter_next(
    function: &mut Function,
    s: u32,
    i: u32,
    len: u32,
    k: u32,
    helpers: Helpers<'_>,
) {
    let payload = u64::from(abi::HEADER_SIZE) + 4;
    // k = the sequence length: 1 + (lead >= 0xC0) + (lead >= 0xE0) + (lead >= 0xF0).
    wasm!(function;
        LocalGet(s), LocalGet(i), I32Add,
        I32Load8U(MemArg { offset: payload, align: 0, memory_index: 0 }), LocalTee(k),
        I32Const(0xC0), I32GeU,
        LocalGet(k), I32Const(0xE0), I32GeU, I32Add,
        LocalGet(k), I32Const(0xF0), I32GeU, I32Add,
        I32Const(1), I32Add, LocalSet(k),
        LocalGet(len), LocalGet(i), I32Sub, LocalGet(k),
        LocalGet(len), LocalGet(i), I32Sub, LocalGet(k), I32LtU, Select, LocalSet(k),
        // The first byte, then each continuation byte appended.
        LocalGet(s), LocalGet(i), Call(helpers.str_index),
        LocalGet(i), LocalGet(k), I32Add, LocalSet(k),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(k), I32GeU, BrIf(1),
        LocalGet(s), LocalGet(i), Call(helpers.str_index), Call(helpers.concat),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
}

/// The [`Builtin::IterSource`] body: `this` is the iterated value, `env` is
/// `1` for a live walk. An array is itself; a string (or a `String`
/// wrapper's string) is itself when live, else its characters; a generator is
/// drained; a `Map` / `Set` is itself when live, else a fresh array of its
/// `[key, value]` entries / values; an `arguments` object is copied by
/// `Array.prototype.slice`; anything else throws `<x> is not iterable`. A
/// value whose chain holds a `@@iterator` (a user hook, `__symbol_get`; no
/// builtin defines one before Y2) throws the interim `TypeError` first
/// (design/symbol-strings.md §9.4).
pub(super) fn emit_iter_source(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (value, item, key, entry) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let (i, at, dst) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let live = frame.k;
    let kind = |function: &mut Function, kind: i32| {
        emit_load_type_byte(function, value);
        wasm!(function; I32Const(kind), I32Eq);
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(value));
    emit_hook_guard(
        function,
        value,
        WellKnown::Iterator,
        frame.scratch(),
        strings,
        helpers,
    );
    return_if_pending(function);
    // A `String` wrapper iterates its string.
    emit_is_wrapper(function, value, abi::CLASS_STRING);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(value), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::WRAPPER_VALUE_OFFSET), align: 3, memory_index: 0 }),
        LocalSet(value),
        End,
    );
    emit_is_pointer(function, value);
    wasm!(function; If(BlockType::Empty));
    kind(function, TYPE_ARRAY);
    wasm!(function; If(BlockType::Empty), LocalGet(value), Return, End);
    kind(function, TYPE_STRING);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(value),
        LocalGet(live), I32WrapI64, I32Eqz,
        If(BlockType::Empty), LocalGet(value), Call(helpers.str_to_array), Return, End,
        Return,
        End,
    );
    // A (sync) generator is drained; an async generator is not iterable.
    emit_is_generator(function, value);
    wasm!(function; If(BlockType::Empty), LocalGet(value), Call(helpers.gen_to_array), Return, End);
    kind(function, TYPE_MAP);
    kind(function, TYPE_SET);
    wasm!(function;
        I32Or, If(BlockType::Empty),
        LocalGet(live), I32WrapI64, If(BlockType::Empty), LocalGet(value), Return, End,
        I32Const(0), Call(helpers.new_array()), LocalSet(dst),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i),
    );
    map_set::entries_len(function, value);
    wasm!(function; I32GeU, BrIf(1));
    map_set::load_entry(function, value, i, at, key, item);
    kind(function, TYPE_MAP);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    let pair = IrNode::Array(vec![IrNode::Local(key), IrNode::Local(item)]);
    emit_node(&pair, function, frame.scratch(), strings, helpers);
    wasm!(function;
        Else, LocalGet(key), End, LocalSet(entry),
        LocalGet(dst), LocalGet(entry), Call(helpers.array_push), Drop,
        LocalGet(i), I32Const(2), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Return,
        End,
    );
    // An `arguments` object: `Array.prototype.slice.call(arguments)`.
    emit_is_wrapper(function, value, abi::CLASS_ARGUMENTS);
    wasm!(function; If(BlockType::Empty));
    for _ in 0..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(value), GlobalSet(THIS_GLOBAL), I64Const(0));
    emit_argc(0, function);
    wasm!(function;
        Call(helpers.builtin(Builtin::ArraySlice)), Return,
        End,
        End,
    );
    emit_drop(
        function,
        frame,
        &map_set::not_iterable(value),
        strings,
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS));
}
