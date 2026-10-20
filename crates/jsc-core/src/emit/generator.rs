//! Emit the wasm for generators: the `MakeGenerator` constructor
//! body, the `gen.next(v)` driver, and the state-machine dispatch function.
//!
//! A `function*` compiles to two ordinary uniform-ABI functions. The CONSTRUCTOR
//! (`IrNode::MakeGenerator`) allocates the [`abi::TYPE_GENERATOR`] handle plus its
//! frame — a `TYPE_ENV` block of boxed cells holding every generator local — and
//! returns it without running the body. The STATE MACHINE ([`emit_generator_sm`])
//! is a `loop { br_table (gen.state) }` dispatch over the CFG basic blocks;
//! `Yield`/`Return` blocks `return`, while `Jump`/`Branch` blocks store the next
//! state and re-dispatch. `gen.next(v)` ([`emit_gen_resume`]) drives it
//! through `call_indirect`, resuming at `state` and reaching the frame's cells
//! through the ordinary closure `EnvLoad` path.

#[allow(clippy::wildcard_imports)]
use super::*;

use crate::ir::GenTerm;

/// Emit `IrNode::MakeGenerator`: build a generator handle and its frame, leaving
/// the handle jsval on the stack. Allocates a `TYPE_ENV` frame of `total` boxed
/// cells (the first `slots.len()` initialized from the constructor's argument
/// expressions, the rest to `undefined`), then the [`abi::TYPE_GENERATOR`] block
/// pointing at it with `sm_idx`/`state = 0`/`done = 0`.
pub(super) fn emit_make_generator(
    sm_table_index: u32,
    slots: &[IrNode],
    total: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let env_base = scratch + 21;
    let gen_dst = scratch + 3;
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // 1. Allocate the frame env block: header, u32 count, then `total` cell slots.
    function.instruction(&Instruction::I32Const(
        HEADER_SIZE + 4 + 8 * total.cast_signed(),
    ));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(env_base));
    function.instruction(&Instruction::LocalGet(env_base));
    function.instruction(&Instruction::I32Const(TYPE_ENV));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalGet(env_base));
    function.instruction(&Instruction::I32Const(total.cast_signed()));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));

    // 2. Each frame cell: a fresh boxed cell holding the parameter argument (for
    // the leading slots) or `undefined` (for the body locals). `emit_cell_new`
    // uses the `+3` dst scratch, disjoint from `env_base` (`+21`).
    for i in 0..total {
        function.instruction(&Instruction::LocalGet(env_base));
        match slots.get(i as usize) {
            Some(value) => emit_cell_new(value, function, scratch, strings, helpers),
            None => emit_cell_new(&IrNode::Undefined, function, scratch, strings, helpers),
        }
        function.instruction(&Instruction::I64Store(MemArg {
            offset: HEADER_SIZE as u64 + 4 + 8 * u64::from(i),
            align: 3,
            memory_index: 0,
        }));
    }

    // 3. Allocate the generator handle and fill its fields (keeping its base on
    // the stack via the tee/get idiom, as `emit_object_node` does).
    function.instruction(&Instruction::I32Const(GEN_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalTee(gen_dst));
    emit_object_prefix(function, gen_dst, TYPE_GENERATOR);
    // sm_idx (u32).
    function.instruction(&Instruction::LocalTee(gen_dst));
    function.instruction(&Instruction::LocalGet(gen_dst));
    function.instruction(&Instruction::I32Const(sm_table_index.cast_signed()));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: GEN_SM_IDX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    // state = 0.
    function.instruction(&Instruction::LocalTee(gen_dst));
    function.instruction(&Instruction::LocalGet(gen_dst));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: GEN_STATE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    // done = 0.
    function.instruction(&Instruction::LocalTee(gen_dst));
    function.instruction(&Instruction::LocalGet(gen_dst));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: GEN_DONE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    // env = the frame block, reboxed as a heap jsval.
    function.instruction(&Instruction::LocalTee(gen_dst));
    function.instruction(&Instruction::LocalGet(gen_dst));
    function.instruction(&Instruction::LocalGet(env_base));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(MemArg {
        offset: GEN_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));

    // Rebox the handle base (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit `IrNode::GenResume` (`gen.next(sent)`), leaving the `{ value, done }`
/// result object's jsval on the stack. Evaluates the handle, throws a `TypeError`
/// for a non-generator receiver, `call_indirect`s the state machine with
/// `(sent, gen)` under the uniform ABI (passing the frame as the trailing `env`
/// argument), then builds the result object from the returned value and the
/// handle's `done` flag.
pub(super) fn emit_gen_resume(
    handle: &IrNode,
    sent: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let handle_slot = scratch + 22;
    let sm_result = scratch + 23;

    // 1. Evaluate the handle once into a stable scratch slot.
    emit_node(handle, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(handle_slot));

    // With no functions in the program there is no funcref table / uniform call
    // type, so no generator can exist — the receiver is provably a non-generator.
    // Throw the `TypeError` UNCONDITIONALLY (skipping the invalid indirect call)
    // and leave `undefined` as the fall-through result of this expression.
    if !helpers.has_funcs {
        emit_throw_type_error(function, scratch, strings, helpers);
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        return;
    }

    // 2. A non-generator receiver throws a `TypeError`.
    emit_is_generator(function, handle_slot);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_throw_type_error(function, scratch, strings, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // 3. Drive the state machine: SM(sent, gen, …, env = gen.env, argc = 2).
    let k = helpers.max_args as usize;
    emit_node(sent, function, scratch, strings, helpers);
    if k < 1 {
        function.instruction(&Instruction::Drop);
    }
    function.instruction(&Instruction::LocalGet(handle_slot));
    if k < 2 {
        function.instruction(&Instruction::Drop);
    }
    for _ in 2..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // Trailing env argument: the generator's frame (its `env` field).
    function.instruction(&Instruction::LocalGet(handle_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: GEN_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    emit_argc(2, function);
    // Funcref-table index (the SM), on top for `call_indirect`.
    function.instruction(&Instruction::LocalGet(handle_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_SM_IDX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The state machine can throw; unwind if it left an exception pending.
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::LocalSet(sm_result));

    // 4. Build the `{ value, done }` result object from the SM's value and the
    // handle's `done` flag.
    emit_result_object(sm_result, handle_slot, scratch, function, strings, helpers);
}

/// Emit the `TypeError` throw shared by the two non-generator-receiver
/// paths of [`emit_gen_resume`]: stash the error in `$exc_value`, set the pending
/// flag, clear any routed completion, and unwind to the nearest handler.
fn emit_throw_type_error(
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(&internal_type_error(), function, scratch, strings, helpers);
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
    emit_throw_return(function, helpers);
}

/// Emit the `{ value, done }` iterator result object, leaving its jsval on the
/// stack: `value` is the state machine's returned value (in `sm_result`), `done`
/// is the handle's `done` flag (in `handle_slot`) as a boolean, staged in the
/// general i64 scratch slot (`scratch`) before the object literal is built.
fn emit_result_object(
    sm_result: u32,
    handle_slot: u32,
    scratch: u32,
    function: &mut Function,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::I64Const(TRUE_BITS));
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::LocalGet(handle_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_DONE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(scratch));
    let entries = [
        (
            PropKey::Static("value".to_owned()),
            IrNode::Local(sm_result),
        ),
        (PropKey::Static("done".to_owned()), IrNode::Local(scratch)),
    ];
    emit_object_node(
        &entries,
        None,
        abi::CLASS_ORDINARY,
        0,
        function,
        scratch,
        strings,
        helpers,
    );
}

/// Emit a generator STATE MACHINE function. Its wasm parameters are the
/// uniform ABI `(sent = local 0, gen = local 1, …, env, argc)`; the body is a
/// `br_table` dispatch on `gen.state` (read from local 1) into one block per
/// segment. Each segment runs its straight-line statements and ends by either
/// storing the next state and returning the yielded value (`done` stays `0`), or
/// setting `done = 1` and returning the final value. Re-entry after completion
/// hits the out-of-range default and returns `undefined`.
pub(super) fn emit_generator_sm(
    func: &IrFunction,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let Some(segments) = func.machine.as_ref() else {
        unreachable!("emit_generator_sm is only called for a function with a machine")
    };
    let declared = func.locals - func.params;
    // Same scratch layout as `emit_function` (see its comment): the state machine
    // reuses `emit_node`/`emit_stmts`, so it needs the full scratch block.
    let mut function = Function::new(vec![
        (declared + 3, ValType::I64),
        (1, ValType::I32),
        (3, ValType::I64),
        (1, ValType::I32),
        (5, ValType::I32),
        (1, ValType::I64),
        (7, ValType::I64),
        (1, ValType::I32),
        (2, ValType::I64),
    ]);
    let scratch = func.locals;
    helpers.unwind.borrow_mut().reset();

    let n = u32::try_from(segments.len()).unwrap_or(u32::MAX);
    // The generator handle is local 1 (a1); read/write `state`/`done` through it.
    let gen_local = 1;

    // A `loop` wraps the whole dispatch so a `Jump`/`Branch` terminator can store
    // the next block index and `br` back to re-dispatch (CFG flattening). Inside
    // it, one `block` per basic block plus a trailing out-of-range default, then a
    // `br_table` on `gen.state`. Branching to block label `i` lands just past its
    // `End`, where block `i`'s code begins; the code ends by `return`ing (a `yield`
    // / a `return`) or by re-dispatching (`Jump`/`Branch`).
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers);
    for _ in 0..=n {
        function.instruction(&Instruction::Block(BlockType::Empty));
        enter_frame(helpers);
    }
    function.instruction(&Instruction::LocalGet(gen_local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_STATE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    let targets: Vec<u32> = (0..n).collect();
    function.instruction(&Instruction::BrTable(targets.into(), n));

    for segment in segments {
        // Close this block's dispatch frame; its code follows the `End`.
        function.instruction(&Instruction::End);
        leave_frame(helpers);
        emit_stmts(
            &segment.stmts,
            &mut function,
            scratch,
            strings,
            helpers,
            None,
            None,
        );
        // The `loop` sits at depth 1 (reset = 0, the loop is the first frame), so a
        // re-dispatch `br` targets `unwind.depth - 1` from wherever we are now.
        let loop_rel = helpers.unwind.borrow().depth - 1;
        emit_segment_term(
            &segment.term,
            gen_local,
            n,
            loop_rel,
            &mut function,
            scratch,
            strings,
            helpers,
        );
    }

    // Close the default block: a resume with an out-of-range state (a completed
    // generator) returns `undefined`.
    function.instruction(&Instruction::End);
    leave_frame(helpers);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Return);
    // Close the dispatch loop. Every path above `return`s or re-dispatches, so the
    // fall-through here is unreachable; emit a dummy result to satisfy validation.
    function.instruction(&Instruction::End);
    leave_frame(helpers);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function
}

/// Emit a basic block's terminator. `loop_rel` is the `br` immediate
/// that re-dispatches (targets the enclosing state-machine loop); `n` is the block
/// count (the out-of-range default state used to mark completion).
#[allow(clippy::too_many_arguments)]
fn emit_segment_term(
    term: &GenTerm,
    gen_local: u32,
    n: u32,
    loop_rel: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match term {
        GenTerm::Yield { value, resume } => {
            // state = resume; return the yielded value (done stays 0).
            store_gen_i32(function, gen_local, GEN_STATE_OFFSET, resume.cast_signed());
            emit_node(value, function, scratch, strings, helpers);
            function.instruction(&Instruction::Return);
        }
        GenTerm::Return(value) => {
            // done = 1; state = n (out of range) so re-entry yields undefined.
            store_gen_i32(function, gen_local, GEN_DONE_OFFSET, 1);
            store_gen_i32(function, gen_local, GEN_STATE_OFFSET, n.cast_signed());
            match value {
                Some(value) => emit_node(value, function, scratch, strings, helpers),
                None => {
                    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
                }
            }
            function.instruction(&Instruction::Return);
        }
        GenTerm::Jump(target) => {
            // state = target; re-dispatch.
            store_gen_i32(function, gen_local, GEN_STATE_OFFSET, target.cast_signed());
            function.instruction(&Instruction::Br(loop_rel));
        }
        GenTerm::Branch {
            cond,
            then_block,
            else_block,
        } => {
            // state = cond ? then_block : else_block; re-dispatch.
            function.instruction(&Instruction::LocalGet(gen_local));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::I32Const(then_block.cast_signed()));
            function.instruction(&Instruction::I32Const(else_block.cast_signed()));
            emit_cond(cond, function, scratch, strings, helpers);
            function.instruction(&Instruction::Select);
            function.instruction(&Instruction::I32Store(MemArg {
                offset: GEN_STATE_OFFSET,
                align: 2,
                memory_index: 0,
            }));
            function.instruction(&Instruction::Br(loop_rel));
        }
    }
}

/// Emit the `__gen_to_array(gen) -> array` helper: drive the generator
/// to exhaustion via `call_indirect`, collecting each yielded value into a fresh
/// growable array (the generator's `return` value is NOT included, matching the
/// iterator protocol). Backs `[...gen]` / `f(...gen)` spread.
///
/// When the program declares no functions there is no funcref table, so no
/// generator can exist — a STUB (returns `undefined`, never called) keeps the
/// helper index stable without an invalid `call_indirect`.
pub(super) fn emit_gen_to_array(helpers: Helpers<'_>) -> Function {
    // Param: gen (local 0). Locals: val (i64, local 1), arr base (i32, local 2).
    let mut function = Function::new(vec![(1, ValType::I64), (1, ValType::I32)]);
    let gen_p = 0;
    let val = 1;
    let arr = 2;

    if !helpers.has_funcs {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::End);
        return function;
    }

    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    // arr = a fresh empty array: __alloc(HEADER_SIZE + 4), TYPE_ARRAY byte, len 0.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));

    let k = helpers.max_args as usize;
    function.instruction(&Instruction::Block(BlockType::Empty)); // $exit (depth 1)
    function.instruction(&Instruction::Loop(BlockType::Empty)); // $L (depth 2)
    // val = SM(undefined, gen, …, env = gen.env, argc = 2).
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS)); // a0 = sent
    function.instruction(&Instruction::LocalGet(gen_p)); // a1 = gen
    for _ in 2..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    function.instruction(&Instruction::LocalGet(gen_p));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: GEN_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    emit_argc(2, &mut function);
    function.instruction(&Instruction::LocalGet(gen_p));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_SM_IDX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    function.instruction(&Instruction::LocalSet(val));
    // If the generator threw, stop draining (the exception stays pending) — this
    // also breaks the loop a non-terminating throw would otherwise spin.
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::BrIf(1)); // -> $exit
    // If it completed (`done`), stop WITHOUT pushing the return value.
    function.instruction(&Instruction::LocalGet(gen_p));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_DONE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::BrIf(1)); // -> $exit
    // arr = __array_push(arr, val).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::Br(0)); // -> $L
    function.instruction(&Instruction::End); // $L
    function.instruction(&Instruction::End); // $exit

    // Rebox the array base as a heap jsval and return.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__async_step(gen, sent) -> value` trampoline: advance an
