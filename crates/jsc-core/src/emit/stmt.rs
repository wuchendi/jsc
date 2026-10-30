#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit a statement for its side effects, leaving nothing on the stack.
///
/// Expression statements have their value dropped (only the top-level result
/// expression, handled in [`emit`], keeps its value). Control-flow bodies are
/// emitted recursively via [`emit_stmts`].
#[allow(clippy::too_many_lines)]
pub(super) fn emit_stmt(
    stmt: &IrStmt,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    break_label: Option<u32>,
    continue_label: Option<u32>,
) {
    match stmt {
        IrStmt::Decl { local, init } => {
            emit_node(init, function, scratch, strings, helpers);
            function.instruction(&Instruction::LocalSet(*local));
        }
        IrStmt::Expr(node) => {
            emit_node(node, function, scratch, strings, helpers);
            function.instruction(&Instruction::Drop);
        }
        IrStmt::Return(value) => {
            if let Some(node) = value {
                emit_node(node, function, scratch, strings, helpers);
            } else {
                // Bare `return;` yields the `undefined` jsval so the `-> i64`
                // result stays satisfied.
                function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            }
            // Route through every enclosing `finally` before the actual return;
            // with none active this is a plain `return`.
            emit_return_transfer(function, helpers);
        }
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            emit_cond(cond, function, scratch, strings, helpers);
            function.instruction(&Instruction::If(BlockType::Empty));
            // The `if`/`else` frame sits one level inside `break_label` (and one
            // deeper for a throw's handler-relative `br`).
            enter_frame(helpers);
            emit_stmts(
                then_body,
                function,
                scratch,
                strings,
                helpers,
                break_label.map(|l| l + 1),
                continue_label.map(|l| l + 1),
            );
            if !else_body.is_empty() {
                function.instruction(&Instruction::Else);
                emit_stmts(
                    else_body,
                    function,
                    scratch,
                    strings,
                    helpers,
                    break_label.map(|l| l + 1),
                    continue_label.map(|l| l + 1),
                );
            }
            leave_frame(helpers);
            function.instruction(&Instruction::End);
        }
        IrStmt::While {
            cond,
            body,
            update,
            label,
        } => {
            emit_while(
                cond, body, update, *label, function, scratch, strings, helpers,
            );
        }
        IrStmt::Switch {
            disc_local,
            disc,
            cases,
        } => {
            emit_switch(
                *disc_local,
                disc,
                cases,
                function,
                scratch,
                strings,
                helpers,
                continue_label,
            );
        }
        IrStmt::Break(target) => {
            emit_abrupt_loop(function, helpers, CF_BREAK, *target, break_label, false);
        }
        IrStmt::Continue(target) => {
            emit_abrupt_loop(
                function,
                helpers,
                CF_CONTINUE,
                *target,
                continue_label,
                true,
            );
        }
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            emit_try(
                body,
                catch_body.as_deref(),
                finally_body.as_deref(),
                function,
                scratch,
                strings,
                helpers,
                break_label,
                continue_label,
            );
        }
    }
}

/// Emit a `break`/`continue` (`kind` = [`CF_BREAK`]/[`CF_CONTINUE`]), running any
/// intervening `finally` on the way to its target. `target` is the labeled
/// loop id for `break LABEL`/`continue LABEL` (`Some`) or `None` for the nearest
/// enclosing loop/switch; `nearest` is that nearest target's relative label,
/// threaded through the emit context (`None` for a labeled transfer, which derives
/// its relative depth from the recorded label entry instead).
/// `want_continue` selects the labeled loop's continue vs break landing block. An
/// out-of-context unlabeled `break`/`continue` is rejected during lowering.
fn emit_abrupt_loop(
    function: &mut Function,
    helpers: Helpers<'_>,
    kind: i32,
    target: Option<u32>,
    nearest: Option<u32>,
    want_continue: bool,
) {
    match target {
        None => {
            let Some(label) = nearest else {
                unreachable!("break/continue outside of an enclosing loop or switch");
            };
            emit_loop_transfer(function, helpers, kind, label, None);
        }
        Some(id) => {
            let rel = labeled_target_rel(helpers, id, want_continue);
            emit_loop_transfer(function, helpers, kind, rel, Some(id));
        }
    }
}

