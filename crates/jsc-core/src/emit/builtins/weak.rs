//! The weak collections (design/runtime-memory.md §6): `%WeakMap.prototype%`
//! and `%WeakSet.prototype%` methods, the `WeakMap` / `WeakSet` constructors
//! and the shared `CanBeHeldWeakly` test; `WeakRef` (`deref`,
//! `KeepDuringJob`) and `FinalizationRegistry` (`register`, `unregister`).
//!
//! A handle (`TYPE_WEAKMAP` / `TYPE_WEAKSET`, [`abi::WEAK_BLOCK_SIZE`] bytes)
//! keeps its entries in a [`abi::ELEMS_FLAG_WEAK`] element store: `(key,
//! value)` pairs for a `WeakMap`, keys alone for a `WeakSet`. Every slot past the
//! live entries holds a `HOLE` key (and an `undefined` value), so lookup is a
//! linear identity scan of the whole store, `delete` tombstones its pair in
//! place, and `set` / `add` reuse the first `HOLE` or grow the store by
//! doubling. The collector (`emit/gc.rs`) never traces the store's slots: it
//! marks a value only while its key is live (the ephemeron fixpoint) and
//! compacts away the pairs of dead keys.
//!
//! A `WeakRef` ([`abi::TYPE_WEAKREF`]) holds its target in one weak slot the
//! collector clears; creating one and every successful `deref()` push the
//! target onto the `KeepDuringJob` array `$__kept_objects`, which the next
//! microtask checkpoint empties (`emit/promise_engine.rs`). A
//! `FinalizationRegistry` ([`abi::TYPE_FINREG`]) keeps `(target, held,
//! token)` cells in a weak element store of the same shape: `register`
//! reuses the first unused cell (`undefined` target) or grows the store,
//! `unregister` turns every cell of its token unused, and the collector
//! empties a cell of a dead target (`HOLE`) and queues the registry's
//! cleanup job.

#[allow(clippy::wildcard_imports)]
use super::*;

use super::map_set::{Collection, emit_add_from_iterable};
use crate::emit::realm::WeakCellOp;

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

