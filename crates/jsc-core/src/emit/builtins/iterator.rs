//! The `Iterator` global (design/symbol-strings.md §5.7; the abstract
//! constructor's body is in [`super::ctor`]): `Iterator.from` with its `%WrapForValidIteratorPrototype%`
//! wrappers, the `constructor` / `[@@toStringTag]` accessors of
//! `%IteratorPrototype%` (`Iterator.prototype`), and the lazy helpers
//! `map` / `filter` / `take` / `drop` / `flatMap` with their
//! `%IteratorHelperPrototype%` objects, and the eager helpers `reduce` /
//! `toArray` / `forEach` / `some` / `every` / `find`.
//!
//! A wrapper is a `CLASS_ITERATOR` object of kind [`WRAP_KIND`] whose
//! 32-byte head is the one of the builtin iterators (§5.4), followed by the
//! wrapped record: the iterator at [`WRAP_ITERATOR_OFFSET`] and its `next`
//! at [`WRAP_NEXT_OFFSET`].
//!
//! A helper object is a `CLASS_ITERATOR` object of a kind past
//! [`WRAP_KIND`] ([`helper_kind_byte`]) whose head's `[[NextIndex]]` holds
//! its generator state ([`SUSPENDED_START`] .. [`COMPLETED`]), followed by
//! the underlying record (iterator, `next`), the counter (`map` / `filter` /
//! `flatMap`) or remaining limit (`take` / `drop`), the callback, and for
//! `flatMap` the inner record (iterator `undefined` while none is open).

use super::iter::guarded;
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::{EagerKind, HelperKind, IteratorOp};
use crate::ir::ConvertOp;

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

/// Offset of a helper's generator state (the head's `[[NextIndex]]`).
const HELPER_STATE_OFFSET: u64 = WRAP_INDEX_OFFSET;
/// Offset of a helper's underlying `[[Iterator]]`.
const HELPER_ITERATOR_OFFSET: u64 = 32;
/// Offset of a helper's underlying `[[NextMethod]]`.
const HELPER_NEXT_OFFSET: u64 = 40;
/// Offset of a helper's counter or remaining limit (`f64`).
const HELPER_COUNTER_OFFSET: u64 = 48;
/// Offset of a helper's mapper / predicate.
const HELPER_CALLBACK_OFFSET: u64 = 56;
/// Offset of a `flatMap` helper's inner `[[Iterator]]` (`undefined` while
/// none is open).
const HELPER_INNER_OFFSET: u64 = 64;
/// Offset of a `flatMap` helper's inner `[[NextMethod]]`.
const HELPER_INNER_NEXT_OFFSET: u64 = 72;
/// Byte size of a helper block.
const HELPER_BLOCK_SIZE: i32 = 80;
/// The number of helper kinds (the `kind` bytes past [`WRAP_KIND`]).
const HELPER_KINDS: i32 = 5;

/// Generator state: not started (`return()` closes without resuming).
const SUSPENDED_START: i32 = 0;
/// Generator state: suspended after a yield.
const SUSPENDED_YIELD: i32 = 1;
/// Generator state: a `next` / `return` in progress.
const RUNNING: i32 = 2;
/// Generator state: finished.
const COMPLETED: i32 = 3;

/// The `kind` byte of a `kind` helper object.
fn helper_kind_byte(kind: HelperKind) -> i32 {
    WRAP_KIND
        + match kind {
            HelperKind::Map => 1,
            HelperKind::Filter => 2,
            HelperKind::Take => 3,
            HelperKind::Drop => 4,
            HelperKind::FlatMap => 5,
        }
}

/// An `i32` access at `offset`.
fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

/// An `i64` / `f64` access at `offset`.
fn m64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

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

/// A `%WrapForValidIteratorPrototype%` or `%IteratorHelperPrototype%`
/// method on another receiver.
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

/// A lazy helper called on a non-object, by [`HelperKind`] order.
const HELPER_NOT_OBJECT: [CalleeErrorText<'static>; 5] = [
    [
        "Iterator.prototype.map called on non-object",
        "TypeError: Iterator.prototype.map called on non-object",
    ],
    [
        "Iterator.prototype.filter called on non-object",
        "TypeError: Iterator.prototype.filter called on non-object",
    ],
    [
        "Iterator.prototype.take called on non-object",
        "TypeError: Iterator.prototype.take called on non-object",
    ],
    [
        "Iterator.prototype.drop called on non-object",
        "TypeError: Iterator.prototype.drop called on non-object",
    ],
    [
        "Iterator.prototype.flatMap called on non-object",
        "TypeError: Iterator.prototype.flatMap called on non-object",
    ],
];

/// A `map` / `filter` / `flatMap` callback that is not callable.
const CALLBACK_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Iterator helper callback is not a function",
    "TypeError: Iterator helper callback is not a function",
];

/// A `take` / `drop` limit that is `NaN` or negative.
const LIMIT_RANGE: CalleeErrorText<'static> = [
    "Iterator helper limit must be a non-negative number",
    "RangeError: Iterator helper limit must be a non-negative number",
];

/// A helper's `next` / `return` while it runs.
const HELPER_RUNNING: CalleeErrorText<'static> = [
    "Generator is already running",
    "TypeError: Generator is already running",
];