/// Emit a `while`/desugared-`for` loop.
///
/// `block B { loop L { br_if(B) !cond; block C { body } update; br(L) } }`: the
/// outer block `B` is the break target, the inner `block C` the continue target.
/// A `continue` `br`s to the end of `C`, so `update` (the desugared `for` step)
/// still runs before the next iteration; a plain `while` has an empty `update`.
/// Relative depths from inside `C`: `C`=0 (continue), `L`=1, `B`=2 (break).
///
/// `label` is the loop id when this loop is the target of a labeled `break`/
/// `continue`: the absolute depths just inside `B` and `C` are registered
/// for the body so a labeled transfer anywhere within (even from a nested loop)
/// resolves to the right relative `br` immediate. It is popped after the body.
#[allow(clippy::too_many_arguments)]
fn emit_while(
    cond: &IrNode,
    body: &[IrStmt],
    update: &[IrStmt],
    label: Option<u32>,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    function.instruction(&Instruction::Block(BlockType::Empty));
    enter_frame(helpers); // B (break target)
    let break_entry = helpers.unwind.borrow().depth;
    function.instruction(&Instruction::Loop(BlockType::Empty));
    enter_frame(helpers); // L (loop)
    emit_cond(cond, function, scratch, strings, helpers);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::BrIf(1)); // exit loop: B is label 1 here
    function.instruction(&Instruction::Block(BlockType::Empty));
    enter_frame(helpers); // C (continue target)
    // Register the labeled-loop landing blocks (absolute depths) for the body so a
    // labeled `break`/`continue` targeting this loop resolves against them.
    if let Some(id) = label {
        let continue_entry = helpers.unwind.borrow().depth;
        helpers.unwind.borrow_mut().labels.push(LabelTarget {
            id,
            break_entry,
            continue_entry,
        });
    }
    // Inside the body, break reaches B (label 2) and continue reaches the end of C
    // (label 0), shadowing any enclosing switch/loop.
    emit_stmts(body, function, scratch, strings, helpers, Some(2), Some(0));
    if label.is_some() {
        helpers.unwind.borrow_mut().labels.pop();
    }
    leave_frame(helpers); // C
    function.instruction(&Instruction::End); // C
    // The update runs on both the fall-through and the continue edge; it sits
    // directly in the loop, so break is label 1 and it has no inner continue
    // target of its own.
    emit_stmts(
        update,
        function,
        scratch,
        strings,
        helpers,
        Some(1),
        Some(0),
    );
    function.instruction(&Instruction::Br(0)); // next iteration: re-test cond
    leave_frame(helpers); // L
    function.instruction(&Instruction::End); // L
    leave_frame(helpers); // B
    function.instruction(&Instruction::End); // B
}

