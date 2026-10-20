#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit an array literal `[e0, e1, ..]`, leaving the array jsval (single heap tag
/// `0b101`, type byte `TYPE_ARRAY`) on the stack. `__alloc(HEADER_SIZE + 4 + 8*n)`
/// reserves the block; the type byte, the u32 `len` header, and each element jsval
/// are then written at `dst + HEADER_SIZE + 4 + 8*k` (mirroring the
/// alloc→write→rebox shape of the string literal emission).
///
/// `dst` is kept on the operand stack (never in a persistent local) so nested
/// array literals compose: an element expression may itself build an array,
/// clobbering the transient i32 scratch local, without disturbing this array's
/// pointer sitting beneath it on the stack.
pub(super) fn emit_array_node(
    elems: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The i32 scratch local (declared just past the three i64 scratch locals) is
    // used only transiently, to duplicate `dst` before each write.
    let dst = scratch + 3;
    // Block header: the u32 len header sits at `+HEADER_SIZE`; elements at
    // `+HEADER_SIZE+4+8*k`.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let payload = (HEADER_SIZE + 4) as u64;
    let len = u32::try_from(elems.len()).unwrap_or(u32::MAX);
    let size = HEADER_SIZE + 4 + 8 * i32::try_from(elems.len()).unwrap_or(i32::MAX);

    // dst = __alloc(HEADER_SIZE + 4 + 8*n); leave dst (i32) on the stack.
    function.instruction(&Instruction::I32Const(size));
    function.instruction(&Instruction::Call(helpers.alloc));

    // Write the TYPE_ARRAY type byte at dst (offset 0), keeping dst on the stack.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));

    // Store the u32 len header at dst + HEADER_SIZE, keeping dst on the stack.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(len.cast_signed()));
    function.instruction(&Instruction::I32Store(header));

    // Store each element jsval at dst + HEADER_SIZE + 4 + 8*k, left-to-right.
    // `dst` stays on the stack throughout; the element load/eval sits above it.
    for (k, elem) in elems.iter().enumerate() {
        function.instruction(&Instruction::LocalTee(dst));
        function.instruction(&Instruction::LocalGet(dst));
        emit_node(elem, function, scratch, strings, helpers);
        function.instruction(&Instruction::I64Store(MemArg {
            offset: payload + 8 * k as u64,
            align: 0,
            memory_index: 0,
        }));
    }

    // Rebox the array block's base offset (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit an array literal with a spread `[0, ...a, 9]`, leaving the built
/// array's heap jsval on the stack. Because a spread source's length is only
/// known at runtime, the array is grown incrementally: a fresh empty array is
/// initialized, then each part is appended left-to-right — a plain [`Item`] via
/// `__array_push`, a [`Spread`] source's elements via `__array_concat`.
///
/// The growing block base is kept purely on the operand stack (both runtime
/// helpers take it as their deepest argument and return the reallocated base),
/// so a nested spread array in any part composes without a persistent local.
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
    // The i32 block-alloc scratch local (past the three i64 scratch locals), used
    // only transiently to initialize the fresh empty array's header. After that
    // the base lives on the operand stack.
    let tmp = scratch + 3;
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // base = a fresh empty array: __alloc(HEADER_SIZE + 4), TYPE_ARRAY byte, len 0.
    // Leave base (i32) on the operand stack.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalTee(tmp));
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalTee(tmp));
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(header));

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
/// the stack. The old block base is read from `local`, `__array_push` grows and
/// copies it (appending `value`), and `local` is REBOUND (`local.set`) to the
/// reallocated block so later reads / `.length` observe the growth. The new length
/// is then read back from the fresh block's header.
pub(super) fn emit_array_push_node(
    local: u32,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The transient i32 scratch local (past the three i64 scratch slots) stashes
    // the reallocated block base; `value` finishes using it before we write here.
    let nb = scratch + 3;
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };

    // new_base = __array_push(wrap(old_block), value).
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::LocalSet(nb));

    // Rebind the source local to the reallocated block (boxed as a heap jsval).
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::LocalSet(local));

    // Yield the new length (the fresh block's u32 header) as a number jsval.
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit `array.pop()`, leaving the removed last element (or `undefined`
/// for an empty array) on the stack. No realloc: the block base read from `local`
/// is decremented IN PLACE — its u32 length header drops by one and the removed
/// element jsval at the old last slot is yielded — so the local keeps its pointer.
pub(super) fn emit_array_pop_node(local: u32, function: &mut Function, scratch: u32) {
    // Two transient i32 scratch locals: the block base and the new length.
    let base = scratch + 3;
    let new_len = scratch + 7;
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };

    // base = wrap(array_block).
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // len == 0 ? undefined : pop the last element.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    // new_len = len - 1; write it back into the header.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(new_len));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::I32Store(header));
    // removed = load the element jsval at base + HEADER_SIZE + 4 + 8*new_len.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(new_len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::End);
}

