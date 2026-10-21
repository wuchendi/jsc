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

        // A base class constructor defines the instance fields first (design
        // 4.5: right after allocation, before the parameters are bound).
        if self.frame().ctor == FnKind::Class
            && let Some(this_local) = self.ensure_this_local()
            && let Some(fields) = self.lower_field_inits(IrNode::Local(this_local))?
        {
            body.push(IrStmt::Expr(fields));
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

        if self.frame().derived()
            && let BodyKind::Block(stmts) = body_kind
        {
            self.frame().supers.top_level = stmts.iter().find_map(super_call_span);
            if stmts.iter().any(|s| stmt_arrow_super(s, false)) {
                let (cell, id) = self.hidden_local();
                self.frame().supers.this_cell = Some(id);
                body.push(IrStmt::Expr(IrNode::Assign {
                    local: cell,
                    value: Box::new(IrNode::CellNew(Box::new(IrNode::Hole))),
                }));
            }
        }
        match body_kind {
            BodyKind::Block(stmts) => self.lower_stmts(stmts, &mut body)?,
            BodyKind::Expr(expr) => {
                let node = self.lower_expr(expr)?;
                body.push(IrStmt::Return(Some(node)));
            }
        }

        // A derived constructor falling off its end returns `this`, which must
        // have been bound by `super(..)`.
        let derived = self.frame().derived();
        if derived {
            let fi = self.top();
            let this = self.derived_this(fi);
            body.push(IrStmt::Return(Some(this)));
        }

        // If the body read `this`, prepend the prologue snapshot as the VERY FIRST
        // statement: `this_slot = __this`, so a nested `this.other()` that
        // re-sets the global cannot disturb this frame's own receiver. A function
        // that never reads `this` needs no snapshot (its slot was never allocated),
        // leaving its IR unchanged. A derived constructor's `this` starts in the
        // TDZ instead (the hole), until `super(..)` binds it.
        if let Some(this_local) = self.frame().this_local {
            body.insert(
                0,
                IrStmt::Expr(IrNode::Assign {
                    local: this_local,
                    value: Box::new(if derived {
                        IrNode::Hole
                    } else {
                        IrNode::ThisGlobal
                    }),
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

        // `arguments` (referenced here or by a nested arrow): built from the
        // incoming slots before any parameter is boxed, defaulted or assigned
        // (unmapped semantics).
        if let Some(local) = self.frame().arguments_local {
            let init = self.lower_arguments_object(local, k);
            body.splice(0..0, init);
        }
        // A function gathering its arguments snapshots `__spread_args` first,
        // before any call in its prologue or body can overwrite it.
        if let Some(local) = self.frame().spread_args_local {
            body.insert(
                0,
                IrStmt::Decl {
                    local,
                    init: IrNode::SpreadArgsGlobal,
                },
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
    /// excluding the `undefined` padding beyond the real arguments — and the
    /// arguments `k..n` of a spread call are appended from the spread argument
    /// array ([`Self::overflow_loop`]). The array is
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
                    not_array: None,
                })],
                else_body: Vec::new(),
            });
        }
        // Arguments beyond the `K` slots come from the spread argument array.
        let spread = self.spread_args_local();
        let overflow = self.overflow_loop(n, k, spread, |_, value| IrNode::ArrayPush {
            local: tmp,
            value: Box::new(value),
            not_array: None,
        });
        body.push(overflow);
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

    /// The prologue building this function's `arguments` object into `local`:
    /// an ordinary object of class ARGUMENTS (`proto` the `%Object.prototype%`
    /// placeholder `null`) with a non-enumerable `length` = `n` and the own
    /// properties `"0".."n-1"` holding every actual argument (slots `0..k`, then
    /// the spread overflow). Unmapped: the values are copied, never aliased.
    fn lower_arguments_object(&mut self, local: u32, k: u32) -> Vec<IrStmt> {
        let n = self.fresh();
        let mut out = vec![
            IrStmt::Decl {
                local: n,
                init: IrNode::ArgCount(k + 1),
            },
            IrStmt::Decl {
                local,
                init: IrNode::Object {
                    entries: vec![(PropKey::Hidden("length".to_owned()), IrNode::Local(n))],
                    proto: None,
                    class: crate::abi::CLASS_ARGUMENTS,
                    reserve: k + 1,
                },
            },
        ];
        let set = |index: IrNode, value: IrNode| IrNode::IndexSet {
            obj: Box::new(IrNode::Local(local)),
            index: Box::new(index),
            value: Box::new(value),
        };
        for j in 0..k {
            out.push(IrStmt::If {
                cond: IrNode::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(IrNode::Num(f64::from(j))),
                    rhs: Box::new(IrNode::Local(n)),
                },
                then_body: vec![IrStmt::Expr(set(
                    IrNode::Num(f64::from(j)),
                    IrNode::Local(j),
                ))],
                else_body: Vec::new(),
            });
        }
        let spread = self.spread_args_local();
        out.push(self.overflow_loop(n, k, spread, set));
        out
    }

    /// `if (k < n) for (let i = k; i < n; i++) store(i, spread[i])`: hand each
    /// argument beyond the `K` slots — only a spread call passes more than `K`
    /// — from the spread argument array snapshot in `spread` to `store`. `n` is
    /// the local holding the decoded argument count.
    fn overflow_loop(
        &mut self,
        n: u32,
        k: u32,
        spread: u32,
        store: impl FnOnce(IrNode, IrNode) -> IrNode,
    ) -> IrStmt {
        let i = self.fresh();
        let index = IrNode::Local(i);
        let value = IrNode::Index {
            obj: Box::new(IrNode::Local(spread)),
            index: Box::new(index.clone()),
        };
        IrStmt::If {
            cond: IrNode::Binary {
                op: BinOp::Lt,
                lhs: Box::new(IrNode::Num(f64::from(k))),
                rhs: Box::new(IrNode::Local(n)),
            },
            then_body: vec![
                IrStmt::Decl {
                    local: i,
                    init: IrNode::Num(f64::from(k)),
                },
                IrStmt::While {
                    cond: IrNode::Binary {
                        op: BinOp::Lt,
                        lhs: Box::new(index.clone()),
                        rhs: Box::new(IrNode::Local(n)),
                    },
                    body: vec![IrStmt::Expr(store(index.clone(), value))],
                    update: vec![IrStmt::Expr(IrNode::Assign {
                        local: i,
                        value: Box::new(IrNode::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(index),
                            rhs: Box::new(IrNode::Num(1.0)),
                        }),
                    })],
                    label: None,
                },
            ],
            else_body: Vec::new(),
        }
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
        if matches!(kind, FnKind::Class | FnKind::DerivedClass) {
            self.frame().ctor = kind;
        }
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
        if matches!(kind, FnKind::Class | FnKind::DerivedClass) {
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
    /// forms all lower the same way. Arrows read the enclosing function's
    /// `new.target`/`arguments`, and are not constructors. `name` is the
    /// inferred `name` (`""` when anonymous).
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

impl LowerCtx<'_> {
    /// The derived constructor frame whose `this` the current code reads: the
    /// current frame, or the one enclosing the current arrows when its `this`
    /// lives in a cell ([`SuperCalls::this_cell`]).
    pub(super) fn derived_frame(&self) -> Option<usize> {
        let fi = self.frames.iter().rposition(|f| !f.arrow)?;
        let frame = &self.frames[fi];
        (frame.derived() && (fi == self.top() || frame.supers.this_cell.is_some())).then_some(fi)
    }

    /// The current value of derived constructor frame `fi`'s `this` (the
    /// hole before `super(..)`), unchecked.
    pub(super) fn derived_this_raw(&mut self, fi: usize) -> IrNode {
        match self.frames[fi].supers.this_cell {
            Some(id) => IrNode::CellGet(Box::new(self.read_hidden(fi, id))),
            None => IrNode::Local(self.ensure_this_local().unwrap_or(0)),
        }
    }

    /// Read derived constructor frame `fi`'s `this`: TDZ-checked unless a
    /// top-level `super(..)` already bound it (see [`SuperCalls::top_level`]).
    pub(super) fn derived_this(&mut self, fi: usize) -> IrNode {
        let raw = self.derived_this_raw(fi);
        if fi == self.top() && self.frame().supers.this_bound {
            return raw;
        }
        IrNode::Ternary {
            cond: Box::new(IrNode::IsHole(Box::new(raw.clone()))),
            then_branch: Box::new(IrNode::Throw(Box::new(reference_error(DERIVED_THIS_TDZ)))),
            else_branch: Box::new(raw),
        }
    }

    /// Bind derived constructor frame `fi`'s `this` to `value`.
    pub(super) fn derived_bind_this(&mut self, fi: usize, value: IrNode) -> IrNode {
        match self.frames[fi].supers.this_cell {
            Some(id) => IrNode::CellSet {
                cell: Box::new(self.read_hidden(fi, id)),
                value: Box::new(value),
            },
            None => IrNode::Assign {
                local: self.ensure_this_local().unwrap_or(0),
                value: Box::new(value),
            },
        }
    }

    /// Define the current class's instance fields ([`Self::class_fields`]) on
    /// `this` in source order with `CreateDataProperty` semantics (design 11.2
    /// q8: `__define`, so an inherited setter is not invoked); `None` when the
    /// class declares no fields. An uninitialized `f;` defines `undefined`.
    pub(super) fn lower_field_inits(&mut self, this: IrNode) -> anyhow::Result<Option<IrNode>> {
        if self.class_fields.is_empty() {
            return Ok(None);
        }
        let fields = std::mem::take(&mut self.class_fields);
        let mut entries = Vec::with_capacity(fields.len());
        for p in &fields {
            let key = super::stmt::prop_key_name(&p.key)?;
            let value = match &p.value {
                Some(init) => self.lower_named_expr(init, &key)?,
                None => IrNode::Undefined,
            };
            entries.push((PropKey::Static(key), value));
        }
        self.class_fields = fields;
        let local = self.fresh();
        Ok(Some(IrNode::DefineProps {
            obj: Box::new(this),
            local,
            entries,
        }))
    }
}

