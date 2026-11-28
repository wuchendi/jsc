//! The `%TypedArray.prototype%` methods of stage T3b (DESIGN.md §7.9):
//! `at`, `copyWithin`, `every`, `fill`, `filter`, `find`, `findIndex`,
//! `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`, `join`,
//! `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`, `set`, `slice`,
//! `some`, `sort`, `subarray`, `toLocaleString`, `toReversed`, `toSorted`,
//! `with` (`toString` is `%Array.prototype.toString%` itself).
//!
//! Each body follows ECMA-262 §23.2.3 step by step: `ValidateTypedArray`
//! (`__ta_validate`) first (`set` and `subarray` only check the receiver's
//! type), the length read once, the arguments converted in spec order, and
//! the spec's explicit re-validations after user code (a conversion, a
//! callback, a comparator, a species constructor) emitted as written. Every
//! element read or write in between goes through `__ta_get` / `__ta_set` /
//! `__ta_set_value`, which re-check the index against the live buffer, so a
//! buffer detached or shrunk by user code reads `undefined` and ignores
//! writes (DESIGN.md §6.2). The bulk copies of `copyWithin`, `set` and
//! `slice` re-derive their byte range from the witness taken after the last
//! user-code step.

#[allow(clippy::wildcard_imports)]
use super::*;

use super::array::{CallArg, MergeLocals, locale_element, merge_pairs, pair_at, push_call};
use super::typed_array::{
    FROM_NULLISH, create_same_type, field, get_index, is_kind, length_of_array_like, list_element,
    m8, range_error_if, species_create, to_integer, type_error_if,
};
use crate::emit::binary::RAW_DATA;
use crate::emit::realm::TypedArrayOp;
use crate::emit::registry::Big;

/// `[message, stack]` of a `TypeError` with `message`.
macro_rules! type_error {
    ($message:expr) => {
        [$message, concat!("TypeError: ", $message)]
    };
}

/// `[message, stack]` of a `RangeError` with `message`.
macro_rules! range_error {
    ($message:expr) => {
        [$message, concat!("RangeError: ", $message)]
    };
}

/// `(op, receiver name)` of each method: the name its `__ta_validate` /
/// `__prop_throw` `TypeError`s splice in.
macro_rules! method_names {
    ($($op:ident => $name:literal),* $(,)?) => {
        [$((TypedArrayOp::$op, concat!("%TypedArray%.prototype.", $name))),*]
    };
}

/// The receiver name of each method (DESIGN.md §6.2).
const NAMES: [(TypedArrayOp, &str); 27] = method_names![
    At => "at",
    CopyWithin => "copyWithin",
    Every => "every",
    Fill => "fill",
    Filter => "filter",
    Find => "find",
    FindIndex => "findIndex",
    FindLast => "findLast",
    FindLastIndex => "findLastIndex",
    ForEach => "forEach",
    Includes => "includes",
    IndexOf => "indexOf",
    Join => "join",
    LastIndexOf => "lastIndexOf",
    Map => "map",
    Reduce => "reduce",
    ReduceRight => "reduceRight",
    Reverse => "reverse",
    Set => "set",
    Slice => "slice",
    Some => "some",
    Sort => "sort",
    Subarray => "subarray",
    ToLocaleString => "toLocaleString",
    ToReversed => "toReversed",
    ToSorted => "toSorted",
    With => "with",
];

/// `(op, TypeError)` of each callback method whose callback is not callable.
macro_rules! callback_texts {
    ($($op:ident => $name:literal),* $(,)?) => {
        [$((
            TypedArrayOp::$op,
            type_error!(concat!("%TypedArray%.prototype.", $name, " callback is not a function")),
        )),*]
    };
}

/// The not-callable `TypeError` of each callback method.
const CALLBACK_TEXTS: [(TypedArrayOp, CalleeErrorText<'static>); 11] = callback_texts![
    Every => "every",
    Filter => "filter",
    Find => "find",
    FindIndex => "findIndex",
    FindLast => "findLast",
    FindLastIndex => "findLastIndex",
    ForEach => "forEach",
    Map => "map",
    Reduce => "reduce",
    ReduceRight => "reduceRight",
    Some => "some",
];

/// `reduce` / `reduceRight` of an empty typed array without an initial value.
const REDUCE_EMPTY: CalleeErrorText<'static> =
    type_error!("Reduce of empty array with no initial value");

/// `sort` / `toSorted` with a comparator neither callable nor `undefined`.
const SORT_NOT_CALLABLE: CalleeErrorText<'static> =
    type_error!("The comparison function must be either a function or undefined");

/// `with` at an index that is not a valid integer index.
const WITH_INVALID_INDEX: CalleeErrorText<'static> = range_error!("Invalid typed array index");

/// `set` with a negative offset, or a source that does not fit.
const SET_OFFSET: CalleeErrorText<'static> = range_error!("offset is out of bounds");

/// `set` from a typed array of the other content type.
const SET_CONTENT_TYPE: CalleeErrorText<'static> =
    type_error!("Cannot mix BigInt and other types, use explicit conversions");

/// `toLocaleString` over an element whose `toLocaleString` is not callable.
const LOCALE_NOT_CALLABLE: CalleeErrorText<'static> =
    type_error!("%TypedArray%.prototype.toLocaleString: element toLocaleString is not a function");

/// The fixed `TypeError` and `RangeError` texts of this family.
const TEXTS: [CalleeErrorText<'static>; 6] = [
    REDUCE_EMPTY,
    SORT_NOT_CALLABLE,
    WITH_INVALID_INDEX,
    SET_OFFSET,
    SET_CONTENT_TYPE,
    LOCALE_NOT_CALLABLE,
];

/// The `join` / `toLocaleString` separator.
const COMMA: &str = ",";

/// The first `BigInt` element kind (DESIGN.md §6.2).
const FIRST_BIGINT_KIND: i32 = 10;

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    TEXTS
        .into_iter()
        .chain(CALLBACK_TEXTS.map(|(_, text)| text))
        .flatten()
        .chain(NAMES.map(|(_, name)| name))
        .chain([COMMA, ""])
}

/// The receiver name of `op`.
fn name(op: TypedArrayOp) -> &'static str {
    NAMES
        .iter()
        .find_map(|&(o, name)| (o == op).then_some(name))
        .unwrap_or(NAMES[0].1)
}

/// The not-callable `TypeError` of the callback method `op`.
fn callback_text(op: TypedArrayOp) -> CalleeErrorText<'static> {
    CALLBACK_TEXTS
        .iter()
        .find_map(|&(o, text)| (o == op).then_some(text))
        .unwrap_or(CALLBACK_TEXTS[0].1)
}

/// The locals of a method body.
#[derive(Clone, Copy)]
struct L {
    frame: Frame,
    op: TypedArrayOp,
    /// The receiver (i64).
    o: u32,
    /// The leading arguments (i64).
    a: [u32; 3],
    /// i64 temps: an element, a call result.
    v: u32,
    r: u32,
    /// The result typed array (i64).
    out: u32,
    /// A constructor (i64).
    c: u32,
    /// A number jsval (i64): a creation length.
    nv: u32,
    /// i64 temps.
    t: [u32; 3],
    /// The validated length (i32).
    len: u32,
    /// i32 temps.
    k: u32,
    n: u32,
    kind: u32,
    i: [u32; 4],
    /// f64 temps.
    f: [u32; 4],
    /// Rooted i32 bases: two arrays, a string accumulator, a separator, a
    /// call scratch base.
    list: u32,
    list2: u32,
    acc: u32,
    sep: u32,
    base: u32,
}

