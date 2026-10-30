//! The iteration protocol (design/symbol-strings.md §5.1-§5.3): the
//! iterator record nodes ([`IrNode::GetIterator`], [`IrNode::IteratorStep`],
//! [`IrNode::IteratorValue`], [`IrNode::IteratorClose`]) with their guarded
//! fast paths, the internal protocol builtins behind them, and the builtin
//! iterator objects (§5.4): the `keys` / `values` / `entries` /
//! `[@@iterator]` methods that create them and the `next` of their
//! prototypes.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::{IterFamily, IterKind, IterOp};
use crate::ir::ConvertOp;

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

/// The `TypeError` of an `@@iterator` method returning a non-object.
const ITERATOR_NOT_OBJECT: CalleeErrorText<'static> = [
    "Result of the Symbol.iterator method is not an object",
    "TypeError: Result of the Symbol.iterator method is not an object",
];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    CREATE_TEXTS
        .into_iter()
        .flat_map(|(_, _, text)| text)
        .chain(NEXT_INCOMPATIBLE)
        .chain(STRING_NULLISH)
        .chain(ITERATOR_NOT_OBJECT)
        .chain(["next", "return", "value", "done"])
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let local = Frame { k }.i64(0);
    vec![
        iter_result(IrNode::Local(local), false),
        iter_result(IrNode::Undefined, true),
        IrNode::Array(vec![IrNode::Local(local), IrNode::Local(local)]),
        not_a_function(local, local),
        result_not_object(local, local),
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

/// A Map / Set Iterator step into `value`: the next live backing entry pair
/// (tombstones skipped; the slot count re-read, so appended entries are
/// seen), as its key, value or `[key, value]` (a Set's value is its key).
fn step_collection(
    set: bool,
    function: &mut Function,
    st: Step,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    map_set::skip_tombstones(function, st.target, st.i, st.at, st.value, st.item);
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

// --- the iteration protocol (design/symbol-strings.md §5.1-§5.3) -------------

/// Record state: done (closing does nothing).
const ST_DONE: f64 = 0.0;
/// Record state: a clean array or `arguments` object walked by index
/// (`next` holds the index).
pub(in crate::emit) const ST_ARRAY: f64 = 1.0;
/// Record state: a primitive string walked by code point (`next` holds the
/// byte index).
pub(in crate::emit) const ST_STRING: f64 = 2.0;
/// Record state: a clean `Map` walked live (`next` holds the backing slot).
const ST_MAP: f64 = 3.0;
/// Record state: a clean `Set` walked live (`next` holds the backing slot).
const ST_SET: f64 = 4.0;
/// Record state: a clean generator stepped by its state machine.
pub(in crate::emit) const ST_GEN: f64 = 5.0;
/// Record state: an `arguments` object whose own `@@iterator` is still
/// `%Array.prototype.values%`, walked like an array.
const ST_ARGS: f64 = 6.0;
/// Record state: an open protocol record (closing calls `return`).
const ST_PROTOCOL: f64 = 7.0;
/// Record state: a protocol step or its value read in flight, left behind
/// by a throw from either (closing does nothing: a throw from the iterator
/// is not closed).
const ST_STEPPING: f64 = 8.0;

/// The jsval of record state `state`.
fn st(state: f64) -> i64 {
    state.to_bits().cast_signed()
}

/// The record state of an open protocol record, for a builtin body
/// testing [`emit_iter_classify`]'s result.
pub(super) fn protocol_state() -> i64 {
    st(ST_PROTOCOL)
}

/// How [`emit_iterator_close`] treats the completion being propagated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::emit) enum CloseMode {
    /// A non-throw completion: a throw of the close itself propagates.
    Normal,
    /// A throw completion seen from a `catch` (none pending): a throw of the
    /// close itself is discarded.
    Catch,
    /// A throw completion still pending (a builtin body): it is set aside
    /// around the close and restored, discarding the close's own throw.
    Pending,
}

/// Write `value` into a record place (a local, or a promoted frame cell).
fn place_set(place: &IrNode, value: IrNode) -> IrNode {
    match place {
        IrNode::Local(local) => IrNode::Assign {
            local: *local,
            value: Box::new(value),
        },
        IrNode::CellGet(cell) => IrNode::CellSet {
            cell: cell.clone(),
            value: Box::new(value),
        },
        _ => unreachable!("an iterator record place is a local or a frame cell"),
    }
}

/// Emit `place = value`, discarding the value.
fn store(
    function: &mut Function,
    place: &IrNode,
    value: IrNode,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(
        &place_set(place, value),
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function; Drop);
}

/// Push the record place `place`.
fn load(
    function: &mut Function,
    place: &IrNode,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(place, function, scratch, strings, helpers);
}

/// Push i32 `1` when the record's state is `state`.
fn state_is(
    function: &mut Function,
    rec: &IterRecord,
    state: f64,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    load(function, &rec.state, scratch, strings, helpers);
    wasm!(function; I64Const(st(state)), I64Eq);
}

/// Push i32 `1` when none of `intrinsics` has its dirty bit set (always,
/// when `sealed`).
fn bits_clear(function: &mut Function, intrinsics: &[Intrinsic], sealed: bool) {
    let mask = intrinsics
        .iter()
        .filter_map(|intrinsic| intrinsic.watch_bit())
        .fold(0, |mask, bit| mask | (1 << bit));
    if sealed || mask == 0 {
        wasm!(function; I32Const(1));
    } else {
        wasm!(function; GlobalGet(DIRTY_GLOBAL), I32Const(mask), I32And, I32Eqz);
    }
}

/// Call the internal protocol builtin `builtin` with `this` = i64 `this`
/// and `env` = i64 `env` (`0` when `None`), no arguments, leaving its
/// result (a throw left pending).
fn call_internal(
    function: &mut Function,
    builtin: Builtin,
    this: u32,
    env: Option<u32>,
    helpers: Helpers<'_>,
) {
    for _ in 0..helpers.max_args {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    if let Some(env) = env {
        wasm!(function; LocalGet(env));
    } else {
        wasm!(function; I64Const(0));
    }
    emit_argc(0, function);
    wasm!(function; Call(helpers.builtin(builtin)));
}

/// Push the record state of the fast path the jsval in i64 `v` takes
/// (design/symbol-strings.md §5.3), or [`ST_PROTOCOL`]: each guard proves
/// the builtin `@@iterator` and the builtin `%XIteratorPrototype%.next` (and
/// no `return` on `%IteratorPrototype%`) through the watched dirty bits,
/// unless `sealed`. Without dirty tracking (and not `sealed`) the bits prove
/// nothing, so every value takes the protocol. An `arguments` object takes
/// the array walk while its own `@@iterator` is still the data property
/// `%Array.prototype.values%` (read without a getter call). A generator
/// whose body has a `yield*` ([`abi::GEN_DELEGATES`]) takes the protocol.
/// `tmp` is a free i32 local.
pub(in crate::emit) fn emit_iter_classify(
    function: &mut Function,
    v: u32,
    sealed: bool,
    tmp: u32,
    helpers: Helpers<'_>,
) {
    if !sealed && !helpers.dirty_tracking {
        wasm!(function; I64Const(st(ST_PROTOCOL)));
        return;
    }
    let iterator = Intrinsic::IteratorPrototype;
    let family = Intrinsic::IterPrototype;
    let arms: [(ProtoKind, Intrinsic, f64); 4] = [
        (ProtoKind::Array, family(IterFamily::Array), ST_ARRAY),
        (ProtoKind::Map, family(IterFamily::Map), ST_MAP),
        (ProtoKind::Set, family(IterFamily::Set), ST_SET),
        (ProtoKind::Generator, iterator, ST_GEN),
    ];
    emit_is_string(function, v);
    bits_clear(
        function,
        &[
            Intrinsic::Prototype(BuiltinCtor::String),
            family(IterFamily::String),
            iterator,
        ],
        sealed,
    );
    wasm!(function; I32And, If(BlockType::Result(ValType::I64)), I64Const(st(ST_STRING)), Else);
    for (kind, prototype, state) in arms {
        emit_proto_kind_test(function, v, kind, sealed, tmp, helpers);
        if kind == ProtoKind::Generator {
            // A body with a `yield*` passes delegate result objects through,
            // which only the protocol (`%GeneratorPrototype%.next`) reads.
            wasm!(function;
                If(BlockType::Result(ValType::I32)),
                LocalGet(v), I32WrapI64,
                I32Load(MemArg { offset: u64::from(abi::GEN_LIMITS_OFFSET), align: 2, memory_index: 0 }),
                I32Const(abi::GEN_DELEGATES.cast_signed()), I32And, I32Eqz,
                Else, I32Const(0), End,
            );
        }
        bits_clear(function, &[prototype, iterator], sealed);
        wasm!(function; I32And, If(BlockType::Result(ValType::I64)), I64Const(st(state)), Else);
    }
    emit_is_wrapper(function, v, abi::CLASS_ARGUMENTS);
    bits_clear(function, &[family(IterFamily::Array), iterator], sealed);
    let values = Builtin::Iter(IterOp::ArrayIter(IterKind::Values));
    wasm!(function;
        I32And, If(BlockType::Result(ValType::I64)),
        LocalGet(v), I32WrapI64, I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.prop_find), LocalTee(tmp),
        If(BlockType::Result(ValType::I32)),
        LocalGet(tmp),
        I64Load(MemArg { offset: u64::from(abi::PROP_VALUE_OFFSET), align: 3, memory_index: 0 }),
        I64Const(realm::method_bits(values, helpers)), I64Eq,
        Else, I32Const(0), End,
        If(BlockType::Result(ValType::I64)), I64Const(st(ST_ARGS)),
        Else, I64Const(st(ST_PROTOCOL)), End,
        Else, I64Const(st(ST_PROTOCOL)), End,
    );
    for _ in 0..=arms.len() {
        wasm!(function; End);
    }
}

/// Open the protocol iterator of the jsval in i64 `v` into `rec` (state
/// [`ST_PROTOCOL`]): `GetMethod(v, @@iterator)`, the [`ProtocolOp::Open`]
/// call (`not iterable`, the call, the object check) and `next` read once.
/// A throw propagates, leaving the record done. `method` is a free i64 local.
pub(in crate::emit) fn emit_iter_open(
    function: &mut Function,
    v: u32,
    method: u32,
    rec: &IterRecord,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(v), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    emit_propagate_if_pending(function, helpers);
    emit_iter_open_with(function, v, method, rec, scratch, strings, helpers);
}

/// [`emit_iter_open`] with the `@@iterator` method already in i64 `method`
/// (clobbered).
pub(in crate::emit) fn emit_iter_open_with(
    function: &mut Function,
    v: u32,
    method: u32,
    rec: &IterRecord,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    call_internal(
        function,
        Builtin::Protocol(ProtocolOp::Open),
        v,
        Some(method),
        helpers,
    );
    wasm!(function; LocalSet(method));
    emit_propagate_if_pending(function, helpers);
    store(
        function,
        &rec.iter,
        IrNode::Local(method),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(method), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(method));
    emit_propagate_if_pending(function, helpers);
    store(
        function,
        &rec.next,
        IrNode::Local(method),
        scratch,
        strings,
        helpers,
    );
    store(
        function,
        &rec.state,
        IrNode::Num(ST_PROTOCOL),
        scratch,
        strings,
        helpers,
    );
}

/// Emit [`IrNode::GetIterator`]: evaluate `value`, then open it into `rec`
/// on its fast path (the source itself, cursor `0`) or through the protocol.
pub(in crate::emit) fn emit_get_iterator(
    value: &IrNode,
    rec: &IterRecord,
    sealed: bool,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, kind, method) = (scratch + 22, scratch + 26, scratch + 24);
    emit_node(value, function, scratch, strings, helpers);
    wasm!(function; LocalSet(v));
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    emit_iter_classify(function, v, sealed, scratch + 3, helpers);
    wasm!(function; LocalTee(kind), I64Const(st(ST_PROTOCOL)), I64Eq, If(BlockType::Empty));
    enter_frame(helpers);
    emit_iter_open(function, v, method, rec, scratch, strings, helpers);
    leave_frame(helpers);
    wasm!(function; Else);
    store(
        function,
        &rec.iter,
        IrNode::Local(v),
        scratch,
        strings,
        helpers,
    );
    store(
        function,
        &rec.next,
        IrNode::Num(0.0),
        scratch,
        strings,
        helpers,
    );
    store(
        function,
        &rec.state,
        IrNode::Local(kind),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; End);
}

