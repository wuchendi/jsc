#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit the `main` entry function body from the program's top-level statements.
///
/// The result is the last expression statement's value, left on the stack;
/// every other expression statement's value is dropped so the stack stays
/// balanced. A program with no expression statement pushes the `undefined`
/// jsval (matching JS: the completion value of a declaration-only program).
pub(super) fn emit_main(
    program: &IrProgram,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    // One i64 local per top-level variable, plus three trailing i64 scratch
    // locals (indices `program.locals`/`+1`/`+2`) used to inspect operand bits:
    // `typeof` and truthiness use the first, strict `===`/`!==` uses both, and
    // `obj[i] = v` uses the third to stash the assigned value across the tag
    // dispatch. One further i32 scratch local (index `+3`) holds the fresh
    // allocation pointer while an array/object/function/cell/env block is written;
    // three more i64 scratch locals (`+4`/`+5`/`+6`) hold a short-circuit
    // operator's left operand, a `CellSet` value across its store, and a
    // closure environment pointer across a function-value build; and a
    // final i32 scratch local (`+7`) holds an indirect call's callee block base.
    // Five further i32 scratch locals (`+8`..`+12`) and one i64 (`+13`) drive an
    // array higher-order loop: receiver base, length, index, callback base,
    // result base, and the accumulator / truthiness temp.
    // Five more i64 scratch locals (`+14`..`+18`) are DEDICATED HIGH slots for the
    // member/index WRITE (`+14` receiver, `+15` key/index, `+16` value) and the
    // computed-index READ (`+17` receiver, `+18` key/index) stash. They sit
    // ABOVE the `+0..+13` slots that child sub-expressions reuse, so a computed RHS
    // or index (e.g. `o.v = 3+4`, `a[i+1]`) can no longer clobber the stashed
    // receiver/key/value before the store/load reads them back. Get and set use
    // DISJOINT slots so a set whose value child is itself an index read (e.g.
    // `o.v = a[0]`) is safe too. LIMITATION: same-kind nesting (`a[b[i]]`, chained
    // `a[i] = (b[j] = k)`) still shares these fixed slots and is out of scope — a
    // full fix needs a depth-indexed scratch allocator.
    // Two more i64 slots (`+19` current prototype, `+20` target prototype) walk the
    // `@@prototype` chain for a multi-hop `instanceof` (`extends`).
    // One i32 (`+21`) holds a generator frame/env base and two i64 (`+22`/`+23`)
    // hold the generator handle and its state-machine result across a `gen.next()`
    // driver / `MakeGenerator` construction.
    // Six more i64 slots (`+24`..`+29`) are the `Date` field-method scratch: the
    // guest-side civil-from-days / time-of-day arithmetic runs entirely over these
    // (a `Date` method takes no arguments, so no child sub-expression is live).
    let mut function = Function::new(vec![
        (program.locals + 3, ValType::I64),
        (1, ValType::I32),
        (3, ValType::I64),
        (1, ValType::I32),
        (5, ValType::I32),
        (1, ValType::I64),
        (7, ValType::I64),
        (1, ValType::I32),
        (2, ValType::I64),
        (6, ValType::I64),
    ]);
    let scratch = program.locals;
    // Fresh unwinding state for this function body: no active `try`
    // handler, control-nesting depth 0.
    helpers.unwind.borrow_mut().reset();
    // Allocate every module cell holding the TDZ hole before any user code, so a
    // hoisted top-level function called before the binding's declaration runs
    // reaches a valid cell and its access guard throws.
    for slot in 0..program.module_cells {
        let global = MODULE_CELL_GLOBAL_BASE + slot;
        emit_cell_new(&IrNode::Undefined, &mut function, scratch, strings, helpers);
        function.instruction(&Instruction::GlobalSet(global));
        function.instruction(&Instruction::GlobalGet(global));
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::I64Const(TDZ_HOLE_BITS));
        function.instruction(&Instruction::I64Store(MemArg {
            offset: HEADER_SIZE as u64,
            align: 3,
            memory_index: 0,
        }));
    }

    let last_expr = program
        .stmts
        .iter()
        .rposition(|stmt| matches!(stmt, IrStmt::Expr(_)));
    for (index, stmt) in program.stmts.iter().enumerate() {
        match stmt {
            IrStmt::Expr(node) if Some(index) == last_expr => {
                emit_node(node, &mut function, scratch, strings, helpers);
            }
            _ => emit_stmt(stmt, &mut function, scratch, strings, helpers, None, None),
        }
    }
    if last_expr.is_none() {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    function.instruction(&Instruction::End);
    function
}

