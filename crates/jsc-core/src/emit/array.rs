#[allow(clippy::wildcard_imports)]
use super::*;

/// Load the element-store base of the array handle whose `i32` base is on the
/// stack (its `elems` slot); element `i` is then the jsval at
/// `store + ELEMS_DATA_OFFSET + 8*i`.
pub(super) fn emit_array_elems(function: &mut Function) {
    function.instruction(&Instruction::I32Load(MemArg {
        offset: ARRAY_ELEMS_OFFSET,
        align: 2,
        memory_index: 0,
    }));
}

/// The [`MemArg`] of element `k` of an element store (the store base on the
/// stack, plus any `8*i` already added).
pub(super) fn elem_slot(k: u64) -> MemArg {
    MemArg {
        offset: ELEMS_DATA_OFFSET + 8 * k,
        align: 3,
        memory_index: 0,
    }
}

/// Push the element jsval in i64 local `elem` as JS reads it: a [`HOLE_BITS`]
/// slot reads `undefined` (`elem ^ (elem == HOLE)`, since the hole is
/// `undefined | 1`); any other element is itself. Every element read that can
/// meet a hole goes through here.
// S4 adds the hole read's proto continuation (an inherited index on a dirty
// `Array.prototype`/`Object.prototype`); until then a hole is plainly `undefined`.
pub(super) fn emit_elem_read(function: &mut Function, elem: u32) {
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Xor);
}

/// Push i32 nonzero when the array handle whose i32 base is in local `arr` is
/// `SPARSE` (its header `flags` bit), else `0`.
pub(super) fn emit_is_sparse(function: &mut Function, arr: u32) {
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(ARRAY_SPARSE));
    function.instruction(&Instruction::I32And);
}

/// Push the raw element jsval `idx` (an i32 local below the length) of the
/// array whose i32 base is in local `arr`, a hole as [`HOLE_BITS`]: the store
/// slot of a dense array, `__sparse_get` (function index `sparse_get`) of a
/// `SPARSE` one. The store base is reloaded per access, since a callback may
/// grow the array (replacing its store).
pub(super) fn emit_elem_load(function: &mut Function, arr: u32, idx: u32, sparse_get: u32) {
    emit_is_sparse(function, arr);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::Call(sparse_get));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(arr));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::End);
}

/// The [`MemArg`] of an array handle's u32 `length`.
pub(super) fn array_len_slot() -> MemArg {
    MemArg {
        offset: ARRAY_LEN_OFFSET,
        align: 2,
        memory_index: 0,
    }
}

/// Emit an array literal `[e0, e1, ..]`, leaving the array jsval (single heap tag
/// `0b101`, type byte `TYPE_ARRAY`) on the stack. A non-empty literal first
/// builds its `cap = n` element store (`__alloc(8 + 8*n)`, `TYPE_ELEMS`, `cap`,
/// then each element jsval at `store + 8 + 8*k`), then the handle pointing at
/// it; an empty literal is `__new_array(0)` (no store).
///
/// The store base is kept on the operand stack (never in a persistent local)
/// while the elements are evaluated, so nested array literals compose: an
/// element expression may itself build an array, clobbering the transient i32
/// scratch locals, without disturbing this store's base sitting beneath it.
pub(super) fn emit_array_node(
    elems: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The two transient i32 scratch locals: `dst` duplicates the block base
    // before each write; `store` holds the finished store while the handle is
    // built (no element expression runs after that).
    let dst = scratch + 3;
    let store = scratch + 7;
    if elems.is_empty() {
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::Call(helpers.new_array()));
    } else {
        let len = i32::try_from(elems.len()).unwrap_or(i32::MAX);
        // store = __alloc(8 + 8*n) with its type byte and cap; left on the stack.
        function.instruction(&Instruction::I32Const(
            abi::ELEMS_DATA_OFFSET
                .cast_signed()
                .saturating_add(len.saturating_mul(8)),
        ));
        function.instruction(&Instruction::Call(helpers.alloc));
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(TYPE_ELEMS));
        function.instruction(&Instruction::I32Store8(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(len));
        function.instruction(&Instruction::I32Store(MemArg {
            offset: ELEMS_CAP_OFFSET,
            align: 2,
            memory_index: 0,
        }));
        // Each element jsval at store + 8 + 8*k, left-to-right.
        for (k, elem) in elems.iter().enumerate() {
            function.instruction(&Instruction::LocalTee(dst));
            function.instruction(&Instruction::LocalGet(dst));
            emit_node(elem, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Store(elem_slot(k as u64)));
        }
        // The handle: object prefix, `len = n`, `elems = store`.
        function.instruction(&Instruction::LocalSet(store));
        function.instruction(&Instruction::I32Const(ARRAY_BLOCK_SIZE));
        function.instruction(&Instruction::Call(helpers.alloc));
        function.instruction(&Instruction::LocalSet(dst));
        emit_object_prefix(
            function,
            dst,
            TYPE_ARRAY,
            intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Array), helpers),
        );
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::I32Const(len));
        function.instruction(&Instruction::I32Store(array_len_slot()));
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::LocalGet(store));
        function.instruction(&Instruction::I32Store(MemArg {
            offset: ARRAY_ELEMS_OFFSET,
            align: 2,
            memory_index: 0,
        }));
        function.instruction(&Instruction::LocalGet(dst));
    }

    // Rebox the handle base (on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an array literal with a spread `[0, ...a, 9]`, leaving the built
