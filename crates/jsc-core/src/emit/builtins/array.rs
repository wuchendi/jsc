//! `%Array.prototype%` methods. Each body is generic over array-likes
//! (`ToObject(this)`, `LengthOfArrayLike`, then `HasProperty` / `Get` / `Set`
//! by index key) and runs the inline fast path of its call sites (DESIGN.md §6.4)
//! for a real array receiver.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::ArrayFindOp;

/// The `TypeError`s of one method: a `null`/`undefined` receiver, and a
/// non-callable callback (callback methods only).
struct Texts {
    nullish: CalleeErrorText<'static>,
    not_callable: CalleeErrorText<'static>,
}

/// The [`Texts`] of `Array.prototype.<name>`.
macro_rules! texts {
    ($name:literal) => {
        Texts {
            nullish: [
                concat!("Array.prototype.", $name, " called on null or undefined"),
                concat!(
                    "TypeError: Array.prototype.",
                    $name,
                    " called on null or undefined"
                ),
            ],
            not_callable: [
                concat!("Array.prototype.", $name, " callback is not a function"),
                concat!(
                    "TypeError: Array.prototype.",
                    $name,
                    " callback is not a function"
                ),
            ],
        }
    };
}

const PUSH: Texts = texts!("push");
const POP: Texts = texts!("pop");
const MAP: Texts = texts!("map");
const FILTER: Texts = texts!("filter");
const REDUCE: Texts = texts!("reduce");
const FOR_EACH: Texts = texts!("forEach");
const INDEX_OF: Texts = texts!("indexOf");
const INCLUDES: Texts = texts!("includes");
const JOIN_TEXTS: Texts = texts!("join");
const SLICE: Texts = texts!("slice");
const TO_LOCALE_STRING: Texts = Texts {
    not_callable: [
        "Array.prototype.toLocaleString: element toLocaleString is not a function",
        "TypeError: Array.prototype.toLocaleString: element toLocaleString is not a function",
    ],
    ..texts!("toLocaleString")
};

/// `2^53 - 1`, the largest array-like length.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The `TypeError` of a `push` past [`MAX_SAFE_INTEGER`].
const PUSH_TOO_LONG: CalleeErrorText<'static> = [
    "Pushing past the maximum array-like length",
    "TypeError: Pushing past the maximum array-like length",
];

/// The key `Array.prototype.toString` calls on a non-array receiver.
const JOIN: &str = "join";

/// The default `join` separator.
const COMMA: &str = ",";

/// The method `Array.prototype.toLocaleString` invokes on each element.
const TO_LOCALE_STRING_KEY: &str = "toLocaleString";

/// The [`Texts`] of the callback method `op`.
fn hof_texts(op: ArrayHofOp) -> &'static Texts {
    match op {
        ArrayHofOp::Map => &MAP,
        ArrayHofOp::Filter => &FILTER,
        ArrayHofOp::Reduce => &REDUCE,
        ArrayHofOp::ForEach => &FOR_EACH,
    }
}

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    let nullish = [&PUSH, &POP, &INDEX_OF, &INCLUDES, &JOIN_TEXTS, &SLICE]
        .into_iter()
        .flat_map(|t| t.nullish);
    let callbacks = [&MAP, &FILTER, &REDUCE, &FOR_EACH]
        .into_iter()
        .flat_map(|t| t.nullish.into_iter().chain(t.not_callable));
    nullish
        .chain(callbacks)
        .chain(REDUCE_EMPTY)
        .chain(PUSH_TOO_LONG)
        .chain(TO_LOCALE_STRING.nullish)
        .chain(TO_LOCALE_STRING.not_callable)
        .chain([JOIN, COMMA, TO_LOCALE_STRING_KEY, ""])
        .chain(more_strings())
}

/// The IR fragments the bodies emit, over the argument slots of `K = k`, for
/// the static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let o = Frame { k }.i64(0);
    let mut nodes: Vec<IrNode> = [
        ArrayHofOp::Map,
        ArrayHofOp::Filter,
        ArrayHofOp::Reduce,
        ArrayHofOp::ForEach,
    ]
    .into_iter()
    .map(|op| array_hof(op, k, o, Some(Box::new(arg(k, 1)))))
    .collect();
    nodes.extend([
        IrNode::ArrayPop { local: o },
        set(o, IrNode::Local(o), o),
        delete(o, o),
        strict_eq(o, o),
    ]);
    nodes.extend(more_fragments(k));
    nodes
}

/// Argument slot `i` as an IR node (`undefined` past the `K` slots).
fn arg(k: u32, i: u32) -> IrNode {
    if i < k {
        IrNode::Local(i)
    } else {
        IrNode::Undefined
    }
}

/// `op` over the array in local `o` with the callback in slot 0 and `extra`
/// (the initial value of `reduce`, else `thisArg`).
fn array_hof(op: ArrayHofOp, k: u32, o: u32, extra: Option<Box<IrNode>>) -> IrNode {
    let reduce = op == ArrayHofOp::Reduce;
    IrNode::ArrayHof {
        op,
        array: Box::new(IrNode::Local(o)),
        callback: Box::new(arg(k, 0)),
        init: if reduce { extra.clone() } else { None },
        this_arg: if reduce { None } else { extra },
    }
}

/// `Set(O, key, V, true)`: the member store `o[key] = v` (i64 locals).
fn set(o: u32, key: IrNode, v: u32) -> IrNode {
    IrNode::IndexSet {
        obj: Box::new(IrNode::Local(o)),
        index: Box::new(key),
        value: Box::new(IrNode::Local(v)),
    }
}

/// `DeletePropertyOrThrow(O, key)` (i64 locals).
fn delete(o: u32, key: u32) -> IrNode {
    IrNode::ObjectDelete {
        obj: Box::new(IrNode::Local(o)),
        key: Box::new(IrNode::Local(key)),
    }
}

/// `a === b` (i64 locals), a boolean jsval.
fn strict_eq(a: u32, b: u32) -> IrNode {
    IrNode::Binary {
        op: BinOp::Eq,
        lhs: Box::new(IrNode::Local(a)),
        rhs: Box::new(IrNode::Local(b)),
    }
}

/// `ToObject(this)` into i64 `o`: `undefined`/`null` throws `nullish`, a
/// primitive becomes its wrapper object.
fn this_object(
    function: &mut Function,
    frame: Frame,
    o: u32,
    nullish: CalleeErrorText<'static>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_is_nullish(function, o);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, nullish, strings, helpers);
    wasm!(function; End, LocalGet(o), Call(helpers.to_object()), LocalSet(o));
}

/// Push i32 `1` when the jsval in `o` is an array handle that is not in
/// dictionary mode (whose elements may carry any attributes, so it takes the
/// generic per-index path).
fn is_array(function: &mut Function, o: u32) {
    emit_is_pointer(function, o);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, o);
    wasm!(function;
        I32Const(TYPE_ARRAY), I32Eq,
        LocalGet(o), I32WrapI64, I32Load8U(MemArg { offset: u64::from(abi::HEADER_FLAGS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(ARRAY_DICTIONARY), I32And, I32Eqz, I32And,
        Else, I32Const(0), End,
    );
}

/// Push i32 `1` when [`is_array`] holds for the jsval in `o` and no
/// prototype of it can hold an integer index (`__protos_indexless`), so a
/// host fast path that treats a hole as absent reads what `HasProperty` /
/// `Get` would.
fn indexless_array(function: &mut Function, o: u32, helpers: Helpers<'_>) {
    is_array(function, o);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(o), I32WrapI64, Call(helpers.protos_indexless()),
        Else, I32Const(0), End,
    );
}

/// `LengthOfArrayLike(o)` into i32 `len`: `ToLength(Get(o, "length"))` (NaN
/// and negatives are 0; the u32 range bounds the rest).
fn length_of(
    function: &mut Function,
    o: u32,
    len: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(o), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get));
    return_if_pending(function);
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function; F64ReinterpretI64, I32TruncSatF64U, LocalSet(len));
}

/// Push the i32 index in local `i` as a number jsval.
fn index_value(function: &mut Function, i: u32) {
    wasm!(function; LocalGet(i), F64ConvertI32U, I64ReinterpretF64);
}

/// Push `HasProperty(o, i)` (i32) for the i32 index in local `i`, leaving
/// its interned key in i32 `key`.
fn has_index(function: &mut Function, o: u32, i: u32, key: u32, helpers: Helpers<'_>) {
    index_value(function, i);
    wasm!(function;
        I32Const(1), Call(helpers.to_key), LocalTee(key),
        LocalGet(o), Call(helpers.has),
    );
}

/// `Get(o, i)` into i64 `v` for the i32 index in local `i` (`key` an i32
/// temp); a throwing getter returns.
fn get_index(function: &mut Function, o: u32, i: u32, key: u32, v: u32, helpers: Helpers<'_>) {
    index_value(function, i);
    wasm!(function;
        I32Const(1), Call(helpers.to_key), LocalSet(key),
        LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(v),
    );
    return_if_pending(function);
}

/// `ToIntegerOrInfinity` of the jsval on the stack into f64 `r` (NaN is 0; a
/// throwing `valueOf` returns).
fn to_integer(function: &mut Function, r: u32, helpers: Helpers<'_>) {
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function;
        F64ReinterpretI64, F64Trunc, LocalTee(r), LocalGet(r), F64Ne,
        If(BlockType::Empty), F64Const(Ieee64::from(0.0)), LocalSet(r), End,
    );
}

/// `Array.prototype.push(...items)`: an array appends in place
/// (`__array_push`); any other object gets `Set(O, len + i, item)` per item
/// and then `Set(O, "length", len + n)`. Returns the new length.
pub(super) fn emit_push(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (n, i, spread) = (frame.i32(0), frame.i32(1), frame.x64(0));
    // The spread argument array, before a setter's spread call republishes it.
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(spread));
    this_object(function, frame, o, PUSH.nullish, strings, helpers);
    push_argc(function, frame);
    wasm!(function; LocalSet(n), I32Const(0), LocalSet(i));
    // An array grows in place unless it would pass `2^32 - 1` elements (the
    // generic path then sets the non-index key and throws on `length`) or a
    // prototype may hold an index (each `Set` then consults the chain).
    indexless_array(function, o, helpers);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    wasm!(function;
        LocalGet(o), I32WrapI64, I32Load(array_len_slot()), I64ExtendI32U,
        LocalGet(n), I64ExtendI32U, I64Add, I64Const(0xffff_ffff), I64LeU,
        Else, I32Const(0), End,
        If(BlockType::Empty),
    );
    count_loop(function, i, n, |function| {
        wasm!(function; LocalGet(o), I32WrapI64);
        arg_at(function, frame, i, spread, v);
        wasm!(function; LocalGet(v), Call(helpers.array_push), Drop);
    });
    wasm!(function; LocalGet(o), I32WrapI64, I32Load(array_len_slot()));
    wasm!(function; F64ConvertI32U, I64ReinterpretF64, Return, End);
    // ToLength over the full integer range: `len + n` past 2^53 - 1 throws.
    let flen = frame.f64(0);
    wasm!(function; LocalGet(o), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get));
    return_if_pending(function);
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function;
        F64ReinterpretI64, F64Trunc, F64Const(Ieee64::from(0.0)), F64Max,
        F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Min, LocalSet(flen),
        // NaN propagates through `max`/`min`; ToLength reads it as 0.
        LocalGet(flen), LocalGet(flen), F64Ne,
        If(BlockType::Empty), F64Const(Ieee64::from(0.0)), LocalSet(flen), End,
        LocalGet(flen), LocalGet(n), F64ConvertI32U, F64Add,
        F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Gt,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, PUSH_TOO_LONG, strings, helpers);
    wasm!(function; End);
    count_loop(function, i, n, |function| {
        wasm!(function; LocalGet(flen), LocalGet(i), F64ConvertI32U, F64Add);
        wasm!(function; I64ReinterpretF64, LocalSet(kv));
        arg_at(function, frame, i, spread, v);
        emit_drop(
            function,
            frame,
            &set(o, IrNode::Local(kv), v),
            strings,
            helpers,
        );
    });
    wasm!(function;
        LocalGet(flen), LocalGet(n), F64ConvertI32U, F64Add,
        I64ReinterpretF64, LocalSet(kv),
    );
    let length = IrNode::Str(INDEX_LENGTH_KEY.to_owned());
    emit_drop(function, frame, &set(o, length, kv), strings, helpers);
    wasm!(function; LocalGet(kv));
}