/// `async function` body's generator state machine by one suspend. It
/// `call_indirect`s the SM with `(sent, gen, …, env = gen.env, argc = 2)` under
/// the uniform ABI — exactly like [`emit_gen_resume`]'s drive step — and returns
/// the SM's raw value (the yielded/awaited value, or the final return value). It
/// deliberately does NOT build a `{ value, done }` object or propagate a pending
/// exception: the host async engine reads the handle's `done` flag and the
/// `__exc_pending`/`__exc_value` globals directly after the call.
///
/// When the program declares no functions there is no funcref table, so no async
/// generator can exist — a STUB (returns `undefined`, never called) keeps the
/// export/index stable without an invalid `call_indirect`.
pub(super) fn emit_async_step(helpers: Helpers<'_>) -> Function {
    // Params: gen (local 0, i64), sent (local 1, i64) -> i64. No extra locals.
    let mut function = Function::new(vec![]);
    let gen_p = 0;
    let sent_p = 1;

    if !helpers.has_funcs {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::End);
        return function;
    }

    // Drive the state machine: SM(sent, gen, …, env = gen.env, argc = 2). Mirrors
    // `emit_gen_resume` step 3 (an async generator forces K >= 2, so the drop
    // guards never fire at runtime; they only keep the stub-arity cases valid).
    let k = helpers.max_args as usize;
    function.instruction(&Instruction::LocalGet(sent_p));
    if k < 1 {
        function.instruction(&Instruction::Drop);
    }
    function.instruction(&Instruction::LocalGet(gen_p));
    if k < 2 {
        function.instruction(&Instruction::Drop);
    }
    for _ in 2..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // Trailing env argument: the generator's frame (its `env` field).
    function.instruction(&Instruction::LocalGet(gen_p));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: GEN_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    emit_argc(2, &mut function);
    // Funcref-table index (the SM), on top for `call_indirect`.
    function.instruction(&Instruction::LocalGet(gen_p));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: GEN_SM_IDX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The SM's value is on the stack; the host reads `done`/`__exc_pending`.
    function.instruction(&Instruction::End);
    function
}

/// Store the i32 constant `value` into the generator handle field at `offset`
/// (base = the handle jsval in `gen_local`).
fn store_gen_i32(function: &mut Function, gen_local: u32, offset: u64, value: i32) {
    function.instruction(&Instruction::LocalGet(gen_local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Const(value));
    function.instruction(&Instruction::I32Store(MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }));
}
