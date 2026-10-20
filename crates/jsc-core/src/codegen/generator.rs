//! Lower a `function*` declaration into a generator constructor plus a state
//! machine.
//!
//! A `function*` becomes TWO ordinary uniform-ABI functions. The generator body
//! is lowered in an `env_backed` frame, so every local resolves to a boxed cell
//! reached through the frame environment (an `EnvLoad`) — the persistent frame
//! that lets a local survive across a `yield`. The lowered statement list is then
//! turned into a CONTROL-FLOW GRAPH of basic blocks ([`build_segments`]): a run of
//! straight-line statements is one block, and `if`/`while`/`for` crossing a
//! `yield` are flattened into blocks linked by `Jump`/`Branch`/`Yield`, so
//! control can suspend anywhere. The CONSTRUCTOR (an [`IrNode::MakeGenerator`])
//! builds the frame and handle without running the body; the STATE MACHINE
//! dispatches on `gen.state` over the blocks.
//!
//! `yield* inner` (delegation) desugars to a driving `while` loop over
//! `inner.next()` (see [`LowerCtx::lower_yield_delegate`]), reusing the CFG.
//!
//! A `yield` (or, in an `async function`, an `await`) nested anywhere inside an
//! expression is LINEARIZED by the CFG builder ([`CfgBuilder::linearize`]): the
//! expression's sub-expressions are evaluated left-to-right, every value computed
//! before a suspend and needed after it is spilled to a fresh frame cell (raw wasm
//! locals do not survive a suspend), and `?:`/`&&`/`||`/`??` with a `yield` in a
//! lazily evaluated operand become CFG branches into a result cell, so the operand
//! only suspends when it actually runs. This covers `yield` in `if`/loop
//! conditions and `yield yield x` too.
//!
//! Still rejected (deferred): a `yield` inside `try`/`catch`/`finally` (handler
//! state across a suspend) or inside a `switch`; a `yield` whose continuation would
//! read a compiler temporary set before the suspend; generator
//! expressions/arrows/methods; and non-simple parameters.
//!
//! A closure nested in the body captures the frame's env cells by reference
//! (see [`LowerCtx::capture_source`]), so it shares them with the body across
//! suspends and keeps them alive after the generator finishes.

#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::HashSet;

use swc_common::Spanned;

use crate::error::CompileError;
use crate::ir::{GenSegment, GenTerm};

use super::capture::binding_id;