/// A `flatMap` mapper result that is not an object (strings included).
const FLAT_NOT_OBJECT: CalleeErrorText<'static> = [
    "Iterator.prototype.flatMap mapper result is not an object",
    "TypeError: Iterator.prototype.flatMap mapper result is not an object",
];

/// An eager helper called on a non-object, by [`EagerKind`] order.
const EAGER_NOT_OBJECT: [CalleeErrorText<'static>; 6] = [
    [
        "Iterator.prototype.reduce called on non-object",
        "TypeError: Iterator.prototype.reduce called on non-object",
    ],
    [
        "Iterator.prototype.toArray called on non-object",
        "TypeError: Iterator.prototype.toArray called on non-object",
    ],
    [
        "Iterator.prototype.forEach called on non-object",
        "TypeError: Iterator.prototype.forEach called on non-object",
    ],
    [
        "Iterator.prototype.some called on non-object",
        "TypeError: Iterator.prototype.some called on non-object",
    ],
    [
        "Iterator.prototype.every called on non-object",
        "TypeError: Iterator.prototype.every called on non-object",
    ],
    [
        "Iterator.prototype.find called on non-object",
        "TypeError: Iterator.prototype.find called on non-object",
    ],
];

/// `reduce` without an initial value on an empty iterator.
const REDUCE_EMPTY: CalleeErrorText<'static> = [
    "Reduce of empty iterator with no initial value",
    "TypeError: Reduce of empty iterator with no initial value",
];

/// Every static string the bodies reference.
pub(super) const STRINGS: [&str; 52] = [
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
    HELPER_NOT_OBJECT[0][0],
    HELPER_NOT_OBJECT[0][1],
    HELPER_NOT_OBJECT[1][0],
    HELPER_NOT_OBJECT[1][1],
    HELPER_NOT_OBJECT[2][0],
    HELPER_NOT_OBJECT[2][1],
    HELPER_NOT_OBJECT[3][0],
    HELPER_NOT_OBJECT[3][1],
    HELPER_NOT_OBJECT[4][0],
    HELPER_NOT_OBJECT[4][1],
    CALLBACK_NOT_CALLABLE[0],
    CALLBACK_NOT_CALLABLE[1],
    LIMIT_RANGE[0],
    LIMIT_RANGE[1],
    HELPER_RUNNING[0],
    HELPER_RUNNING[1],
    FLAT_NOT_OBJECT[0],
    FLAT_NOT_OBJECT[1],
    EAGER_NOT_OBJECT[0][0],
    EAGER_NOT_OBJECT[0][1],
    EAGER_NOT_OBJECT[1][0],
    EAGER_NOT_OBJECT[1][1],
    EAGER_NOT_OBJECT[2][0],
    EAGER_NOT_OBJECT[2][1],
    EAGER_NOT_OBJECT[3][0],
    EAGER_NOT_OBJECT[3][1],
    EAGER_NOT_OBJECT[4][0],
    EAGER_NOT_OBJECT[4][1],
    EAGER_NOT_OBJECT[5][0],
    EAGER_NOT_OBJECT[5][1],
    REDUCE_EMPTY[0],
    REDUCE_EMPTY[1],
];

/// The IR fragments the bodies emit, for the static-string scan.
pub(super) fn fragments() -> Vec<IrNode> {
    vec![done_result(), limit_error()]
}