/// `Array.prototype.pop()`: an array shrinks in place; any other object
/// reads `O[len - 1]`, deletes it and sets `length` (`0` for an empty one,
/// which pops `undefined`).
pub(super) fn emit_pop(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let len = frame.yf64(0);
    let length = || IrNode::Str(INDEX_LENGTH_KEY.to_owned());
    this_object(function, frame, o, POP.nullish, strings, helpers);
    is_array(function, o);
    wasm!(function; If(BlockType::Empty));
    emit_node(
        &IrNode::ArrayPop { local: o },
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; Return, End);
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(0.0)), F64Eq,
        If(BlockType::Empty),
        I64Const(0.0f64.to_bits().cast_signed()), LocalSet(kv),
    );
    emit_drop(function, frame, &set(o, length(), kv), strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(1.0)), F64Sub, I64ReinterpretF64, LocalSet(kv),
    );
    let read = IrNode::Index {
        obj: Box::new(IrNode::Local(o)),
        index: Box::new(IrNode::Local(kv)),
    };
    emit_node(&read, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(v));
    emit_drop(function, frame, &delete(o, kv), strings, helpers);
    emit_drop(function, frame, &set(o, length(), kv), strings, helpers);
    wasm!(function; LocalGet(v));
}

/// `Array.prototype.map/filter/reduce/forEach(callback, thisArg | initial)`:
/// `len = LengthOfArrayLike(O)`, the `IsCallable` check, `map` / `filter`
/// then `A = ArraySpeciesCreate(O, len | 0)`; then for each index `k < len`
/// present on `O` (`HasProperty`), the callback over `(O[k], k, O)` with
/// `this = thisArg` (`reduce`: over `(acc, O[k], k, O)` with
/// `this = undefined`, the accumulator seeded from the initial value or the
/// first present element), `map` / `filter` storing into `A` by
/// `CreateDataPropertyOrThrow`. An array receiver runs the call sites'
/// [`IrNode::ArrayHof`] loop instead (for `map` / `filter` only when the
/// species is `ArrayCreate`).
#[allow(clippy::too_many_lines)]
pub(super) fn emit_hof(
    op: ArrayHofOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let texts = hof_texts(op);
    let (o, callback, this_arg, value) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (acc, result) = (frame.x64(0), frame.x64(1));
    let (c, kv, target) = (frame.y64(0), frame.y64(1), frame.y64(4));
    let (len, k, to, seeded) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let key = frame.x32(0);
    let flen = frame.yf64(0);
    let reduce = op == ArrayHofOp::Reduce;
    let species = matches!(op, ArrayHofOp::Map | ArrayHofOp::Filter);
    this_object(function, frame, o, texts.nullish, strings, helpers);
    // `reduce` has an initial value when called with two arguments or more.
    let has_init = |function: &mut Function| {
        wasm!(function;
            LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Ge,
        );
    };
    let extra = || Some(Box::new(arg(frame.k, 1)));
    let fast = |function: &mut Function| {
        if reduce {
            has_init(function);
            wasm!(function; If(BlockType::Result(ValType::I64)));
            let node = array_hof(op, frame.k, o, extra());
            emit_node(&node, function, frame.scratch(), strings, helpers);
            wasm!(function; Else);
            let node = array_hof(op, frame.k, o, None);
            emit_node(&node, function, frame.scratch(), strings, helpers);
            wasm!(function; End);
        } else {
            let node = array_hof(op, frame.k, o, extra());
            emit_node(&node, function, frame.scratch(), strings, helpers);
        }
        wasm!(function; Return, End);
    };
    if !species {
        is_array(function, o);
        wasm!(function; If(BlockType::Empty));
        fast(function);
    }
    // The full `ToLength` value sizes `map`'s `ArraySpeciesCreate` (past
    // `2^32 - 1` a `RangeError`); the loop runs over its u32 clamp.
    length_of_f64(function, o, flen, strings, helpers);
    wasm!(function; LocalGet(flen), I32TruncSatF64U, LocalSet(len));
    frame.arg(function, 0);
    wasm!(function; LocalSet(callback));
    require_callable(
        function,
        frame,
        callback,
        texts.not_callable,
        strings,
        helpers,
    );
    if species {
        species_ctor(function, frame, o, c, strings, helpers);
        is_array(function, o);
        wasm!(function;
            LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, I32And,
            If(BlockType::Empty),
        );
        fast(function);
        if op == ArrayHofOp::Map {
            num(function, flen);
        } else {
            wasm!(function; I64Const(0.0f64.to_bits().cast_signed()));
        }
        wasm!(function; LocalSet(kv));
        species_from(function, frame, c, kv, target, strings, helpers);
    }
    if reduce {
        wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(this_arg));
        has_init(function);
        wasm!(function; LocalTee(seeded), If(BlockType::Empty));
        frame.arg(function, 1);
        wasm!(function; LocalSet(acc), End);
    } else {
        frame.arg(function, 1);
        wasm!(function; LocalSet(this_arg));
    }
    // The callback over `(acc,) value, k, O` with `this = thisArg`, its
    // result in `result`.
    let call = |function: &mut Function| {
        let args: &[CallArg] = if reduce {
            &[
                CallArg::Val(acc),
                CallArg::Val(value),
                CallArg::Index(k),
                CallArg::Val(o),
            ]
        } else {
            &[CallArg::Val(value), CallArg::Index(k), CallArg::Val(o)]
        };
        call_fn(function, frame, callback, this_arg, args, result, helpers);
    };
    wasm!(function; I32Const(0), LocalSet(k), I32Const(0), LocalSet(to));
    count_loop(function, k, len, |function| {
        has_index(function, o, k, key, helpers);
        wasm!(function; If(BlockType::Empty));
        wasm!(function; LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(value));
        return_if_pending(function);
        match op {
            ArrayHofOp::Map => {
                call(function);
                index_value(function, k);
                wasm!(function; LocalSet(kv));
                create_data_property(function, frame, target, kv, result, strings, helpers);
            }
            ArrayHofOp::Filter => {
                call(function);
                emit_cond(
                    &IrNode::Local(result),
                    function,
                    frame.scratch(),
                    strings,
                    helpers,
                );
                wasm!(function; If(BlockType::Empty));
                index_value(function, to);
                wasm!(function; LocalSet(kv));
                create_data_property(function, frame, target, kv, value, strings, helpers);
                wasm!(function; LocalGet(to), I32Const(1), I32Add, LocalSet(to), End);
            }
            ArrayHofOp::Reduce => {
                wasm!(function; LocalGet(seeded), If(BlockType::Empty));
                call(function);
                wasm!(function;
                    LocalGet(result), LocalSet(acc),
                    Else,
                    LocalGet(value), LocalSet(acc), I32Const(1), LocalSet(seeded),
                    End,
                );
            }
            ArrayHofOp::ForEach => call(function),
        }
        wasm!(function; End);
    });
    match op {
        ArrayHofOp::Map | ArrayHofOp::Filter => wasm!(function; LocalGet(target)),
        ArrayHofOp::Reduce => {
            wasm!(function; LocalGet(seeded), I32Eqz, If(BlockType::Empty));
            throw_type_error(function, frame, REDUCE_EMPTY, strings, helpers);
            wasm!(function; End, LocalGet(acc));
        }
        ArrayHofOp::ForEach => wasm!(function; I64Const(UNDEFINED_BITS)),
    }
}

/// `Array.prototype.indexOf(search, fromIndex)` (`includes`: `includes`).
/// An array without a `fromIndex` (and no prototype holding an index) runs
/// the call sites' host search. Any
/// other receiver scans `k` from `fromIndex` (relative to `len`): `indexOf`
/// over the present indices with `===`, `includes` over every index with
/// `SameValueZero` (an absent index reads `undefined`).
pub(super) fn emit_search(
    includes: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let texts = if includes { &INCLUDES } else { &INDEX_OF };
    let (o, search, v) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let key = frame.i32(2);
    let r = frame.f64(0);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let (op, not_found) = if includes {
        (abi::METHOD_OP_INCLUDES, FALSE_BITS)
    } else {
        (abi::METHOD_OP_INDEX_OF, (-1.0f64).to_bits().cast_signed())
    };
    this_object(function, frame, o, texts.nullish, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(search));
    indexless_array(function, o, helpers);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    wasm!(function; LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Lt);
    frame.arg(function, 1);
    wasm!(function;
        I64Const(UNDEFINED_BITS), I64Eq, I32Or,
        Else, I32Const(0), End,
        If(BlockType::Empty),
        LocalGet(o), I32Const(op), LocalGet(search), I64Const(UNDEFINED_BITS),
        Call(helpers.method), Return,
        End,
    );
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(0.0)), F64Eq,
        If(BlockType::Empty), I64Const(not_found), Return, End,
    );
    frame.arg(function, 1);
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), LocalGet(len), F64Ge,
        If(BlockType::Empty), I64Const(not_found), Return, End,
    );
    relative_f64(function, r, len);
    wasm!(function; LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
        |function| {
            if includes {
                get_at(function, o, k, key, v, helpers);
                wasm!(function;
                    LocalGet(v), LocalGet(search), Call(helpers.same_value_zero),
                    If(BlockType::Empty), I64Const(TRUE_BITS), Return, End,
                );
            } else {
                has_at(function, o, k, key, helpers);
                wasm!(function; If(BlockType::Empty));
                get_key(function, o, key, v, helpers);
                emit_node(
                    &strict_eq(v, search),
                    function,
                    frame.scratch(),
                    strings,
                    helpers,
                );
                wasm!(function; I64Const(TRUE_BITS), I64Eq, If(BlockType::Empty));
                num(function, k);
                wasm!(function; Return, End, End);
            }
            bump(function, k, 1.0);
        },
    );
    wasm!(function; I64Const(not_found));
}

/// `Array.prototype.join(separator)`: an array runs `__array_join` (as its
/// call sites do); any other receiver joins `ToString(O[k])` for `k < len`
/// (`undefined`/`null` and absent indices are empty) with `ToString(separator)`
/// (`","` when `undefined`).
pub(super) fn emit_join(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, sep_v, v) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (len, k, acc, sep) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let key = frame.x32(0);
    this_object(function, frame, o, JOIN_TEXTS.nullish, strings, helpers);
    is_array(function, o);
    wasm!(function; If(BlockType::Empty), LocalGet(o));
    frame.arg(function, 0);
    wasm!(function; Call(helpers.array_join()));
    return_if_pending(function);
    wasm!(function; Return, End);
    length_of(function, o, len, strings, helpers);
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(sep_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)),
        I32Const(static_key(strings, COMMA)),
        Else,
        LocalGet(sep_v), Call(helpers.to_str()),
        End,
        LocalSet(sep),
    );
    return_if_pending(function);
    wasm!(function; I32Const(static_key(strings, "")), LocalSet(acc), I32Const(0), LocalSet(k));
    count_loop(function, k, len, |function| {
        wasm!(function;
            LocalGet(k),
            If(BlockType::Empty), LocalGet(acc), LocalGet(sep), Call(helpers.concat), LocalSet(acc), End,
        );
        get_index(function, o, k, key, v, helpers);
        emit_is_nullish(function, v);
        wasm!(function;
            I32Eqz,
            If(BlockType::Empty),
            LocalGet(acc), LocalGet(v), Call(helpers.to_str()), Call(helpers.concat), LocalSet(acc),
        );
        return_if_pending(function);
        wasm!(function; End);
    });
    wasm!(function; LocalGet(acc), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `Array.prototype.toLocaleString()`: joins with `","` the result of
/// `ToString(Invoke(O[k], "toLocaleString"))` for `k < len`
/// (`undefined`/`null` and absent indices are empty); a non-callable
/// `toLocaleString` throws.
pub(super) fn emit_to_locale_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, f) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let (len, k, acc, base) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let key = frame.x32(0);
    this_object(
        function,
        frame,
        o,
        TO_LOCALE_STRING.nullish,
        strings,
        helpers,
    );
    length_of(function, o, len, strings, helpers);
    wasm!(function; I32Const(static_key(strings, "")), LocalSet(acc), I32Const(0), LocalSet(k));
    count_loop(function, k, len, |function| {
        wasm!(function;
            LocalGet(k),
            If(BlockType::Empty),
            LocalGet(acc), I32Const(static_key(strings, COMMA)), Call(helpers.concat), LocalSet(acc),
            End,
        );
        get_index(function, o, k, key, v, helpers);
        emit_is_nullish(function, v);
        wasm!(function;
            I32Eqz,
            If(BlockType::Empty),
            LocalGet(v), I32Const(static_key(strings, TO_LOCALE_STRING_KEY)), Call(helpers.get),
            LocalSet(f),
        );
        return_if_pending(function);
        emit_is_function(function, f);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        throw_type_error(
            function,
            frame,
            TO_LOCALE_STRING.not_callable,
            strings,
            helpers,
        );
        wasm!(function; End);
        emit_accessor_call(function, f, v, None, base, helpers);
        wasm!(function; LocalSet(v));
        return_if_pending(function);
        wasm!(function; LocalGet(acc), LocalGet(v), Call(helpers.to_str()), Call(helpers.concat), LocalSet(acc));
        return_if_pending(function);
        wasm!(function; End);
    });
    wasm!(function; LocalGet(acc), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// `Array.prototype.slice(start, end)`: `k` and `final` from the relative
/// `start` and `end` (`len` when `undefined`), `A = ArraySpeciesCreate(O,
/// count)`; an array whose species is `ArrayCreate` then runs the call sites'
/// host slice over `[k, final)`; otherwise each present index of `[k, final)`
/// is copied into `A` by `CreateDataPropertyOrThrow` (an absent one stays a
/// hole) and `A.length` is set.
pub(super) fn emit_slice(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, value, kv, end_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (c, count, target) = (frame.y64(0), frame.y64(1), frame.y64(4));
    let key = frame.x32(1);
    let rel = frame.f64(0);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let (fin, to) = (frame.zf64(0), frame.zf64(1));
    frame.arg(function, 0);
    wasm!(function; LocalSet(value));
    frame.arg(function, 1);
    wasm!(function; LocalSet(end_v));
    this_object(function, frame, o, SLICE.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    relative_arg(function, value, false, rel, len, k, helpers);
    relative_arg(function, end_v, true, rel, len, fin, helpers);
    species_ctor(function, frame, o, c, strings, helpers);
    indexless_array(function, o, helpers);
    wasm!(function;
        LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, I32And,
        If(BlockType::Empty),
        LocalGet(o), I32Const(abi::METHOD_OP_SLICE),
    );
    num(function, k);
    num(function, fin);
    wasm!(function;
        Call(helpers.method), Return,
        End,
        // count = max(final - k, 0)
        LocalGet(fin), LocalGet(k), F64Sub, F64Const(Ieee64::from(0.0)), F64Max,
        I64ReinterpretF64, LocalSet(count),
    );
    species_from(function, frame, c, count, target, strings, helpers);
    return_if_pending(function);
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(to));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(fin), F64Lt),
        |function| {
            has_at(function, o, k, key, helpers);
            wasm!(function; If(BlockType::Empty));
            get_key(function, o, key, value, helpers);
            num(function, to);
            wasm!(function; LocalSet(kv));
            create_data_property(function, frame, target, kv, value, strings, helpers);
            wasm!(function; End);
            bump(function, k, 1.0);
            bump(function, to, 1.0);
        },
    );
    num(function, to);
    wasm!(function; LocalSet(kv));
    set_length(function, frame, target, kv, strings, helpers);
    wasm!(function; LocalGet(target));
}

