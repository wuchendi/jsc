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
    strings: &StrOffsets,
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
            let offset = strings.get_str(text).copied().unwrap_or(HEAP_BASE);
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
        IrNode::ArrayHole => {
            function.instruction(&Instruction::I64Const(HOLE_BITS));
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
            emit_argc(call_argc(args.len(), helpers), function);
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
            function.instruction(&Instruction::I64Const(intrinsic_bits(
                Intrinsic::ErrorPrototype(*kind),
                helpers,
            )));
        }
        IrNode::Intrinsic(global) => {
            let bits = intrinsic_bits(Intrinsic::from(*global), helpers);
            function.instruction(&Instruction::I64Const(bits));
        }
        IrNode::IteratorSymbol => {
            let addr = realm::symbol_addr(realm::WellKnown::Iterator, helpers);
            function.instruction(&Instruction::I64Const(
                HEAP_BITS | i64::from(addr.cast_unsigned()),
            ));
        }
        IrNode::ArrayValues => {
            let values = Builtin::Iter(realm::IterOp::ArrayIter(realm::IterKind::Values));
            function.instruction(&Instruction::I64Const(realm::method_bits(values, helpers)));
        }
        IrNode::GlobalGet {
            global,
            typeof_operand,
        } => {
            emit_global_get(global, *typeof_operand, function, scratch, strings, helpers);
        }
        IrNode::GlobalSet { global, value } => {
            emit_global_set(global, value, function, scratch, strings, helpers);
        }
        IrNode::GlobalDeclare { name, value } => {
            emit_global_declare(name, value.as_deref(), function, scratch, strings, helpers);
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
            limits,
        } => {
            generator::emit_make_generator(
                *sm_table_index,
                slots,
                *total,
                *limits,
                function,
                scratch,
                strings,
                helpers,
            );
        }
        IrNode::GetIterator { value, rec, sealed } => {
            builtins::emit_get_iterator(value, rec, *sealed, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        IrNode::OpenIterator {
            value,
            rec,
            is_async,
        } => {
            if *is_async {
                builtins::emit_open_async_iterator(value, rec, function, scratch, strings, helpers);
            } else {
                builtins::emit_open_iterator(value, rec, function, scratch, strings, helpers);
            }
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        IrNode::AsyncIteratorNext(rec) => {
            builtins::emit_async_iterator_next(rec, function, scratch, strings, helpers);
        }
        IrNode::AsyncIteratorComplete { rec, result } => {
            builtins::emit_async_iterator_complete(
                rec, result, function, scratch, strings, helpers,
            );
            function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::Else);
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::End);
        }
        IrNode::AsyncIteratorReturn { rec, throwing } => {
            builtins::emit_async_iterator_return(
                rec, *throwing, function, scratch, strings, helpers,
            );
        }
        IrNode::IteratorResultCheck(operand) => {
            builtins::emit_iterator_result_check(operand, function, scratch, strings, helpers);
        }
        IrNode::IteratorStep(rec) => {
            builtins::emit_iterator_step(rec, function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
            function.instruction(&Instruction::I64Const(TRUE_BITS));
            function.instruction(&Instruction::Else);
            function.instruction(&Instruction::I64Const(FALSE_BITS));
            function.instruction(&Instruction::End);
        }
        IrNode::IteratorValue(rec) => {
            builtins::emit_iterator_value(rec, function, scratch, strings, helpers);
        }
        IrNode::IteratorClose { rec, throwing } => {
            let mode = if *throwing {
                builtins::CloseMode::Catch
            } else {
                builtins::CloseMode::Normal
            };
            builtins::emit_iterator_close(rec, mode, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        }
        IrNode::GenResume { handle, sent } => {
            generator::emit_gen_resume(handle, sent, function, scratch, strings, helpers);
        }
        // `Yield` is consumed by the generator segmentation pass; reaching emit
        // means an unsupported yield position slipped past codegen's diagnostics.
        IrNode::Yield(_) | IrNode::YieldDelegate { .. } => {
            unreachable!("a `yield` survived to emit — codegen must reject it first")
        }
        IrNode::AwaitMark(operand) => {
            emit_node(operand, function, scratch, strings, helpers);
            generator::emit_await_mark(function);
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
        IrNode::ConsoleLogSpread { args } => {
            emit_console_log_spread(args, function, scratch, strings, helpers);
        }
        IrNode::ArrayPush { local, value } => {
            emit_array_push_node(*local, value, function, scratch, strings, helpers);
        }
        IrNode::ArrayPop { local } => {
            emit_array_pop_node(*local, function, scratch, helpers);
        }
        IrNode::ArrayHof {
            op,
            array,
            callback,
            init,
            this_arg,
        } => {
            emit_array_hof(
                *op,
                array,
                callback,
                init.as_deref(),
                this_arg.as_deref(),
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
        node @ IrNode::NamespaceGuard { .. } => {
            emit_namespace_guard(node, function, scratch, strings, helpers);
        }
        node @ IrNode::ProtoMethod { .. } => {
            emit_proto_method(node, function, scratch, strings, helpers);
        }
        node @ IrNode::ProtoGuard { .. } => {
            emit_proto_guard(node, function, scratch, strings, helpers);
        }
        IrNode::ToObject(operand) => {
            emit_node(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::Call(helpers.to_object()));
        }
        IrNode::ToPropertyKey(operand) => {
            emit_node(operand, function, scratch, strings, helpers);
            function.instruction(&Instruction::I32Const(1));
            function.instruction(&Instruction::Call(helpers.to_key));
            emit_propagate_if_pending(function, helpers);
            function.instruction(&Instruction::I64ExtendI32U);
            function.instruction(&Instruction::I64Const(HEAP_BITS));
            function.instruction(&Instruction::I64Or);
        }
        IrNode::SetFunctionName {
            func,
            key,
            prefix,
            local,
        } => {
            emit_node(func, function, scratch, strings, helpers);
            emit_set_function_name(*key, prefix, *local, function, strings, helpers);
        }
        IrNode::ThisGlobal => {
            // Snapshot read of the `this`-binding carrier; emitted only in a
            // function/arrow prologue.
            function.instruction(&Instruction::GlobalGet(THIS_GLOBAL));
        }
        IrNode::NewTargetGlobal => {
            function.instruction(&Instruction::GlobalGet(NEW_TARGET_GLOBAL));
        }
        IrNode::SpreadArgsGlobal => {
            function.instruction(&Instruction::GlobalGet(SPREAD_ARGS_GLOBAL));
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
        IrNode::SizeRead { sealed, obj } => {
            emit_size_read(*sealed, obj, function, scratch, strings, helpers);
        }
        IrNode::CollectionOp { op, handle, args } => {
            emit_map_op(*op, *handle, args, function, scratch, strings, helpers);
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
        // millisecond payload. `DateNew`/`DateNow` seed from `arg` or the host clock;
        // `DateField` computes a field from a Date receiver's payload.
        IrNode::DateNew { arg } => {
            emit_date_new_node(arg.as_deref(), function, scratch, strings, helpers);
        }
        IrNode::DateNow => emit_date_now_node(function, scratch, helpers),
        IrNode::DateField { op, recv } => {
            emit_date_field(*op, *recv, function, scratch, strings, helpers);
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
    strings: &StrOffsets,
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
/// callee's boxed jsval result on the stack. JS order (ECMA-262 13.3.6.1): the
/// receiver is evaluated once into the stable frame local `obj_local`, the
/// callee — the function value at member `key` of the receiver — is read via
/// the ordinary object-member path into the frame local `callee_local`, then
/// the `K` padded argument slots are pushed (env-last ABI), then `IsCallable`
/// is checked and `__this` is set to the receiver immediately before the
/// `call_indirect`, so nothing evaluated afterward can clobber it. The callee's
/// own environment is the trailing `env` argument, then `argc`, then the funcref
/// index.
pub(super) fn emit_method_call_user(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let IrNode::MethodCallUser {
        obj,
        key,
        args,
        obj_local,
        callee_local,
        not_callable,
    } = node
    else {
        unreachable!("emit_method_call_user is only called with a MethodCallUser node")
    };
    let (obj_local, callee_val) = (*obj_local, *callee_local);
    // Evaluate the receiver ONCE into a stable frame local (survives argument
    // evaluation, the member read, and the `__this` set).
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(obj_local));
    // A nullish receiver throws on the method lookup, before any argument runs.
    // The key is only evaluated here on the throw path.
    emit_nullish_guard(function, obj_local, key, false, scratch, strings, helpers);

    // Read the callee — the function value at `receiver[key]` — via the
    // ordinary object-member read (already nullish-guarded above) before any
    // argument runs, into its stable frame local.
    emit_index_node(
        &IrNode::Local(obj_local),
        key,
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    function.instruction(&Instruction::LocalSet(callee_val));

    // K padded argument slots (deepest on the stack). Nested method calls here set
    // `__this`, but it is overwritten below after all arguments are evaluated.
    emit_call_args(args, function, scratch, strings, helpers);

    // Runtime guard: the resolved member MUST be a function value. For a
    // dynamic-shape receiver — a `new`-instance, a reassigned binding, a computed
    // key, or a call result — the compile-time unknown-method guards
    // cannot prove the method absent, so `receiver[key]` may read `undefined` (or
    // any non-function). Dispatching that as a funcref index is undefined
    // behaviour; instead throw a clean `TypeError` (after the arguments, as
    // `IsCallable` runs in JS) and unwind to the nearest handler (or the
    // program boundary), exactly like `throw`.
    emit_bound_call(
        (callee_val, obj_local),
        args.len(),
        callee_error_text(not_callable),
        function,
        scratch,
        strings,
        helpers,
    );
}

/// With the `K` argument slots of an `argc`-argument call pushed, call the
/// jsval in local `callee` with `this` = the jsval in local `this`: the
/// `IsCallable` guard (throwing the `TypeError` `text`), then `__this` set
/// immediately before the `call_indirect`, the callee's own environment as
/// the trailing `env` argument, `argc` and the funcref index; unwinds if the
/// callee left an exception pending.
fn emit_bound_call(
    (callee_val, this_local): (u32, u32),
    argc: usize,
    text: CalleeErrorText<'_>,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let base = scratch + 7;
    emit_callable_guard(function, text, callee_val, scratch, strings, helpers);

    // Not-thrown path: proceed with the callee's block base (its wrapped pointer).
    function.instruction(&Instruction::LocalGet(callee_val));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));

    // Bind `this` immediately before the call (after every sub-eval).
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
    emit_argc(call_argc(argc, helpers), function);
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
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let IrNode::ThisCall {
        callee,
        args,
        this_local,
        construct,
        not_callable,
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

    // Runtime guard: the callee MUST be a function value — a `super.m` that
    // resolves to `undefined` (missing method) throws a TypeError instead
    // of dispatching a bogus funcref index. Identical to the method-call guard.
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
        emit_construct_argc(call_argc(args.len(), helpers), function);
    } else {
        emit_argc(call_argc(args.len(), helpers), function);
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

/// Emit a guarded namespace member call ([`IrNode::NamespaceGuard`], design
/// 3.2 row 2), leaving its result on the stack. The namespace value and the
/// arguments are evaluated once into their scratch locals; the fast path runs
/// when the value is the intrinsic with its dirty bit clear (a watched
/// namespace), or when the member read from it is still the builtin method.
pub(super) fn emit_namespace_guard(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let IrNode::NamespaceGuard {
        namespace,
        key,
        obj,
        obj_local,
        member_local,
        args,
        arg_locals,
        fast,
        slow,
    } = node
    else {
        unreachable!("emit_namespace_guard is only called with a NamespaceGuard node")
    };
    // `obj.key` into `member_local` (JS reads the callee before the
    // arguments).
    let read = |function: &mut Function| {
        emit_index_node(
            &IrNode::Local(*obj_local),
            &IrNode::Str(key.as_str().into()),
            true,
            function,
            scratch,
            strings,
            helpers,
        );
        function.instruction(&Instruction::LocalTee(*member_local));
    };
    let emit_args = |function: &mut Function| {
        for (arg, &local) in args.iter().zip(arg_locals) {
            emit_node(arg, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(local));
        }
    };
    emit_node(obj, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(*obj_local));
    let intrinsic = Intrinsic::from(*namespace);
    let Some(builtin) = realm::method_of(intrinsic, key) else {
        read(function);
        function.instruction(&Instruction::Drop);
        emit_args(function);
        emit_node(slow, function, scratch, strings, helpers);
        return;
    };
    // `obj.key === %builtin%`: the member read, compared with the constant.
    let read_compare = |function: &mut Function| {
        read(function);
        function.instruction(&Instruction::I64Const(realm::method_bits(builtin, helpers)));
        function.instruction(&Instruction::I64Eq);
    };
    if let Some(bit) = intrinsic.watch_bit() {
        // The intrinsic itself, never written: no member read needed.
        function.instruction(&Instruction::LocalGet(*obj_local));
        function.instruction(&Instruction::I64Const(intrinsic_bits(intrinsic, helpers)));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::GlobalGet(DIRTY_GLOBAL));
        function.instruction(&Instruction::I32Const(1 << bit));
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        function.instruction(&Instruction::I32Const(1));
        function.instruction(&Instruction::Else);
        enter_frame(helpers);
        read_compare(function);
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    } else {
        read_compare(function);
    }
    // The test result waits on the stack while the arguments run.
    emit_args(function);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    emit_node(fast, function, scratch, strings, helpers);
    function.instruction(&Instruction::Else);
    emit_node(slow, function, scratch, strings, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// The realm prototypes a receiver of `kind` must inherit from directly,
/// each with the kind byte it pairs with.
fn proto_kind_cases(kind: ProtoKind) -> &'static [(i32, Intrinsic)] {
    const ARRAY: [(i32, Intrinsic); 1] = [(TYPE_ARRAY, Intrinsic::Prototype(BuiltinCtor::Array))];
    const MAP: [(i32, Intrinsic); 1] = [(TYPE_MAP, Intrinsic::Prototype(BuiltinCtor::Map))];
    const SET: [(i32, Intrinsic); 1] = [(TYPE_SET, Intrinsic::Prototype(BuiltinCtor::Set))];
    const COLLECTION: [(i32, Intrinsic); 2] = [MAP[0], SET[0]];
    const PROMISE: [(i32, Intrinsic); 1] = [(
        abi::TYPE_PROMISE as i32,
        Intrinsic::Prototype(BuiltinCtor::Promise),
    )];
    const DATE: [(i32, Intrinsic); 1] = [(TYPE_DATE, Intrinsic::Prototype(BuiltinCtor::Date))];
    const GENERATOR: [(i32, Intrinsic); 1] = [(TYPE_GENERATOR, Intrinsic::GeneratorPrototype)];
    match kind {
        ProtoKind::Array => &ARRAY,
        ProtoKind::Map => &MAP,
        ProtoKind::Set => &SET,
        ProtoKind::Collection => &COLLECTION,
        ProtoKind::Promise => &PROMISE,
        ProtoKind::Date => &DATE,
        ProtoKind::Generator => &GENERATOR,
    }
}

/// Push i32 `1` when the jsval in `local` is a receiver of `kind` whose
/// builtin methods are provably the realm's (DESIGN.md §6.4): a heap block of the
/// kind byte with no own properties (`props == 0`, one i64 load of the header
/// and `props` words; an array also not in dictionary mode) whose `proto` is
/// the family's prototype and, unless
/// `sealed`, whose dirty bit is clear. `tmp` is a free i32 local.
pub(super) fn emit_proto_kind_test(
    function: &mut Function,
    local: u32,
    kind: ProtoKind,
    sealed: bool,
    tmp: u32,
    helpers: Helpers<'_>,
) {
    if kind == ProtoKind::Generator {
        emit_generator_kind_test(function, local, sealed, tmp, helpers);
        return;
    }
    let cases = proto_kind_cases(kind);
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    for (i, &(type_byte, prototype)) in cases.iter().enumerate() {
        if i == 0 {
            function.instruction(&Instruction::LocalGet(local));
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::LocalTee(tmp));
        } else {
            function.instruction(&Instruction::LocalGet(tmp));
        }
        function.instruction(&Instruction::I64Load(MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        function.instruction(&Instruction::I64Const(if type_byte == TYPE_ARRAY {
            ARRAY_KIND_AND_PROPS_MASK
        } else {
            KIND_AND_PROPS_MASK
        }));
        function.instruction(&Instruction::I64And);
        function.instruction(&Instruction::I64Const(i64::from(type_byte)));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::LocalGet(tmp));
        function.instruction(&Instruction::I64Load(MemArg {
            offset: OBJ_PROTO_OFFSET,
            align: 3,
            memory_index: 0,
        }));
        function.instruction(&Instruction::I64Const(intrinsic_bits(prototype, helpers)));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::I32And);
        if i > 0 {
            function.instruction(&Instruction::I32Or);
        }
    }
    // An array fast path reads a hole as absent, which holds while neither
    // `%Array.prototype%` nor `%Object.prototype%` has changed (DESIGN.md
    // §6.4).
    let bits = cases
        .iter()
        .flat_map(|&(type_byte, prototype)| {
            let object = (type_byte == TYPE_ARRAY).then_some(Intrinsic::ObjectPrototype);
            [Some(prototype), object]
        })
        .flatten()
        .filter_map(Intrinsic::watch_bit)
        .fold(0, |bits, bit| bits | (1 << bit));
    if !sealed && bits != 0 {
        function.instruction(&Instruction::GlobalGet(DIRTY_GLOBAL));
        function.instruction(&Instruction::I32Const(bits));
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::I32And);
    }
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// [`emit_proto_kind_test`] for [`ProtoKind::Generator`]: a
/// `TYPE_GENERATOR` without own properties whose `proto` is
/// `%GeneratorPrototype%`, or an object without own properties (its
/// function's `prototype`) whose `proto` is `%GeneratorPrototype%`, so
/// `next` resolves to the builtin either way.
fn emit_generator_kind_test(
    function: &mut Function,
    local: u32,
    sealed: bool,
    tmp: u32,
    helpers: Helpers<'_>,
) {
    let prototype = Intrinsic::GeneratorPrototype;
    let proto_load = Instruction::I64Load(MemArg {
        offset: OBJ_PROTO_OFFSET,
        align: 3,
        memory_index: 0,
    });
    let header_load = Instruction::I64Load(MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    });
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(tmp));
    function.instruction(&header_load);
    function.instruction(&Instruction::I64Const(KIND_AND_PROPS_MASK));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(i64::from(TYPE_GENERATOR)));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&proto_load);
    function.instruction(&Instruction::I64Const(intrinsic_bits(prototype, helpers)));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::Else);
    // One hop: the generator function's `prototype` (a heap object).
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&proto_load);
    function.instruction(&Instruction::I64Const(48));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(HEAP_BITS >> 48));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&proto_load);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(tmp));
    function.instruction(&header_load);
    function.instruction(&Instruction::I64Const(PROPS_MASK));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Eqz);
    function.instruction(&Instruction::LocalGet(tmp));
    function.instruction(&proto_load);
    function.instruction(&Instruction::I64Const(intrinsic_bits(prototype, helpers)));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    if !sealed && let Some(bit) = prototype.watch_bit() {
        function.instruction(&Instruction::GlobalGet(DIRTY_GLOBAL));
        function.instruction(&Instruction::I32Const(1 << bit));
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::I32And);
    }
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Emit a builtin method call with guarded fast paths ([`IrNode::ProtoGuard`],
/// DESIGN.md §6.4), leaving its result on the stack. Each fast path runs
/// while `recv` passes its [`emit_proto_kind_test`]. Every other receiver
/// takes ONE call: the argument slots,
/// `this = recv`, the packed `env` (interned key, not-a-function message) and
/// `argc`, through the out-of-line generic method call — or, when `key` is a
/// `String.prototype` builtin, straight into that builtin for a string
/// receiver while `%String.prototype%` is clean (the `call_indirect` slot is a
/// `select`); when the arguments already have the types the builtin coerces
/// to, that string receiver runs the builtin's host op inline instead.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_proto_guard(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let IrNode::ProtoGuard {
        sealed,
        recv,
        key,
        args,
        fast,
        callee,
        not_callable,
    } = node
    else {
        unreachable!("emit_proto_guard is only called with a ProtoGuard node")
    };
    let local = if let IrNode::Local(local) = **recv {
        local
    } else {
        emit_node(recv, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(scratch));
        scratch
    };
    let string_guard = |function: &mut Function| {
        emit_string_receiver_test(function, local, *sealed);
    };
    // With an early callee read: whether it found the fast path `slot`'s
    // builtin (its marker, see [`early_marker`]).
    let found = |function: &mut Function, slot: usize| {
        if let Some(callee) = callee {
            emit_node(callee, function, scratch, strings, helpers);
            function.instruction(&Instruction::I64Const(early_marker(slot)));
            function.instruction(&Instruction::I64Eq);
        }
    };
    let string = realm::method_of(realm::prototype_of(BuiltinCtor::String), key);
    let mut opened = 0;
    // Arguments already of the types the builtin coerces to: on a string
    // receiver the builtin is exactly its host op, inline.
    let inline_string = match string {
        Some(Builtin::StringMethod(op)) => builtins::inline_host_op(op, args),
        _ => None,
    };
    if let Some((flag, may_fail)) = inline_string {
        // A host op that may refuse its input (DESIGN.md §13) runs inside a
        // block: its result leaves the block unless it is the
        // `METHOD_RANGE_ERROR` sentinel, which falls through to the builtin
        // call below (the builtin throws).
        if may_fail {
            function.instruction(&Instruction::Block(BlockType::Result(ValType::I64)));
            enter_frame(helpers);
        }
        // A primitive keeps its kind: the early read decides.
        if callee.is_some() {
            found(function, STRING_SLOT);
        } else {
            string_guard(function);
        }
        let block = if may_fail {
            BlockType::Empty
        } else {
            BlockType::Result(ValType::I64)
        };
        function.instruction(&Instruction::If(block));
        enter_frame(helpers);
        function.instruction(&Instruction::LocalGet(local));
        function.instruction(&Instruction::I32Const(flag));
        for slot in 0..2 {
            match args.get(slot) {
                Some(arg) => emit_node(arg, function, scratch, strings, helpers),
                None => {
                    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
                }
            }
        }
        function.instruction(&Instruction::Call(helpers.method));
        if may_fail {
            let result = scratch + 1;
            function.instruction(&Instruction::LocalTee(result));
            function.instruction(&Instruction::LocalGet(result));
            builtins::emit_is_method_refusal(function);
            function.instruction(&Instruction::I32Eqz);
            function.instruction(&Instruction::BrIf(1));
            function.instruction(&Instruction::Drop);
            leave_frame(helpers);
            function.instruction(&Instruction::End);
        } else {
            function.instruction(&Instruction::Else);
        }
        opened += 1;
    }
    for (slot, (kind, path)) in fast.iter().enumerate() {
        // An object keeps its kind byte through the arguments, so the early
        // read decides; only a path reading own properties (an array's
        // elements, a promise's `constructor`) tests the receiver again.
        if callee.is_none() || props_sensitive(*kind) {
            emit_proto_kind_test(function, local, *kind, *sealed, scratch + 3, helpers);
            if callee.is_some() {
                found(function, slot);
                function.instruction(&Instruction::I32And);
            }
        } else {
            found(function, slot);
        }
        if let Some(bit) = species_bit(path) {
            function.instruction(&Instruction::GlobalGet(DIRTY_GLOBAL));
            function.instruction(&Instruction::I32Const(1 << bit));
            function.instruction(&Instruction::I32And);
            function.instruction(&Instruction::I32Eqz);
            function.instruction(&Instruction::I32And);
        }
        if matches!(path, IrNode::ArrayPush { .. }) {
            emit_push_room_test(function, scratch + 3);
        }
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        enter_frame(helpers);
        emit_node(path, function, scratch, strings, helpers);
        function.instruction(&Instruction::Else);
        opened += 1;
    }
    // With an early callee read, every other receiver calls it (a marker
    // that reaches here names its builtin).
    if let Some(callee) = callee {
        emit_call_args(args, function, scratch, strings, helpers);
        let callee_val = scratch + 1;
        emit_node(callee, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(callee_val));
        let mut markers: Vec<(usize, Builtin)> =
            string.map(|b| (STRING_SLOT, b)).into_iter().collect();
        for (slot, (kind, _)) in fast.iter().enumerate() {
            if props_sensitive(*kind)
                && let Some(builtin) = kind_method(*kind, key)
            {
                markers.push((slot, builtin));
            }
        }
        for (slot, builtin) in markers {
            function.instruction(&Instruction::LocalGet(callee_val));
            function.instruction(&Instruction::I64Const(early_marker(slot)));
            function.instruction(&Instruction::I64Eq);
            function.instruction(&Instruction::If(BlockType::Empty));
            function.instruction(&Instruction::I64Const(realm::method_bits(builtin, helpers)));
            function.instruction(&Instruction::LocalSet(callee_val));
            function.instruction(&Instruction::End);
        }
        emit_bound_call(
            (callee_val, local),
            args.len(),
            callee_error_text(not_callable),
            function,
            scratch,
            strings,
            helpers,
        );
        for _ in 0..opened {
            leave_frame(helpers);
            function.instruction(&Instruction::End);
        }
        return;
    }
    // ONE uniform-ABI call: the argument slots, `this = recv`, the packed
    // `env` and `argc`, into the generic call — or, for a `String.prototype`
    // builtin not run inline above, a `call_indirect` whose slot selects that
    // builtin for a string receiver.
    let generic = Builtin::MethodCall;
    let message = static_key(strings, &not_callable.message).cast_unsigned();
    let env = (i64::from(message) << 32) | i64::from(static_key(strings, key).cast_unsigned());
    emit_call_args(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
    function.instruction(&Instruction::I64Const(env));
    emit_argc(call_argc(args.len(), helpers), function);
    match string {
        Some(builtin) if inline_string.is_none() => {
            let slot = |b: Builtin| Instruction::I32Const(helpers.builtin_slot(b).cast_signed());
            function.instruction(&slot(builtin));
            function.instruction(&slot(generic));
            string_guard(function);
            function.instruction(&Instruction::Select);
            function.instruction(&Instruction::CallIndirect {
                type_index: helpers.fn_type,
                table_index: 0,
            });
        }
        _ => {
            function.instruction(&Instruction::Call(helpers.builtin(generic)));
        }
    }
    emit_propagate_if_pending(function, helpers);
    for _ in 0..opened {
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }
}