/// Emit a `try`/`catch`/`finally` statement.
///
/// With no `finally` the structure is one `block $catch_dispatch` wrapping the
/// guarded `body`, immediately followed by an `if (global.get $exc_pending)`
/// catch-dispatch:
///
/// ```text
///   block $catch_dispatch          ; active handler landing pad
///     <body>                       ; a throw here `br`s to this block's end
///   end
///   if (global.get $exc_pending)   ; reached by fall-through OR the throw `br`
///     global.set $exc_pending 0    ;   clear the in-flight flag
///     <catch_body>                 ;   catch param binds $exc_value (first stmt)
///   end
/// ```
///
/// While `body` is emitted the catch-dispatch block is pushed onto the handler
/// stack at the current control-nesting depth, so [`emit_throw_return`] `br`s
/// here (relative `depth - entry`) instead of returning. On a normal,
/// non-throwing completion `$exc_pending` is `0`, so the dispatch `if` is
/// skipped; a throw sets it to `1` and `br`s to the block's end, landing in the
/// dispatch which runs the handler. The handler is popped before the dispatch so
/// a re-throw inside `catch_body` propagates to the NEXT outer handler.
///
/// # `finally`
///
/// When a `finally` is present the whole catch-dispatch unit is wrapped in an
/// outer `block $finally_target` so EVERY exit edge — normal fall-through and
/// every exceptional `br` — converges after it, where `finally_body` always
/// runs; a still-pending exception afterward re-propagates:
///
/// ```text
///   block $finally_target          ; pushed FIRST as the handler
///     block $catch_dispatch         ; (only when a `catch` is present)
///       <body>
///     end
///     if $exc_pending               ; (only when a `catch` is present)
///       global.set $exc_pending 0
///       <catch_body>                ;   a re-throw here `br`s to $finally_target
///     end
///   end                            ; pop $finally_target BEFORE the finally body
///   stash $exc_pending/$exc_value  ; on the operand stack; clear the flag
///   <finally_body>                 ; ALWAYS runs
///   restore the stash              ; unless the body threw its own
///   if $exc_pending                ; still pending after finally => re-propagate
///     <emit_throw_return>          ;   br to next outer handler / function return
///   end
/// ```
///
/// `$finally_target` is pushed first and `$catch_dispatch` on top of it, so
/// during `catch_body` (after `$catch_dispatch` is popped) the active handler is
/// `$finally_target` — a re-throw in `catch` still runs `finally`. With NO
/// `catch` a throw in `body` `br`s straight to `$finally_target`. It is popped
/// BEFORE `finally_body` is emitted, so a throw INSIDE `finally` propagates to
/// the next outer handler (replacing any in-flight exception, standard JS) rather
/// than back into itself.
///
/// # `return`/`break`/`continue` through `finally`
///
/// The SAME `$finally_target` block also catches a `return`/`break`/`continue`
/// that must run this `finally` first: it is pushed onto the finalizer stack (see
/// [`Unwind::finalizers`]) for the duration of `body`/`catch_body`, so those
/// abrupt completions `br` here via [`emit_loop_transfer`]/[`emit_return_transfer`]
/// (recording the pending completion in `$cf_kind`/`$cf_value`). After the finally
/// body runs, [`emit_resume_pending_completion`] re-issues the pending completion
/// from the `try`'s own depth — routing to the next outer finalizer or reaching
/// the ultimate target — unless the finally itself completed abruptly and already
/// replaced it.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_try(
    body: &[IrStmt],
    catch_body: Option<&[IrStmt]>,
    finally_body: Option<&[IrStmt]>,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    break_label: Option<u32>,
    continue_label: Option<u32>,
) {
    let Some(finally_body) = finally_body else {
        // No `finally`: the plain catch unit (a `catch` is always present
        // in this shape). `body` sits one block deeper than the `try` statement.
        let Some(catch_body) = catch_body else {
            unreachable!("try with neither catch nor finally");
        };
        emit_catch_unit(
            body,
            catch_body,
            function,
            scratch,
            strings,
            helpers,
            break_label,
            continue_label,
        );
        return;
    };

    // `finally` present: open the outer landing block and push it as the active
    // handler (throws) AND finalizer (return/break/continue) so every abrupt edge
    // converges here before the finally runs.
    function.instruction(&Instruction::Block(BlockType::Empty));
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.depth += 1;
        let depth = unwind.depth;
        unwind.handlers.push(depth);
        unwind.finalizers.push(depth);
    }
    // Inside the outer block, emit the catch unit (or the bare body when there is
    // no `catch`). Both sit one block deeper than the `try` statement.
    let inner_break = break_label.map(|l| l + 1);
    let inner_continue = continue_label.map(|l| l + 1);
    match catch_body {
        Some(catch_body) => emit_catch_unit(
            body,
            catch_body,
            function,
            scratch,
            strings,
            helpers,
            inner_break,
            inner_continue,
        ),
        None => {
            // No `catch`: a throw in `body` `br`s straight to $finally_target
            // (the only active handler). No catch-dispatch block or `if`.
            emit_stmts(
                body,
                function,
                scratch,
                strings,
                helpers,
                inner_break,
                inner_continue,
            );
        }
    }
    // Pop $finally_target (as both handler and finalizer) BEFORE the finally body
    // so an abrupt completion inside `finally` routes OUTWARD, not back into this
    // same block.
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.handlers.pop();
        unwind.finalizers.pop();
        unwind.depth -= 1;
    }
    function.instruction(&Instruction::End); // $finally_target

    // The finally body always runs — on normal fall-through and after every `br`
    // to $finally_target. It sits at the `try` statement's own control depth.
    // An exception in flight is set aside on the operand stack (the body is
    // stack-neutral; an abrupt exit from it discards the stash) so the body
    // runs with `$exc_pending` clear: every call-site check inside it sees
    // only a throw of its own.
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::GlobalGet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    emit_stmts(
        finally_body,
        function,
        scratch,
        strings,
        helpers,
        break_label,
        continue_label,
    );
    // Restore the stashed exception unless the body threw its own (which
    // supersedes it): value = pending ? current : stashed; pending |= stashed.
    function.instruction(&Instruction::GlobalGet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));

    // Re-propagate: if an exception is still pending after `finally`, unwind to
    // the next outer handler (or the function boundary).
    emit_propagate_if_pending(function, helpers);
    // Resume a `return`/`break`/`continue` routed through this finally, unless the
    // finally itself completed abruptly and superseded it.
    emit_resume_pending_completion(function, helpers, break_label, continue_label);
}