/// `Array.prototype.toString()`: an array joins with `","`; any other
/// receiver calls its own `join` when callable, else
/// `%Object.prototype.toString%`.
pub(super) fn emit_to_string(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (this, join, base) = (frame.i64(0), frame.i64(1), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_array(function, this);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(this), I64Const(UNDEFINED_BITS), Call(helpers.array_join()), Return,
        End,
    );
    object::require_object_coercible(function, frame, this, strings, helpers);
    wasm!(function; LocalGet(this), I32Const(static_key(strings, JOIN)), Call(helpers.get), LocalSet(join));
    return_if_pending(function);
    emit_is_function(function, join);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    emit_accessor_call(function, join, this, None, base, helpers);
    wasm!(function; Else);
    for _ in 0..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function;
        LocalGet(this), GlobalSet(THIS_GLOBAL),
        I64Const(UNDEFINED_BITS), I64Const(0),
        Call(helpers.builtin(Builtin::ObjectToString)),
        End,
    );
}

// --- shared steps of the statics and the non-mutating methods ----------------------

const AT: Texts = texts!("at");
const REDUCE_RIGHT: Texts = texts!("reduceRight");
const LAST_INDEX_OF: Texts = texts!("lastIndexOf");
const CONCAT: Texts = texts!("concat");
const FLAT: Texts = texts!("flat");
const FLAT_MAP: Texts = texts!("flatMap");
const FIND: [Texts; 6] = [
    texts!("find"),
    texts!("findIndex"),
    texts!("findLast"),
    texts!("findLastIndex"),
    texts!("some"),
    texts!("every"),
];

/// The `TypeError` of `Array.from` over `undefined` / `null`.
const FROM_NULLISH: CalleeErrorText<'static> = [
    "Array.from requires an array-like or iterable object",
    "TypeError: Array.from requires an array-like or iterable object",
];

/// The `TypeError` of `Array.from` with a non-callable `mapFn`.
const FROM_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Array.from: when provided, the second argument must be a function",
    "TypeError: Array.from: when provided, the second argument must be a function",
];

/// The `TypeError` of `ArraySpeciesCreate` over a non-object `constructor`.
const CTOR_NOT_OBJECT: CalleeErrorText<'static> = [
    "object.constructor is not an object",
    "TypeError: object.constructor is not an object",
];

/// The not-a-constructor message of `ArraySpeciesCreate`.
const SPECIES_NOT_CTOR: &str = "object.constructor[Symbol.species]";

/// The not-a-constructor message of `Array.of` / `Array.from` (never thrown:
/// they construct only after `IsConstructor`).
const THIS_NOT_CTOR: &str = "this";

/// The `TypeError` of `CreateDataPropertyOrThrow` on a property it cannot
/// define.
const CANNOT_DEFINE: CalleeErrorText<'static> = [
    "Cannot add or redefine an element of the result object",
    "TypeError: Cannot add or redefine an element of the result object",
];

/// The key `ArraySpeciesCreate` reads.
const CONSTRUCTOR: &str = "constructor";

/// The [`Texts`] of the predicate method `op`.
fn find_texts(op: ArrayFindOp) -> &'static Texts {
    let i = match op {
        ArrayFindOp::Find => 0,
        ArrayFindOp::FindIndex => 1,
        ArrayFindOp::FindLast => 2,
        ArrayFindOp::FindLastIndex => 3,
        ArrayFindOp::Some => 4,
        ArrayFindOp::Every => 5,
    };
    &FIND[i]
}

/// The static strings of the statics and the non-mutating methods.
fn more_strings() -> impl Iterator<Item = &'static str> {
    [&AT, &LAST_INDEX_OF, &CONCAT, &FLAT]
        .into_iter()
        .flat_map(|t| t.nullish)
        .chain(
            FIND.iter()
                .chain([&REDUCE_RIGHT, &FLAT_MAP])
                .flat_map(|t| t.nullish.into_iter().chain(t.not_callable)),
        )
        .chain(FROM_NULLISH)
        .chain(FROM_NOT_CALLABLE)
        .chain(CTOR_NOT_OBJECT)
        .chain(CANNOT_DEFINE)
        .chain([CONSTRUCTOR, "done", "value"])
        .chain(mutating_strings())
}

/// The IR fragments of the statics and the non-mutating methods, over the
/// locals of `K = k`, for the static-string scan.
fn more_fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    let o = frame.i64(0);
    vec![
        construct(frame, o, Some(o), SPECIES_NOT_CTOR),
        construct(frame, o, None, THIS_NOT_CTOR),
    ]
}

/// `Construct(c, args)` (`args` = `[len]` or none) as an IR node; `what`
/// names `c` in its not-a-constructor `TypeError`.
fn construct(frame: Frame, c: u32, len: Option<u32>, what: &str) -> IrNode {
    IrNode::Construct {
        callee: Box::new(IrNode::Local(c)),
        args: Vec::new(),
        spread: len.map(|len| Box::new(IrNode::Array(vec![IrNode::Local(len)]))),
        callee_local: frame.y64(2),
        this_local: frame.y64(3),
        reserve: 0,
        not_constructor: CalleeError::not_a_constructor(what),
        new_target: None,
    }
}

/// One argument of a [`call_fn`] call.
#[derive(Clone, Copy)]
enum CallArg {
    /// The jsval in an i64 local.
    Val(u32),
    /// The i32 index in a local, as a number.
    Index(u32),
}

/// Call the function in i64 `callee` with `this` = i64 `this` over `args`,
/// leaving its result (or a pending throw): the first `K` in the positional
/// slots and, when there are more, all of them in a fresh spread argument
/// array published for the callee (built in the [`Frame::arg`] temp).
fn push_call(
    function: &mut Function,
    frame: Frame,
    callee: u32,
    this: u32,
    args: &[CallArg],
    helpers: Helpers<'_>,
) {
    let push = |function: &mut Function, arg: Option<&CallArg>| match arg {
        Some(CallArg::Val(local)) => wasm!(function; LocalGet(*local)),
        Some(CallArg::Index(local)) => index_value(function, *local),
        None => wasm!(function; I64Const(UNDEFINED_BITS)),
    };
    for slot in 0..frame.k {
        push(function, args.get(slot as usize));
    }
    if args.len() > frame.k as usize {
        let spread = frame.scratch() + 35;
        let n = i32::try_from(args.len()).unwrap_or(i32::MAX);
        wasm!(function; I32Const(n), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(spread));
        for (j, arg) in (0u64..).zip(args) {
            wasm!(function; LocalGet(spread), I32WrapI64);
            emit_array_elems(function);
            push(function, Some(arg));
            wasm!(function; I64Store(elem_slot(j)));
        }
        wasm!(function; LocalGet(spread), GlobalSet(SPREAD_ARGS_GLOBAL));
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    let count = args.len();
    invoke(
        function,
        callee,
        |function| emit_argc(count, function),
        helpers,
    );
}

/// [`push_call`] with the result into i64 `result`; a throw returns.
fn call_fn(
    function: &mut Function,
    frame: Frame,
    callee: u32,
    this: u32,
    args: &[CallArg],
    result: u32,
    helpers: Helpers<'_>,
) {
    push_call(function, frame, callee, this, args, helpers);
    wasm!(function; LocalSet(result));
    return_if_pending(function);
}

/// Throw `text` unless the jsval in `local` is callable.
fn require_callable(
    function: &mut Function,
    frame: Frame,
    local: u32,
    text: CalleeErrorText<'static>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_is_function(function, local);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// `ArrayCreate(len)` into i64 `out` for the number jsval in i64 `len`: a
/// fresh array whose `length` write leaves `len` holes.
fn array_create(
    function: &mut Function,
    frame: Frame,
    len: u32,
    out: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        I32Const(0), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        LocalSet(out),
    );
    let length = IrNode::Str(INDEX_LENGTH_KEY.to_owned());
    emit_drop(function, frame, &set(out, length, len), strings, helpers);
}

/// `IsConstructor(c) ? Construct(c, args) : ArrayCreate(len)` into i64
/// `out` (`args` = `[len]` for `Some(len)`, else none and `len` 0), the shared
/// step of `Array.of` / `Array.from`; `%Array%` itself is `ArrayCreate`.
fn construct_or_create(
    function: &mut Function,
    frame: Frame,
    c: u32,
    len: Option<u32>,
    out: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let array = intrinsic_bits(Intrinsic::Ctor(BuiltinCtor::Array), helpers);
    wasm!(function; LocalGet(c), I64Const(array), I64Ne);
    emit_is_constructor(function, c);
    wasm!(function; I32Const(0), I32Ne, I32And, If(BlockType::Empty));
    let node = construct(frame, c, len, THIS_NOT_CTOR);
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(out), Else);
    match len {
        Some(len) => array_create(function, frame, len, out, strings, helpers),
        None => {
            wasm!(function;
                I32Const(0), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
                LocalSet(out),
            );
        }
    }
    wasm!(function; End);
}