impl LowerCtx<'_> {
    /// Lower a top-level `function*` declaration into its CONSTRUCTOR
    /// [`IrFunction`], registering the state-machine function in the arrow
    /// accumulator. Called from [`lower_function`] for a generator.
    ///
    /// Also lowers a top-level `async function`, keyed on
    /// `fn_decl.function.is_async`: the body is compiled to the SAME generator
    /// state machine (`await e` lowers to `yield e`; see
    /// [`LowerCtx::lower_generator_body`]), but the function the name binds to is a
    /// WRAPPER that builds the generator and spawns it as a promise
    /// ([`IrNode::AsyncSpawn`]) rather than returning the raw generator handle.
    pub(super) fn lower_generator(&mut self, fn_decl: &FnDecl) -> anyhow::Result<IrFunction> {
        let function = &fn_decl.function;
        let block = function
            .body
            .as_ref()
            .ok_or_else(|| anyhow!("generator declaration requires a body"))?;

        // The uniform ABI arity `K`: forced to >= 2 when generators exist (below),
        // so `(sent, gen)` fit the first two positional slots. `K + 2` = the full
        // uniform param count (`K` args + env + argc).
        let k = u32::try_from(self.shared.max_arity).unwrap_or(u32::MAX);
        let uniform_params = k + 2;

        // Reserve the state machine's funcref-table slot before lowering its body
        // (mirrors `lower_lifted_function`).
        let sm_pos = {
            let mut arrows = self.shared.arrows.borrow_mut();
            let pos = arrows.len();
            arrows.push(IrFunction {
                params: 0,
                locals: 0,
                body: Vec::new(),
                machine: None,
                info: FnInfo::default(),
            });
            pos
        };
        let sm_table_index = self.shared.arrow_base - 1 + u32::try_from(sm_pos).unwrap_or(u32::MAX);

        // Lower the body in a fresh, `env_backed` frame: every local becomes a
        // frame cell reached via `EnvLoad`. Body temporaries (from `fresh`) start
        // ABOVE the uniform ABI slots so they cannot collide with `sent`/`gen`/env.
        // The frame's env size (`total`) and local count are read before popping.
        self.push_frame();
        let lowered = self.lower_generator_body(function, block, uniform_params);
        let (body, nparams, mut total, sm_locals) = match lowered {
            Ok((body, nparams)) => {
                let total = u32::try_from(self.env_len()).unwrap_or(u32::MAX);
                let sm_locals = self.frame().next;
                (body, nparams, total, sm_locals)
            }
            Err(e) => {
                self.pop_frame();
                return Err(e);
            }
        };
        self.pop_frame();

        // Spill cells for values live across a nested suspend extend the frame.
        let segments = build_segments(&body, fn_decl, &mut total)?;
        self.shared.arrows.borrow_mut()[sm_pos] = IrFunction {
            params: uniform_params,
            locals: sm_locals,
            body: Vec::new(),
            machine: Some(segments),
            info: FnInfo::default(),
        };

        // The constructor: `return MakeGenerator(...)`, reading each parameter from
        // its uniform-ABI argument slot to seed the frame's leading cells. An
        // `async function` wraps the handle in `AsyncSpawn` so the wrapper returns
        // the spawned result promise instead of the raw generator.
        let slots: Vec<IrNode> = (0..nparams).map(IrNode::Local).collect();
        let make = IrNode::MakeGenerator {
            sm_table_index,
            slots,
            total,
        };
        let returned = if function.is_async {
            IrNode::AsyncSpawn(Box::new(make))
        } else {
            make
        };
        let ctor_body = vec![IrStmt::Return(Some(returned))];
        Ok(IrFunction {
            params: uniform_params,
            locals: uniform_params,
            body: ctor_body,
            machine: None,
            info: FnInfo::default(),
        })
    }

    /// Desugar a `yield* iterable;` statement over the same iterables `for-of`
    /// walks, dispatching on the operand's runtime kind (like
    /// [`Self::lower_for_of`]'s `IsGenerator` test):
    ///
    /// ```text
    /// let $it = iterable;
    /// if (IsGenerator($it)) {
    ///   let $r = $it.next();
    ///   while (!$r.done) { yield $r.value; $r = $it.next(); }
    /// } else if ($it instanceof Array || typeof $it === "string") {
    ///   let $i = 0;
    ///   while ($i < $it.length) { yield $it[$i]; $i = $i + 1; }
    /// } else {
    ///   throw new TypeError("yield* operand is not iterable");
    /// }
    /// ```
    ///
    /// which the CFG builder then flattens (the loops cross the `yield`). A string
    /// yields one character per element, as `for-of` does; the array walk re-reads
    /// the length each step, so elements pushed during delegation are delegated
    /// too. The temporaries are generator locals (env cells), so they survive the
    /// suspend. Only the delegate's per-value forwarding is modeled; threading the
    /// sent value / final value back through `yield*` is out of scope.
    pub(super) fn lower_yield_delegate(
        &mut self,
        yield_expr: &swc_ecma_ast::YieldExpr,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let Some(arg) = &yield_expr.arg else {
            return Err(
                CompileError::spanned(yield_expr.span, "`yield*` requires an operand").into(),
            );
        };
        let iter_val = self.lower_expr(arg)?;
        // Synthetic generator-local temporaries. Their ids are irrelevant to a
        // generator frame (no enclosing closure references them), so any distinct
        // values work; the `%`-prefixed names cannot collide with a user binding.
        let base = yield_expr.span.lo.0;
        let source = self.delegate_temp(&format!("%ys_it_{base}"), base, iter_val, out);
        let mut gen_body = Vec::new();
        let step = self.delegate_temp(
            &format!("%ys_r_{base}"),
            base.wrapping_add(1),
            IrNode::Undefined,
            &mut gen_body,
        );
        let mut index_body = Vec::new();
        let cursor = self.delegate_temp(
            &format!("%ys_i_{base}"),
            base.wrapping_add(2),
            IrNode::Num(0.0),
            &mut index_body,
        );
        let read = |kind: &AccessKind| Self::read_of(kind.clone());
        let next_of_source = || IrNode::GenResume {
            handle: Box::new(read(&source)),
            sent: Box::new(IrNode::Undefined),
        };
        let field = |kind: &AccessKind, key: &str| IrNode::Index {
            obj: Box::new(read(kind)),
            index: Box::new(IrNode::Str(key.to_owned())),
        };

        // Generator: $r = $it.next(); while (!$r.done) { yield $r.value; $r = $it.next(); }
        gen_body.push(IrStmt::Expr(Self::write_of(step.clone(), next_of_source())));
        gen_body.push(IrStmt::While {
            cond: IrNode::Not(Box::new(field(&step, "done"))),
            body: vec![
                IrStmt::Expr(IrNode::Yield(Box::new(field(&step, "value")))),
                IrStmt::Expr(Self::write_of(step.clone(), next_of_source())),
            ],
            update: Vec::new(),
            label: None,
        });

        // Array/string: while ($i < $it.length) { yield $it[$i]; $i = $i + 1; }
        index_body.push(IrStmt::While {
            cond: IrNode::Binary {
                op: BinOp::Lt,
                lhs: Box::new(read(&cursor)),
                rhs: Box::new(IrNode::StrLen(Box::new(read(&source)))),
            },
            body: vec![
                IrStmt::Expr(IrNode::Yield(Box::new(IrNode::Index {
                    obj: Box::new(read(&source)),
                    index: Box::new(read(&cursor)),
                }))),
                IrStmt::Expr(Self::write_of(
                    cursor.clone(),
                    IrNode::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(read(&cursor)),
                        rhs: Box::new(IrNode::Num(1.0)),
                    },
                )),
            ],
            update: Vec::new(),
            label: None,
        });

        let is_indexable = IrNode::Logical {
            op: LogicalOp::Or,
            lhs: Box::new(IrNode::InstanceOf {
                value: Box::new(read(&source)),
                kind: InstanceOfKind::Array,
            }),
            rhs: Box::new(IrNode::Binary {
                op: BinOp::Eq,
                lhs: Box::new(IrNode::TypeOf(Box::new(read(&source)))),
                rhs: Box::new(IrNode::Str("string".to_owned())),
            }),
        };
        out.push(IrStmt::If {
            cond: IrNode::IsGenerator(Box::new(read(&source))),
            then_body: gen_body,
            else_body: vec![IrStmt::If {
                cond: is_indexable,
                then_body: index_body,
                else_body: vec![IrStmt::Expr(IrNode::Throw(Box::new(not_iterable_error())))],
            }],
        });
        Ok(())
    }

    /// Declare a `yield*` generator-local temporary named `name`, bind it to
    /// `init` in `out`, and return its access kind.
    fn delegate_temp(
        &mut self,
        name: &str,
        id: u32,
        init: IrNode,
        out: &mut Vec<IrStmt>,
    ) -> AccessKind {
        let local = self.declare(name, true, id);
        self.push_binding(local, init, out);
        let Some(resolved) = self.resolve(name) else {
            unreachable!("a `yield*` temporary was just declared in this frame")
        };
        resolved.kind
    }

    /// Lower the parameters and body of a generator into the current (already
    /// pushed, `env_backed`) frame, returning the lowered statements and the
    /// parameter count. Rejects the unsupported forms with spans.
    fn lower_generator_body(
        &mut self,
        function: &swc_ecma_ast::Function,
        block: &swc_ecma_ast::FunctionBody,
        uniform_params: u32,
    ) -> anyhow::Result<(Vec<IrStmt>, u32)> {
        self.set_env_backed();
        // Body temporaries live above the uniform ABI slots (`sent`/`gen`/…/env/
        // argc), so a `fresh` temp never aliases `sent` (local 0) or the env slot.
        self.frame().next = uniform_params;

        // An `async function` body lowers `await e` to a generator `yield e`;
        // a plain `function*` keeps `await` a parse error, so the flag
        // stays off. Restored before returning on the success path (a compile
        // error aborts the whole function, so the leak on the error path is inert).
        let prev_async = self.async_body;
        self.async_body = function.is_async;

        // Only simple-identifier parameters are supported.
        let mut nparams = 0u32;
        for param in &function.params {
            let Pat::Ident(ident) = &param.pat else {
                return Err(CompileError::spanned(
                    param.pat.span(),
                    "generator parameters must be simple identifiers",
                )
                .into());
            };
            self.declare(ident.id.sym.as_ref(), true, binding_id(ident));
            nparams += 1;
        }

        let mut body = Vec::new();
        for stmt in &block.stmts {
            self.lower_stmt(stmt, &mut body)?;
        }

        self.async_body = prev_async;
        Ok((body, nparams))
    }
}