impl L {
    fn new(frame: Frame, op: TypedArrayOp) -> Self {
        Self {
            frame,
            op,
            o: frame.y64(0),
            a: [frame.y64(1), frame.y64(2), frame.y64(3)],
            v: frame.y64(4),
            r: frame.y64(5),
            out: frame.x64(0),
            c: frame.x64(1),
            nv: frame.i64(0),
            t: [frame.i64(1), frame.i64(2), frame.i64(3)],
            len: frame.w32(0),
            k: frame.w32(1),
            n: frame.w32(2),
            kind: frame.w32(3),
            i: [frame.w32(4), frame.w32(5), frame.w32(6), frame.w32(7)],
            f: [frame.zf64(0), frame.zf64(1), frame.zf64(2), frame.zf64(3)],
            list: frame.base(0),
            list2: frame.base(1),
            acc: frame.base(2),
            sep: frame.base(3),
            base: frame.base(4),
        }
    }

    /// Read the first `n` arguments into [`Self::a`] (on entry, before any
    /// user code runs).
    fn args(self, function: &mut FnBuilder<'_>, n: u32) {
        for (i, local) in (0..n).zip(self.a) {
            self.frame.arg(function, i);
            wasm!(function; LocalSet(local));
        }
    }

    /// `ValidateTypedArray(this)` into `o` and the length into `len` (a
    /// throw returns).
    fn validate(self, function: &mut FnBuilder<'_>, strings: &StrOffsets, helpers: Helpers<'_>) {
        wasm!(function;
            GlobalGet(THIS_GLOBAL), LocalTee(self.o),
            I32Const(static_key(strings, name(self.op))), Call(helpers.ta_validate()),
            LocalSet(self.len),
        );
        return_if_pending(function);
    }

    /// `RequireInternalSlot(this, [[TypedArrayName]])` into `o`: the
    /// incompatible-receiver `TypeError` unless it is a typed array.
    fn require_typed_array(
        self,
        function: &mut FnBuilder<'_>,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(self.o));
        is_kind(function, self.o, abi::TYPE_TYPEDARRAY);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        self.prop_throw(function, PROP_THROW_TA_INCOMPATIBLE, strings, helpers);
        wasm!(function; End);
    }

    /// Raise `__prop_throw` mode `mode` with this method's name and return.
    fn prop_throw(
        self,
        function: &mut FnBuilder<'_>,
        mode: i32,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        wasm!(function;
            LocalGet(self.o), I64Const(HEAP_BITS | i64::from(static_key(strings, name(self.op)))),
            I32Const(mode), Call(helpers.prop_throw),
            I64Const(UNDEFINED_BITS), Return,
        );
    }

    /// The spec's re-validation `If IsTypedArrayOutOfBounds(taRecord), throw
    /// a TypeError` of the receiver (the out-of-bounds text of mode `45`).
    fn revalidate(self, function: &mut FnBuilder<'_>, strings: &StrOffsets, helpers: Helpers<'_>) {
        wasm!(function; LocalGet(self.o), I32WrapI64, Call(helpers.ta_oob()), If(BlockType::Empty));
        self.prop_throw(function, PROP_THROW_TA_OUT_OF_BOUNDS, strings, helpers);
        wasm!(function; End);
    }

    /// Throw the callback `TypeError` of this method unless argument 0 is
    /// callable.
    fn require_callback(
        self,
        function: &mut FnBuilder<'_>,
        strings: &StrOffsets,
        helpers: Helpers<'_>,
    ) {
        emit_is_function(function, self.a[0], helpers);
        wasm!(function; I32Eqz);
        type_error_if(
            function,
            self.frame,
            callback_text(self.op),
            strings,
            helpers,
        );
    }

    /// The validated length as an f64 into `out`.
    fn len_f64(self, function: &mut FnBuilder<'_>, out: u32) {
        wasm!(function; LocalGet(self.len), F64ConvertI32U, LocalSet(out));
    }
}

/// `v = __ta_get(ta, k)` for the typed array in i64 `ta` and the i32
/// index in local `k`, then [`return_if_element_pending`].
fn get(function: &mut FnBuilder<'_>, ta: u32, k: u32, v: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(ta), I32WrapI64, LocalGet(k), Call(helpers.ta_get()), LocalSet(v));
    return_if_element_pending(function, helpers);
}

/// After an element read: a `BigInt` element read allocates its value, and
/// out of memory leaves the `RangeError` pending with a placeholder value,
/// so return (the exception stays pending) before that value is used. Only
/// with the program's `BigInt` gate open (no `BigInt` typed array exists
/// otherwise, and a Number read never throws).
fn return_if_element_pending(function: &mut FnBuilder<'_>, helpers: Helpers<'_>) {
    if helpers.bigints() {
        return_if_pending(function);
    }
}

/// `__ta_set(ta, k, v)` of the number in i64 `v`.
fn set(function: &mut FnBuilder<'_>, ta: u32, k: u32, v: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(ta), I32WrapI64, LocalGet(k), LocalGet(v), Call(helpers.ta_set()));
}

/// The i32 local `n` as a number jsval into i64 `out`.
fn number(function: &mut FnBuilder<'_>, n: u32, out: u32) {
    wasm!(function; LocalGet(n), F64ConvertI32U, I64ReinterpretF64, LocalSet(out));
}

/// `ToIntegerOrInfinity` of the jsval in i64 `v` as a relative index into
/// the f64 length `len`, into f64 `out`: `max(len + r, 0)` when negative,
/// else `min(r, len)`; a throw returns.
fn relative(function: &mut FnBuilder<'_>, v: u32, len: u32, out: u32, helpers: Helpers<'_>) {
    to_integer(function, v, out, helpers);
    wasm!(function;
        LocalGet(out), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Result(ValType::F64)),
        LocalGet(len), LocalGet(out), F64Add, F64Const(Ieee64::from(0.0)), F64Max,
        Else,
        LocalGet(out), LocalGet(len), F64Min,
        End,
        LocalSet(out),
    );
}

/// [`relative`], or `len` itself when the jsval in `v` is `undefined` (an
/// `end` argument).
fn relative_end(function: &mut FnBuilder<'_>, v: u32, len: u32, out: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty), LocalGet(len), LocalSet(out), Else,
    );
    relative(function, v, len, out, helpers);
    wasm!(function; End);
}

/// `while k > 0 { k -= 1; body }` over the i32 local `k`.
fn down_loop(function: &mut FnBuilder<'_>, k: u32, body: impl FnOnce(&mut FnBuilder<'_>)) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(k), I32Eqz, BrIf(1),
        LocalGet(k), I32Const(1), I32Sub, LocalSet(k),
    );
    body(function);
    wasm!(function; Br(0), End, End);
}

/// Push the byte address of element `k` (an i32 local) of the typed array in
/// i64 `ta`: `data + 8 + byte_offset + (k << shift)`. Only for a validated
/// range (an empty byte block is address `0`, which a zero-length copy
/// never dereferences).
fn element_addr(function: &mut FnBuilder<'_>, ta: u32, k: u32) {
    field(function, ta, abi::TA_BUFFER_OFFSET);
    wasm!(function;
        I32Load(MemArg { offset: u64::from(abi::AB_DATA_OFFSET), align: 2, memory_index: 0 }),
        I32Const(RAW_DATA.cast_signed()), I32Add,
    );
    field(function, ta, abi::TA_BYTE_OFFSET_OFFSET);
    wasm!(function; I32Add, LocalGet(k));
    shift(function, ta);
    wasm!(function; I32Shl, I32Add);
}

