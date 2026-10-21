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
//! The hidden temporaries a `for-of`/`for-in` desugaring keeps in raw wasm locals
//! (its iterable, index, length, step result, key snapshot) are PROMOTED to frame
//! cells before the CFG is built ([`promote_raw_locals`]), so such a loop may
//! suspend in its body or iterable. A `switch` crossing a suspend is flattened
//! into a chain of strict-equality test blocks and one block per clause body,
//! with fall-through jumps and its own `break` target.
//!
//! A `try` crossing a suspend (or holding a `return`/`break`/`continue`) is
//! flattened too ([`CfgBuilder::lower_try`]). Every block carries an optional
//! HANDLER block: the state machine emits a block with a handler inside a wasm
//! landing pad registered as the active exception handler, so a throw anywhere in
//! it stores `gen.state = handler` and re-dispatches with the exception still
//! pending; the handler block (a `catch` entry, or a `finally`'s on-throw entry)
//! clears it on entry and reads the thrown value through `ExcValue`. A `finally`
//! keeps a COMPLETION RECORD in two frame cells (kind + value), so it survives a
//! suspend inside the `finally`: every exit from the guarded region — fall
//! through, an uncaught throw, `return`, a `break`/`continue` leaving it — records
//! its kind and jumps into the `finally` blocks, which end in a dispatch on the
//! record (continue after the `try`, re-throw to the next outer handler, or
//! resume the `return`/jump, through any outer `finally` first). A throw no
//! handler catches leaves the state machine and completes the generator.
//!
//! Still rejected (deferred): a `yield` whose continuation would read a per-node
//! scratch local set before the suspend; generator expressions/arrows/methods;
//! and non-simple parameters.
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

/// The resume `limits` bits of a generator body (see
/// [`crate::abi::GEN_LIMIT_RETURN`]): a `return()` at a suspended `yield` must
/// run the enclosing `finally` blocks and a `throw()` / `return()` must
/// forward to a `yield*` delegate, which the state machine cannot do.
fn resume_limits(body: &swc_ecma_ast::FunctionBody) -> u32 {
    use swc_ecma_visit::{Visit, VisitWith};

    /// Finds `finally` and `yield*` outside nested functions and classes.
    #[derive(Default)]
    struct Scan {
        finally: bool,
        delegate: bool,
    }
    impl Visit for Scan {
        fn visit_function(&mut self, _: &swc_ecma_ast::Function) {}
        fn visit_arrow_expr(&mut self, _: &swc_ecma_ast::ArrowExpr) {}
        fn visit_class(&mut self, _: &swc_ecma_ast::Class) {}
        fn visit_try_stmt(&mut self, node: &swc_ecma_ast::TryStmt) {
            self.finally |= node.finalizer.is_some();
            node.visit_children_with(self);
        }
        fn visit_yield_expr(&mut self, node: &swc_ecma_ast::YieldExpr) {
            self.delegate |= node.delegate;
            node.visit_children_with(self);
        }
    }
    let mut scan = Scan::default();
    body.visit_with(&mut scan);
    let mut limits = 0;
    if scan.finally || scan.delegate {
        limits |= crate::abi::GEN_LIMIT_RETURN;
    }
    if scan.delegate {
        limits |= crate::abi::GEN_LIMIT_THROW;
    }
    limits
}

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
            limits: resume_limits(block),
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
/// `total` (the frame cell count) grows accordingly. Only a few corner forms are
/// still rejected (see [`CfgBuilder::track`]).
fn build_segments(
    stmts: &[IrStmt],
    fn_decl: &FnDecl,
    total: &mut u32,
) -> anyhow::Result<Vec<GenSegment>> {
    let mut next_slot = *total;
    let stmts = promote_raw_locals(stmts, &mut next_slot)
        .map_err(|msg| CfgBuilder::unsupported_in(fn_decl, msg))?;
    let mut cfg = CfgBuilder {
        fn_decl,
        blocks: vec![PendingBlock::default()],
        cur: 0,
        loops: Vec::new(),
        handlers: Vec::new(),
        finallies: Vec::new(),
        next_slot,
        suspended: false,
        live: HashSet::new(),
    };
    cfg.lower_stmts(&stmts)?;
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
            handler: b.handler,
            catches: b.catches,
        })
        .collect())
}

