//! `%Array.prototype%` methods. Each body is generic over array-likes
//! (`ToObject(this)`, `LengthOfArrayLike`, then `HasProperty` / `Get` / `Set`
//! by index key) and runs the inline fast path of its call sites (design 3.2)
//! for a real array receiver.

#[allow(clippy::wildcard_imports)]
use super::*;

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
        .chain([JOIN, COMMA, ""])
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

/// Emit `node` in the body's scratch layout, dropping its value.
fn emit_drop(
    function: &mut Function,
    frame: Frame,
    node: &IrNode,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(node, function, frame.scratch(), strings, helpers);
    wasm!(function; Drop);
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

/// Push i32 `1` when the jsval in `o` is an array handle.
fn is_array(function: &mut Function, o: u32) {
    emit_is_pointer(function, o);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, o);
    wasm!(function; I32Const(TYPE_ARRAY), I32Eq, Else, I32Const(0), End);
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

/// Push the argument count (`argc`, a number jsval) as an i32.
fn argc(function: &mut Function, frame: Frame) {
    wasm!(function; LocalGet(frame.argc()), F64ReinterpretI64, I32TruncSatF64U);
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

/// `while i < n { body; i += 1 }` over i32 locals.
fn count_loop(function: &mut Function, i: u32, n: u32, body: impl FnOnce(&mut Function)) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    body(function);
    wasm!(function;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );
}

/// Argument `i` (an i32 local) into i64 `out`: from the spread argument
/// array when the call passed more than `K` arguments, else from slot `i`
/// (`undefined` past the slots); a spread hole reads `undefined`.
fn arg_at(function: &mut Function, frame: Frame, i: u32, out: u32) {
    let k = frame.k;
    argc(function, frame);
    wasm!(function;
        I32Const(k.cast_signed()), I32GtU,
        If(BlockType::Result(ValType::I64)),
        GlobalGet(SPREAD_ARGS_GLOBAL), I32WrapI64,
    );
    emit_array_elems(function);
    wasm!(function; LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), Else);
    for j in 0..k {
        wasm!(function;
            LocalGet(i), I32Const(j.cast_signed()), I32Eq,
            If(BlockType::Result(ValType::I64)), LocalGet(j), Else,
        );
    }
    wasm!(function; I64Const(UNDEFINED_BITS));
    for _ in 0..k {
        wasm!(function; End);
    }
    wasm!(function; End, LocalSet(out));
    emit_elem_read(function, out);
    wasm!(function; LocalSet(out));
}

/// `ToIntegerOrInfinity` of the jsval on the stack into f64 `r` (a throwing
/// `valueOf` returns).
fn to_integer(function: &mut Function, r: u32, helpers: Helpers<'_>) {
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function; F64ReinterpretI64, F64Trunc, LocalSet(r));
}

