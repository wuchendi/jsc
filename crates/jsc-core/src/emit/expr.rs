#[allow(clippy::wildcard_imports)]
use super::*;

/// Recursively emit the instructions for `node` into `function`.
// The central per-IR-variant dispatch: one match arm per `IrNode`, so it grows
// with the node set rather than with any single case's complexity.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_node(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    match node {
        IrNode::Num(value) => {
            // A number jsval is the literal's raw f64 bits in the i64 carrier; a
            // NaN constant (e.g. a folded `-NaN`) is the canonical QNAN bits.
            let bits = if value.is_nan() {
                QNAN_BITS
            } else {
                value.to_bits().cast_signed()
            };
            function.instruction(&Instruction::I64Const(bits));
        }
        IrNode::Str(text) => {
            // A string literal is a heap-pointer jsval: the pointer header OR'd
            // with the literal's compile-time byte offset in static data.
            // `collect_strings` walked the same tree, so every literal has an
            // offset; fall back to HEAP_BASE rather than panic if one is missing.
            let offset = strings.get(text.as_str()).copied().unwrap_or(HEAP_BASE);
            function.instruction(&Instruction::I64Const(HEAP_BITS | i64::from(offset)));
        }
        IrNode::Bool(value) => {
            // A boolean jsval is the boxed TRUE/FALSE immediate.
            function.instruction(&Instruction::I64Const(if *value {
                TRUE_BITS
            } else {
                FALSE_BITS
            }));
        }
        IrNode::Null => {
            function.instruction(&Instruction::I64Const(NULL_BITS));
        }
        IrNode::Undefined => {
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        IrNode::Local(index) => {
            function.instruction(&Instruction::LocalGet(*index));
        }
        IrNode::Assign { local, value } => {
            emit_node(value, function, scratch, strings, helpers);
            // `local.tee` stores the value and leaves a copy on the stack so an
            // assignment can be used as an expression.
            function.instruction(&Instruction::LocalTee(*local));
        }
        IrNode::Call { func, args } => {
            // Direct call to a known function declaration (env-last ABI): push the
            // K padded args, then a trailing `undefined` env (a top-level function
            // never captures) and the argument count, then a plain `call`. The
            // callee leaves its boxed jsval result.
            emit_call_args(args, function, scratch, strings, helpers);
            // A plain call binds `this = undefined`: reset the carrier after
            // every argument is evaluated, so a method call in an argument does not
            // leak its receiver into this callee.
            emit_reset_this(function);
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            emit_argc(args.len(), function);
            // `func` is codegen's index (main = 0, user functions 1..=n); the host
            // import shifts every defined function up by IMPORT_FUNC_COUNT.
            function.instruction(&Instruction::Call(*func + IMPORT_FUNC_COUNT));
            // A user function can throw; unwind if it left an exception in flight.
            emit_propagate_if_pending(function, helpers);
        }
        IrNode::ConsoleLog { args } => {
            emit_console_log(args, function, scratch, strings, helpers);
        }
        IrNode::MathCall { op, args } => {
            emit_math_call(*op, args, function, scratch, strings, helpers);
        }
        IrNode::ConvertCall { op, arg, radix } => {
            emit_convert_call(
                *op,
                arg,
                radix.as_deref(),
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::FuncValue {
            table_index,
            captures,
            kind,
        } => {
            emit_func_value(
                *table_index,
                captures,
                *kind,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::ErrorProto(kind) => {
            function.instruction(&Instruction::I64Const(error_proto_bits(*kind, helpers)));
        }
        IrNode::DeclFunc(table_index) => {
            let base = decl_func_base(*table_index, helpers);
            function.instruction(&Instruction::I64Const(
                HEAP_BITS | i64::from(base.cast_unsigned()),
            ));
        }
        IrNode::DefineProps {
            obj,
            local,
            entries,
        } => {
            emit_define_props_node(obj, *local, entries, function, scratch, strings, helpers);
        }
        IrNode::CallIndirect {
            callee,
            args,
            not_callable,
        } => {
            emit_call_indirect(
                callee,
                args,
                not_callable,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::CellNew(value) => {
            emit_cell_new(value, function, scratch, strings, helpers);
        }
        IrNode::CellGet(cell) => {
            emit_node(cell, function, scratch, strings, helpers);
            emit_cell_load(function);
        }
        IrNode::CellSet { cell, value } => {
            emit_cell_set(cell, value, function, scratch, strings, helpers);
        }
        IrNode::EnvLoad(slot) => emit_env_load(*slot, function, helpers),
        IrNode::ModuleCell(slot) => {
            function.instruction(&Instruction::GlobalGet(MODULE_CELL_GLOBAL_BASE + slot));
        }
        IrNode::IsHole(operand) => {
            // Compare the raw bits to the TDZ hole and box the i32 result as a
            // boolean jsval (the i32 scratch carries the cond past the two consts).
            emit_node(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Const(TDZ_HOLE_BITS));
            function.instruction(&Instruction::I64Eq);
            function.instruction(&Instruction::LocalSet(scratch + 21));
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::LocalGet(scratch + 21));
            function.instruction(&Instruction::Select);
        }
        IrNode::Hole => {
            function.instruction(&Instruction::I64Const(TDZ_HOLE_BITS));
        }
        IrNode::ClassHeritage {
            value,
            parent,
            proto,
        } => emit_class_heritage(value, *parent, *proto, function, scratch, strings, helpers),
        IrNode::IsObject(operand) => {
            emit_node(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(scratch + 22));
            emit_is_object(function, scratch + 22);
            emit_box_bool(function, scratch);
        }
        IrNode::SetProto { obj, proto } => {
            emit_node(obj, function, scratch, strings, helpers);
            function.instruction(&Instruction::I32WrapI64);
            emit_node(proto, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Store(MemArg {
                offset: u64::from(abi::OBJ_PROTO_OFFSET),
                align: 3,
                memory_index: 0,
            }));
            emit_node(obj, function, scratch, strings, helpers);
        }
        // The value passed to `.next(v)` is the state machine's first wasm
        // parameter (local 0, `sent`).
        IrNode::SentValue => {
            function.instruction(&Instruction::LocalGet(0));
        }
        IrNode::MakeGenerator {
            sm_table_index,
            slots,
            total,
        } => {
            generator::emit_make_generator(
                *sm_table_index,
                slots,
                *total,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::IsGenerator(operand) => {
            // Evaluate into a stable scratch slot, test the type byte, and box the
            // i32 result as a boolean jsval. `select` wants (a, b, cond), so stash
            // the cond in the i32 scratch and push it last.
            emit_node(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(scratch + 22));
            emit_is_generator(function, scratch + 22);
            function.instruction(&Instruction::LocalSet(scratch + 21));
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::LocalGet(scratch + 21));
            function.instruction(&Instruction::Select);
        }
        IrNode::GenResume { handle, sent } => {
            generator::emit_gen_resume(handle, sent, function, scratch, strings, helpers);
        }
        // `Yield` is consumed by the generator segmentation pass; reaching emit
        // means an unsupported yield position slipped past codegen's diagnostics.
        IrNode::Yield(_) => {
            unreachable!("a `yield` survived to emit — codegen must reject it first")
        }
        IrNode::TypeOf(operand) => {
            emit_typeof_node(operand, function, scratch, strings, helpers);
        }
        IrNode::StrLen(operand) => {
            emit_str_len(operand, function, scratch, strings, helpers);
        }
        IrNode::NullishCheck { value, key } => {
            emit_nullish_check_node(value, key, function, scratch, strings, helpers);
        }
        IrNode::Array(elems) => {
            emit_array_node(elems, function, scratch, strings, helpers);
        }
        IrNode::Seq(nodes) => {
            for (i, node) in nodes.iter().enumerate() {
                if i > 0 {
                    function.instruction(&Instruction::Drop);
                }
                emit_node(node, function, scratch, strings, helpers);
            }
        }
        IrNode::ArraySpread(parts) => {
            emit_array_spread(parts, function, scratch, strings, helpers);
        }
        IrNode::CallSpread { target, args } => {
            emit_call_spread(target, args, function, scratch, strings, helpers);
        }
        IrNode::MathSpread { op, args } => {
            emit_math_spread(*op, args, function, scratch, strings, helpers);
        }
        // The receiver binding is guarded before the argument runs (the member
        // read precedes argument evaluation).
        IrNode::ArrayPush { local, value } => {
            let key = IrNode::Str("push".to_owned());
            emit_nullish_guard(function, *local, &key, false, scratch, strings, helpers);
            emit_array_push_node(*local, value, function, scratch, strings, helpers);
        }
        IrNode::ArrayPop { local } => {
            let key = IrNode::Str("pop".to_owned());
            emit_nullish_guard(function, *local, &key, false, scratch, strings, helpers);
            emit_array_pop_node(*local, function, scratch);
        }
        IrNode::ArrayHof {
            op,
            array,
            callback,
            init,
        } => {
            emit_array_hof(
                *op,
                array,
                callback,
                init.as_deref(),
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::MethodCall { op, recv, args } => {
            emit_method_call(*op, recv, args, function, scratch, strings, helpers);
        }
        node @ IrNode::MethodCallUser { .. } => {
            emit_method_call_user(node, function, scratch, strings, helpers);
        }
        node @ IrNode::Construct { .. } => {
            emit_construct(node, function, scratch, strings, helpers);
        }
        node @ IrNode::ThisCall { .. } => {
            emit_this_call(node, function, scratch, strings, helpers);
        }
        IrNode::ThisGlobal => {
            // Snapshot read of the `this`-binding carrier; emitted only in a
            // function/arrow prologue.
            function.instruction(&Instruction::GlobalGet(THIS_GLOBAL));
        }
        IrNode::NewTargetGlobal => {
            function.instruction(&Instruction::GlobalGet(NEW_TARGET_GLOBAL));
        }
        IrNode::IsConstructCall(argc) => {
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            emit_is_construct_call(function, *argc);
            function.instruction(&Instruction::Select);
        }
        IrNode::ArgCount(argc) => {
            // marker ? -1 - argc : argc (both numbers, so no reboxing).
            function.instruction(&Instruction::F64Const(Ieee64::from(-1.0)));
            function.instruction(&Instruction::LocalGet(*argc));
            function.instruction(&Instruction::F64ReinterpretI64);
            function.instruction(&Instruction::F64Sub);
            function.instruction(&Instruction::I64ReinterpretF64);
            function.instruction(&Instruction::LocalGet(*argc));
            emit_is_construct_call(function, *argc);
            function.instruction(&Instruction::Select);
        }
        IrNode::Object {
            entries,
            proto,
            class,
            reserve,
        } => {
            emit_object_node(
                entries,
                proto.as_deref(),
                *class,
                *reserve,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::ObjectRest { source, exclude } => {
            emit_object_rest_node(source, exclude, function, scratch, strings, helpers);
        }
        IrNode::Index { obj, index } => {
            emit_index_node(obj, index, true, function, scratch, strings, helpers);
        }
        IrNode::MakeAccessor { getter, setter } => {
            emit_make_accessor_node(getter, setter, function, scratch, strings, helpers);
        }
        IrNode::ObjectBuiltin { op, arg } => {
            emit_object_builtin_node(*op, arg, function, scratch, strings, helpers);
        }
        IrNode::ObjectHas { key, obj } => {
            emit_object_has_node(key, obj, function, scratch, strings, helpers);
        }
        IrNode::InstanceOf { value, kind } => {
            emit_instanceof_node(value, kind, function, scratch, strings, helpers);
        }
        IrNode::ObjectDelete { obj, key } => {
            emit_object_delete_node(obj, key, function, scratch, strings, helpers);
        }
        IrNode::JsonStringify { arg } => {
            emit_json_stringify_node(arg, function, scratch, strings, helpers);
        }
        IrNode::JsonParse { arg } => {
            emit_json_parse_node(arg, function, scratch, strings, helpers);
        }
        IrNode::MapNew { set } => {
            emit_map_new_node(*set, function, scratch, helpers);
        }
        node @ IrNode::MapMethod { .. } => {
            emit_map_method_node(node, function, scratch, strings, helpers);
        }
        node @ IrNode::ReceiverMethod { .. } => {
            emit_receiver_method_node(node, function, scratch, strings, helpers);
        }
        IrNode::IndexSet { obj, index, value } => {
            emit_index_set_node(obj, index, value, function, scratch, strings, helpers);
        }
        node @ IrNode::IndexOpSet { .. } => {
            emit_index_op_set_node(node, function, scratch, strings, helpers);
        }
        IrNode::Binary { op, lhs, rhs } => {
            emit_binary(*op, lhs, rhs, function, scratch, strings, helpers);
        }
        IrNode::IncDec { local, inc, prefix } => {
            emit_inc_dec(*local, *inc, *prefix, function, helpers);
        }
        // The promise-creation primitives: push the `__promise` op flag,
        // the value operand, and an unused `b` slot, then call the host import. It
        // returns the fresh (or, for `resolve` of a promise, adopted) promise jsval.
        IrNode::PromiseResolve(value) => {
            emit_promise_call(
                PROMISE_OP_RESOLVE,
                value,
                &IrNode::Undefined,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::PromiseReject(value) => {
            emit_promise_call(
                PROMISE_OP_REJECT,
                value,
                &IrNode::Undefined,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        // `p.finally(fn)`: the finalizer rides in the `b` slot; the host runs it on
        // settlement and passes the original settlement through.
        IrNode::PromiseFinally {
            promise,
            on_finally,
        } => {
            emit_promise_call(
                PROMISE_OP_FINALLY,
                promise,
                on_finally,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        // `await p`: drive the microtask queue until `p` settles. A rejection is
        // surfaced through the exception globals, so unwind if it left one pending.
        IrNode::Await(promise) => {
            emit_promise_call(
                PROMISE_OP_AWAIT,
                promise,
                &IrNode::Undefined,
                function,
                scratch,
                strings,
                helpers,
            );
            emit_propagate_if_pending(function, helpers);
        }
        // `p.then(onF, onR)` / `p.catch(onR)`: register reactions on `p` and yield
        // the chained result promise (the host `__then` import).
        IrNode::PromiseThen {
            promise,
            on_fulfilled,
            on_rejected,
        } => {
            emit_node(promise, function, scratch, strings, helpers);
            emit_node(on_fulfilled, function, scratch, strings, helpers);
            emit_node(on_rejected, function, scratch, strings, helpers);
            function.instruction(&Instruction::Call(helpers.then));
        }
        // `new Promise(executor)`: build the promise + resolve/reject settlers, run
        // the executor synchronously (a throw rejects the promise), and yield it.
        IrNode::PromiseCtor { executor, p_local } => {
            emit_promise_ctor(executor, *p_local, function, scratch, strings, helpers);
        }
        // Spawn an `async function` body: build the generator (the inner
        // `MakeGenerator`) and hand it to the host async engine, which allocates
        // the result promise, drives the first step, and returns the promise.
        IrNode::AsyncSpawn(make_generator) => {
            emit_promise_call(
                PROMISE_OP_ASYNC_SPAWN,
                make_generator,
                &IrNode::Undefined,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        // The `Date` builtins: allocate/read a `TYPE_DATE` block's f64
        // millisecond payload. `DateNew`/`DateNow` seed from `ms` or the host clock;
        // `DateGetTime` reads the payload back (or `NaN` for a non-Date receiver).
        IrNode::DateNew { ms } => {
            emit_date_new_node(ms.as_deref(), function, scratch, strings, helpers);
        }
        IrNode::DateNow => emit_date_now_node(function, helpers),
        IrNode::DateGetTime { recv } => {
            emit_date_get_time_node(recv, function, scratch, strings, helpers);
        }
        node @ IrNode::DateMethod { .. } => {
            emit_date_method_node(node, function, scratch, strings, helpers);
        }
        IrNode::Not(operand) => {
            // `!operand` = boolean(!truthy(operand)). `emit_cond` leaves the i32
            // truthiness (1/0); `i32.eqz` inverts it, then it is boxed into a
            // TRUE/FALSE jsval (FALSE_BITS | negated_bit).
            emit_cond(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::I32Eqz);
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::I64Or);
        }
        IrNode::Logical { op, lhs, rhs } => {
            emit_logical(*op, lhs, rhs, function, scratch, strings, helpers);
        }
        IrNode::Ternary {
            cond,
            then_branch,
            else_branch,
        } => {
            // Same JS truthiness as an `if`; the chosen branch leaves one jsval.
            emit_cond(cond, function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            // Each branch may contain a throwing call or `throw`, one frame deeper.
            enter_frame(helpers);
            emit_node(then_branch, function, scratch, strings, helpers);
            function.instruction(&Instruction::Else);
            emit_node(else_branch, function, scratch, strings, helpers);
            leave_frame(helpers);
            function.instruction(&Instruction::End);
        }
        IrNode::Throw(value) => {
            // Evaluate the operand, stash it in `$exc_value`, raise `$exc_pending`,
            // then unwind to the enclosing handler (the function boundary when none is active).
            emit_node(value, function, scratch, strings, helpers);
            function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
            function.instruction(&Instruction::I32Const(1));
            function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
            // A throw is an abrupt completion that REPLACES any pending
            // return/break/continue that a `finally` was mid-routing, so
            // clear `$cf_kind` to keep at most one completion in flight.
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
            emit_throw_return(function, helpers);
            // `throw` diverges, but as an expression node it must nominally leave
            // one jsval on the (now unreachable) stack so any wrapping expression
            // stays stack-balanced under wasm's unreachable-code typing.
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        IrNode::ExcValue => {
            // The in-flight thrown value: read the `$exc_value` global.
            // Emitted on a `catch` entry path to bind the catch parameter.
            function.instruction(&Instruction::GlobalGet(EXC_VALUE_GLOBAL));
        }
    }
}

/// Push the `K` positional argument jsvals of a uniform-ABI call frame onto the
/// stack (env-last ABI): exactly `helpers.max_args` slots, surplus arguments
/// evaluated then dropped, missing arguments padded with `undefined`. The
/// trailing `env` slot is pushed by the caller *after* these (a direct call
/// pushes `undefined`; an indirect call pushes the callee's own environment),
/// then the callee index if indirect.
pub(super) fn emit_call_args(
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let k = helpers.max_args as usize;
    for (i, arg) in args.iter().enumerate() {
        emit_node(arg, function, scratch, strings, helpers);
        // JS ignores extra arguments; drop any beyond the fixed K slots.
        if i >= k {
            function.instruction(&Instruction::Drop);
        }
    }
    // Missing arguments are `undefined`.
    for _ in args.len()..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
}

/// Emit `__this = undefined`: a plain (non-method) call resets the
/// this-binding carrier so the callee's prologue snapshots `undefined`. Placed
/// right before the `call`/`call_indirect`, after every argument has been
/// evaluated, so a nested method call in an argument does not leak its receiver.
/// The push+`global.set` pair is stack-neutral.
pub(super) fn emit_reset_this(function: &mut Function) {
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
}

/// Emit a user method call `obj.m(args)` binding `this = obj`, leaving the
/// callee's boxed jsval result on the stack. The receiver is evaluated once into
/// the stable frame local `obj_local`; the `K` padded argument slots are pushed
/// (env-last ABI); the callee — the function value at member `key` of the
/// receiver — is read via the ordinary object-member path and its block base
/// stashed; then `__this` is set to the receiver immediately before the
/// `call_indirect`, so nothing evaluated afterward can clobber it. The callee's
/// own environment is the trailing `env` argument, then `argc`, then the funcref
/// index.
pub(super) fn emit_method_call_user(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::MethodCallUser {
        obj,
        key,
        args,
        obj_local,
        not_callable,
    } = node
    else {
        unreachable!("emit_method_call_user is only called with a MethodCallUser node")
    };
    let obj_local = *obj_local;
    let base = scratch + 7;
    // Evaluate the receiver ONCE into a stable frame local (survives argument
    // evaluation, the member read, and the `__this` set).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(obj_local));
    // A nullish receiver throws on the method lookup, before any argument runs.
    // The key is only evaluated here on the throw path.
    emit_nullish_guard(function, obj_local, key, false, scratch, strings, helpers);

    // K padded argument slots (deepest on the stack). Nested method calls here set
    // `__this`, but it is overwritten below after all arguments are evaluated.
    emit_call_args(args, function, scratch, strings, helpers);

    // Evaluate the callee — the function value at `receiver[key]` — via the
    // ordinary object-member read (already nullish-guarded above), and stash the
    // full jsval in a scratch i64.
    emit_index_node(
        &IrNode::Local(obj_local),
        key,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    // The general i64 scratch slot (+0) is free here: the member read has already
    // left its result on the stack, and nothing below reads +0 before this call.
    let callee_val = scratch;
    function.instruction(&Instruction::LocalSet(callee_val));
    // No funcref table (a function-less program): the member can never be a
    // function value, so throw instead of emitting an invalid `call_indirect`.
    if !helpers.has_funcs {
        emit_throw_not_a_function(
            function,
            callee_error_text(not_callable),
            scratch,
            strings,
            helpers,
        );
        return;
    }

    // Runtime guard: the resolved member MUST be a function value. For a
    // dynamic-shape receiver — a `new`-instance, a reassigned binding, a computed
    // key, or a call result — the compile-time unknown-method guards
    // cannot prove the method absent, so `receiver[key]` may read `undefined` (or
    // any non-function). Dispatching that as a funcref index is undefined
    // behaviour; instead throw a clean `TypeError` and unwind to the nearest
    // handler (or the program boundary), exactly like `throw`.
    emit_callable_guard(
        function,
        callee_error_text(not_callable),
        callee_val,
        scratch,
        strings,
        helpers,
    );

    // Not-thrown path: proceed with the callee's block base (its wrapped pointer).
    function.instruction(&Instruction::LocalGet(callee_val));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // Bind `this = receiver` immediately before the call (after every sub-eval).
    function.instruction(&Instruction::LocalGet(obj_local));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));

    // Trailing env argument: the callee's own environment (at FUNC_ENV_OFFSET).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI: after env, before the funcref index).
    emit_argc(args.len(), function);
    // Funcref-table index (at FUNC_INDEX_OFFSET), on top for call_indirect.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The called method can throw; unwind if it left an exception pending.
    emit_propagate_if_pending(function, helpers);
}

/// Emit a `super(args)` / `super.m(args)` call (`IrNode::ThisCall`),
/// leaving the callee's boxed result on the stack. Mirrors [`emit_method_call_user`]
/// but binds `this` to the ENCLOSING instance (`this_local`, the current method's
/// snapshotted `this`) rather than a member receiver, so a parent constructor's
/// `this.<field> = ..` mutates the instance the subclass is building. `callee` is
/// a direct function value (the parent constructor or a parent-prototype method);
/// the same not-a-function runtime guard throws a `TypeError` if `super.m`
/// resolves to a non-function (e.g. a missing method).
pub(super) fn emit_this_call(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::ThisCall {
        callee,
        args,
        this_local,
        construct,
    } = node
    else {
        unreachable!("emit_this_call is only called with a ThisCall node")
    };
    let this_local = *this_local;
    let base = scratch + 7;

    // K padded argument slots (deepest on the stack).
    emit_call_args(args, function, scratch, strings, helpers);

    // Evaluate the callee function value and stash the full jsval in a scratch i64.
    emit_node(callee, function, scratch, strings, helpers);
    let callee_val = scratch;
    function.instruction(&Instruction::LocalSet(callee_val));
    // No funcref table (a function-less program, e.g. an optional call
    // `o.m?.()` on a literal): the callee can never be a function value.
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

    // Runtime guard: the callee MUST be a function value — a `super.m` that
    // resolves to `undefined` (missing method) throws a TypeError instead
    // of dispatching a bogus funcref index. Identical to the method-call guard.
    emit_callable_guard(
        function,
        EXPRESSION_NOT_A_FUNCTION,
        callee_val,
        scratch,
        strings,
        helpers,
    );

    // Not-thrown path: proceed with the callee's block base (its wrapped pointer).
    function.instruction(&Instruction::LocalGet(callee_val));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // Keep `this` as the enclosing instance, set immediately before the call.
    function.instruction(&Instruction::LocalGet(this_local));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));

    // Trailing env argument: the callee's own environment (at FUNC_ENV_OFFSET).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI: after env, before the funcref index).
    if *construct {
        emit_construct_argc(args.len(), function);
    } else {
        emit_argc(args.len(), function);
    }
    // Funcref-table index (at FUNC_INDEX_OFFSET), on top for call_indirect.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The parent constructor/method can throw; unwind if it left one pending.
    emit_propagate_if_pending(function, helpers);
}

/// Emit `new callee(args)` ([`IrNode::Construct`], design 4.5), leaving the
/// constructed object on the stack. JS order: the callee is evaluated into
/// `callee_local`, then the `K` argument slots are pushed (from the spread
/// array when present); then `IsConstructor(callee)` is checked, `this` is
/// allocated into `this_local` with `proto = Get(callee, "prototype")` when that
/// is an object (else `null`, the `%Object.prototype%` placeholder), `__this` /
/// `__new_target` are set, and the callee runs with the construct marker
/// `argc = -(n + 1)`. An object return value replaces `this`.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_construct(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let IrNode::Construct {
        callee,
        args,
        spread,
        callee_local,
        this_local,
        reserve,
        not_constructor,
        new_target,
    } = node
    else {
        unreachable!("emit_construct is only called with a Construct node")
    };
    let (f, this) = (*callee_local, *this_local);
    // `new.target`: the callee itself, or the derived class for `super(..)`.
    let emit_new_target = |function: &mut Function| match new_target {
        Some(new_target) => emit_node(new_target, function, scratch, strings, helpers),
        None => {
            function.instruction(&Instruction::LocalGet(f));
        }
    };
    let text = callee_error_text(not_constructor);
    // The spread argument array's block base and length (as in
    // `emit_call_spread`); nothing below touches them before `argc` is pushed.
    let (base, len) = (scratch + 8, scratch + 9);
    emit_node(callee, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(f));
    match spread {
        Some(spread) => {
            emit_spread_args_setup(spread, base, len, function, scratch, strings, helpers);
            emit_spread_arg_slots(base, len, function, helpers);
        }
        None => emit_call_args(args, function, scratch, strings, helpers),
    }
    // No funcref table (a function-less program): nothing is a constructor.
    if !helpers.has_funcs {
        emit_throw_not_a_function(function, text, scratch, strings, helpers);
        return;
    }
    emit_is_constructor(function, f);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_throw_not_a_function(function, text, scratch, strings, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // A SELF_ALLOC callee (a derived class) allocates its own `this` through
    // `super(..)`; `__this` is then unused.
    function.instruction(&Instruction::LocalGet(f));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(i32::from(abi::FUNC_FLAG_SELF_ALLOC)));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    // `this`: proto = Get(new.target, "prototype") if an object, else null.
    emit_new_target(function);
    function.instruction(&Instruction::I32Const(static_key(strings, "prototype")));
    function.instruction(&Instruction::Call(helpers.get));
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
    function.instruction(&Instruction::LocalSet(this));
    emit_select_object(function, this, &Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::LocalSet(this));
    emit_object_node(
        &[],
        Some(&IrNode::Local(this)),
        abi::CLASS_ORDINARY,
        *reserve,
        function,
        scratch,
        strings,
        helpers,
    );
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalTee(this));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
    emit_new_target(function);
    function.instruction(&Instruction::GlobalSet(NEW_TARGET_GLOBAL));

    // env, marker argc, funcref index; call.
    function.instruction(&Instruction::LocalGet(f));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    if spread.is_some() {
        emit_spread_construct_argc(len, function);
    } else {
        emit_construct_argc(args.len(), function);
    }
    function.instruction(&Instruction::LocalGet(f));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    emit_propagate_if_pending(function, helpers);

    // An object return value replaces `this`.
    function.instruction(&Instruction::LocalSet(f));
    emit_select_object(function, f, &Instruction::LocalGet(this));
}

/// Emit an [`IrNode::ClassHeritage`]: `parent = value`; a non-`null` parent
/// must be a constructor, and its `prototype` (into `proto`) an object or
/// `null`; a failure raises the `__prop_throw` `TypeError` naming the value.
/// Leaves `undefined` on the stack.
fn emit_class_heritage(
    value: &IrNode,
    parent: u32,
    proto: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(parent));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::LocalSet(proto));
    function.instruction(&Instruction::LocalGet(parent));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_is_constructor(function, parent);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_prop_throw_call(function, parent, parent, PROP_THROW_EXTENDS, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(parent));
    function.instruction(&Instruction::I32Const(static_key(strings, "prototype")));
    function.instruction(&Instruction::Call(helpers.get));
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
    function.instruction(&Instruction::LocalTee(proto));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::I64Ne);
    emit_is_object(function, proto);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_prop_throw_call(function, proto, proto, PROP_THROW_EXTENDS_PROTO, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
}

/// Push the jsval in `local` when it is an object-family heap value (any heap
/// kind but a string), else the i64 pushed by `alt`.
fn emit_select_object(function: &mut Function, local: u32, alt: &Instruction<'_>) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(alt);
    emit_is_object(function, local);
    function.instruction(&Instruction::Select);
}

/// Push `1` if the jsval in `local` is an object (a heap value other than a
/// string), else `0`.
fn emit_is_object(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push nonzero if the jsval in `local` is a constructor: a function value
/// whose header carries `FUNC_FLAG_CONSTRUCTOR`, else `0`.
fn emit_is_constructor(function: &mut Function, local: u32) {
    emit_is_function(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(i32::from(
        abi::FUNC_FLAG_CONSTRUCTOR,
    )));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Box the i32 condition on the stack as a boolean jsval (the i32 scratch
/// `+21` carries it past the two constants).
fn emit_box_bool(function: &mut Function, scratch: u32) {
    function.instruction(&Instruction::LocalSet(scratch + 21));
    function.instruction(&Instruction::I64Const(TRUE_BITS));
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::LocalGet(scratch + 21));
    function.instruction(&Instruction::Select);
}

/// Push `1` if the `argc` local holds the construct marker: a negative number,
/// i.e. its sign bit is set (a plain count is `+0` or positive).
pub(super) fn emit_is_construct_call(function: &mut Function, argc: u32) {
    function.instruction(&Instruction::LocalGet(argc));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::I64LtS);
}

/// Push the construct marker `argc = -(count + 1)` as a NUMBER jsval for a
/// `new` / `super(..)` call passing `count` arguments.
pub(super) fn emit_construct_argc(count: usize, function: &mut Function) {
    // See `emit_argc`: the count is tiny, so the conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    let bits = (-(count as f64) - 1.0).to_bits().cast_signed();
    function.instruction(&Instruction::I64Const(bits));
}

/// Push the construct marker `argc = -(len + 1)` for a spread `new` /
/// `super(...)` whose argument count is the i32 in `len`.
fn emit_spread_construct_argc(len: u32, function: &mut Function) {
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::F64Const(Ieee64::from(1.0)));
    function.instruction(&Instruction::F64Add);
    function.instruction(&Instruction::F64Neg);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Push the trailing `argc` argument of a uniform-ABI call frame: the
/// actual source argument `count` as a NUMBER jsval (its `f64` bit pattern),
/// letting a callee's rest parameter collect exactly the real arguments. Pushed
/// after the env slot, before the funcref index for an indirect call.
pub(super) fn emit_argc(count: usize, function: &mut Function) {
    // A call's argument count is tiny (bounded by `K`), so the `usize -> f64`
    // conversion is always exact — well within `f64`'s 52-bit mantissa.
    #[allow(clippy::cast_precision_loss)]
    let bits = (count as f64).to_bits().cast_signed();
    function.instruction(&Instruction::I64Const(bits));
}

/// Emit a first-class function value `IrNode::FuncValue`, leaving its jsval
/// (single heap tag `0b101`, type byte `TYPE_FUNCTION`) on the stack.
/// `__alloc(FUNC_BLOCK_SIZE)` reserves the block, then the object-family prefix
/// (header `flags` `LAZY` plus the function's constructor kind, no properties,
/// `null` prototype), the funcref-table index (at [`FUNC_INDEX_OFFSET`]), and
/// the environment slot (at [`FUNC_ENV_OFFSET`]) are written.
///
/// For a non-capturing function `captures` is empty and the env slot is
/// `undefined`. For a **closure** the environment block is built first (via
/// [`emit_env_block`]) and stashed in an i64 scratch local, then written into
/// the env slot — so the block-building allocation does not clash with the
/// function block's own `dst` scratch.
pub(super) fn emit_func_value(
    table_index: u32,
    captures: &[IrNode],
    kind: FnKind,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let env_tmp = scratch + 6;
    let func_idx_slot = MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    };
    let env_slot = MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    };

    // Build the environment first (if any) into a scratch local, so its own
    // allocation is complete before the function block reuses `dst`.
    if captures.is_empty() {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    } else {
        emit_env_block(captures, function, scratch, strings, helpers);
    }
    function.instruction(&Instruction::LocalSet(env_tmp));

    // dst = __alloc(FUNC_BLOCK_SIZE); leave dst (i32) on the stack.
    function.instruction(&Instruction::I32Const(FUNC_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));

    // Write the TYPE_FUNCTION prefix at dst, keeping dst on the stack.
    let flags = fn_flags(kind);
    function.instruction(&Instruction::LocalTee(dst));
    emit_object_prefix(function, dst, TYPE_FUNCTION | i32::from(flags) << 16);

    // Write the u32 funcref-table index at dst + FUNC_INDEX_OFFSET.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(table_index.cast_signed()));
    function.instruction(&Instruction::I32Store(func_idx_slot));

    // Write the env slot at dst + FUNC_ENV_OFFSET.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(env_tmp));
    function.instruction(&Instruction::I64Store(env_slot));

    // Rebox the function block's base offset (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit a closure environment block, leaving its heap jsval on the
/// stack: `__alloc(HEADER_SIZE + 4 + 8*count)`, the `TYPE_ENV` type byte, the
/// u32 `count`, then each captured cell pointer at `+HEADER_SIZE+4+8*i`. Each
/// capture source expression is a side-effect-free cell-pointer read (a `Local`
/// or `EnvLoad`), so `dst` is not clobbered while they are evaluated.
pub(super) fn emit_env_block(
    captures: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    let count = i32::try_from(captures.len()).unwrap_or(i32::MAX);

    // dst = __alloc(HEADER_SIZE + 4 + 8*count).
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4 + 8 * count));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));

    // Type byte and count header.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ENV));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(count));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));

    // Each captured cell pointer at dst + HEADER_SIZE + 4 + 8*i.
    for (i, capture) in captures.iter().enumerate() {
        function.instruction(&Instruction::LocalGet(dst));
        emit_node(capture, function, scratch, strings, helpers);
        function.instruction(&Instruction::I64Store(MemArg {
            offset: HEADER_SIZE as u64 + 4 + 8 * i as u64,
            align: 3,
            memory_index: 0,
        }));
    }

    // Rebox dst as a heap jsval.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit `IrNode::CellNew`: allocate a boxed cell holding `value`, leaving the
/// cell's heap jsval on the stack. `__alloc(CELL_BLOCK_SIZE)`, the `TYPE_CELL`
/// type byte, the value at `+HEADER_SIZE`, then rebox the base as a heap jsval.
pub(super) fn emit_cell_new(
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    // Evaluate the value first and stash it (`+5`): building it may allocate
    // through the same `dst` scratch.
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch + 5));
    // dst = __alloc(CELL_BLOCK_SIZE).
    function.instruction(&Instruction::I32Const(CELL_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    function.instruction(&Instruction::LocalSet(dst));
    // Type byte.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_CELL));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    // Value at dst + HEADER_SIZE.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(scratch + 5));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 3,
        memory_index: 0,
    }));
    // Rebox dst as a heap jsval.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Emit `<recv>.length`: for a string or array operand (low 32 bits are the
/// block base) read the u32 `len` header at `base + HEADER_SIZE` and rebox it
/// as a number jsval; any other heap operand reads its `length` property
/// through `__get`; a primitive operand yields `undefined` without touching
/// memory. The operand is stashed in the general i64 scratch slot (`+0`).
pub(super) fn emit_str_len(
    operand: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(operand, function, scratch, strings, helpers);
    // A primitive operand (number/boolean) has no header to read: its `.length`
    // is `undefined`.
    function.instruction(&Instruction::LocalSet(scratch));
    emit_is_pointer(function, scratch);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // A string or array (kind byte <= TYPE_ARRAY) reads its length header; any
    // other heap value (a function's `length`, an object's own) is a `__get`.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32LeU);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32Const(static_key(
        strings,
        INDEX_LENGTH_KEY,
    )));
    function.instruction(&Instruction::Call(helpers.get));
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
}

/// Load a boxed cell's value: given a cell heap jsval on the stack, wrap it to a
/// base offset and load the jsval stored at `+HEADER_SIZE`.
pub(super) fn emit_cell_load(function: &mut Function) {
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 3,
        memory_index: 0,
    }));
}