/// A basic block under construction: its statements, its terminator (`None`
/// until set — a still-open or an unreachable block), and its exception handler
/// (see [`GenSegment`]).
#[derive(Default)]
struct PendingBlock {
    stmts: Vec<IrStmt>,
    term: Option<GenTerm>,
    handler: Option<u32>,
    catches: bool,
}

/// One enclosing loop's (or `switch`'s) jump targets during CFG construction.
struct LoopCtx {
    /// Block a `continue` jumps to (the update block, or the head); `None` for a
    /// `switch`, which a `continue` skips to reach its enclosing loop.
    continue_block: Option<u32>,
    /// Block a `break` jumps to (the loop exit).
    break_block: u32,
    /// The loop's label id, for a labeled `break`/`continue`.
    label: Option<u32>,
    /// How many `finally` clauses enclosed the loop: a jump to its targets runs
    /// every `finally` opened inside it (those above this depth) first.
    finally_depth: usize,
}

/// The completion-record kinds a `finally` dispatches on; a recorded jump to an
/// outer block uses `EXIT_BASE + i` for its `exits[i]`.
const COMPLETION_NORMAL: f64 = 0.0;
const COMPLETION_THROW: f64 = 1.0;
const COMPLETION_RETURN: f64 = 2.0;
const EXIT_BASE: usize = 3;

/// An enclosing `finally` during CFG construction: its completion record cells
/// and the block its body starts in.
struct FinallyCtx {
    /// Frame cell holding the completion kind (`COMPLETION_*` / `EXIT_BASE + i`).
    kind: u32,
    /// Frame cell holding the thrown or returned value.
    value: u32,
    /// The first block of the `finally` body.
    entry: u32,
    /// The jump targets (block, its loop's `finally_depth`) routed through this
    /// `finally`, indexed by `kind - EXIT_BASE`.
    exits: Vec<(u32, usize)>,
}

