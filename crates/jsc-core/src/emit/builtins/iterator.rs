//! The `Iterator` global (design/symbol-strings.md §5.7; the abstract
//! constructor's body is in [`super::ctor`]): `Iterator.from` with its `%WrapForValidIteratorPrototype%`
//! wrappers, and the `constructor` / `[@@toStringTag]` accessors of
//! `%IteratorPrototype%` (`Iterator.prototype`).
//!
//! A wrapper is a `CLASS_ITERATOR` object of kind [`WRAP_KIND`] whose
//! 32-byte head is the one of the builtin iterators (§5.4), followed by the
//! wrapped record: the iterator at [`WRAP_ITERATOR_OFFSET`] and its `next`
//! at [`WRAP_NEXT_OFFSET`].

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::IteratorOp;

/// The `kind` byte of a `%WrapForValidIteratorPrototype%` object (the first
/// of the helper kinds, §5.4).
const WRAP_KIND: i32 = 16;
/// Offset of the head's `[[IteratedObject]]` (unused by a wrapper).
const WRAP_ITERATED_OFFSET: u64 = abi::OBJ_PREFIX_SIZE as u64;
/// Offset of the head's `[[NextIndex]]` (unused by a wrapper).
const WRAP_INDEX_OFFSET: u64 = WRAP_ITERATED_OFFSET + 8;
/// Offset of the head's `kind` byte (and its three pad bytes).
const WRAP_KIND_OFFSET: u64 = WRAP_ITERATED_OFFSET + 12;
/// Offset of the wrapped record's `[[Iterator]]`.
const WRAP_ITERATOR_OFFSET: u64 = 32;
/// Offset of the wrapped record's `[[NextMethod]]`.
const WRAP_NEXT_OFFSET: u64 = 40;
/// Byte size of a wrapper block.
const WRAP_BLOCK_SIZE: i32 = 48;

/// `Iterator.from` of a primitive other than a string.
const FROM_PRIMITIVE: CalleeErrorText<'static> = [
    "Iterator.from called on non-object",
    "TypeError: Iterator.from called on non-object",
];

/// `Iterator.from` of a value whose `@@iterator` is not callable.
const FROM_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Symbol.iterator is not a function",
    "TypeError: Symbol.iterator is not a function",
];

/// `Iterator.from` of a value whose `@@iterator` returns a non-object.
const FROM_NOT_OBJECT: CalleeErrorText<'static> = [
    "Result of the Symbol.iterator method is not an object",
    "TypeError: Result of the Symbol.iterator method is not an object",
];

/// A `%WrapForValidIteratorPrototype%` method on another receiver.
const WRAP_INCOMPATIBLE: CalleeErrorText<'static> = [
    "Method called on incompatible receiver",
    "TypeError: Method called on incompatible receiver",
];

/// A wrapped iterator whose `next` is not callable.
const NEXT_NOT_CALLABLE: CalleeErrorText<'static> = [
    "next is not a function",
    "TypeError: next is not a function",
];

/// A wrapped iterator whose `return` is neither nullish nor callable.
const RETURN_NOT_CALLABLE: CalleeErrorText<'static> = [
    "return is not a function",
    "TypeError: return is not a function",
];

/// `SetterThatIgnoresPrototypeProperties` on a non-object.
const SETTER_NOT_OBJECT: CalleeErrorText<'static> = [
    "Iterator.prototype setter called on non-object",
    "TypeError: Iterator.prototype setter called on non-object",
];

/// `SetterThatIgnoresPrototypeProperties` on `%Iterator.prototype%` itself.
const SETTER_HOME: CalleeErrorText<'static> = [
    "Cannot assign to read only property of Iterator.prototype",
    "TypeError: Cannot assign to read only property of Iterator.prototype",
];

/// Every static string the bodies reference.
pub(super) const STRINGS: [&str; 20] = [
    FROM_PRIMITIVE[0],
    FROM_PRIMITIVE[1],
    FROM_NOT_CALLABLE[0],
    FROM_NOT_CALLABLE[1],
    FROM_NOT_OBJECT[0],
    FROM_NOT_OBJECT[1],
    WRAP_INCOMPATIBLE[0],
    WRAP_INCOMPATIBLE[1],
    NEXT_NOT_CALLABLE[0],
    NEXT_NOT_CALLABLE[1],
    RETURN_NOT_CALLABLE[0],
    RETURN_NOT_CALLABLE[1],
    SETTER_NOT_OBJECT[0],
    SETTER_NOT_OBJECT[1],
    SETTER_HOME[0],
    SETTER_HOME[1],
    "Iterator",
    "constructor",
    "next",
    "return",
];