/// Emit the `block $catch_dispatch { body } if $exc_pending { clear; catch_body }`
/// unit shared by a plain `try`/`catch` and by a `try`/`catch`/`finally`
/// (nested inside the finally landing block). `break_label`/`continue_label`
/// are relative to the control depth just OUTSIDE this unit's catch-dispatch block.
/// Only the catch-dispatch (a throw target) is pushed on the handler stack — NOT
/// the finalizer stack — so a `return`/`break`/`continue` in `body` skips this
/// unit and routes to the enclosing `finally` (or its ultimate target) directly.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_catch_unit(
    body: &[IrStmt],
    catch_body: &[IrStmt],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    break_label: Option<u32>,
    continue_label: Option<u32>,
) {
    // Open the catch-dispatch block and register it as the active handler at the
    // depth just inside it, so a throw in `body` `br`s here.
    function.instruction(&Instruction::Block(BlockType::Empty));
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.depth += 1;
        let depth = unwind.depth;
        unwind.handlers.push(depth);
    }
    // `body` sits one block deeper than this unit, so an enclosing `break`/
    // `continue` target is one further out.
    emit_stmts(
        body,
        function,
        scratch,
        strings,
        helpers,
        break_label.map(|l| l + 1),
        continue_label.map(|l| l + 1),
    );
    {
        let mut unwind = helpers.unwind.borrow_mut();
        unwind.handlers.pop();
        unwind.depth -= 1;
    }
    function.instruction(&Instruction::End); // $catch_dispatch

    // Catch dispatch: on the throw path `$exc_pending` is set; clear it and run
    // the handler. On normal fall-through it is `0`, so the handler is skipped.
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    emit_stmts(
        catch_body,
        function,
        scratch,
        strings,
        helpers,
        break_label.map(|l| l + 1),
        continue_label.map(|l| l + 1),
    );
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Emit a `switch` statement: evaluate the discriminant once, dispatch on strict
/// equality (`===`), then run the selected clause with JS fall-through.
///
/// Layout, with `n = cases.len()`:
/// ```text
///   block B_break                 (relative label n+1 inside L_disp)
///     block L_{n-1} .. block L_0   (one per clause, outer..inner)
///       block L_disp
///         <dispatch: br_if L into the matching clause body>
///       end L_disp
///       <clause 0 body> end L_0
///     <clause 1 body> end L_1 .. <clause n-1 body> end L_{n-1}
///   end B_break
/// ```
/// A `br` to clause `i`'s body entry is relative label `i` from inside `L_disp`;
/// `break` targets `B_break` at relative label `n - i` from clause `i`'s body.
///
/// `continue_label` (the enclosing loop's continue target, as a `br` immediate at
/// the switch's own depth) is re-based for each clause body: reaching the switch
/// exterior from clause `i` takes `n - i` frames, so the continue target sits at
/// `continue_label + (n - i) + 1`. A bare `switch` (no enclosing loop) passes
/// `None`, and a `continue` there is rejected during lowering.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_switch(
    disc_local: u32,
    disc: &IrNode,
    cases: &[IrCase],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    continue_label: Option<u32>,
) {
    // Evaluate the discriminant once into its temp local.
    emit_node(disc, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(disc_local));

    let n = u32::try_from(cases.len()).unwrap_or(u32::MAX);
    function.instruction(&Instruction::Block(BlockType::Empty)); // B_break
    enter_frame(helpers); // B_break
    for _ in 0..n {
        function.instruction(&Instruction::Block(BlockType::Empty));
        enter_frame(helpers); // one per clause block
    }
    function.instruction(&Instruction::Block(BlockType::Empty)); // L_disp
    enter_frame(helpers); // L_disp

    let default_index = cases.iter().position(|case| case.test.is_none());
    for (i, case) in cases.iter().enumerate() {
        if let Some(test) = &case.test {
            let cmp = IrNode::Binary {
                op: BinOp::Eq,
                lhs: Box::new(IrNode::Local(disc_local)),
                rhs: Box::new(test.clone()),
            };
            emit_cond(&cmp, function, scratch, strings, helpers);
            function.instruction(&Instruction::BrIf(u32::try_from(i).unwrap_or(u32::MAX)));
        }
    }
    // No case matched: enter `default` at its source position, or exit.
    let fallback = default_index.map_or(n + 1, |d| u32::try_from(d).unwrap_or(u32::MAX));
    function.instruction(&Instruction::Br(fallback));
    leave_frame(helpers); // L_disp
    function.instruction(&Instruction::End); // L_disp

    for (i, case) in cases.iter().enumerate() {
        let to_exterior = n - u32::try_from(i).unwrap_or(u32::MAX);
        // `break` exits the switch (`B_break`); `continue` targets the enclosing
        // loop, one frame past the switch exterior.
        let case_continue = continue_label.map(|l| l + to_exterior + 1);
        emit_stmts(
            &case.body,
            function,
            scratch,
            strings,
            helpers,
            Some(to_exterior),
            case_continue,
        );
        leave_frame(helpers); // L_i
        function.instruction(&Instruction::End); // L_i
    }
    leave_frame(helpers); // B_break
    function.instruction(&Instruction::End); // B_break
}