/// Push the element jsval at `arr[idx]` — `load i64 at arr + HEADER_SIZE + 4 +
/// 8*idx` — onto the stack. `arr`/`idx` are the array-base / loop-index i32
/// scratch locals of an [`emit_array_hof`] loop.
pub(super) fn emit_hof_load_elem(function: &mut Function, arr: u32, idx: u32) {
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
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
/// the callback's own environment (at `+HEADER_SIZE+4`) as the env-last argument
/// and its funcref-table index (at `+HEADER_SIZE`), and `call_indirect`. The real
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
        offset: (HEADER_SIZE + 4) as u64,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI): the real positional args the callback
    // receives, so a rest-parameter callback collects exactly those.
    emit_argc(real_args as usize, function);
    function.instruction(&Instruction::LocalGet(cb));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
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
/// uniform `call_indirect` ABI (env-last) via [`emit_hof_invoke`]:
///
/// - `Map` fills a fresh same-length array `res[i] = fn(element_i, i)`.
/// - `Filter` grows a fresh array via `__array_push` for JS-truthy `fn(element_i, i)`.
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
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };

    // Prologue: receiver base -> `arr`, its length -> `len`, callback base -> `cb`,
    // and `idx = 0`. The receiver is evaluated first (JS `arr.method(cb)` order),
    // then the callback.
    emit_node(array, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));
    emit_node(callback, function, scratch, strings, helpers);
    // No funcref table (a function-less program): the callback can never be a
    // function value, so throw instead of emitting an invalid `call_indirect`.
    if !helpers.has_funcs {
        emit_throw_not_a_function(function, scratch, strings, helpers);
        return;
    }
    // IsCallable (DF4): a non-function callback throws before any iteration
    // (even over an empty array); a function literal needs no check.
    if !is_static_function(callback) {
        function.instruction(&Instruction::LocalSet(scratch));
        emit_callable_guard(function, scratch, scratch, strings, helpers);
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
            // res = __alloc(HEADER_SIZE + 4 + 8*len); type byte + copied length.
            function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
            function.instruction(&Instruction::LocalGet(len));
            function.instruction(&Instruction::I32Const(8));
            function.instruction(&Instruction::I32Mul);
            function.instruction(&Instruction::I32Add);
            function.instruction(&Instruction::Call(helpers.alloc));
            function.instruction(&Instruction::LocalSet(res));
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::I32Const(TYPE_ARRAY));
            function.instruction(&Instruction::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::LocalGet(len));
            function.instruction(&Instruction::I32Store(header));
        }
        ArrayHofOp::Filter => {
            // res = a fresh empty array (grown per truthy element by __array_push).
            function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
            function.instruction(&Instruction::Call(helpers.alloc));
            function.instruction(&Instruction::LocalSet(res));
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::I32Const(TYPE_ARRAY));
            function.instruction(&Instruction::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::I32Store(header));
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

    match op {
        ArrayHofOp::Map => {
            // res[idx] = callback(element, index). The store address (res + 8*idx)
            // is pushed first and survives beneath the call_indirect, which is
            // stack-balanced (leaves one i64).
            function.instruction(&Instruction::LocalGet(res));
            function.instruction(&Instruction::LocalGet(idx));
            function.instruction(&Instruction::I32Const(8));
            function.instruction(&Instruction::I32Mul);
            function.instruction(&Instruction::I32Add);
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 2);
            function.instruction(&Instruction::I64Store(MemArg {
                offset: (HEADER_SIZE + 4) as u64,
                align: 0,
                memory_index: 0,
            }));
        }
        ArrayHofOp::Filter => {
            // if truthy(callback(element, index)) { res = __array_push(res, element) }.
            emit_hof_load_elem(function, arr, idx);
            emit_hof_index(function, idx);
            emit_hof_invoke(function, helpers, cb, 2);
            function.instruction(&Instruction::LocalSet(acc));
            emit_cond(&IrNode::Local(acc), function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::LocalGet(res));
            emit_hof_load_elem(function, arr, idx);
            function.instruction(&Instruction::Call(helpers.array_push));
            function.instruction(&Instruction::LocalSet(res));
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