/// Push `1` when the jsval in `local` is a string while (unless `sealed`)
/// `%String.prototype%`'s dirty bit is clear: the heap tag in the top 16
/// bits, then the kind byte.
fn emit_string_receiver_test(function: &mut Function, local: u32, sealed: bool) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(48));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(HEAP_BITS >> 48));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    if !sealed && let Some(bit) = realm::prototype_of(BuiltinCtor::String).watch_bit() {
        function.instruction(&Instruction::GlobalGet(DIRTY_GLOBAL));
        function.instruction(&Instruction::I32Const(1 << bit));
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::I32And);
    }
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// The [`early_marker`] slot of a string receiver.
const STRING_SLOT: usize = 6;

/// The value an [`IrNode::ProtoMethod`] leaves for a receiver that passed the
/// test of fast path `slot` ([`STRING_SLOT`]: a string receiver): a heap tag
/// over an address no block has (blocks are 8-aligned), so no `[[Get]]`
/// result equals it. A slot past [`STRING_SLOT`] has no marker (the read
/// then always takes `[[Get]]`).
fn early_marker(slot: usize) -> i64 {
    HEAP_BITS | i64::try_from(slot + 1).unwrap_or(0)
}

/// Whether the fast paths of `kind` read the receiver's own properties, so
/// arguments that run after the early read can invalidate them.
/// The dirty bit of the constructor whose builtin `@@species` the fast path
/// `path` assumes (`map` / `filter` create a plain array, `then` / `catch` /
/// `finally` a `%Promise%`): the receiver test proves `constructor` is
/// `%Array%` / `%Promise%`, not that its `@@species` is untouched, and the
/// global constructor is reachable even in an intrinsics-sealed program, so
/// the bit is tested whatever `sealed` says.
pub(super) fn species_bit(path: &IrNode) -> Option<u8> {
    let ctor = match path {
        IrNode::ArrayHof {
            op: ArrayHofOp::Map | ArrayHofOp::Filter,
            ..
        } => BuiltinCtor::Array,
        IrNode::PromiseThen { .. } | IrNode::PromiseFinally { .. } => BuiltinCtor::Promise,
        _ => return None,
    };
    Intrinsic::Ctor(ctor).watch_bit()
}