/// `CreateIterResultObject(value, done)`.
fn iter_result(value: IrNode, done: bool) -> IrNode {
    IrNode::Object {
        entries: vec![
            (PropKey::Static("value".into()), value),
            (PropKey::Static("done".into()), IrNode::Bool(done)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// `CreateIterResultObject(undefined, true)`.
fn done_result() -> IrNode {
    iter_result(IrNode::Undefined, true)
}

/// The `RangeError` of a `NaN` or negative `take` / `drop` limit.
fn limit_error() -> IrNode {
    IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(LIMIT_RANGE[0].into())),
        IrNode::Str(LIMIT_RANGE[1].into()),
    )
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
        IteratorOp::Helper(kind) => emit_helper(kind, function, frame, strings, helpers),
        IteratorOp::HelperNext => emit_helper_next(function, frame, strings, helpers),
        IteratorOp::HelperReturn => emit_helper_return(function, frame, strings, helpers),
        IteratorOp::Eager(kind) => emit_eager(kind, function, frame, strings, helpers),
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
    call_args(function, frame, callee, Some(this), &[], helpers);
    wasm!(function; LocalSet(out));
    return_if_pending(function);
}

/// Push the result of calling the function in i64 `callee` with `this` =
/// i64 `this` (`undefined` when `None`) on the i64 locals `args` (past `K`
/// published as the spread argument array); a throw is left pending.
fn call_args(
    function: &mut Function,
    frame: Frame,
    callee: u32,
    this: Option<u32>,
    args: &[u32],
    helpers: Helpers<'_>,
) {
    for slot in 0..frame.k {
        if let Some(&local) = args.get(slot as usize) {
            wasm!(function; LocalGet(local));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    if args.len() > frame.k as usize {
        let spread = frame.scratch() + 35;
        let n = i32::try_from(args.len()).unwrap_or(i32::MAX);
        wasm!(function;
            I32Const(n), Call(helpers.new_array()),
            I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(spread),
        );
        for (j, &arg) in (0u64..).zip(args) {
            wasm!(function; LocalGet(spread), I32WrapI64);
            emit_array_elems(function);
            wasm!(function; LocalGet(arg), I64Store(elem_slot(j)));
        }
        wasm!(function; LocalGet(spread), GlobalSet(SPREAD_ARGS_GLOBAL));
    }
    if let Some(this) = this {
        wasm!(function; LocalGet(this));
    } else {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; GlobalSet(THIS_GLOBAL));
    let count = args.len();
    invoke(
        function,
        callee,
        |function| emit_argc(count, function),
        helpers,
    );
}

/// Push the result of the internal protocol builtin `op` with `this` = i64
/// `this` and `env` = i64 `env` (`0` when `None`), no arguments; a throw
/// is left pending.
fn call_protocol(
    function: &mut Function,
    op: ProtocolOp,
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
    wasm!(function; Call(helpers.builtin(Builtin::Protocol(op))));
}

/// `IteratorClose(iterator, throw completion)` of the iterator in i64 `it`
/// with an exception pending, then return: the pending exception is set
/// aside (in i64 `saved`) around `GetMethod(it, "return")` and its call
/// and rethrown, whatever they do.
fn close_pending(function: &mut Function, it: u32, saved: u32, helpers: Helpers<'_>) {
    wasm!(function;
        GlobalGet(EXC_VALUE_GLOBAL), LocalSet(saved),
        I32Const(0), GlobalSet(EXC_PENDING_GLOBAL),
    );
    call_protocol(function, ProtocolOp::Close, it, None, helpers);
    wasm!(function;
        Drop,
        LocalGet(saved), GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I64Const(UNDEFINED_BITS), Return,
    );
}

/// Throw a `TypeError` with `text`, leaving it pending (no return), so the
/// caller's abrupt path follows.
fn raise_type(
    function: &mut Function,
    frame: Frame,
    text: CalleeErrorText<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    guarded(function, helpers, |function| {
        throw_type_error(function, frame, text, strings, helpers);
    });
}

/// The operands of [`emit_flattenable`].
struct Flattenable<'a> {
    /// The i64 local holding the value.
    o: u32,
    /// The i64 local receiving the iterator.
    it: u32,
    /// The i64 local receiving its `next`.
    next: u32,
    /// An i64 temp.
    method: u32,
    /// Strings are iterated (`iterate-string-primitives`), not rejected.
    iterate_strings: bool,
    /// The `TypeError` of a rejected primitive.
    not_object: CalleeErrorText<'static>,
    /// The abrupt path, entered with the exception pending; it must not
    /// fall through.
    abrupt: &'a dyn Fn(&mut Function),
}

/// `GetIteratorFlattenable(o, strings)` into the record `it` / `next`: a
/// non-object throws unless `iterate_strings` and a string;
/// `GetMethod(o, @@iterator)`, `o` itself when nullish, else its call,
/// which must yield an object; then `GetIteratorDirect` (`next` read once).
/// Every throw runs `abrupt`.
fn emit_flattenable(
    function: &mut Function,
    frame: Frame,
    op: &Flattenable<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let Flattenable {
        o,
        it,
        next,
        method,
        iterate_strings,
        not_object,
        abrupt,
    } = *op;
    let if_pending = |function: &mut Function| {
        wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
        abrupt(function);
        wasm!(function; End);
    };
    emit_is_object(function, o);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    if iterate_strings {
        emit_is_string(function, o);
        wasm!(function; I32Eqz, If(BlockType::Empty));
    }
    raise_type(function, frame, not_object, strings, helpers);
    abrupt(function);
    if iterate_strings {
        wasm!(function; End);
    }
    wasm!(function;
        End,
        LocalGet(o), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    if_pending(function);
    wasm!(function; LocalGet(o), LocalSet(it));
    emit_is_nullish(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_function(function, method);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    raise_type(function, frame, FROM_NOT_CALLABLE, strings, helpers);
    abrupt(function);
    wasm!(function; End);
    call_args(function, frame, method, Some(o), &[], helpers);
    wasm!(function; LocalSet(it));
    if_pending(function);
    wasm!(function; End);
    emit_is_object(function, it);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    raise_type(function, frame, FROM_NOT_OBJECT, strings, helpers);
    abrupt(function);
    wasm!(function;
        End,
        LocalGet(it), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(next),
    );
    if_pending(function);
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
    let proto = m64(u64::from(abi::OBJ_PROTO_OFFSET));
    let iterator_proto = intrinsic_bits(Intrinsic::IteratorPrototype, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(o));
    let abrupt = |function: &mut Function| wasm!(function; I64Const(UNDEFINED_BITS), Return);
    let op = Flattenable {
        o,
        it,
        next,
        method,
        iterate_strings: true,
        not_object: FROM_PRIMITIVE,
        abrupt: &abrupt,
    };
    emit_flattenable(function, frame, &op, strings, helpers);
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

/// A lazy helper `Iterator.prototype.<kind>(arg)`: `this` must be an
/// object; the argument is validated before anything else is read
/// (`map` / `filter` / `flatMap`: callable; `take` / `drop`:
/// `ToNumber`, then `NaN` or a negative integer is a `RangeError`), a
/// failure closing `this` (`IteratorClose` of `{ this, undefined }` under
/// the throw); then `GetIteratorDirect(this)` and a fresh helper object
/// over `%IteratorHelperPrototype%` in state suspended-start (nothing is
/// stepped before its first `next`).
fn emit_helper(
    kind: HelperKind,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (o, arg, next, saved) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (obj, limit) = (frame.i32(0), frame.f64(0));
    let index = match kind {
        HelperKind::Map => 0,
        HelperKind::Filter => 1,
        HelperKind::Take => 2,
        HelperKind::Drop => 3,
        HelperKind::FlatMap => 4,
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_is_object(function, o);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, HELPER_NOT_OBJECT[index], strings, helpers);
    wasm!(function; End);
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    if matches!(kind, HelperKind::Take | HelperKind::Drop) {
        wasm!(function;
            LocalGet(arg), Call(helpers.to_number), LocalSet(next),
            GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty),
        );
        close_pending(function, o, saved, helpers);
        wasm!(function;
            End,
            LocalGet(next), F64ReinterpretI64, F64Trunc, LocalTee(limit),
            LocalGet(limit), F64Ne,
            LocalGet(limit), F64Const(Ieee64::from(0.0)), F64Lt, I32Or,
            If(BlockType::Empty),
        );
        guarded(function, helpers, |function| {
            emit_throw_error(&limit_error(), function, frame.scratch(), strings, helpers);
        });
        close_pending(function, o, saved, helpers);
        // `+ 0` turns a truncated `-0` into `+0`.
        wasm!(function;
            End,
            LocalGet(limit), F64Const(Ieee64::from(0.0)), F64Add, LocalSet(limit),
        );
    } else {
        emit_is_function(function, arg);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        raise_type(function, frame, CALLBACK_NOT_CALLABLE, strings, helpers);
        close_pending(function, o, saved, helpers);
        wasm!(function; End, F64Const(Ieee64::from(0.0)), LocalSet(limit));
    }
    wasm!(function;
        LocalGet(o), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(next),
    );
    return_if_pending(function);
    let header = TYPE_OBJECT | (i32::from(abi::CLASS_ITERATOR) << 24);
    let proto = intrinsic_bits(Intrinsic::IteratorHelperPrototype, helpers);
    wasm!(function;
        I32Const(HELPER_BLOCK_SIZE), Call(helpers.alloc), LocalTee(obj),
        I32Const(header), I32Store(m32(0)),
        LocalGet(obj), I32Const(0), I32Store(m32(u64::from(abi::OBJ_PROPS_OFFSET))),
        LocalGet(obj), I64Const(proto), I64Store(m64(u64::from(abi::OBJ_PROTO_OFFSET))),
        LocalGet(obj), I64Const(UNDEFINED_BITS), I64Store(m64(WRAP_ITERATED_OFFSET)),
        LocalGet(obj), I32Const(SUSPENDED_START), I32Store(m32(HELPER_STATE_OFFSET)),
        LocalGet(obj), I32Const(helper_kind_byte(kind)), I32Store(m32(WRAP_KIND_OFFSET)),
        LocalGet(obj), LocalGet(o), I64Store(m64(HELPER_ITERATOR_OFFSET)),
        LocalGet(obj), LocalGet(next), I64Store(m64(HELPER_NEXT_OFFSET)),
        LocalGet(obj), LocalGet(limit), F64Store(m64(HELPER_COUNTER_OFFSET)),
        LocalGet(obj), LocalGet(arg), I64Store(m64(HELPER_CALLBACK_OFFSET)),
        LocalGet(obj), I64Const(UNDEFINED_BITS), I64Store(m64(HELPER_INNER_OFFSET)),
        LocalGet(obj), I64Const(UNDEFINED_BITS), I64Store(m64(HELPER_INNER_NEXT_OFFSET)),
        LocalGet(obj), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// The locals of a helper's `next` / `return` body.
#[derive(Clone, Copy)]
struct Helper {
    /// The receiver (i64).
    this: u32,
    /// The receiver's address (i32).
    h: u32,
    /// The underlying iterator (i64).
    it: u32,
    /// The underlying `next` (i64).
    next: u32,
    /// A step result (i64).
    r: u32,
    /// A stepped value (i64).
    v: u32,
    /// A callback result, or the inner iterator (i64).
    m: u32,
    /// The callback, or the inner `next` (i64).
    cb: u32,
    /// The counter as a jsval (i64).
    cv: u32,
    /// The exception set aside while closing (i64).
    saved: u32,
    /// An i64 temp.
    tmp: u32,
    /// The counter or remaining limit (f64).
    counter: u32,
}

impl Helper {
    /// The locals of `frame`.
    fn new(frame: Frame) -> Self {
        Self {
            this: frame.i64(0),
            h: frame.i32(0),
            it: frame.i64(1),
            next: frame.i64(2),
            r: frame.i64(3),
            v: frame.y64(0),
            m: frame.y64(1),
            cb: frame.y64(2),
            cv: frame.y64(3),
            saved: frame.y64(4),
            tmp: frame.y64(5),
            counter: frame.f64(0),
        }
    }
}

/// How a helper body leaves on a throw.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Abrupt {
    /// The helper completes and the throw propagates (a throw from the
    /// underlying iterator itself).
    Propagate,
    /// The helper completes and the underlying iterator is closed under the
    /// throw (`IfAbruptCloseIterator`).
    Close,
}

/// Store generator state `state` into the helper.
fn set_state(function: &mut Function, n: Helper, state: i32) {
    wasm!(function; LocalGet(n.h), I32Const(state), I32Store(m32(HELPER_STATE_OFFSET)));
}

/// Leave the helper body with the pending exception under `abrupt`.
fn leave_abrupt(function: &mut Function, n: Helper, abrupt: Abrupt, helpers: Helpers<'_>) {
    set_state(function, n, COMPLETED);
    match abrupt {
        Abrupt::Propagate => wasm!(function; I64Const(UNDEFINED_BITS), Return),
        Abrupt::Close => close_pending(function, n.it, n.saved, helpers),
    }
}

/// [`leave_abrupt`] when an exception is pending.
fn if_abrupt(function: &mut Function, n: Helper, abrupt: Abrupt, helpers: Helpers<'_>) {
    wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    leave_abrupt(function, n, abrupt, helpers);
    wasm!(function; End);
}

/// Complete the helper and return `{ value: undefined, done: true }`.
fn finish(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    set_state(function, n, COMPLETED);
    emit_node(&done_result(), function, frame.scratch(), strings, helpers);
    wasm!(function; Return);
}

/// `IteratorStep` of the record i64 `it` / `next` into `n.r`, pushing i32
/// `1` once done; a throw leaves under `abrupt`.
fn step(
    function: &mut Function,
    n: Helper,
    (it, next): (u32, u32),
    abrupt: Abrupt,
    helpers: Helpers<'_>,
) {
    call_protocol(function, ProtocolOp::Step, it, Some(next), helpers);
    wasm!(function; LocalSet(n.r));
    if_abrupt(function, n, abrupt, helpers);
    wasm!(function; LocalGet(n.r), I64Const(HOLE_BITS), I64Eq);
}

/// `IteratorValue` of the step result `n.r` into `n.v`; a throw leaves
/// under `abrupt`.
fn step_value(
    function: &mut Function,
    n: Helper,
    abrupt: Abrupt,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(n.r), I32Const(static_key(strings, "value")), Call(helpers.get), LocalSet(n.v),
    );
    if_abrupt(function, n, abrupt, helpers);
}

/// `IteratorStepValue` of the underlying record into `n.v`: done completes
/// the helper (`{ value: undefined, done: true }`).
fn step_underlying(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    step(function, n, (n.it, n.next), Abrupt::Propagate, helpers);
    wasm!(function; If(BlockType::Empty));
    finish(function, n, frame, strings, helpers);
    wasm!(function; End);
    step_value(function, n, Abrupt::Propagate, strings, helpers);
}

/// Suspend the helper yielding the jsval in i64 `value`.
fn yield_value(
    function: &mut Function,
    n: Helper,
    value: u32,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    set_state(function, n, SUSPENDED_YIELD);
    emit_node(
        &iter_result(IrNode::Local(value), false),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; Return);
}

/// `Call(callback, undefined, « n.v, 𝔽(counter) »)` into `n.m`, then the
/// counter incremented; a throw closes the underlying iterator.
fn call_callback(function: &mut Function, n: Helper, frame: Frame, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(n.h), F64Load(m64(HELPER_COUNTER_OFFSET)), LocalTee(n.counter),
        I64ReinterpretF64, LocalSet(n.cv),
        LocalGet(n.h), I64Load(m64(HELPER_CALLBACK_OFFSET)), LocalSet(n.cb),
    );
    call_args(function, frame, n.cb, None, &[n.v, n.cv], helpers);
    wasm!(function; LocalSet(n.m));
    if_abrupt(function, n, Abrupt::Close, helpers);
    wasm!(function;
        LocalGet(n.h), LocalGet(n.counter), F64Const(Ieee64::from(1.0)), F64Add,
        F64Store(m64(HELPER_COUNTER_OFFSET)),
    );
}

/// Read `this` into `n.this` / `n.h`, throwing unless it is a helper object
/// (`GeneratorValidate`'s brand check), then its state into i32 `state`:
/// running throws, completed returns `{ value: undefined, done: true }`.
fn helper_receiver(
    function: &mut Function,
    n: Helper,
    state: u32,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(n.this));
    emit_is_wrapper(function, n.this, abi::CLASS_ITERATOR);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(n.this), I32WrapI64,
        I32Load8U(MemArg { offset: WRAP_KIND_OFFSET, align: 0, memory_index: 0 }),
        I32Const(WRAP_KIND + 1), I32Sub, I32Const(HELPER_KINDS), I32LtU,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty),
    );
    throw_type_error(function, frame, WRAP_INCOMPATIBLE, strings, helpers);
    wasm!(function;
        End,
        LocalGet(n.this), I32WrapI64, LocalTee(n.h),
        I32Load(m32(HELPER_STATE_OFFSET)), LocalTee(state),
        I32Const(RUNNING), I32Eq, If(BlockType::Empty),
    );
    throw_type_error(function, frame, HELPER_RUNNING, strings, helpers);
    wasm!(function; End, LocalGet(state), I32Const(COMPLETED), I32Eq, If(BlockType::Empty));
    emit_node(&done_result(), function, frame.scratch(), strings, helpers);
    wasm!(function;
        Return, End,
        LocalGet(n.h), I64Load(m64(HELPER_ITERATOR_OFFSET)), LocalSet(n.it),
    );
}

/// `%IteratorHelperPrototype%.next()`: `GeneratorResume` of the helper's
/// closure (§5.7), running until its next yield or completion.
fn emit_helper_next(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let n = Helper::new(frame);
    let (state, kind) = (frame.i32(1), frame.i32(2));
    helper_receiver(function, n, state, frame, strings, helpers);
    set_state(function, n, RUNNING);
    wasm!(function;
        LocalGet(n.h), I64Load(m64(HELPER_NEXT_OFFSET)), LocalSet(n.next),
        LocalGet(n.h), I32Load8U(MemArg { offset: WRAP_KIND_OFFSET, align: 0, memory_index: 0 }),
        LocalSet(kind),
    );
    for helper in [
        HelperKind::Map,
        HelperKind::Filter,
        HelperKind::Take,
        HelperKind::Drop,
        HelperKind::FlatMap,
    ] {
        wasm!(function;
            LocalGet(kind), I32Const(helper_kind_byte(helper)), I32Eq, If(BlockType::Empty),
        );
        match helper {
            HelperKind::Map => next_map(function, n, frame, strings, helpers),
            HelperKind::Filter => next_filter(function, n, frame, strings, helpers),
            HelperKind::Take => next_take(function, n, frame, strings, helpers),
            HelperKind::Drop => next_drop(function, n, frame, strings, helpers),
            HelperKind::FlatMap => next_flat_map(function, n, frame, strings, helpers),
        }
        wasm!(function; End);
    }
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// `map`: the mapped value of the next underlying value.
fn next_map(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    step_underlying(function, n, frame, strings, helpers);
    call_callback(function, n, frame, helpers);
    yield_value(function, n, n.m, frame, strings, helpers);
}

/// `filter`: the next underlying value the predicate selects.
fn next_filter(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; Loop(BlockType::Empty));
    step_underlying(function, n, frame, strings, helpers);
    call_callback(function, n, frame, helpers);
    let selected = IrNode::ConvertCall {
        op: ConvertOp::ToBoolean,
        arg: Box::new(IrNode::Local(n.m)),
        radix: None,
    };
    emit_node(&selected, function, frame.scratch(), strings, helpers);
    wasm!(function; I64Const(TRUE_BITS), I64Eq, If(BlockType::Empty));
    yield_value(function, n, n.v, frame, strings, helpers);
    wasm!(function; End, Br(0), End);
}

/// Count one step of a `take` / `drop` limit: `remaining - 1` unless
/// `+∞` (`remaining` in `n.counter`).
fn count_down(function: &mut Function, n: Helper) {
    wasm!(function;
        LocalGet(n.counter), F64Const(Ieee64::from(f64::INFINITY)), F64Ne, If(BlockType::Empty),
        LocalGet(n.h), LocalGet(n.counter), F64Const(Ieee64::from(1.0)), F64Sub,
        F64Store(m64(HELPER_COUNTER_OFFSET)),
        End,
    );
}

/// `take`: once the limit is reached the underlying iterator is closed
/// (`IteratorClose` under a return completion) and the helper completes;
/// else the next underlying value.
fn next_take(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(n.h), F64Load(m64(HELPER_COUNTER_OFFSET)), LocalTee(n.counter),
        F64Const(Ieee64::from(0.0)), F64Eq, If(BlockType::Empty),
    );
    call_protocol(function, ProtocolOp::Close, n.it, None, helpers);
    wasm!(function; Drop);
    set_state(function, n, COMPLETED);
    return_if_pending(function);
    finish(function, n, frame, strings, helpers);
    wasm!(function; End);
    count_down(function, n);
    step_underlying(function, n, frame, strings, helpers);
    yield_value(function, n, n.v, frame, strings, helpers);
}