/// Build the generator's control-flow graph of basic blocks. A run of
/// yield-free, control-flow-free statements is one block; `if`/`while`/`for`
/// (which desugar to [`IrStmt::If`]/[`IrStmt::While`]) crossing a `yield` are
/// flattened into blocks linked by `Jump`/`Branch`/`Yield` terminators, so
/// control can suspend anywhere. `break`/`continue`/`return` become jumps to the
/// relevant block. A `yield` nested in an expression is linearized (see
/// [`CfgBuilder::linearize`]); each spill cell it needs is a new frame slot, so
/// `total` (the frame cell count) grows accordingly. Only `yield`/`try` crossing a
/// suspend and a few corner forms are still rejected (see [`CfgBuilder::lower_stmt`]).
fn build_segments(
    stmts: &[IrStmt],
    fn_decl: &FnDecl,
    total: &mut u32,
) -> anyhow::Result<Vec<GenSegment>> {
    let mut cfg = CfgBuilder {
        fn_decl,
        blocks: vec![PendingBlock::default()],
        cur: 0,
        loops: Vec::new(),
        next_slot: *total,
        suspended: false,
        live: HashSet::new(),
    };
    cfg.lower_stmts(stmts)?;
    *total = cfg.next_slot;
    // Fall off the end: `return undefined` (sets `done`).
    if cfg.blocks[cfg.cur].term.is_none() {
        cfg.blocks[cfg.cur].term = Some(GenTerm::Return(None));
    }
    // Finalize: an unreachable block with no terminator returns `undefined`.
    Ok(cfg
        .blocks
        .into_iter()
        .map(|b| GenSegment {
            stmts: b.stmts,
            term: b.term.unwrap_or(GenTerm::Return(None)),
        })
        .collect())
}