struct CfgBuilder<'a> {
    fn_decl: &'a FnDecl,
    blocks: Vec<PendingBlock>,
    /// The block currently being filled.
    cur: usize,
    loops: Vec<LoopCtx>,
    /// The enclosing exception handler blocks, innermost last; a new block takes
    /// the innermost as its handler.
    handlers: Vec<u32>,
    /// The enclosing `finally` clauses, innermost last.
    finallies: Vec<FinallyCtx>,
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
        self.blocks.push(PendingBlock {
            handler: self.handlers.last().copied(),
            ..PendingBlock::default()
        });
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
        Self::unsupported_in(self.fn_decl, msg)
    }

    fn unsupported_in(fn_decl: &FnDecl, msg: &str) -> anyhow::Error {
        CompileError::spanned(fn_decl.function.span(), msg.to_owned()).into()
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
                self.return_out(value)?;
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
                    continue_block: Some(cont),
                    break_block: exit,
                    label: *label,
                    finally_depth: self.finallies.len(),
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
                let (target, depth) = self.loop_target(*label, true)?;
                self.jump_out(target, depth)?;
            }
            IrStmt::Continue(label) => {
                let (target, depth) = self.loop_target(*label, false)?;
                self.jump_out(target, depth)?;
            }
            IrStmt::Try {
                body,
                catch_body,
                finally_body,
            } => self.lower_try(body, catch_body.as_deref(), finally_body.as_deref())?,
            IrStmt::Switch { disc, cases, .. } => self.lower_switch(disc, cases)?,
        }
        Ok(())
    }

    /// Flatten a `switch` crossing a suspend (or holding a `break`/`continue`):
    /// the discriminant is evaluated once into a frame cell, then each case test
    /// runs in its own block in source order (so a suspend in a test only happens
    /// when it is reached), comparing with the same strict equality the ordinary
    /// switch emitter uses. A match enters that clause's body block; no match
    /// enters `default` at its source position, or the exit. Each body block
    /// falls through to the next; an unlabeled `break` exits the switch, while a
    /// `continue` skips it to target the enclosing loop.
    fn lower_switch(&mut self, disc: &IrNode, cases: &[IrCase]) -> anyhow::Result<()> {
        self.begin_statement();
        let disc = self.linearize(disc.clone())?;
        let disc_slot = self.fresh_slot();
        self.push(IrStmt::Expr(slot_set(disc_slot, disc)))?;
        let bodies: Vec<u32> = cases.iter().map(|_| self.new_block()).collect();
        let exit = self.new_block();
        for (case, &body) in cases.iter().zip(&bodies) {
            let Some(test) = &case.test else { continue };
            self.begin_statement();
            let test = self.linearize(test.clone())?;
            let cond = IrNode::Binary {
                op: BinOp::Eq,
                lhs: Box::new(slot_get(disc_slot)),
                rhs: Box::new(test),
            };
            self.track(&cond)?;
            let next = self.new_block();
            self.terminate(GenTerm::Branch {
                cond,
                then_block: body,
                else_block: next,
            });
            self.cur = next as usize;
        }
        let fallback = cases
            .iter()
            .position(|case| case.test.is_none())
            .map_or(exit, |d| bodies[d]);
        self.terminate(GenTerm::Jump(fallback));
        self.loops.push(LoopCtx {
            continue_block: None,
            break_block: exit,
            label: None,
            finally_depth: self.finallies.len(),
        });
        for (i, case) in cases.iter().enumerate() {
            self.cur = bodies[i] as usize;
            self.lower_stmts(&case.body)?;
            self.terminate(GenTerm::Jump(bodies.get(i + 1).copied().unwrap_or(exit)));
        }
        self.loops.pop();
        self.cur = exit as usize;
        Ok(())
    }

    /// Flatten a `try` crossing a suspend (or holding a `return`/`break`/
    /// `continue`). The guarded body runs in blocks whose handler is the catch
    /// entry (or, with no `catch`, the `finally`'s on-throw entry); the catch body
    /// runs under the on-throw entry. With a `finally`, every exit from the body
    /// and catch body records a completion (see [`FinallyCtx`]) and enters the
    /// `finally` blocks, which run under the OUTER handler and end in a dispatch
    /// on the record.
    fn lower_try(
        &mut self,
        body: &[IrStmt],
        catch_body: Option<&[IrStmt]>,
        finally_body: Option<&[IrStmt]>,
    ) -> anyhow::Result<()> {
        // Blocks outside the guarded region, created under the outer handler.
        let after = self.new_block();
        let finally = if finally_body.is_some() {
            let ctx = FinallyCtx {
                kind: self.fresh_slot(),
                value: self.fresh_slot(),
                entry: self.new_block(),
                exits: Vec::new(),
            };
            let on_throw = self.new_block();
            self.blocks[on_throw as usize].catches = true;
            self.finallies.push(ctx);
            self.handlers.push(on_throw);
            Some(on_throw)
        } else {
            None
        };
        let catch_entry = catch_body.map(|_| {
            let entry = self.new_block();
            self.blocks[entry as usize].catches = true;
            entry
        });

        // The guarded body.
        if let Some(entry) = catch_entry {
            self.handlers.push(entry);
        }
        let body_b = self.new_block();
        self.terminate(GenTerm::Jump(body_b));
        self.cur = body_b as usize;
        self.lower_stmts(body)?;
        self.complete_normally(after)?;
        if catch_entry.is_some() {
            self.handlers.pop();
        }

        // The catch body, entered with the exception just cleared; its leading
        // statement binds the parameter from `ExcValue`.
        if let (Some(entry), Some(catch_body)) = (catch_entry, catch_body) {
            self.cur = entry as usize;
            self.lower_stmts(catch_body)?;
            self.complete_normally(after)?;
        }

        let (Some(on_throw), Some(finally_body)) = (finally, finally_body) else {
            self.cur = after as usize;
            return Ok(());
        };
        self.handlers.pop();
        let Some(ctx) = self.finallies.pop() else {
            unreachable!("the `finally` context was pushed above")
        };
        // An exception escaping the body / catch body: record it, run `finally`.
        self.cur = on_throw as usize;
        self.push(IrStmt::Expr(slot_set(
            ctx.kind,
            IrNode::Num(COMPLETION_THROW),
        )))?;
        self.push(IrStmt::Expr(slot_set(ctx.value, IrNode::ExcValue)))?;
        self.terminate(GenTerm::Jump(ctx.entry));

        // The finally body, then the dispatch on the completion record (skipped
        // when the finally itself completes abruptly, superseding the record).
        self.cur = ctx.entry as usize;
        self.lower_stmts(finally_body)?;
        let kind_is = |kind: f64| IrNode::Binary {
            op: BinOp::Eq,
            lhs: Box::new(slot_get(ctx.kind)),
            rhs: Box::new(IrNode::Num(kind)),
        };
        let branch = |cfg: &mut Self, kind: f64| -> u32 {
            let taken = cfg.new_block();
            let next = cfg.new_block();
            cfg.terminate(GenTerm::Branch {
                cond: kind_is(kind),
                then_block: taken,
                else_block: next,
            });
            cfg.cur = taken as usize;
            next
        };
        if self.terminated() {
            self.cur = after as usize;
            return Ok(());
        }
        let next = branch(self, COMPLETION_NORMAL);
        self.terminate(GenTerm::Jump(after));
        self.cur = next as usize;
        let next = branch(self, COMPLETION_THROW);
        self.push(IrStmt::Expr(IrNode::Throw(Box::new(slot_get(ctx.value)))))?;
        self.terminate(GenTerm::Return(None)); // unreachable: the throw transfers
        self.cur = next as usize;
        let next = branch(self, COMPLETION_RETURN);
        self.return_out(Some(slot_get(ctx.value)))?;
        self.cur = next as usize;
        for (i, &(target, depth)) in ctx.exits.iter().enumerate() {
            let next = branch(
                self,
                f64::from(u32::try_from(EXIT_BASE + i).unwrap_or(u32::MAX)),
            );
            self.jump_out(target, depth)?;
            self.cur = next as usize;
        }
        self.terminate(GenTerm::Jump(after));
        self.cur = after as usize;
        Ok(())
    }

    /// End a guarded body / catch body that falls through: run the innermost
    /// `finally` with a normal completion, or continue at `after`.
    fn complete_normally(&mut self, after: u32) -> anyhow::Result<()> {
        if self.terminated() {
            return Ok(());
        }
        match self.finallies.last() {
            Some(ctx) => {
                let (kind, entry) = (ctx.kind, ctx.entry);
                self.push(IrStmt::Expr(slot_set(kind, IrNode::Num(COMPLETION_NORMAL))))?;
                self.terminate(GenTerm::Jump(entry));
            }
            None => self.terminate(GenTerm::Jump(after)),
        }
        Ok(())
    }

    /// End the current block with a `return value`: through the innermost
    /// `finally` (recording the value) when one is open, else directly.
    fn return_out(&mut self, value: Option<IrNode>) -> anyhow::Result<()> {
        let Some(ctx) = self.finallies.last() else {
            self.terminate(GenTerm::Return(value));
            return Ok(());
        };
        let (kind, slot, entry) = (ctx.kind, ctx.value, ctx.entry);
        self.push(IrStmt::Expr(slot_set(
            slot,
            value.unwrap_or(IrNode::Undefined),
        )))?;
        self.push(IrStmt::Expr(slot_set(kind, IrNode::Num(COMPLETION_RETURN))))?;
        self.terminate(GenTerm::Jump(entry));
        Ok(())
    }

    /// End the current block with a jump to `target`, a loop/switch block opened
    /// inside `depth` `finally` clauses: through the innermost `finally` opened
    /// since (recording the target as an exit) when there is one, else directly.
    fn jump_out(&mut self, target: u32, depth: usize) -> anyhow::Result<()> {
        if self.finallies.len() <= depth {
            self.terminate(GenTerm::Jump(target));
            return Ok(());
        }
        let Some(ctx) = self.finallies.last_mut() else {
            unreachable!("more `finally` clauses are open than `depth`")
        };
        let index = if let Some(index) = ctx.exits.iter().position(|&exit| exit == (target, depth))
        {
            index
        } else {
            ctx.exits.push((target, depth));
            ctx.exits.len() - 1
        };
        let (kind, entry) = (ctx.kind, ctx.entry);
        let code = f64::from(u32::try_from(EXIT_BASE + index).unwrap_or(u32::MAX));
        self.push(IrStmt::Expr(slot_set(kind, IrNode::Num(code))))?;
        self.terminate(GenTerm::Jump(entry));
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

    /// Resolve a `break`/`continue` target block: the innermost loop or `switch`
    /// for an unlabeled `break`, the innermost loop for an unlabeled `continue`,
    /// or the loop whose label matches — with that loop's `finally_depth`.
    fn loop_target(&self, label: Option<u32>, is_break: bool) -> anyhow::Result<(u32, usize)> {
        let target = match label {
            None if is_break => self.loops.last().map(|l| (l.break_block, l.finally_depth)),
            None => self
                .loops
                .iter()
                .rev()
                .find_map(|l| l.continue_block.map(|b| (b, l.finally_depth))),
            Some(id) => self
                .loops
                .iter()
                .rev()
                .find(|l| l.label == Some(id))
                .and_then(|l| {
                    let block = if is_break {
                        Some(l.break_block)
                    } else {
                        l.continue_block
                    };
                    block.map(|b| (b, l.finally_depth))
                }),
        };
        target
            .ok_or_else(|| self.unsupported("a `break`/`continue` target is not an enclosing loop"))
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

/// Promote to frame cells the raw wasm locals that a CFG-lowered statement may
/// carry across a suspend. In a generator body every source variable is already
/// an env cell, so a raw local declared by a [`IrStmt::Decl`] or written by an
/// [`IrNode::Assign`] is a statement-level compiler temporary (a `for-of`/
/// `for-in` iterable, index, length, step result or key snapshot). Each such
/// local referenced inside a statement that is not emitted inline gets a fresh
/// frame cell (numbered from `next_slot`), and EVERY read and write of it in the
/// whole body is rewritten to that cell, so the inline statements that
/// initialize it write the cell too. Per-node scratch locals (`obj_local`, ...)
/// are never `Decl`/`Assign` targets and stay raw.
fn promote_raw_locals(stmts: &[IrStmt], next_slot: &mut u32) -> Result<Vec<IrStmt>, &'static str> {
    let mut written = HashSet::new();
    let mut crossing = HashSet::new();
    for stmt in stmts {
        collect_locals(stmt, &mut written, &mut crossing, false);
    }
    let mut promoted: Vec<u32> = written.intersection(&crossing).copied().collect();
    promoted.sort_unstable();
    let map: HashMap<u32, u32> = promoted
        .into_iter()
        .map(|local| {
            let slot = *next_slot;
            *next_slot += 1;
            (local, slot)
        })
        .collect();
    let mut stmts = stmts.to_vec();
    if !map.is_empty() {
        for stmt in &mut stmts {
            remap_stmt(stmt, &map)?;
        }
    }
    Ok(stmts)
}

/// Record the raw locals `stmt` declares or assigns (into `written`), and every
/// raw local referenced inside a statement that is not inline (into `crossing`;
/// `in_cfg` marks that an enclosing statement already is not).
fn collect_locals(
    stmt: &IrStmt,
    written: &mut HashSet<u32>,
    crossing: &mut HashSet<u32>,
    in_cfg: bool,
) {
    let in_cfg = in_cfg || !is_inline(stmt);
    if let IrStmt::Decl { local, .. } = stmt {
        written.insert(*local);
        if in_cfg {
            crossing.insert(*local);
        }
    }
    stmt.any_node(|node| {
        collect_node_locals(node, written, crossing, in_cfg);
        false
    });
    stmt.any_stmt(|inner| {
        collect_locals(inner, written, crossing, in_cfg);
        false
    });
}

/// The expression half of [`collect_locals`].
fn collect_node_locals(
    node: &IrNode,
    written: &mut HashSet<u32>,
    crossing: &mut HashSet<u32>,
    in_cfg: bool,
) {
    if let IrNode::Assign { local, .. } = node {
        written.insert(*local);
    }
    if in_cfg
        && let IrNode::Local(local)
        | IrNode::Assign { local, .. }
        | IrNode::IncDec { local, .. }
        | IrNode::ArrayPush { local, .. }
        | IrNode::ArrayPop { local, .. } = node
    {
        crossing.insert(*local);
    }
    node.any_child(|child| {
        collect_node_locals(child, written, crossing, in_cfg);
        false
    });
}

/// Rewrite every access of a promoted local in `stmt` (recursively) to its frame
/// cell (see [`promote_raw_locals`]).
fn remap_stmt(stmt: &mut IrStmt, map: &HashMap<u32, u32>) -> Result<(), &'static str> {
    match stmt {
        IrStmt::Decl { local, init } => {
            remap_node(init, map)?;
            if let Some(&slot) = map.get(local) {
                let init = std::mem::replace(init, IrNode::Undefined);
                *stmt = IrStmt::Expr(slot_set(slot, init));
            }
        }
        IrStmt::Expr(node) | IrStmt::Return(Some(node)) => remap_node(node, map)?,
        IrStmt::Return(None) | IrStmt::Break(_) | IrStmt::Continue(_) => {}
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            remap_node(cond, map)?;
            remap_stmts(then_body, map)?;
            remap_stmts(else_body, map)?;
        }
        IrStmt::While {
            cond, body, update, ..
        } => {
            remap_node(cond, map)?;
            remap_stmts(body, map)?;
            remap_stmts(update, map)?;
        }
        IrStmt::Switch { disc, cases, .. } => {
            remap_node(disc, map)?;
            for case in cases {
                if let Some(test) = &mut case.test {
                    remap_node(test, map)?;
                }
                remap_stmts(&mut case.body, map)?;
            }
        }
        IrStmt::Try {
            body,
            catch_body,
            finally_body,
        } => {
            remap_stmts(body, map)?;
            if let Some(catch_body) = catch_body {
                remap_stmts(catch_body, map)?;
            }
            if let Some(finally_body) = finally_body {
                remap_stmts(finally_body, map)?;
            }
        }
    }
    Ok(())
}