/// array's heap jsval on the stack. Because a spread source's length is only
/// known at runtime, the array is grown in place: a fresh empty array
/// (`__new_array(0)`), then each part is appended left-to-right — a plain
/// [`Item`] via `__array_push`, a [`Spread`] source's elements via
/// `__array_concat`.
///
/// The handle base is kept purely on the operand stack (both runtime helpers
/// take it as their deepest argument and return it), so a nested spread array
/// in any part composes without a persistent local.
///
/// [`Item`]: SpreadElem::Item
/// [`Spread`]: SpreadElem::Spread
pub(super) fn emit_array_spread(
    parts: &[SpreadElem],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // base = a fresh empty array, left (i32) on the operand stack.
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.new_array()));

    // Append each part; `base` stays on the stack, the source/value sits above it.
    for part in parts {
        match part {
            SpreadElem::Item(value) => {
                // base = __array_push(base, value).
                emit_node(value, function, scratch, strings, helpers);
                function.instruction(&Instruction::Call(helpers.array_push));
            }
            SpreadElem::Spread(source) => {
                // base = __array_concat(base, wrap(elements)). The spread source is
                // normalized to an array first: a STRING is exploded into a fresh
                // array of one-char strings via `__str_to_array` (so `[...str]`
                // spreads its characters instead of reinterpreting UTF-8 bytes as
                // jsvals); a GENERATOR is drained via `__gen_to_array`,
                // selected by an `IsGenerator` test; any other source (an array) is
                // passed straight through. `base` (i32) stays beneath on the
                // operand stack throughout.
                emit_node(source, function, scratch, strings, helpers);
                function.instruction(&Instruction::LocalSet(scratch + 22));
                emit_is_string(function, scratch + 22);
                function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                function.instruction(&Instruction::LocalGet(scratch + 22));
                function.instruction(&Instruction::Call(helpers.str_to_array));
                function.instruction(&Instruction::Else);
                emit_is_generator(function, scratch + 22);
                function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                function.instruction(&Instruction::LocalGet(scratch + 22));
                function.instruction(&Instruction::Call(helpers.gen_to_array));
                function.instruction(&Instruction::Else);
                function.instruction(&Instruction::LocalGet(scratch + 22));
                function.instruction(&Instruction::End);
                function.instruction(&Instruction::End);
                function.instruction(&Instruction::I32WrapI64);
                function.instruction(&Instruction::Call(helpers.array_concat));
            }
        }
    }

    // Rebox the finished block's base offset (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit `array.push(value)`, leaving the new length (a number jsval) on