/// Push the element shift of the typed array in i64 `ta`.
fn shift(function: &mut FnBuilder<'_>, ta: u32) {
    wasm!(function; LocalGet(ta), I32WrapI64, I32Load8U(m8(abi::TA_SHIFT_OFFSET)));
}

/// Push the element kind of the typed array in i64 `ta`.
fn kind(function: &mut FnBuilder<'_>, ta: u32) {
    wasm!(function; LocalGet(ta), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)));
}

/// Push `1` when the typed arrays in i64 `a` and `b` have different content
/// types (Number and `BigInt`).
fn content_types_differ(function: &mut FnBuilder<'_>, a: u32, b: u32) {
    kind(function, a);
    wasm!(function; I32Const(FIRST_BIGINT_KIND), I32GeU);
    kind(function, b);
    wasm!(function; I32Const(FIRST_BIGINT_KIND), I32GeU, I32Ne);
}

/// The value conversion of `TypedArraySetElement` for the typed array in
/// i64 `ta` of the jsval in i64 `v` into i64 `out`: `ToBigInt` for a `BigInt`
/// kind (DESIGN.md §6.2; only with the program's `BigInt` gate open, as no
/// `BigInt` typed array exists otherwise), else `ToNumber`; a throw returns.
fn to_element(function: &mut FnBuilder<'_>, ta: u32, v: u32, out: u32, helpers: Helpers<'_>) {
    if helpers.bigints() {
        is_bigint_kind(function, ta);
        wasm!(function;
            If(BlockType::Result(ValType::I64)),
            LocalGet(v), Call(helpers.bigint(Big::ToBigInt)),
            Else,
        );
    }
    wasm!(function; LocalGet(v), Call(helpers.to_number));
    if helpers.bigints() {
        wasm!(function; End);
    }
    wasm!(function; LocalSet(out));
    return_if_pending(function);
}

/// Push `1` when the typed array in i64 `ta` has the `BigInt` content type.
fn is_bigint_kind(function: &mut FnBuilder<'_>, ta: u32) {
    kind(function, ta);
    wasm!(function; I32Const(FIRST_BIGINT_KIND), I32GeU);
}

/// Emit the body of the T3b method `op` into `f`.
pub(super) fn emit(
    op: TypedArrayOp,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let l = L::new(frame, op);
    match op {
        TypedArrayOp::At => emit_at(f, l, strings, helpers),
        TypedArrayOp::CopyWithin => emit_copy_within(f, l, strings, helpers),
        TypedArrayOp::Every
        | TypedArrayOp::Some
        | TypedArrayOp::Find
        | TypedArrayOp::FindIndex
        | TypedArrayOp::FindLast
        | TypedArrayOp::FindLastIndex
        | TypedArrayOp::ForEach => emit_find(f, l, strings, helpers),
        TypedArrayOp::Fill => emit_fill(f, l, strings, helpers),
        TypedArrayOp::Filter => emit_filter(f, l, strings, helpers),
        TypedArrayOp::Includes => emit_includes(f, l, strings, helpers),
        TypedArrayOp::IndexOf => emit_index_of(f, l, strings, helpers),
        TypedArrayOp::LastIndexOf => emit_last_index_of(f, l, strings, helpers),
        TypedArrayOp::Join => emit_join(f, l, strings, helpers),
        TypedArrayOp::Map => emit_map(f, l, strings, helpers),
        TypedArrayOp::Reduce => emit_reduce(false, f, l, strings, helpers),
        TypedArrayOp::ReduceRight => emit_reduce(true, f, l, strings, helpers),
        TypedArrayOp::Reverse => emit_reverse(f, l, strings, helpers),
        TypedArrayOp::Set => emit_set(f, l, strings, helpers),
        TypedArrayOp::Slice => emit_slice(f, l, strings, helpers),
        TypedArrayOp::Sort => emit_sort(false, f, l, strings, helpers),
        TypedArrayOp::ToSorted => emit_sort(true, f, l, strings, helpers),
        TypedArrayOp::Subarray => emit_subarray(f, l, strings, helpers),
        TypedArrayOp::ToLocaleString => emit_to_locale_string(f, l, strings, helpers),
        TypedArrayOp::ToReversed => emit_to_reversed(f, l, strings, helpers),
        TypedArrayOp::With => emit_with(f, l, strings, helpers),
        // Only the T3b methods are routed here.
        _ => wasm!(f; I64Const(UNDEFINED_BITS)),
    }
}

/// `at(index)`: `k = ToIntegerOrInfinity(index)`, relative to `len` when
/// negative; `undefined` outside `[0, len)`, else `Get(O, k)` (`undefined`
/// when the conversion detached or shrank the buffer).
fn emit_at(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [r, len, ..] = l.f;
    l.args(function, 1);
    l.validate(function, strings, helpers);
    l.len_f64(function, len);
    to_integer(function, l.a[0], r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), LocalGet(len), LocalGet(r), F64Add, LocalSet(r), End,
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(r), LocalGet(len), F64Ge, I32Or,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(l.o), I32WrapI64, LocalGet(r), I32TruncF64U, Call(helpers.ta_get()),
    );
}

/// `copyWithin(target, start, end)`: the three relative indices over `len`,
/// `count = min(final - from, len - to)`; when positive the receiver is
/// re-validated and the bytes are moved (`memory.copy`, overlap-safe)
/// within the current length: the first `min(countBytes, limit - from,
/// limit - to)` bytes, which are exactly the ones the spec's byte loop
/// copies in either direction.
fn emit_copy_within(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let [to, from, fin, len] = l.f;
    let [limit, to_b, from_b, count] = l.i;
    l.args(function, 3);
    l.validate(function, strings, helpers);
    l.len_f64(function, len);
    relative(function, l.a[0], len, to, helpers);
    relative(function, l.a[1], len, from, helpers);
    relative_end(function, l.a[2], len, fin, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(from), F64Sub, LocalGet(len), LocalGet(to), F64Sub, F64Min,
        F64Const(Ieee64::from(0.0)), F64Gt,
        If(BlockType::Empty),
    );
    l.revalidate(function, strings, helpers);
    wasm!(function; LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()));
    shift(function, l.o);
    wasm!(function; I32Shl, LocalSet(limit), LocalGet(to), I32TruncF64U);
    shift(function, l.o);
    wasm!(function; I32Shl, LocalSet(to_b), LocalGet(from), I32TruncF64U);
    shift(function, l.o);
    wasm!(function;
        I32Shl, LocalSet(from_b),
        LocalGet(fin), LocalGet(from), F64Sub, LocalGet(len), LocalGet(to), F64Sub, F64Min,
        I32TruncF64U,
    );
    shift(function, l.o);
    wasm!(function;
        I32Shl, LocalSet(count),
        // count = min(count, limit - from, limit - to), signed.
        LocalGet(count), LocalGet(limit), LocalGet(from_b), I32Sub, LocalTee(l.n),
        LocalGet(count), LocalGet(l.n), I32LtS, Select, LocalSet(count),
        LocalGet(count), LocalGet(limit), LocalGet(to_b), I32Sub, LocalTee(l.n),
        LocalGet(count), LocalGet(l.n), I32LtS, Select, LocalSet(count),
        LocalGet(count), I32Const(0), I32GtS,
        If(BlockType::Empty),
        I32Const(0), LocalSet(l.k),
    );
    element_addr(function, l.o, l.k);
    wasm!(function; LocalGet(to_b), I32Add);
    element_addr(function, l.o, l.k);
    wasm!(function;
        LocalGet(from_b), I32Add,
        LocalGet(count), MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
        End,
        LocalGet(l.o),
    );
}