/// The constructor `ArraySpeciesCreate(o, ..)` uses, into i64 `c`
/// (`undefined` for `ArrayCreate`): a non-array `o` creates an array; else
/// `C = Get(o, "constructor")`: `undefined` creates an array, a non-object
/// throws, and an object is its own species when its prototype chain reaches
/// a builtin constructor with a `@@species` getter returning `this`
/// (`%Array%`, `%Map%`, `%Set%`, `%Promise%`), else (`C[@@species]` is
/// `undefined`) an array is created; `%Array%` itself creates an array. No
/// builtin defines `@@species` yet, so a `C` whose chain holds a `@@species`
/// (a user hook, `__symbol_get`) throws the interim `TypeError` (Y4 honours
/// it, design/symbol-strings.md §9.4).
fn species_ctor(
    function: &mut Function,
    frame: Frame,
    o: u32,
    c: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (walk, found) = (frame.y64(5), frame.y32(0));
    let bits = |ctor| intrinsic_bits(Intrinsic::Ctor(ctor), helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(c));
    is_array(function, o);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(o), I32Const(static_key(strings, CONSTRUCTOR)), Call(helpers.get), LocalSet(c),
    );
    return_if_pending(function);
    wasm!(function; LocalGet(c), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    emit_is_object(function, c);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, CTOR_NOT_OBJECT, strings, helpers);
    wasm!(function; End);
    emit_hook_guard(
        function,
        c,
        WellKnown::Species,
        frame.scratch(),
        strings,
        helpers,
    );
    return_if_pending(function);
    wasm!(function;
        LocalGet(c), LocalSet(walk), I32Const(0), LocalSet(found),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    emit_is_pointer(function, walk);
    wasm!(function; I32Eqz, BrIf(1));
    for (i, ctor) in [
        BuiltinCtor::Array,
        BuiltinCtor::Map,
        BuiltinCtor::Set,
        BuiltinCtor::Promise,
    ]
    .into_iter()
    .enumerate()
    {
        wasm!(function; LocalGet(walk), I64Const(bits(ctor)), I64Eq);
        if i > 0 {
            wasm!(function; I32Or);
        }
    }
    wasm!(function;
        If(BlockType::Empty), I32Const(1), LocalSet(found), Br(2), End,
        LocalGet(walk), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::OBJ_PROTO_OFFSET), align: 3, memory_index: 0 }),
        LocalSet(walk),
        Br(0),
        End, End,
        LocalGet(found), I32Eqz,
        LocalGet(c), I64Const(bits(BuiltinCtor::Array)), I64Eq, I32Or,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), LocalSet(c), End,
        End,
        End,
    );
}

/// The array of `ArraySpeciesCreate` from the [`species_ctor`] in i64 `c`
/// and the number jsval in i64 `len`, into i64 `out`.
fn species_from(
    function: &mut Function,
    frame: Frame,
    c: u32,
    len: u32,
    out: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(c), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
    array_create(function, frame, len, out, strings, helpers);
    wasm!(function; Else);
    let node = construct(frame, c, Some(len), SPECIES_NOT_CTOR);
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(out), End);
}

/// `ArraySpeciesCreate(o, len)` into i64 `out` (`len` a number jsval in an
/// i64 local).
fn species_create(
    function: &mut Function,
    frame: Frame,
    o: u32,
    len: u32,
    out: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let c = frame.y64(0);
    species_ctor(function, frame, o, c, strings, helpers);
    species_from(function, frame, c, len, out, strings, helpers);
}

/// `CreateDataPropertyOrThrow(a, key, v)` over i64 locals (`key` a number
/// jsval): an array stores the element; any other object defines an own
/// enumerable, writable, configurable data property, throwing over a
/// non-configurable own property or (absent) a non-extensible object.
fn create_data_property(
    function: &mut Function,
    frame: Frame,
    a: u32,
    key: u32,
    v: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (k, entry) = (frame.y32(2), frame.y32(3));
    is_array(function, a);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(a), LocalGet(key), LocalGet(v), Call(helpers.array_define()),
    );
    enter_frame(helpers);
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    wasm!(function;
        Else,
        LocalGet(key), I32Const(1), Call(helpers.to_key), LocalSet(k),
        LocalGet(a), I32WrapI64, LocalGet(k), Call(helpers.prop_find), LocalTee(entry),
        If(BlockType::Result(ValType::I32)),
        LocalGet(entry),
        I32Load(MemArg { offset: u64::from(abi::PROP_ATTRS_OFFSET), align: 2, memory_index: 0 }),
        I32Const(abi::ATTR_CONFIGURABLE.cast_signed()), I32And, I32Eqz,
        Else,
        LocalGet(a), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(abi::HEADER_FLAGS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(i32::from(abi::OBJ_FLAG_NOT_EXTENSIBLE)), I32And,
        End,
        If(BlockType::Empty),
    );
    // Inside the `else` arm and this `if` (a handler may be active).
    enter_frame(helpers);
    enter_frame(helpers);
    throw_type_error(function, frame, CANNOT_DEFINE, strings, helpers);
    leave_frame(helpers);
    leave_frame(helpers);
    wasm!(function;
        End,
        LocalGet(a), I32WrapI64, LocalGet(k), LocalGet(v),
        I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
        End,
    );
}

/// `Set(a, "length", len, true)` over i64 locals.
fn set_length(
    function: &mut Function,
    frame: Frame,
    a: u32,
    len: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let length = IrNode::Str(INDEX_LENGTH_KEY.to_owned());
    emit_drop(function, frame, &set(a, length, len), strings, helpers);
}

/// `while k > 0 { k -= 1; body }` over the f64 local `k`.
fn down_loop_f64(function: &mut Function, k: u32, body: impl FnOnce(&mut Function)) {
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), F64Const(Ieee64::from(0.0)), F64Gt),
        |function| {
            bump(function, k, -1.0);
            body(function);
        },
    );
}

// --- Array.of / Array.from ------------------------------------------------------------

/// `Array.of(...items)`: `A = IsConstructor(this) ? Construct(this, [n]) :
/// ArrayCreate(n)`, then `CreateDataPropertyOrThrow(A, k, items[k])` and
/// `Set(A, "length", n)`.
pub(super) fn emit_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (c, target, item, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (saved, len) = (frame.x64(0), frame.y64(1));
    let (n, i) = (frame.i32(0), frame.i32(1));
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    push_argc(function, frame);
    wasm!(function; LocalSet(n));
    index_value(function, n);
    wasm!(function; LocalSet(len), GlobalGet(THIS_GLOBAL), LocalSet(c));
    construct_or_create(function, frame, c, Some(len), target, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(i));
    count_loop(function, i, n, |function| {
        arg_at(function, frame, i, saved, item);
        index_value(function, i);
        wasm!(function; LocalSet(kv));
        create_data_property(function, frame, target, kv, item, strings, helpers);
    });
    set_length(function, frame, target, len, strings, helpers);
    wasm!(function; LocalGet(target));
}

/// `Array.from(items, mapFn, thisArg)`: a non-callable `mapFn` throws first.
/// `GetMethod(items, @@iterator)` (through the guarded fast paths of
/// design/symbol-strings.md §5.3) decides: an iterable fills `A =
/// IsConstructor(this) ? Construct(this) : ArrayCreate(0)` by
/// `CreateDataPropertyOrThrow` from its iterator record, closing the
/// iterator when `mapFn` or the define throws; any other value is an
/// array-like (`ToObject`, `LengthOfArrayLike`, `A` from `Construct(this,
/// [len])`, `Get` per index). `A.length` is set last.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_from(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (c, items, map_fn, this_arg) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (a, v, kv, len_v) = (frame.x64(0), frame.x64(1), frame.y64(0), frame.y64(1));
    let (k, len, mapping, key) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let method = frame.y64(4);
    let rec = iter::local_record(frame.z64(0), frame.z64(1), frame.z64(2), frame.z64(3));
    let scratch = frame.scratch();
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(c));
    frame.arg(function, 0);
    wasm!(function; LocalSet(items));
    frame.arg(function, 1);
    wasm!(function; LocalSet(map_fn));
    frame.arg(function, 2);
    wasm!(function;
        LocalSet(this_arg),
        LocalGet(map_fn), I64Const(UNDEFINED_BITS), I64Ne, LocalTee(mapping),
        If(BlockType::Empty),
    );
    require_callable(function, frame, map_fn, FROM_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    emit_is_nullish(function, items);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, FROM_NULLISH, strings, helpers);
    wasm!(function; End);
    // `v = mapFn(v, k)` when mapping, then `A[k] = v`, `k += 1`; a throw
    // propagates (to the caller's handler, if any).
    let store = |function: &mut Function| {
        wasm!(function; LocalGet(mapping), If(BlockType::Empty));
        enter_frame(helpers);
        push_call(
            function,
            frame,
            map_fn,
            this_arg,
            &[CallArg::Val(v), CallArg::Index(k)],
            helpers,
        );
        wasm!(function; LocalSet(v));
        emit_propagate_if_pending(function, helpers);
        leave_frame(helpers);
        wasm!(function; End);
        index_value(function, k);
        wasm!(function; LocalSet(kv));
        create_data_property(function, frame, a, kv, v, strings, helpers);
        wasm!(function; LocalGet(k), I32Const(1), I32Add, LocalSet(k));
    };
    wasm!(function; I32Const(0), LocalSet(k));
    // The record state of `items`' fast path, or the protocol.
    emit_iter_classify(function, items, false, frame.y32(0), helpers);
    wasm!(function; LocalTee(frame.z64(2)), I64Const(iter::protocol_state()), I64Eq, If(BlockType::Empty));
    wasm!(function;
        LocalGet(items), I32Const(realm::symbol_addr(WellKnown::Iterator, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    return_if_pending(function);
    // No `@@iterator`: an array-like.
    emit_is_nullish(function, method);
    wasm!(function; If(BlockType::Empty));
    wasm!(function; LocalGet(items), Call(helpers.to_object()), LocalSet(items));
    length_of(function, items, len, strings, helpers);
    index_value(function, len);
    wasm!(function; LocalSet(len_v));
    construct_or_create(function, frame, c, Some(len_v), a, strings, helpers);
    count_loop(function, k, len, |function| {
        get_index(function, items, k, key, v, helpers);
        store(function);
        // `store` advanced `k`; the loop advances it again.
        wasm!(function; LocalGet(k), I32Const(1), I32Sub, LocalSet(k));
    });
    set_length(function, frame, a, len_v, strings, helpers);
    wasm!(function; LocalGet(a), Return, End);
    construct_or_create(function, frame, c, None, a, strings, helpers);
    iter::emit_iter_open_with(function, items, method, &rec, scratch, strings, helpers);
    wasm!(function; Else);
    wasm!(function; LocalGet(items), LocalSet(frame.z64(0)), I64Const(0), LocalSet(frame.z64(1)));
    construct_or_create(function, frame, c, None, a, strings, helpers);
    wasm!(function; End);
    // Step the record; a throw from `mapFn` or the define closes it (one
    // from the iterator itself leaves it done, so the close does nothing).
    iter::guarded(function, helpers, |function| {
        wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
        enter_frame(helpers);
        enter_frame(helpers);
        emit_iterator_step(&rec, function, scratch, strings, helpers);
        wasm!(function; I32Eqz, BrIf(1));
        emit_iterator_value(&rec, function, scratch, strings, helpers);
        wasm!(function; LocalSet(v));
        store(function);
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
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    index_value(function, k);
    wasm!(function; LocalSet(kv));
    set_length(function, frame, a, kv, strings, helpers);
    wasm!(function; LocalGet(a));
}

// --- at / find* / some / every / reduceRight / lastIndexOf ----------------------------

/// `LengthOfArrayLike(o)` into f64 `len` over the full `ToLength` range.
fn length_of_f64(
    function: &mut Function,
    o: u32,
    len: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(o), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get));
    return_if_pending(function);
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function;
        F64ReinterpretI64, F64Trunc, F64Const(Ieee64::from(0.0)), F64Max,
        F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Min, LocalSet(len),
        // NaN propagates through `max`/`min`; ToLength reads it as 0.
        LocalGet(len), LocalGet(len), F64Ne,
        If(BlockType::Empty), F64Const(Ieee64::from(0.0)), LocalSet(len), End,
    );
}

/// `Array.prototype.at(index)`: `k = ToIntegerOrInfinity(index)`, relative
/// to `len` when negative; `Get(O, k)` when `0 <= k < len`, else `undefined`.
pub(super) fn emit_at(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let o = frame.i64(0);
    let (len, r) = (frame.yf64(0), frame.f64(0));
    this_object(function, frame, o, AT.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    frame.arg(function, 0);
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), LocalGet(len), LocalGet(r), F64Add, LocalSet(r), End,
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(r), LocalGet(len), F64Ge, I32Or,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        // `+ 0` turns `-0` into `+0`.
        LocalGet(o),
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Add, I64ReinterpretF64,
        I32Const(1), Call(helpers.to_key), Call(helpers.get),
    );
}