/// Emit `IrNode::EnvLoad`: load the boxed-cell pointer at env slot `slot`. The
/// environment jsval is the closure's last wasm parameter (local
/// `helpers.max_args` under the env-last ABI); its cell pointers begin at
/// `+HEADER_SIZE+4`.
pub(super) fn emit_env_load(slot: u32, function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::LocalGet(helpers.max_args));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(MemArg {
        offset: HEADER_SIZE as u64 + 4 + 8 * u64::from(slot),
        align: 3,
        memory_index: 0,
    }));
}

/// Emit `IrNode::CellSet`: store `value` into the cell at `+HEADER_SIZE` and
/// leave the stored value on the stack. The cell base is computed first, then
/// `value` (saved into an i64 scratch via `tee`) is stored and re-pushed.
pub(super) fn emit_cell_set(
    cell: &IrNode,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let tmp = scratch + 5;
    // base = wrap(cell).
    emit_node(cell, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    // value on top, saved into `tmp` (tee leaves it on the stack).
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalTee(tmp));
    // store value at base + HEADER_SIZE (consumes base and value).
    function.instruction(&Instruction::I64Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 3,
        memory_index: 0,
    }));
    // yield the stored value.
    function.instruction(&Instruction::LocalGet(tmp));
}

/// Emit a call through a function **value** (`IrNode::CallIndirect`), leaving the
/// callee's boxed jsval result on the stack. Under the env-last ABI it pushes
/// the `K` padded args, then evaluates the callee once (stashing its block base
/// in an i32 scratch), pushes the callee's **own environment** as the trailing
/// `env` argument, then the funcref-table index, and `call_indirect`s.
///
/// The callee is evaluated **after** the arguments and its base then read twice
/// (env, then index) with no intervening evaluation, so a nested indirect call
/// in an argument cannot clobber the stashed base.
/// Emit a call to the host `__promise(op, a, b) -> i64` import: push the
/// `op` flag, then evaluate `a` and `b` into the two jsval slots, then `call`.
/// Used by `Promise.resolve`/`reject` (`b` unused), `await` (`b` unused), and
/// `p.finally` (`a` = the promise, `b` = the finalizer).
pub(super) fn emit_promise_call(
    op: i32,
    a: &IrNode,
    b: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::I32Const(op));
    emit_node(a, function, scratch, strings, helpers);
    emit_node(b, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.promise));
}