/// The receiver `TypeError`s, one per method.
const RECEIVER_TEXTS: [(Builtin, CalleeErrorText<'static>); 10] = [
    (
        op(false, MapOp::Delete),
        incompatible!("WeakMap.prototype.delete"),
    ),
    (
        op(false, MapOp::Get),
        incompatible!("WeakMap.prototype.get"),
    ),
    (
        op(false, MapOp::Has),
        incompatible!("WeakMap.prototype.has"),
    ),
    (
        op(false, MapOp::Set),
        incompatible!("WeakMap.prototype.set"),
    ),
    (op(true, MapOp::Add), incompatible!("WeakSet.prototype.add")),
    (
        op(true, MapOp::Delete),
        incompatible!("WeakSet.prototype.delete"),
    ),
    (op(true, MapOp::Has), incompatible!("WeakSet.prototype.has")),
    (
        Builtin::WeakCell(WeakCellOp::Deref),
        incompatible!("WeakRef.prototype.deref"),
    ),
    (
        Builtin::WeakCell(WeakCellOp::Register),
        incompatible!("FinalizationRegistry.prototype.register"),
    ),
    (
        Builtin::WeakCell(WeakCellOp::Unregister),
        incompatible!("FinalizationRegistry.prototype.unregister"),
    ),
];

/// `[message, stack]` of a `TypeError` with `message`.
macro_rules! type_error {
    ($message:literal) => {
        [$message, concat!("TypeError: ", $message)]
    };
}

/// `new WeakRef(t)` with a `t` that cannot be held weakly.
const WEAKREF_TARGET: CalleeErrorText<'static> = type_error!("WeakRef: target must be an object");

/// `new FinalizationRegistry(c)` with a non-callable `c`.
const FINREG_CLEANUP: CalleeErrorText<'static> =
    type_error!("FinalizationRegistry: cleanup must be callable");

/// `register` with a target that cannot be held weakly.
const FINREG_TARGET: CalleeErrorText<'static> =
    type_error!("FinalizationRegistry.register: invalid target");

/// `register` with `SameValue(target, held)`.
const FINREG_SAME: CalleeErrorText<'static> =
    type_error!("FinalizationRegistry.register: target and holdings must not be same");

/// `register` with a token neither `undefined` nor holdable weakly.
const FINREG_REGISTER_TOKEN: CalleeErrorText<'static> =
    type_error!("FinalizationRegistry.register: invalid unregister token");

/// `unregister` with a token that cannot be held weakly.
const FINREG_UNREGISTER_TOKEN: CalleeErrorText<'static> =
    type_error!("FinalizationRegistry.unregister: invalid unregister token");

/// The other `TypeError`s of `WeakRef` and `FinalizationRegistry`.
const CELL_TEXTS: [CalleeErrorText<'static>; 6] = [
    WEAKREF_TARGET,
    FINREG_CLEANUP,
    FINREG_TARGET,
    FINREG_SAME,
    FINREG_REGISTER_TOKEN,
    FINREG_UNREGISTER_TOKEN,
];

/// The byte size of a `FinalizationRegistry` cell `(target, held, token)`.
const CELL_SIZE: i32 = 24;

/// The empty entry of a `WeakMap` (`HOLE` key, `undefined` value), a
/// `WeakSet` (`HOLE` key) and an unused `FinalizationRegistry` cell.
const EMPTY_PAIR: &[i64] = &[HOLE_BITS, UNDEFINED_BITS];
/// See [`EMPTY_PAIR`].
const EMPTY_KEY: &[i64] = &[HOLE_BITS];
/// See [`EMPTY_PAIR`].
const EMPTY_CELL: &[i64] = &[UNDEFINED_BITS; 3];

/// The `TypeError`s of a key that cannot be held weakly (`WeakMap`, `WeakSet`).
const INVALID_KEY: [CalleeErrorText<'static>; 2] = [
    [
        "Invalid value used as weak map key",
        "TypeError: Invalid value used as weak map key",
    ],
    [
        "Invalid value used in weak set",
        "TypeError: Invalid value used in weak set",
    ],
];

/// The entries a fresh store holds (four pairs or four keys).
const INITIAL_ENTRIES: i32 = 4;

/// `WeakOp { set, op }`.
const fn op(set: bool, op: MapOp) -> Builtin {
    Builtin::WeakOp { set, op }
}

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    RECEIVER_TEXTS
        .into_iter()
        .flat_map(|(_, text)| text)
        .chain(INVALID_KEY.into_iter().flatten())
        .chain(CELL_TEXTS.into_iter().flatten())
}

/// The receiver `TypeError` of `builtin`.
fn receiver_text(builtin: Builtin) -> CalleeErrorText<'static> {
    RECEIVER_TEXTS
        .iter()
        .find_map(|&(b, text)| (b == builtin).then_some(text))
        .unwrap_or(RECEIVER_TEXTS[0].1)
}

/// The byte stride of one entry: a pair, or a `WeakSet`'s key alone.
fn stride(set: bool) -> i32 {
    if set { 8 } else { 16 }
}

/// The empty entry of a `WeakSet` (`set`) or `WeakMap` store.
fn empty_entry(set: bool) -> &'static [i64] {
    if set { EMPTY_KEY } else { EMPTY_PAIR }
}

/// Push `1` when the jsval in `local` can be held weakly (`CanBeHeldWeakly`,
/// design/runtime-memory.md §6.1): an object, or a symbol that is not in the
/// `Symbol.for` registry (the well-known symbols qualify), else `0`.
pub(super) fn emit_can_be_held_weakly(function: &mut impl Sink, local: u32) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(TYPE_STRING), I32Ne);
    emit_load_type_byte(function, local);
    wasm!(function;
        I32Const(TYPE_SYMBOL), I32Ne,
        LocalGet(local), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(abi::HEADER_FLAGS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(i32::from(abi::SYMBOL_FLAG_REGISTERED)), I32And, I32Eqz,
        I32Or, I32And,
        Else, I32Const(0), End,
    );
}

/// Read `this` into `this` and throw `builtin`'s receiver `TypeError` unless
/// it is a block of type byte `kind` (a `WeakMap`, `WeakSet`, `WeakRef` or
/// `FinalizationRegistry`).
fn this_weak(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    this: u32,
    kind: u8,
    builtin: Builtin,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function;
        I32Const(i32::from(kind)), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, receiver_text(builtin), strings, helpers);
    wasm!(function; End);
}