fn remap_stmts(stmts: &mut [IrStmt], map: &HashMap<u32, u32>) -> Result<(), &'static str> {
    stmts.iter_mut().try_for_each(|stmt| remap_stmt(stmt, map))
}

/// The expression half of [`remap_stmt`]: `Local` becomes a cell read, `Assign`
/// a cell write, and `++`/`--` the read-modify-write a captured cell uses.
fn remap_node(node: &mut IrNode, map: &HashMap<u32, u32>) -> Result<(), &'static str> {
    for child in node.children_mut() {
        remap_node(child, map)?;
    }
    let slot = match node {
        IrNode::Local(local)
        | IrNode::Assign { local, .. }
        | IrNode::IncDec { local, .. }
        | IrNode::ArrayPush { local, .. }
        | IrNode::ArrayPop { local, .. } => match map.get(local) {
            Some(&slot) => slot,
            None => return Ok(()),
        },
        _ => return Ok(()),
    };
    *node = match std::mem::replace(node, IrNode::Undefined) {
        IrNode::Local(_) => slot_get(slot),
        IrNode::Assign { value, .. } => slot_set(slot, *value),
        IrNode::IncDec { inc, prefix, .. } => {
            let delta = if inc { 1.0 } else { -1.0 };
            let store = slot_set(
                slot,
                IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::ConvertCall {
                        op: ConvertOp::ToNumber,
                        arg: Box::new(slot_get(slot)),
                        radix: None,
                    }),
                    rhs: Box::new(IrNode::Num(delta)),
                },
            );
            if prefix {
                store
            } else {
                IrNode::Binary {
                    op: BinOp::Sub,
                    lhs: Box::new(store),
                    rhs: Box::new(IrNode::Num(delta)),
                }
            }
        }
        _ => {
            return Err(
                "an in-place array update of a compiler temporary is not supported inside a generator",
            );
        }
    };
    Ok(())
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
        Some(IrNode::Str(message.to_owned())),
        IrNode::Str(format!("TypeError: {message}")),
    )
}
