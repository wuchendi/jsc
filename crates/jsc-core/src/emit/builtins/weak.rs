//! The weak collections (design/runtime-memory.md §6): `%WeakMap.prototype%`
//! and `%WeakSet.prototype%` methods, the `WeakMap` / `WeakSet` constructors
//! and the shared `CanBeHeldWeakly` test.
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

#[allow(clippy::wildcard_imports)]
use super::*;

use super::map_set::{Collection, emit_add_from_iterable};

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
const RECEIVER_TEXTS: [(Builtin, CalleeErrorText<'static>); 7] = [
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
];

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
/// it is a `WeakMap` (or, for `set`, a `WeakSet`) handle.
fn this_weak(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    this: u32,
    set: bool,
    builtin: Builtin,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let kind = if set {
        abi::TYPE_WEAKSET
    } else {
        abi::TYPE_WEAKMAP
    };
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

/// Set the i32 `found` to the address of the first entry of the handle in
/// `handle` whose key slot holds the bits `needle` pushes (an identity
/// match), or `0`; the i32 `at` and `end` are temps.
fn scan(
    function: &mut impl Sink,
    set: bool,
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
        LocalGet(at), I32Const(stride(set)), I32Add, LocalSet(at),
        Br(0),
        End, End,
        End,
    );
}

/// Grow the store of the handle in `handle` to twice its capacity (a fresh
/// store holds [`INITIAL_ENTRIES`]), copying the entries and filling the new
/// tail with `HOLE` keys (`undefined` values), and set the i32 `found` to
/// the first new entry. Out of memory returns `undefined` with the
/// `RangeError` pending (the handle keeps its old store). The handle is a
/// rooted jsval; no other block base is live across the allocation.
fn grow(
    function: &mut FnBuilder<'_>,
    set: bool,
    handle: u32,
    [old, cap, fresh, at, found]: [u32; 5],
    helpers: Helpers<'_>,
) {
    let data = abi::ELEMS_DATA_OFFSET.cast_signed();
    let slots = if set { 1 } else { 2 };
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
        LocalGet(cap), I64Const(HOLE_BITS), I64Store(m64(0)),
    );
    if !set {
        wasm!(function; LocalGet(cap), I64Const(UNDEFINED_BITS), I64Store(m64(8)));
    }
    wasm!(function;
        LocalGet(cap), I32Const(stride(set)), I32Add, LocalSet(cap),
        Br(0),
        End, End,
        LocalGet(handle), I32WrapI64, LocalGet(fresh), I32Store(m32(abi::WEAK_ENTRIES_OFFSET)),
    );
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
    this_weak(function, frame, this, set, builtin, strings, helpers);
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
    scan(function, set, this, &by_key, [at, end, found]);
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
            scan(function, set, this, &by_hole, [at, end, found]);
            wasm!(function; LocalGet(found), I32Eqz, If(BlockType::Empty));
            grow(function, set, this, [old, end, fresh, at, found], helpers);
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