/// A basic block under construction: its statements and its terminator (`None`
/// until set — a still-open or an unreachable block).
#[derive(Default)]
struct PendingBlock {
    stmts: Vec<IrStmt>,
    term: Option<GenTerm>,
}

/// One enclosing loop's jump targets during CFG construction.
struct LoopCtx {
    /// Block a `continue` jumps to (the update block, or the head).
    continue_block: u32,
    /// Block a `break` jumps to (the loop exit).
    break_block: u32,
    /// The loop's label id, for a labeled `break`/`continue`.
    label: Option<u32>,
}

struct CfgBuilder<'a> {
    fn_decl: &'a FnDecl,
    blocks: Vec<PendingBlock>,
    /// The block currently being filled.
    cur: usize,
    loops: Vec<LoopCtx>,
    /// The next free frame cell slot, for spill cells.
    next_slot: u32,
    /// Whether the statement being linearized has already suspended.
    suspended: bool,
    /// Raw wasm locals written since the statement's last suspend — the only ones
    /// a continuation may still read (see [`CfgBuilder::track`]).
    live: HashSet<u32>,
}

impl CfgBuilder<'_> {
    fn new_block(&mut self) -> u32 {
        let id = u32::try_from(self.blocks.len()).unwrap_or(u32::MAX);
        self.blocks.push(PendingBlock::default());
        id
    }

    fn terminated(&self) -> bool {
        self.blocks[self.cur].term.is_some()
    }

    /// Terminate the current block with `term` (idempotent-safe: only the first
    /// terminator sticks, so a `break` before a fall-through wins).
    fn terminate(&mut self, term: GenTerm) {
        if self.blocks[self.cur].term.is_none() {
            self.blocks[self.cur].term = Some(term);
        }
    }

    fn unsupported(&self, msg: &str) -> anyhow::Error {
        CompileError::spanned(self.fn_decl.function.span(), msg.to_owned()).into()
    }

    fn lower_stmts(&mut self, stmts: &[IrStmt]) -> anyhow::Result<()> {
        for stmt in stmts {
            if self.terminated() {
                break; // dead code after a jump/return/yield-return
            }
            self.lower_stmt(stmt)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn lower_stmt(&mut self, stmt: &IrStmt) -> anyhow::Result<()> {
        // A statement with no suspend and no non-local exit is emitted verbatim
        // inside the current block (a self-contained inner loop keeps its own wasm
        // control flow).
        if is_inline(stmt) {
            self.blocks[self.cur].stmts.push(stmt.clone());
            return Ok(());
        }
        match stmt {
            // An expression statement / declaration / `return` with a suspend
            // somewhere inside: linearize it around each suspend point.
            IrStmt::Expr(node) => {
                self.begin_statement();
                let value = self.linearize(node.clone())?;
                if !matches!(value, IrNode::SentValue) {
                    self.push(IrStmt::Expr(value))?;
                }
            }
            IrStmt::Decl { local, init } => {
                self.begin_statement();
                let init = self.linearize(init.clone())?;
                self.push(IrStmt::Decl {
                    local: *local,
                    init,
                })?;
            }
            // `return e` / `return;` — end the generator (`Return` sets `done`).
            IrStmt::Return(value) => {
                self.begin_statement();
                let value = match value {
                    Some(node) => {
                        let node = self.linearize(node.clone())?;
                        self.track(&node)?;
                        Some(node)
                    }
                    None => None,
                };
                self.terminate(GenTerm::Return(value));
            }
            // `if (cond) then else` crossing a yield — flatten to a `Branch`.
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                self.begin_statement();
                let cond = self.linearize(cond.clone())?;
                self.track(&cond)?;
                let then_b = self.new_block();
                let else_b = self.new_block();
                let join = self.new_block();
                self.terminate(GenTerm::Branch {
                    cond,
                    then_block: then_b,
                    else_block: else_b,
                });
                self.cur = then_b as usize;
                self.lower_stmts(then_body)?;
                self.terminate(GenTerm::Jump(join));
                self.cur = else_b as usize;
                self.lower_stmts(else_body)?;
                self.terminate(GenTerm::Jump(join));
                self.cur = join as usize;
            }
            // `while (cond) body` (also a desugared `for`) crossing a yield.
            IrStmt::While {
                cond,
                body,
                update,
                label,
            } => {
                // In a generator every source variable is an env cell (never a raw
                // `Local`); a raw `Local` in a flattened loop's condition is a
                // NON-PERSISTING temporary — the tell-tale of a `for-of`/`for-in`
                // (its hidden index/length locals). Those do not survive a suspend,
                // so such a loop crossing a `yield` would silently mis-iterate.
                let crosses_yield = node_contains_yield(cond)
                    || body.iter().any(stmt_contains_yield)
                    || update.iter().any(stmt_contains_yield);
                if crosses_yield && node_reads_raw_local(cond) {
                    return Err(self.unsupported(
                        "a `for-of`/`for-in` loop with a `yield` in its body is not supported \
                         inside a generator (its loop temporaries do not persist across a \
                         suspend); use an indexed `for` loop over `.length` instead",
                    ));
                }
                let head = self.new_block();
                let body_b = self.new_block();
                let exit = self.new_block();
                let cont = if update.is_empty() {
                    head
                } else {
                    self.new_block()
                };
                self.terminate(GenTerm::Jump(head));
                self.cur = head as usize;
                self.begin_statement();
                let test = self.linearize(cond.clone())?;
                self.track(&test)?;
                self.terminate(GenTerm::Branch {
                    cond: test,
                    then_block: body_b,
                    else_block: exit,
                });
                self.loops.push(LoopCtx {
                    continue_block: cont,
                    break_block: exit,
                    label: *label,
                });
                self.cur = body_b as usize;
                self.lower_stmts(body)?;
                self.terminate(GenTerm::Jump(cont));
                if !update.is_empty() {
                    self.cur = cont as usize;
                    self.lower_stmts(update)?;
                    self.terminate(GenTerm::Jump(head));
                }
                self.loops.pop();
                self.cur = exit as usize;
            }
            IrStmt::Break(label) => {
                let target = self.loop_target(*label, true)?;
                self.terminate(GenTerm::Jump(target));
            }
            IrStmt::Continue(label) => {
                let target = self.loop_target(*label, false)?;
                self.terminate(GenTerm::Jump(target));
            }
            // A `yield` inside `try`/`catch`/`finally` — exception-handler state
            // across a suspend is out of scope (CUT A).
            IrStmt::Try { .. } => {
                return Err(self.unsupported("a `yield` inside try/catch/finally is not supported"));
            }
            // A `yield` inside a `switch` — deferred.
            IrStmt::Switch { .. } => {
                return Err(self.unsupported("a `yield` inside a `switch` is not supported"));
            }
        }
        Ok(())
    }

    /// Start linearizing a new statement: nothing has suspended yet, so every
    /// raw local is still readable.
    fn begin_statement(&mut self) {
        self.suspended = false;
        self.live.clear();
    }

    /// Append `stmt` to the current block (after [`Self::track`]ing its nodes).
    fn push(&mut self, stmt: IrStmt) -> anyhow::Result<()> {
        let mut result = Ok(());
        stmt.any_node(|node| {
            result = self.track(node);
            result.is_err()
        });
        result?;
        self.blocks[self.cur].stmts.push(stmt);
        Ok(())
    }

    /// Record the raw wasm locals `node` writes and reject a read of one that a
    /// suspend earlier in the statement has invalidated: raw locals do not
    /// survive a suspend, so a continuation may only read a local it set itself.
    fn track(&mut self, node: &IrNode) -> anyhow::Result<()> {
        if let IrNode::Assign { local, .. } = node {
            self.live.insert(*local);
        }
        let read = match node {
            IrNode::Local(local)
            | IrNode::IncDec { local, .. }
            | IrNode::ArrayPush { local, .. }
            | IrNode::ArrayPop { local, .. } => Some(*local),
            _ => None,
        };
        if let Some(local) = read
            && self.suspended
            && !self.live.contains(&local)
        {
            return Err(self.unsupported(
                "a `yield` here would lose a compiler temporary across the suspend; \
                 move the `yield` into its own statement",
            ));
        }
        let mut result = Ok(());
        node.any_child(|child| {
            result = self.track(child);
            result.is_err()
        });
        result
    }

    /// Suspend with `value`: end the current block with a `Yield` and continue in
    /// the resume block, where the sent value is [`IrNode::SentValue`].
    fn suspend(&mut self, value: IrNode) -> anyhow::Result<()> {
        self.track(&value)?;
        let resume = self.new_block();
        self.terminate(GenTerm::Yield { value, resume });
        self.cur = resume as usize;
        self.suspended = true;
        self.live.clear();
        Ok(())
    }

    /// A fresh frame cell slot (an env cell the constructor pre-creates), which —
    /// unlike a raw wasm local — survives a suspend.
    fn fresh_slot(&mut self) -> u32 {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }

    /// Evaluate `value` now into a fresh frame cell and return the read of that
    /// cell, so the value outlives a later suspend. A constant or a cell pointer
    /// is returned as is.
    fn spill(&mut self, value: IrNode) -> anyhow::Result<IrNode> {
        if matches!(
            value,
            IrNode::Num(_)
                | IrNode::Str(_)
                | IrNode::Bool(_)
                | IrNode::Null
                | IrNode::Undefined
                | IrNode::EnvLoad(_)
                | IrNode::ModuleCell(_)
        ) {
            return Ok(value);
        }
        let slot = self.fresh_slot();
        self.push(IrStmt::Expr(slot_set(slot, value)))?;
        Ok(slot_get(slot))
    }

    /// Linearize `node` into the current block: emit every suspend it contains
    /// (splitting blocks), and return the yield-free expression computing its
    /// value in the block where control ends up. Sub-expressions run
    /// left-to-right; each one evaluated before the LAST child containing a
    /// suspend is spilled to a frame cell. A lazily evaluated operand of
    /// `?:`/`&&`/`||`/`??` that contains a suspend becomes a CFG branch, so it
    /// only suspends when it runs.
    fn linearize(&mut self, node: IrNode) -> anyhow::Result<IrNode> {
        if !node_contains_yield(&node) {
            return Ok(node);
        }
        match node {
            IrNode::Yield(value) => {
                let value = self.linearize(*value)?;
                self.suspend(value)?;
                Ok(IrNode::SentValue)
            }
            IrNode::Ternary {
                cond,
                then_branch,
                else_branch,
            } if node_contains_yield(&then_branch) || node_contains_yield(&else_branch) => {
                self.linearize_ternary(*cond, *then_branch, *else_branch)
            }
            IrNode::Logical { op, lhs, rhs } if node_contains_yield(&rhs) => {
                self.linearize_logical(op, *lhs, *rhs)
            }
            // `obj[key] op= rhs` reads the current member value BEFORE evaluating
            // `rhs`, so with a suspend in `rhs` the read is spilled up front and the
            // update becomes a plain store.
            IrNode::IndexOpSet {
                obj,
                index,
                op,
                rhs,
                kind: OpSetKind::Compound,
                ..
            } if node_contains_yield(&rhs) => {
                let obj = self.linearize(*obj)?;
                let obj = self.spill(obj)?;
                let index = self.linearize(*index)?;
                let index = self.spill(index)?;
                let current = self.spill(IrNode::Index {
                    obj: Box::new(obj.clone()),
                    index: Box::new(index.clone()),
                })?;
                let rhs = self.linearize(*rhs)?;
                Ok(IrNode::IndexSet {
                    obj: Box::new(obj),
                    index: Box::new(index),
                    value: Box::new(IrNode::Binary {
                        op,
                        lhs: Box::new(current),
                        rhs: Box::new(rhs),
                    }),
                })
            }
            mut node => {
                let mut children = node.children_mut();
                let last = children
                    .iter()
                    .rposition(|child| node_contains_yield(child))
                    .unwrap_or(0);
                for (i, child) in children.iter_mut().enumerate().take(last + 1) {
                    let value = self.linearize(std::mem::replace(*child, IrNode::Undefined))?;
                    **child = if i < last { self.spill(value)? } else { value };
                }
                drop(children);
                Ok(node)
            }
        }
    }

    /// Linearize `cond ? then_branch : else_branch` with a suspend in a branch:
    /// branch on `cond` and evaluate only the taken branch into a result cell.
    fn linearize_ternary(
        &mut self,
        cond: IrNode,
        then_branch: IrNode,
        else_branch: IrNode,
    ) -> anyhow::Result<IrNode> {
        let cond = self.linearize(cond)?;
        self.track(&cond)?;
        let result = self.fresh_slot();
        let then_b = self.new_block();
        let else_b = self.new_block();
        let join = self.new_block();
        self.terminate(GenTerm::Branch {
            cond,
            then_block: then_b,
            else_block: else_b,
        });
        for (block, branch) in [(then_b, then_branch), (else_b, else_branch)] {
            self.cur = block as usize;
            let value = self.linearize(branch)?;
            self.push(IrStmt::Expr(slot_set(result, value)))?;
            self.terminate(GenTerm::Jump(join));
        }
        self.cur = join as usize;
        Ok(slot_get(result))
    }

    /// Linearize `lhs op rhs` with a suspend in `rhs`: store `lhs` into a result
    /// cell and branch to evaluate `rhs` (overwriting it) only when not
    /// short-circuited.
    fn linearize_logical(
        &mut self,
        op: LogicalOp,
        lhs: IrNode,
        rhs: IrNode,
    ) -> anyhow::Result<IrNode> {
        let lhs = self.linearize(lhs)?;
        let result = self.fresh_slot();
        self.push(IrStmt::Expr(slot_set(result, lhs)))?;
        let rhs_b = self.new_block();
        let join = self.new_block();
        // The rhs runs when `&&`'s lhs is truthy, `||`'s is falsy, or
        // `??`'s is nullish.
        let (cond, then_block, else_block) = match op {
            LogicalOp::And => (slot_get(result), rhs_b, join),
            LogicalOp::Or => (slot_get(result), join, rhs_b),
            LogicalOp::Nullish => {
                let is = |value| IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(slot_get(result)),
                    rhs: Box::new(value),
                };
                let nullish = IrNode::Logical {
                    op: LogicalOp::Or,
                    lhs: Box::new(is(IrNode::Null)),
                    rhs: Box::new(is(IrNode::Undefined)),
                };
                (nullish, rhs_b, join)
            }
        };
        self.terminate(GenTerm::Branch {
            cond,
            then_block,
            else_block,
        });
        self.cur = rhs_b as usize;
        let value = self.linearize(rhs)?;
        self.push(IrStmt::Expr(slot_set(result, value)))?;
        self.terminate(GenTerm::Jump(join));
        self.cur = join as usize;
        Ok(slot_get(result))
    }

    /// Resolve a `break`/`continue` target block: the innermost loop for an
    /// unlabeled statement, or the loop whose label matches.
    fn loop_target(&self, label: Option<u32>, is_break: bool) -> anyhow::Result<u32> {
        let ctx = match label {
            None => self.loops.last(),
            Some(id) => self.loops.iter().rev().find(|l| l.label == Some(id)),
        };
        let ctx = ctx.ok_or_else(|| {
            self.unsupported("a `break`/`continue` target is not an enclosing loop")
        })?;
        Ok(if is_break {
            ctx.break_block
        } else {
            ctx.continue_block
        })
    }
}

