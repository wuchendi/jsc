//! `%Map.prototype%` and `%Set.prototype%` methods, their `size` getters, and
//! the iterable argument of the `Map` / `Set` constructors. A method body
//! checks its receiver's kind, then runs the inline body its call sites'
//! fast paths share ([`IrNode::CollectionOp`]).

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `<name>` on a receiver of the wrong kind.
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

/// The receiver `TypeError`s, one per builtin of this family.
const RECEIVER_TEXTS: [(Builtin, CalleeErrorText<'static>); 13] = [
    (op(false, MapOp::Get), incompatible!("Map.prototype.get")),
    (op(false, MapOp::Set), incompatible!("Map.prototype.set")),
    (op(false, MapOp::Has), incompatible!("Map.prototype.has")),
    (
        op(false, MapOp::Delete),
        incompatible!("Map.prototype.delete"),
    ),
    (
        op(false, MapOp::Clear),
        incompatible!("Map.prototype.clear"),
    ),
    (
        Builtin::CollectionForEach { set: false },
        incompatible!("Map.prototype.forEach"),
    ),
    (
        Builtin::CollectionSize { set: false },
        incompatible!("get Map.prototype.size"),
    ),
    (op(true, MapOp::Add), incompatible!("Set.prototype.add")),
    (op(true, MapOp::Has), incompatible!("Set.prototype.has")),
    (
        op(true, MapOp::Delete),
        incompatible!("Set.prototype.delete"),
    ),
    (op(true, MapOp::Clear), incompatible!("Set.prototype.clear")),
    (
        Builtin::CollectionForEach { set: true },
        incompatible!("Set.prototype.forEach"),
    ),
    (
        Builtin::CollectionSize { set: true },
        incompatible!("get Set.prototype.size"),
    ),
];

/// The non-callable `forEach` callback `TypeError`s (Map, Set).
const NOT_CALLABLE: [CalleeErrorText<'static>; 2] = [
    [
        "Map.prototype.forEach callback is not a function",
        "TypeError: Map.prototype.forEach callback is not a function",
    ],
    [
        "Set.prototype.forEach callback is not a function",
        "TypeError: Set.prototype.forEach callback is not a function",
    ],
];

/// The entry keys a `Map` constructor reads from each iterated entry.
const ENTRY_KEYS: [&str; 2] = ["0", "1"];

/// `CollectionOp { set, op }`.
const fn op(set: bool, op: MapOp) -> Builtin {
    Builtin::CollectionOp { set, op }
}

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    RECEIVER_TEXTS
        .into_iter()
        .flat_map(|(_, text)| text)
        .chain(NOT_CALLABLE.into_iter().flatten())
        .chain(ENTRY_KEYS)
        .chain(["set", "add"])
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    let local = frame.i64(0);
    vec![
        not_iterable(local),
        not_an_entry(local),
        adder_not_callable(local, false),
        adder_not_callable(local, true),
    ]
}

/// The receiver `TypeError` of `builtin`.
fn receiver_text(builtin: Builtin) -> CalleeErrorText<'static> {
    RECEIVER_TEXTS
        .iter()
        .find_map(|&(b, text)| (b == builtin).then_some(text))
        .unwrap_or(RECEIVER_TEXTS[0].1)
}

/// Throw a fresh `TypeError` whose message is the string value of `message`.
fn throw_message(message: IrNode, tmp: u32) -> IrNode {
    IrNode::Seq(vec![
        IrNode::Assign {
            local: tmp,
            value: Box::new(message),
        },
        IrNode::Throw(Box::new(IrNode::error_object(
            ErrorKind::TypeError,
            Some(IrNode::Local(tmp)),
            concat(IrNode::Str("TypeError: ".into()), IrNode::Local(tmp)),
        ))),
    ])
}

/// `a + b`.
fn concat(a: IrNode, b: IrNode) -> IrNode {
    IrNode::Binary {
        op: BinOp::Add,
        lhs: Box::new(a),
        rhs: Box::new(b),
    }
}