/// Emit [`IrNode::OpenIterator`]: evaluate `value`, then open its protocol
/// iterator into `rec` (no fast path; a throw leaves the record done).
pub(in crate::emit) fn emit_open_iterator(
    value: &IrNode,
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, method) = (scratch + 22, scratch + 24);
    emit_node(value, function, scratch, strings, helpers);
    wasm!(function; LocalSet(v));
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    emit_iter_open(function, v, method, rec, scratch, strings, helpers);
}

/// Emit [`IrNode::IteratorStep`], pushing i32 `1` when the record produced
/// a value (in `rec.result`), `0` once done. The fast paths run in the
/// order array, string, Map, Set, generator, `arguments`.
pub(in crate::emit) fn emit_iterator_step(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; Block(BlockType::Result(ValType::I32)));
    enter_frame(helpers);
    for state in [
        ST_ARRAY,
        ST_STRING,
        ST_MAP,
        ST_SET,
        ST_GEN,
        ST_ARGS,
        ST_PROTOCOL,
    ] {
        state_is(function, rec, state, scratch, strings, helpers);
        wasm!(function; If(BlockType::Empty));
        enter_frame(helpers);
        if state == ST_ARRAY || state == ST_ARGS {
            step_indexed(rec, function, scratch, strings, helpers);
        } else if state == ST_STRING {
            step_code_point(rec, function, scratch, strings, helpers);
        } else if state == ST_MAP || state == ST_SET {
            step_entry(rec, state == ST_SET, function, scratch, strings, helpers);
        } else if state == ST_GEN {
            step_generator(rec, function, scratch, strings, helpers);
        } else {
            step_protocol(rec, function, scratch, strings, helpers);
        }
        leave_frame(helpers);
        wasm!(function; Br(1), End);
    }
    leave_frame(helpers);
    wasm!(function; I32Const(0), End);
}