/// the stack: `__array_push` appends `value` to the array handle read from
/// `local` in place (the handle never moves, so every alias observes the
/// growth) and returns the handle, whose `length` is then read back.
pub(super) fn emit_array_push_node(
    local: u32,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit `array.pop()`, leaving the removed last element (or `undefined`
/// for an empty array or a trailing hole) on the stack: the array handle read
/// from `local` is shrunk IN PLACE — its `length`
/// drops by one and the element at the old last slot of its store is yielded
/// (a `SPARSE` array reads it through `__sparse_get` and shrinks through
/// `__array_set_length`, which drops it from the property table).
pub(super) fn emit_array_pop_node(
    local: u32,
    function: &mut Function,
    scratch: u32,
    helpers: Helpers<'_>,
) {
    // Transient scratch locals: the handle base and the new length (i32), and
    // a sparse array's removed element (i64).
    let base = scratch + 3;
    let new_len = scratch + 7;
    let removed = scratch + 4;

    // base = wrap(array_handle).
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // len == 0 ? undefined : pop the last element.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    // A SPARSE array: removed = __sparse_get(base, len - 1), then the `length`
    // write `len - 1` drops that element; a hole reads `undefined`.
    emit_is_sparse(function, base);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalTee(new_len));
    function.instruction(&Instruction::Call(helpers.sparse_get()));
    function.instruction(&Instruction::LocalSet(removed));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::Call(helpers.array_set_length()));
    emit_elem_read(function, removed);
    function.instruction(&Instruction::Else);
    // new_len = len - 1; write it back into the handle.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalTee(new_len));
    function.instruction(&Instruction::I32Store(array_len_slot()));
    // removed = the element jsval at store + 8 + 8*new_len (`base` now holds
    // that address), a hole read as `undefined` (`x ^ (x == HOLE)`).
    function.instruction(&Instruction::LocalGet(base));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalTee(base));
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Xor);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Push the raw element jsval at `arr[idx]` (a hole as [`HOLE_BITS`]) onto the
/// stack via [`emit_elem_load`]. `arr`/`idx` are the array-handle-base /
/// loop-index i32 scratch locals of an [`emit_array_hof`] loop.
pub(super) fn emit_hof_load_elem(
    function: &mut Function,
    arr: u32,
    idx: u32,
    helpers: Helpers<'_>,
) {
    emit_elem_load(function, arr, idx, helpers.sparse_get());
}

/// Push the loop index `idx` (an i32 scratch local) as a number jsval — its
/// `f64` bit pattern in the i64 carrier.
fn emit_hof_index(function: &mut Function, idx: u32) {
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit the trailing half of a higher-order callback invocation: pad the
/// remaining positional slots to the shared funcref arity with `undefined`, set
/// `this` (from i64 local `this`, else `undefined`), push the callback's own
/// environment (at `+FUNC_ENV_OFFSET`) as the env-last argument and its
/// funcref-table index (at `+FUNC_INDEX_OFFSET`), and `call_indirect`. The
/// real positional args (already pushed by the caller) number at most
/// `helpers.max_args`. Leaves the callback's boxed jsval result.
fn emit_hof_invoke(
    function: &mut Function,
    helpers: Helpers<'_>,
    cb: u32,
    this: Option<u32>,
    real_args: u32,
) {
    for _ in real_args..helpers.max_args {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // The real positional args are already on the stack, so setting the
    // carrier here (stack-neutral) does not disturb them.
    match this {
        Some(this) => {
            function.instruction(&Instruction::LocalGet(this));
            function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
        }
        None => emit_reset_this(function),
    }
    function.instruction(&Instruction::LocalGet(cb));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI): the real positional args the callback
    // receives, so a rest-parameter callback collects exactly those.
    emit_argc(real_args as usize, function);
    function.instruction(&Instruction::LocalGet(cb));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // A higher-order callback can throw; unwind if it left an exception pending.
    // `return` exits the whole function regardless of the enclosing HOF loop's
    // block nesting, so an in-flight exception cleanly abandons the loop.
    emit_propagate_if_pending(function, helpers);
}

/// The `TypeError` of `reduce` over no element without an initial value.
pub(super) const REDUCE_EMPTY: CalleeErrorText<'static> = [
    "Reduce of empty array with no initial value",
    "TypeError: Reduce of empty array with no initial value",
];