/// The `TypeError` for an iterated value in `local` that is not iterable:
/// `object is not iterable` (`function is not iterable`), `null is not
/// iterable` / `undefined is not iterable`, or `<type> <value> is not
/// iterable` for another primitive.
pub(super) fn not_iterable(local: u32) -> IrNode {
    let kind = IrNode::TypeOf(Box::new(IrNode::Local(local)));
    let primitive = concat(
        concat(
            concat(kind.clone(), IrNode::Str(" ".into())),
            IrNode::Local(local),
        ),
        IrNode::Str(" is not iterable".into()),
    );
    let nullish = IrNode::Binary {
        op: BinOp::LooseEq,
        lhs: Box::new(IrNode::Local(local)),
        rhs: Box::new(IrNode::Null),
    };
    let message = IrNode::Ternary {
        cond: Box::new(IrNode::IsObject(Box::new(IrNode::Local(local)))),
        then_branch: Box::new(concat(kind, IrNode::Str(" is not iterable".into()))),
        else_branch: Box::new(IrNode::Ternary {
            cond: Box::new(nullish),
            then_branch: Box::new(concat(
                IrNode::Local(local),
                IrNode::Str(" is not iterable".into()),
            )),
            else_branch: Box::new(primitive),
        }),
    };
    throw_message(message, local)
}

/// The `TypeError` for a `Map` constructor (or `Object.fromEntries`) entry in
/// `local` that is not an object.
pub(super) fn not_an_entry(local: u32) -> IrNode {
    let message = concat(
        concat(IrNode::Str("Iterator value ".into()), IrNode::Local(local)),
        IrNode::Str(" is not an entry object".into()),
    );
    throw_message(message, local)
}

/// The `TypeError` for a non-callable adder (`map.set` / `set.add`) in
/// `local`.
fn adder_not_callable(local: u32, set: bool) -> IrNode {
    let (key, object) = if set { ("add", "Set") } else { ("set", "Map") };
    let message = concat(
        concat(IrNode::Str("'".into()), IrNode::Local(local)),
        IrNode::Str(
            format!("' returned for property '{key}' of object '#<{object}>' is not a function")
                .into(),
        ),
    );
    throw_message(message, local)
}

/// Read `this` into `this` and throw `builtin`'s receiver `TypeError` unless
/// it is a `Map` (or, for `set`, a `Set`) handle.
fn this_collection(
    function: &mut Function,
    frame: Frame,
    this: u32,
    builtin: Builtin,
    set: bool,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function;
        I32Const(if set { TYPE_SET } else { TYPE_MAP }), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, receiver_text(builtin), strings, helpers);
    wasm!(function; End);
}

/// Push the slot count (two per entry, tombstones included) of the backing
/// array of the handle in `handle`.
pub(super) fn entries_len(function: &mut Function, handle: u32) {
    wasm!(function;
        LocalGet(handle), I32WrapI64,
        I64Load(MemArg { offset: MAP_ENTRIES_OFFSET, align: 3, memory_index: 0 }), I32WrapI64,
        I32Load(MemArg { offset: ARRAY_LEN_OFFSET, align: 2, memory_index: 0 }),
    );
}

/// Load the key and value of the backing slot pair at i32 index `i` of the
/// handle in `handle` into `key` / `value` (`at` an i32 temp).
pub(super) fn load_entry(
    function: &mut Function,
    handle: u32,
    i: u32,
    at: u32,
    key: u32,
    value: u32,
) {
    wasm!(function;
        LocalGet(handle), I32WrapI64,
        I64Load(MemArg { offset: MAP_ENTRIES_OFFSET, align: 3, memory_index: 0 }), I32WrapI64,
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, LocalTee(at),
        I64Load(elem_slot(0)), LocalSet(key),
        LocalGet(at), I64Load(elem_slot(1)), LocalSet(value),
    );
}