/// The record is done: `state = done`, push i32 `0`.
fn step_done(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    store(
        function,
        &rec.state,
        IrNode::Num(ST_DONE),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(0));
}

/// An array / `arguments` step: `next < length` (re-read per step) reads
/// `iter[next]` (a hole through the prototype chain) and advances.
fn step_indexed(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let step = IrNode::Ternary {
        cond: Box::new(IrNode::Binary {
            op: BinOp::Lt,
            lhs: Box::new(rec.next.clone()),
            rhs: Box::new(IrNode::StrLen(Box::new(rec.iter.clone()))),
        }),
        then_branch: Box::new(IrNode::Seq(vec![
            place_set(
                &rec.result,
                IrNode::Index {
                    obj: Box::new(rec.iter.clone()),
                    index: Box::new(rec.next.clone()),
                },
            ),
            place_set(
                &rec.next,
                IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(rec.next.clone()),
                    rhs: Box::new(IrNode::Num(1.0)),
                },
            ),
            IrNode::Bool(true),
        ])),
        else_branch: Box::new(IrNode::Seq(vec![
            place_set(&rec.state, IrNode::Num(ST_DONE)),
            IrNode::Bool(false),
        ])),
    };
    emit_node(&step, function, scratch, strings, helpers);
    wasm!(function; I64Const(TRUE_BITS), I64Eq);
}