fn props_sensitive(kind: ProtoKind) -> bool {
    matches!(kind, ProtoKind::Array | ProtoKind::Promise)
}

/// The builtin `key` of the prototype of the family `kind` (the first one of
/// [`ProtoKind::Collection`]).
fn kind_method(kind: ProtoKind, key: &str) -> Option<Builtin> {
    proto_kind_cases(kind)
        .first()
        .and_then(|&(_, prototype)| realm::method_of(prototype, key))
}

/// Emit an [`IrNode::ProtoMethod`], leaving the method on the stack: the
/// [`early_marker`] of a string receiver or of the first fast path whose test
/// the receiver passes (its builtin, with no lookup), else the generic
/// `[[Get]]` of `key`.
pub(super) fn emit_proto_method(
    node: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let IrNode::ProtoMethod {
        sealed,
        recv,
        key,
        kinds,
    } = node
    else {
        unreachable!("emit_proto_method is only called with a ProtoMethod node")
    };
    let local = if let IrNode::Local(local) = **recv {
        local
    } else {
        emit_node(recv, function, scratch, strings, helpers);
        function.instruction(&Instruction::LocalSet(scratch));
        scratch
    };
    let mut opened = 0;
    let mut open = |function: &mut Function, slot: usize| {
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        enter_frame(helpers);
        function.instruction(&Instruction::I64Const(early_marker(slot)));
        function.instruction(&Instruction::Else);
        opened += 1;
    };
    if realm::method_of(realm::prototype_of(BuiltinCtor::String), key).is_some() {
        emit_string_receiver_test(function, local, *sealed);
        open(function, STRING_SLOT);
    }
    for (slot, &kind) in kinds.iter().enumerate().take(STRING_SLOT) {
        emit_proto_kind_test(function, local, kind, *sealed, scratch + 3, helpers);
        open(function, slot);
    }
    emit_index_node(
        &IrNode::Local(local),
        &IrNode::Str(key.as_str().into()),
        false,
        function,
        scratch,
        strings,
        helpers,
    );
    for _ in 0..opened {
        leave_frame(helpers);
        function.instruction(&Instruction::End);
    }
}