/// Advance the i32 backing slot index `i` of the handle in `handle` past
/// tombstones (a `HOLE` key, DESIGN.md §5.2), stopping at the
/// next live entry or the end (`at`, `key`, `value` temps).
pub(super) fn skip_tombstones(
    function: &mut Function,
    handle: u32,
    i: u32,
    at: u32,
    key: u32,
    value: u32,
) {
    wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty), LocalGet(i));
    entries_len(function, handle);
    wasm!(function; I32GeU, BrIf(1));
    load_entry(function, handle, i, at, key, value);
    wasm!(function;
        LocalGet(key), I64Const(HOLE_BITS), I64Ne, BrIf(1),
        LocalGet(i), I32Const(2), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
}

/// A `Map.prototype` / `Set.prototype` method with an inline body.
pub(super) fn emit_op(
    set: bool,
    op: MapOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, a0, a1) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let builtin = Builtin::CollectionOp { set, op };
    this_collection(function, frame, this, builtin, set, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    frame.arg(function, 1);
    wasm!(function; LocalSet(a1));
    let node = IrNode::CollectionOp {
        op,
        handle: this,
        args: vec![IrNode::Local(a0), IrNode::Local(a1)],
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}

/// The `size` getter.
pub(super) fn emit_size(
    set: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    let builtin = Builtin::CollectionSize { set };
    this_collection(function, frame, this, builtin, set, strings, helpers);
    emit_u32_field_boxed(function, this, MAP_SIZE_OFFSET);
}

/// `forEach(callback, thisArg)`: `callback(value, key, map)` (a Set passes
/// its value as both) with `this = thisArg` for each live entry in insertion
/// order, walking the backing live (DESIGN.md §5.2): the slot
/// count is re-read each step so entries added by a callback are visited,
/// and tombstones (deleted or cleared entries) are skipped in place, so a
/// deletion never shifts the walk.
pub(super) fn emit_for_each(
    set: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, callback, this_arg, key) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let value = frame.x64(0);
    let (i, at) = (frame.i32(0), frame.i32(1));
    let builtin = Builtin::CollectionForEach { set };
    this_collection(function, frame, this, builtin, set, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(callback));
    emit_is_function(function, callback);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(
        function,
        frame,
        NOT_CALLABLE[usize::from(set)],
        strings,
        helpers,
    );
    wasm!(function; End);
    frame.arg(function, 1);
    wasm!(function;
        LocalSet(this_arg),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i),
    );
    entries_len(function, this);
    wasm!(function; I32GeU, BrIf(1));
    load_entry(function, this, i, at, key, value);
    wasm!(function; LocalGet(key), I64Const(HOLE_BITS), I64Ne, If(BlockType::Empty));
    let args = if set {
        [key, key, this]
    } else {
        [value, key, this]
    };
    for slot in 0..frame.k {
        if let Some(&local) = args.get(slot as usize) {
            wasm!(function; LocalGet(local));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    wasm!(function; LocalGet(this_arg), GlobalSet(THIS_GLOBAL));
    let count = args.len().min(frame.k as usize);
    invoke(
        function,
        callback,
        |function| emit_argc(count, function),
        helpers,
    );
    wasm!(function; Drop);
    return_if_pending(function);
    wasm!(function;
        End,
        LocalGet(i), I32Const(2), I32Add, LocalSet(i),
        Br(0),
        End, End,
        I64Const(UNDEFINED_BITS),
    );
}

/// Call the adder in i64 `adder` with `this` = the collection in i64 `map`
/// on `args` (a Set's one value, a Map's key and value): inline
/// `__map_set` when i32 `builtin_adder` is set, else a call (a throw left
/// pending).
fn call_adder(
    function: &mut Function,
    frame: Frame,
    map: u32,
    adder: u32,
    builtin_adder: u32,
    args: &[u32],
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(builtin_adder), If(BlockType::Empty), LocalGet(map), I32WrapI64);
    for &arg in args {
        wasm!(function; LocalGet(arg));
    }
    if args.len() == 1 {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; Call(helpers.map_set), Else);
    for slot in 0..frame.k {
        if let Some(&local) = args.get(slot as usize) {
            wasm!(function; LocalGet(local));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    wasm!(function; LocalGet(map), GlobalSet(THIS_GLOBAL));
    let count = args.len().min(frame.k as usize);
    invoke(
        function,
        adder,
        |function| emit_argc(count, function),
        helpers,
    );
    wasm!(function; Drop, End);
}

/// `new Map(iterable)` / `new Set(iterable)`: the allocation of
/// [`super::ctor::fragment`], then, for a non-nullish `iterable`,
/// `AddEntriesFromIterable` (DESIGN.md §7.8): the adder
/// `map.set` / `set.add` is read once from the new collection (a user
/// override runs; the builtin itself is called inline), then each value of
/// `iterable`'s iterator record (a guarded fast path or the protocol; `<x> is
/// not iterable` without one) is added. A `Map` entry must be an object,
/// read at keys `"0"` and `"1"`. A non-object entry (`TypeError`), a
/// throwing entry read or a throwing adder closes the iterator; a throw from
/// the iterator itself does not.
pub(super) fn emit_ctor(
    set: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (map, src) = (frame.x64(0), frame.x64(1));
    let (adder, item, key, value) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let builtin_adder = frame.i32(1);
    let rec = iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    let ctor = if set {
        BuiltinCtor::Set
    } else {
        BuiltinCtor::Map
    };
    let alloc = super::ctor::fragment(ctor, frame.k);
    emit_node(&alloc, function, scratch, strings, helpers);
    wasm!(function; LocalSet(map));
    return_if_pending(function);
    frame.arg(function, 0);
    wasm!(function; LocalSet(src));
    emit_is_nullish(function, src);
    wasm!(function; If(BlockType::Empty), LocalGet(map), Return, End);
    let name = if set { "add" } else { "set" };
    wasm!(function;
        LocalGet(map), I32Const(static_key(strings, name)), Call(helpers.get), LocalSet(adder),
    );
    return_if_pending(function);
    emit_is_function(function, adder);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_drop(
        function,
        frame,
        &adder_not_callable(adder, set),
        strings,
        helpers,
    );
    let builtin = if set {
        op(true, MapOp::Add)
    } else {
        op(false, MapOp::Set)
    };
    wasm!(function;
        I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(adder), I64Const(realm::method_bits(builtin, helpers)), I64Eq, LocalSet(builtin_adder),
    );
    emit_get_iterator(
        &IrNode::Local(src),
        &rec,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    iter::guarded(function, helpers, |function| {
        wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
        enter_frame(helpers);
        enter_frame(helpers);
        emit_iterator_step(&rec, function, scratch, strings, helpers);
        wasm!(function; I32Eqz, BrIf(1));
        emit_iterator_value(&rec, function, scratch, strings, helpers);
        wasm!(function; LocalSet(item));
        if !set {
            emit_is_object(function, item);
            wasm!(function; I32Eqz, If(BlockType::Empty));
            enter_frame(helpers);
            emit_drop(function, frame, &not_an_entry(item), strings, helpers);
            leave_frame(helpers);
            wasm!(function; End);
            for (key_text, out) in ENTRY_KEYS.into_iter().zip([key, value]) {
                wasm!(function;
                    LocalGet(item), I32Const(static_key(strings, key_text)), Call(helpers.get),
                    LocalSet(out),
                );
                emit_propagate_if_pending(function, helpers);
            }
        }
        let args: &[u32] = if set { &[item] } else { &[key, value] };
        call_adder(function, frame, map, adder, builtin_adder, args, helpers);
        emit_propagate_if_pending(function, helpers);
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
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End, LocalGet(map));
}
