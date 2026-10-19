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
//! Still rejected (deferred): a `yield` inside `try`/`catch`/`finally` (handler
//! state across a suspend), inside a `switch`, in a loop/if CONDITION, or nested
//! in a larger expression; generator expressions/arrows/methods; and non-simple parameters.
//!
//! A closure nested in the body captures the frame's env cells by reference
//! (see [`LowerCtx::capture_source`]), so it shares them with the body across
//! suspends and keeps them alive after the generator finishes.

#[allow(clippy::wildcard_imports)]
use super::*;

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
        let (body, nparams, total, sm_locals) = match lowered {
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

        let segments = build_segments(&body, fn_decl)?;
        self.shared.arrows.borrow_mut()[sm_pos] = IrFunction {
            params: uniform_params,
            locals: sm_locals,
            body: Vec::new(),
            machine: Some(segments),
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
        // values work; the `@@`-prefixed names cannot collide with a user binding.
        let base = yield_expr.span.lo.0;
        let source = self.delegate_temp(&format!("@@ys_it_{base}"), base, iter_val, out);
        let mut gen_body = Vec::new();
        let step = self.delegate_temp(
            &format!("@@ys_r_{base}"),
            base.wrapping_add(1),
            IrNode::Undefined,
            &mut gen_body,
        );
        let mut index_body = Vec::new();
        let cursor = self.delegate_temp(
            &format!("@@ys_i_{base}"),
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
/// relevant block. Only `yield`/`try` crossing a suspend and a few corner forms
/// are still rejected (see [`CfgBuilder::lower_stmt`]).
fn build_segments(stmts: &[IrStmt], fn_decl: &FnDecl) -> anyhow::Result<Vec<GenSegment>> {
    let mut cfg = CfgBuilder {
        fn_decl,
        blocks: vec![PendingBlock::default()],
        cur: 0,
        loops: Vec::new(),
    };
    cfg.lower_stmts(stmts)?;
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
            // `yield e;` — suspend, discard the sent value.
            IrStmt::Expr(IrNode::Yield(value)) => {
                reject_nested_yield(value, self.fn_decl)?;
                let resume = self.new_block();
                self.terminate(GenTerm::Yield {
                    value: (**value).clone(),
                    resume,
                });
                self.cur = resume as usize;
            }
            // `x = yield e` — suspend, then bind the sent value into the cell.
            IrStmt::Expr(IrNode::CellSet { cell, value })
                if matches!(value.as_ref(), IrNode::Yield(_)) =>
            {
                let IrNode::Yield(inner) = value.as_ref() else {
                    unreachable!("guarded by the match arm")
                };
                reject_nested_yield(inner, self.fn_decl)?;
                let resume = self.new_block();
                self.terminate(GenTerm::Yield {
                    value: (**inner).clone(),
                    resume,
                });
                self.cur = resume as usize;
                self.blocks[self.cur]
                    .stmts
                    .push(IrStmt::Expr(IrNode::CellSet {
                        cell: cell.clone(),
                        value: Box::new(IrNode::SentValue),
                    }));
            }
            // `return yield e` — suspend, then the resume returns the sent value.
            IrStmt::Return(Some(IrNode::Yield(value))) => {
                reject_nested_yield(value, self.fn_decl)?;
                let resume = self.new_block();
                self.terminate(GenTerm::Yield {
                    value: (**value).clone(),
                    resume,
                });
                self.cur = resume as usize;
                self.terminate(GenTerm::Return(Some(IrNode::SentValue)));
            }
            // `return e` / `return;` — end the generator (`Return` sets `done`).
            IrStmt::Return(value) => {
                if let Some(node) = value {
                    reject_nested_yield(node, self.fn_decl)?;
                }
                self.terminate(GenTerm::Return(value.clone()));
            }
            // `if (cond) then else` crossing a yield — flatten to a `Branch`.
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                if node_contains_yield(cond) {
                    return Err(self.unsupported("a `yield` in an `if` condition is not supported"));
                }
                let then_b = self.new_block();
                let else_b = self.new_block();
                let join = self.new_block();
                self.terminate(GenTerm::Branch {
                    cond: cond.clone(),
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
                if node_contains_yield(cond) || update.iter().any(stmt_contains_yield) {
                    return Err(self
                        .unsupported("a `yield` in a loop condition or update is not supported"));
                }
                // In a generator every source variable is an env cell (never a raw
                // `Local`); a raw `Local` in a flattened loop's condition is a
                // NON-PERSISTING temporary — the tell-tale of a `for-of`/`for-in`
                // (its hidden index/length locals). Those do not survive a suspend,
                // so such a loop crossing a `yield` would silently mis-iterate.
                let crosses_yield =
                    body.iter().any(stmt_contains_yield) || update.iter().any(stmt_contains_yield);
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
                self.terminate(GenTerm::Branch {
                    cond: cond.clone(),
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
            // A `yield` nested inside a larger expression (a call argument, an
            // operand, …) — the segmentation only splits at statement granularity.
            _ => {
                return Err(self.unsupported(
                    "a `yield` in this position is not supported: use `yield e;`, \
                     `x = yield e;`, or `return yield e;`",
                ));
            }
        }
        Ok(())
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
/// temps) crossing a `yield`. Walks the same node shapes as [`node_contains_yield`].
fn node_reads_raw_local(node: &IrNode) -> bool {
    match node {
        IrNode::Local(_) | IrNode::Assign { .. } => true,
        IrNode::Binary { lhs, rhs, .. } | IrNode::Logical { lhs, rhs, .. } => {
            node_reads_raw_local(lhs) || node_reads_raw_local(rhs)
        }
        IrNode::Not(inner)
        | IrNode::CellNew(inner)
        | IrNode::CellGet(inner)
        | IrNode::StrLen(inner)
        | IrNode::NullishCheck { value: inner, .. }
        | IrNode::Throw(inner)
        | IrNode::Yield(inner)
        | IrNode::Await(inner)
        | IrNode::IsGenerator(inner) => node_reads_raw_local(inner),
        IrNode::CellSet { cell, value } => {
            node_reads_raw_local(cell) || node_reads_raw_local(value)
        }
        IrNode::Ternary {
            cond,
            then_branch,
            else_branch,
        } => {
            node_reads_raw_local(cond)
                || node_reads_raw_local(then_branch)
                || node_reads_raw_local(else_branch)
        }
        IrNode::Call { args, .. } | IrNode::ConsoleLog { args } => {
            args.iter().any(node_reads_raw_local)
        }
        IrNode::CallIndirect { callee, args } => {
            node_reads_raw_local(callee) || args.iter().any(node_reads_raw_local)
        }
        IrNode::Array(elems) => elems.iter().any(node_reads_raw_local),
        IrNode::Object(entries) => entries.iter().any(|(_, v)| node_reads_raw_local(v)),
        IrNode::Index { obj, index } => node_reads_raw_local(obj) || node_reads_raw_local(index),
        _ => false,
    }
}

/// Whether a statement contains a `break`/`continue` anywhere (recursively).
fn stmt_contains_break_or_continue(stmt: &IrStmt) -> bool {
    match stmt {
        IrStmt::Break(_) | IrStmt::Continue(_) => true,
        IrStmt::Expr(_) | IrStmt::Decl { .. } | IrStmt::Return(_) => false,
        IrStmt::If {
            then_body,
            else_body,
            ..
        } => {
            then_body.iter().any(stmt_contains_break_or_continue)
                || else_body.iter().any(stmt_contains_break_or_continue)
        }
        IrStmt::While { body, update, .. } => {
            body.iter().any(stmt_contains_break_or_continue)
                || update.iter().any(stmt_contains_break_or_continue)
        }
        IrStmt::Switch { cases, .. } => cases
            .iter()
            .any(|c| c.body.iter().any(stmt_contains_break_or_continue)),
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            body.iter().any(stmt_contains_break_or_continue)
                || catch_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_break_or_continue))
                || finally_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_break_or_continue))
        }
    }
}

/// Reject a `yield` whose operand itself contains a `yield` (`yield yield x`),
/// which the single-suspend segmentation cannot model.
fn reject_nested_yield(value: &IrNode, fn_decl: &FnDecl) -> anyhow::Result<()> {
    if node_contains_yield(value) {
        return Err(CompileError::spanned(
            fn_decl.function.span(),
            "a `yield` operand may not itself contain a `yield`",
        )
        .into());
    }
    Ok(())
}

/// Whether a statement contains a `yield` anywhere (recursively).
fn stmt_contains_yield(stmt: &IrStmt) -> bool {
    match stmt {
        IrStmt::Expr(node) => node_contains_yield(node),
        IrStmt::Decl { init, .. } => node_contains_yield(init),
        IrStmt::Return(value) => value.as_ref().is_some_and(node_contains_yield),
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            node_contains_yield(cond)
                || then_body.iter().any(stmt_contains_yield)
                || else_body.iter().any(stmt_contains_yield)
        }
        IrStmt::While {
            cond, body, update, ..
        } => {
            node_contains_yield(cond)
                || body.iter().any(stmt_contains_yield)
                || update.iter().any(stmt_contains_yield)
        }
        IrStmt::Switch { disc, cases, .. } => {
            node_contains_yield(disc)
                || cases.iter().any(|c| {
                    c.test.as_ref().is_some_and(node_contains_yield)
                        || c.body.iter().any(stmt_contains_yield)
                })
        }
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            body.iter().any(stmt_contains_yield)
                || catch_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_yield))
                || finally_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_yield))
        }
        IrStmt::Break(_) | IrStmt::Continue(_) => false,
    }
}