/// The IR fragments the bodies emit, for the static-string scan.
pub(super) fn fragments() -> Vec<IrNode> {
    vec![done_result()]
}

/// `CreateIterResultObject(undefined, true)`.
fn done_result() -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".into()), IrNode::Undefined),
            (PropKey::Static("done".into()), IrNode::Bool(true)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// Emit the body of the `Iterator` builtin `op`.
pub(super) fn emit(
    op: IteratorOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        IteratorOp::From => emit_from(function, frame, strings, helpers),
        IteratorOp::WrapNext => emit_wrap_next(function, frame, strings, helpers),
        IteratorOp::WrapReturn => emit_wrap_return(function, frame, strings, helpers),
        IteratorOp::ConstructorGet => {
            let ctor = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Iterator), helpers);
            wasm!(function; I64Const(ctor));
        }
        IteratorOp::ConstructorSet => {
            let key = static_key(strings, "constructor");
            emit_ignoring_setter(key, function, frame, strings, helpers);
        }
        IteratorOp::TagGet => push_str(function, strings, "Iterator"),
        IteratorOp::TagSet => {
            let key = realm::symbol_addr(WellKnown::ToStringTag, helpers);
            emit_ignoring_setter(key, function, frame, strings, helpers);
        }
    }
}

/// Call the function in i64 `callee` with `this` = i64 `this` and no
/// arguments into i64 `out`, returning on a throw.
fn call0(
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

/// `Iterator.from(O)`: `GetIteratorFlattenable(O, iterate-string-primitives)`
/// (a primitive other than a string throws; `GetMethod(O, @@iterator)`,
/// `O` itself when nullish, else its call, which must yield an object;
/// `next` read once), then the iterator itself when `%Iterator.prototype%`
/// is on its `proto` chain (`OrdinaryHasInstance(%Iterator%, it)`), else a
/// fresh `%WrapForValidIteratorPrototype%` wrapper of the record.
fn emit_from(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (o, method, it, next) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (p, obj) = (frame.x64(0), frame.i32(0));
    let proto = MemArg {
        offset: u64::from(abi::OBJ_PROTO_OFFSET),
        align: 3,
        memory_index: 0,
    };
    let iterator_proto = intrinsic_bits(Intrinsic::IteratorPrototype, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(o));
    emit_is_object(function, o);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_string(function, o);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, FROM_PRIMITIVE, strings, helpers);
    wasm!(function;
        End, End,
        LocalGet(o), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    return_if_pending(function);
    wasm!(function; LocalGet(o), LocalSet(it));
    emit_is_nullish(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, FROM_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    call0(function, frame, method, o, it, helpers);
    wasm!(function; End);
    emit_is_object(function, it);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, FROM_NOT_OBJECT, strings, helpers);
    wasm!(function;
        End,
        LocalGet(it), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(next),
    );
    return_if_pending(function);
    // `%Iterator.prototype%` on the chain: the iterator itself.
    wasm!(function;
        LocalGet(it), I32WrapI64, I64Load(proto), LocalSet(p),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    emit_is_object(function, p);
    wasm!(function;
        I32Eqz, BrIf(1),
        LocalGet(p), I64Const(iterator_proto), I64Eq,
        If(BlockType::Empty), LocalGet(it), Return, End,
        LocalGet(p), I32WrapI64, I64Load(proto), LocalSet(p),
        Br(0),
        End, End,
    );
    let header = TYPE_OBJECT | (i32::from(abi::CLASS_ITERATOR) << 24);
    let wrap_proto = intrinsic_bits(Intrinsic::WrapForValidIteratorPrototype, helpers);
    let m32 = |offset| MemArg {
        offset,
        align: 2,
        memory_index: 0,
    };
    let m64 = |offset| MemArg {
        offset,
        align: 3,
        memory_index: 0,
    };
    wasm!(function;
        I32Const(WRAP_BLOCK_SIZE), Call(helpers.alloc), LocalTee(obj),
        I32Const(header), I32Store(m32(0)),
        LocalGet(obj), I32Const(0), I32Store(m32(u64::from(abi::OBJ_PROPS_OFFSET))),
        LocalGet(obj), I64Const(wrap_proto), I64Store(m64(u64::from(abi::OBJ_PROTO_OFFSET))),
        LocalGet(obj), I64Const(UNDEFINED_BITS), I64Store(m64(WRAP_ITERATED_OFFSET)),
        LocalGet(obj), I32Const(0), I32Store(m32(WRAP_INDEX_OFFSET)),
        LocalGet(obj), I32Const(WRAP_KIND), I32Store(m32(WRAP_KIND_OFFSET)),
        LocalGet(obj), LocalGet(it), I64Store(m64(WRAP_ITERATOR_OFFSET)),
        LocalGet(obj), LocalGet(next), I64Store(m64(WRAP_NEXT_OFFSET)),
        LocalGet(obj), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// Read `this` into i64 `this`, throwing unless it is a
/// `%WrapForValidIteratorPrototype%` wrapper (`RequireInternalSlot(O,
/// [[Iterated]])`), then its wrapped `[[Iterator]]` into i64 `it`.
fn wrap_receiver(
    function: &mut Function,
    frame: Frame,
    this: u32,
    it: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_wrapper(function, this, abi::CLASS_ITERATOR);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(this), I32WrapI64,
        I32Load8U(MemArg { offset: WRAP_KIND_OFFSET, align: 0, memory_index: 0 }),
        I32Const(WRAP_KIND), I32Eq,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty),
    );
    throw_type_error(function, frame, WRAP_INCOMPATIBLE, strings, helpers);
    wasm!(function;
        End,
        LocalGet(this), I32WrapI64,
        I64Load(MemArg { offset: WRAP_ITERATOR_OFFSET, align: 3, memory_index: 0 }),
        LocalSet(it),
    );
}

/// `%WrapForValidIteratorPrototype%.next()`: `Call(next, iterator)` of the
/// wrapped record, its result returned as is.
fn emit_wrap_next(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, it, next, out) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wrap_receiver(function, frame, this, it, strings, helpers);
    wasm!(function;
        LocalGet(this), I32WrapI64,
        I64Load(MemArg { offset: WRAP_NEXT_OFFSET, align: 3, memory_index: 0 }),
        LocalSet(next),
    );
    emit_is_function(function, next);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, NEXT_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    call0(function, frame, next, it, out, helpers);
    wasm!(function; LocalGet(out));
}

/// `%WrapForValidIteratorPrototype%.return()`: `GetMethod(iterator,
/// "return")` of the wrapped iterator; `{ value: undefined, done: true }`
/// when nullish, else its call's result as is.
fn emit_wrap_return(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, it, method, out) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wrap_receiver(function, frame, this, it, strings, helpers);
    wasm!(function;
        LocalGet(it), I32Const(static_key(strings, "return")), Call(helpers.get), LocalSet(method),
    );
    return_if_pending(function);
    emit_is_nullish(function, method);
    wasm!(function; If(BlockType::Empty));
    emit_node(&done_result(), function, frame.scratch(), strings, helpers);
    wasm!(function; Return, End);
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, RETURN_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    call0(function, frame, method, it, out, helpers);
    wasm!(function; LocalGet(out));
}