/// Push the index the relative position in f64 `r` designates in a length
/// `len` (i32): `max(len + r, 0)` when `r < 0`, else `min(r, len)` (NaN is 0).
fn relative_index(function: &mut Function, r: u32, len: u32) {
    wasm!(function;
        LocalGet(r), F64Const(Ieee64::from(0.0)), F64Lt,
        If(BlockType::Result(ValType::F64)),
        LocalGet(len), F64ConvertI32U, LocalGet(r), F64Add, F64Const(Ieee64::from(0.0)), F64Max,
        Else,
        LocalGet(r), LocalGet(len), F64ConvertI32U, F64Min,
        End,
        I32TruncSatF64U,
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
    let (n, i) = (frame.i32(0), frame.i32(1));
    this_object(function, frame, o, PUSH.nullish, strings, helpers);
    argc(function, frame);
    wasm!(function; LocalSet(n), I32Const(0), LocalSet(i));
    is_array(function, o);
    wasm!(function; If(BlockType::Empty));
    count_loop(function, i, n, |function| {
        wasm!(function; LocalGet(o), I32WrapI64);
        arg_at(function, frame, i, v);
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
        arg_at(function, frame, i, v);
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
    let len = frame.i32(0);
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
    length_of(function, o, len, strings, helpers);
    wasm!(function;
        LocalGet(len), I32Eqz,
        If(BlockType::Empty),
        I64Const(0.0f64.to_bits().cast_signed()), LocalSet(kv),
    );
    emit_drop(function, frame, &set(o, length(), kv), strings, helpers);
    wasm!(function; I64Const(UNDEFINED_BITS), Return, End);
    wasm!(function;
        LocalGet(len), I32Const(1), I32Sub, F64ConvertI32U, I64ReinterpretF64, LocalSet(kv),
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

/// `Array.prototype.map/filter/reduce/forEach(callback, thisArg | initial)`.
/// An array runs the call sites' [`IrNode::ArrayHof`] loop. Any other object:
/// `len = ToLength(this.length)`, the `IsCallable` check, then for each index
/// `k < len` present on `this` (`HasProperty`), the callback over
/// `(this[k], k, this)` with `this = thisArg` (`reduce`: over
/// `(acc, this[k], k, this)` with `this = undefined`, the accumulator seeded
/// from the initial value or the first present element); `map` leaves an
/// absent index a hole of its fresh result (no array species).
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
    let (len, k, dst, seeded) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let key = frame.x32(0);
    let reduce = op == ArrayHofOp::Reduce;
    this_object(function, frame, o, texts.nullish, strings, helpers);
    // `reduce` has an initial value when called with two arguments or more.
    let has_init = |function: &mut Function| {
        wasm!(function;
            LocalGet(frame.argc()), F64ReinterpretI64, F64Const(Ieee64::from(2.0)), F64Ge,
        );
    };
    let extra = || Some(Box::new(arg(frame.k, 1)));
    is_array(function, o);
    wasm!(function; If(BlockType::Empty));
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
    length_of(function, o, len, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(callback));
    emit_is_function(function, callback);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, texts.not_callable, strings, helpers);
    wasm!(function; End);
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
    match op {
        ArrayHofOp::Map => wasm!(function; LocalGet(len), Call(helpers.new_array()), LocalSet(dst)),
        ArrayHofOp::Filter => {
            wasm!(function; I32Const(0), Call(helpers.new_array()), LocalSet(dst));
        }
        ArrayHofOp::Reduce | ArrayHofOp::ForEach => {}
    }
    // The callback over `(acc,) value, k, O` with `this = thisArg`, its
    // result in `result`.
    let call = |function: &mut Function| {
        let args: &[u32] = if reduce {
            &[acc, value, k, o]
        } else {
            &[value, k, o]
        };
        for slot in 0..frame.k {
            match args.get(slot as usize) {
                Some(&local) if local == k => index_value(function, k),
                Some(&local) => wasm!(function; LocalGet(local)),
                None => wasm!(function; I64Const(UNDEFINED_BITS)),
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
        wasm!(function; LocalSet(result));
        return_if_pending(function);
    };
    wasm!(function; I32Const(0), LocalSet(k));
    count_loop(function, k, len, |function| {
        has_index(function, o, k, key, helpers);
        wasm!(function; If(BlockType::Empty));
        wasm!(function; LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(value));
        return_if_pending(function);
        match op {
            ArrayHofOp::Map => {
                call(function);
                wasm!(function; LocalGet(dst));
                emit_array_elems(function);
                wasm!(function;
                    LocalGet(k), I32Const(3), I32Shl, I32Add, LocalGet(result), I64Store(elem_slot(0)),
                    Else,
                    LocalGet(dst),
                );
                emit_array_elems(function);
                wasm!(function;
                    LocalGet(k), I32Const(3), I32Shl, I32Add, I64Const(HOLE_BITS),
                    I64Store(elem_slot(0)),
                );
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
                wasm!(function;
                    If(BlockType::Empty),
                    LocalGet(dst), LocalGet(value), Call(helpers.array_push), Drop,
                    End,
                );
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
        ArrayHofOp::Map | ArrayHofOp::Filter => {
            wasm!(function; LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
        }
        ArrayHofOp::Reduce => {
            wasm!(function; LocalGet(seeded), I32Eqz, If(BlockType::Empty));
            throw_type_error(function, frame, REDUCE_EMPTY, strings, helpers);
            wasm!(function; End, LocalGet(acc));
        }
        ArrayHofOp::ForEach => wasm!(function; I64Const(UNDEFINED_BITS)),
    }
}

/// `Array.prototype.indexOf(search, fromIndex)` (`includes`: `includes`).
/// An array without a `fromIndex` runs the call sites' host search. Any
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
    let (len, k, key) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let r = frame.f64(0);
    let (op, not_found) = if includes {
        (abi::METHOD_OP_INCLUDES, FALSE_BITS)
    } else {
        (abi::METHOD_OP_INDEX_OF, (-1.0f64).to_bits().cast_signed())
    };
    this_object(function, frame, o, texts.nullish, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(search));
    is_array(function, o);
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
    length_of(function, o, len, strings, helpers);
    wasm!(function; LocalGet(len), I32Eqz, If(BlockType::Empty), I64Const(not_found), Return, End);
    frame.arg(function, 1);
    to_integer(function, r, helpers);
    wasm!(function;
        LocalGet(r), LocalGet(len), F64ConvertI32U, F64Ge,
        If(BlockType::Empty), I64Const(not_found), Return, End,
    );
    relative_index(function, r, len);
    wasm!(function; LocalSet(k));
    count_loop(function, k, len, |function| {
        if includes {
            get_index(function, o, k, key, v, helpers);
            wasm!(function;
                LocalGet(v), LocalGet(search), Call(helpers.same_value_zero),
                If(BlockType::Empty), I64Const(TRUE_BITS), Return, End,
            );
        } else {
            has_index(function, o, k, key, helpers);
            wasm!(function;
                If(BlockType::Empty),
                LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(v),
            );
            return_if_pending(function);
            emit_node(
                &strict_eq(v, search),
                function,
                frame.scratch(),
                strings,
                helpers,
            );
            wasm!(function; I64Const(TRUE_BITS), I64Eq, If(BlockType::Empty));
            index_value(function, k);
            wasm!(function; Return, End, End);
        }
    });
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

/// `Array.prototype.slice(start, end)`: `k` and `final` from the relative
/// `start` and `end` (`len` when `undefined`); an array then runs the call
/// sites' host slice over `[k, final)`; any other receiver copies each present
/// index of `[k, final)` into a fresh array (an absent one stays a hole).
pub(super) fn emit_slice(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, value) = (frame.i64(0), frame.i64(1));
    let (len, k, fin, n) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (dst, key) = (frame.x32(0), frame.x32(1));
    let rel = frame.f64(0);
    this_object(function, frame, o, SLICE.nullish, strings, helpers);
    length_of(function, o, len, strings, helpers);
    frame.arg(function, 0);
    to_integer(function, rel, helpers);
    relative_index(function, rel, len);
    wasm!(function; LocalSet(k));
    frame.arg(function, 1);
    wasm!(function;
        LocalTee(value), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(len),
        Else,
        LocalGet(value),
    );
    to_integer(function, rel, helpers);
    relative_index(function, rel, len);
    wasm!(function; End, LocalSet(fin));
    is_array(function, o);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(o), I32Const(abi::METHOD_OP_SLICE),
    );
    index_value(function, k);
    index_value(function, fin);
    wasm!(function;
        Call(helpers.method), Return,
        End,
        // count = max(final - k, 0)
        LocalGet(fin), LocalGet(k), I32Sub, I32Const(0), LocalGet(fin), LocalGet(k), I32GtU, Select,
        Call(helpers.new_array()), LocalSet(dst),
        I32Const(0), LocalSet(n),
    );
    count_loop(function, k, fin, |function| {
        has_index(function, o, k, key, helpers);
        wasm!(function;
            If(BlockType::Result(ValType::I64)),
            LocalGet(o), LocalGet(key), Call(helpers.get),
        );
        return_if_pending(function);
        wasm!(function;
            Else, I64Const(HOLE_BITS), End,
            LocalSet(value),
            LocalGet(dst),
        );
        emit_array_elems(function);
        wasm!(function;
            LocalGet(n), I32Const(3), I32Shl, I32Add, LocalGet(value), I64Store(elem_slot(0)),
            LocalGet(n), I32Const(1), I32Add, LocalSet(n),
        );
    });
    wasm!(function; LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
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