/// A string step: the next code point's string (`__str_iter_next`).
fn step_code_point(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, i, len, k, out) = (
        scratch + 8,
        scratch + 9,
        scratch + 10,
        scratch + 11,
        scratch + 24,
    );
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function;
        I32WrapI64, LocalTee(s),
        I32Load(MemArg { offset: u64::from(abi::HEADER_SIZE), align: 2, memory_index: 0 }),
        LocalSet(len),
    );
    load(function, &rec.next, scratch, strings, helpers);
    wasm!(function;
        F64ReinterpretI64, I32TruncSatF64U, LocalTee(i), LocalGet(len), I32GeU,
        If(BlockType::Result(ValType::I32)),
    );
    step_done(rec, function, scratch, strings, helpers);
    wasm!(function; Else);
    emit_str_iter_next(function, s, i, len, k, helpers);
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out));
    store(
        function,
        &rec.result,
        IrNode::Local(out),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(i), F64ConvertI32U, I64ReinterpretF64, LocalSet(out));
    store(
        function,
        &rec.next,
        IrNode::Local(out),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(1), End);
}

/// A Map / Set step: the next live backing entry (tombstones skipped; the
/// slot count re-read), as `[key, value]` / the value.
fn step_entry(
    rec: &IterRecord,
    set: bool,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (handle, key, value, i, at) = (
        scratch + 22,
        scratch + 24,
        scratch + 25,
        scratch + 9,
        scratch + 10,
    );
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function; LocalSet(handle));
    load(function, &rec.next, scratch, strings, helpers);
    wasm!(function; F64ReinterpretI64, I32TruncSatF64U, LocalSet(i));
    map_set::skip_tombstones(function, handle, i, at, key, value);
    wasm!(function; LocalGet(i));
    map_set::entries_len(function, handle);
    wasm!(function; I32GeU, If(BlockType::Result(ValType::I32)));
    step_done(rec, function, scratch, strings, helpers);
    wasm!(function; Else);
    map_set::load_entry(function, handle, i, at, key, value);
    if !set {
        let entry = IrNode::Array(vec![IrNode::Local(key), IrNode::Local(value)]);
        emit_node(&entry, function, scratch, strings, helpers);
        wasm!(function; LocalSet(key));
    }
    store(
        function,
        &rec.result,
        IrNode::Local(key),
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(i), I32Const(2), I32Add, F64ConvertI32U, I64ReinterpretF64, LocalSet(value),
    );
    store(
        function,
        &rec.next,
        IrNode::Local(value),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(1), End);
}