/// `every` / `some` / `find` / `findIndex` / `findLast` / `findLastIndex` /
/// `forEach(callback, thisArg)`: the callback check after the validation,
/// then `callback(O[k], k, O)` with `this = thisArg` for each `k < len`
/// (descending for `findLast*`), every element read by `Get` (`undefined`
/// once the buffer is detached or shrunk), until the result decides.
fn emit_find(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let op = l.op;
    let (cb, this_arg) = (l.a[0], l.a[1]);
    l.args(function, 2);
    l.validate(function, strings, helpers);
    l.require_callback(function, strings, helpers);
    let body = |function: &mut FnBuilder<'_>| {
        get(function, l.o, l.k, l.v, helpers);
        let args = [CallArg::Val(l.v), CallArg::Index(l.k), CallArg::Val(l.o)];
        push_call(function, l.frame, cb, this_arg, &args, helpers);
        wasm!(function; LocalSet(l.r));
        return_if_pending(function);
        if op == TypedArrayOp::ForEach {
            return;
        }
        emit_cond(
            &IrNode::Local(l.r),
            function,
            l.frame.scratch(),
            strings,
            helpers,
        );
        if op == TypedArrayOp::Every {
            wasm!(function; I32Eqz);
        }
        wasm!(function; If(BlockType::Empty));
        match op {
            TypedArrayOp::Find | TypedArrayOp::FindLast => wasm!(function; LocalGet(l.v)),
            TypedArrayOp::FindIndex | TypedArrayOp::FindLastIndex => {
                wasm!(function; LocalGet(l.k), F64ConvertI32U, I64ReinterpretF64);
            }
            TypedArrayOp::Some => wasm!(function; I64Const(TRUE_BITS)),
            _ => wasm!(function; I64Const(FALSE_BITS)),
        }
        wasm!(function; Return, End);
    };
    if matches!(op, TypedArrayOp::FindLast | TypedArrayOp::FindLastIndex) {
        wasm!(function; LocalGet(l.len), LocalSet(l.k));
        down_loop(function, l.k, body);
    } else {
        wasm!(function; I32Const(0), LocalSet(l.k));
        count_loop(function, l.k, l.len, body);
    }
    match op {
        TypedArrayOp::FindIndex | TypedArrayOp::FindLastIndex => {
            wasm!(function; I64Const((-1.0f64).to_bits().cast_signed()));
        }
        TypedArrayOp::Some => wasm!(function; I64Const(FALSE_BITS)),
        TypedArrayOp::Every => wasm!(function; I64Const(TRUE_BITS)),
        _ => wasm!(function; I64Const(UNDEFINED_BITS)),
    }
}

/// `fill(value, start, end)`: `ToNumber(value)` (`ToBigInt` for a `BigInt`
/// kind), the relative `start` / `end`, then the receiver is re-validated
/// and `end` clamped to the current length; every element of `[start, end)`
/// gets the value.
fn emit_fill(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [start, fin, len, _] = l.f;
    l.args(function, 3);
    l.validate(function, strings, helpers);
    l.len_f64(function, len);
    to_element(function, l.o, l.a[0], l.v, helpers);
    relative(function, l.a[1], len, start, helpers);
    relative_end(function, l.a[2], len, fin, helpers);
    l.revalidate(function, strings, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, F64Min,
        I32TruncF64U, LocalSet(l.n),
        LocalGet(start), I32TruncF64U, LocalSet(l.k),
    );
    count_loop(function, l.k, l.n, |function| {
        set(function, l.o, l.k, l.v, helpers);
    });
    wasm!(function; LocalGet(l.o));
}

/// `filter(callback, thisArg)`: the kept elements (by `ToBoolean` of
/// `callback(O[k], k, O)`) are collected first, then `A =
/// TypedArraySpeciesCreate(O, [captured])` gets them in order.
fn emit_filter(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (cb, this_arg) = (l.a[0], l.a[1]);
    l.args(function, 2);
    l.validate(function, strings, helpers);
    l.require_callback(function, strings, helpers);
    wasm!(function; I32Const(0), Call(helpers.new_array()), LocalSet(l.list));
    return_if_pending(function);
    wasm!(function; I32Const(0), LocalSet(l.n), I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.len, |function| {
        get(function, l.o, l.k, l.v, helpers);
        let args = [CallArg::Val(l.v), CallArg::Index(l.k), CallArg::Val(l.o)];
        push_call(function, l.frame, cb, this_arg, &args, helpers);
        wasm!(function; LocalSet(l.r));
        return_if_pending(function);
        emit_cond(
            &IrNode::Local(l.r),
            function,
            l.frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function;
            If(BlockType::Empty),
            LocalGet(l.list), LocalGet(l.v), Call(helpers.array_push), Drop,
            LocalGet(l.n), I32Const(1), I32Add, LocalSet(l.n),
        );
        return_if_pending(function);
        wasm!(function; End);
    });
    number(function, l.n, l.nv);
    species_create(
        function,
        l.frame,
        (l.o, &[l.nv], l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.n, |function| {
        list_element(function, l.list, l.k);
        wasm!(function;
            LocalSet(l.v),
            LocalGet(l.out), I32WrapI64, LocalGet(l.k), LocalGet(l.v), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; LocalGet(l.out));
}

/// The start index of `includes` / `indexOf` into i32 `k`, after
/// `len == 0` returned `miss`: `n = ToIntegerOrInfinity(fromIndex)`; `+∞`
/// returns `miss`; a negative `n` counts from the end (clamped at `0`).
fn search_start(function: &mut FnBuilder<'_>, l: L, miss: i64, helpers: Helpers<'_>) {
    let [n, len, ..] = l.f;
    wasm!(function;
        LocalGet(l.len), I32Eqz, If(BlockType::Empty), I64Const(miss), Return, End,
    );
    l.len_f64(function, len);
    to_integer(function, l.a[1], n, helpers);
    wasm!(function;
        LocalGet(n), F64Const(Ieee64::from(f64::INFINITY)), F64Eq,
        If(BlockType::Empty), I64Const(miss), Return, End,
        LocalGet(n), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty),
        LocalGet(len), LocalGet(n), F64Add, F64Const(Ieee64::from(0.0)), F64Max, LocalSet(n),
        End,
        LocalGet(n), I32TruncSatF64U, LocalSet(l.k),
    );
}

/// `includes(searchElement, fromIndex)`: `SameValueZero` against `Get(O,
/// k)` from the start index, so after a detaching `fromIndex` conversion
/// `undefined` is found; a non-number compares by `__same_value_zero` when
/// the program can hold a `BigInt` (by value), else by identity.
fn emit_includes(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let search = l.a[0];
    let [_, _, s, _] = l.f;
    let is_num = l.n;
    l.args(function, 2);
    l.validate(function, strings, helpers);
    search_start(function, l, FALSE_BITS, helpers);
    emit_is_number(function, search);
    wasm!(function; LocalSet(is_num), LocalGet(search), F64ReinterpretI64, LocalSet(s));
    count_loop(function, l.k, l.len, |function| {
        get(function, l.o, l.k, l.v, helpers);
        wasm!(function; LocalGet(is_num), If(BlockType::Result(ValType::I32)));
        emit_is_number(function, l.v);
        wasm!(function;
            If(BlockType::Result(ValType::I32)),
            LocalGet(l.v), F64ReinterpretI64, LocalGet(s), F64Eq,
            LocalGet(s), LocalGet(s), F64Ne,
            LocalGet(l.v), F64ReinterpretI64, LocalGet(l.v), F64ReinterpretI64, F64Ne, I32And,
            I32Or,
            Else, I32Const(0), End,
            Else,
            LocalGet(l.v), LocalGet(search),
        );
        if helpers.bigints() {
            wasm!(function; Call(helpers.same_value_zero));
        } else {
            wasm!(function; I64Eq);
        }
        wasm!(function;
            End,
            If(BlockType::Empty), I64Const(TRUE_BITS), Return, End,
        );
    });
    wasm!(function; I64Const(FALSE_BITS));
}

/// Push `1` when the element in `v` (a number, a `BigInt` or `undefined`)
/// is strictly equal to the number or `BigInt` in i64 `search`
/// ([`search_or_miss`] let only those through): numbers by `f64.eq`, a
/// `BigInt` by value (`__bigint_eq`, `0` unless both are `BigInts`).
fn strictly_equal(function: &mut FnBuilder<'_>, v: u32, search: u32, helpers: Helpers<'_>) {
    if helpers.bigints() {
        emit_is_number(function, search);
        wasm!(function; If(BlockType::Result(ValType::I32)));
    }
    emit_is_number(function, v);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), F64ReinterpretI64, LocalGet(search), F64ReinterpretI64, F64Eq,
        Else, I32Const(0), End,
    );
    if helpers.bigints() {
        wasm!(function;
            Else, LocalGet(v), LocalGet(search), Call(helpers.bigint(Big::Eq)), End,
        );
    }
}