/// Push a `u32` field of the handle jsval in `handle`.
fn handle_field(function: &mut impl Sink, handle: u32, offset: u32) {
    wasm!(function;
        LocalGet(handle), I32WrapI64,
        I32Load(MemArg { offset: u64::from(offset), align: 2, memory_index: 0 }),
    );
}

/// Add `delta` to the handle's `live` count.
fn bump_live(function: &mut impl Sink, handle: u32, delta: i32) {
    wasm!(function; LocalGet(handle), I32WrapI64);
    handle_field(function, handle, abi::WEAK_LIVE_OFFSET);
    wasm!(function;
        I32Const(delta), I32Add,
        I32Store(MemArg { offset: u64::from(abi::WEAK_LIVE_OFFSET), align: 2, memory_index: 0 }),
    );
}

/// Set the i32 `found` to the address of the first entry (of `stride`
/// bytes) of the handle in `handle` whose key slot holds the bits `needle`
/// pushes (an identity match), or `0`; the i32 `at` and `end` are temps.
fn scan(
    function: &mut impl Sink,
    stride: i32,
    handle: u32,
    needle: &Instruction<'_>,
    [at, end, found]: [u32; 3],
) {
    let data = abi::ELEMS_DATA_OFFSET.cast_signed();
    wasm!(function; I32Const(0), LocalSet(found));
    handle_field(function, handle, abi::WEAK_ENTRIES_OFFSET);
    wasm!(function;
        LocalTee(at),
        If(BlockType::Empty),
        LocalGet(at),
        I32Load(MemArg { offset: u64::from(abi::ELEMS_CAP_OFFSET), align: 2, memory_index: 0 }),
        I32Const(3), I32Shl, LocalGet(at), I32Add, I32Const(data), I32Add, LocalSet(end),
        LocalGet(at), I32Const(data), I32Add, LocalSet(at),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(at), I64Load(MemArg { offset: 0, align: 3, memory_index: 0 }),
    );
    function.instruction(needle);
    wasm!(function;
        I64Eq, If(BlockType::Empty), LocalGet(at), LocalSet(found), Br(2), End,
        LocalGet(at), I32Const(stride), I32Add, LocalSet(at),
        Br(0),
        End, End,
        End,
    );
}

/// Grow the store of the handle in `handle` (at the handle's
/// [`abi::WEAK_ENTRIES_OFFSET`], which is also a registry's
/// [`abi::FINREG_CELLS_OFFSET`]) to twice its capacity (a fresh store holds
/// [`INITIAL_ENTRIES`]), copying the entries and filling the new tail with
/// `empty` entries (one value per slot of an entry), and set the i32 `found`
/// to the first new entry. Out of memory returns `undefined` with the
/// `RangeError` pending (the handle keeps its old store). The handle is a
/// rooted jsval; no other block base is live across the allocation.
fn grow(
    function: &mut FnBuilder<'_>,
    empty: &[i64],
    handle: u32,
    [old, cap, fresh, at, found]: [u32; 5],
    helpers: Helpers<'_>,
) {
    let data = abi::ELEMS_DATA_OFFSET.cast_signed();
    let slots = i32::try_from(empty.len()).unwrap_or(1);
    let m32 = |offset: u32| MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    };
    let m64 = |offset: u32| MemArg {
        offset: u64::from(offset),
        align: 3,
        memory_index: 0,
    };
    // cap = old ? old.cap : 0; new cap = cap ? 2 * cap : INITIAL_ENTRIES slots.
    handle_field(function, handle, abi::WEAK_ENTRIES_OFFSET);
    wasm!(function;
        LocalTee(old),
        If(BlockType::Result(ValType::I32)),
        LocalGet(old), I32Load(m32(abi::ELEMS_CAP_OFFSET)),
        Else, I32Const(0), End,
        LocalSet(cap),
        LocalGet(cap), I32Const(1), I32Shl,
        I32Const(INITIAL_ENTRIES * slots),
        LocalGet(cap), Select, LocalSet(at),
        LocalGet(at), I32Const(3), I32Shl, I32Const(data), I32Add, Call(helpers.alloc),
        LocalSet(fresh),
    );
    return_if_pending(function);
    // The allocation may have collected: reload the old store.
    handle_field(function, handle, abi::WEAK_ENTRIES_OFFSET);
    wasm!(function;
        LocalSet(old),
        LocalGet(fresh),
        I32Const(i32::from(abi::TYPE_ELEMS) | (i32::from(abi::ELEMS_FLAG_WEAK) << 16)),
        I32Store(m32(0)),
        LocalGet(fresh), LocalGet(at), I32Store(m32(abi::ELEMS_CAP_OFFSET)),
        // at = the end of the new store.
        LocalGet(at), I32Const(3), I32Shl, LocalGet(fresh), I32Add, I32Const(data), I32Add,
        LocalSet(at),
        LocalGet(old),
        If(BlockType::Empty),
        LocalGet(fresh), I32Const(data), I32Add,
        LocalGet(old), I32Const(data), I32Add,
        LocalGet(cap), I32Const(3), I32Shl,
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
        LocalGet(cap), I32Const(3), I32Shl, LocalGet(fresh), I32Add, I32Const(data), I32Add,
        LocalTee(found), LocalSet(cap),
        // Fill [found, at) with empty entries (`cap` is the cursor now).
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(cap), LocalGet(at), I32GeU, BrIf(1),
    );
    for (offset, &bits) in (0u32..).step_by(8).zip(empty) {
        wasm!(function; LocalGet(cap), I64Const(bits), I64Store(m64(offset)));
    }
    wasm!(function;
        LocalGet(cap), I32Const(8 * slots), I32Add, LocalSet(cap),
        Br(0),
        End, End,
        LocalGet(handle), I32WrapI64, LocalGet(fresh), I32Store(m32(abi::WEAK_ENTRIES_OFFSET)),
    );
}