/// `Array.prototype.find/findIndex/findLast/findLastIndex/some/every
/// (predicate, thisArg)`: `len`, the `IsCallable` check, then the predicate
/// over `(O[k], k, O)` with `this = thisArg` for each `k` (descending for
/// `findLast*`) until it decides: `find*` visit every index (an absent one
/// reads `undefined`), `some` / `every` only the present ones.
pub(super) fn emit_find(
    op: ArrayFindOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let texts = find_texts(op);
    let (o, pred, this_arg, v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (r, kv) = (frame.x64(0), frame.x64(1));
    let key = frame.i32(2);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let present_only = matches!(op, ArrayFindOp::Some | ArrayFindOp::Every);
    this_object(function, frame, o, texts.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(pred));
    require_callable(function, frame, pred, texts.not_callable, strings, helpers);
    frame.arg(function, 1);
    wasm!(function; LocalSet(this_arg));
    let body = |function: &mut Function| {
        if present_only {
            has_at(function, o, k, key, helpers);
            wasm!(function; If(BlockType::Empty));
            get_key(function, o, key, v, helpers);
        } else {
            get_at(function, o, k, key, v, helpers);
        }
        num(function, k);
        wasm!(function; LocalSet(kv));
        let args = [CallArg::Val(v), CallArg::Val(kv), CallArg::Val(o)];
        call_fn(function, frame, pred, this_arg, &args, r, helpers);
        emit_cond(
            &IrNode::Local(r),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        if op == ArrayFindOp::Every {
            wasm!(function; I32Eqz);
        }
        wasm!(function; If(BlockType::Empty));
        match op {
            ArrayFindOp::Find | ArrayFindOp::FindLast => wasm!(function; LocalGet(v)),
            ArrayFindOp::FindIndex | ArrayFindOp::FindLastIndex => num(function, k),
            ArrayFindOp::Some => wasm!(function; I64Const(TRUE_BITS)),
            ArrayFindOp::Every => wasm!(function; I64Const(FALSE_BITS)),
        }
        wasm!(function; Return, End);
        if present_only {
            wasm!(function; End);
        }
    };
    if matches!(op, ArrayFindOp::FindLast | ArrayFindOp::FindLastIndex) {
        wasm!(function; LocalGet(len), LocalSet(k));
        down_loop_f64(function, k, body);
    } else {
        wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k));
        while_loop(
            function,
            |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
            |function| {
                body(function);
                bump(function, k, 1.0);
            },
        );
    }
    match op {
        ArrayFindOp::Find | ArrayFindOp::FindLast => wasm!(function; I64Const(UNDEFINED_BITS)),
        ArrayFindOp::FindIndex | ArrayFindOp::FindLastIndex => {
            wasm!(function; I64Const((-1.0f64).to_bits().cast_signed()));
        }
        ArrayFindOp::Some => wasm!(function; I64Const(FALSE_BITS)),
        ArrayFindOp::Every => wasm!(function; I64Const(TRUE_BITS)),
    }
}

/// `Array.prototype.reduceRight(callback, initial)`: `reduce` from the last
/// index down, over the present indices; without an initial value the
/// accumulator is seeded from the last present element (none throws).
pub(super) fn emit_reduce_right(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, callback, this_arg, value) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (acc, kv) = (frame.x64(0), frame.x64(1));
    let (key, seeded) = (frame.i32(2), frame.i32(3));
    let k = frame.yf64(1);
    this_object(function, frame, o, REDUCE_RIGHT.nullish, strings, helpers);
    length_of_f64(function, o, k, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(callback));
    require_callable(
        function,
        frame,
        callback,
        REDUCE_RIGHT.not_callable,
        strings,
        helpers,
    );
    wasm!(function;
        I64Const(UNDEFINED_BITS), LocalSet(this_arg),
        LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Ge,
        LocalTee(seeded), If(BlockType::Empty),
    );
    frame.arg(function, 1);
    wasm!(function; LocalSet(acc), End);
    down_loop_f64(function, k, |function| {
        has_at(function, o, k, key, helpers);
        wasm!(function; If(BlockType::Empty));
        get_key(function, o, key, value, helpers);
        wasm!(function; LocalGet(seeded), If(BlockType::Empty));
        num(function, k);
        wasm!(function; LocalSet(kv));
        let args = [
            CallArg::Val(acc),
            CallArg::Val(value),
            CallArg::Val(kv),
            CallArg::Val(o),
        ];
        call_fn(function, frame, callback, this_arg, &args, acc, helpers);
        wasm!(function;
            Else,
            LocalGet(value), LocalSet(acc), I32Const(1), LocalSet(seeded),
            End,
            End,
        );
    });
    wasm!(function; LocalGet(seeded), I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, REDUCE_EMPTY, strings, helpers);
    wasm!(function; End, LocalGet(acc));
}

/// `Array.prototype.lastIndexOf(search, fromIndex)`: from
/// `min(fromIndex, len - 1)` (relative to `len` when negative; `len - 1`
/// without the argument) down to 0 over the present indices, comparing with
/// `===`.
pub(super) fn emit_last_index_of(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, search, v) = (frame.i64(0), frame.i64(1), frame.i64(2));
    let key = frame.i32(2);
    let r = frame.f64(0);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let not_found = (-1.0f64).to_bits().cast_signed();
    this_object(function, frame, o, LAST_INDEX_OF.nullish, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(search));
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(0.0)), F64Eq,
        If(BlockType::Empty), I64Const(not_found), Return, End,
        LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Ge,
        If(BlockType::Empty),
    );
    frame.arg(function, 1);
    to_integer(function, r, helpers);
    wasm!(function;
        Else,
        F64Const(Ieee64::from(f64::INFINITY)), LocalSet(r),
        End,
        // k = r >= 0 ? min(r, len - 1) : len + r; none when k < 0.
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Result(ValType::F64)),
        LocalGet(len), LocalGet(r), F64Add,
        Else,
        LocalGet(r), LocalGet(len), F64Const(Ieee64::from(1.0)), F64Sub, F64Min,
        End,
        LocalTee(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), I64Const(not_found), Return, End,
        LocalGet(r), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(k),
    );
    down_loop_f64(function, k, |function| {
        has_at(function, o, k, key, helpers);
        wasm!(function; If(BlockType::Empty));
        get_key(function, o, key, v, helpers);
        emit_node(
            &strict_eq(v, search),
            function,
            frame.scratch(),
            strings,
            helpers,
        );
        wasm!(function; I64Const(TRUE_BITS), I64Eq, If(BlockType::Empty));
        num(function, k);
        wasm!(function; Return, End, End);
    });
    wasm!(function; I64Const(not_found));
}

// --- concat / flat / flatMap --------------------------------------------------------

/// `Array.prototype.concat(...items)`: `A = ArraySpeciesCreate(O, 0)`; for
/// each of `O, ...items` an array (`IsArray`) appends its present indices at
/// `n + k` (`n` advancing over holes too), any other value is one element;
/// then `Set(A, "length", n)`. An object item whose chain holds a
/// `@@isConcatSpreadable` (`__symbol_get`) throws the interim `TypeError`
/// (Y4 honours it, design/symbol-strings.md §9.4).
pub(super) fn emit_concat(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, item, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (target, saved, zero) = (frame.x64(0), frame.x64(1), frame.y64(1));
    let (j, argn, len, k) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let key = frame.x32(0);
    let count = frame.yf64(0);
    let advance = |function: &mut Function| {
        wasm!(function; LocalGet(count), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(count));
    };
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    this_object(function, frame, o, CONCAT.nullish, strings, helpers);
    wasm!(function; I64Const(0.0f64.to_bits().cast_signed()), LocalSet(zero));
    species_create(function, frame, o, zero, target, strings, helpers);
    push_argc(function, frame);
    wasm!(function;
        LocalSet(argn),
        F64Const(Ieee64::from(0.0)), LocalSet(count),
        LocalGet(o), LocalSet(item),
        I32Const(0), LocalSet(j),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    emit_is_object(function, item);
    wasm!(function; If(BlockType::Empty));
    emit_hook_guard(
        function,
        item,
        WellKnown::IsConcatSpreadable,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; End);
    return_if_pending(function);
    is_array(function, item);
    wasm!(function; If(BlockType::Empty));
    length_of(function, item, len, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(k));
    count_loop(function, k, len, |function| {
        has_index(function, item, k, key, helpers);
        wasm!(function;
            If(BlockType::Empty),
            LocalGet(item), LocalGet(key), Call(helpers.get), LocalSet(v),
        );
        return_if_pending(function);
        wasm!(function; LocalGet(count), I64ReinterpretF64, LocalSet(kv));
        create_data_property(function, frame, target, kv, v, strings, helpers);
        wasm!(function; End);
        advance(function);
    });
    wasm!(function; Else, LocalGet(count), I64ReinterpretF64, LocalSet(kv));
    create_data_property(function, frame, target, kv, item, strings, helpers);
    advance(function);
    wasm!(function;
        End,
        LocalGet(j), LocalGet(argn), I32GeU, BrIf(1),
    );
    arg_at(function, frame, j, saved, item);
    wasm!(function;
        LocalGet(j), I32Const(1), I32Add, LocalSet(j),
        Br(0),
        End, End,
        LocalGet(count), I64ReinterpretF64, LocalSet(kv),
    );
    set_length(function, frame, target, kv, strings, helpers);
    wasm!(function; LocalGet(target));
}

/// `Array.prototype.flat(depth)` / `flatMap(mapper, thisArg)`:
/// `A = ArraySpeciesCreate(O, 0)`, then `FlattenIntoArray` without recursion:
/// a stack array of `(source, next k, len)` frames; each present element
/// (`flatMap`: the mapper's result over `(O[k], k, O)` at the top level) that
/// is an array while depth remains (`flat`: `ToIntegerOrInfinity(depth)`, 1
/// when `undefined`; `flatMap`: 1) is descended into, any other is appended.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_flat(
    map: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let texts = if map { &FLAT_MAP } else { &FLAT };
    let (o, cur, el, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (a, mapper, zero, this_arg) = (frame.x64(0), frame.x64(1), frame.y64(1), frame.y64(4));
    let (k, len, key, stack) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let top = frame.x32(0);
    let (depth, n) = (frame.f64(0), frame.yf64(0));
    this_object(function, frame, o, texts.nullish, strings, helpers);
    length_of(function, o, len, strings, helpers);
    if map {
        frame.arg(function, 0);
        wasm!(function; LocalSet(mapper));
        require_callable(
            function,
            frame,
            mapper,
            texts.not_callable,
            strings,
            helpers,
        );
        frame.arg(function, 1);
        wasm!(function; LocalSet(this_arg), F64Const(Ieee64::from(1.0)), LocalSet(depth));
    } else {
        frame.arg(function, 0);
        wasm!(function;
            LocalTee(el), I64Const(UNDEFINED_BITS), I64Eq,
            If(BlockType::Empty),
            F64Const(Ieee64::from(1.0)), LocalSet(depth),
            Else,
            LocalGet(el),
        );
        to_integer(function, depth, helpers);
        wasm!(function;
            LocalGet(depth), F64Const(Ieee64::from(0.0)), F64Max, LocalSet(depth),
            End,
        );
    }
    wasm!(function; I64Const(0.0f64.to_bits().cast_signed()), LocalSet(zero));
    species_create(function, frame, o, zero, a, strings, helpers);
    wasm!(function;
        F64Const(Ieee64::from(0.0)), LocalSet(n),
        I32Const(0), Call(helpers.new_array()), LocalSet(stack),
        LocalGet(o), LocalSet(cur),
        I32Const(0), LocalSet(k),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        // The source is exhausted: done at the top level, else pop a frame.
        LocalGet(k), LocalGet(len), I32GeU,
        If(BlockType::Empty),
        LocalGet(stack), I32Load(array_len_slot()), I32Eqz, BrIf(2),
        LocalGet(stack),
        LocalGet(stack), I32Load(array_len_slot()), I32Const(3), I32Sub, LocalTee(top),
        I32Store(array_len_slot()),
        LocalGet(stack),
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(top), I32Const(3), I32Shl, I32Add, LocalSet(top),
        LocalGet(top), I64Load(elem_slot(0)), LocalSet(cur),
        LocalGet(top), I64Load(elem_slot(1)), F64ReinterpretI64, I32TruncSatF64U, LocalSet(k),
        LocalGet(top), I64Load(elem_slot(2)), F64ReinterpretI64, I32TruncSatF64U, LocalSet(len),
        LocalGet(depth), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(depth),
        Br(1),
        End,
    );
    has_index(function, cur, k, key, helpers);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(cur), LocalGet(key), Call(helpers.get), LocalSet(el),
    );
    return_if_pending(function);
    if map {
        wasm!(function; LocalGet(stack), I32Load(array_len_slot()), I32Eqz, If(BlockType::Empty));
        let args = [CallArg::Val(el), CallArg::Index(k), CallArg::Val(o)];
        call_fn(function, frame, mapper, this_arg, &args, el, helpers);
        wasm!(function; End);
    }
    wasm!(function; LocalGet(depth), F64Const(Ieee64::from(0.0)), F64Gt);
    is_array(function, el);
    wasm!(function;
        I32And,
        If(BlockType::Empty),
        LocalGet(stack), LocalGet(cur), Call(helpers.array_push), Drop,
        LocalGet(stack),
    );
    wasm!(function; LocalGet(k), I32Const(1), I32Add);
    wasm!(function;
        F64ConvertI32U, I64ReinterpretF64, Call(helpers.array_push), Drop,
        LocalGet(stack),
    );
    index_value(function, len);
    wasm!(function;
        Call(helpers.array_push), Drop,
        LocalGet(el), LocalSet(cur),
        LocalGet(depth), F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(depth),
    );
    length_of(function, cur, len, strings, helpers);
    wasm!(function;
        I32Const(0), LocalSet(k),
        Br(2),
        Else,
        LocalGet(n), I64ReinterpretF64, LocalSet(kv),
    );
    create_data_property(function, frame, a, kv, el, strings, helpers);
    wasm!(function;
        LocalGet(n), F64Const(Ieee64::from(1.0)), F64Add, LocalSet(n),
        End,
        End,
        LocalGet(k), I32Const(1), I32Add, LocalSet(k),
        Br(0),
        End, End,
        LocalGet(a),
    );
}