/// AND the passed array kind test on the stack with `length != 2^32 - 1` of
/// the array base in i32 local `base`: an array that long takes the builtin
/// `push`, which sets the non-index key and throws the `RangeError` on
/// `length`.
fn emit_push_room_test(function: &mut Function, base: u32) {
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// The bits of an object's first eight bytes holding its kind byte and its
/// `props` word.
const KIND_AND_PROPS_MASK: i64 = 0xffff_ffff_0000_00ff_u64.cast_signed();

/// The bits of an object's first eight bytes holding its `props` word.
const PROPS_MASK: i64 = 0xffff_ffff_0000_0000_u64.cast_signed();

/// [`KIND_AND_PROPS_MASK`] plus an array's `DICTIONARY` flag bit (header byte
/// 2, bit `32`): a dictionary array's elements may carry any attributes, so it
/// takes the generic call.
const ARRAY_KIND_AND_PROPS_MASK: i64 = KIND_AND_PROPS_MASK | 0x0020_0000;
const _: () = assert!(abi::ARRAY_FLAG_DICTIONARY == 0x20);

/// Emit `new callee(args)` ([`IrNode::Construct`], DESIGN.md §7.6), leaving the
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
    strings: &StrOffsets,
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
    // `this`: proto = Get(new.target, "prototype") if an object, else
    // `%Object.prototype%`.
    emit_new_target(function);
    function.instruction(&Instruction::I32Const(static_key(strings, "prototype")));
    function.instruction(&Instruction::Call(helpers.get));
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
    function.instruction(&Instruction::LocalSet(this));
    emit_select_object(
        function,
        this,
        &Instruction::I64Const(intrinsic_bits(Intrinsic::ObjectPrototype, helpers)),
    );
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
        emit_publish_spread_args(base, function);
        emit_spread_construct_argc(len, function);
    } else {
        emit_construct_argc(call_argc(args.len(), helpers), function);
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
    strings: &StrOffsets,
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
/// kind but a string or a symbol), else the i64 pushed by `alt`.
fn emit_select_object(function: &mut Function, local: u32, alt: &Instruction<'_>) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(alt);
    emit_is_object(function, local);
    function.instruction(&Instruction::Select);
}

/// Push `1` if the jsval in `local` is an object (a heap value other than a
/// string or a symbol), else `0`.
pub(super) fn emit_is_object(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Ne);
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_SYMBOL));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push nonzero if the jsval in `local` is a constructor: a function value
/// whose header carries `FUNC_FLAG_CONSTRUCTOR`, else `0`.
pub(super) fn emit_is_constructor(function: &mut Function, local: u32) {
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

/// The `argc` a plain (non-spread) call with `count` source arguments passes:
/// clamped to the `K` slots it fills, so a count above `K` only ever comes
/// from a spread call, which publishes its overflow in `$__spread_args`.
pub(super) fn call_argc(count: usize, helpers: Helpers<'_>) -> usize {
    count.min(helpers.max_args as usize)
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
/// `%Function.prototype%`), the funcref-table index (at [`FUNC_INDEX_OFFSET`]), and
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
    strings: &StrOffsets,
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
    emit_object_prefix(
        function,
        dst,
        TYPE_FUNCTION | i32::from(flags) << 16,
        intrinsic_bits(Intrinsic::FunctionPrototype, helpers),
    );

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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
/// block base) read its u32 length (a string's at `base + STR_LEN_OFFSET`, an
/// array's at `base + ARRAY_LEN_OFFSET`) and rebox it as a number jsval; any other heap operand reads its `length` property
/// through `__get`; a primitive operand yields `undefined` without touching
/// memory. The operand is stashed in the general i64 scratch slot (`+0`).
pub(super) fn emit_str_len(
    operand: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(operand, function, scratch, strings, helpers);
    // A primitive operand (number/boolean) has no header to read: its `.length`
    // is `undefined`.
    function.instruction(&Instruction::LocalSet(scratch));
    emit_is_pointer(function, scratch);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // A string or array (kind byte <= TYPE_ARRAY) reads its length; any other
    // heap value (a function's `length`, an object's own) is a `__get`.
    let kind = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(kind));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32LeU);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // kind != TYPE_STRING (so TYPE_ARRAY) ? array length : string length.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(kind));
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    emit_load_str_len(function);
    function.instruction(&Instruction::End);
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
        // A getter may throw: unwind from two `if`s deep.
        enter_frame(helpers);
        enter_frame(helpers);
        emit_propagate_if_pending(function, helpers);
        leave_frame(helpers);
        leave_frame(helpers);
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let exec_base = scratch + 7;

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
pub(super) fn emit_settler_value(
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
        intrinsic_bits(Intrinsic::FunctionPrototype, helpers),
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
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let base = scratch + 7;
    // K padded argument slots (deepest on the stack).
    emit_call_args(args, function, scratch, strings, helpers);
    // Evaluate the callee once; stash its block base.
    emit_node(callee, function, scratch, strings, helpers);
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
    emit_argc(call_argc(args.len(), helpers), function);
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
/// callable guards and the builtins; the transfer leaves the stack
/// polymorphic, so any discarded operands need no cleanup.
pub(super) fn emit_throw_not_a_function(
    function: &mut Function,
    text: CalleeErrorText<'_>,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_throw_error(
        &callee_type_error(text),
        function,
        scratch,
        strings,
        helpers,
    );
}

/// Throw the error object `error` builds and unwind to the nearest handler
/// (the stack is polymorphic afterwards).
pub(super) fn emit_throw_error(
    error: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(error, function, scratch, strings, helpers);
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
    emit_throw_return(function, helpers);
}

/// The `ReferenceError` an unresolvable reference to `global` throws.
fn reference_error(global: &GlobalRef) -> IrNode {
    IrNode::error_object(
        ErrorKind::ReferenceError,
        Some(IrNode::Str(global.message.as_str().into())),
        IrNode::Str(global.stack.as_str().into()),
    )
}

/// Push `HasProperty(global object, key of global)` as an i32.
fn emit_global_has(
    function: &mut Function,
    global: &GlobalRef,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::I32Const(static_key(strings, &global.name)));
    function.instruction(&Instruction::I64Const(intrinsic_bits(
        Intrinsic::Global,
        helpers,
    )));
    function.instruction(&Instruction::Call(helpers.has));
}

