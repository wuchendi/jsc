#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::Spanned;

use crate::error::CompileError;

/// Lower a single top-level function declaration into an [`IrFunction`].
///
/// The locals are laid out for the uniform ABI: locals `0..K` are the fixed
/// positional-argument slots (`K = max_arity`; the source parameters bind to
/// the first of them, surplus slots stay unused), local `K` is the **env slot**
/// (`undefined` for a non-capturing function), and body-declared locals follow.
/// Every function therefore shares the one wasm signature
/// `(a0..a_{K-1}, env) -> i64`, so a single `call_indirect` type serves them
/// all. A top-level function does not close over `main`'s locals (top-level
/// bindings it references live in module cells), so its environment layout is
/// always empty. The body may call any function in
/// `funcs`, including itself (recursion).
pub(super) fn lower_function(fn_decl: &FnDecl, shared: &Shared<'_>) -> anyhow::Result<IrFunction> {
    let mut ctx = LowerCtx::new(shared);
    let param_pats: Vec<&Pat> = fn_decl.function.params.iter().map(|p| &p.pat).collect();
    let name = fn_decl.ident.sym.as_ref();
    // A top-level `function*` lowers to a generator constructor + state machine,
    // not an ordinary function body. A top-level `async function` compiles its
    // body to a generator state machine (`await` == `yield`) with a wrapper that
    // spawns it as a promise; `lower_generator` handles both, keyed on
    // `function.is_async`. (An `async function*` — an async generator — stays
    // out of scope and is rejected by `reject_unsupported_function` below.)
    // Neither is a constructor.
    if fn_decl.function.is_generator != fn_decl.function.is_async {
        let mut func = ctx.lower_generator(fn_decl)?;
        func.info = fn_info(name, &param_pats, FnKind::Plain);
        return Ok(func);
    }
    let block = fn_decl
        .function
        .body
        .as_ref()
        .ok_or_else(|| anyhow!("function declaration requires a body"))?;
    reject_unsupported_function(&fn_decl.function)?;
    let (mut func, _env) = ctx.build_function_body(&param_pats, BodyKind::Block(&block.stmts))?;
    func.info = fn_info(name, &param_pats, FnKind::Constructor);
    Ok(func)
}

/// The [`FnInfo`] of a function named `name` with parameters `param_pats`:
/// its `length` counts the parameters before the first default or rest.
pub(super) fn fn_info(name: &str, param_pats: &[&Pat], kind: FnKind) -> FnInfo {
    let length = param_pats
        .iter()
        .take_while(|p| !matches!(p, Pat::Assign(_) | Pat::Rest(_)))
        .count();
    FnInfo {
        name: name.to_owned(),
        length: u32::try_from(length).unwrap_or(u32::MAX),
        kind,
    }
}

/// Reject the out-of-scope function forms — `async function` and generator
/// (`function*`) — that the function-value lowering would otherwise compile
/// as an *ordinary* function and run to a **wrong result** (an `async` body
/// yields a `Promise`, a `function*` yields a generator object; neither is
/// modeled). A clear compile error is emitted instead of silently
/// mis-handling them. See DESIGN.md.
pub(super) fn reject_unsupported_function(function: &swc_ecma_ast::Function) -> anyhow::Result<()> {
    if function.is_async {
        return Err(
            CompileError::spanned(function.span(), "async functions are unsupported").into(),
        );
    }
    if function.is_generator {
        return Err(
            CompileError::spanned(function.span(), "generator functions are unsupported").into(),
        );
    }
    Ok(())
}