// --- splice / sort / reverse / shift / unshift / fill / copyWithin --------------------

const SPLICE: Texts = texts!("splice");
const SORT: Texts = texts!("sort");
const REVERSE: Texts = texts!("reverse");
const SHIFT: Texts = texts!("shift");
const UNSHIFT: Texts = texts!("unshift");
const FILL: Texts = texts!("fill");
const COPY_WITHIN: Texts = texts!("copyWithin");
const WITH: Texts = texts!("with");
const TO_REVERSED: Texts = texts!("toReversed");
const TO_SORTED: Texts = texts!("toSorted");
const TO_SPLICED: Texts = texts!("toSpliced");

/// The `RangeError` of `with` with an index outside the receiver.
const WITH_OUT_OF_RANGE: CalleeErrorText<'static> = ["Invalid index", "RangeError: Invalid index"];

/// The `TypeError` of `sort` with a comparator neither callable nor
/// `undefined`.
const SORT_NOT_CALLABLE: CalleeErrorText<'static> = [
    "The comparison function must be either a function or undefined",
    "TypeError: The comparison function must be either a function or undefined",
];

/// The `TypeError` of a `splice` / `unshift` whose new length passes
/// [`MAX_SAFE_INTEGER`].
const TOO_LONG: CalleeErrorText<'static> = [
    "The new length exceeds the maximum array-like length",
    "TypeError: The new length exceeds the maximum array-like length",
];

/// The static strings of the mutating methods.
fn mutating_strings() -> impl Iterator<Item = &'static str> {
    [
        &SPLICE,
        &SORT,
        &REVERSE,
        &SHIFT,
        &UNSHIFT,
        &FILL,
        &COPY_WITHIN,
        &WITH,
        &TO_REVERSED,
        &TO_SORTED,
        &TO_SPLICED,
    ]
    .into_iter()
    .flat_map(|t| t.nullish)
    .chain(WITH_OUT_OF_RANGE)
    .chain(SORT_NOT_CALLABLE)
    .chain(TOO_LONG)
}

/// Push the f64 index in local `f` as a number jsval.
fn num(function: &mut Function, f: u32) {
    wasm!(function; LocalGet(f), I64ReinterpretF64);
}

/// `f += delta` over an f64 local.
fn bump(function: &mut Function, f: u32, delta: f64) {
    wasm!(function; LocalGet(f), F64Const(Ieee64::from(delta)), F64Add, LocalSet(f));
}

/// `while cond { body }`: `cond` pushes an i32, nonzero to run `body` again.
fn while_loop(
    function: &mut Function,
    cond: impl FnOnce(&mut Function),
    body: impl FnOnce(&mut Function),
) {
    wasm!(function; Block(BlockType::Empty), Loop(BlockType::Empty));
    cond(function);
    wasm!(function; I32Eqz, BrIf(1));
    body(function);
    wasm!(function; Br(0), End, End);
}

/// Push the index the relative position in f64 `r` designates in the f64
/// length `len`: `max(len + r, 0)` when `r < 0`, else `min(r, len)` (`-0`
/// is `+0`).
fn relative_f64(function: &mut Function, r: u32, len: u32) {
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Result(ValType::F64)),
        LocalGet(len), LocalGet(r), F64Add, F64Const(Ieee64::from(0.0)), F64Max,
        Else,
        LocalGet(r), LocalGet(len), F64Min,
        End,
        F64Const(Ieee64::from(0.0)), F64Add,
    );
}

/// `ToIntegerOrInfinity` of the jsval in i64 `v` relative to the f64 length
/// `len` (or `len` itself when `v` is `undefined` and `undefined_is_len`),
/// into f64 `out` (`r` an f64 temp).
fn relative_arg(
    function: &mut Function,
    v: u32,
    undefined_is_len: bool,
    r: u32,
    len: u32,
    out: u32,
    helpers: Helpers<'_>,
) {
    if undefined_is_len {
        wasm!(function;
            LocalGet(v), I64Const(UNDEFINED_BITS), I64Eq,
            If(BlockType::Empty), LocalGet(len), LocalSet(out), Else,
        );
    }
    wasm!(function; LocalGet(v));
    to_integer(function, r, helpers);
    relative_f64(function, r, len);
    wasm!(function; LocalSet(out));
    if undefined_is_len {
        wasm!(function; End);
    }
}

/// Push `HasProperty(o, f)` (i32) for the f64 index in local `f`, leaving its
/// interned key in i32 `key`.
fn has_at(function: &mut Function, o: u32, f: u32, key: u32, helpers: Helpers<'_>) {
    num(function, f);
    wasm!(function;
        I32Const(1), Call(helpers.to_key), LocalTee(key),
        LocalGet(o), Call(helpers.has),
    );
}

/// `Get(o, key)` into i64 `v` for the interned key in i32 `key`; a throwing
/// getter returns.
fn get_key(function: &mut Function, o: u32, key: u32, v: u32, helpers: Helpers<'_>) {
    wasm!(function; LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(v));
    return_if_pending(function);
}

/// `Set(o, f, v, true)` for the f64 index in local `f` (`kv` an i64 temp).
fn set_at(
    function: &mut Function,
    frame: Frame,
    (o, f, v, kv): (u32, u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    num(function, f);
    wasm!(function; LocalSet(kv));
    emit_drop(
        function,
        frame,
        &set(o, IrNode::Local(kv), v),
        strings,
        helpers,
    );
}

/// `DeletePropertyOrThrow(o, f)` for the f64 index in local `f` (`kv` an i64
/// temp).
fn delete_at(
    function: &mut Function,
    frame: Frame,
    (o, f, kv): (u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    num(function, f);
    wasm!(function; LocalSet(kv));
    emit_drop(function, frame, &delete(o, kv), strings, helpers);
}

/// The element move of `splice` / `shift` / `unshift` / `copyWithin`: when
/// `HasProperty(o, from)`, `Set(o, to, Get(o, from), true)`, else
/// `DeletePropertyOrThrow(o, to)` (f64 indices; `key`, `v`, `kv` temps).
fn move_or_delete(
    function: &mut Function,
    frame: Frame,
    (o, from, to): (u32, u32, u32),
    (key, v, kv): (u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    has_at(function, o, from, key, helpers);
    wasm!(function; If(BlockType::Empty));
    get_key(function, o, key, v, helpers);
    set_at(function, frame, (o, to, v, kv), strings, helpers);
    wasm!(function; Else);
    delete_at(function, frame, (o, to, kv), strings, helpers);
    wasm!(function; End);
}

/// `Set(o, "length", f, true)` for the f64 length in local `f` (`kv` an i64
/// temp).
fn set_length_f64(
    function: &mut Function,
    frame: Frame,
    (o, f, kv): (u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    num(function, f);
    wasm!(function; LocalSet(kv));
    set_length(function, frame, o, kv, strings, helpers);
}

/// Throw [`TOO_LONG`] when `len + delta` (f64 locals; `delta` a small
/// integer, so the sum rounds past [`MAX_SAFE_INTEGER`] only when it is past
/// it) exceeds [`MAX_SAFE_INTEGER`].
fn check_new_length(
    function: &mut Function,
    frame: Frame,
    len: u32,
    delta: impl FnOnce(&mut Function),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(len));
    delta(function);
    wasm!(function;
        F64Add, F64Const(Ieee64::from(MAX_SAFE_INTEGER)), F64Gt,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, TOO_LONG, strings, helpers);
    wasm!(function; End);
}

/// `Array.prototype.splice(start, deleteCount, ...items)`: `actualStart`
/// relative to `len`; `actualDeleteCount` 0 without arguments, `len -
/// actualStart` without a `deleteCount`, else clamped to `[0, len -
/// actualStart]`; a new length past `2^53 - 1` throws; `A =
/// ArraySpeciesCreate(O, actualDeleteCount)` receives the deleted present
/// elements and its `length`; the tail moves down (then the vacated top is
/// deleted) or up to fit the items, which are `Set` from `actualStart`; then
/// `Set(O, "length", len - actualDeleteCount + itemCount)`.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_splice(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, kv, start_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (dc_v, saved) = (frame.x64(0), frame.x64(1));
    let (count, target) = (frame.y64(1), frame.y64(4));
    let (key, arg_i, argn) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let r = frame.f64(0);
    let (len, start) = (frame.yf64(0), frame.yf64(1));
    let (dc, items, k) = (frame.zf64(0), frame.zf64(1), frame.zf64(2));
    let (from, to, bound) = (frame.zf64(3), frame.zf64(4), frame.zf64(5));
    let temps = (key, v, kv);
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    frame.arg(function, 0);
    wasm!(function; LocalSet(start_v));
    frame.arg(function, 1);
    wasm!(function; LocalSet(dc_v));
    push_argc(function, frame);
    wasm!(function; LocalSet(argn));
    this_object(function, frame, o, SPLICE.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    relative_arg(function, start_v, false, r, len, start, helpers);
    wasm!(function;
        // itemCount = max(argc - 2, 0)
        LocalGet(argn), I32Const(2), I32Sub, I32Const(0), LocalGet(argn), I32Const(2), I32GtU, Select,
        F64ConvertI32U, LocalSet(items),
        LocalGet(argn), I32Eqz,
        If(BlockType::Empty),
        F64Const(Ieee64::from(0.0)), LocalSet(dc),
        Else,
        LocalGet(len), LocalGet(start), F64Sub, LocalSet(dc),
        LocalGet(argn), I32Const(1), I32GtU,
        If(BlockType::Empty),
        LocalGet(dc_v),
    );
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Max, LocalGet(dc), F64Min, LocalSet(dc),
        End,
        End,
    );
    check_new_length(
        function,
        frame,
        len,
        |function| wasm!(function; LocalGet(items), LocalGet(dc), F64Sub),
        strings,
        helpers,
    );
    num(function, dc);
    wasm!(function; LocalSet(count));
    species_create(function, frame, o, count, target, strings, helpers);
    // The deleted elements into `A`.
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(dc), F64Lt),
        |function| {
            wasm!(function; LocalGet(start), LocalGet(k), F64Add, LocalSet(from));
            has_at(function, o, from, key, helpers);
            wasm!(function; If(BlockType::Empty));
            get_key(function, o, key, v, helpers);
            num(function, k);
            wasm!(function; LocalSet(kv));
            create_data_property(function, frame, target, kv, v, strings, helpers);
            wasm!(function; End);
            bump(function, k, 1.0);
        },
    );
    set_length_f64(function, frame, (target, dc, kv), strings, helpers);
    // Shift the tail to fit the items.
    wasm!(function; LocalGet(items), LocalGet(dc), F64Lt, If(BlockType::Empty));
    wasm!(function;
        LocalGet(start), LocalSet(k),
        LocalGet(len), LocalGet(dc), F64Sub, LocalSet(bound),
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(bound), F64Lt),
        |function| {
            wasm!(function;
                LocalGet(k), LocalGet(dc), F64Add, LocalSet(from),
                LocalGet(k), LocalGet(items), F64Add, LocalSet(to),
            );
            move_or_delete(function, frame, (o, from, to), temps, strings, helpers);
            bump(function, k, 1.0);
        },
    );
    wasm!(function;
        LocalGet(len), LocalSet(k),
        LocalGet(len), LocalGet(dc), F64Sub, LocalGet(items), F64Add, LocalSet(bound),
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(bound), F64Gt),
        |function| {
            bump(function, k, -1.0);
            delete_at(function, frame, (o, k, kv), strings, helpers);
        },
    );
    wasm!(function; Else, LocalGet(items), LocalGet(dc), F64Gt, If(BlockType::Empty));
    wasm!(function; LocalGet(len), LocalGet(dc), F64Sub, LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(start), F64Gt),
        |function| {
            wasm!(function;
                LocalGet(k), LocalGet(dc), F64Add, F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(from),
                LocalGet(k), LocalGet(items), F64Add, F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(to),
            );
            move_or_delete(function, frame, (o, from, to), temps, strings, helpers);
            bump(function, k, -1.0);
        },
    );
    wasm!(function; End, End);
    // The items from `actualStart`.
    wasm!(function; LocalGet(start), LocalSet(k), I32Const(2), LocalSet(arg_i));
    count_loop(function, arg_i, argn, |function| {
        arg_at(function, frame, arg_i, saved, v);
        set_at(function, frame, (o, k, v, kv), strings, helpers);
        bump(function, k, 1.0);
    });
    wasm!(function;
        LocalGet(len), LocalGet(dc), F64Sub, LocalGet(items), F64Add, LocalSet(bound),
    );
    set_length_f64(function, frame, (o, bound, kv), strings, helpers);
    wasm!(function; LocalGet(target));
}