/// Emit a [`IrNode::GlobalGet`]: `Get` of the global-object property when it
/// exists, else `undefined` (a `typeof` operand) or the `ReferenceError`.
fn emit_global_get(
    global: &GlobalRef,
    typeof_operand: bool,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_global_has(function, global, strings, helpers);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    enter_frame(helpers);
    function.instruction(&Instruction::I64Const(intrinsic_bits(
        Intrinsic::Global,
        helpers,
    )));
    function.instruction(&Instruction::I32Const(static_key(strings, &global.name)));
    function.instruction(&Instruction::Call(helpers.get));
    function.instruction(&Instruction::Else);
    if typeof_operand {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    } else {
        emit_throw_error(
            &reference_error(global),
            function,
            scratch,
            strings,
            helpers,
        );
    }
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    if helpers.has_accessors {
        emit_propagate_if_pending(function, helpers);
    }
}

/// Emit a [`IrNode::GlobalSet`]: evaluate the value (stashed in the first
/// scratch slot), throw the `ReferenceError` when the global-object property
/// does not exist, else `__set` it; yields the value.
fn emit_global_set(
    global: &GlobalRef,
    value: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(value, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));
    emit_global_has(function, global, strings, helpers);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    emit_throw_error(
        &reference_error(global),
        function,
        scratch,
        strings,
        helpers,
    );
    leave_frame(helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I64Const(intrinsic_bits(
        Intrinsic::Global,
        helpers,
    )));
    function.instruction(&Instruction::I32Const(static_key(strings, &global.name)));
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::Call(helpers.set));
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::LocalGet(scratch));
}