/// Emit an array higher-order method ([`IrNode::ArrayHof`]), leaving the
/// method's result jsval on the stack. The receiver is evaluated once into
/// the i32 base scratch local `arr` (not rebound), its length on entry into
/// `len`, the callback's base into `cb` (after its `IsCallable` check) and the
/// `this_arg` into `this`; a counted loop over `0..len` then invokes the
/// callback per element still present (below the current length, not a hole)
/// through the uniform `call_indirect` ABI via [`emit_hof_invoke`], passing
/// `(element, index, array)` (`reduce`: `(acc, element, index, array)`)
/// truncated to `K`:
///
/// - `Map` fills a fresh same-length array `res[i] = fn(..)`, a skipped index
///   staying a hole.
/// - `Filter` grows a fresh array in place via `__array_push` for JS-truthy
///   results.
/// - `Reduce` folds `acc = fn(acc, ..)` from `init`, or without one from the
///   first present element (`seeded` tracks it; none throws [`REDUCE_EMPTY`]).
/// - `ForEach` discards each result and yields `undefined`.
///
/// The sub-expressions are fully evaluated before the loop seeds the HOF
/// scratch locals (`scratch+8`..`scratch+14`), so a nested HOF in any of them
/// completes and frees those locals first — the loop body itself only reads
/// memory and `call_indirect`s, never re-entering an inner HOF at this frame.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn emit_array_hof(
    op: ArrayHofOp,
    array: &IrNode,
    callback: &IrNode,
    init: Option<&IrNode>,
    this_arg: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let arr = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let cb = scratch + 11;
    // The result array of map/filter; reduce's `seeded` flag.
    let res = scratch + 12;
    let seeded = res;
    let acc = scratch + 13;
    let this = scratch + 14;

    // Prologue: receiver base -> `arr`, its length -> `len`, then the
    // callback (IsCallable, DF4: a non-function throws before any iteration,
    // even over an empty array; a function literal needs no check), `this`
    // and the initial accumulator.
    emit_node(array, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalSet(len));
    emit_node(callback, function, scratch, strings, helpers);
    if !is_static_function(callback) {
        function.instruction(&Instruction::LocalSet(scratch));
        emit_callable_guard(
            function,
            EXPRESSION_NOT_A_FUNCTION,
            scratch,
            scratch,
            strings,
            helpers,
        );
        function.instruction(&Instruction::LocalGet(scratch));
    }
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(cb));
    if let Some(this_arg) = this_arg {
        emit_node(this_arg, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(this));
    }
    let this = this_arg.map(|_| this);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(idx));

    // Pre-loop setup: allocate the result array (map/filter) or seed the
    // accumulator (reduce).
    match op {
        ArrayHofOp::Map => {
            // res = __new_array(len): a same-length array with a `cap = len` store.
            function.instruction(&Instruction::LocalGet(len));
            function.instruction(&Instruction::Call(helpers.new_array()));
            function.instruction(&Instruction::LocalSet(res));
        }
        ArrayHofOp::Filter => {
            // res = a fresh empty array (grown in place per truthy element by
            // __array_push).
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::Call(helpers.new_array()));
            function.instruction(&Instruction::LocalSet(res));
        }
        ArrayHofOp::Reduce => {
            if let Some(init) = init {
                emit_node(init, function, scratch, strings, helpers);
                function.instruction(&Instruction::LocalSet(acc));
            } else {
                function.instruction(&Instruction::I32Const(0));
                function.instruction(&Instruction::LocalSet(seeded));
            }
        }
        ArrayHofOp::ForEach => {}
    }
    // Push the callback's argument `j` of `(element, index, array)`.
    let callback_arg = |function: &mut Function, j: u32| match j {
        0 => emit_hof_load_elem(function, arr, idx, helpers),
        1 => emit_hof_index(function, idx),
        _ => {
            function.instruction(&Instruction::LocalGet(arr));
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(HEAP_BITS));
            function.instruction(&Instruction::I64Or);
        }
    };
    let call = |function: &mut Function, before: u32| {
        let n = (before + 3).min(helpers.max_args);
        for j in before..n {
            callback_arg(function, j - before);
        }
        emit_hof_invoke(function, helpers, cb, this, n);
    };

    // Counted loop: `while (idx < len) { <body>; idx++ }`.
    function.instruction(&Instruction::Block(BlockType::Empty));
    enter_frame(helpers); // outer block
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers); // loop — the callback's throw check sits inside both
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // Visit a present element only: below the current length (a callback may
    // shrink the array) and not a hole.
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_hof_load_elem(function, arr, idx, helpers);
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);

    match op {
        ArrayHofOp::Map => {
            // res[idx] = callback(..). The slot address (res.elems + 8*idx) is
            // pushed first and survives beneath the call_indirect, which is
            // stack-balanced (leaves one i64); `res` is private to the loop,
            // so its store never moves.
            function.instruction(&Instruction::LocalGet(res));
            emit_array_elems(function);
            function.instruction(&Instruction::LocalGet(idx));
            function.instruction(&Instruction::I32Const(8));
            function.instruction(&Instruction::I32Mul);
            function.instruction(&Instruction::I32Add);
            call(function, 0);
            function.instruction(&Instruction::I64Store(elem_slot(0)));
        }
        ArrayHofOp::Filter => {
            // if truthy(callback(..)) { __array_push(res, element) }.
            call(function, 0);
            function.instruction(&Instruction::LocalSet(acc));
            emit_cond(&IrNode::Local(acc), function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::LocalGet(res));
            emit_hof_load_elem(function, arr, idx, helpers);
            function.instruction(&Instruction::Call(helpers.array_push));
            function.instruction(&Instruction::Drop);
            function.instruction(&Instruction::End);
        }
        ArrayHofOp::Reduce => {
            // acc = callback(acc, ..), or the first present element seeds it.
            if init.is_none() {
                function.instruction(&Instruction::LocalGet(seeded));
                function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                enter_frame(helpers);
            }
            if helpers.max_args > 0 {
                function.instruction(&Instruction::LocalGet(acc));
            }
            call(function, 1);
            if init.is_none() {
                function.instruction(&Instruction::Else);
                emit_hof_load_elem(function, arr, idx, helpers);
                function.instruction(&Instruction::I32Const(1));
                function.instruction(&Instruction::LocalSet(seeded));
                leave_frame(helpers);
                function.instruction(&Instruction::End);
            }
            function.instruction(&Instruction::LocalSet(acc));
        }
        ArrayHofOp::ForEach => {
            // callback(..) for side effects; discard the result.
            call(function, 0);
            function.instruction(&Instruction::Drop);
        }
    }
    if op == ArrayHofOp::Map {
        // A skipped index stays a hole.
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::LocalGet(res));
        emit_array_elems(function);
        function.instruction(&Instruction::LocalGet(idx));
        function.instruction(&Instruction::I32Const(8));
        function.instruction(&Instruction::I32Mul);
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::I64Const(HOLE_BITS));
        function.instruction(&Instruction::I64Store(elem_slot(0)));
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // idx++ and continue.
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(idx));
    function.instruction(&Instruction::Br(0));
    leave_frame(helpers); // loop
    function.instruction(&Instruction::End); // loop
    leave_frame(helpers); // block
    function.instruction(&Instruction::End); // block

    if op == ArrayHofOp::Reduce && init.is_none() {
        function.instruction(&Instruction::LocalGet(seeded));
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::If(BlockType::Empty));
        enter_frame(helpers);
        emit_throw_not_a_function(function, REDUCE_EMPTY, scratch, strings, helpers);
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }

    // Result: the fresh array (map/filter), the accumulator (reduce), or
    // `undefined` (forEach).
    match op {
        ArrayHofOp::Map | ArrayHofOp::Filter => {
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(HEAP_BITS));
            function.instruction(&Instruction::I64Or);
        }
        ArrayHofOp::Reduce => {
            function.instruction(&Instruction::LocalGet(acc));
        }
        ArrayHofOp::ForEach => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
    }
}