/// Return `miss` unless the jsval in i64 `search` is a number, or a `BigInt`
/// in a program that can hold one: nothing else is strictly equal to an
/// element.
fn search_or_miss(function: &mut FnBuilder<'_>, search: u32, miss: i64, helpers: Helpers<'_>) {
    emit_is_number(function, search);
    if helpers.bigints() {
        crate::emit::bigint::emit_is_bigint(function, search);
        wasm!(function; I32Or);
    }
    wasm!(function; I32Eqz, If(BlockType::Empty), I64Const(miss), Return, End);
}

/// `indexOf(searchElement, fromIndex)`: `IsStrictlyEqual` against each
/// present element from the start index (a detached or shrunk buffer has no
/// elements there); a value that is neither a number nor a `BigInt` never
/// matches.
fn emit_index_of(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let search = l.a[0];
    let miss = (-1.0f64).to_bits().cast_signed();
    l.args(function, 2);
    l.validate(function, strings, helpers);
    search_start(function, l, miss, helpers);
    search_or_miss(function, search, miss, helpers);
    count_loop(function, l.k, l.len, |function| {
        get(function, l.o, l.k, l.v, helpers);
        strictly_equal(function, l.v, search, helpers);
        wasm!(function;
            If(BlockType::Empty), LocalGet(l.k), F64ConvertI32U, I64ReinterpretF64, Return, End,
        );
    });
    wasm!(function; I64Const(miss));
}

/// `lastIndexOf(searchElement, fromIndex)`: `n` is `len - 1` unless a
/// `fromIndex` is passed (`ToIntegerOrInfinity`; `-∞` misses); from
/// `min(n, len - 1)` (or `len + n` when negative) down to `0`.
fn emit_last_index_of(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let search = l.a[0];
    let [n, len, ..] = l.f;
    let miss = (-1.0f64).to_bits().cast_signed();
    l.args(function, 2);
    push_argc(function, l.frame);
    wasm!(function; LocalSet(l.n));
    l.validate(function, strings, helpers);
    wasm!(function; LocalGet(l.len), I32Eqz, If(BlockType::Empty), I64Const(miss), Return, End);
    l.len_f64(function, len);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(n),
        LocalGet(l.n), I32Const(1), I32GtU, If(BlockType::Empty),
    );
    to_integer(function, l.a[1], n, helpers);
    wasm!(function;
        End,
        LocalGet(n), F64Const(Ieee64::from(0.0)), F64Ge,
        If(BlockType::Result(ValType::F64)),
        LocalGet(n), LocalGet(len), F64Const(Ieee64::from(1.0)), F64Sub, F64Min,
        Else,
        LocalGet(len), LocalGet(n), F64Add,
        End,
        LocalTee(n), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), I64Const(miss), Return, End,
    );
    search_or_miss(function, search, miss, helpers);
    wasm!(function; LocalGet(n), I32TruncF64U, I32Const(1), I32Add, LocalSet(l.k));
    down_loop(function, l.k, |function| {
        get(function, l.o, l.k, l.v, helpers);
        strictly_equal(function, l.v, search, helpers);
        wasm!(function;
            If(BlockType::Empty), LocalGet(l.k), F64ConvertI32U, I64ReinterpretF64, Return, End,
        );
    });
    wasm!(function; I64Const(miss));
}

