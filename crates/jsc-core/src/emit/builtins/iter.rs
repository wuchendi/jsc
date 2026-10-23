//! `GetIterator` of the iteration sources walked by index
//! ([`IrNode::IterSource`]): for-of, `yield*`, spread and array
//! destructuring.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The [`Builtin::IterSource`] body: `this` is the iterated value, `env` is
/// `1` for a live walk. An array is itself; a string (or a `String`
/// wrapper's string) is itself when live, else its characters; a generator is
/// drained; a `Map` / `Set` is itself when live, else a fresh array of its
/// `[key, value]` entries / values; an `arguments` object is copied by
/// `Array.prototype.slice`; anything else throws `<x> is not iterable`.
pub(super) fn emit_iter_source(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (value, item, key, entry) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let (i, at, dst) = (frame.i32(0), frame.i32(1), frame.i32(2));
    let live = frame.k;
    let kind = |function: &mut Function, kind: i32| {
        emit_load_type_byte(function, value);
        wasm!(function; I32Const(kind), I32Eq);
    };
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(value));
    // A `String` wrapper iterates its string.
    emit_is_wrapper(function, value, abi::CLASS_STRING);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(value), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::WRAPPER_VALUE_OFFSET), align: 3, memory_index: 0 }),
        LocalSet(value),
        End,
    );
    emit_is_pointer(function, value);
    wasm!(function; If(BlockType::Empty));
    kind(function, TYPE_ARRAY);
    wasm!(function; If(BlockType::Empty), LocalGet(value), Return, End);
    kind(function, TYPE_STRING);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(value),
        LocalGet(live), I32WrapI64, I32Eqz,
        If(BlockType::Empty), LocalGet(value), Call(helpers.str_to_array), Return, End,
        Return,
        End,
    );
    // A (sync) generator is drained; an async generator is not iterable.
    emit_is_generator(function, value);
    wasm!(function; If(BlockType::Empty), LocalGet(value), Call(helpers.gen_to_array), Return, End);
    kind(function, TYPE_MAP);
    kind(function, TYPE_SET);
    wasm!(function;
        I32Or, If(BlockType::Empty),
        LocalGet(live), I32WrapI64, If(BlockType::Empty), LocalGet(value), Return, End,
        I32Const(0), Call(helpers.new_array()), LocalSet(dst),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i),
    );
    map_set::entries_len(function, value);
    wasm!(function; I32GeU, BrIf(1));
    map_set::load_entry(function, value, i, at, key, item);
    kind(function, TYPE_MAP);
    wasm!(function; If(BlockType::Result(ValType::I64)));
    let pair = IrNode::Array(vec![IrNode::Local(key), IrNode::Local(item)]);
    emit_node(&pair, function, frame.scratch(), strings, helpers);
    wasm!(function;
        Else, LocalGet(key), End, LocalSet(entry),
        LocalGet(dst), LocalGet(entry), Call(helpers.array_push), Drop,
        LocalGet(i), I32Const(2), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Return,
        End,
    );
    // An `arguments` object: `Array.prototype.slice.call(arguments)`.
    emit_is_wrapper(function, value, abi::CLASS_ARGUMENTS);
    wasm!(function; If(BlockType::Empty));
    for _ in 0..frame.k {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function; LocalGet(value), GlobalSet(THIS_GLOBAL), I64Const(0));
    emit_argc(0, function);
    wasm!(function;
        Call(helpers.builtin(Builtin::ArraySlice)), Return,
        End,
        End,
    );
    emit_drop(
        function,
        frame,
        &map_set::not_iterable(value),
        strings,
        helpers,
    );
    wasm!(function; I64Const(UNDEFINED_BITS));
}