/// Emit a sequence of statements for their side effects (nested body context).
pub(super) fn emit_stmts(
    stmts: &[IrStmt],
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    break_label: Option<u32>,
    continue_label: Option<u32>,
) {
    for stmt in stmts {
        emit_stmt(
            stmt,
            function,
            scratch,
            strings,
            helpers,
            break_label,
            continue_label,
        );
    }
}

/// Emit `cond` as a wasm `i32` truthiness value (`1` truthy, `0` falsy),
/// suitable as the operand of an `if`/`br_if`, applying JS truthiness to *any*
/// jsval. FALSY = `false`, `null`, `undefined`, number `0`/`-0`, number `NaN`,
/// empty string; everything else is TRUTHY (a `typeof` result is a non-empty heap
/// string, so it is truthy via the string path).
///
/// The evaluated condition is stashed in the `scratch` local so its bits can be
/// inspected twice: once as an `f64` (the number path) and once as a NaN-box tag
/// (the boxed path). The two candidate results are combined with `select` on
/// whether the value is a number, avoiding any control flow.
pub(super) fn emit_cond(
    cond: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    if let IrNode::IsConstructCall(argc) = cond {
        emit_is_construct_call(function, *argc);
        return;
    }
    if let IrNode::IteratorStep(rec) = cond {
        builtins::emit_iterator_step(rec, function, scratch, strings, helpers);
        return;
    }
    emit_node(cond, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(scratch));

    // number_truthy = (f != 0.0) & (f == f): nonzero and not NaN. `f != 0.0`
    // rejects 0/-0 but is `true` for NaN, so the `f == f` guard rejects NaN.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Ne);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::I32And);

    // boxed_truthy: a heap pointer (single tag 0b101) reads its TYPE BYTE
    //   is_string ? (string length != 0) : 1   (array/object/function are truthy)
    // otherwise a non-heap boxed value:
    //   is_boolean ? (bits & 1) : 0.
    // The type-byte and length reads are guarded by the is_heap `if`, so they never
    // run for a non-pointer whose wrapped bits would be a huge, invalid byte offset.
    // A heap string is truthy iff non-empty (`""` is falsy); an array/object/
    // function block is UNCONDITIONALLY truthy — even `[]` / `{}` — so its contents
    // are never read. Booleans are truthy only when the value bit is set;
    // null/undefined (and the canonical number NaN, tag 0b000) are falsy. A `typeof`
    // result is now an ordinary non-empty heap string, so it is truthy via the HEAP
    // string path — no non-heap `typeof` case remains.
    //   is_heap = is_boxed(bits) & (tag == 0b101)
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Eq);
    emit_tag_eq(function, scratch, 0b101);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    // HEAP: dispatch on the type byte — STRING reads its length, everything else
    // (array/object/function) is unconditionally truthy.
    emit_load_type_byte(function, scratch);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    // STRING: load the u32 length at base + STR_LEN_OFFSET; truthy iff nonzero.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    emit_load_str_len(function);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::Else);
    // ARRAY / OBJECT / FUNCTION: unconditionally truthy.
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    // NON-HEAP boxed: is_boolean ? (bits & 1) : 0.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::I32Const(0));
    emit_tag_eq(function, scratch, 0b001);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::End);

    // Pick the number path when the value is a number (QNAN mask not fully set),
    // otherwise the boxed path.
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::Select);
}