/// `Array.prototype.reverse()`: swap `lower` and `len - 1 - lower` for each
/// `lower < floor(len / 2)`, by `HasProperty` / `Get` of both, then `Set` of
/// the present ones and `DeletePropertyOrThrow` of the absent ones.
pub(super) fn emit_reverse(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, lv, uv, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (key, lower_present, upper_present) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let (len, lower) = (frame.yf64(0), frame.yf64(1));
    let (upper, middle) = (frame.zf64(0), frame.zf64(1));
    this_object(function, frame, o, REVERSE.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function;
        LocalGet(len), F64Const(Ieee64::from(2.0)), F64Div, F64Floor, LocalSet(middle),
        F64Const(Ieee64::from(0.0)), LocalSet(lower),
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(lower), LocalGet(middle), F64Lt),
        |function| {
            wasm!(function;
                LocalGet(len), LocalGet(lower), F64Sub, F64Const(Ieee64::from(1.0)), F64Sub,
                LocalSet(upper),
            );
            has_at(function, o, lower, key, helpers);
            wasm!(function; LocalTee(lower_present), If(BlockType::Empty));
            get_key(function, o, key, lv, helpers);
            wasm!(function; End);
            has_at(function, o, upper, key, helpers);
            wasm!(function; LocalTee(upper_present), If(BlockType::Empty));
            get_key(function, o, key, uv, helpers);
            wasm!(function; End, LocalGet(upper_present), If(BlockType::Empty));
            set_at(function, frame, (o, lower, uv, kv), strings, helpers);
            wasm!(function; LocalGet(lower_present), If(BlockType::Empty));
            set_at(function, frame, (o, upper, lv, kv), strings, helpers);
            wasm!(function; Else);
            delete_at(function, frame, (o, upper, kv), strings, helpers);
            wasm!(function; End, Else, LocalGet(lower_present), If(BlockType::Empty));
            delete_at(function, frame, (o, lower, kv), strings, helpers);
            set_at(function, frame, (o, upper, lv, kv), strings, helpers);
            wasm!(function; End, End);
            bump(function, lower, 1.0);
        },
    );
    wasm!(function; LocalGet(o));
}

/// `Array.prototype.shift()`: an empty receiver gets `length` 0 and returns
/// `undefined`; else `first = Get(O, "0")`, each index `1..len` moves down
/// one, the last index is deleted and `length` becomes `len - 1`.
pub(super) fn emit_shift(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, first, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let key = frame.i32(0);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let to = frame.zf64(0);
    this_object(function, frame, o, SHIFT.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function; LocalGet(len), F64Const(Ieee64::from(0.0)), F64Eq, If(BlockType::Empty));
    set_length_f64(function, frame, (o, len, kv), strings, helpers);
    wasm!(function;
        I64Const(UNDEFINED_BITS), Return,
        End,
        F64Const(Ieee64::from(0.0)), LocalSet(k),
    );
    num(function, k);
    wasm!(function; I32Const(1), Call(helpers.to_key), LocalSet(key));
    get_key(function, o, key, first, helpers);
    wasm!(function; F64Const(Ieee64::from(1.0)), LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
        |function| {
            wasm!(function; LocalGet(k), F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(to));
            move_or_delete(function, frame, (o, k, to), (key, v, kv), strings, helpers);
            bump(function, k, 1.0);
        },
    );
    wasm!(function; LocalGet(len), F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(to));
    delete_at(function, frame, (o, to, kv), strings, helpers);
    set_length_f64(function, frame, (o, to, kv), strings, helpers);
    wasm!(function; LocalGet(first));
}

/// `Array.prototype.unshift(...items)`: with items, a new length past
/// `2^53 - 1` throws, each index `len - 1..0` moves up by the item count and
/// the items are `Set` from 0; `length` becomes (and the call returns)
/// `len + argCount`.
pub(super) fn emit_unshift(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, kv, saved) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let (key, arg_i, argn) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    let (from, to, items) = (frame.zf64(0), frame.zf64(1), frame.zf64(2));
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    push_argc(function, frame);
    wasm!(function; LocalTee(argn), F64ConvertI32U, LocalSet(items));
    this_object(function, frame, o, UNSHIFT.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function; LocalGet(argn), If(BlockType::Empty));
    check_new_length(
        function,
        frame,
        len,
        |function| wasm!(function; LocalGet(items)),
        strings,
        helpers,
    );
    wasm!(function; LocalGet(len), LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), F64Const(Ieee64::from(0.0)), F64Gt),
        |function| {
            wasm!(function;
                LocalGet(k), F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(from),
                LocalGet(from), LocalGet(items), F64Add, LocalSet(to),
            );
            move_or_delete(
                function,
                frame,
                (o, from, to),
                (key, v, kv),
                strings,
                helpers,
            );
            bump(function, k, -1.0);
        },
    );
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k), I32Const(0), LocalSet(arg_i));
    count_loop(function, arg_i, argn, |function| {
        arg_at(function, frame, arg_i, saved, v);
        set_at(function, frame, (o, k, v, kv), strings, helpers);
        bump(function, k, 1.0);
    });
    wasm!(function; End, LocalGet(len), LocalGet(items), F64Add, LocalSet(to));
    set_length_f64(function, frame, (o, to, kv), strings, helpers);
    wasm!(function; LocalGet(kv));
}

/// `Array.prototype.fill(value, start, end)`: `Set(O, k, value, true)` for
/// each `k` in `[relative start, relative end)` (`end` is `len` when
/// `undefined`).
pub(super) fn emit_fill(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, value, kv, end_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let start_v = frame.x64(0);
    let r = frame.f64(0);
    let (len, k, fin) = (frame.yf64(0), frame.yf64(1), frame.zf64(0));
    frame.arg(function, 0);
    wasm!(function; LocalSet(value));
    frame.arg(function, 1);
    wasm!(function; LocalSet(start_v));
    frame.arg(function, 2);
    wasm!(function; LocalSet(end_v));
    this_object(function, frame, o, FILL.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    relative_arg(function, start_v, false, r, len, k, helpers);
    relative_arg(function, end_v, true, r, len, fin, helpers);
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(fin), F64Lt),
        |function| {
            set_at(function, frame, (o, k, value, kv), strings, helpers);
            bump(function, k, 1.0);
        },
    );
    wasm!(function; LocalGet(o));
}

/// `Array.prototype.copyWithin(target, start, end)`: `to`, `from` and
/// `final` relative to `len` (`final` is `len` when `end` is `undefined`),
/// `count = min(final - from, len - to)`; the move runs downwards when the
/// ranges overlap with `from < to`, and each step moves or deletes one index.
pub(super) fn emit_copy_within(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, v, kv, target_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (start_v, end_v) = (frame.x64(0), frame.x64(1));
    let key = frame.i32(0);
    let r = frame.f64(0);
    let (len, to) = (frame.yf64(0), frame.yf64(1));
    let (from, fin, count, dir) = (frame.zf64(0), frame.zf64(1), frame.zf64(2), frame.zf64(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(target_v));
    frame.arg(function, 1);
    wasm!(function; LocalSet(start_v));
    frame.arg(function, 2);
    wasm!(function; LocalSet(end_v));
    this_object(function, frame, o, COPY_WITHIN.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    relative_arg(function, target_v, false, r, len, to, helpers);
    relative_arg(function, start_v, false, r, len, from, helpers);
    relative_arg(function, end_v, true, r, len, fin, helpers);
    wasm!(function;
        LocalGet(fin), LocalGet(from), F64Sub, LocalGet(len), LocalGet(to), F64Sub, F64Min,
        LocalSet(count),
        F64Const(Ieee64::from(1.0)), LocalSet(dir),
        LocalGet(from), LocalGet(to), F64Lt,
        LocalGet(to), LocalGet(from), LocalGet(count), F64Add, F64Lt, I32And,
        If(BlockType::Empty),
        F64Const(Ieee64::from(-1.0)), LocalSet(dir),
        LocalGet(from), LocalGet(count), F64Add, F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(from),
        LocalGet(to), LocalGet(count), F64Add, F64Const(Ieee64::from(1.0)), F64Sub, LocalSet(to),
        End,
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(count), F64Const(Ieee64::from(0.0)), F64Gt),
        |function| {
            move_or_delete(
                function,
                frame,
                (o, from, to),
                (key, v, kv),
                strings,
                helpers,
            );
            wasm!(function;
                LocalGet(from), LocalGet(dir), F64Add, LocalSet(from),
                LocalGet(to), LocalGet(dir), F64Add, LocalSet(to),
            );
            bump(function, count, -1.0);
        },
    );
    wasm!(function; LocalGet(o));
}

/// Push the address of pair `p` (an i32 local) of the sort buffer whose array
/// handle base is in i32 `buf`: its sort key at [`elem_slot`] 0, its value at
/// slot 1.
fn pair_at(function: &mut Function, buf: u32, p: u32) {
    wasm!(function; LocalGet(buf));
    emit_array_elems(function);
    wasm!(function; LocalGet(p), I32Const(4), I32Shl, I32Add);
}

/// `Array.prototype.sort(comparefn)`: a comparator neither `undefined` nor
/// callable throws before `ToObject`. The present indices `k < len` are read
/// (`HasProperty` / `Get`) into a private buffer of `(key, value)` pairs —
/// the key is `ToString(value)` without a comparator — counting the
/// `undefined` values apart; a bottom-up merge sort (stable, `O(n log n)`
/// comparisons) orders the pairs by `comparefn(x, y) < 0` (its result
/// `ToNumber`'d) or by code-unit order of the keys; then the sorted values,
/// the `undefined`s and (by `DeletePropertyOrThrow` up to `len`) the holes
/// are written back. A throw from a getter, `ToString` or the comparator
/// leaves `O` unchanged.
///
/// `Array.prototype.toSorted(comparefn)` (`copy`) is the same sort into `A =
/// ArrayCreate(len)` (created before any element is read): every index is
/// read by `Get` alone (a hole is an `undefined`), and the values and the
/// `undefined`s are defined on `A`, which is returned.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_sort(
    copy: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, cmp, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (lhs, rhs, result, this_arg) = (frame.x64(0), frame.x64(1), frame.y64(1), frame.y64(4));
    let (src, dst, pairs, width) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (lo, mid) = (frame.x32(0), frame.x32(1));
    let (hi, left, right, out) = (frame.y32(0), frame.y32(1), frame.y32(2), frame.y32(3));
    // The key temp of the read and write-back loops (the merge is done or
    // not yet started there).
    let key = lo;
    let (len, k, undefs) = (frame.yf64(0), frame.yf64(1), frame.zf64(0));
    let (texts, target) = if copy {
        (&TO_SORTED, frame.y64(0))
    } else {
        (&SORT, o)
    };
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(cmp), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
    );
    require_callable(function, frame, cmp, SORT_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    this_object(function, frame, o, texts.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    if copy {
        array_create_f64(function, frame, (len, kv, target), strings, helpers);
    }
    let default_order = |function: &mut Function| {
        wasm!(function; LocalGet(cmp), I64Const(UNDEFINED_BITS), I64Eq);
    };
    // Collect.
    wasm!(function;
        I32Const(0), Call(helpers.new_array()), LocalSet(src),
        F64Const(Ieee64::from(0.0)), LocalSet(k),
        F64Const(Ieee64::from(0.0)), LocalSet(undefs),
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
        |function| {
            if copy {
                get_at(function, o, k, key, v, helpers);
                wasm!(function; I32Const(1));
            } else {
                has_at(function, o, k, key, helpers);
            }
            wasm!(function; If(BlockType::Empty));
            if !copy {
                get_key(function, o, key, v, helpers);
            }
            wasm!(function; LocalGet(v), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
            bump(function, undefs, 1.0);
            wasm!(function; Else, LocalGet(v), LocalSet(lhs));
            default_order(function);
            wasm!(function;
                If(BlockType::Empty),
                LocalGet(v), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
                LocalSet(lhs),
            );
            return_if_pending(function);
            wasm!(function;
                End,
                LocalGet(src), LocalGet(lhs), Call(helpers.array_push), Drop,
                LocalGet(src), LocalGet(v), Call(helpers.array_push), Drop,
                End,
                End,
            );
            bump(function, k, 1.0);
        },
    );
    wasm!(function;
        LocalGet(src), I32Load(array_len_slot()), LocalTee(pairs), Call(helpers.new_array()),
        LocalSet(dst),
        LocalGet(pairs), I32Const(1), I32ShrU, LocalSet(pairs),
        I64Const(UNDEFINED_BITS), LocalSet(this_arg),
    );
    // Push i32 `1` when pair `right` sorts strictly before pair `left`.
    let less = |function: &mut Function| {
        pair_at(function, src, right);
        wasm!(function; I64Load(elem_slot(0)), LocalSet(lhs));
        pair_at(function, src, left);
        wasm!(function; I64Load(elem_slot(0)), LocalSet(rhs));
        default_order(function);
        wasm!(function;
            If(BlockType::Result(ValType::I32)),
            LocalGet(lhs), I32WrapI64, LocalGet(rhs), I32WrapI64, Call(helpers.str_cmp),
            I32Const(0), I32LtS,
            Else,
        );
        let args = [CallArg::Val(lhs), CallArg::Val(rhs)];
        call_fn(function, frame, cmp, this_arg, &args, result, helpers);
        wasm!(function; LocalGet(result), Call(helpers.to_number), LocalSet(result));
        return_if_pending(function);
        wasm!(function;
            LocalGet(result), F64ReinterpretI64, F64Const(Ieee64::from(0.0)), F64Lt,
            End,
        );
    };
    // dst[out] = src[pair], then pair += 1 and out += 1.
    let take = |function: &mut Function, pair: u32| {
        for slot in 0..2 {
            pair_at(function, dst, out);
            pair_at(function, src, pair);
            wasm!(function; I64Load(elem_slot(slot)), I64Store(elem_slot(slot)));
        }
        wasm!(function;
            LocalGet(pair), I32Const(1), I32Add, LocalSet(pair),
            LocalGet(out), I32Const(1), I32Add, LocalSet(out),
        );
    };
    // Merge runs of `width` pairs from `src` into `dst`, then swap them.
    wasm!(function; I32Const(1), LocalSet(width));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(width), LocalGet(pairs), I32LtU),
        |function| {
            wasm!(function; I32Const(0), LocalSet(lo));
            while_loop(
                function,
                |function| wasm!(function; LocalGet(lo), LocalGet(pairs), I32LtU),
                |function| {
                    wasm!(function;
                        LocalGet(lo), LocalGet(width), I32Add, LocalTee(mid),
                        LocalGet(pairs), LocalGet(mid), LocalGet(pairs), I32LtU, Select, LocalSet(mid),
                        LocalGet(mid), LocalGet(width), I32Add, LocalTee(hi),
                        LocalGet(pairs), LocalGet(hi), LocalGet(pairs), I32LtU, Select, LocalSet(hi),
                        LocalGet(lo), LocalTee(left), LocalSet(out),
                        LocalGet(mid), LocalSet(right),
                    );
                    while_loop(
                        function,
                        |function| wasm!(function; LocalGet(out), LocalGet(hi), I32LtU),
                        |function| {
                            // Take from the right run when the left is
                            // exhausted or the right pair is strictly less.
                            wasm!(function;
                                LocalGet(left), LocalGet(mid), I32GeU,
                                If(BlockType::Result(ValType::I32)),
                                I32Const(1),
                                Else,
                                LocalGet(right), LocalGet(hi), I32GeU,
                                If(BlockType::Result(ValType::I32)),
                                I32Const(0),
                                Else,
                            );
                            less(function);
                            wasm!(function; End, End, If(BlockType::Empty));
                            take(function, right);
                            wasm!(function; Else);
                            take(function, left);
                            wasm!(function; End);
                        },
                    );
                    wasm!(function; LocalGet(hi), LocalSet(lo));
                },
            );
            wasm!(function;
                LocalGet(src), LocalGet(dst), LocalSet(src), LocalSet(dst),
                LocalGet(width), I32Const(1), I32Shl, LocalSet(width),
            );
        },
    );
    // Write back the values, the `undefined`s, then delete up to `len`.
    let put = |function: &mut Function| {
        if copy {
            put_at(function, frame, (target, k, v, kv), strings, helpers);
        } else {
            set_at(function, frame, (o, k, v, kv), strings, helpers);
        }
    };
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k), I32Const(0), LocalSet(left));
    count_loop(function, left, pairs, |function| {
        pair_at(function, src, left);
        wasm!(function; I64Load(elem_slot(1)), LocalSet(v));
        put(function);
        bump(function, k, 1.0);
    });
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(v));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(undefs), F64Const(Ieee64::from(0.0)), F64Gt),
        |function| {
            put(function);
            bump(function, k, 1.0);
            bump(function, undefs, -1.0);
        },
    );
    if !copy {
        while_loop(
            function,
            |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
            |function| {
                delete_at(function, frame, (o, k, kv), strings, helpers);
                bump(function, k, 1.0);
            },
        );
    }
    wasm!(function; LocalGet(target));
}