/// Whether `builtin` is one of the constructors [`emit`] emits: `WeakMap`,
/// `WeakSet`, `WeakRef`, `FinalizationRegistry`.
pub(super) fn is_ctor(builtin: Builtin) -> bool {
    matches!(
        builtin,
        Builtin::Ctor(
            BuiltinCtor::WeakMap
                | BuiltinCtor::WeakSet
                | BuiltinCtor::WeakRef
                | BuiltinCtor::FinalizationRegistry,
        )
    )
}

/// Emit the body of the weak-collection builtin `builtin` into `f`.
pub(super) fn emit(
    builtin: Builtin,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match builtin {
        Builtin::WeakOp { set, op } => emit_op(set, op, f, frame, strings, helpers),
        Builtin::WeakCell(WeakCellOp::Deref) => emit_deref(f, frame, strings, helpers),
        Builtin::WeakCell(WeakCellOp::Register) => emit_register(f, frame, strings, helpers),
        Builtin::WeakCell(WeakCellOp::Unregister) => emit_unregister(f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::WeakRef) => emit_weak_ref_ctor(f, frame, strings, helpers),
        Builtin::Ctor(BuiltinCtor::FinalizationRegistry) => {
            emit_finreg_ctor(f, frame, strings, helpers);
        }
        Builtin::Ctor(ctor) => {
            emit_ctor(ctor == BuiltinCtor::WeakSet, f, frame, strings, helpers);
        }
        // Only the variants `emit_builtin` routes here reach this match.
        _ => {}
    }
}