/// Emit a user-defined function body.
///
/// Parameters occupy locals `0..params`; only the remaining `params..locals`
/// are declared here (wasm parameters are not re-declared as locals). Every
/// statement runs for its side effects — the function's value flows out through
/// `return`. A trailing `undefined` jsval is pushed as the fall-through result
/// for a function that reaches the end without returning; when the body always
/// returns, this constant is unreachable (and thus valid).
pub(super) fn emit_function(
    func: &IrFunction,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let declared = func.locals - func.params;
    // Body-declared locals plus the same scratch-local layout as [`emit_main`]:
    // three i64 (`func.locals`/`+1`/`+2`) to inspect operand bits and stash the
    // value of `obj[i] = v`, one i32 (`+3`) for block allocation, three i64
    // (`+4`/`+5`/`+6`) for a short-circuit operand / `CellSet` value / closure
    // environment pointer, one i32 (`+7`) for an indirect call's callee base, the
    // five i32 (`+8`..`+12`) plus one i64 (`+13`) array higher-order loop slots,
    // five i64 (`+14`..`+18`) dedicated high slots for the member/index
    // WRITE (`+14`/`+15`/`+16`) and computed-index READ (`+17`/`+18`) stash,
    // which sit above the child-reused `+0..+13` slots so a computed RHS/index
    // cannot clobber the stashed receiver/key/value, and two i64 (`+19`/`+20`) for
    // the multi-hop `instanceof` prototype-chain walk, one i32
    // (`+21`) plus two i64 (`+22`/`+23`) for the generator frame base / handle /
    // state-machine result, and six i64 (`+24`..`+29`) for the `Date`
    // field-method arithmetic. See [`emit_main`] for the full rationale
    // and the same-kind-nesting limitation.
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
        (6, ValType::I64),
    ]);
    let scratch = func.locals;
    // Fresh unwinding state for this function body.
    helpers.unwind.borrow_mut().reset();
    emit_stmts(
        &func.body,
        &mut function,
        scratch,
        strings,
        helpers,
        None,
        None,
    );
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function
}

/// Emit the unwinding transfer for an in-flight exception.
///
/// This is the single primitive both `throw` and every call-site propagation
/// check funnel through, so the unwinding policy lives in one place.
///
/// # Handler-label stack
///
/// The emit context threads a stack of active handler landing-pad labels (the
/// `Unwind` handler entries). When that stack is non-empty this
/// helper `br`s to the top handler (the nearest enclosing `try`) so control lands
/// in its catch-dispatch block. With an empty stack the exception unwinds to the
/// enclosing function boundary: it emits the function's dummy result (every jsval
/// function returns `i64`, so the `undefined` bit-pattern) and a `return`,
/// popping the current frame. A callee thus returns to its caller, whose own
/// post-call check (see [`emit_propagate_if_pending`]) re-fires and unwinds one
/// frame further, until `main` returns and the run stage surfaces the exception.
/// No call site needs to know which unwinding target is active.
pub(super) fn emit_throw_return(function: &mut Function, helpers: Helpers<'_>) {
    // The nearest enclosing `try`'s catch-dispatch block, as a `br`
    // immediate relative to the current control-nesting depth, or `None` when no
    // `try` is active in this function.
    let target = {
        let unwind = helpers.unwind.borrow();
        unwind.handlers.last().map(|&entry| unwind.depth - entry)
    };
    if let Some(rel) = target {
        // A handler is active: branch into its catch-dispatch block. The block's
        // catch arm then observes `$exc_pending` set and runs the `catch` body.
        function.instruction(&Instruction::Br(rel));
    } else {
        // No handler: unwind to the function boundary — emit the dummy
        // `i64` result and `return`, popping this frame so the caller's own
        // post-call check re-fires one frame further out.
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::Return);
    }
}

/// Enter a wasm control frame (`block`/`loop`/`if`) that may wrap a throw or a
/// throwing-call site: bump the tracked nesting depth so a `throw`
/// reached inside computes the correct handler-relative `br`. Every
/// [`enter_frame`] must be balanced by a [`leave_frame`].
pub(super) fn enter_frame(helpers: Helpers<'_>) {
    helpers.unwind.borrow_mut().depth += 1;
}

/// Leave a frame entered with [`enter_frame`], restoring the nesting depth.
pub(super) fn leave_frame(helpers: Helpers<'_>) {
    helpers.unwind.borrow_mut().depth -= 1;
}