/// `join(separator)`: `ToString(separator)` (`","` when `undefined`), then
/// each `Get(O, k)` joined (`undefined` — a detached or shrunk buffer — is
/// the empty string, a number its `ToString`).
fn emit_join(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let sep_v = l.a[0];
    l.args(function, 1);
    l.validate(function, strings, helpers);
    wasm!(function;
        LocalGet(sep_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)),
        I32Const(static_key(strings, COMMA)),
        Else,
        LocalGet(sep_v), Call(helpers.to_str()),
        End,
        LocalSet(l.sep),
    );
    return_if_pending(function);
    wasm!(function;
        I32Const(static_key(strings, "")), LocalSet(l.acc), I32Const(0), LocalSet(l.k),
    );
    count_loop(function, l.k, l.len, |function| {
        wasm!(function;
            LocalGet(l.k),
            If(BlockType::Empty),
            LocalGet(l.acc), LocalGet(l.sep), Call(helpers.concat), LocalSet(l.acc),
        );
        return_if_pending(function);
        wasm!(function; End);
        get(function, l.o, l.k, l.v, helpers);
        wasm!(function;
            LocalGet(l.v), I64Const(UNDEFINED_BITS), I64Ne,
            If(BlockType::Empty),
            LocalGet(l.acc), LocalGet(l.v), Call(helpers.to_str()), Call(helpers.concat),
            LocalSet(l.acc),
        );
        return_if_pending(function);
        wasm!(function; End);
    });
    wasm!(function; LocalGet(l.acc), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `map(callback, thisArg)`: `A = TypedArraySpeciesCreate(O, [len])`, then
/// `A[k] = callback(O[k], k, O)` (`TypedArraySetElement`: `ToNumber` of the
/// result, ignored at an invalid index).
fn emit_map(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (cb, this_arg) = (l.a[0], l.a[1]);
    l.args(function, 2);
    l.validate(function, strings, helpers);
    l.require_callback(function, strings, helpers);
    number(function, l.len, l.nv);
    species_create(
        function,
        l.frame,
        (l.o, &[l.nv], l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.len, |function| {
        get(function, l.o, l.k, l.v, helpers);
        let args = [CallArg::Val(l.v), CallArg::Index(l.k), CallArg::Val(l.o)];
        push_call(function, l.frame, cb, this_arg, &args, helpers);
        wasm!(function; LocalSet(l.r));
        return_if_pending(function);
        wasm!(function;
            LocalGet(l.out), I32WrapI64, LocalGet(l.k), LocalGet(l.r), Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; LocalGet(l.out));
}

/// `reduce` / `reduceRight(callback, initialValue)`: an empty receiver
/// without an initial value throws; else the accumulator (the initial value
/// or the first / last element) is folded by `callback(acc, O[k], k, O)`
/// with `this = undefined` over the remaining indices.
fn emit_reduce(
    right: bool,
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (cb, init) = (l.a[0], l.a[1]);
    let (acc, undefined) = (l.r, l.t[0]);
    l.args(function, 2);
    push_argc(function, l.frame);
    wasm!(function; LocalSet(l.n));
    l.validate(function, strings, helpers);
    l.require_callback(function, strings, helpers);
    wasm!(function;
        LocalGet(l.len), I32Eqz, LocalGet(l.n), I32Const(2), I32LtU, I32And,
    );
    type_error_if(function, l.frame, REDUCE_EMPTY, strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(undefined));
    if right {
        wasm!(function; LocalGet(l.len), LocalSet(l.k));
    } else {
        wasm!(function; I32Const(0), LocalSet(l.k));
    }
    wasm!(function;
        LocalGet(l.n), I32Const(2), I32GeU,
        If(BlockType::Empty), LocalGet(init), LocalSet(acc), Else,
    );
    if right {
        wasm!(function; LocalGet(l.k), I32Const(1), I32Sub, LocalSet(l.k));
        get(function, l.o, l.k, acc, helpers);
    } else {
        get(function, l.o, l.k, acc, helpers);
        wasm!(function; I32Const(1), LocalSet(l.k));
    }
    wasm!(function; End);
    let body = |function: &mut FnBuilder<'_>| {
        get(function, l.o, l.k, l.v, helpers);
        let args = [
            CallArg::Val(acc),
            CallArg::Val(l.v),
            CallArg::Index(l.k),
            CallArg::Val(l.o),
        ];
        push_call(function, l.frame, cb, undefined, &args, helpers);
        wasm!(function; LocalSet(acc));
        return_if_pending(function);
    };
    if right {
        down_loop(function, l.k, body);
    } else {
        count_loop(function, l.k, l.len, body);
    }
    wasm!(function; LocalGet(acc));
}

/// `reverse()`: swaps `O[lower]` and `O[len - 1 - lower]` up to the middle
/// (no user code runs).
fn emit_reverse(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let (lower, upper) = (l.k, l.n);
    let (lv, uv) = (l.v, l.r);
    l.validate(function, strings, helpers);
    wasm!(function;
        I32Const(0), LocalSet(lower),
        LocalGet(l.len), I32Const(1), I32Sub, LocalSet(upper),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(lower), LocalGet(upper), I32GeS, BrIf(1),
    );
    get(function, l.o, lower, lv, helpers);
    get(function, l.o, upper, uv, helpers);
    set(function, l.o, lower, uv, helpers);
    set(function, l.o, upper, lv, helpers);
    wasm!(function;
        LocalGet(lower), I32Const(1), I32Add, LocalSet(lower),
        LocalGet(upper), I32Const(1), I32Sub, LocalSet(upper),
        Br(0), End, End,
        LocalGet(l.o),
    );
}

/// `set(source, offset)`: the receiver must be a typed array (it is not
/// validated up front); `ToIntegerOrInfinity(offset)` (negative: a
/// `RangeError`); then `SetTypedArrayFromTypedArray` or
/// `SetTypedArrayFromArrayLike`.
fn emit_set(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [source, ..] = l.a;
    let [off, ..] = l.f;
    l.args(function, 2);
    l.require_typed_array(function, strings, helpers);
    to_integer(function, l.a[1], off, helpers);
    wasm!(function; LocalGet(off), F64Const(Ieee64::from(0.0)), F64Lt);
    range_error_if(function, l.frame, SET_OFFSET, strings, helpers);
    is_kind(function, source, abi::TYPE_TYPEDARRAY);
    wasm!(function; If(BlockType::Empty));
    set_from_typed_array(function, l, strings, helpers);
    wasm!(function; End);
    set_from_array_like(function, l, strings, helpers);
}

/// `SetTypedArrayFromTypedArray`: both views re-validated (out of bounds:
/// `TypeError`), the source must fit at the offset (`RangeError`) and share
/// the content type (`TypeError`); the same kind copies the bytes with one
/// overlap-safe `memory.copy` (the spec's clone of a shared buffer yields
/// the same bytes); another kind converts element by element, through a
/// temporary list of the source's values when both share a buffer.
/// Returns `undefined`.
fn set_from_typed_array(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let [source, ..] = l.a;
    let [off, ..] = l.f;
    let (tlen, slen, at) = (l.len, l.n, l.i[0]);
    l.revalidate(function, strings, helpers);
    wasm!(function; LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), LocalSet(tlen));
    wasm!(function; LocalGet(source), I32WrapI64, Call(helpers.ta_oob()), If(BlockType::Empty));
    l.prop_throw(function, PROP_THROW_TA_OUT_OF_BOUNDS, strings, helpers);
    wasm!(function;
        End,
        LocalGet(source), I32WrapI64, Call(helpers.ta_length()), LocalSet(slen),
        LocalGet(slen), F64ConvertI32U, LocalGet(off), F64Add, LocalGet(tlen), F64ConvertI32U,
        F64Gt,
    );
    range_error_if(function, l.frame, SET_OFFSET, strings, helpers);
    content_types_differ(function, l.o, source);
    type_error_if(function, l.frame, SET_CONTENT_TYPE, strings, helpers);
    wasm!(function; LocalGet(off), I32TruncF64U, LocalSet(at), I32Const(0), LocalSet(l.k));
    kind(function, l.o);
    kind(function, source);
    wasm!(function; I32Eq, If(BlockType::Empty));
    element_addr(function, l.o, at);
    element_addr(function, source, l.k);
    wasm!(function; LocalGet(slen));
    shift(function, source);
    wasm!(function;
        I32Shl, MemoryCopy { src_mem: 0, dst_mem: 0 },
        I64Const(UNDEFINED_BITS), Return,
        End,
    );
    field(function, l.o, abi::TA_BUFFER_OFFSET);
    field(function, source, abi::TA_BUFFER_OFFSET);
    wasm!(function;
        I32Eq, If(BlockType::Empty),
        LocalGet(slen), Call(helpers.new_array()), LocalSet(l.list),
    );
    return_if_pending(function);
    count_loop(function, l.k, slen, |function| {
        get(function, source, l.k, l.v, helpers);
        wasm!(function; LocalGet(l.list));
        emit_array_elems(function);
        wasm!(function;
            LocalGet(l.k), I32Const(3), I32Shl, I32Add,
            LocalGet(l.v), I64Store(elem_slot(0)),
        );
    });
    wasm!(function; I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, slen, |function| {
        list_element(function, l.list, l.k);
        wasm!(function;
            LocalSet(l.v),
            LocalGet(l.o), I32WrapI64, LocalGet(at), LocalGet(l.k), I32Add, LocalGet(l.v),
            Call(helpers.ta_set()),
        );
    });
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    count_loop(function, l.k, slen, |function| {
        get(function, source, l.k, l.v, helpers);
        wasm!(function;
            LocalGet(l.o), I32WrapI64, LocalGet(at), LocalGet(l.k), I32Add, LocalGet(l.v),
            Call(helpers.ta_set()),
        );
    });
    wasm!(function; I64Const(UNDEFINED_BITS), Return);
}

/// `SetTypedArrayFromArrayLike`: the receiver re-validated, `ToObject
/// (source)`, `LengthOfArrayLike`, the fit check (`RangeError`), then `Get`
/// of each index stored by `TypedArraySetElement` (`ToNumber`, ignored at
/// an invalid index). Returns `undefined`.
fn set_from_array_like(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let [source, ..] = l.a;
    let [off, slen, ..] = l.f;
    let (src, at) = (l.t[0], l.i[0]);
    l.revalidate(function, strings, helpers);
    wasm!(function; LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), LocalSet(l.len));
    emit_is_nullish(function, source);
    type_error_if(function, l.frame, FROM_NULLISH, strings, helpers);
    wasm!(function; LocalGet(source), Call(helpers.to_object()), LocalSet(src));
    length_of_array_like(function, (src, l.v, slen), strings, helpers);
    wasm!(function;
        LocalGet(slen), LocalGet(off), F64Add, LocalGet(l.len), F64ConvertI32U, F64Gt,
    );
    range_error_if(function, l.frame, SET_OFFSET, strings, helpers);
    wasm!(function;
        LocalGet(off), I32TruncF64U, LocalSet(at),
        LocalGet(slen), I32TruncF64U, LocalSet(l.n),
        I32Const(0), LocalSet(l.k),
    );
    count_loop(function, l.k, l.n, |function| {
        get_index(function, src, l.k, helpers);
        wasm!(function; LocalSet(l.v));
        return_if_pending(function);
        wasm!(function;
            LocalGet(l.o), I32WrapI64, LocalGet(at), LocalGet(l.k), I32Add, LocalGet(l.v),
            Call(helpers.ta_set_value()),
        );
        return_if_pending(function);
    });
    wasm!(function; I64Const(UNDEFINED_BITS));
}

/// `slice(start, end)`: the relative indices, `A = TypedArraySpeciesCreate(O,
/// [count])`; when `count > 0` the receiver is re-validated and `end`
/// clamped to its current length; the same kind copies the bytes (a forward
/// byte loop, as the spec's, when `A` overlaps the source from above, else
/// `memory.copy`), another kind converts element by element.
fn emit_slice(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [start, fin, len, _] = l.f;
    let [src, dst, end, _] = l.i;
    l.args(function, 2);
    l.validate(function, strings, helpers);
    l.len_f64(function, len);
    relative(function, l.a[0], len, start, helpers);
    relative_end(function, l.a[1], len, fin, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(start), F64Sub, F64Const(Ieee64::from(0.0)), F64Max,
        I64ReinterpretF64, LocalSet(l.nv),
    );
    species_create(
        function,
        l.frame,
        (l.o, &[l.nv], l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(l.nv), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Gt,
        If(BlockType::Empty),
    );
    l.revalidate(function, strings, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, F64Min,
        LocalGet(start), F64Sub, F64Const(Ieee64::from(0.0)), F64Max, I32TruncF64U, LocalSet(l.n),
        LocalGet(start), I32TruncF64U, LocalSet(l.k),
    );
    kind(function, l.o);
    kind(function, l.out);
    wasm!(function; I32Eq, If(BlockType::Empty));
    element_addr(function, l.o, l.k);
    wasm!(function; LocalSet(src), I32Const(0), LocalSet(l.i[3]));
    element_addr(function, l.out, l.i[3]);
    wasm!(function; LocalSet(dst), LocalGet(l.n));
    shift(function, l.o);
    wasm!(function;
        I32Shl, LocalGet(src), I32Add, LocalSet(end),
        LocalGet(src), LocalGet(dst), I32LtU, LocalGet(dst), LocalGet(end), I32LtU, I32And,
        If(BlockType::Empty),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(src), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(dst), LocalGet(src), I32Load8U(m8(0)), I32Store8(m8(0)),
        LocalGet(src), I32Const(1), I32Add, LocalSet(src),
        LocalGet(dst), I32Const(1), I32Add, LocalSet(dst),
        Br(0), End, End,
        Else,
        LocalGet(dst), LocalGet(src), LocalGet(end), LocalGet(src), I32Sub,
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        End,
        Else,
        I32Const(0), LocalSet(l.i[3]),
    );
    count_loop(function, l.i[3], l.n, |function| {
        wasm!(function;
            LocalGet(l.o), I32WrapI64, LocalGet(l.k), LocalGet(l.i[3]), I32Add, Call(helpers.ta_get()),
            LocalSet(l.v),
        );
        return_if_element_pending(function, helpers);
        wasm!(function;
            LocalGet(l.out), I32WrapI64, LocalGet(l.i[3]), LocalGet(l.v), Call(helpers.ta_set()),
        );
    });
    wasm!(function; End, End, LocalGet(l.out));
}

/// `sort(comparefn)` / `toSorted(comparefn)` (`copy`): a comparator neither
/// `undefined` nor callable throws before the validation; `toSorted`
/// creates `A = TypedArrayCreateSameType(O, [len])`; the `len` elements are
/// read into a private list of `(value, value)` pairs, sorted by the stable
/// merge sort of `Array.prototype.sort` — by `ToNumber(comparefn(x, y)) <
/// 0`, or numerically with `-0` before `+0` and `NaN` last (a `BigInt`
/// kind by value, `__bigint_cmp`) — and written
/// back (to `O`, or `A`) through `__ta_set`, which ignores an index a
/// comparator detached or shrank away.
fn emit_sort(
    copy: bool,
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let frame = l.frame;
    let cmp = l.a[0];
    let (lhs, rhs, undefined) = (l.t[0], l.t[1], l.t[2]);
    let target = if copy { l.out } else { l.o };
    let merge = MergeLocals {
        src: l.list,
        dst: l.list2,
        pairs: l.len,
        width: frame.i32(0),
        lo: frame.i32(1),
        mid: frame.i32(2),
        hi: frame.i32(3),
        left: frame.y32(0),
        right: frame.y32(1),
        out: frame.y32(2),
    };
    l.args(function, 1);
    wasm!(function; LocalGet(cmp), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    emit_is_function(function, cmp, helpers);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, SORT_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    l.validate(function, strings, helpers);
    if copy {
        number(function, l.len, l.nv);
        create_same_type(
            function,
            frame,
            (l.o, l.nv, l.c, l.kind, l.out),
            strings,
            helpers,
        );
    }
    // Collect the pairs.
    for list in [merge.src, merge.dst] {
        wasm!(function;
            LocalGet(l.len), I32Const(1), I32Shl, Call(helpers.new_array()), LocalSet(list),
        );
        return_if_pending(function);
    }
    wasm!(function; I32Const(0), LocalSet(l.k), I64Const(UNDEFINED_BITS), LocalSet(undefined));
    count_loop(function, l.k, l.len, |function| {
        get(function, l.o, l.k, l.v, helpers);
        for slot in 0..2 {
            pair_at(function, merge.src, l.k);
            wasm!(function; LocalGet(l.v), I64Store(elem_slot(slot)));
        }
    });
    // Push i32 `1` when pair `right` sorts strictly before pair `left`.
    let less = |function: &mut FnBuilder<'_>| {
        pair_at(function, merge.src, merge.right);
        wasm!(function; I64Load(elem_slot(0)), LocalSet(lhs));
        pair_at(function, merge.src, merge.left);
        wasm!(function;
            I64Load(elem_slot(0)), LocalSet(rhs),
            LocalGet(cmp), I64Const(UNDEFINED_BITS), I64Eq,
            If(BlockType::Result(ValType::I32)),
        );
        default_less(function, l.o, (lhs, rhs), helpers);
        wasm!(function; Else);
        let args = [CallArg::Val(lhs), CallArg::Val(rhs)];
        push_call(function, frame, cmp, undefined, &args, helpers);
        wasm!(function; LocalSet(l.r));
        return_if_pending(function);
        wasm!(function; LocalGet(l.r), Call(helpers.to_number), LocalSet(l.r));
        return_if_pending(function);
        wasm!(function;
            LocalGet(l.r), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Lt,
            End,
        );
    };
    merge_pairs(function, merge, less);
    // Write back.
    wasm!(function; I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.len, |function| {
        pair_at(function, merge.src, l.k);
        wasm!(function; I64Load(elem_slot(1)), LocalSet(l.v));
        set(function, target, l.k, l.v, helpers);
    });
    wasm!(function; LocalGet(target));
}

/// Push i32 `1` when the element in i64 `lhs` sorts strictly before the one
/// in `rhs` in the default order of the typed array in i64 `o`: numerically
/// with `-0` before `+0` and `NaN` last, or by `BigInt` value for a `BigInt`
/// kind.
fn default_less(
    function: &mut FnBuilder<'_>,
    o: u32,
    (lhs, rhs): (u32, u32),
    helpers: Helpers<'_>,
) {
    if helpers.bigints() {
        // A `BigInt` kind: every pair holds a `BigInt` (no user code ran
        // while collecting), compared by value.
        is_bigint_kind(function, o);
        wasm!(function;
            If(BlockType::Result(ValType::I32)),
            LocalGet(lhs), I32WrapI64, LocalGet(rhs), I32WrapI64,
            Call(helpers.bigint(Big::Cmp)), I32Const(0), I32LtS,
            Else,
        );
    }
    wasm!(function;
        // x < y, or y is NaN and x is not, or x is -0 and y is +0.
        LocalGet(lhs), F64ReinterpretI64, LocalGet(rhs), F64ReinterpretI64, F64Lt,
        LocalGet(rhs), F64ReinterpretI64, LocalGet(rhs), F64ReinterpretI64, F64Ne,
        LocalGet(lhs), F64ReinterpretI64, LocalGet(lhs), F64ReinterpretI64, F64Eq, I32And,
        I32Or,
        LocalGet(lhs), I64Const(0), I64LtS,
        LocalGet(rhs), I64Const(0), I64GeS, I32And,
        LocalGet(lhs), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Eq, I32And,
        LocalGet(rhs), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Eq, I32And,
        I32Or,
    );
    if helpers.bigints() {
        wasm!(function; End);
    }
}

/// `subarray(start, end)`: the receiver must be a typed array (never
/// validated: out of bounds counts as length `0`); `beginByteOffset` from
/// the relative `start`; a length-tracking receiver with `end` `undefined`
/// gives `TypedArraySpeciesCreate(O, [buffer, beginByteOffset])` (a
/// length-tracking result), else `[buffer, beginByteOffset, newLength]`.
fn emit_subarray(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [start, fin, len, _] = l.f;
    let (buffer, begin, new_len) = (l.t[0], l.t[1], l.t[2]);
    l.args(function, 2);
    l.require_typed_array(function, strings, helpers);
    wasm!(function;
        LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, LocalSet(len),
    );
    relative(function, l.a[0], len, start, helpers);
    field(function, l.o, abi::TA_BUFFER_OFFSET);
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(buffer));
    field(function, l.o, abi::TA_BYTE_OFFSET_OFFSET);
    wasm!(function; F64ConvertI32U, LocalGet(start), I32Const(1));
    shift(function, l.o);
    wasm!(function;
        I32Shl, F64ConvertI32U, F64Mul, F64Add, I64ReinterpretF64, LocalSet(begin),
        LocalGet(l.o), I32WrapI64, I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::TA_FLAG_LENGTH_TRACKING)), I32And, I32Const(0), I32Ne,
        LocalGet(l.a[1]), I64Const(UNDEFINED_BITS), I64Eq, I32And,
        If(BlockType::Empty),
    );
    species_create(
        function,
        l.frame,
        (l.o, &[buffer, begin], l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; LocalGet(l.out), Return, End);
    relative_end(function, l.a[1], len, fin, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(start), F64Sub, F64Const(Ieee64::from(0.0)), F64Max,
        I64ReinterpretF64, LocalSet(new_len),
    );
    species_create(
        function,
        l.frame,
        (l.o, &[buffer, begin, new_len], l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; LocalGet(l.out));
}

/// `toLocaleString()`: `Array.prototype.toLocaleString`'s algorithm over
/// `len`: each `Get(O, k)` that is not `undefined` contributes
/// `ToString(Invoke(element, "toLocaleString"))`, joined with `","`.
fn emit_to_locale_string(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    l.validate(function, strings, helpers);
    wasm!(function;
        I32Const(static_key(strings, "")), LocalSet(l.acc), I32Const(0), LocalSet(l.k),
    );
    count_loop(function, l.k, l.len, |function| {
        wasm!(function;
            LocalGet(l.k),
            If(BlockType::Empty),
            LocalGet(l.acc), I32Const(static_key(strings, COMMA)), Call(helpers.concat),
            LocalSet(l.acc),
        );
        return_if_pending(function);
        wasm!(function; End);
        get(function, l.o, l.k, l.v, helpers);
        locale_element(
            function,
            l.frame,
            (l.v, l.t[0], l.acc, l.base),
            LOCALE_NOT_CALLABLE,
            strings,
            helpers,
        );
    });
    wasm!(function; LocalGet(l.acc), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `toReversed()`: `A = TypedArrayCreateSameType(O, [len])` gets element
/// `len - 1 - k` of `O` at each `k`.
fn emit_to_reversed(
    function: &mut FnBuilder<'_>,
    l: L,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let from = l.n;
    l.validate(function, strings, helpers);
    number(function, l.len, l.nv);
    create_same_type(
        function,
        l.frame,
        (l.o, l.nv, l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.len, |function| {
        wasm!(function;
            LocalGet(l.len), LocalGet(l.k), I32Sub, I32Const(1), I32Sub, LocalSet(from),
        );
        get(function, l.o, from, l.v, helpers);
        set(function, l.out, l.k, l.v, helpers);
    });
    wasm!(function; LocalGet(l.out));
}

/// `with(index, value)`: the relative index (not clamped), `ToNumber(value)`
/// (`ToBigInt` for a `BigInt` kind),
/// then an index that is not a valid integer index of the live view is a
/// `RangeError`; `A = TypedArrayCreateSameType(O, [len])` gets the number at
/// the index and `Get(O, k)` elsewhere (`undefined`, stored as `NaN`, past
/// a buffer the conversion shrank).
fn emit_with(function: &mut FnBuilder<'_>, l: L, strings: &StrOffsets, helpers: Helpers<'_>) {
    let [r, len, ..] = l.f;
    let value = l.t[0];
    l.args(function, 2);
    l.validate(function, strings, helpers);
    l.len_f64(function, len);
    to_integer(function, l.a[0], r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), LocalGet(len), LocalGet(r), F64Add, LocalSet(r), End,
    );
    to_element(function, l.o, l.a[1], value, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(r), LocalGet(l.o), I32WrapI64, Call(helpers.ta_length()), F64ConvertI32U, F64Ge,
        I32Or,
    );
    range_error_if(function, l.frame, WITH_INVALID_INDEX, strings, helpers);
    number(function, l.len, l.nv);
    create_same_type(
        function,
        l.frame,
        (l.o, l.nv, l.c, l.kind, l.out),
        strings,
        helpers,
    );
    wasm!(function; LocalGet(r), I32TruncF64U, LocalSet(l.n), I32Const(0), LocalSet(l.k));
    count_loop(function, l.k, l.len, |function| {
        wasm!(function;
            LocalGet(l.k), LocalGet(l.n), I32Eq,
            If(BlockType::Empty), LocalGet(value), LocalSet(l.v), Else,
        );
        get(function, l.o, l.k, l.v, helpers);
        wasm!(function;
            End,
            LocalGet(l.out), I32WrapI64, LocalGet(l.k), LocalGet(l.v), Call(helpers.ta_set_value()),
        );
    });
    wasm!(function; LocalGet(l.out));
}