impl LowerCtx<'_> {
    /// Lower the parameters, env slot, capture-boxing prologue, and body of a
    /// function/arrow into the current frame, returning the [`IrFunction`] and
    /// this frame's environment layout (the captured binding ids it references,
    /// in env-slot order). Operates on the current top frame (the caller pushes
    /// one for a nested function; a top-level function uses the initial frame).
    #[allow(clippy::too_many_lines)]
    fn build_function_body(
        &mut self,
        param_pats: &[&Pat],
        body_kind: BodyKind<'_>,
    ) -> anyhow::Result<(IrFunction, Vec<u32>)> {
        // Parameters bind to locals `0..`; a captured parameter is a boxed cell.
        // A `Pat::Assign` (`a = default`) binds its `Pat::Ident` like a plain
        // parameter and records the default expression for the prologue guard.
        let mut boxed_params = Vec::new();
        let mut defaults: Vec<(u32, &Expr)> = Vec::new();
        // A trailing rest parameter: its binding ident and its start
        // position `p` (the number of preceding positional parameters). It binds a
        // synthesized array rather than a single positional slot, so it is built
        // after the slots are laid out (below), not declared inline here.
        let mut rest: Option<(&swc_ecma_ast::BindingIdent, u32)> = None;
        // Destructuring parameters: each holds the temp positional slot the
        // incoming argument lands in, the pattern to destructure it with, and an
        // optional parameter-level default (`f([a,b] = [1,2])`). Destructured in
        // the prologue (below) by reusing `lower_binding_pattern`.
        let mut destructure: Vec<(u32, &Pat, Option<&Expr>)> = Vec::new();
        for (i, pat) in param_pats.iter().enumerate() {
            let (ident, default) = match pat {
                Pat::Ident(ident) => (ident, None),
                Pat::Assign(assign) => match assign.left.as_ref() {
                    Pat::Ident(ident) => (ident, Some(assign.right.as_ref())),
                    // A destructuring pattern with a parameter-level default binds
                    // the argument to a fresh temp slot (guarded by the default),
                    // then destructures it in the prologue.
                    left @ (Pat::Array(_) | Pat::Object(_)) => {
                        let temp = self.fresh();
                        destructure.push((temp, left, Some(assign.right.as_ref())));
                        continue;
                    }
                    _ => {
                        return Err(CompileError::spanned(
                            assign.left.span(),
                            "unsupported parameter pattern",
                        )
                        .into());
                    }
                },
                // A destructuring parameter (`f([a,b])`, `g({x,y})`): bind the
                // incoming argument to a fresh temp positional slot, destructured
                // into the named locals in the prologue.
                Pat::Array(_) | Pat::Object(_) => {
                    let temp = self.fresh();
                    destructure.push((temp, *pat, None));
                    continue;
                }
                Pat::Rest(rest_pat) => {
                    // A rest parameter must be the final parameter.
                    if i + 1 != param_pats.len() {
                        return Err(CompileError::spanned(
                            rest_pat.span(),
                            "rest parameter must be last",
                        )
                        .into());
                    }
                    let Pat::Ident(ident) = rest_pat.arg.as_ref() else {
                        return Err(CompileError::spanned(
                            rest_pat.arg.span(),
                            "unsupported rest parameter pattern",
                        )
                        .into());
                    };
                    rest = Some((ident, u32::try_from(i).unwrap_or(u32::MAX)));
                    continue;
                }
                _ => {
                    return Err(
                        CompileError::spanned(pat.span(), "unsupported parameter pattern").into(),
                    );
                }
            };
            let local = self.declare(ident.id.sym.as_ref(), true, binding_id(ident));
            if self.is_cell(local) {
                boxed_params.push(local);
            }
            if let Some(default) = default {
                defaults.push((local, default));
            }
        }
        // Pad the surplus argument slots to `K = max_arity`, then reserve the two
        // trailing ABI slots: the env slot (local `K`) and the argument-count slot
        // (local `K + 1`).
        let k = u32::try_from(self.shared.max_arity).unwrap_or(u32::MAX);
        while self.frame().next < k {
            self.fresh();
        }
        self.fresh();
        self.fresh();
        let params = self.frame().next;

        // Mark this frame as a function scope so a body `this` binds here;
        // the `this` slot itself is allocated lazily, only if the body reads it.
        self.frame().allows_this = true;

        // Prologue: move each captured parameter's incoming value into a fresh
        // cell, so reads/writes (and the closure) share it by reference.
        let mut body = Vec::new();
        for local in boxed_params {
            body.push(IrStmt::Expr(IrNode::Assign {
                local,
                value: Box::new(IrNode::CellNew(Box::new(IrNode::Local(local)))),
            }));
        }

        // Default-parameter guards, IN PARAMETER ORDER (after capture-boxing so a
        // defaulted cell param reads/writes its boxed cell).
        self.lower_default_guards(defaults, &mut body)?;

        // Trailing rest parameter: gather the surplus arguments into a
        // real array bound to the rest name (after the fixed params / defaults).
        if let Some((rest_ident, p)) = rest {
            self.lower_rest_param(rest_ident, p, k, &mut body);
        }

        // Destructuring parameters: each temp slot holds the incoming
        // argument; apply an optional parameter-level default, then destructure it
        // into the named locals by reusing the `lower_binding_pattern`
        // helper (which handles nested patterns, holes, defaults, and rest).
        for (temp, pat, default) in destructure {
            if let Some(default) = default {
                let default_value = self.lower_expr(default)?;
                body.push(IrStmt::If {
                    cond: IrNode::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(IrNode::Local(temp)),
                        rhs: Box::new(IrNode::Undefined),
                    },
                    then_body: vec![IrStmt::Expr(IrNode::Assign {
                        local: temp,
                        value: Box::new(default_value),
                    })],
                    else_body: Vec::new(),
                });
            }
            self.lower_binding_pattern(pat, IrNode::Local(temp), true, &mut body)?;
        }

        match body_kind {
            BodyKind::Block(stmts) => self.lower_stmts(stmts, &mut body)?,
            BodyKind::Expr(expr) => {
                let node = self.lower_expr(expr)?;
                body.push(IrStmt::Return(Some(node)));
            }
        }

        // If the body read `this`, prepend the prologue snapshot as the VERY FIRST
        // statement: `this_slot = __this`, so a nested `this.other()` that
        // re-sets the global cannot disturb this frame's own receiver. A function
        // that never reads `this` needs no snapshot (its slot was never allocated),
        // leaving its IR unchanged.
        if let Some(this_local) = self.frame().this_local {
            body.insert(
                0,
                IrStmt::Expr(IrNode::Assign {
                    local: this_local,
                    value: Box::new(IrNode::ThisGlobal),
                }),
            );
        }
        // Likewise `new.target` (referenced here or by a nested arrow): the
        // `__new_target` global under the construct marker (`argc < 0`), else
        // `undefined`.
        if let Some(local) = self.frame().new_target_local {
            body.insert(
                0,
                IrStmt::Expr(IrNode::Assign {
                    local,
                    value: Box::new(IrNode::Ternary {
                        cond: Box::new(IrNode::IsConstructCall(k + 1)),
                        then_branch: Box::new(IrNode::NewTargetGlobal),
                        else_branch: Box::new(IrNode::Undefined),
                    }),
                }),
            );
        }

        let locals = self.frame().next;
        let env_layout = self.frame().env.clone();
        Ok((
            IrFunction {
                params,
                locals,
                body,
                machine: None,
                info: FnInfo::default(),
            },
            env_layout,
        ))
    }

    /// Emit the default-parameter guards in parameter order: each emits
    /// `if (param === undefined) param = <default>`. The default is evaluated per
    /// call only on the undefined path and may reference earlier params, which are
    /// already bound (and, if defaulted, already guarded before it). A cell param
    /// reads/writes through its boxed cell.
    fn lower_default_guards(
        &mut self,
        defaults: Vec<(u32, &Expr)>,
        body: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        for (local, default) in defaults {
            let cell = self.is_cell(local);
            let read = if cell {
                IrNode::CellGet(Box::new(IrNode::Local(local)))
            } else {
                IrNode::Local(local)
            };
            let default_value = self.lower_expr(default)?;
            let write = if cell {
                IrNode::CellSet {
                    cell: Box::new(IrNode::Local(local)),
                    value: Box::new(default_value),
                }
            } else {
                IrNode::Assign {
                    local,
                    value: Box::new(default_value),
                }
            };
            body.push(IrStmt::If {
                cond: IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(read),
                    rhs: Box::new(IrNode::Undefined),
                },
                then_body: vec![IrStmt::Expr(write)],
                else_body: Vec::new(),
            });
        }
        Ok(())
    }

    /// Emit the prologue for a trailing rest parameter: collect
    /// `[arg_p, .., arg_{argc-1}]` into a REAL array bound to the rest name.
    ///
    /// `argc` (local `k + 1`, a number jsval) is the actual source argument count
    /// `n`, or the construct marker `-(n + 1)` (decoded first), so each
    /// positional slot `j in p..k` is appended only when `j < n` —
    /// excluding the `undefined` padding beyond the real arguments. The array is
    /// built in a plain temp local (`ArrayPush` needs a plain array binding), then
    /// bound to the rest name, boxed into a cell when the binding is captured.
    fn lower_rest_param(
        &mut self,
        rest_ident: &swc_ecma_ast::BindingIdent,
        p: u32,
        k: u32,
        body: &mut Vec<IrStmt>,
    ) {
        let n = self.fresh();
        body.push(IrStmt::Decl {
            local: n,
            init: IrNode::ArgCount(k + 1),
        });
        let argc = IrNode::Local(n);
        let tmp = self.fresh();
        body.push(IrStmt::Decl {
            local: tmp,
            init: IrNode::Array(Vec::new()),
        });
        for j in p..k {
            body.push(IrStmt::If {
                cond: IrNode::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(IrNode::Num(f64::from(j))),
                    rhs: Box::new(argc.clone()),
                },
                then_body: vec![IrStmt::Expr(IrNode::ArrayPush {
                    local: tmp,
                    value: Box::new(IrNode::Local(j)),
                })],
                else_body: Vec::new(),
            });
        }
        let rest_local = self.declare(rest_ident.id.sym.as_ref(), true, binding_id(rest_ident));
        let init = if self.is_cell(rest_local) {
            IrNode::CellNew(Box::new(IrNode::Local(tmp)))
        } else {
            IrNode::Local(tmp)
        };
        body.push(IrStmt::Decl {
            local: rest_local,
            init,
        });
    }

    /// Lift a function/arrow to a top-level [`IrFunction`] (registered in the
    /// funcref table) and produce the [`IrNode::FuncValue`] that builds its
    /// value at the use site, with the capture sources for its environment.
    /// `name` and `kind` become the function object's `name` and constructor
    /// kind.
    pub(super) fn lower_lifted_function(
        &mut self,
        param_pats: &[&Pat],
        body_kind: BodyKind<'_>,
        name: &str,
        kind: FnKind,
    ) -> anyhow::Result<IrNode> {
        self.lower_lifted(param_pats, body_kind, name, kind, false)
    }

    /// [`Self::lower_lifted_function`], for an arrow body when `arrow` is set
    /// (it reads its enclosing function's `new.target`).
    fn lower_lifted(
        &mut self,
        param_pats: &[&Pat],
        body_kind: BodyKind<'_>,
        name: &str,
        kind: FnKind,
        arrow: bool,
    ) -> anyhow::Result<IrNode> {
        // Reserve this function's table slot before lowering its body, so any
        // function nested in the body takes a later slot.
        let pos = {
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
        let table_index = self.shared.arrow_base - 1 + u32::try_from(pos).unwrap_or(u32::MAX);

        self.push_frame();
        self.frame().arrow = arrow;
        let built = self.build_function_body(param_pats, body_kind);
        let (mut func, env_layout) = match built {
            Ok(pair) => pair,
            Err(e) => {
                self.pop_frame();
                return Err(e);
            }
        };
        self.pop_frame();
        // A class constructor called without the construct marker throws
        // before anything else runs (design 4.5).
        if kind == FnKind::Class {
            func.body.insert(0, class_call_guard(name, func.params - 1));
        }
        func.info = fn_info(name, param_pats, kind);
        self.shared.arrows.borrow_mut()[pos] = func;

        // Build the capture sources in the enclosing frame (now the top frame).
        let captures = env_layout
            .iter()
            .map(|&id| self.capture_source(id))
            .collect();
        Ok(IrNode::FuncValue {
            table_index,
            captures,
            kind,
        })
    }

    /// Lower a nested `function foo(){ .. }` declaration in place (outside a
    /// hoisting statement list, see [`Self::lower_stmts`]): bind `foo` to its
    /// function value in the current scope. `foo` is not visible inside its own
    /// body here (lowered before the name is bound).
    pub(super) fn lower_nested_fn_decl(
        &mut self,
        fn_decl: &FnDecl,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let value = self.lower_nested_fn_value(fn_decl)?;
        let local = self.declare(fn_decl.ident.sym.as_ref(), true, fn_decl.ident.span.lo.0);
        // A generator frame's binding lives in its env cell (`push_binding`).
        if self.env_slot_for_local(local).is_some() {
            self.push_binding(local, value, out);
            return Ok(());
        }
        let init = if self.is_cell(local) {
            IrNode::CellNew(Box::new(value))
        } else {
            value
        };
        out.push(IrStmt::Decl { local, init });
        Ok(())
    }

    /// Lift a nested function declaration's body to a top-level function
    /// (registered in the funcref table, may capture enclosing bindings like an
    /// arrow) and return the [`IrNode::FuncValue`] building its value.
    pub(super) fn lower_nested_fn_value(&mut self, fn_decl: &FnDecl) -> anyhow::Result<IrNode> {
        let block = fn_decl
            .function
            .body
            .as_ref()
            .ok_or_else(|| anyhow!("function declaration requires a body"))?;
        reject_unsupported_function(&fn_decl.function)?;
        let param_pats: Vec<&Pat> = fn_decl.function.params.iter().map(|p| &p.pat).collect();
        self.lower_lifted_function(
            &param_pats,
            BodyKind::Block(&block.stmts),
            fn_decl.ident.sym.as_ref(),
            FnKind::Constructor,
        )
    }

    /// Lower an arrow function `(a, b) => a + b` / `x => x * x` / `(n) => { .. }`
    /// into a first-class function value. It lifts to a top-level
    /// [`IrFunction`] registered in the funcref table and the use-site produces an
    /// [`IrNode::FuncValue`], capturing (by reference, through boxed cells) any
    /// enclosing bindings the body references. An expression body (`=> expr`)
    /// desugars to `return expr`; a block body (`=> { .. }`) lowers its statements
    /// directly. Single-parameter (`x => ..`) and parenthesized zero/multi-parameter
    /// forms all lower the same way. Arrows carry no `this`/`arguments`, and are
    /// not constructors. `name` is the inferred `name` (`""` when anonymous).
    pub(super) fn lower_arrow(&mut self, arrow: &ArrowExpr, name: &str) -> anyhow::Result<IrNode> {
        // `async`/generator arrows are out of scope: reject rather than
        // silently compile the body as a plain arrow (wrong result).
        if arrow.is_async {
            return Err(
                CompileError::spanned(arrow.span(), "async functions are unsupported").into(),
            );
        }
        if arrow.is_generator {
            return Err(
                CompileError::spanned(arrow.span(), "generator functions are unsupported").into(),
            );
        }
        let param_pats: Vec<&Pat> = arrow.params.iter().collect();
        let body_kind = match arrow.body.as_ref() {
            ArrowFunctionBody::FunctionBody(block) => BodyKind::Block(&block.stmts),
            ArrowFunctionBody::Expr(expr) => BodyKind::Expr(expr),
        };
        self.lower_lifted(&param_pats, body_kind, name, FnKind::Plain, true)
    }
}

/// `if (argc >= 0) throw TypeError("Class constructor <name> cannot be invoked
/// without 'new'")`, the class constructor prologue rejecting a plain call
/// (`argc` is the local `argc`).
fn class_call_guard(name: &str, argc: u32) -> IrStmt {
    let message = format!("Class constructor {name} cannot be invoked without 'new'");
    let stack = format!("TypeError: {message}");
    IrStmt::If {
        cond: IrNode::IsConstructCall(argc),
        then_body: Vec::new(),
        else_body: vec![IrStmt::Expr(IrNode::Throw(Box::new(IrNode::error_object(
            ErrorKind::TypeError,
            None,
            Some(IrNode::Str(message)),
            IrNode::Str(stack),
        ))))],
    }
}