/// A generator step: one direct resume of its state machine (no result
/// object); its `done` flag ends the walk without the return value.
fn step_generator(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (handle, value) = (scratch + 22, scratch + 23);
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function; LocalSet(handle));
    crate::emit::generator::emit_gen_drive(
        handle,
        &IrNode::Undefined,
        value,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(handle), I32WrapI64,
        I32Load(MemArg { offset: GEN_DONE_OFFSET, align: 2, memory_index: 0 }),
        If(BlockType::Result(ValType::I32)),
    );
    step_done(rec, function, scratch, strings, helpers);
    wasm!(function; Else);
    store(
        function,
        &rec.result,
        IrNode::Local(value),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(1), End);
}

/// A protocol step ([`ProtocolOp::Step`]) with the record marked stepping
/// for its duration, so a throw from `next` leaves it unclosed; a produced
/// result leaves it open again, so an elision (a step without a value read)
/// still closes it.
fn step_protocol(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (iter, next) = (scratch + 22, scratch + 24);
    store(
        function,
        &rec.state,
        IrNode::Num(ST_STEPPING),
        scratch,
        strings,
        helpers,
    );
    load(function, &rec.iter, scratch, strings, helpers);
    wasm!(function; LocalSet(iter));
    load(function, &rec.next, scratch, strings, helpers);
    wasm!(function; LocalSet(next));
    call_internal(
        function,
        Builtin::Protocol(ProtocolOp::Step),
        iter,
        Some(next),
        helpers,
    );
    wasm!(function; LocalSet(next));
    emit_propagate_if_pending(function, helpers);
    wasm!(function; LocalGet(next), I64Const(HOLE_BITS), I64Eq, If(BlockType::Result(ValType::I32)));
    step_done(rec, function, scratch, strings, helpers);
    wasm!(function; Else);
    store(
        function,
        &rec.result,
        IrNode::Local(next),
        scratch,
        strings,
        helpers,
    );
    store(
        function,
        &rec.state,
        IrNode::Num(ST_PROTOCOL),
        scratch,
        strings,
        helpers,
    );
    wasm!(function; I32Const(1), End);
}