/// Emit a short-circuit logical operation `lhs op rhs` (`&&`/`||`/`??`), leaving
/// the selected **operand jsval** on the stack (JS value semantics — not a
/// coerced boolean).
///
/// The left operand is evaluated exactly once into the dedicated logical temp
/// local (index `scratch + 4`). A predicate on that temp — JS truthiness for
/// `&&`/`||`, a nullish check (`== null || == undefined`) for `??` — decides
/// whether to yield the right operand or the (stored) left. Because the right
/// operand sits in exactly one `if` arm, it is evaluated only on the
/// non-short-circuit path. Nested logicals may reuse the same temp: after the
/// predicate the temp is dead on whichever runtime path reads it, since the arm
/// that re-evaluates (and thus clobbers) the temp never reads it back.
pub(super) fn emit_logical(
    op: LogicalOp,
    lhs: &IrNode,
    rhs: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    // The i64 temp holding the left value across the branch; it sits just past
    // the i32 alloc scratch (see `emit_main`/`emit_function`).
    let temp = scratch + 4;

    // Evaluate the left operand once into `temp`.
    emit_node(lhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::LocalSet(temp));

    // Push the branch predicate: truthy(temp) for &&/||, nullish(temp) for ??.
    match op {
        LogicalOp::And | LogicalOp::Or => {
            emit_cond(&IrNode::Local(temp), function, scratch, strings, helpers);
        }
        LogicalOp::Nullish => {
            // nullish = (temp == null) | (temp == undefined). Uses exact bit
            // patterns, NOT truthiness (`0 ?? 5` must yield `0`).
            function.instruction(&Instruction::LocalGet(temp));
            function.instruction(&Instruction::I64Const(NULL_BITS));
            function.instruction(&Instruction::I64Eq);
            function.instruction(&Instruction::LocalGet(temp));
            function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
            function.instruction(&Instruction::I64Eq);
            function.instruction(&Instruction::I32Or);
        }
    }

    // The predicate is true when the operator should yield the RIGHT operand:
    // `&&` yields rhs when the left is truthy; `??` yields rhs when the left is
    // nullish; `||` yields rhs when the left is FALSY, so invert its truthiness.
    if let LogicalOp::Or = op {
        function.instruction(&Instruction::I32Eqz);
    }
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    // The right operand runs one frame deeper and may throw.
    enter_frame(helpers);
    emit_node(rhs, function, scratch, strings, helpers);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(temp));
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Push `1` if the jsval in `scratch` has NaN-box tag `tag` (its payload bits
/// `[50:48]`), else `0`. Used by [`emit_cond`] to discriminate boxed types.
pub(super) fn emit_tag_eq(function: &mut Function, scratch: u32, tag: i64) {
    function.instruction(&Instruction::LocalGet(scratch));
    function.instruction(&Instruction::I64Const(48));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I64Const(0b111));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(tag));
    function.instruction(&Instruction::I64Eq);
}