/// `drop`: the first call steps past `limit` values (no value read),
/// completing when the underlying iterator ends first; then the next
/// underlying value.
fn next_drop(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(n.h), F64Load(m64(HELPER_COUNTER_OFFSET)), LocalTee(n.counter),
        F64Const(Ieee64::from(0.0)), F64Gt, I32Eqz, BrIf(1),
    );
    count_down(function, n);
    step(function, n, (n.it, n.next), Abrupt::Propagate, helpers);
    wasm!(function; If(BlockType::Empty));
    finish(function, n, frame, strings, helpers);
    wasm!(function; End, Br(0), End, End);
    step_underlying(function, n, frame, strings, helpers);
    yield_value(function, n, n.v, frame, strings, helpers);
}

/// `flatMap`: the next value of the open inner iterator; when none is open
/// (or it ends), the mapper's result for the next underlying value is
/// opened (`GetIteratorFlattenable(mapped, reject-primitives)`, so a string
/// throws). A throw from the mapper, the opening or the inner iterator
/// closes the underlying iterator.
fn next_flat_map(
    function: &mut Function,
    n: Helper,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let abrupt = |function: &mut Function| leave_abrupt(function, n, Abrupt::Close, helpers);
    wasm!(function;
        Loop(BlockType::Empty),
        LocalGet(n.h), I64Load(m64(HELPER_INNER_OFFSET)), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
    );
    step_underlying(function, n, frame, strings, helpers);
    call_callback(function, n, frame, helpers);
    // `n.m` (the mapped value) is read before `it` (`n.r`) is written.
    let op = Flattenable {
        o: n.m,
        it: n.r,
        next: n.cb,
        method: n.tmp,
        iterate_strings: false,
        not_object: FLAT_NOT_OBJECT,
        abrupt: &abrupt,
    };
    emit_flattenable(function, frame, &op, strings, helpers);
    wasm!(function;
        LocalGet(n.h), LocalGet(n.r), I64Store(m64(HELPER_INNER_OFFSET)),
        LocalGet(n.h), LocalGet(n.cb), I64Store(m64(HELPER_INNER_NEXT_OFFSET)),
        End,
        LocalGet(n.h), I64Load(m64(HELPER_INNER_OFFSET)), LocalSet(n.m),
        LocalGet(n.h), I64Load(m64(HELPER_INNER_NEXT_OFFSET)), LocalSet(n.cb),
    );
    step(function, n, (n.m, n.cb), Abrupt::Close, helpers);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(n.h), I64Const(UNDEFINED_BITS), I64Store(m64(HELPER_INNER_OFFSET)),
        Br(1),
        End,
    );
    step_value(function, n, Abrupt::Close, strings, helpers);
    yield_value(function, n, n.v, frame, strings, helpers);
    wasm!(function; End);
}