/// Emit `new Promise(executor)`, leaving the fresh promise's jsval on the
/// stack.
///
/// Steps: (1) `P = __promise(NEW)`, stashed in the dedicated `p_local`; (2) build
/// `resolve`/`reject` FUNCTION values pointing at the settler funcref slots with
/// `P` in their env slot; (3) call `executor(resolve, reject)` synchronously
/// through `call_indirect` (env-last ABI), padding/truncating the two settlers to
/// the fixed `K` positional slots; (4) if the executor left `$exc_pending` set (it
/// threw), reject `P` with the thrown value and clear the flag — a throwing
/// executor rejects the promise rather than propagating; (5) push `P`.
///
/// `p_local` is a real frame local (not scratch), so building the settlers and
/// evaluating the executor cannot clobber the stashed promise.
pub(super) fn emit_promise_ctor(
    executor: &IrNode,
    p_local: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let exec_base = scratch + 7;

    // No funcref table (a function-less program): the executor can never be a
    // function value, so throw instead of emitting an invalid `call_indirect`.
    // Allocating P first is unobservable, so it is skipped.
    if !helpers.has_funcs {
        emit_node(executor, function, scratch, strings, helpers);
        emit_throw_not_a_function(
            function,
            EXPRESSION_NOT_A_FUNCTION,
            scratch,
            strings,
            helpers,
        );
        return;
    }

    // P = __promise(NEW, undefined, undefined); stash it in p_local.
    function.instruction(&Instruction::I32Const(PROMISE_OP_NEW));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Call(helpers.promise));
    function.instruction(&Instruction::LocalSet(p_local));

    // Push the K padded positional args: resolve (a0), reject (a1), then undefined
    // padding — dropping any settler beyond K (JS ignores extra args), exactly as
    // `emit_call_args` would for a two-argument call.
    let k = helpers.max_args as usize;
    emit_settler_value(
        helpers.settle_resolve_slot,
        p_local,
        function,
        scratch,
        helpers,
    );
    if k < 1 {
        function.instruction(&Instruction::Drop);
    }
    emit_settler_value(
        helpers.settle_reject_slot,
        p_local,
        function,
        scratch,
        helpers,
    );
    if k < 2 {
        function.instruction(&Instruction::Drop);
    }
    for _ in 2..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }

    // Evaluate the executor once, stash its block base, and call it env-last. A
    // non-function executor throws synchronously (DF4).
    emit_node(executor, function, scratch, strings, helpers);
    if !is_static_function(executor) {
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
    function.instruction(&Instruction::LocalSet(exec_base));
    emit_reset_this(function);
    function.instruction(&Instruction::LocalGet(exec_base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    emit_argc(2, function);
    function.instruction(&Instruction::LocalGet(exec_base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The executor's return value is ignored.
    function.instruction(&Instruction::Drop);

    // If the executor threw, its exception is caught HERE and turned into a
    // rejection of P (not propagated): reject P with `$exc_value`, then clear the
    // pending flag so execution continues normally with P (now rejected).
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(PROMISE_OP_SETTLE_REJECT));
    function.instruction(&Instruction::LocalGet(p_local));
    function.instruction(&Instruction::GlobalGet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::Call(helpers.promise));
    function.instruction(&Instruction::Drop);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::End);

    // The `new Promise` expression yields P.
    function.instruction(&Instruction::LocalGet(p_local));
}

/// Emit a `new Promise` settler FUNCTION value, leaving its heap jsval on
/// the stack: a `TYPE_FUNCTION` block whose funcref-table index is the settler
/// slot `table_index` and whose ENV slot holds the bound promise (read from
/// `p_local`) rather than a closure environment. Mirrors [`emit_func_value`] but
/// stores `p_local` in the env slot directly.
fn emit_settler_value(
    table_index: u32,
    p_local: u32,
    function: &mut Function,
    scratch: u32,
    helpers: Helpers<'_>,
) {
    let dst = scratch + 3;
    // dst = __alloc(FUNC_BLOCK_SIZE); leave dst (i32) on the stack.
    function.instruction(&Instruction::I32Const(FUNC_BLOCK_SIZE));
    function.instruction(&Instruction::Call(helpers.alloc));
    // Write the TYPE_FUNCTION prefix at dst (a lazy anonymous non-constructor,
    // its info entry right after the user functions'), keeping dst on the stack.
    function.instruction(&Instruction::LocalTee(dst));
    emit_object_prefix(
        function,
        dst,
        TYPE_FUNCTION | i32::from(abi::FUNC_FLAG_LAZY) << 16,
    );
    // Write the u32 funcref-table index at dst + FUNC_INDEX_OFFSET.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(table_index.cast_signed()));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    // Write the bound promise into the env slot at dst + FUNC_ENV_OFFSET.
    function.instruction(&Instruction::LocalTee(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(p_local));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Rebox the block base (still on the stack) as a heap jsval.
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

pub(super) fn emit_call_indirect(
    callee: &IrNode,
    args: &[IrNode],
    not_callable: &CalleeError,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let base = scratch + 7;
    // K padded argument slots (deepest on the stack).
    emit_call_args(args, function, scratch, strings, helpers);
    // Evaluate the callee once; stash its block base.
    emit_node(callee, function, scratch, strings, helpers);
    // A program that declares no function has no funcref table, so the callee
    // can never be a function value: throw the not-a-function TypeError
    // instead of emitting an invalid `call_indirect`. The transfer leaves the
    // stack polymorphic, so the discarded operands need no cleanup.
    if !helpers.has_funcs {
        emit_throw_not_a_function(
            function,
            callee_error_text(not_callable),
            scratch,
            strings,
            helpers,
        );
        return;
    }
    // IsCallable (DF4): a non-function callee throws instead of dispatching its
    // low bits as a funcref index. The general i64 scratch slot (+0) is free:
    // every argument is already on the stack.
    if !is_static_function(callee) {
        function.instruction(&Instruction::LocalSet(scratch));
        emit_callable_guard(
            function,
            callee_error_text(not_callable),
            scratch,
            scratch,
            strings,
            helpers,
        );
        function.instruction(&Instruction::LocalGet(scratch));
    }
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    // A plain indirect call binds `this = undefined` — method calls take the
    // dedicated `emit_method_call_user` path instead.
    emit_reset_this(function);
    // Trailing env argument: the callee's own environment (at FUNC_ENV_OFFSET).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI: after env, before the funcref index).
    emit_argc(args.len(), function);
    // Funcref-table index (at FUNC_INDEX_OFFSET), on top for call_indirect.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    // The called function value can throw; unwind if it left an exception pending.
    emit_propagate_if_pending(function, helpers);
}

/// Throw the not-a-function `TypeError` with `text` (see
/// [`callee_type_error`]) and unwind to the nearest handler. Used by the
/// callable guards and where a program with no funcref table would otherwise
/// need a `call_indirect`; the transfer leaves the stack polymorphic, so any
/// discarded operands need no cleanup.
pub(super) fn emit_throw_not_a_function(
    function: &mut Function,
    text: CalleeErrorText<'_>,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(
        &callee_type_error(text),
        function,
        scratch,
        strings,
        helpers,
    );
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
    emit_throw_return(function, helpers);
}

/// Emit the `IsCallable` check guarding an indirect call (DF4): unless the jsval in
/// `local` is a heap FUNCTION value, throw the not-a-function `TypeError` with
/// `text` (see [`emit_throw_not_a_function`]) and unwind. Stack-neutral on the
/// not-thrown path. The throw path builds the error object in the low scratch
/// slots, so `local` may itself be a scratch slot the caller re-reads afterwards
/// — that read only runs when nothing was thrown.
pub(super) fn emit_callable_guard(
    function: &mut Function,
    text: CalleeErrorText<'_>,
    local: u32,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_is_function(function, local);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    // The `if` opens a control frame around the throw transfer; count it so the
    // handler-relative `br` uses the right nesting depth.
    enter_frame(helpers);
    emit_throw_not_a_function(function, text, scratch, strings, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Whether `callee` is a function literal, so its value is statically a
/// function and an indirect call through it needs no `IsCallable` check.
pub(super) fn is_static_function(callee: &IrNode) -> bool {
    matches!(callee, IrNode::FuncValue { .. } | IrNode::DeclFunc(_))
}

/// Emit a call whose argument list contains a spread `f(...a)`, leaving
/// the callee's boxed jsval result on the stack. The `args` node (an
/// [`IrNode::ArraySpread`]) is materialized into a heap array; its elements are
/// then spread into the uniform ABI's fixed `K` positional slots (padding with
/// `undefined`, dropping any beyond `K`) with the array's length passed as the
/// dynamic `argc`. A `Direct` target `call`s a known function index; an
/// `Indirect` target evaluates the callee first (JS order) and `call_indirect`s.
pub(super) fn emit_call_spread(
    target: &SpreadCallee,
    args: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    // The argument array's block base and element count (reusing the array
    // higher-order loop's i32 slots, idle here). A `Spread` source inside `args`
    // finishes evaluating — including any nested HOF that also uses these — before
    // the array is stashed, so no live value is clobbered.
    let base = scratch + 8;
    let len = scratch + 9;

    match target {
        SpreadCallee::Direct(func) => {
            emit_spread_args_setup(args, base, len, function, scratch, strings, helpers);
            emit_spread_arg_slots(base, len, function, helpers);
            // A plain (non-method) call binds `this = undefined`.
            emit_reset_this(function);
            // A top-level declaration never captures, so its env slot is `undefined`.
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            emit_spread_argc(len, function);
            function.instruction(&Instruction::Call(*func + IMPORT_FUNC_COUNT));
            emit_propagate_if_pending(function, helpers);
        }
        SpreadCallee::Indirect(callee, _) | SpreadCallee::This { callee, .. } => {
            let cb = scratch + 7;
            let text = match target {
                SpreadCallee::Indirect(_, not_callable) => callee_error_text(not_callable),
                _ => EXPRESSION_NOT_A_FUNCTION,
            };
            // The callee is evaluated first (JS order) and its base stashed, then
            // the argument array is built and read.
            emit_node(callee, function, scratch, strings, helpers);
            // No funcref table (a function-less program): the callee can never
            // be a function value, so throw instead of an invalid `call_indirect`.
            if !helpers.has_funcs {
                emit_throw_not_a_function(function, text, scratch, strings, helpers);
                return;
            }
            // IsCallable (DF4), checked while the full callee jsval is at hand.
            if !is_static_function(callee) {
                function.instruction(&Instruction::LocalSet(scratch));
                emit_callable_guard(function, text, scratch, scratch, strings, helpers);
                function.instruction(&Instruction::LocalGet(scratch));
            }
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::LocalSet(cb));
            emit_spread_args_setup(args, base, len, function, scratch, strings, helpers);
            emit_spread_arg_slots(base, len, function, helpers);
            // A plain indirect call binds `this = undefined`; `super(...)`
            // keeps the enclosing constructor's `this`.
            if let SpreadCallee::This { this_local, .. } = target {
                function.instruction(&Instruction::LocalGet(*this_local));
                function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
            } else {
                emit_reset_this(function);
            }
            // Trailing env argument: the callee's own environment (FUNC_ENV_OFFSET).
            function.instruction(&Instruction::LocalGet(cb));
            function.instruction(&Instruction::I64Load(MemArg {
                offset: FUNC_ENV_OFFSET,
                align: 3,
                memory_index: 0,
            }));
            // `super(...)` passes the construct marker.
            if matches!(target, SpreadCallee::This { .. }) {
                emit_spread_construct_argc(len, function);
            } else {
                emit_spread_argc(len, function);
            }
            // Funcref-table index (FUNC_INDEX_OFFSET), on top for call_indirect.
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
            emit_propagate_if_pending(function, helpers);
        }
    }
}

/// Build a spread call's argument array (the `args` [`IrNode::ArraySpread`]) and
/// stash its block base into `base` and its element count into `len`.
fn emit_spread_args_setup(
    args: &IrNode,
    base: u32,
    len: u32,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalSet(len));
}

/// Push the uniform ABI's `K` positional slots from a spread call's argument
/// array: slot `i` is `array[i]` (the i64 at `base + HEADER_SIZE + 4 + 8*i`)
/// when `i < len`, else `undefined`. Any element beyond `K` is left in the
/// array and never read (JS ignores extra arguments a function does not bind).
fn emit_spread_arg_slots(base: u32, len: u32, function: &mut Function, helpers: Helpers<'_>) {
    for i in 0..helpers.max_args {
        // len > i ? array[i] : undefined.
        function.instruction(&Instruction::LocalGet(len));
        function.instruction(&Instruction::I32Const(i.cast_signed()));
        function.instruction(&Instruction::I32GtU);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        function.instruction(&Instruction::LocalGet(base));
        function.instruction(&Instruction::I64Load(MemArg {
            offset: (HEADER_SIZE + 4) as u64 + 8 * u64::from(i),
            align: 0,
            memory_index: 0,
        }));
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::End);
    }
}

/// Push a spread call's dynamic argument count `len` as a NUMBER jsval (its
/// `f64` bit pattern), so a callee's rest parameter collects exactly the
/// real spread arguments.
fn emit_spread_argc(len: u32, function: &mut Function) {
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::F64ConvertI32U);
    function.instruction(&Instruction::I64ReinterpretF64);
}

/// Emit a `console.log(a0, .., a_{n-1})` expression, leaving the
/// `undefined` jsval (its expression value) on the stack.
///
/// Each argument is evaluated left-to-right and passed to the host `print`
/// import as `print(jsval_bits, flag)`: every argument but the last uses
/// `PRINT_FLAG_SEP` (value + a single space) and the last uses `PRINT_FLAG_END`
/// (value + newline). A zero-argument call emits one `print(undefined,
/// PRINT_FLAG_NEWLINE)` so the line is just a newline. The host formats the
/// jsval (reusing `format_value`) and reads linear memory for pointer jsvals.
pub(super) fn emit_console_log(
    args: &[IrNode],
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    if args.is_empty() {
        // `console.log()` prints just a newline; the jsval operand is ignored, so
        // a dummy `undefined` satisfies the import's `(i64, i32)` signature.
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::I32Const(PRINT_FLAG_NEWLINE));
        function.instruction(&Instruction::Call(helpers.print));
    } else {
        let last = args.len() - 1;
        for (i, arg) in args.iter().enumerate() {
            emit_node(arg, function, scratch, strings, helpers);
            let flag = if i == last {
                PRINT_FLAG_END
            } else {
                PRINT_FLAG_SEP
            };
            function.instruction(&Instruction::I32Const(flag));
            function.instruction(&Instruction::Call(helpers.print));
        }
    }
    // `console.log(..)` evaluates to `undefined`.
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
}