/// Emit [`IrNode::IteratorValue`], pushing the stepped value: the fast
/// paths' value itself, or `Get(result, "value")` of a protocol result
/// (with the record marked stepping for the read, so a throw from it leaves
/// the record unclosed). Read once per successful step.
pub(in crate::emit) fn emit_iterator_value(
    rec: &IterRecord,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    state_is(function, rec, ST_PROTOCOL, scratch, strings, helpers);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    let read = IrNode::Seq(vec![
        place_set(&rec.state, IrNode::Num(ST_STEPPING)),
        place_set(
            &rec.result,
            IrNode::Index {
                obj: Box::new(rec.result.clone()),
                index: Box::new(IrNode::Str("value".to_owned())),
            },
        ),
        place_set(&rec.state, IrNode::Num(ST_PROTOCOL)),
        rec.result.clone(),
    ]);
    emit_node(&read, function, scratch, strings, helpers);
    leave_frame(helpers);
    wasm!(function; Else);
    load(function, &rec.result, scratch, strings, helpers);
    wasm!(function; End);
}

/// Emit `IteratorClose` of `rec` (design/symbol-strings.md §5.1) under
/// `mode`: an open protocol record calls [`ProtocolOp::Close`] and a
/// generator its builtin `return()` (the guard proved `return` is
/// `%GeneratorPrototype%.return`); a done or stepping record, or one on
/// another fast path (whose iterator has no `return`), does nothing. The
/// record is done afterwards.
pub(in crate::emit) fn emit_iterator_close(
    rec: &IterRecord,
    mode: CloseMode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (iter, saved) = (scratch + 22, scratch + 25);
    for (state, builtin) in [
        (ST_PROTOCOL, Builtin::Protocol(ProtocolOp::Close)),
        (ST_GEN, Builtin::GeneratorReturn),
    ] {
        state_is(function, rec, state, scratch, strings, helpers);
        wasm!(function; If(BlockType::Empty));
        enter_frame(helpers);
        if mode == CloseMode::Pending {
            wasm!(function;
                GlobalGet(EXC_VALUE_GLOBAL), LocalSet(saved),
                I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
            );
        }
        store(
            function,
            &rec.state,
            IrNode::Num(ST_DONE),
            scratch,
            strings,
            helpers,
        );
        load(function, &rec.iter, scratch, strings, helpers);
        wasm!(function; LocalSet(iter));
        call_internal(function, builtin, iter, None, helpers);
        wasm!(function; Drop);
        match mode {
            CloseMode::Normal => emit_propagate_if_pending(function, helpers),
            CloseMode::Catch => wasm!(function; I32Const(0), GlobalSet(EXC_PENDING_GLOBAL)),
            CloseMode::Pending => wasm!(function;
                LocalGet(saved), GlobalSet(EXC_VALUE_GLOBAL),
                I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
            ),
        }
        leave_frame(helpers);
        wasm!(function; End);
    }
}