/// Emit a [`IrNode::GlobalDeclare`]: `__define` the global-object property
/// `name` as writable, enumerable and non-configurable — a function
/// declaration's unconditionally (a rejected redefinition propagates), a
/// `var`'s with `undefined` only when it is not already an own property;
/// yields `undefined`.
fn emit_global_declare(
    name: &str,
    value: Option<&IrNode>,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let global = intrinsic_bits(Intrinsic::Global, helpers);
    let key = static_key(strings, name);
    let attrs = (abi::ATTR_WRITABLE | abi::ATTR_ENUMERABLE).cast_signed();
    let define = |function: &mut Function| {
        function.instruction(&Instruction::I64Const(global));
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::I32Const(key));
    };
    if let Some(value) = value {
        define(function);
        emit_node(value, function, scratch, strings, helpers);
        function.instruction(&Instruction::I32Const(attrs));
        function.instruction(&Instruction::Call(helpers.define));
        emit_propagate_if_pending(function, helpers);
    } else {
        function.instruction(&Instruction::I32Const(key));
        function.instruction(&Instruction::I64Const(global));
        function.instruction(&Instruction::Call(helpers.has_own()));
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::If(BlockType::Empty));
        define(function);
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::I32Const(attrs));
        function.instruction(&Instruction::Call(helpers.define));
        function.instruction(&Instruction::End);
    }
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
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
    strings: &StrOffsets,
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
    strings: &StrOffsets,
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
            emit_publish_spread_args(base, function);
            emit_spread_argc(len, function);
            function.instruction(&Instruction::Call(*func + IMPORT_FUNC_COUNT));
            emit_propagate_if_pending(function, helpers);
        }
        SpreadCallee::Indirect(callee, _)
        | SpreadCallee::This { callee, .. }
        | SpreadCallee::Method { callee, .. } => {
            let cb = scratch + 7;
            let text = match target {
                SpreadCallee::Indirect(_, not_callable)
                | SpreadCallee::Method { not_callable, .. } => callee_error_text(not_callable),
                _ => EXPRESSION_NOT_A_FUNCTION,
            };
            // The callee is evaluated first (JS order) and kept on the operand
            // stack while the argument array is built (a call nested in the
            // arguments reuses the scratch locals), then stashed.
            emit_node(callee, function, scratch, strings, helpers);
            emit_spread_args_setup(args, base, len, function, scratch, strings, helpers);
            // IsCallable (DF4), checked after the arguments (spec order).
            if !is_static_function(callee) {
                function.instruction(&Instruction::LocalSet(scratch));
                emit_callable_guard(function, text, scratch, scratch, strings, helpers);
                function.instruction(&Instruction::LocalGet(scratch));
            }
            function.instruction(&Instruction::I32WrapI64);
            function.instruction(&Instruction::LocalSet(cb));
            emit_spread_arg_slots(base, len, function, helpers);
            // A plain indirect call binds `this = undefined`; `super(...)`
            // keeps the enclosing constructor's `this`; a method call binds the
            // receiver.
            if let SpreadCallee::This { this_local, .. } | SpreadCallee::Method { this_local, .. } =
                target
            {
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
            emit_publish_spread_args(base, function);
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
/// stash its handle base into `base` and its element count into `len`. The
/// array is fresh, private and dense (spreading reads holes as `undefined`).
fn emit_spread_args_setup(
    args: &IrNode,
    base: u32,
    len: u32,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalSet(len));
}

/// Push the uniform ABI's `K` positional slots from a spread call's argument
/// array (handle base in `base`): slot `i` is `array[i]` (the i64 at
/// `store + ELEMS_DATA_OFFSET + 8*i`, `store` the handle's element store) when
/// `i < len`, else `undefined`. Any element beyond `K` stays in the array,
/// which [`emit_publish_spread_args`] hands to the callee.
pub(super) fn emit_spread_arg_slots(
    base: u32,
    len: u32,
    function: &mut Function,
    helpers: Helpers<'_>,
) {
    for i in 0..helpers.max_args {
        // len > i ? array[i] : undefined.
        function.instruction(&Instruction::LocalGet(len));
        function.instruction(&Instruction::I32Const(i.cast_signed()));
        function.instruction(&Instruction::I32GtU);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
        function.instruction(&Instruction::LocalGet(base));
        emit_array_elems(function);
        function.instruction(&Instruction::I64Load(elem_slot(u64::from(i))));
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::End);
    }
}