/// `%IteratorHelperPrototype%.return()`: before the first `next`, the
/// helper completes and its underlying iterator is closed; after a yield,
/// the return completion resumes the closure: a `flatMap`'s open inner
/// iterator is closed first (a throw from that closes the underlying one
/// under it), then the underlying iterator, and the helper completes.
/// `{ value: undefined, done: true }` either way.
fn emit_helper_return(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let n = Helper::new(frame);
    let state = frame.i32(1);
    helper_receiver(function, n, state, frame, strings, helpers);
    wasm!(function; LocalGet(state), I32Const(SUSPENDED_START), I32Eq, If(BlockType::Empty));
    set_state(function, n, COMPLETED);
    call_protocol(function, ProtocolOp::Close, n.it, None, helpers);
    wasm!(function; Drop);
    return_if_pending(function);
    emit_node(&done_result(), function, frame.scratch(), strings, helpers);
    wasm!(function; Return, End);
    set_state(function, n, RUNNING);
    wasm!(function;
        LocalGet(n.h), I64Load(m64(HELPER_INNER_OFFSET)), LocalTee(n.m),
        I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty),
    );
    call_protocol(function, ProtocolOp::Close, n.m, None, helpers);
    wasm!(function; Drop);
    if_abrupt(function, n, Abrupt::Close, helpers);
    wasm!(function; End);
    call_protocol(function, ProtocolOp::Close, n.it, None, helpers);
    wasm!(function; Drop);
    set_state(function, n, COMPLETED);
    return_if_pending(function);
    emit_node(&done_result(), function, frame.scratch(), strings, helpers);
}