/// Emit the record locals of a builtin body as a record over i64 locals.
pub(super) fn local_record(iter: u32, next: u32, state: u32, result: u32) -> IterRecord {
    IterRecord {
        iter: IrNode::Local(iter),
        next: IrNode::Local(next),
        state: IrNode::Local(state),
        result: IrNode::Local(result),
    }
}

/// Run `body` under a handler: a throw in it (propagated, not returned)
/// lands after it with the exception pending, so the caller can close an
/// iterator before returning.
pub(super) fn guarded(
    function: &mut Function,
    helpers: Helpers<'_>,
    body: impl FnOnce(&mut Function),
) {
    wasm!(function; Block(BlockType::Empty));
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.depth += 1;
        let depth = unwind.depth;
        unwind.handlers.push(depth);
    }
    body(function);
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.handlers.pop();
        unwind.depth -= 1;
    }
    wasm!(function; End);
}

/// `<x> is not a function` for the jsval in `local` (`String(x)` of a
/// primitive, `typeof x` of an object), thrown as a `TypeError` (`tmp` an
/// i64 temp).
fn not_a_function(local: u32, tmp: u32) -> IrNode {
    let name = IrNode::Ternary {
        cond: Box::new(IrNode::IsObject(Box::new(IrNode::Local(local)))),
        then_branch: Box::new(IrNode::TypeOf(Box::new(IrNode::Local(local)))),
        else_branch: Box::new(describe(local)),
    };
    throw_type(
        concat(name, IrNode::Str(" is not a function".to_owned())),
        tmp,
    )
}

/// `Iterator result <x> is not an object` for the non-object jsval in
/// `local`, thrown as a `TypeError` (`tmp` an i64 temp).
fn result_not_object(local: u32, tmp: u32) -> IrNode {
    let message = concat(
        concat(IrNode::Str("Iterator result ".to_owned()), describe(local)),
        IrNode::Str(" is not an object".to_owned()),
    );
    throw_type(message, tmp)
}

/// `String(x)` of the primitive in `local` (a symbol is described).
fn describe(local: u32) -> IrNode {
    IrNode::ConvertCall {
        op: ConvertOp::String,
        arg: Box::new(IrNode::Local(local)),
        radix: None,
    }
}

/// `a + b`.
fn concat(a: IrNode, b: IrNode) -> IrNode {
    IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}

/// Throw a fresh `TypeError` whose message is the string value of `message`.
fn throw_type(message: IrNode, tmp: u32) -> IrNode {
    IrNode::Seq(vec![
        IrNode::Assign {
            local: tmp,
            value: Box::new(message),
        },
        IrNode::Throw(Box::new(IrNode::error_object(
            ErrorKind::TypeError,
            Some(IrNode::Local(tmp)),
            concat(IrNode::Str("TypeError: ".to_owned()), IrNode::Local(tmp)),
        ))),
    ])
}

/// Throw `node` (an IR throw) from a builtin body and return.
fn throw_and_return(
    function: &mut Function,
    frame: Frame,
    node: &IrNode,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_drop(function, frame, node, strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), Return);
}

/// Call the function in i64 `callee` with `this` = i64 `this` and no
/// arguments into i64 `out`, returning on a throw.
fn call_method(
    function: &mut Function,
    frame: Frame,
    callee: u32,
    this: u32,
    out: u32,
    helpers: Helpers<'_>,
) {
    for _ in 0..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    invoke(function, callee, |function| emit_argc(0, function), helpers);
    wasm!(function; LocalSet(out));
    return_if_pending(function);
}

/// Emit the body of the internal protocol builtin `op`.
pub(super) fn emit_protocol(
    op: ProtocolOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match op {
        ProtocolOp::Open => emit_open_body(function, frame, strings, helpers),
        ProtocolOp::Step => emit_step_body(function, frame, strings, helpers),
        ProtocolOp::Close => emit_close_body(function, frame, strings, helpers),
        ProtocolOp::ToList => emit_to_list_body(function, frame, strings, helpers),
    }
}