/// Publish a spread call's argument array (handle base in `base`) in
/// `$__spread_args` right before the call, after every argument and the callee
/// were evaluated, so a callee gathering its arguments reads the ones beyond
/// `K` from it (only when its argument count exceeds `K`). Stack-neutral.
pub(super) fn emit_publish_spread_args(base: u32, function: &mut Function) {
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::GlobalSet(SPREAD_ARGS_GLOBAL));
}

/// Push a spread call's dynamic argument count `len` as a NUMBER jsval (its
/// `f64` bit pattern), so a callee's rest parameter collects exactly the
/// real spread arguments.
pub(super) fn emit_spread_argc(len: u32, function: &mut Function) {
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
    strings: &StrOffsets,
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

/// Emit a `console.log(...)` spread call: build the argument array, then print
/// each element (a hole as `undefined`) like [`emit_console_log`] prints its
/// arguments — a bare newline for none — and push `undefined`.
pub(super) fn emit_console_log_spread(
    args: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (base, len, idx, elem) = (scratch + 8, scratch + 9, scratch + 10, scratch + 13);
    emit_node(args, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(base));
    function.instruction(&Instruction::I32Load(array_len_slot()));
    function.instruction(&Instruction::LocalTee(len));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I32Const(PRINT_FLAG_NEWLINE));
    function.instruction(&Instruction::Call(helpers.print));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(idx));
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(base));
    emit_array_elems(function);
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot(0)));
    function.instruction(&Instruction::LocalSet(elem));
    emit_elem_read(function, elem);
    // The last element ends the line; every other one is followed by a space.
    function.instruction(&Instruction::I32Const(PRINT_FLAG_END));
    function.instruction(&Instruction::I32Const(PRINT_FLAG_SEP));
    function.instruction(&Instruction::LocalGet(idx));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalTee(idx));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::Call(helpers.print));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
}