/// Emit a post-call check that unwinds if a callee left an exception in flight.
/// Inserted after every user-function / callback invocation (but not
/// after pure host builtins, which cannot throw): the callee's `i64` result is
/// already on top of the stack, so this reads `$exc_pending` and, when set,
/// runs [`emit_throw_return`] to unwind one more frame. On the no-exception path
/// the `if` falls through leaving the callee's result untouched, so the check is
/// transparent to the surrounding expression.
pub(super) fn emit_propagate_if_pending(function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    // The `if` opens a control frame around the throw transfer; count it so a
    // `br` to an enclosing `try` handler uses the right relative depth.
    enter_frame(helpers);
    emit_throw_return(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// The nearest enclosing `try` FINALLY landing pad as a relative `br`
/// immediate at the current control-nesting depth, or `None` when no `finally` is
/// active in this function. A `return`/`break`/`continue` that must run a finally
/// `br`s here (mirrors [`emit_throw_return`]'s handler lookup, but over the
/// finalizer-only stack).
fn nearest_finalizer_rel(helpers: Helpers<'_>) -> Option<u32> {
    let unwind = helpers.unwind.borrow();
    unwind.finalizers.last().map(|&entry| unwind.depth - entry)
}

/// Emit a `return` transfer that runs every enclosing `finally` first.
///
/// The return value is already on top of the stack. When a `finally` is active
/// the value is stashed in `$cf_value`, the completion kind recorded in
/// `$cf_kind`, and control `br`s to the nearest finalizer (whose post-body
/// dispatch later resumes the return); with no active `finally` the value is
/// returned directly. Either path first clears the OTHER in-flight completion
/// channel (`$exc_pending`) so at most one abrupt completion stays pending — this
/// is how a `return` inside a `finally` overrides a still-pending throw.
pub(super) fn emit_return_transfer(function: &mut Function, helpers: Helpers<'_>) {
    if let Some(rel) = nearest_finalizer_rel(helpers) {
        function.instruction(&Instruction::GlobalSet(CF_VALUE_GLOBAL));
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
        function.instruction(&Instruction::I32Const(CF_RETURN));
        function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
        function.instruction(&Instruction::Br(rel));
    } else {
        // Direct return: clear any pending completion this return overrides (a
        // `finally`'s own return supersedes the pending throw/return that
        // triggered the finally), then return the value left on the stack.
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
        function.instruction(&Instruction::Return);
    }
}

/// Emit a `break`/`continue` transfer that runs any enclosing `finally` between
/// here and the loop target first. `label` is the relative `br` immediate
/// of the ultimate target (the loop's break or continue block); `kind` is
/// [`CF_BREAK`] or [`CF_CONTINUE`]. `target` is the labeled-loop id when this is a
/// labeled `break`/`continue` (`Some(id)`) or `None` for the nearest loop; it is
/// stashed in [`CF_VALUE_GLOBAL`] so a resume after an intervening `finally`
/// re-issues toward the SAME loop. A finalizer strictly INSIDE the loop
/// (relative depth below `label`, so encountered before the target when
/// unwinding) is run first via the pending-completion channel; a finalizer
/// outside the loop — or none — means the target is branched to directly. The
/// direct path clears the pending channels so a `break`/`continue` inside a
/// `finally` overrides whatever completion triggered it.
pub(super) fn emit_loop_transfer(
    function: &mut Function,
    helpers: Helpers<'_>,
    kind: i32,
    label: u32,
    target: Option<u32>,
) {
    match nearest_finalizer_rel(helpers) {
        Some(rel) if rel < label => {
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
            function.instruction(&Instruction::I32Const(kind));
            function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
            // Stash the target loop (0 = nearest, id + 1 = labeled) so the resume
            // past the finally aims at the same loop.
            function.instruction(&Instruction::I64Const(encode_loop_target(target)));
            function.instruction(&Instruction::GlobalSet(CF_VALUE_GLOBAL));
            function.instruction(&Instruction::Br(rel));
        }
        _ => {
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
            function.instruction(&Instruction::I32Const(0));
            function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
            function.instruction(&Instruction::Br(label));
        }
    }
}

/// Encode a labeled-`break`/`continue` target for [`CF_VALUE_GLOBAL`]: `0` for the
/// nearest loop (an unlabeled `break`/`continue`), or the loop id `+ 1` for a
/// labeled one, so `0` stays reserved for "nearest".
fn encode_loop_target(target: Option<u32>) -> i64 {
    match target {
        Some(id) => i64::from(id) + 1,
        None => 0,
    }
}

/// The relative `br` immediate from the current depth to the labeled loop `id`'s
/// `break` (`want_continue == false`) or `continue` (`true`) landing block, using
/// the absolute-depth entries recorded when its `while` was emitted. The target is
/// always present — a labeled `break`/`continue` is only lowered inside its loop.
pub(super) fn labeled_target_rel(helpers: Helpers<'_>, id: u32, want_continue: bool) -> u32 {
    let unwind = helpers.unwind.borrow();
    let Some(target) = unwind.labels.iter().rev().find(|t| t.id == id) else {
        // A labeled break/continue is only ever lowered inside its target loop, so
        // the entry is always on the stack here.
        unreachable!("labeled break/continue target loop must be active");
    };
    let entry = if want_continue {
        target.continue_entry
    } else {
        target.break_entry
    };
    unwind.depth - entry
}

/// Resume a `return`/`break`/`continue` that was routed through the `finally`
/// just emitted. Placed right after [`emit_propagate_if_pending`] (which
/// handles a pending throw): reads `$cf_kind` and, for the pending kind, re-issues
/// the transfer FROM THIS lexical point — routing to the next outer finalizer
/// when one still intervenes, or reaching the ultimate target (function boundary
/// / loop label) otherwise. The three checks are independent `if`s; each transfer
/// diverges, so at most one runs. If the `finally` itself completed abruptly its
/// own transfer already overwrote `$cf_kind`, so a superseded completion never
/// fires here. `break_label`/`continue_label` are the loop targets in scope at
/// the enclosing `try`'s own depth (the same values threaded to the finally body).
pub(super) fn emit_resume_pending_completion(
    function: &mut Function,
    helpers: Helpers<'_>,
    break_label: Option<u32>,
    continue_label: Option<u32>,
) {
    // Pending return: the routed value is already in `$cf_value`.
    function.instruction(&Instruction::GlobalGet(CF_KIND_GLOBAL));
    function.instruction(&Instruction::I32Const(CF_RETURN));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    function.instruction(&Instruction::GlobalGet(CF_VALUE_GLOBAL));
    emit_return_transfer(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);

    // Pending break/continue: re-issue toward the loop target in scope here — the
    // nearest loop (unlabeled) or, disambiguated by the stashed `$cf_value`, an
    // enclosing labeled loop.
    emit_loop_resume(function, helpers, CF_BREAK, break_label, false);
    emit_loop_resume(function, helpers, CF_CONTINUE, continue_label, true);
}

/// Emit the resume dispatch for one abrupt-loop kind (`break` or `continue`) after
/// a `finally`. When no labeled loop is active this is the original
/// single re-issue toward `nearest_label` (the nearest loop's target in scope
/// here). Inside a labeled loop it becomes a guarded dispatch: one `if` re-issuing
/// toward the nearest loop (`$cf_value == 0`), plus one per active labeled loop
/// (`$cf_value == id + 1`), so a routed labeled `break`/`continue` reaches the
/// SAME loop it originally targeted. `want_continue` picks the loop's continue vs
/// break landing block for the labeled targets.
fn emit_loop_resume(
    function: &mut Function,
    helpers: Helpers<'_>,
    kind: i32,
    nearest_label: Option<u32>,
    want_continue: bool,
) {
    // Snapshot the active labeled loop ids before emitting (each guard opens a
    // frame that shifts the depth `labeled_target_rel` reads).
    let label_ids: Vec<u32> = helpers
        .unwind
        .borrow()
        .labels
        .iter()
        .map(|t| t.id)
        .collect();

    if label_ids.is_empty() {
        // No labeled loop can be pending: keep the plain nearest-target re-issue.
        if let Some(label) = nearest_label {
            emit_kind_guard(function, helpers, kind, None, |f, h| {
                emit_loop_transfer(f, h, kind, label + 1, None);
            });
        }
        return;
    }

    // Nearest loop (unlabeled `break`/`continue`, `$cf_value == 0`).
    if let Some(label) = nearest_label {
        emit_kind_guard(
            function,
            helpers,
            kind,
            Some(encode_loop_target(None)),
            |f, h| {
                emit_loop_transfer(f, h, kind, label + 1, None);
            },
        );
    }
    // One guarded re-issue per active labeled loop (`$cf_value == id + 1`).
    for id in label_ids {
        emit_kind_guard(
            function,
            helpers,
            kind,
            Some(encode_loop_target(Some(id))),
            |f, h| {
                let rel = labeled_target_rel(h, id, want_continue);
                emit_loop_transfer(f, h, kind, rel, Some(id));
            },
        );
    }
}

/// Emit `if ($cf_kind == kind [&& $cf_value == v]) { body }`, opening one control
/// frame around `body` (which is expected to diverge via a loop transfer). The
/// optional `$cf_value` guard `v` disambiguates the labeled-loop target;
/// omitting it reproduces the original unguarded kind check.
fn emit_kind_guard(
    function: &mut Function,
    helpers: Helpers<'_>,
    kind: i32,
    value_check: Option<i64>,
    body: impl FnOnce(&mut Function, Helpers<'_>),
) {
    function.instruction(&Instruction::GlobalGet(CF_KIND_GLOBAL));
    function.instruction(&Instruction::I32Const(kind));
    function.instruction(&Instruction::I32Eq);
    if let Some(v) = value_check {
        function.instruction(&Instruction::GlobalGet(CF_VALUE_GLOBAL));
        function.instruction(&Instruction::I64Const(v));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::I32And);
    }
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    body(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}
