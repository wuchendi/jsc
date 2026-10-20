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
        emit_object_prefix(function, dst, TYPE_ARRAY);
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
                // operand stack throughout. When the program declares no functions
                // no generator can exist, so that test is skipped.
                emit_node(source, function, scratch, strings, helpers);
                function.instruction(&Instruction::LocalSet(scratch + 22));
                emit_is_string(function, scratch + 22);
                function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                function.instruction(&Instruction::LocalGet(scratch + 22));
                function.instruction(&Instruction::Call(helpers.str_to_array));
                function.instruction(&Instruction::Else);
                if helpers.has_funcs {
                    emit_is_generator(function, scratch + 22);
                    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
                    function.instruction(&Instruction::LocalGet(scratch + 22));
                    function.instruction(&Instruction::Call(helpers.gen_to_array));
                    function.instruction(&Instruction::Else);
                    function.instruction(&Instruction::LocalGet(scratch + 22));
                    function.instruction(&Instruction::End);
                } else {
                    function.instruction(&Instruction::LocalGet(scratch + 22));
                }
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
/// the stack. Once `value` is evaluated, a non-array `local` throws
/// `not_array` (when given); otherwise `__array_push` appends `value` to the
/// handle read from `local` in place (the handle never moves, so every alias
/// observes the growth) and returns the handle, whose `length` is then read
/// back.
pub(super) fn emit_array_push_node(
    local: u32,
    value: &IrNode,
    not_array: Option<&CalleeError>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    emit_node(value, function, scratch, strings, helpers);
    if let Some(not_array) = not_array {
        emit_array_guard(function, local, not_array, scratch, strings, helpers);
    }
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit `array.pop()`, leaving the removed last element (or `undefined`
/// for an empty array or a trailing hole) on the stack. A non-array `local` throws `not_array`;
/// otherwise the handle read from `local` is shrunk IN PLACE — its `length`
/// drops by one and the element at the old last slot of its store is yielded.
pub(super) fn emit_array_pop_node(
    local: u32,
    not_array: &CalleeError,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // Two transient i32 scratch locals: the handle base and the new length.
    let base = scratch + 3;
    let new_len = scratch + 7;

    emit_array_guard(function, local, not_array, scratch, strings, helpers);

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
}

/// Unless the jsval in `local` is a `TYPE_ARRAY` handle, throw the
/// not-a-function `TypeError` `not_array` (the receiver has no `push`/`pop`)
/// and unwind. Stack-neutral on the not-thrown path.
fn emit_array_guard(
    function: &mut Function,
    local: u32,
    not_array: &CalleeError,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    // The `if` opens a control frame around the throw transfer.
    enter_frame(helpers);
    emit_throw_not_a_function(
        function,
        callee_error_text(not_array),
        scratch,
        strings,
        helpers,
    );
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Push the element jsval at `arr[idx]` onto the stack. `arr`/`idx` are the
/// array-handle-base / loop-index i32 scratch locals of an [`emit_array_hof`]
/// loop; the store base is reloaded per access, since a callback may grow the
/// array (replacing its store).
pub(super) fn emit_hof_load_elem(function: &mut Function, arr: u32, idx: u32) {
    function.instruction(&Instruction::LocalGet(arr));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
}

/// Push the loop index `idx` (an i32 scratch local) as a number jsval — its
/// `f64` bit pattern in the i64 carrier.
pub(super) fn emit_hof_index(function: &mut Function, idx: u32) {
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit the trailing half of a higher-order callback invocation: pad the
/// remaining positional slots to the shared funcref arity with `undefined`, push
/// the callback's own environment (at `+FUNC_ENV_OFFSET`) as the env-last argument
/// and its funcref-table index (at `+FUNC_INDEX_OFFSET`), and `call_indirect`. The real
/// positional args (already pushed by the caller) plus this padding total exactly
/// `helpers.max_args`, which the arity scan guarantees is `>= real_args` so no
/// funcref type mismatch can trap. Leaves the callback's boxed jsval result.
pub(super) fn emit_hof_invoke(
    function: &mut Function,
    helpers: Helpers<'_>,
    cb: u32,
    real_args: u32,
) {
    for _ in real_args..helpers.max_args {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // A higher-order callback is invoked with no receiver, so `this = undefined`;
    // the real positional args are already on the stack, so resetting the
    // carrier here (stack-neutral) does not disturb them.
    emit_reset_this(function);
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

/// Emit an array higher-order method `array.map/filter/reduce/forEach(callback)`,
/// leaving the method's result jsval on the stack. The receiver is
/// evaluated once into the i32 base scratch local `arr` (not rebound), its u32
/// length header into `len`, and the callback function value's base into `cb`; a
/// counted loop over `0..len` then invokes the callback per element through the
/// uniform `call_indirect` ABI (env-last) via [`emit_hof_invoke`]. A hole is
/// skipped (no call); `Map` keeps it as a hole in the result:
///
/// - `Map` fills a fresh same-length array `res[i] = fn(element_i, i)`.
/// - `Filter` grows a fresh array in place via `__array_push` for JS-truthy
///   `fn(element_i, i)`.
/// - `Reduce` folds `acc = fn(acc, element_i, i)` from `acc = init`.
/// - `ForEach` discards each `fn(element_i, i)` result and yields `undefined`.
///
/// The receiver/callback/init sub-expressions are fully evaluated before the loop
/// seeds the HOF scratch locals (`scratch+8`..`scratch+13`), so a nested HOF in
/// any of them completes and frees those locals first — the loop body itself only
/// reads memory and `call_indirect`s, never re-entering an inner HOF at this frame.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn emit_array_hof(
    op: ArrayHofOp,
    array: &IrNode,
    callback: &IrNode,
    init: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let arr = scratch + 8;
    let len = scratch + 9;
    let idx = scratch + 10;
    let cb = scratch + 11;
    let res = scratch + 12;
    let acc = scratch + 13;

    // Prologue: receiver base -> `arr`, its length -> `len`, callback base -> `cb`,
    // and `idx = 0`. The receiver is evaluated first (JS `arr.method(cb)` order),
    // then the callback.
    emit_node(array, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalSet(len));
    emit_node(callback, function, scratch, strings, helpers);
    // No funcref table (a function-less program): the callback can never be a
    // function value, so throw instead of emitting an invalid `call_indirect`.
    if !helpers.has_funcs {
        emit_throw_not_a_function(
            function,
            EXPRESSION_NOT_A_FUNCTION,
            scratch,
            strings,
            helpers,
        );
        return;
    }
    // IsCallable (DF4): a non-function callback throws before any iteration
    // (even over an empty array); a function literal needs no check.
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
            // acc = init (always present at lowering); an absent init would fold from
            // `undefined`, but lowering rejects a no-init reduce before reaching here.
            if let Some(init) = init {
                emit_node(init, function, scratch, strings, helpers);
            } else {
                function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            }
            function.instruction(&Instruction::LocalSet(acc));
        }
        ArrayHofOp::ForEach => {}
    }

    // Counted loop: `while (idx < len) { <body>; idx++ }`.
    function.instruction(&Instruction::Block(BlockType::Empty));
    enter_frame(helpers); // outer block
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers); // loop — the callback's throw check sits inside both
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // Skip a hole: the body runs only for a present element.
    emit_hof_load_elem(function, arr, idx);
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);

    match op {
        ArrayHofOp::Map => {
            // res[idx] = callback(element, index). The slot address
            // (res.elems + 8*idx) is pushed first and survives beneath the
            // call_indirect, which is stack-balanced (leaves one i64); `res` is
            // private to the loop, so its store never moves.
            function.instruction(&Instruction::LocalGet(res));
            emit_array_elems(function);
            function.instruction(&Instruction::LocalGet(idx));
            function.instruction(&Instruction::I32Const(8));
            function.instruction(&Instruction::I32Mul);
            function.instruction(&Instruction::I32Add);
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 2);
            function.instruction(&Instruction::I64Store(elem_slot(0)));
        }
        ArrayHofOp::Filter => {
            // if truthy(callback(element, index)) { __array_push(res, element) }.
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 2);
            function.instruction(&Instruction::LocalSet(acc));
            emit_cond(&IrNode::Local(acc), function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::LocalGet(res));
            emit_hof_load_elem(function, arr, idx);
            function.instruction(&Instruction::Call(helpers.array_push));
            function.instruction(&Instruction::Drop);
            function.instruction(&Instruction::End);
        }
        ArrayHofOp::Reduce => {
            // acc = callback(acc, element, index).
            function.instruction(&Instruction::LocalGet(acc));
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 3);
            function.instruction(&Instruction::LocalSet(acc));
        }
        ArrayHofOp::ForEach => {
            // callback(element, index) for side effects; discard the result.
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 2);
            function.instruction(&Instruction::Drop);
        }
    }
    if op == ArrayHofOp::Map {
        // A hole maps to a hole.
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