/// The span of the `super(..)` call when `stmt` is the expression statement
/// `super(..);`.
fn super_call_span(stmt: &Stmt) -> Option<swc_common::Span> {
    let Stmt::Expr(expr_stmt) = stmt else {
        return None;
    };
    match strip_parens(&expr_stmt.expr) {
        Expr::Call(call) if matches!(call.callee, Callee::Super(_)) => Some(call.span),
        _ => None,
    }
}

/// Whether `stmt` contains a `super(..)` call, `this`, or `super.x` inside an
/// arrow function (`arrow`: already inside one), not descending into non-arrow
/// functions or classes. Conservative: an arrow `super(..)` in a shape not
/// walked here stays a compile error.
fn stmt_arrow_super(stmt: &Stmt, arrow: bool) -> bool {
    let e = |x: &Expr| expr_arrow_super(x, arrow);
    let s = |x: &Stmt| stmt_arrow_super(x, arrow);
    match stmt {
        Stmt::Expr(x) => e(&x.expr),
        Stmt::Return(r) => r.arg.as_deref().is_some_and(e),
        Stmt::Throw(t) => e(&t.arg),
        Stmt::Block(b) => b.stmts.iter().any(s),
        Stmt::If(i) => e(&i.test) || s(&i.cons) || i.alt.as_deref().is_some_and(s),
        Stmt::Decl(Decl::Var(v)) => v.decls.iter().any(|d| d.init.as_deref().is_some_and(e)),
        Stmt::Try(t) => {
            t.block.stmts.iter().any(s)
                || t.handler
                    .as_ref()
                    .is_some_and(|h| h.body.stmts.iter().any(s))
                || t.finalizer.as_ref().is_some_and(|f| f.stmts.iter().any(s))
        }
        Stmt::While(w) => e(&w.test) || s(&w.body),
        Stmt::DoWhile(w) => e(&w.test) || s(&w.body),
        Stmt::Labeled(l) => s(&l.body),
        _ => false,
    }
}