/// A `%WeakMap.prototype%` / `%WeakSet.prototype%` method: the receiver
/// check, then `get` / `has` / `delete` (`undefined` / `false` for a key that
/// cannot be held weakly) or `set` / `add` (a `TypeError` for one),
/// returning the receiver.
fn emit_op(
    set: bool,
    op: MapOp,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, key, value) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (at, end, found) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let builtin = Builtin::WeakOp { set, op };
    let kind = if set {
        abi::TYPE_WEAKSET
    } else {
        abi::TYPE_WEAKMAP
    };
    this_weak(function, frame, this, kind, builtin, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(key));
    let adds = matches!(op, MapOp::Set | MapOp::Add);
    emit_can_be_held_weakly(function, key);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    if adds {
        throw_type_error(
            function,
            frame,
            INVALID_KEY[usize::from(set)],
            strings,
            helpers,
        );
    } else {
        let miss = if op == MapOp::Get {
            UNDEFINED_BITS
        } else {
            FALSE_BITS
        };
        wasm!(function; I64Const(miss), Return);
    }
    wasm!(function; End);
    if op == MapOp::Set {
        frame.arg(function, 1);
        wasm!(function; LocalSet(value));
    }
    let by_key = Instruction::LocalGet(key);
    scan(function, stride(set), this, &by_key, [at, end, found]);
    let m64 = |offset: u64| MemArg {
        offset,
        align: 3,
        memory_index: 0,
    };
    match op {
        MapOp::Get => wasm!(function;
            LocalGet(found),
            If(BlockType::Result(ValType::I64)),
            LocalGet(found), I64Load(m64(8)),
            Else, I64Const(UNDEFINED_BITS), End,
        ),
        MapOp::Has => wasm!(function;
            I64Const(TRUE_BITS), I64Const(FALSE_BITS), LocalGet(found), Select,
        ),
        MapOp::Delete => {
            wasm!(function;
                LocalGet(found),
                If(BlockType::Result(ValType::I64)),
                LocalGet(found), I64Const(HOLE_BITS), I64Store(m64(0)),
            );
            if !set {
                wasm!(function; LocalGet(found), I64Const(UNDEFINED_BITS), I64Store(m64(8)));
            }
            bump_live(function, this, -1);
            wasm!(function; I64Const(TRUE_BITS), Else, I64Const(FALSE_BITS), End);
        }
        MapOp::Set | MapOp::Add | MapOp::Clear => {
            let fresh = frame.w32(0);
            let old = frame.w32(1);
            wasm!(function; LocalGet(found), I32Eqz, If(BlockType::Empty));
            let by_hole = Instruction::I64Const(HOLE_BITS);
            scan(function, stride(set), this, &by_hole, [at, end, found]);
            wasm!(function; LocalGet(found), I32Eqz, If(BlockType::Empty));
            grow(
                function,
                empty_entry(set),
                this,
                [old, end, fresh, at, found],
                helpers,
            );
            wasm!(function;
                End,
                LocalGet(found), LocalGet(key), I64Store(m64(0)),
            );
            bump_live(function, this, 1);
            wasm!(function; End);
            if !set {
                wasm!(function; LocalGet(found), LocalGet(value), I64Store(m64(8)));
            }
            wasm!(function; LocalGet(this));
        }
    }
}

/// `new WeakMap(iterable)` / `new WeakSet(iterable)`: the prototype from
/// [`super::ctor::fragment`] (the "requires 'new'" `TypeError` when called),
/// a fresh empty handle over it (no store until the first entry), then the
/// iterable's values through the collection's `set` / `add`
/// ([`emit_add_from_iterable`]).
fn emit_ctor(
    set: bool,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (map, proto, base) = (frame.x64(0), frame.y64(0), frame.base(0));
    let (ctor, kind, type_byte) = if set {
        (BuiltinCtor::WeakSet, Collection::WeakSet, abi::TYPE_WEAKSET)
    } else {
        (BuiltinCtor::WeakMap, Collection::WeakMap, abi::TYPE_WEAKMAP)
    };
    let prototype = super::ctor::fragment(ctor, frame.k);
    emit_node(&prototype, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(proto));
    return_if_pending(function);
    emit_alloc_const(function, helpers, abi::WEAK_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalTee(base), I32Const(i32::from(type_byte)),
        I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(base), LocalGet(proto),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(map),
    );
    emit_add_from_iterable(kind, function, frame, strings, helpers);
}

/// `AddToKeptObjects(target)` for the jsval in `target`: push it onto the
/// internal `KeepDuringJob` array `$__kept_objects` (allocated by the first
/// push), which keeps it alive until the next microtask checkpoint
/// (design/runtime-memory.md §6.3). Out of memory returns `undefined` with
/// the `RangeError` pending.
fn keep_during_job(function: &mut FnBuilder<'_>, target: u32, helpers: Helpers<'_>) {
    wasm!(function;
        GlobalGet(KEPT_OBJECTS_GLOBAL), I32Eqz,
        If(BlockType::Empty),
        I32Const(0), Call(helpers.new_array()), GlobalSet(KEPT_OBJECTS_GLOBAL),
    );
    return_if_pending(function);
    wasm!(function;
        End,
        GlobalGet(KEPT_OBJECTS_GLOBAL), LocalGet(target), Call(helpers.array_push), Drop,
    );
    return_if_pending(function);
}