// --- with / toReversed / toSpliced ----------------------------------------------------

/// `Get(o, f)` into i64 `v` for the f64 index in local `f` (`key` an i32
/// temp); a throwing getter returns.
fn get_at(function: &mut Function, o: u32, f: u32, key: u32, v: u32, helpers: Helpers<'_>) {
    num(function, f);
    wasm!(function; I32Const(1), Call(helpers.to_key), LocalSet(key));
    get_key(function, o, key, v, helpers);
}

/// `CreateDataPropertyOrThrow(a, f, v)` for the f64 index in local `f` (`kv`
/// an i64 temp).
fn put_at(
    function: &mut Function,
    frame: Frame,
    (a, f, v, kv): (u32, u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    num(function, f);
    wasm!(function; LocalSet(kv));
    create_data_property(function, frame, a, kv, v, strings, helpers);
}

/// `ArrayCreate(len)` into i64 `out` for the f64 length in local `len` (`kv`
/// an i64 temp): a length past `2^32 - 1` throws the `RangeError`.
fn array_create_f64(
    function: &mut Function,
    frame: Frame,
    (len, kv, out): (u32, u32, u32),
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    num(function, len);
    wasm!(function; LocalSet(kv));
    array_create(function, frame, kv, out, strings, helpers);
    return_if_pending(function);
}

/// `Array.prototype.with(index, value)`: `actualIndex` is `index` relative to
/// `len` (no clamping); outside `[0, len)` throws a `RangeError`; `A =
/// ArrayCreate(len)` gets `value` at `actualIndex` and `Get(O, k)` (holes
/// read `undefined`) everywhere else.
pub(super) fn emit_with(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, value, kv, index_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (copy, elem) = (frame.x64(0), frame.x64(1));
    let key = frame.i32(0);
    let r = frame.f64(0);
    let (len, k) = (frame.yf64(0), frame.yf64(1));
    frame.arg(function, 0);
    wasm!(function; LocalSet(index_v));
    frame.arg(function, 1);
    wasm!(function; LocalSet(value));
    this_object(function, frame, o, WITH.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    wasm!(function; LocalGet(index_v));
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Empty), LocalGet(len), LocalGet(r), F64Add, LocalSet(r), End,
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        LocalGet(r), LocalGet(len), F64Ge, I32Or,
        If(BlockType::Empty),
    );
    let error = IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(WITH_OUT_OF_RANGE[0].to_owned())),
        IrNode::Str(WITH_OUT_OF_RANGE[1].to_owned()),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
    wasm!(function; End);
    array_create_f64(function, frame, (len, kv, copy), strings, helpers);
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
        |function| {
            wasm!(function;
                LocalGet(k), LocalGet(r), F64Eq,
                If(BlockType::Empty), LocalGet(value), LocalSet(elem), Else,
            );
            get_at(function, o, k, key, elem, helpers);
            wasm!(function; End);
            put_at(function, frame, (copy, k, elem, kv), strings, helpers);
            bump(function, k, 1.0);
        },
    );
    wasm!(function; LocalGet(copy));
}

/// `Array.prototype.toReversed()`: `A = ArrayCreate(len)` gets `Get(O, len -
/// 1 - k)` at each `k` (a hole reads through the prototype chain).
pub(super) fn emit_to_reversed(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, a, v, kv) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let key = frame.i32(0);
    let (len, k, from) = (frame.yf64(0), frame.yf64(1), frame.zf64(0));
    this_object(function, frame, o, TO_REVERSED.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    array_create_f64(function, frame, (len, kv, a), strings, helpers);
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(k));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(k), LocalGet(len), F64Lt),
        |function| {
            wasm!(function;
                LocalGet(len), LocalGet(k), F64Sub, F64Const(Ieee64::from(1.0)), F64Sub,
                LocalSet(from),
            );
            get_at(function, o, from, key, v, helpers);
            put_at(function, frame, (a, k, v, kv), strings, helpers);
            bump(function, k, 1.0);
        },
    );
    wasm!(function; LocalGet(a));
}

/// `Array.prototype.toSpliced(start, skipCount, ...items)`: `actualStart` and
/// `actualSkipCount` as in `splice`; a new length past `2^53 - 1` throws a
/// `TypeError`, then `A = ArrayCreate(newLen)` (past `2^32 - 1` a
/// `RangeError`) gets `Get(O, i)` for `i < actualStart`, the items, then
/// `Get(O, i + actualSkipCount - itemCount)` up to `newLen` (holes read
/// `undefined`).
pub(super) fn emit_to_spliced(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, elem, kv, start_v) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (sc_v, saved, copy) = (frame.x64(0), frame.x64(1), frame.y64(0));
    let (key, arg_i, argn) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let r = frame.f64(0);
    let (len, start) = (frame.yf64(0), frame.yf64(1));
    let (sc, items, i) = (frame.zf64(0), frame.zf64(1), frame.zf64(2));
    let (from, new_len) = (frame.zf64(3), frame.zf64(4));
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(saved));
    frame.arg(function, 0);
    wasm!(function; LocalSet(start_v));
    frame.arg(function, 1);
    wasm!(function; LocalSet(sc_v));
    push_argc(function, frame);
    wasm!(function; LocalSet(argn));
    this_object(function, frame, o, TO_SPLICED.nullish, strings, helpers);
    length_of_f64(function, o, len, strings, helpers);
    relative_arg(function, start_v, false, r, len, start, helpers);
    wasm!(function;
        // itemCount = max(argc - 2, 0)
        LocalGet(argn), I32Const(2), I32Sub, I32Const(0), LocalGet(argn), I32Const(2), I32GtU, Select,
        F64ConvertI32U, LocalSet(items),
        LocalGet(argn), I32Eqz,
        If(BlockType::Empty),
        F64Const(Ieee64::from(0.0)), LocalSet(sc),
        Else,
        LocalGet(len), LocalGet(start), F64Sub, LocalSet(sc),
        LocalGet(argn), I32Const(1), I32GtU,
        If(BlockType::Empty),
        LocalGet(sc_v),
    );
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Max, LocalGet(sc), F64Min, LocalSet(sc),
        End,
        End,
    );
    check_new_length(
        function,
        frame,
        len,
        |function| wasm!(function; LocalGet(items), LocalGet(sc), F64Sub),
        strings,
        helpers,
    );
    wasm!(function;
        LocalGet(len), LocalGet(items), F64Add, LocalGet(sc), F64Sub, LocalSet(new_len),
    );
    array_create_f64(function, frame, (new_len, kv, copy), strings, helpers);
    wasm!(function; F64Const(Ieee64::from(0.0)), LocalSet(i));
    while_loop(
        function,
        |function| wasm!(function; LocalGet(i), LocalGet(start), F64Lt),
        |function| {
            get_at(function, o, i, key, elem, helpers);
            put_at(function, frame, (copy, i, elem, kv), strings, helpers);
            bump(function, i, 1.0);
        },
    );
    wasm!(function; I32Const(2), LocalSet(arg_i));
    count_loop(function, arg_i, argn, |function| {
        arg_at(function, frame, arg_i, saved, elem);
        put_at(function, frame, (copy, i, elem, kv), strings, helpers);
        bump(function, i, 1.0);
    });
    wasm!(function;
        LocalGet(start), LocalGet(sc), F64Add, LocalSet(from),
    );
    while_loop(
        function,
        |function| wasm!(function; LocalGet(i), LocalGet(new_len), F64Lt),
        |function| {
            get_at(function, o, from, key, elem, helpers);
            put_at(function, frame, (copy, i, elem, kv), strings, helpers);
            bump(function, i, 1.0);
            bump(function, from, 1.0);
        },
    );
    wasm!(function; LocalGet(copy));
}