/// Emit a `typeof operand` expression, leaving a real **heap-string** jsval (tag
/// `0b101`, `TYPE_STRING`) naming the operand's type on the stack. The operand is
/// evaluated, stashed, mapped to a `TYPEOF_STRINGS` index, and that index selects
/// the INTERNED data-segment offset of the matching word — so a `typeof` result is
/// an ordinary string with no distinct representation: `===`, `+`, `.length`, and
/// `console.log` all flow through the existing string machinery (no `typeof`
/// special-casing anywhere). The seven words are interned by `collect_node` whenever
/// a program uses `typeof`, so this allocates nothing per call.
///
/// The concrete heap kind is a TYPE BYTE, not the NaN-box tag, so
/// `typeof` splits on whether the value is a heap pointer (single tag `0b101`):
///
/// - **Heap pointer** — read the type byte and map it to the `TYPEOF_STRINGS`
///   index: `TYPE_STRING` -> 4 ("string"), `TYPE_FUNCTION` -> 5 ("function"),
///   `TYPE_SYMBOL` -> 6 ("symbol"), every other kind -> 3 ("object").
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
    strings: &StrOffsets,
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
    // override string -> 4, function -> 5 and symbol -> 6 with layered selects.
    function.instruction(&Instruction::I64Const(4)); // val1: "string"
    function.instruction(&Instruction::I64Const(6)); // val1: "symbol"
    function.instruction(&Instruction::I64Const(5)); // val1: "function"
    function.instruction(&Instruction::I64Const(3)); // val2: base "object"
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_FUNCTION));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select); // function ? 5 : 3
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_SYMBOL));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select); // symbol ? 6 : (function ? 5 : 3)
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Select); // string ? 4 : (symbol ? 6 : ...)
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
    // interns all seven words when the program uses `typeof`; the `HEAP_BASE` fallback
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

/// The tail of an [`IrNode::SetFunctionName`], with the function value on the
/// stack: stash it in `local`, redefine its own `name` (configurable only) as
/// `prefix` + the key in `key` (a string key itself; a symbol `"[desc]"`, or
/// `""` when its description is `undefined`), and push the function again.
fn emit_set_function_name(
    key: u32,
    prefix: &str,
    local: u32,
    function: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let description = MemArg {
        offset: u64::from(abi::SYMBOL_DESCRIPTION_OFFSET),
        align: 3,
        memory_index: 0,
    };
    let text = |function: &mut Function, text: &str| {
        function.instruction(&Instruction::I32Const(static_key(strings, text)));
    };
    function.instruction(&Instruction::LocalTee(local));
    function.instruction(&Instruction::I32WrapI64);
    text(function, "name");
    if !prefix.is_empty() {
        text(function, prefix);
    }
    emit_is_symbol(function, key);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(description));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    text(function, "");
    function.instruction(&Instruction::Else);
    text(function, "[");
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I64Load(description));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.concat));
    text(function, "]");
    function.instruction(&Instruction::Call(helpers.concat));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::End);
    if !prefix.is_empty() {
        function.instruction(&Instruction::Call(helpers.concat));
    }
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I32Const(abi::ATTR_CONFIGURABLE.cast_signed()));
    function.instruction(&Instruction::Call(helpers.define));
    function.instruction(&Instruction::LocalGet(local));
}