/// Whether a statement can be emitted verbatim inside a generator block — no
/// `yield`, no `return`, and no `break`/`continue` (which would need to become a
/// CFG jump). A self-contained inner loop with its own local control flow (no
/// escaping jump) qualifies and keeps its ordinary wasm lowering.
fn is_inline(stmt: &IrStmt) -> bool {
    !stmt_contains_yield(stmt)
        && !stmt_contains_return(stmt)
        && !stmt_contains_break_or_continue(stmt)
}

/// Whether an expression reads a raw wasm `Local` — which, in a generator body
/// (where every source variable is an env cell), means a compiler temporary. Used
/// to detect a `for-of`/`for-in` loop (whose hidden index/length live in raw
/// temps) crossing a `yield`. Per-node scratch temps (`obj_local`, ...) are
/// written and read within one evaluation, so they do not count.
fn node_reads_raw_local(node: &IrNode) -> bool {
    matches!(
        node,
        IrNode::Local(_)
            | IrNode::Assign { .. }
            | IrNode::IncDec { .. }
            | IrNode::ArrayPush { .. }
            | IrNode::ArrayPop { .. }
    ) || node.any_child(node_reads_raw_local)
}

/// Whether a statement contains a `break`/`continue` anywhere (recursively).
fn stmt_contains_break_or_continue(stmt: &IrStmt) -> bool {
    matches!(stmt, IrStmt::Break(_) | IrStmt::Continue(_))
        || stmt.any_stmt(stmt_contains_break_or_continue)
}

