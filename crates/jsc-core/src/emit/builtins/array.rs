//! `%Array.prototype%` methods.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of `map` on an `undefined` / `null` receiver.
const MAP_NULLISH: CalleeErrorText<'static> = [
    "Array.prototype.map called on null or undefined",
    "TypeError: Array.prototype.map called on null or undefined",
];

/// The `TypeError` of `map` with a non-callable callback.
const MAP_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Array.prototype.map callback is not a function",
    "TypeError: Array.prototype.map callback is not a function",
];

/// The static strings of this family.
pub(super) const STRINGS: [&str; 4] = [
    MAP_NULLISH[0],
    MAP_NULLISH[1],
    MAP_NOT_CALLABLE[0],
    MAP_NOT_CALLABLE[1],
];

/// `Array.prototype.map(callback, thisArg)`, generic over array-likes:
/// `len = ToLength(this.length)`, then for each index `k < len` present on
/// `this` (`HasProperty`), `A[k] = callback.call(thisArg, this[k], k, this)`;
/// an absent index stays a hole of the fresh `A` (no array species).
pub(super) fn emit_map(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (o, callback, this_arg, value) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (len, dst, i, key) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_is_nullish(function, o);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, MAP_NULLISH, strings, helpers);
    wasm!(function; End);
    wasm!(function; LocalGet(o), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get));
    return_if_pending(function);
    // ToLength: NaN and negatives are 0; the store's u32 bounds the rest.
    wasm!(function; Call(helpers.to_number), F64ReinterpretI64, I32TruncSatF64U, LocalSet(len));
    frame.arg(function, 0);
    wasm!(function; LocalSet(callback));
    emit_is_function(function, callback);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, MAP_NOT_CALLABLE, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 1);
    wasm!(function;
        LocalSet(this_arg),
        LocalGet(len), Call(helpers.new_array()), LocalSet(dst),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(i), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
        LocalTee(key), LocalGet(o), Call(helpers.has),
        If(BlockType::Empty),
        LocalGet(o), LocalGet(key), Call(helpers.get), LocalSet(value),
    );
    return_if_pending(function);
    // callback(value, k, O) with `this = thisArg`.
    let k = frame.k;
    for slot in 0..k {
        match slot {
            0 => wasm!(function; LocalGet(value)),
            1 => wasm!(function; LocalGet(i), F64ConvertI32U, I64ReinterpretF64),
            2 => wasm!(function; LocalGet(o)),
            _ => wasm!(function; I64Const(UNDEFINED_BITS)),
        }
    }
    wasm!(function; LocalGet(this_arg), GlobalSet(THIS_GLOBAL));
    let argc = 3.min(k as usize);
    invoke(
        function,
        callback,
        |function| emit_argc(argc, function),
        helpers,
    );
    wasm!(function; LocalSet(value));
    return_if_pending(function);
    wasm!(function; LocalGet(dst));
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, LocalGet(value), I64Store(elem_slot(0)),
        Else,
        LocalGet(dst),
    );
    emit_array_elems(function);
    wasm!(function;
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Const(HOLE_BITS), I64Store(elem_slot(0)),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}