/// [`stmt_arrow_super`] for an expression.
fn expr_arrow_super(expr: &Expr, arrow: bool) -> bool {
    let e = |x: &Expr| expr_arrow_super(x, arrow);
    match expr {
        Expr::Call(c) => {
            (arrow && matches!(c.callee, Callee::Super(_)))
                || matches!(&c.callee, Callee::Expr(x) if e(x))
                || c.args.iter().any(|a| e(&a.expr))
        }
        Expr::Arrow(a) => match a.body.as_ref() {
            ArrowFunctionBody::FunctionBody(b) => b.stmts.iter().any(|s| stmt_arrow_super(s, true)),
            ArrowFunctionBody::Expr(x) => expr_arrow_super(x, true),
        },
        // An arrow's `this` / `super.x` is the constructor's (lexical) `this`.
        Expr::This(_) | Expr::SuperProp(_) => arrow,
        Expr::Paren(p) => e(&p.expr),
        Expr::Seq(q) => q.exprs.iter().any(|x| e(x)),
        Expr::Assign(a) => e(&a.right),
        Expr::Bin(b) => e(&b.left) || e(&b.right),
        Expr::Cond(c) => e(&c.test) || e(&c.cons) || e(&c.alt),
        Expr::Unary(u) => e(&u.arg),
        Expr::Member(m) => e(&m.obj),
        Expr::New(n) => e(&n.callee) || n.args.iter().flatten().any(|a| e(&a.expr)),
        _ => false,
    }
}

/// The derived-constructor `this` TDZ message (design 4.5).
pub(super) const DERIVED_THIS_TDZ: &str = "Must call super constructor in derived class before accessing 'this' or returning from derived constructor";

/// A `ReferenceError` with `message`.
pub(super) fn reference_error(message: &str) -> IrNode {
    IrNode::error_object(
        ErrorKind::ReferenceError,
        Some(IrNode::Str(message.to_owned())),
        IrNode::Str(format!("ReferenceError: {message}")),
    )
}

/// A `TypeError` with `message`: a string literal, or a side-effect-free read
/// of a string (evaluated twice, for `message` and `stack`).
pub(super) fn type_error(message: IrNode) -> IrNode {
    let stack = match &message {
        IrNode::Str(text) => IrNode::Str(format!("TypeError: {text}")),
        _ => IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str("TypeError: ".to_owned())),
            rhs: Box::new(message.clone()),
        },
    };
    IrNode::error_object(ErrorKind::TypeError, Some(message), stack)
}

/// `cond ? undefined : throw error`: a guard throwing `error` unless `cond`.
pub(super) fn guard(cond: IrNode, error: IrNode) -> IrNode {
    IrNode::Ternary {
        cond: Box::new(cond),
        then_branch: Box::new(IrNode::Undefined),
        else_branch: Box::new(IrNode::Throw(Box::new(error))),
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
            Some(IrNode::Str(message)),
            IrNode::Str(stack),
        ))))],
    }
}
