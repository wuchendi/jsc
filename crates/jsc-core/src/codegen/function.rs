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
    let block = fn_decl
        .function
        .body
        .as_ref()
        .ok_or_else(|| anyhow!("function declaration requires a body"))?;
    let body = BodyKind::Block(&block.stmts);
    // A top-level `function*` / `async function` lowers to a generator
    // constructor + state machine, not an ordinary function body; neither is a
    // constructor. A generator reads its own `prototype` through its static
    // function object.
    if let Some(machine) = state_machine(&fn_decl.function) {
        let callee = shared
            .funcs
            .get(name)
            .filter(|_| machine.has_prototype())
            .map(|sig| IrNode::DeclFunc(sig.index - 1));
        let (mut func, _env) =
            ctx.build_generator_ctor(&param_pats, body, machine, fn_decl.function.span, callee)?;
        func.info = fn_info(name, &param_pats, machine.fn_kind());
        return Ok(func);
    }
    let (mut func, _env) = ctx.build_function_body(&param_pats, body)?;
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

/// What a function compiles to when its body is a generator state machine
/// (DESIGN.md §7.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Machine {
    /// A `function*`: the constructor returns the generator.
    Generator,
    /// An `async function` or arrow: the constructor spawns the body as a
    /// promise; `await e` suspends like `yield e`.
    Async,
    /// An `async function*`: the constructor returns the async generator,
    /// driven by `%AsyncGeneratorPrototype%`; both `await` and `yield`
    /// suspend.
    AsyncGenerator,
}

impl Machine {
    /// Whether `await` suspends the body.
    pub(super) fn awaits(self) -> bool {
        self != Self::Generator
    }

    /// Whether the function owns a `prototype` its objects inherit.
    pub(super) fn has_prototype(self) -> bool {
        self != Self::Async
    }

    /// The [`FnKind`] of the function's constructor.
    pub(super) fn fn_kind(self) -> FnKind {
        match self {
            Self::Generator => FnKind::Generator,
            Self::Async => FnKind::Plain,
            Self::AsyncGenerator => FnKind::AsyncGenerator,
        }
    }
}

/// Whether `function` compiles to a generator state machine, and which
/// kind; `None` for an ordinary function.
pub(super) fn state_machine(function: &swc_ecma_ast::Function) -> Option<Machine> {
    match (function.is_generator, function.is_async) {
        (true, true) => Some(Machine::AsyncGenerator),
        (true, false) => Some(Machine::Generator),
        (false, true) => Some(Machine::Async),
        (false, false) => None,
    }
}

/// A function's declared parameters still to bind in its prologue (see
/// [`LowerCtx::declare_params`]), in source order.
pub(super) struct Params<'p> {
    binds: Vec<ParamBind<'p>>,
    /// The ids of every parameter binding.
    ids: Vec<u32>,
    /// Whether the list has parameter expressions (see
    /// [`has_param_expressions`]): parameters then bind left to right with a
    /// TDZ.
    tdz: bool,
}

/// One declared parameter still to bind.
enum ParamBind<'p> {
    /// An identifier parameter: its local and binding id, its default, and —
    /// for a cell whose TDZ a closure may observe — the temp local holding
    /// the incoming argument while the cell holds the hole.
    Ident {
        local: u32,
        id: u32,
        default: Option<&'p Expr>,
        arg: Option<u32>,
    },
    /// A trailing rest parameter: its binding ident and its start position
    /// `p` (the number of preceding positional parameters). It binds a
    /// synthesized array rather than a single positional slot.
    Rest(&'p swc_ecma_ast::BindingIdent, u32),
    /// A destructuring parameter: the temp positional slot the incoming
    /// argument lands in, the pattern, and an optional parameter-level default
    /// (`f([a,b] = [1,2])`).
    Pattern {
        temp: u32,
        pat: &'p Pat,
        default: Option<&'p Expr>,
    },
}