/// Emit a `typeof operand` expression, leaving a real **heap-string** jsval (tag
/// `0b101`, `TYPE_STRING`) naming the operand's type on the stack. The operand is
/// evaluated, stashed, mapped to a `TYPEOF_STRINGS` index, and that index selects
/// the INTERNED data-segment offset of the matching word — so a `typeof` result is
/// an ordinary string with no distinct representation: `===`, `+`, `.length`, and
/// `console.log` all flow through the existing string machinery (no `typeof`
/// special-casing anywhere). The six words are interned by `collect_node` whenever
/// a program uses `typeof`, so this allocates nothing per call.
///
/// The concrete heap kind is a TYPE BYTE, not the NaN-box tag, so
/// `typeof` splits on whether the value is a heap pointer (single tag `0b101`):
///
/// - **Heap pointer** — read the type byte and map it to the `TYPEOF_STRINGS`
///   index: `TYPE_STRING` -> 4 ("string"), `TYPE_ARRAY`/`TYPE_OBJECT` -> 3
///   ("object"), `TYPE_FUNCTION` -> 5 ("function").
/// - **Immediate** — the NaN-box tag orders boolean(1), null(2), undefined(3),
///   while `TYPEOF_STRINGS` orders boolean(1), undefined(2), object(3); booleans
///   line up but null/undefined are swapped, so `boxed index = tag ^ ((tag >> 1)
///   & 1)` flips the low bit exactly when the tag's middle bit is set (2<->3),
///   leaving 0 and 1 untouched. A number (any value without the full QNAN mask)
///   maps to index 0 ("number").
pub(super) fn emit_typeof_node(
    operand: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    emit_node(operand, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));

    // is_heap = is_boxed(bits) & (tag == 0b101).
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Eq);
    emit_tag_eq(function, scratch, 0b101);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // HEAP: map the type byte -> typeof index. Base is 3 ("object", array/object);
    // override string -> 4 and function -> 5 with layered selects.
    function.instruction(&Instruction::I64Const(4)); // val1: "string"
    function.instruction(&Instruction::I64Const(5)); // val1: "function"
    function.instruction(&Instruction::I64Const(3)); // val2: base "object"
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_FUNCTION));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select); // function ? 5 : 3
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select); // string ? 4 : (function ? 5 : 3)
    function.instruction(&Instruction::Else);
    // IMMEDIATE: derived = tag ^ ((tag >> 1) & 1); is_boxed ? derived : 0.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(48));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(0b111));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(49));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(1));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Xor);
    // val2 = 0 ("number"); cond = is_boxed.
    function.instruction(&Instruction::I64Const(0));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::End);

    // Map the `TYPEOF_STRINGS` index (now on the stack) to the interned heap-string
    // pointer for that word: stash the index, then a branchless nested-`select` fold
    // picks `HEAP_BITS | <offset>` for the matching entry (each step keeps the
    // accumulator as the deeper operand, testing `index != k`). `collect_node`
    // interns all six words when the program uses `typeof`; the `HEAP_BASE` fallback
    // keeps it valid regardless.
    function.instruction(&Instruction::LocalSet(scratch));
    let word_ptr = |k: usize| {
        let offset = strings
            .get(crate::abi::TYPEOF_STRINGS[k])
            .copied()
            .unwrap_or(HEAP_BASE);
        HEAP_BITS | i64::from(offset)
    };
    function.instruction(&Instruction::I64Const(word_ptr(0)));
    for k in 1..crate::abi::TYPEOF_STRINGS.len() {
        function.instruction(&Instruction::I64Const(word_ptr(k)));
        function.instruction(&Instruction::LocalGet(scratch));
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::I32Const(i32::try_from(k).unwrap_or(0)));
        function.instruction(&Instruction::I32Ne);
        function.instruction(&Instruction::Select);
    }
}