/// The [`ProtocolOp::Open`] body: `this` is the iterated value, `env` its
/// `@@iterator` method; `<x> is not iterable` unless callable, else the
/// iterator its call returns (a `TypeError` unless an object).
fn emit_open_body(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, it, method) = (frame.i64(0), frame.i64(1), frame.k);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(v));
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(function, frame, &map_set::not_iterable(v), strings, helpers);
    wasm!(function; End);
    call_method(function, frame, method, v, it, helpers);
    emit_is_object(function, it);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, ITERATOR_NOT_OBJECT, strings, helpers);
    wasm!(function; End, LocalGet(it));
}

/// The [`ProtocolOp::Step`] body: `this` is the iterator, `env` its cached
/// `next`; the result object of `next()` (a `TypeError` unless callable /
/// unless the result is an object), or the hole when `ToBoolean(result.done)`.
fn emit_step_body(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (it, r, done, tmp, next) = (
        frame.i64(0),
        frame.i64(1),
        frame.i64(2),
        frame.i64(3),
        frame.k,
    );
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(it));
    emit_is_function(function, next);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &not_a_function(next, tmp),
        strings,
        helpers,
    );
    wasm!(function; End);
    call_method(function, frame, next, it, r, helpers);
    emit_is_object(function, r);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &result_not_object(r, tmp),
        strings,
        helpers,
    );
    wasm!(function; End, LocalGet(r), I32Const(static_key(strings, "done")), Call(helpers.get), LocalSet(done));
    return_if_pending(function);
    let truthy = IrNode::ConvertCall {
        op: ConvertOp::ToBoolean,
        arg: Box::new(IrNode::Local(done)),
        radix: None,
    };
    emit_node(&truthy, function, frame.scratch(), strings, helpers);
    wasm!(function;
        I64Const(TRUE_BITS), I64Eq,
        If(BlockType::Result(ValType::I64)), I64Const(HOLE_BITS), Else, LocalGet(r), End,
    );
}

/// The [`ProtocolOp::Close`] body (a non-throw completion): `this` is the
/// iterator; `GetMethod(this, "return")` (nothing when nullish, a
/// `TypeError` unless callable), its call, and a `TypeError` unless the
/// result is an object.
fn emit_close_body(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (it, method, r, tmp) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wasm!(function;
        GlobalGet(THIS_GLOBAL), LocalTee(it),
        I32Const(static_key(strings, "return")), Call(helpers.get), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_nullish(function, method);
    wasm!(function; If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End);
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &not_a_function(method, tmp),
        strings,
        helpers,
    );
    wasm!(function; End);
    call_method(function, frame, method, it, r, helpers);
    emit_is_object(function, r);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_and_return(
        function,
        frame,
        &result_not_object(r, tmp),
        strings,
        helpers,
    );
    wasm!(function; End, I64Const(UNDEFINED_BITS));
}

/// The [`ProtocolOp::ToList`] body: `this` is the iterated value; a fresh
/// array of every value its iterator record yields (on its guarded fast
/// path, e.g. a clean `arguments` walked by index, or through the
/// protocol; never closed: a throw marks it done).
fn emit_to_list_body(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, value) = (frame.i64(0), frame.i64(2));
    let rec = local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let (list, scratch) = (frame.i32(0), frame.scratch());
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(v));
    emit_get_iterator(
        &IrNode::Local(v),
        &rec,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    wasm!(function;
        I32Const(0), Call(helpers.new_array()), LocalSet(list),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    enter_frame(helpers);
    enter_frame(helpers);
    emit_iterator_step(&rec, function, scratch, strings, helpers);
    wasm!(function; I32Eqz, BrIf(1));
    emit_iterator_value(&rec, function, scratch, strings, helpers);
    wasm!(function;
        LocalSet(value),
        LocalGet(list), LocalGet(value), Call(helpers.array_push), Drop,
        Br(0),
    );
    leave_frame(helpers);
    leave_frame(helpers);
    wasm!(function;
        End, End,
        LocalGet(list), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