impl LowerCtx<'_> {
    /// Lower the parameters, env slot, capture-boxing prologue, and body of a
    /// function/arrow into the current frame, returning the [`IrFunction`] and
    /// this frame's environment layout (the captured binding ids it references,
    /// in env-slot order). Operates on the current top frame (the caller pushes
    /// one for a nested function; a top-level function uses the initial frame).
    fn build_function_body(
        &mut self,
        param_pats: &[&Pat],
        body_kind: BodyKind<'_>,
    ) -> anyhow::Result<(IrFunction, Vec<u32>)> {
        let (params, mut body) = self.declare_params(param_pats)?;

        // A base class constructor defines the instance fields first (design
        // 4.5: right after allocation, before the parameters are bound).
        if self.frame().ctor == FnKind::Class
            && let Some(this_local) = self.ensure_this_local()
            && let Some(fields) = self.lower_field_inits(IrNode::Local(this_local))?
        {
            body.push(IrStmt::Expr(fields));
        }

        self.bind_params(params, &mut body)?;
        if let BodyKind::Block(stmts) = body_kind {
            self.bind_vars(stmts, &mut body)?;
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
        if self.frame().derived() {
            let fi = self.top();
            let this = self.derived_this(fi);
            body.push(IrStmt::Return(Some(this)));
        }
        Ok(self.finish_function(body))
    }

    /// Declare the parameters `param_pats` in the current frame and lay out
    /// its uniform-ABI slots: parameters bind to locals `0..`, padded to
    /// `K = max_arity`, then the env slot (local `K`) and the argument-count
    /// slot (local `K + 1`). The names a destructuring or rest parameter
    /// binds are declared too, so a default resolves every parameter (DESIGN.md
    /// §7.4). Returns the [`Params`] still to bind (see [`Self::bind_params`])
    /// and the prologue moving each captured parameter's incoming value into a
    /// fresh cell, so reads/writes (and the closure) share it by reference.
    /// With parameter expressions such a cell starts as the TDZ hole instead
    /// (the argument waits in a temp), until the parameter binds.
    #[allow(clippy::too_many_lines)]
    pub(super) fn declare_params<'p>(
        &mut self,
        param_pats: &[&'p Pat],
    ) -> anyhow::Result<(Params<'p>, Vec<IrStmt>)> {
        let tdz = has_param_expressions(param_pats);
        let mut params = Params {
            binds: Vec::new(),
            ids: Vec::new(),
            tdz,
        };
        let mut boxed_params = Vec::new();
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
                        params.binds.push(ParamBind::Pattern {
                            temp,
                            pat: left,
                            default: Some(assign.right.as_ref()),
                        });
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
                    params.binds.push(ParamBind::Pattern {
                        temp,
                        pat,
                        default: None,
                    });
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
                    params
                        .binds
                        .push(ParamBind::Rest(ident, u32::try_from(i).unwrap_or(u32::MAX)));
                    continue;
                }
                _ => {
                    return Err(
                        CompileError::spanned(pat.span(), "unsupported parameter pattern").into(),
                    );
                }
            };
            let id = binding_id(ident);
            let local = self.declare(ident.id.sym.as_ref(), true, id);
            if self.is_cell(local) {
                boxed_params.push(local);
            }
            params.binds.push(ParamBind::Ident {
                local,
                id,
                default,
                arg: None,
            });
        }
        let k = u32::try_from(self.shared.max_arity).unwrap_or(u32::MAX);
        while self.frame().next < k {
            self.fresh();
        }
        self.fresh();
        self.fresh();

        // Mark this frame as a function scope so a body `this` binds here;
        // the `this` slot itself is allocated lazily, only if the body reads it.
        self.frame().allows_this = true;

        let mut prologue = Vec::new();
        for bind in &mut params.binds {
            if let ParamBind::Ident { local, arg, .. } = bind
                && boxed_params.contains(local)
            {
                let local = *local;
                let init = if tdz {
                    let temp = self.fresh();
                    prologue.push(IrStmt::Decl {
                        local: temp,
                        init: IrNode::Local(local),
                    });
                    *arg = Some(temp);
                    IrNode::Hole
                } else {
                    IrNode::Local(local)
                };
                prologue.push(IrStmt::Expr(IrNode::Assign {
                    local,
                    value: Box::new(IrNode::CellNew(Box::new(init))),
                }));
            }
        }

        // The destructured and rest names, declared up front; with parameter
        // expressions a captured one's cell holds the hole until it binds.
        let mut names = Vec::new();
        for bind in &params.binds {
            match bind {
                ParamBind::Ident { .. } => {}
                ParamBind::Rest(ident, _) => {
                    names.push((ident.id.sym.as_ref().to_owned(), binding_id(ident)));
                }
                ParamBind::Pattern { pat, .. } => pat_bindings(pat, &mut names),
            }
        }
        for (name, id) in &names {
            let local = self.declare(name, true, *id);
            if tdz && self.is_cell(local) {
                prologue.push(IrStmt::Decl {
                    local,
                    init: IrNode::CellNew(Box::new(IrNode::Hole)),
                });
            }
        }
        params.ids = params
            .binds
            .iter()
            .filter_map(|bind| match bind {
                ParamBind::Ident { id, .. } => Some(*id),
                _ => None,
            })
            .chain(names.iter().map(|(_, id)| *id))
            .collect();
        let frame = self.frame();
        frame.var_ids.extend(params.ids.iter().copied());
        if tdz {
            for pat in param_pats {
                pat_bindings(pat, &mut frame.params);
            }
        }
        Ok((params, prologue))
    }

    /// Bind the declared parameters `params` into `body`, left to right
    /// (after capture-boxing, so a defaulted cell param reads/writes its boxed
    /// cell): each identifier's default guard, the trailing rest array, each
    /// destructuring parameter. With parameter expressions a parameter is in
    /// its TDZ until it binds: a reference from the parameter code throws a
    /// `ReferenceError` (DESIGN.md §7.4).
    pub(super) fn bind_params(
        &mut self,
        params: Params<'_>,
        body: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let k = u32::try_from(self.shared.max_arity).unwrap_or(u32::MAX);
        let pending = if params.tdz {
            params.ids.iter().copied().collect()
        } else {
            std::collections::HashSet::new()
        };
        self.frame().binding = Some(ParamBinding {
            tdz: params.tdz,
            pending,
        });
        let result = self.bind_params_in_order(params.binds, k, body);
        self.frame().binding = None;
        result
    }

    fn bind_params_in_order(
        &mut self,
        binds: Vec<ParamBind<'_>>,
        k: u32,
        body: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        for bind in binds {
            match bind {
                ParamBind::Ident {
                    local,
                    id,
                    default,
                    arg,
                } => {
                    if let Some(arg) = arg {
                        // A TDZ cell: default the argument temp, then
                        // initialize the cell from it.
                        if let Some(default) = default {
                            self.lower_default_guard(arg, false, default, body)?;
                        }
                        body.push(IrStmt::Expr(IrNode::CellSet {
                            cell: Box::new(IrNode::Local(local)),
                            value: Box::new(IrNode::Local(arg)),
                        }));
                    } else if let Some(default) = default {
                        let cell = self.is_cell(local);
                        self.lower_default_guard(local, cell, default, body)?;
                    }
                    self.param_bound(id);
                }
                // Trailing rest parameter: gather the surplus arguments into a
                // real array bound to the rest name.
                ParamBind::Rest(rest_ident, p) => self.lower_rest_param(rest_ident, p, k, body),
                // Destructuring parameters: each temp slot holds the incoming
                // argument; apply an optional parameter-level default, then
                // destructure it into the named locals by reusing the
                // `lower_binding_pattern` helper (which handles nested
                // patterns, holes, defaults, and rest).
                ParamBind::Pattern { temp, pat, default } => {
                    if let Some(default) = default {
                        self.lower_default_guard(temp, false, default, body)?;
                    }
                    self.lower_binding_pattern(pat, IrNode::Local(temp), true, body)?;
                }
            }
        }
        Ok(())
    }

    /// Mark parameter `id` initialized: it leaves its TDZ.
    fn param_bound(&mut self, id: u32) {
        if let Some(binding) = &mut self.frame().binding {
            binding.pending.remove(&id);
        }
    }

    /// The local of the pre-declared parameter `name` (binding `id`) while the
    /// current frame binds its parameters, marking it initialized, and
    /// whether its cell already exists (holding the TDZ hole). `None` outside
    /// parameter binding.
    pub(super) fn bind_declared_param(&mut self, name: &str, id: u32) -> Option<(u32, bool)> {
        let tdz = self.frame().binding.as_ref()?.tdz;
        let local = self
            .frame()
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).filter(|b| b.id == id))?
            .local;
        self.param_bound(id);
        Some((local, tdz && self.is_cell(local)))
    }

    /// Hoist the body `stmts`' `var`s into the current frame (DESIGN.md
    /// §7.1): each name binds once, to `undefined`, before any statement. A
    /// `var` redeclaring a parameter or a top-level function declaration of
    /// the body reuses it — except that with parameter expressions one
    /// redeclaring a parameter is a separate binding initialized from the
    /// parameter's value (the parameter code's closures keep the parameter).
    /// The function's parameters live in this frame, or in its constructor's
    /// for a generator body.
    pub(super) fn bind_vars(
        &mut self,
        stmts: &[Stmt],
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let top = self.top();
        let fi = if self.frames[top].env_backed {
            top.saturating_sub(1)
        } else {
            top
        };
        let fns = fn_decl_names(stmts);
        for (name, id) in hoisted_vars(stmts) {
            if fns.contains(name.as_str()) {
                continue;
            }
            let value = if self.frames[fi].params.iter().any(|(p, _)| *p == name) {
                let Some(param) = self.resolve(&name) else {
                    return Err(anyhow!("parameter `{name}` is unbound"));
                };
                Self::read_of(param.kind)
            } else if self.own_var_binding(&name) {
                continue;
            } else {
                IrNode::Undefined
            };
            let local = self.declare(&name, true, id);
            self.frame().var_ids.insert(id);
            self.push_binding(local, value, out);
        }
        Ok(())
    }

    /// Finish the current frame's function around its lowered `body`: prepend
    /// the entry snapshots the body allocated (`this`, `new.target`,
    /// `arguments`, `__spread_args`) and return the [`IrFunction`] with this
    /// frame's environment layout.
    pub(super) fn finish_function(&mut self, mut body: Vec<IrStmt>) -> (IrFunction, Vec<u32>) {
        let k = u32::try_from(self.shared.max_arity).unwrap_or(u32::MAX);
        let derived = self.frame().derived();
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

        // `arguments` (referenced here, by a nested arrow, or by a generator
        // body): built from the incoming slots before any parameter is boxed,
        // defaulted or assigned (unmapped semantics).
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

        let env_layout = self.frame().env.clone();
        (
            IrFunction {
                params: k + 2,
                locals: self.frame().next,
                body,
                machine: None,
                info: FnInfo::default(),
            },
            env_layout,
        )
    }

    /// Emit a default-parameter guard: `if (param === undefined) param =
    /// <default>`, evaluated per call only on the undefined path. A `cell`
    /// param reads/writes through its boxed cell.
    fn lower_default_guard(
        &mut self,
        local: u32,
        cell: bool,
        default: &Expr,
        body: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
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
    /// array ([`Self::overflow_loop`]). The array is built in a temp local
    /// ([`IrNode::ArrayPush`] appends to a handle held in a wasm local), then
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
        // Arguments beyond the `K` slots come from the spread argument array.
        let spread = self.spread_args_local();
        let overflow = self.overflow_loop(n, k, spread, |_, value| IrNode::ArrayPush {
            local: tmp,
            value: Box::new(value),
        });
        body.push(overflow);
        self.bind_ident(rest_ident, IrNode::Local(tmp), true, body);
    }

    /// The prologue building this function's `arguments` object into `local`:
    /// an ordinary object of class ARGUMENTS (`proto` `%Object.prototype%`)
    /// with a non-enumerable `length` = `n` and the own
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
        self.lower_lifted(param_pats, body_kind, name, kind, false, None)
    }

    /// [`Self::lower_lifted_function`], for an arrow body when `arrow` is set
    /// (it reads its enclosing function's `new.target` and `arguments`), and
    /// for a generator (`machine = Some((false, span))`) or async
    /// (`Some((true, span))`) body, whose lifted function is the generator
    /// constructor ([`Self::build_generator_ctor`]). A generator constructor
    /// reads its own function object (for its `prototype`) from a hidden
    /// cell of the enclosing frame, filled right after the value is built.
    fn lower_lifted(
        &mut self,
        param_pats: &[&Pat],
        body_kind: BodyKind<'_>,
        name: &str,
        kind: FnKind,
        arrow: bool,
        machine: Option<(Machine, swc_common::Span)>,
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
        let self_cell = machine
            .is_some_and(|(machine, _)| machine.has_prototype())
            .then(|| (self.top(), self.hidden_local()));

        self.push_frame();
        self.frame().arrow = arrow;
        if matches!(kind, FnKind::Class | FnKind::DerivedClass) {
            self.frame().ctor = kind;
        }
        let callee =
            self_cell.map(|(fi, (_, id))| IrNode::CellGet(Box::new(self.read_hidden(fi, id))));
        let built = match machine {
            Some((machine, span)) => {
                self.build_generator_ctor(param_pats, body_kind, machine, span, callee)
            }
            None => self.build_function_body(param_pats, body_kind),
        };
        let (mut func, env_layout) = match built {
            Ok(pair) => pair,
            Err(e) => {
                self.pop_frame();
                return Err(e);
            }
        };
        self.pop_frame();
        // A class constructor called without the construct marker throws
        // before anything else runs (DESIGN.md §7.6).
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
        let value = IrNode::FuncValue {
            table_index,
            captures,
            kind,
        };
        let Some((_, (cell, _))) = self_cell else {
            return Ok(value);
        };
        let func = self.fresh();
        Ok(IrNode::Seq(vec![
            IrNode::Assign {
                local: cell,
                value: Box::new(IrNode::CellNew(Box::new(IrNode::Undefined))),
            },
            IrNode::Assign {
                local: func,
                value: Box::new(value),
            },
            IrNode::CellSet {
                cell: Box::new(IrNode::Local(cell)),
                value: Box::new(IrNode::Local(func)),
            },
        ]))
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
        self.frame().var_ids.insert(fn_decl.ident.span.lo.0);
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
        self.lower_function_value(&fn_decl.function, fn_decl.ident.sym.as_ref())
    }

    /// Lift a `function`-keyword function (a nested declaration or an
    /// expression) named `name` to a top-level function and return the
    /// [`IrNode::FuncValue`] building its value: a constructor for an ordinary
    /// function, a generator constructor (not a constructor itself) for a
    /// `function*` / `async function`.
    pub(super) fn lower_function_value(
        &mut self,
        function: &swc_ecma_ast::Function,
        name: &str,
    ) -> anyhow::Result<IrNode> {
        self.lower_callable(function, name, FnKind::Constructor)
    }

    /// Lift an object-literal or class method (or accessor) `function` named
    /// `name`: an ordinary method, or the generator constructor of a
    /// generator / async method. Methods are not constructors.
    pub(super) fn lower_method(
        &mut self,
        function: &swc_ecma_ast::Function,
        name: &str,
    ) -> anyhow::Result<IrNode> {
        self.lower_callable(function, name, FnKind::Plain)
    }

    /// [`Self::lower_function_value`] / [`Self::lower_method`]: an ordinary
    /// `function` lifts with `kind`, a generator / async one to its generator
    /// constructor.
    fn lower_callable(
        &mut self,
        function: &swc_ecma_ast::Function,
        name: &str,
        kind: FnKind,
    ) -> anyhow::Result<IrNode> {
        let block = function
            .body
            .as_ref()
            .ok_or_else(|| anyhow!("function requires a body"))?;
        let param_pats: Vec<&Pat> = function.params.iter().map(|p| &p.pat).collect();
        let body = BodyKind::Block(&block.stmts);
        match state_machine(function) {
            Some(machine) => self.lower_lifted(
                &param_pats,
                body,
                name,
                machine.fn_kind(),
                false,
                Some((machine, function.span)),
            ),
            None => self.lower_lifted_function(&param_pats, body, name, kind),
        }
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
        let param_pats: Vec<&Pat> = arrow.params.iter().collect();
        let body_kind = match arrow.body.as_ref() {
            ArrowFunctionBody::FunctionBody(block) => BodyKind::Block(&block.stmts),
            ArrowFunctionBody::Expr(expr) => BodyKind::Expr(expr),
        };
        // An `async` arrow's body is an async body (a generator state machine
        // spawned as a promise).
        let machine = arrow.is_async.then_some((Machine::Async, arrow.span));
        self.lower_lifted(&param_pats, body_kind, name, FnKind::Plain, true, machine)
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
    /// `this` in source order with `CreateDataProperty` semantics
    /// (`__define`, so an inherited setter is not invoked); `None` when the
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

/// The derived-constructor `this` TDZ message (DESIGN.md §7.6).
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