/// The locals of an eager helper body.
#[derive(Clone, Copy)]
struct Eager {
    /// The iterator, `this` (i64).
    o: u32,
    /// The callback (i64).
    cb: u32,
    /// Its `next` (i64).
    next: u32,
    /// The exception set aside while closing (i64).
    saved: u32,
    /// A step result (i64).
    r: u32,
    /// A stepped value (i64).
    v: u32,
    /// The `reduce` accumulator (i64).
    acc: u32,
    /// A callback result (i64).
    res: u32,
    /// The counter as a jsval (i64).
    cv: u32,
    /// The counter (f64).
    counter: u32,
    /// The `toArray` result array (i32).
    list: u32,
}

impl Eager {
    /// The locals of `frame`.
    fn new(frame: Frame) -> Self {
        Self {
            o: frame.i64(0),
            cb: frame.i64(1),
            next: frame.i64(2),
            saved: frame.i64(3),
            r: frame.y64(0),
            v: frame.y64(1),
            acc: frame.y64(2),
            res: frame.y64(3),
            cv: frame.y64(4),
            counter: frame.f64(0),
            list: frame.i32(0),
        }
    }
}

/// `IteratorStepValue` of the record `e.o` / `e.next` into `e.v`, branching
/// to label depth `done` once done; a throw propagates (no close).
fn eager_step(
    function: &mut Function,
    e: Eager,
    done: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    call_protocol(function, ProtocolOp::Step, e.o, Some(e.next), helpers);
    wasm!(function; LocalSet(e.r));
    return_if_pending(function);
    wasm!(function;
        LocalGet(e.r), I64Const(HOLE_BITS), I64Eq, BrIf(done),
        LocalGet(e.r), I32Const(static_key(strings, "value")), Call(helpers.get), LocalSet(e.v),
    );
    return_if_pending(function);
}