/// Throw `text` when this is a construct call (`new`) whose argument in
/// `value` fails the test `check` pushes (`1` = valid): the argument check
/// of `WeakRef` / `FinalizationRegistry`, which comes after the "requires
/// 'new'" check and before the prototype read of
/// [`super::ctor::fragment`].
fn check_construct_arg(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    text: CalleeErrorText<'static>,
    check: impl FnOnce(&mut FnBuilder<'_>),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_construct_call(function, frame.argc());
    wasm!(function; If(BlockType::Empty));
    check(function);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End, End);
}

/// Push the prototype from [`super::ctor::fragment`] for `ctor` (the
/// "requires 'new'" `TypeError` when called) into `proto`, returning on a
/// throw.
fn ctor_proto(
    ctor: BuiltinCtor,
    proto: u32,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let prototype = super::ctor::fragment(ctor, frame.k);
    emit_node(&prototype, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(proto));
    return_if_pending(function);
}

/// `new WeakRef(target)` (§6.3): `target` must be holdable weakly
/// (`TypeError` otherwise), then a fresh `WeakRef` over the prototype, and
/// `AddToKeptObjects(target)`.
fn emit_weak_ref_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (out, proto, target, base) = (frame.x64(0), frame.y64(0), frame.y64(1), frame.base(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(target));
    let valid = |f: &mut FnBuilder<'_>| emit_can_be_held_weakly(f, target);
    check_construct_arg(function, frame, WEAKREF_TARGET, valid, strings, helpers);
    ctor_proto(
        BuiltinCtor::WeakRef,
        proto,
        function,
        frame,
        strings,
        helpers,
    );
    emit_alloc_const(function, helpers, abi::WEAKREF_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalTee(base), I32Const(i32::from(abi::TYPE_WEAKREF)),
        I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(base), LocalGet(proto),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(base), LocalGet(target),
        I64Store(MemArg { offset: u64::from(abi::WEAKREF_TARGET_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(out),
    );
    return_if_pending(function);
    keep_during_job(function, target, helpers);
    wasm!(function; LocalGet(out));
}

/// `WeakRef.prototype.deref()` (§6.3): the receiver check, then the target
/// (`undefined` once collected); a live target is kept for the rest of the
/// job (`AddToKeptObjects`).
fn emit_deref(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, target) = (frame.i64(0), frame.i64(1));
    let builtin = Builtin::WeakCell(WeakCellOp::Deref);
    this_weak(
        function,
        frame,
        this,
        abi::TYPE_WEAKREF,
        builtin,
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(this), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::WEAKREF_TARGET_OFFSET), align: 3, memory_index: 0 }),
        LocalTee(target), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
    );
    keep_during_job(function, target, helpers);
    wasm!(function; End, LocalGet(target));
}