/// `SetterThatIgnoresPrototypeProperties(this, %Iterator.prototype%, key,
/// v)`: a non-object `this` or `%Iterator.prototype%` itself throws; a
/// `this` without an own `key` gets one (`CreateDataPropertyOrThrow`),
/// otherwise `Set(this, key, v, true)`.
fn emit_ignoring_setter(
    key: i32,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, v) = (frame.i64(0), frame.i64(1));
    let home = intrinsic_bits(Intrinsic::IteratorPrototype, helpers);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    frame.arg(function, 0);
    wasm!(function; LocalSet(v));
    emit_is_object(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, SETTER_NOT_OBJECT, strings, helpers);
    wasm!(function; End, LocalGet(this), I64Const(home), I64Eq, If(BlockType::Empty));
    throw_type_error(function, frame, SETTER_HOME, strings, helpers);
    wasm!(function;
        End,
        LocalGet(this), I32WrapI64, I32Const(key), Call(helpers.prop_find), I32Eqz,
        If(BlockType::Empty),
        LocalGet(this), I32WrapI64, I32Const(key), LocalGet(v),
        I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
        Else,
        LocalGet(this), I32Const(key), LocalGet(v), Call(helpers.set),
        End,
        I64Const(UNDEFINED_BITS),
    );
}