/// `reduce`'s accumulator: the initial value when one is passed (counter
/// `0`), else the first value (counter `1`), an empty iterator throwing a
/// `TypeError`.
fn eager_reduce_start(
    function: &mut Function,
    e: Eager,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Ge,
        If(BlockType::Empty),
    );
    frame.arg(function, 1);
    wasm!(function; LocalSet(e.acc), Else, Block(BlockType::Empty));
    eager_step(function, e, 0, strings, helpers);
    wasm!(function;
        LocalGet(e.v), LocalSet(e.acc),
        F64Const(Ieee64::from(1.0)), LocalSet(e.counter),
        Br(1),
        End,
    );
    throw_type_error(function, frame, REDUCE_EMPTY, strings, helpers);
    wasm!(function; End);
}

/// When `ToBoolean(e.res)` is `select`, close the iterator (normal
/// completion; a throw from that propagates) and return `found` (the
/// stepped value) or else `bits`.
fn eager_early(
    function: &mut Function,
    e: Eager,
    (select, found, bits): (bool, bool, i64),
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let truthy = IrNode::ConvertCall {
        op: ConvertOp::ToBoolean,
        arg: Box::new(IrNode::Local(e.res)),
        radix: None,
    };
    emit_node(&truthy, function, frame.scratch(), strings, helpers);
    wasm!(function; I64Const(TRUE_BITS), I64Eq);
    if !select {
        wasm!(function; I32Eqz);
    }
    wasm!(function; If(BlockType::Empty));
    call_protocol(function, ProtocolOp::Close, e.o, None, helpers);
    wasm!(function; Drop);
    return_if_pending(function);
    if found {
        wasm!(function; LocalGet(e.v));
    } else {
        wasm!(function; I64Const(bits));
    }
    wasm!(function; Return, End);
}