/// `new FinalizationRegistry(cleanup)` (§6.4): `cleanup` must be callable
/// (`TypeError` otherwise), then a fresh registry over the prototype with no
/// cells and no queued job.
fn emit_finreg_ctor(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (proto, cleanup, base) = (frame.y64(0), frame.y64(1), frame.base(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(cleanup));
    let valid = |f: &mut FnBuilder<'_>| emit_is_function(f, cleanup);
    check_construct_arg(function, frame, FINREG_CLEANUP, valid, strings, helpers);
    ctor_proto(
        BuiltinCtor::FinalizationRegistry,
        proto,
        function,
        frame,
        strings,
        helpers,
    );
    emit_alloc_const(function, helpers, abi::FINREG_BLOCK_SIZE.cast_signed());
    wasm!(function;
        LocalTee(base), I32Const(i32::from(abi::TYPE_FINREG)),
        I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(base), LocalGet(proto),
        I64Store(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(base), LocalGet(cleanup),
        I64Store(MemArg { offset: u64::from(abi::FINREG_CLEANUP_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(base), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// `FinalizationRegistry.prototype.register(target, held, token)` (§6.4):
/// the receiver check; `target` holdable weakly, not `SameValue` to `held`,
/// and `token` `undefined` or holdable weakly (each a `TypeError`); then the
/// cell goes into the first unused slot, growing the store when there is
/// none (the three values stay rooted in frame locals across the
/// allocation). Returns `undefined`.
fn emit_register(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, target, held, token) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (at, end, found) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let (fresh, old) = (frame.w32(0), frame.w32(1));
    let builtin = Builtin::WeakCell(WeakCellOp::Register);
    this_weak(
        function,
        frame,
        this,
        abi::TYPE_FINREG,
        builtin,
        strings,
        helpers,
    );
    for (i, local) in [(0, target), (1, held), (2, token)] {
        frame.arg(function, i);
        wasm!(function; LocalSet(local));
    }
    emit_can_be_held_weakly(function, target);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, FINREG_TARGET, strings, helpers);
    wasm!(function; End, LocalGet(target), LocalGet(held), I64Eq, If(BlockType::Empty));
    throw_type_error(function, frame, FINREG_SAME, strings, helpers);
    wasm!(function; End);
    emit_can_be_held_weakly(function, token);
    wasm!(function;
        I32Eqz, LocalGet(token), I64Const(UNDEFINED_BITS), I64Ne, I32And,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, FINREG_REGISTER_TOKEN, strings, helpers);
    wasm!(function; End);
    let unused = Instruction::I64Const(UNDEFINED_BITS);
    scan(function, CELL_SIZE, this, &unused, [at, end, found]);
    wasm!(function; LocalGet(found), I32Eqz, If(BlockType::Empty));
    grow(
        function,
        EMPTY_CELL,
        this,
        [old, end, fresh, at, found],
        helpers,
    );
    wasm!(function; End);
    for (offset, local) in [(0, target), (8, held), (16, token)] {
        wasm!(function;
            LocalGet(found), LocalGet(local),
            I64Store(MemArg { offset, align: 3, memory_index: 0 }),
        );
    }
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// `FinalizationRegistry.prototype.unregister(token)` (§6.4): the receiver
/// check, `token` holdable weakly (`TypeError` otherwise), then every cell
/// registered with `token` becomes unused, collected or not; returns whether
/// any did.
fn emit_unregister(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, token) = (frame.i64(0), frame.i64(1));
    let (at, end, removed) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let data = abi::ELEMS_DATA_OFFSET.cast_signed();
    let m64 = |offset: u64| MemArg {
        offset,
        align: 3,
        memory_index: 0,
    };
    let builtin = Builtin::WeakCell(WeakCellOp::Unregister);
    this_weak(
        function,
        frame,
        this,
        abi::TYPE_FINREG,
        builtin,
        strings,
        helpers,
    );
    frame.arg(function, 0);
    wasm!(function; LocalSet(token));
    emit_can_be_held_weakly(function, token);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, FINREG_UNREGISTER_TOKEN, strings, helpers);
    wasm!(function; End, I32Const(0), LocalSet(removed));
    handle_field(function, this, abi::FINREG_CELLS_OFFSET);
    wasm!(function;
        LocalTee(at),
        If(BlockType::Empty),
        LocalGet(at),
        I32Load(MemArg { offset: u64::from(abi::ELEMS_CAP_OFFSET), align: 2, memory_index: 0 }),
        I32Const(3), I32Shl, LocalGet(at), I32Add, I32Const(data), I32Add, LocalSet(end),
        LocalGet(at), I32Const(data), I32Add, LocalSet(at),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(at), I64Load(m64(16)), LocalGet(token), I64Eq,
        If(BlockType::Empty),
        LocalGet(at), I64Const(UNDEFINED_BITS), I64Store(m64(0)),
        LocalGet(at), I64Const(UNDEFINED_BITS), I64Store(m64(8)),
        LocalGet(at), I64Const(UNDEFINED_BITS), I64Store(m64(16)),
        I32Const(1), LocalSet(removed),
        End,
        LocalGet(at), I32Const(CELL_SIZE), I32Add, LocalSet(at),
        Br(0),
        End, End,
        End,
        I64Const(TRUE_BITS), I64Const(FALSE_BITS), LocalGet(removed), Select,
    );
}