/// Write `value` into frame cell `slot` (a spill cell).
fn slot_set(slot: u32, value: IrNode) -> IrNode {
    IrNode::CellSet {
        cell: Box::new(IrNode::EnvLoad(slot)),
        value: Box::new(value),
    }
}

/// Read frame cell `slot` (a spill cell).
fn slot_get(slot: u32) -> IrNode {
    IrNode::CellGet(Box::new(IrNode::EnvLoad(slot)))
}

/// Whether a statement contains a `yield` anywhere (recursively).
fn stmt_contains_yield(stmt: &IrStmt) -> bool {
    stmt.any_node(node_contains_yield) || stmt.any_stmt(stmt_contains_yield)
}

/// Whether a statement contains a `return` anywhere (recursively). A `return`
/// nested inside inline control flow in a generator is an unsupported form
/// (its `done`/state bookkeeping needs the CFG lowering).
fn stmt_contains_return(stmt: &IrStmt) -> bool {
    matches!(stmt, IrStmt::Return(_)) || stmt.any_stmt(stmt_contains_return)
}

/// Whether an expression contains a `yield` anywhere (recursively), through every
/// child position (see [`IrNode::any_child`]).
fn node_contains_yield(node: &IrNode) -> bool {
    matches!(node, IrNode::Yield(_)) || node.any_child(node_contains_yield)
}

/// The `TypeError` a `yield*` over a non-iterable operand throws.
fn not_iterable_error() -> IrNode {
    let message = "yield* operand is not iterable";
    IrNode::error_object(
        ErrorKind::TypeError,
        None,
        Some(IrNode::Str(message.to_owned())),
        IrNode::Str(format!("TypeError: {message}")),
    )
}