/// An eager helper `Iterator.prototype.<kind>(fn[, initialValue])`: `this`
/// must be an object; the callback must be callable (else a `TypeError`
/// closing `this`, as for the lazy helpers); then `GetIteratorDirect(this)`
/// (`next` read once) and the iterator is consumed, the counter passed as
/// the callback's last argument. A throw from stepping or reading a value
/// propagates; a throwing callback closes the iterator under its throw;
/// `some` / `every` / `find` close it (normal completion) on an early
/// result. `reduce` without an initial value starts from the first value
/// (counter `1`; an empty iterator is a `TypeError`).
fn emit_eager(
    kind: EagerKind,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let e = Eager::new(frame);
    let index = match kind {
        EagerKind::Reduce => 0,
        EagerKind::ToArray => 1,
        EagerKind::ForEach => 2,
        EagerKind::Some => 3,
        EagerKind::Every => 4,
        EagerKind::Find => 5,
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(e.o));
    emit_is_object(function, e.o);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, EAGER_NOT_OBJECT[index], strings, helpers);
    wasm!(function; End);
    if kind != EagerKind::ToArray {
        frame.arg(function, 0);
        wasm!(function; LocalSet(e.cb));
        emit_is_function(function, e.cb);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        raise_type(function, frame, CALLBACK_NOT_CALLABLE, strings, helpers);
        close_pending(function, e.o, e.saved, helpers);
        wasm!(function; End);
    }
    wasm!(function;
        LocalGet(e.o), I32Const(static_key(strings, "next")), Call(helpers.get), LocalSet(e.next),
    );
    return_if_pending(function);
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(e.counter));
    match kind {
        EagerKind::ToArray => {
            wasm!(function; I32Const(0), Call(helpers.new_array()), LocalSet(e.list));
        }
        EagerKind::Reduce => eager_reduce_start(function, e, frame, strings, helpers),
        _ => {}
    }
    wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
    eager_step(function, e, 1, strings, helpers);
    wasm!(function; LocalGet(e.counter), I64ReinterpretF64, LocalSet(e.cv));
    if kind == EagerKind::ToArray {
        wasm!(function; LocalGet(e.list), LocalGet(e.v), Call(helpers.array_push), Drop);
    } else {
        let args: &[u32] = if kind == EagerKind::Reduce {
            &[e.acc, e.v, e.cv]
        } else {
            &[e.v, e.cv]
        };
        call_args(function, frame, e.cb, None, args, helpers);
        wasm!(function; LocalSet(e.res), GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
        close_pending(function, e.o, e.saved, helpers);
        wasm!(function; End);
    }
    match kind {
        EagerKind::Reduce => wasm!(function; LocalGet(e.res), LocalSet(e.acc)),
        EagerKind::Some => eager_early(
            function,
            e,
            (true, false, TRUE_BITS),
            frame,
            strings,
            helpers,
        ),
        EagerKind::Every => eager_early(
            function,
            e,
            (false, false, FALSE_BITS),
            frame,
            strings,
            helpers,
        ),
        EagerKind::Find => eager_early(function, e, (true, true, 0), frame, strings, helpers),
        EagerKind::ToArray | EagerKind::ForEach => {}
    }
    wasm!(function;
        LocalGet(e.counter), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(e.counter),
        Br(0),
        End, End,
    );
    match kind {
        EagerKind::Reduce => wasm!(function; LocalGet(e.acc)),
        EagerKind::ToArray => {
            wasm!(function; LocalGet(e.list), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
        }
        EagerKind::ForEach | EagerKind::Find => wasm!(function; I64Const(UNDEFINED_BITS)),
        EagerKind::Some => wasm!(function; I64Const(FALSE_BITS)),
        EagerKind::Every => wasm!(function; I64Const(TRUE_BITS)),
    }
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