/// Whether a statement contains a `return` anywhere (recursively). A `return`
/// nested inside inline control flow in a generator is an unsupported form
/// (its `done`/state bookkeeping needs the CFG lowering).
fn stmt_contains_return(stmt: &IrStmt) -> bool {
    match stmt {
        IrStmt::Return(_) => true,
        IrStmt::Expr(_) | IrStmt::Decl { .. } | IrStmt::Break(_) | IrStmt::Continue(_) => false,
        IrStmt::If {
            then_body,
            else_body,
            ..
        } => {
            then_body.iter().any(stmt_contains_return) || else_body.iter().any(stmt_contains_return)
        }
        IrStmt::While { body, update, .. } => {
            body.iter().any(stmt_contains_return) || update.iter().any(stmt_contains_return)
        }
        IrStmt::Switch { cases, .. } => cases
            .iter()
            .any(|c| c.body.iter().any(stmt_contains_return)),
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            body.iter().any(stmt_contains_return)
                || catch_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_return))
                || finally_body
                    .as_ref()
                    .is_some_and(|b| b.iter().any(stmt_contains_return))
        }
    }
}

/// Whether an expression contains a `yield` anywhere (recursively). Only the node
/// shapes a generator body can produce need to be walked precisely; the rest fall
/// through the catch-all, which is sound because a `yield` in any un-walked
/// position is itself an unsupported form the segmentation rejects.
fn node_contains_yield(node: &IrNode) -> bool {
    match node {
        IrNode::Yield(_) => true,
        IrNode::Binary { lhs, rhs, .. } | IrNode::Logical { lhs, rhs, .. } => {
            node_contains_yield(lhs) || node_contains_yield(rhs)
        }
        IrNode::Not(inner)
        | IrNode::CellNew(inner)
        | IrNode::CellGet(inner)
        | IrNode::StrLen(inner)
        | IrNode::NullishCheck { value: inner, .. }
        | IrNode::Throw(inner)
        | IrNode::IsGenerator(inner)
        | IrNode::Await(inner) => node_contains_yield(inner),
        IrNode::CellSet { cell, value } => node_contains_yield(cell) || node_contains_yield(value),
        IrNode::Assign { value, .. } => node_contains_yield(value),
        IrNode::Ternary {
            cond,
            then_branch,
            else_branch,
        } => {
            node_contains_yield(cond)
                || node_contains_yield(then_branch)
                || node_contains_yield(else_branch)
        }
        IrNode::Call { args, .. } | IrNode::ConsoleLog { args } => {
            args.iter().any(node_contains_yield)
        }
        IrNode::CallIndirect { callee, args } => {
            node_contains_yield(callee) || args.iter().any(node_contains_yield)
        }
        IrNode::Array(elems) => elems.iter().any(node_contains_yield),
        IrNode::Object(entries) => entries.iter().any(|(_, v)| node_contains_yield(v)),
        IrNode::Index { obj, index } => node_contains_yield(obj) || node_contains_yield(index),
        IrNode::GenResume { handle, sent } => {
            node_contains_yield(handle) || node_contains_yield(sent)
        }
        _ => false,
    }
}

/// The `TypeError` a `yield*` over a non-iterable operand throws.
fn not_iterable_error() -> IrNode {
    let message = "yield* operand is not iterable";
    IrNode::Object(vec![
        ("name".to_owned(), IrNode::Str("TypeError".to_owned())),
        ("message".to_owned(), IrNode::Str(message.to_owned())),
        (
            "stack".to_owned(),
            IrNode::Str(format!("TypeError: {message}")),
        ),
        (
            crate::abi::ERROR_KIND_KEY.to_owned(),
            IrNode::Num(crate::abi::ERROR_KIND_TYPE),
        ),
    ])
}
