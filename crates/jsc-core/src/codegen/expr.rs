#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::{Span, Spanned};

use crate::error::CompileError;

impl LowerCtx<'_> {
    /// Recursively lower a single [`Expr`] into an [`IrNode`].
    ///
    /// # Errors
    ///
    /// Returns an error for any expression outside the supported numeric subset
    /// (unsupported operators, undeclared identifiers, calls, and so on).
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_expr(&mut self, expr: &Expr) -> anyhow::Result<IrNode> {
        match expr {
            Expr::Lit(Lit::Num(n)) => Ok(IrNode::Num(n.value)),
            Expr::Lit(Lit::Str(s)) => Ok(IrNode::Str(s.value.to_string_lossy().into_owned())),
            Expr::Lit(Lit::Bool(b)) => Ok(IrNode::Bool(b.value)),
            Expr::Lit(Lit::Null(_)) => Ok(IrNode::Null),
            // A declared local shadows a function of the same name and the
            // global `undefined`; a function name in value position becomes a
            // first-class function value; a bare, otherwise undeclared
            // `undefined` lowers to the `undefined` literal. A captured binding
            // reads through its boxed cell.
            Expr::Ident(ident) => self.lower_ident(ident.sym.as_ref(), ident.span),
            Expr::Paren(p) => self.lower_expr(&p.expr),
            // An arrow function lifts to a top-level function value.
            Expr::Arrow(arrow) => self.lower_arrow(arrow),
            // A function expression `function (..) { .. }` lifts the same way as
            // an arrow (its optional name is not bound), e.g. a closure factory's
            // `return function(){ .. }`.
            Expr::Fn(fn_expr) => {
                let block = fn_expr
                    .function
                    .body
                    .as_ref()
                    .ok_or_else(|| anyhow!("function expression requires a body"))?;
                reject_unsupported_function(&fn_expr.function)?;
                let param_pats: Vec<&Pat> =
                    fn_expr.function.params.iter().map(|p| &p.pat).collect();
                self.lower_lifted_function(&param_pats, BodyKind::Block(&block.stmts))
            }
            Expr::Assign(assign) => self.lower_assign(assign),
            Expr::Update(update) => self.lower_update(update),
            Expr::Call(call) => self.lower_call(call),
            // `await p`: at the top level, drive the microtask queue until
            // `p` settles, then yield its value (or unwind its rejection). Inside
            // an `async function` body, the body is compiled as a
            // generator state machine, so `await e` becomes a `yield e` — the host
            // async engine resumes the machine with the awaited settled value.
            Expr::Await(await_expr) => {
                let arg = self.lower_expr(&await_expr.arg)?;
                if self.async_body {
                    Ok(IrNode::Yield(Box::new(arg)))
                } else {
                    Ok(IrNode::Await(Box::new(arg)))
                }
            }
            // `yield e` / `yield`: lower the operand and wrap it. The
            // generator segmentation pass consumes every `Yield`; one that reaches
            // the backend means an unsupported yield position (rejected there).
            // `yield*` (delegate) is a statement, lowered by `lower_yield_delegate`.
            Expr::Yield(yield_expr) if !yield_expr.delegate => {
                let arg = match &yield_expr.arg {
                    Some(arg) => self.lower_expr(arg)?,
                    None => IrNode::Undefined,
                };
                Ok(IrNode::Yield(Box::new(arg)))
            }
            // Optional chaining `a?.b` / `a?.[i]` / `a?.()`: each `?.` link
            // short-circuits the whole chain to `undefined` when its base is
            // null/undefined (P0.4).
            Expr::OptChain(chain) => self.lower_opt_chain(chain),
            // `<expr>.length` reads a string's/array's byte length; `<expr>.a`
            // (any other static property) is an object read, lowered to the same
            // `Index` node as `<expr>["a"]`; `<expr>[<idx>]` (computed access)
            // indexes a string/array/object at runtime.
            // `Math.PI` / `Math.E` are constant property READS (no call), not
            // object indexing: `Math` is not a real binding, so when nothing named
            // `Math` is in scope they lower to their `f64` literal.
            Expr::Member(member)
                if math_member_name(member).is_some_and(|n| n == "PI" || n == "E")
                    && self.resolve("Math").is_none() =>
            {
                match math_member_name(member) {
                    Some("PI") => Ok(IrNode::Num(std::f64::consts::PI)),
                    Some("E") => Ok(IrNode::Num(std::f64::consts::E)),
                    _ => unreachable!("guard restricts the property to PI/E"),
                }
            }
            Expr::Member(member) => match &member.prop {
                MemberProp::Ident(id) if id.sym.as_ref() == "length" => Ok(IrNode::StrLen(
                    Box::new(nullish_check(self.lower_expr(&member.obj)?, "length")),
                )),
                // `obj.a` ≡ `obj["a"]`: reuse the runtime `Index` dispatch, with
                // the property name lowered to a heap-string key.
                MemberProp::Ident(id) => Ok(IrNode::Index {
                    obj: Box::new(self.lower_expr(&member.obj)?),
                    index: Box::new(IrNode::Str(id.sym.as_ref().to_owned())),
                }),
                MemberProp::Computed(c) => Ok(IrNode::Index {
                    obj: Box::new(self.lower_expr(&member.obj)?),
                    index: Box::new(self.lower_expr(&c.expr)?),
                }),
                MemberProp::PrivateName(_) => Err(CompileError::spanned(
                    member.span(),
                    "private fields are not supported",
                )
                .into()),
            },
            // An array literal `[e0, e1, ..]` lowers to a heap array.
            Expr::Array(arr) => self.lower_array(arr),
            // An object literal `{a: e0, ..}` lowers to a heap object.
            Expr::Object(obj) => self.lower_object(obj),
            // `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
            // lower to a plain heap object `{ name, message }`.
            Expr::New(new_expr) => self.lower_new(new_expr),
            Expr::Cond(cond) => Ok(IrNode::Ternary {
                cond: Box::new(self.lower_expr(&cond.test)?),
                then_branch: Box::new(self.lower_expr(&cond.cons)?),
                else_branch: Box::new(self.lower_expr(&cond.alt)?),
            }),
            // `&&`/`||`/`??` are short-circuit logical operators (swc models them
            // as `BinaryOp`s); the rest are the eager arithmetic/comparison
            // binaries.
            // Exponentiation `a ** b` lowers to the same power op as `Math.pow(a, b)`
            // (swc parses `**` right-associative, so `2 ** 3 ** 2` is already
            // `2 ** (3 ** 2)`; the AST is lowered as given).
            Expr::Bin(b) if matches!(b.op, BinaryOp::Exp) => Ok(IrNode::MathCall {
                op: MathOp::Pow,
                args: vec![self.lower_expr(&b.left)?, self.lower_expr(&b.right)?],
            }),
            // `key in obj`: an own-key membership test, lowered to a runtime
            // key-table scan. `left` is the property key, `right` the object.
            Expr::Bin(b) if matches!(b.op, BinaryOp::In) => Ok(IrNode::ObjectHas {
                key: Box::new(self.lower_expr(&b.left)?),
                obj: Box::new(self.lower_expr(&b.right)?),
            }),
            // `x instanceof RHS` (E-series). The right-hand side must be a bare
            // identifier naming EITHER a user class (checked first, so a
            // `class Error {}` shadows the builtin) OR one of the three builtin
            // error constructors (only when unshadowed); anything else is a clean
            // compile error (jsc models no general prototype-chain `instanceof`).
            Expr::Bin(b) if matches!(b.op, BinaryOp::InstanceOf) => {
                self.lower_instanceof(&b.left, &b.right)
            }
            Expr::Bin(b) => match map_logical_op(b.op) {
                Some(op) => Ok(IrNode::Logical {
                    op,
                    lhs: Box::new(self.lower_expr(&b.left)?),
                    rhs: Box::new(self.lower_expr(&b.right)?),
                }),
                None => Ok(IrNode::Binary {
                    op: map_binary_op(b.op, b.span())?,
                    lhs: Box::new(self.lower_expr(&b.left)?),
                    rhs: Box::new(self.lower_expr(&b.right)?),
                }),
            },
            Expr::Unary(u) => match u.op {
                // Unary minus is constant-folded when the operand lowers to a
                // literal (`-5` becomes `Num(-5.0)`); otherwise it is
                // represented as `-0 - operand`, which flips the sign of zero
                // (`-0 - +0` is `-0`, `-0 - -0` is `+0`) and negates any other
                // number.
                UnaryOp::Minus => {
                    let operand = self.lower_expr(&u.arg)?;
                    match operand {
                        IrNode::Num(v) => Ok(IrNode::Num(-v)),
                        other => Ok(IrNode::Binary {
                            op: BinOp::Sub,
                            lhs: Box::new(IrNode::Num(-0.0)),
                            rhs: Box::new(other),
                        }),
                    }
                }
                // Unary plus is ToNumber (the same conversion as `Number(x)`),
                // a no-op on a numeric literal.
                UnaryOp::Plus => match self.lower_expr(&u.arg)? {
                    num @ IrNode::Num(_) => Ok(num),
                    other => Ok(IrNode::ConvertCall {
                        op: ConvertOp::ToNumber,
                        arg: Box::new(other),
                    }),
                },
                // `typeof operand` inspects the operand's runtime type.
                UnaryOp::TypeOf => Ok(IrNode::TypeOf(Box::new(self.lower_expr(&u.arg)?))),
                // `!operand` is logical NOT: boolean inverse of truthiness.
                UnaryOp::Bang => Ok(IrNode::Not(Box::new(self.lower_expr(&u.arg)?))),
                // `delete obj.k` / `delete obj[k]` removes an own property and
                // yields `true`. Only a member reference is supported; a
                // bare `delete x` (a no-op in non-strict JS) is rejected.
                UnaryOp::Delete => match &*u.arg {
                    Expr::Member(member) => {
                        let obj = self.lower_expr(&member.obj)?;
                        let key = match &member.prop {
                            MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
                            MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
                            MemberProp::PrivateName(_) => {
                                return Err(CompileError::spanned(
                                    member.span(),
                                    "private fields are not supported",
                                )
                                .into());
                            }
                        };
                        Ok(IrNode::ObjectDelete {
                            obj: Box::new(obj),
                            key: Box::new(key),
                        })
                    }
                    other => Err(CompileError::spanned(
                        other.span(),
                        "`delete` requires a property reference",
                    )
                    .into()),
                },
                _ => Err(CompileError::spanned(
                    u.span(),
                    format!("unsupported unary operator `{}`", u.op.as_str()),
                )
                .into()),
            },
            // A template literal `` `a${x}b` `` folds into left-associated `+`
            // concatenations, reusing the string-coercion path. Tagged
            // templates stay unsupported (handled by the fallback below).
            Expr::Tpl(tpl) => self.lower_tpl(tpl),
            // `this` reads the current function's snapshotted `this` slot,
            // allocated on first use; outside any function (the top-level `main`
            // frame) there is no `this` binding, so it stays unsupported. Inside a
            // deferred `static` field initializer (P2b) `this` is the class itself,
            // so it resolves to the class binding.
            Expr::This(this_expr) => {
                if let Some(class) = self.static_init_class.clone() {
                    return self.lower_ident(&class, this_expr.span);
                }
                match self.ensure_this_local() {
                    Some(local) => Ok(IrNode::Local(local)),
                    None => {
                        Err(CompileError::spanned(this_expr.span, "`this` is not supported").into())
                    }
                }
            }
            other => Err(CompileError::spanned(other.span(), unsupported_expr_msg(other)).into()),
        }
    }

    /// Lower a template literal into left-folded `BinOp::Add` concatenations,
    /// reusing the `+` string-coercion path so interpolated values render
    /// exactly like `"a" + x + "b"` (a non-string operand coerces because the
    /// running accumulator is always a string).
    ///
    /// A template always has exactly one more quasi than expr, interleaved in
    /// source order. Seeding with the first quasi (an `IrNode::Str`, possibly
    /// empty) keeps the whole result a string even with no interpolation
    /// (`` `abc` `` => `"abc"`) and when the sole interpolation coerces a
    /// non-string (`` `${5}` `` => `"5"`).
    fn lower_tpl(&mut self, tpl: &swc_ecma_ast::Tpl) -> anyhow::Result<IrNode> {
        let cooked = |q: &swc_ecma_ast::TplElement| {
            q.cooked
                .as_ref()
                .map_or_else(String::new, |c| c.to_string_lossy().into_owned())
        };
        let mut acc = IrNode::Str(cooked(&tpl.quasis[0]));
        for (expr, quasi) in tpl.exprs.iter().zip(tpl.quasis.iter().skip(1)) {
            acc = IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(acc),
                rhs: Box::new(self.lower_expr(expr)?),
            };
            acc = IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(acc),
                rhs: Box::new(IrNode::Str(cooked(quasi))),
            };
        }
        Ok(acc)
    }

    /// Lower an optional-chaining expression `a?.b` / `a?.[i]` / `a?.()` (and any
    /// non-optional continuation links such as the `.c` in `a?.b.c`, which swc
    /// wraps in the same [`OptChainExpr`] tree).
    ///
    /// The chain is flattened into its root base expression plus an ordered list
    /// of member/index/call links (base-to-outer). A `?.` link short-circuits the
    /// WHOLE remaining chain to `undefined` when its base is `null`/`undefined`:
    /// the base is evaluated once into a fresh temp, tested for nullishness, and
    /// the tail (every following link) sits in the `else` arm of a [`IrNode::Ternary`]
    /// so it is skipped on the short-circuit path. A non-optional link is applied
    /// directly to the running value with no guard.
    fn lower_opt_chain(&mut self, chain: &OptChainExpr) -> anyhow::Result<IrNode> {
        let mut links = Vec::new();
        let root = collect_chain_links(chain, &mut links);
        let base = self.lower_expr(root)?;
        self.build_chain(base, &links)
    }

    /// Fold the flattened chain `links` onto the running `value`, threading each
    /// optional link's short-circuit through the `else` arm of a `Ternary` (see
    /// [`Self::lower_opt_chain`]).
    fn build_chain(&mut self, value: IrNode, links: &[ChainLink<'_>]) -> anyhow::Result<IrNode> {
        let Some((link, rest)) = links.split_first() else {
            return Ok(value);
        };
        let optional = match link {
            ChainLink::Member { optional, .. } | ChainLink::Call { optional, .. } => *optional,
        };
        if optional {
            // Evaluate the base once into `temp`, then continue the tail through a
            // read of `temp` so the base's side effects run exactly once.
            let temp = self.fresh();
            let applied = self.apply_link(IrNode::Local(temp), link)?;
            let tail = self.build_chain(applied, rest)?;
            // nullish(temp) ≡ `temp === null || temp === undefined`; the base is
            // teed into `temp` while computing the first comparison.
            let is_nullish = IrNode::Logical {
                op: LogicalOp::Or,
                lhs: Box::new(IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(IrNode::Assign {
                        local: temp,
                        value: Box::new(value),
                    }),
                    rhs: Box::new(IrNode::Null),
                }),
                rhs: Box::new(IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(IrNode::Local(temp)),
                    rhs: Box::new(IrNode::Undefined),
                }),
            };
            Ok(IrNode::Ternary {
                cond: Box::new(is_nullish),
                then_branch: Box::new(IrNode::Undefined),
                else_branch: Box::new(tail),
            })
        } else {
            let applied = self.apply_link(value, link)?;
            self.build_chain(applied, rest)
        }
    }

    /// Apply a single chain link to the running `obj` value: a member/index read
    /// reuses the same `Index`/`StrLen` lowering as an ordinary `obj.k`/`obj[i]`/
    /// `obj.length`; a call becomes an indirect call with `obj` as the callee.
    fn apply_link(&mut self, obj: IrNode, link: &ChainLink<'_>) -> anyhow::Result<IrNode> {
        match link {
            ChainLink::Member { prop, span, .. } => match prop {
                MemberProp::Ident(id) if id.sym.as_ref() == "length" => {
                    Ok(IrNode::StrLen(Box::new(nullish_check(obj, "length"))))
                }
                MemberProp::Ident(id) => Ok(IrNode::Index {
                    obj: Box::new(obj),
                    index: Box::new(IrNode::Str(id.sym.as_ref().to_owned())),
                }),
                MemberProp::Computed(c) => Ok(IrNode::Index {
                    obj: Box::new(obj),
                    index: Box::new(self.lower_expr(&c.expr)?),
                }),
                MemberProp::PrivateName(_) => {
                    Err(CompileError::spanned(*span, "private fields are not supported").into())
                }
            },
            ChainLink::Call { args, .. } => {
                let mut lowered = Vec::with_capacity(args.len());
                for arg in *args {
                    if arg.spread.is_some() {
                        return Err(CompileError::spanned(
                            arg.span(),
                            "spread arguments are unsupported",
                        )
                        .into());
                    }
                    lowered.push(self.lower_expr(&arg.expr)?);
                }
                Ok(IrNode::CallIndirect {
                    callee: Box::new(obj),
                    args: lowered,
                })
            }
        }
    }

    /// Lower a bare identifier read.
    ///
    /// A declared local shadows a function of the same name and the global
    /// pseudo-bindings; a function name in value position becomes a first-class
    /// function value; the bare `undefined`/`NaN`/`Infinity` globals lower to
    /// their canonical literals (`NaN`/`Infinity` are read-only global number
    /// bindings, sharing the computed-`0/0` value path). A captured binding
    /// reads through its boxed cell.
    pub(super) fn lower_ident(&mut self, name: &str, span: Span) -> anyhow::Result<IrNode> {
        if let Some(resolved) = self.resolve(name) {
            Ok(Self::read_of(resolved.kind))
        } else if let Some(sig) = self.shared.funcs.get(name) {
            Ok(IrNode::FuncValue {
                table_index: sig.index - 1,
                captures: Vec::new(),
            })
        } else {
            match name {
                "undefined" => Ok(IrNode::Undefined),
                "NaN" => Ok(IrNode::Num(f64::NAN)),
                "Infinity" => Ok(IrNode::Num(f64::INFINITY)),
                _ => {
                    Err(CompileError::spanned(span, format!("undeclared variable: {name}")).into())
                }
            }
        }
    }

    /// Read a reserved member of a class binding by name — an [`IrNode::Index`]
    /// over the class value for `@@constructor` / `@@prototype`. Shared by the
    /// default-constructor alias, prototype chaining, and `super` calls (E-series).
    pub(super) fn class_member_read(
        &mut self,
        class_name: &str,
        span: Span,
        key: &str,
    ) -> anyhow::Result<IrNode> {
        Ok(IrNode::Index {
            obj: Box::new(self.lower_ident(class_name, span)?),
            index: Box::new(IrNode::Str(key.to_owned())),
        })
    }

    /// Lower `super(args)` (E-series): a call to the parent constructor that keeps
    /// the CURRENT `this` (an [`IrNode::ThisCall`]), so the parent constructor's
    /// `this.<field> = ..` mutates the same instance the subclass is building.
    fn lower_super_ctor_call(
        &mut self,
        call: &swc_ecma_ast::CallExpr,
        span: Span,
    ) -> anyhow::Result<IrNode> {
        let Some(parent) = self.current_super.clone() else {
            return Err(CompileError::spanned(
                span,
                "`super(..)` is only valid inside a derived class constructor",
            )
            .into());
        };
        let Some(this_local) = self.ensure_this_local() else {
            return Err(CompileError::spanned(span, "`super` is not available here").into());
        };
        let args = self.lower_super_args(call)?;
        let callee = self.class_member_read(&parent, span, crate::abi::CLASS_CONSTRUCTOR_KEY)?;
        Ok(IrNode::ThisCall {
            callee: Box::new(callee),
            args,
            this_local,
        })
    }

    /// Lower `super.m(args)` (E-series): a call to the parent prototype's method `m`
    /// with the CURRENT `this`. `m` is resolved through `Parent["@@prototype"]`, so
    /// it walks up the parent's own chain (an [`IrNode::ThisCall`]).
    fn lower_super_method_call(
        &mut self,
        call: &swc_ecma_ast::CallExpr,
        super_prop: &SuperPropExpr,
    ) -> anyhow::Result<IrNode> {
        let Some(parent) = self.current_super.clone() else {
            return Err(CompileError::spanned(
                super_prop.span,
                "`super.<m>` is only valid inside a derived class method",
            )
            .into());
        };
        let Some(this_local) = self.ensure_this_local() else {
            return Err(
                CompileError::spanned(super_prop.span, "`super` is not available here").into(),
            );
        };
        let SuperProp::Ident(id) = &super_prop.prop else {
            return Err(CompileError::spanned(
                super_prop.span,
                "computed `super[..]` is unsupported",
            )
            .into());
        };
        let args = self.lower_super_args(call)?;
        let parent_proto =
            self.class_member_read(&parent, super_prop.span, crate::abi::CLASS_PROTOTYPE_KEY)?;
        let callee = IrNode::Index {
            obj: Box::new(parent_proto),
            index: Box::new(IrNode::Str(id.sym.as_ref().to_owned())),
        };
        Ok(IrNode::ThisCall {
            callee: Box::new(callee),
            args,
            this_local,
        })
    }

    /// Lower the argument list of a `super(..)` / `super.m(..)` call (spread is out
    /// of scope, matching the other class constructor/method call paths).
    fn lower_super_args(&mut self, call: &swc_ecma_ast::CallExpr) -> anyhow::Result<Vec<IrNode>> {
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            if arg.spread.is_some() {
                return Err(
                    CompileError::spanned(arg.span(), "spread arguments are unsupported").into(),
                );
            }
            args.push(self.lower_expr(&arg.expr)?);
        }
        Ok(args)
    }

    /// Lower a call expression `callee(args)`. A call to a bare **function name**
    /// keeps the direct-`call` optimization (`IrNode::Call`, arity-checked); a
    /// call to a **variable/parameter** holding a function value, or to any other
    /// computed callee, is an indirect call (`IrNode::CallIndirect`) whose arity
    /// is not checked (the uniform ABI pads/ignores). Spread arguments are out of
    /// scope.
    #[allow(clippy::too_many_lines)]
    fn lower_call(&mut self, call: &swc_ecma_ast::CallExpr) -> anyhow::Result<IrNode> {
        // `super(..)` — the parent constructor call inside a derived class (E-series).
        if let Callee::Super(sup) = &call.callee {
            return self.lower_super_ctor_call(call, sup.span);
        }
        let Callee::Expr(callee_expr) = &call.callee else {
            return Err(CompileError::spanned(call.span(), "unsupported call target").into());
        };
        // `super.m(..)` — a parent prototype method call inside a derived class.
        if let Expr::SuperProp(super_prop) = callee_expr.as_ref() {
            return self.lower_super_method_call(call, super_prop);
        }
        // A spread argument `f(...a)` makes the call's argument count only
        // known at runtime, so it takes a dedicated lowering path.
        if call.args.iter().any(|arg| arg.spread.is_some()) {
            return self.lower_spread_call(call, callee_expr);
        }
        // Arguments lower the same way for direct and indirect calls.
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.lower_expr(&arg.expr)?);
        }
        // `console.log(..)` is a host print, not a user-function call. It
        // is recognized by its `console.log` member callee (a `console` identifier
        // that resolves to no binding) and lowered to `IrNode::ConsoleLog` so the
        // backend emits calls to the host `print` import.
        if is_console_log(callee_expr) && self.resolve("console").is_none() {
            return Ok(IrNode::ConsoleLog { args });
        }
        // `Math.<name>(..)` is a built-in method, not a user-function call.
        // Like `console`, `Math` is not a real binding: recognize it syntactically
        // (only when nothing named `Math` shadows it) and lower to an `IrNode::MathCall`
        // whose arity is validated here.
        if let Some(name) = math_method_name(callee_expr)
            && self.resolve("Math").is_none()
        {
            return lower_math_call(name, args, callee_expr.span());
        }
        // `array.push(x)` / `array.pop()` are growing array methods,
        // dispatched syntactically by method name (kept distinct from `x[i]`
        // element indexing). The receiver must be a simple bound array variable so
        // `push` can rebind it to the reallocated block.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && matches!(id.sym.as_ref(), "push" | "pop")
        {
            return self.lower_array_method(id.sym.as_ref(), &member.obj, args);
        }
        // `array.map/filter/reduce/forEach(callback[, init])` are higher-order array
        // methods, dispatched syntactically by method name (kept distinct
        // from `x[i]` element indexing). Unlike push/pop the receiver is any array
        // expression (a literal or a binding); it is evaluated once and not rebound.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && matches!(id.sym.as_ref(), "map" | "filter" | "reduce" | "forEach")
        {
            return self.lower_array_hof(id.sym.as_ref(), &member.obj, args);
        }
        // `array/string.indexOf/includes/join/slice/toUpperCase/toLowerCase/charAt(..)`
        // are non-callback array/string methods, dispatched syntactically by
        // method name (kept distinct from `x[i]` element indexing). The receiver is
        // any array/string expression, evaluated once; the whole operation runs
        // host-side, branching on the receiver's runtime TYPE BYTE for the
        // `indexOf`/`slice` methods that exist on both arrays and strings.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && let Some(op) = non_callback_method_op(id.sym.as_ref())
        {
            return self.lower_method_call(op, id.sym.as_ref(), &member.obj, args);
        }
        // `Object.keys/values/entries(o)` are collection built-ins, not user-method
        // calls. Like `Math`, `Object` is not a real binding: recognize it
        // syntactically (only when nothing named `Object` shadows it) and lower to
        // an `IrNode::ObjectBuiltin` whose method name and arity are validated here.
        if let Some(name) = object_method_name(callee_expr)
            && self.resolve("Object").is_none()
        {
            return lower_object_call(name, args, callee_expr.span());
        }
        // `JSON.stringify(v)` / `JSON.parse(s)` are collection built-ins, not
        // user-method calls. Like `Object`, `JSON` is not a real binding:
        // recognize it syntactically (only when nothing named `JSON` shadows it)
        // and lower to an `IrNode::JsonStringify` (or reject `parse`/unknown names).
        if let Some(name) = json_method_name(callee_expr)
            && self.resolve("JSON").is_none()
        {
            return lower_json_call(name, args, callee_expr.span());
        }
        // `Promise.resolve(v)` / `Promise.reject(v)` are the promise-creation
        // statics. Like `Object`/`JSON`, `Promise` is not a real binding:
        // recognize it syntactically (only when nothing named `Promise` shadows
        // it) and lower to the corresponding promise node.
        if let Some(name) = promise_static_method_name(callee_expr)
            && self.resolve("Promise").is_none()
        {
            return lower_promise_static(name, args, callee_expr.span());
        }
        // `Date.now()`: the host wall clock as a number. Like the other
        // statics, `Date` is not a real binding; recognize `Date.now` syntactically
        // (only when nothing named `Date` shadows it) and reject any argument.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let Expr::Ident(obj) = member.obj.as_ref()
            && obj.sym.as_ref() == "Date"
            && let MemberProp::Ident(id) = &member.prop
            && id.sym.as_ref() == "now"
            && self.resolve("Date").is_none()
        {
            if !args.is_empty() {
                return Err(CompileError::spanned(
                    callee_expr.span(),
                    "Date.now() takes no arguments",
                )
                .into());
            }
            return Ok(IrNode::DateNow);
        }
        // `d.getTime()`: the millisecond timestamp of a Date receiver.
        // Claimed for ANY receiver (a non-Date yields `NaN`); a user method named
        // `getTime` is a documented out-of-scope collision.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && id.sym.as_ref() == "getTime"
        {
            if !args.is_empty() {
                return Err(
                    CompileError::spanned(member.span(), "getTime() takes no arguments").into(),
                );
            }
            let recv = Box::new(nullish_check(self.lower_expr(&member.obj)?, "getTime"));
            return Ok(IrNode::DateGetTime { recv });
        }
        // The `Date` field methods `d.valueOf()` / `d.getFullYear()` / ….
        // Like the Map/Set methods, these names can ALSO be ordinary user methods,
        // so the lowered node dispatches at runtime on the receiver's TYPE BYTE and
        // keeps a user-method fallback — safe to claim the name here.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && let Some(op) = date_method_op(id.sym.as_ref())
        {
            return self.lower_date_method(op, id.sym.as_ref(), &member.obj, args);
        }
        // `p.then(onF, onR)` / `p.catch(onR)` / `p.finally(fn)` and `gen.next(v)`
        // are the promise / generator instance methods. Like the Map/Set methods,
        // these names can ALSO be ordinary user methods (thenables, iterator
        // objects), so the lowered node dispatches at runtime on the receiver's
        // TYPE BYTE and keeps a user-method fallback — safe to claim the name here.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && let Some(op) = receiver_method_op(id.sym.as_ref())
        {
            return self.lower_receiver_method(op, &member.obj, args);
        }
        // `map.set/get/has/delete(..)` / `set.add/has/delete(..)` are Map/Set
        // methods. Unlike the builtins above these names can ALSO be ordinary
        // object/class methods, so the lowered node keeps a user-method-call fallback
        // and dispatches at runtime on the receiver's TYPE BYTE — it is safe to claim
        // the name here.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && let Some(op) = map_set_method_op(id.sym.as_ref())
        {
            return self.lower_map_method(op, id.sym.as_ref(), &member.obj, args);
        }
        // Any remaining member callee `obj.m(args)` / `obj[k](args)` is a USER
        // method call: it reads the function value at the member and binds
        // `this = obj` for the call. This must come after the builtin-method
        // checks above (which return early for the recognized names) so only
        // genuine object methods reach here.
        if let Expr::Member(member) = callee_expr.as_ref() {
            return self.lower_method_call_user(member, args);
        }
        let Expr::Ident(ident) = callee_expr.as_ref() else {
            // A computed callee (e.g. a returned function value) is always an
            // indirect call through the funcref table.
            return Ok(IrNode::CallIndirect {
                callee: Box::new(self.lower_expr(callee_expr)?),
                args,
            });
        };
        let name = ident.sym.as_ref();
        // A variable/parameter/captured binding holding a function value is called
        // indirectly (its callee is not statically known); a bare function name
        // keeps the direct `call` optimization.
        if let Some(resolved) = self.resolve(name) {
            Ok(IrNode::CallIndirect {
                callee: Box::new(Self::read_of(resolved.kind)),
                args,
            })
        } else if let Some(sig) = self.shared.funcs.get(name) {
            // JS arity semantics: the uniform ABI pads missing arguments with
            // `undefined` (so defaults apply), and surplus arguments are evaluated
            // for their side effects and ignored (or gathered by a rest parameter).
            Ok(IrNode::Call {
                func: sig.index,
                args,
            })
        } else if let Some(op) = convert_builtin_op(name) {
            // A conversion built-in `String/Number/Boolean/parseInt/parseFloat`.
            // Reached only when the name is NOT a binding or a user
            // function (checked above), so a user `let String = ..` / `function
            // String(){}` shadows it. Arity is validated here.
            lower_convert_call(op, name, args, ident.span())
        } else {
            Err(
                CompileError::spanned(ident.span(), format!("call to undeclared function: {name}"))
                    .into(),
            )
        }
    }

    /// Lower a promise / generator instance method `recv.<op>(args)` into an
    /// [`IrNode::ReceiverMethod`]. Mirrors [`Self::lower_map_method`]: the receiver
    /// is lowered once and two fresh temps are reserved for the runtime TYPE-BYTE
    /// dispatch. No arity check: the argument list is threaded verbatim to the
    /// user-method fallback, which may take any number of arguments.
    fn lower_receiver_method(
        &mut self,
        op: ReceiverOp,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let recv = self.lower_expr(obj)?;
        let recv_local = self.fresh();
        let fallback_obj_local = self.fresh();
        Ok(IrNode::ReceiverMethod {
            op,
            recv: Box::new(recv),
            args,
            recv_local,
            fallback_obj_local,
        })
    }

    /// Lower a call whose argument list contains a spread `f(...a)`. The
    /// argument parts are materialized into a runtime array (an
    /// [`IrNode::ArraySpread`]); the call then spreads that array into the
    /// uniform ABI's fixed `K` positional slots plus the dynamic `argc`.
    ///
    /// Only the variadic `Math.max`/`Math.min` built-ins and ordinary
    /// direct/indirect function calls accept a spread. Every other call form
    /// (`console.log`, the `String`/`Number`/… conversions, and the array/string
    /// methods) is out of scope and rejected with a span.
    ///
    /// # Errors
    ///
    /// Returns an error for a spread into an unsupported call target, or if the
    /// callee / any argument expression is outside the supported subset.
    fn lower_spread_call(
        &mut self,
        call: &swc_ecma_ast::CallExpr,
        callee_expr: &Expr,
    ) -> anyhow::Result<IrNode> {
        // Build the spread argument parts left-to-right.
        let mut parts = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            let lowered = self.lower_expr(&arg.expr)?;
            parts.push(if arg.spread.is_some() {
                SpreadElem::Spread(lowered)
            } else {
                SpreadElem::Item(lowered)
            });
        }
        let args = Box::new(IrNode::ArraySpread(parts));

        // `Math.max(...)` / `Math.min(...)` are variadic: fold the spread
        // elements. Any other `Math.<name>` has a fixed arity, so a spread into
        // it is out of scope.
        if let Some(name) = math_method_name(callee_expr)
            && self.resolve("Math").is_none()
        {
            let op = match name {
                "max" => MathOp::Max,
                "min" => MathOp::Min,
                _ => {
                    return Err(CompileError::spanned(
                        call.span(),
                        format!("spread arguments are unsupported for Math.{name}"),
                    )
                    .into());
                }
            };
            return Ok(IrNode::MathSpread { op, args });
        }

        let Expr::Ident(ident) = callee_expr else {
            // The member-callee built-ins (`console.log`, array/string methods)
            // do not accept a spread; a genuine computed callee is an indirect
            // spread call through the funcref table.
            if let Expr::Member(_) = callee_expr {
                return Err(CompileError::spanned(
                    call.span(),
                    "spread arguments are unsupported for this call target",
                )
                .into());
            }
            return Ok(IrNode::CallSpread {
                target: SpreadCallee::Indirect(Box::new(self.lower_expr(callee_expr)?)),
                args,
            });
        };
        let name = ident.sym.as_ref();
        if let Some(resolved) = self.resolve(name) {
            // A binding holding a function value: an indirect spread call.
            Ok(IrNode::CallSpread {
                target: SpreadCallee::Indirect(Box::new(Self::read_of(resolved.kind))),
                args,
            })
        } else if let Some(sig) = self.shared.funcs.get(name) {
            // A known declaration: a direct spread call. The runtime argument
            // count is unchecked — the uniform ABI pads/truncates to `K` and
            // passes the real count as `argc`.
            Ok(IrNode::CallSpread {
                target: SpreadCallee::Direct(sig.index),
                args,
            })
        } else if convert_builtin_op(name).is_some() {
            Err(CompileError::spanned(
                ident.span(),
                format!("spread arguments are unsupported for {name}"),
            )
            .into())
        } else {
            Err(
                CompileError::spanned(ident.span(), format!("call to undeclared function: {name}"))
                    .into(),
            )
        }
    }

    /// Lower a growing array method call `array.push(x)` / `array.pop()`.
    /// The receiver must be a simple identifier bound to a **plain local** array so
    /// `push` can rebind it (`local.set`) to the reallocated block. A temp /
    /// expression receiver, or a captured (boxed-cell) array, is rejected —
    /// captured arrays do not round-trip through the runtime index/length path,
    /// so growing one is out of scope. Arities are validated here (`push` takes
    /// one argument, `pop` none).
    fn lower_array_method(
        &mut self,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let Expr::Ident(obj_id) = obj else {
            return Err(CompileError::spanned(
                obj.span(),
                format!("array.{method} requires a bound array variable receiver"),
            )
            .into());
        };
        let name = obj_id.sym.as_ref();
        let Some(resolved) = self.resolve(name) else {
            return Err(CompileError::spanned(
                obj_id.span(),
                format!("array.{method} on undeclared variable: {name}"),
            )
            .into());
        };
        let AccessKind::Plain(local) = resolved.kind else {
            return Err(CompileError::spanned(
                obj_id.span(),
                format!("array.{method} on a captured array is unsupported"),
            )
            .into());
        };
        match method {
            "push" => {
                let [value] = <[IrNode; 1]>::try_from(args).map_err(|args| {
                    CompileError::spanned(
                        obj.span(),
                        format!("push expects 1 argument, got {}", args.len()),
                    )
                })?;
                Ok(IrNode::ArrayPush {
                    local,
                    value: Box::new(value),
                })
            }
            "pop" => {
                if !args.is_empty() {
                    return Err(CompileError::spanned(
                        obj.span(),
                        format!("pop expects 0 arguments, got {}", args.len()),
                    )
                    .into());
                }
                Ok(IrNode::ArrayPop { local })
            }
            _ => unreachable!("caller restricts the method to push/pop"),
        }
    }

    /// Lower a higher-order array method call `array.map/filter/reduce/forEach(...)`.
    /// The receiver is lowered as a plain expression (evaluated once into a
    /// scratch base at emit time — no rebinding), and the callback is a function
    /// value invoked per element via the `call_indirect` ABI. `map`, `filter`,
    /// and `forEach` take exactly one argument (the callback); `reduce` requires two
    /// (the callback and the initial accumulator — a no-init `reduce` is rejected
    /// rather than defaulting to the first element, out of scope). Arities are validated here.
    fn lower_array_hof(
        &mut self,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let array = Box::new(nullish_check(self.lower_expr(obj)?, method));
        match method {
            "reduce" => {
                let [callback, init] = <[IrNode; 2]>::try_from(args).map_err(|args| {
                    CompileError::spanned(
                        obj.span(),
                        format!(
                            "reduce expects 2 arguments (callback, init), got {}",
                            args.len()
                        ),
                    )
                })?;
                Ok(IrNode::ArrayHof {
                    op: ArrayHofOp::Reduce,
                    array,
                    callback: Box::new(callback),
                    init: Some(Box::new(init)),
                })
            }
            "map" | "filter" | "forEach" => {
                let [callback] = <[IrNode; 1]>::try_from(args).map_err(|args| {
                    CompileError::spanned(
                        obj.span(),
                        format!("{method} expects 1 argument, got {}", args.len()),
                    )
                })?;
                let op = match method {
                    "map" => ArrayHofOp::Map,
                    "filter" => ArrayHofOp::Filter,
                    "forEach" => ArrayHofOp::ForEach,
                    _ => unreachable!("outer match restricts the method"),
                };
                Ok(IrNode::ArrayHof {
                    op,
                    array,
                    callback: Box::new(callback),
                    init: None,
                })
            }
            _ => unreachable!("caller restricts the method to map/filter/reduce/forEach"),
        }
    }

    /// Lower a non-callback array/string method call
    /// `recv.indexOf/includes/join/slice/toUpperCase/toLowerCase/charAt(args)`.
    /// The receiver is lowered as a plain expression (evaluated once,
    /// host-side); arities are validated here and the result is an
    /// [`IrNode::MethodCall`] whose emit pads to two fixed argument slots. The
    /// receiver's array-vs-string kind is unknown at compile time — the host
    /// `__method` handler branches on its runtime TYPE BYTE for the overloaded
    /// `indexOf`/`slice`.
    fn lower_method_call(
        &mut self,
        op: MethodOp,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        // Per-method argument arity. `join`/`slice` have optional trailing args
        // (defaulted host-side); the rest are fixed. At most two slots are ever
        // passed to `__method`.
        let ok = match op {
            MethodOp::IndexOf
            | MethodOp::Includes
            | MethodOp::CharAt
            | MethodOp::Repeat
            | MethodOp::StartsWith
            | MethodOp::EndsWith => args.len() == 1,
            MethodOp::ToUpper
            | MethodOp::ToLower
            | MethodOp::Trim
            | MethodOp::TrimStart
            | MethodOp::TrimEnd => args.is_empty(),
            MethodOp::Join | MethodOp::Split => args.len() <= 1,
            MethodOp::Slice | MethodOp::PadStart | MethodOp::PadEnd | MethodOp::Substring => {
                args.len() <= 2
            }
        };
        if !ok {
            return Err(CompileError::spanned(
                obj.span(),
                format!(
                    "{method} received an unsupported number of arguments: {}",
                    args.len()
                ),
            )
            .into());
        }
        Ok(IrNode::MethodCall {
            op,
            recv: Box::new(nullish_check(self.lower_expr(obj)?, method)),
            args,
        })
    }

    /// Lower a `Map`/`Set` method call `recv.<op>(args)` into an
    /// [`IrNode::MapMethod`]. The receiver is lowered once; two fresh temps
    /// are allocated — one holding the receiver across the runtime TYPE-BYTE dispatch,
    /// one for the user-method-call fallback's own receiver stash. No arity check is
    /// enforced: the argument list is threaded verbatim to the fallback (which may be
    /// a user method of any arity), and the collection path treats a missing key /
    /// value argument as `undefined`, matching JS.
    fn lower_map_method(
        &mut self,
        op: MapOp,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let recv = nullish_check(self.lower_expr(obj)?, method);
        let recv_local = self.fresh();
        let fallback_obj_local = self.fresh();
        Ok(IrNode::MapMethod {
            op,
            recv: Box::new(recv),
            args,
            method_name: method.to_owned(),
            recv_local,
            fallback_obj_local,
        })
    }

    /// Lower a `Date` field method `recv.<method>(args)` into an
    /// [`IrNode::DateMethod`]. Mirrors [`Self::lower_map_method`]: the receiver is
    /// lowered once and two fresh temps are reserved for the runtime TYPE-BYTE
    /// dispatch (the `TYPE_DATE` compute path and the user-method fallback).
    fn lower_date_method(
        &mut self,
        op: DateOp,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let recv = nullish_check(self.lower_expr(obj)?, method);
        let recv_local = self.fresh();
        let fallback_obj_local = self.fresh();
        Ok(IrNode::DateMethod {
            op,
            recv: Box::new(recv),
            args,
            method_name: method.to_owned(),
            recv_local,
            fallback_obj_local,
        })
    }

    /// The complete own-key set of a method-call receiver whose object shape is
    /// statically known: a direct object literal, or an identifier bound to a
    /// `let`/`const` object literal that is never reassigned (recorded in
    /// [`Shared::object_shapes`]). `None` when the shape is not statically known,
    /// so the caller falls back to runtime dispatch.
    fn receiver_object_shape(&self, recv: &Expr) -> Option<Vec<String>> {
        match strip_parens(recv) {
            literal @ Expr::Object(_) => object_literal_keys(literal),
            Expr::Ident(ident) => {
                let id = self.binding_id_of(ident.sym.as_ref())?;
                self.shared.object_shapes.get(&id).cloned()
            }
            _ => None,
        }
    }

    /// Lower a user method call `obj.m(args)` / `obj[k](args)` into an
    /// [`IrNode::MethodCallUser`] binding `this = obj`. The receiver is
    /// evaluated once into a fresh temp; the callee is the function value at the
    /// member `key`; the `__this` global is set to the receiver just before the
    /// indirect call. A private-name member (`obj.#m`) is unsupported.
    fn lower_method_call_user(
        &mut self,
        member: &swc_ecma_ast::MemberExpr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        // The statically-known method name, if any: `obj.m` and `obj["m"]` name
        // `m`; a dynamic computed key `obj[k]` is unknown until runtime.
        let name = member_method_name(member);
        // Reaching this generic path means the name matched none of the
        // built-in string/array/Map/Set methods. A receiver whose type is a
        // statically-known PRIMITIVE (string/number/boolean literal) or an
        // ARRAY literal can never carry a user-defined method, so the call is
        // provably unknown — reject it with a span diagnostic instead of emitting
        // a `call_indirect` the wasm validator (or runtime) would reject.
        if static_primitive_receiver(&member.obj) {
            return Err(unknown_method_error(member.span(), name.as_deref()));
        }
        // Static-shape guard: when the receiver's complete own-key set is known
        // (a direct object literal, or a `let`/`const` binding initialized with
        // one and never reassigned) and the statically-known method name is not
        // among those keys, the call is provably unknown — the object model has no
        // dynamic key growth, so `obj.<name>` reads `undefined` and dispatches it
        // as a bogus funcref index. Reject it with a span diagnostic instead.
        if let Some(method) = name.as_deref()
            && let Some(keys) = self.receiver_object_shape(&member.obj)
            && !keys.iter().any(|k| k == method)
        {
            return Err(unknown_method_error(member.span(), name.as_deref()));
        }
        let key = match &member.prop {
            MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
            MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
            MemberProp::PrivateName(_) => {
                return Err(CompileError::spanned(
                    member.span(),
                    "private fields are not supported",
                )
                .into());
            }
        };
        let obj = self.lower_expr(&member.obj)?;
        let obj_local = self.fresh();
        Ok(IrNode::MethodCallUser {
            obj: Box::new(obj),
            key: Box::new(key),
            args,
            obj_local,
        })
    }

    /// Lower an array literal `[e0, e1, ..]` into an [`IrNode::Array`], evaluating
    /// each element left-to-right. Holes (elisions) and spread elements are out of
    /// scope (later stages).
    ///
    /// # Errors
    ///
    /// Returns an error on an array hole or a spread element, or if any element
    /// expression is outside the supported subset.
    /// Lower an assignment `target = value`. An identifier target is a local
    /// assign; a computed-member target `obj[i] = v` is an in-place array element
    /// store. A static-member target `obj.a = v` is object territory,
    /// still unsupported here.
    #[allow(clippy::too_many_lines)]
    fn lower_assign(&mut self, assign: &AssignExpr) -> anyhow::Result<IrNode> {
        // `x **= e` desugars to `x = x ** e` — a `MathOp::Pow` (not a `BinOp`), so
        // it is handled ahead of the `BinOp`-based compound path below. Only an
        // identifier target is supported.
        if matches!(assign.op, AssignOp::ExpAssign) {
            let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &assign.left else {
                return Err(CompileError::spanned(
                    assign.left.span(),
                    "unsupported `**=` assignment target",
                )
                .into());
            };
            let name = ident.id.sym.as_ref();
            let Some(resolved) = self.resolve(name) else {
                return Err(CompileError::spanned(
                    ident.span(),
                    format!("undeclared variable: {name}"),
                )
                .into());
            };
            if !resolved.mutable {
                return Err(CompileError::spanned(
                    ident.span(),
                    format!("cannot assign to constant binding: {name}"),
                )
                .into());
            }
            let rhs = self.lower_expr(&assign.right)?;
            let value = IrNode::MathCall {
                op: MathOp::Pow,
                args: vec![Self::read_of(resolved.kind.clone()), rhs],
            };
            return Ok(Self::write_of(resolved.kind, value));
        }
        // `x ??= e` / `x ||= e` / `x &&= e` desugar to a conditional assign that
        // reuses the short-circuit `LogicalOp`: `x = x ?? e` / `x || e` / `x && e`.
        // The logical op evaluates `x` once and only evaluates `e` on the
        // non-short-circuit path, so the RHS is skipped exactly when the assign is
        // (P0.4). Only an identifier target is supported.
        if let Some(logical_op) = map_logical_assign_op(assign.op) {
            let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &assign.left else {
                return Err(CompileError::spanned(
                    assign.left.span(),
                    "unsupported logical-assignment target",
                )
                .into());
            };
            let name = ident.id.sym.as_ref();
            let Some(resolved) = self.resolve(name) else {
                return Err(CompileError::spanned(
                    ident.span(),
                    format!("undeclared variable: {name}"),
                )
                .into());
            };
            if !resolved.mutable {
                return Err(CompileError::spanned(
                    ident.span(),
                    format!("cannot assign to constant binding: {name}"),
                )
                .into());
            }
            let rhs = self.lower_expr(&assign.right)?;
            let value = IrNode::Logical {
                op: logical_op,
                lhs: Box::new(Self::read_of(resolved.kind.clone())),
                rhs: Box::new(rhs),
            };
            return Ok(Self::write_of(resolved.kind, value));
        }
        // A compound operator (`+=`/`-=`/`*=`/`/=`/`%=`) maps to the arithmetic
        // `BinOp` it applies; plain `=` maps to `None`.
        let compound_op = map_assign_op(assign.op, assign.span())?;
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) => {
                let name = ident.id.sym.as_ref();
                let Some(resolved) = self.resolve(name) else {
                    return Err(CompileError::spanned(
                        ident.span(),
                        format!("undeclared variable: {name}"),
                    )
                    .into());
                };
                // Reassigning a `const` binding (plain or compound) is rejected.
                // Attach the target identifier's span so the diagnostic renderer
                // can point a caret at the offending name.
                if !resolved.mutable {
                    return Err(CompileError::spanned(
                        ident.span(),
                        format!("cannot assign to constant binding: {name}"),
                    )
                    .into());
                }
                let rhs = self.lower_expr(&assign.right)?;
                // `x op= e` desugars to `x = x op e`. The target is read once, as
                // a side-effect-free local/cell read, and reuses the binary op — so
                // `+=` shares the `+` string/number overload (string concat when
                // the lhs is a string). A captured target reads/writes its cell.
                let value = match compound_op {
                    Some(op) => IrNode::Binary {
                        op,
                        lhs: Box::new(Self::read_of(resolved.kind.clone())),
                        rhs: Box::new(rhs),
                    },
                    None => rhs,
                };
                Ok(Self::write_of(resolved.kind, value))
            }
            // A member target `obj[i] = v` / `obj.k = v` (plain or compound). The
            // base is evaluated before the key; `obj.k` ≡ `obj["k"]` (a heap-string
            // key), and `.length` is not assignable.
            AssignTarget::Simple(SimpleAssignTarget::Member(m)) => {
                let obj = self.lower_expr(&m.obj)?;
                let index = match &m.prop {
                    MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
                    MemberProp::Ident(id) if id.sym.as_ref() != "length" => {
                        IrNode::Str(id.sym.as_ref().to_owned())
                    }
                    _ => {
                        return Err(CompileError::spanned(
                            m.span(),
                            "unsupported assignment target",
                        )
                        .into());
                    }
                };
                let rhs = self.lower_expr(&assign.right)?;
                match compound_op {
                    // `obj[i] op= e`: read the current member once, apply
                    // `op`, store back. Base and key are stashed in fresh temps so
                    // each side effect runs exactly once.
                    Some(op) => {
                        let obj_local = self.fresh();
                        let idx_local = self.fresh();
                        let new_local = self.fresh();
                        Ok(IrNode::IndexOpSet {
                            obj: Box::new(obj),
                            index: Box::new(index),
                            op,
                            rhs: Box::new(rhs),
                            obj_local,
                            idx_local,
                            new_local,
                            kind: OpSetKind::Compound,
                        })
                    }
                    // `obj[i] = v` reuses the runtime `IndexSet` type-byte dispatch.
                    None => Ok(IrNode::IndexSet {
                        obj: Box::new(obj),
                        index: Box::new(index),
                        value: Box::new(rhs),
                    }),
                }
            }
            _ => Err(CompileError::spanned(
                assign.left.span(),
                "assignment target must be an identifier",
            )
            .into()),
        }
    }

    /// Lower an increment/decrement `++x`/`x++`/`--x`/`x--`. The target is an
    /// assignable identifier or a member `o.p` / `o[k]`; the `.prefix` flag
    /// selects the produced value (new for prefix, old for postfix). A plain local
    /// becomes an [`IrNode::IncDec`]; a captured (boxed-cell) target desugars to a
    /// read-modify-write through its cell; a member target is an
    /// [`IrNode::IndexOpSet`] (see [`Self::lower_member_update`]).
    fn lower_update(&mut self, update: &UpdateExpr) -> anyhow::Result<IrNode> {
        let target = match update.arg.as_ref() {
            Expr::Paren(p) => p.expr.as_ref(),
            other => other,
        };
        if let Expr::Member(member) = target {
            return self.lower_member_update(update, member);
        }
        let Expr::Ident(ident) = target else {
            return Err(CompileError::spanned(
                update.arg.span(),
                "increment/decrement target must be an identifier or member",
            )
            .into());
        };
        let name = ident.sym.as_ref();
        let Some(resolved) = self.resolve(name) else {
            return Err(CompileError::spanned(
                ident.span(),
                format!("undeclared variable: {name}"),
            )
            .into());
        };
        // `++`/`--` on a `const` binding is a reassignment, and is rejected.
        if !resolved.mutable {
            return Err(CompileError::spanned(
                ident.span(),
                format!("cannot assign to constant binding: {name}"),
            )
            .into());
        }
        let inc = matches!(update.op, UpdateOp::PlusPlus);
        match resolved.kind {
            AccessKind::Plain(local) => Ok(IrNode::IncDec {
                local,
                inc,
                prefix: update.prefix,
            }),
            // A captured cell: `x = ToNumber(cell) + delta` (prefix yields new);
            // postfix yields the old value as `new - delta`.
            AccessKind::Cell(ptr) => {
                let delta = if inc { 1.0 } else { -1.0 };
                let new_value = IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::ConvertCall {
                        op: ConvertOp::ToNumber,
                        arg: Box::new(IrNode::CellGet(Box::new(ptr.clone()))),
                    }),
                    rhs: Box::new(IrNode::Num(delta)),
                };
                let store = IrNode::CellSet {
                    cell: Box::new(ptr),
                    value: Box::new(new_value),
                };
                if update.prefix {
                    Ok(store)
                } else {
                    Ok(IrNode::Binary {
                        op: BinOp::Sub,
                        lhs: Box::new(store),
                        rhs: Box::new(IrNode::Num(delta)),
                    })
                }
            }
        }
    }

    /// Lower a member update `o.p++` / `--o[k]` to an [`IrNode::IndexOpSet`] that
    /// adds `1` / `-1` to the current value converted with `ToNumber` (so a string
    /// member `"4"` becomes `5`, never `"41"`). The base and key are evaluated
    /// once; a postfix update yields the old numeric value. A nullish base throws
    /// the member READ `TypeError`, as in V8.
    fn lower_member_update(
        &mut self,
        update: &UpdateExpr,
        member: &swc_ecma_ast::MemberExpr,
    ) -> anyhow::Result<IrNode> {
        let obj = self.lower_expr(&member.obj)?;
        let index = match &member.prop {
            MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
            MemberProp::Ident(id) if id.sym.as_ref() != "length" => {
                IrNode::Str(id.sym.as_ref().to_owned())
            }
            _ => {
                return Err(CompileError::spanned(
                    member.span(),
                    "unsupported increment/decrement target",
                )
                .into());
            }
        };
        let delta = if matches!(update.op, UpdateOp::PlusPlus) {
            1.0
        } else {
            -1.0
        };
        let obj_local = self.fresh();
        let idx_local = self.fresh();
        let new_local = self.fresh();
        Ok(IrNode::IndexOpSet {
            obj: Box::new(obj),
            index: Box::new(index),
            op: BinOp::Add,
            rhs: Box::new(IrNode::Num(delta)),
            obj_local,
            idx_local,
            new_local,
            kind: if update.prefix {
                OpSetKind::PrefixUpdate
            } else {
                OpSetKind::PostfixUpdate
            },
        })
    }

    fn lower_array(&mut self, arr: &ArrayLit) -> anyhow::Result<IrNode> {
        // A spread element `[...a, b]` makes the array's length only known
        // at runtime, so it lowers to the runtime-built `ArraySpread`; a plain
        // literal keeps the fixed-size `Array` path.
        if arr
            .elems
            .iter()
            .any(|e| e.as_ref().is_some_and(|e| e.spread.is_some()))
        {
            let mut parts = Vec::with_capacity(arr.elems.len());
            for elem in &arr.elems {
                let Some(elem) = elem else {
                    return Err(CompileError::spanned(
                        arr.span(),
                        "array holes (elisions) are unsupported",
                    )
                    .into());
                };
                let lowered = self.lower_expr(&elem.expr)?;
                parts.push(if elem.spread.is_some() {
                    SpreadElem::Spread(lowered)
                } else {
                    SpreadElem::Item(lowered)
                });
            }
            return Ok(IrNode::ArraySpread(parts));
        }
        let mut elems = Vec::with_capacity(arr.elems.len());
        for elem in &arr.elems {
            let Some(elem) = elem else {
                return Err(CompileError::spanned(
                    arr.span(),
                    "array holes (elisions) are unsupported",
                )
                .into());
            };
            elems.push(self.lower_expr(&elem.expr)?);
        }
        Ok(IrNode::Array(elems))
    }

    /// Lower an object literal `{a: e0, "b": e1, m(){..}, n, ..}` into an
    /// [`IrNode::Object`], evaluating each value expression left-to-right. Key-value
    /// entries, shorthand properties (`{n}` ≡ `{n: n}`), and method/shorthand-method
    /// properties (`{m(){..}}` ≡ `{m: function(){..}}`, a first-class function value
    /// carrying `this`) are in scope with a static identifier or string key.
    /// Computed keys, spread, and getter/setter/computed/async/generator methods are
    /// rejected with spans.
    ///
    /// # Errors
    ///
    /// Returns an error on any unsupported property kind or key, or if a value
    /// expression is outside the supported subset.
    fn lower_object(&mut self, obj: &ObjectLit) -> anyhow::Result<IrNode> {
        let mut entries = Vec::with_capacity(obj.props.len());
        for prop in &obj.props {
            let prop = match prop {
                PropOrSpread::Prop(prop) => prop,
                PropOrSpread::Spread(_) => {
                    return Err(CompileError::spanned(
                        prop.span(),
                        "spread object properties are unsupported",
                    )
                    .into());
                }
            };
            match prop.as_ref() {
                Prop::KeyValue(kv) => {
                    let key = prop_name_key(&kv.key)?;
                    entries.push((key, self.lower_expr(&kv.value)?));
                }
                // Shorthand `{n}` ≡ `{n: n}`: the value is the identifier read.
                Prop::Shorthand(ident) => {
                    let key = ident.sym.as_ref().to_owned();
                    let value = self.lower_ident(ident.sym.as_ref(), ident.span)?;
                    entries.push((key, value));
                }
                // A method / shorthand method `{m(){..}}` lifts to a first-class
                // function value (like an arrow) that carries `this`.
                Prop::Method(method) => {
                    reject_unsupported_function(&method.function)?;
                    let key = prop_name_key(&method.key)?;
                    let block = method
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("object method requires a body"))?;
                    let param_pats: Vec<&Pat> =
                        method.function.params.iter().map(|p| &p.pat).collect();
                    let value =
                        self.lower_lifted_function(&param_pats, BodyKind::Block(&block.stmts))?;
                    entries.push((key, value));
                }
                Prop::Getter(_) | Prop::Setter(_) => {
                    return Err(CompileError::spanned(
                        prop.span(),
                        "getter and setter properties are not supported",
                    )
                    .into());
                }
                Prop::Assign(_) => {
                    return Err(
                        CompileError::spanned(prop.span(), "unsupported object property").into(),
                    );
                }
            }
        }
        Ok(IrNode::Object(entries))
    }

    /// Lower a `new <Id>(args)` expression for the three built-in error
    /// constructors `Error` / `TypeError` / `RangeError`. Each lowers to
    /// the SAME IR as the plain object literal
    /// `{ name: "<Id>", message: <arg0> }`, so `.name`/`.message` are ordinary
    /// property reads, `typeof` is "object", and the value throws/propagates
    /// through the existing exception machinery with no new heap layout.
    ///
    /// The `name` is a string literal equal to the constructor identifier. The
    /// `message` is the first argument, defaulting to the empty string `""` when
    /// no argument is supplied (matching JS semantics). Only bare-identifier
    /// callees naming one of the three constructors are in scope; anything else
    /// (member callees, spread args, other constructors) is an error — general
    /// class/constructor support is out of scope.
    ///
    /// # Errors
    ///
    /// Returns an error for any callee other than the three error identifiers,
    /// for spread arguments, or if the message expression is unsupported.
    /// Lower `left instanceof right` (E-series) into an [`IrNode::InstanceOf`].
    /// See [`Self::instanceof_kind`] for the right-hand-side dispatch.
    fn lower_instanceof(&mut self, left: &Expr, right: &Expr) -> anyhow::Result<IrNode> {
        let kind = self.instanceof_kind(right)?;
        Ok(IrNode::InstanceOf {
            value: Box::new(self.lower_expr(left)?),
            kind,
        })
    }

    /// Resolve the right-hand side of an `instanceof` to an [`InstanceOfKind`].
    /// A user class is matched FIRST (so a `class Error {}` shadows the builtin)
    /// and lowers to a `@@prototype` comparison; an UNSHADOWED `Error`/
    /// `TypeError`/`RangeError` identifier lowers to a hidden-brand check.
    /// Anything else — a non-identifier, a shadowed error name, or an unrelated
    /// binding — is a clean compile error.
    fn instanceof_kind(&mut self, right: &Expr) -> anyhow::Result<InstanceOfKind> {
        let unsupported = |span: Span| -> anyhow::Error {
            CompileError::spanned(
                span,
                "instanceof requires a user class or one of \
                 Error/TypeError/RangeError/Array/Object/Function/Date \
                 on the right-hand side",
            )
            .into()
        };
        let Expr::Ident(id) = right else {
            return Err(unsupported(right.span()));
        };
        let name = id.sym.as_ref();
        if self.shared.classes.borrow().contains_key(name) {
            // User class: compare the receiver's `@@prototype` to the class's
            // shared prototype (a single pointer hop — exact under no-`extends`).
            let class_read = self.lower_ident(name, id.span)?;
            let class_proto = IrNode::Index {
                obj: Box::new(class_read),
                index: Box::new(IrNode::Str(crate::abi::CLASS_PROTOTYPE_KEY.to_owned())),
            };
            return Ok(InstanceOfKind::Class(Box::new(class_proto)));
        }
        if self.resolve(name).is_none() {
            match name {
                "Error" => return Ok(InstanceOfKind::Error),
                "TypeError" => return Ok(InstanceOfKind::TypeError),
                "RangeError" => return Ok(InstanceOfKind::RangeError),
                // The runtime-TYPE-BYTE builtins: `Array`/`Object`/`Function`/
                // `Date` are not real bindings, so a user `let Array = ..` shadows
                // them (the `resolve` guard above) and falls through to the error.
                "Array" => return Ok(InstanceOfKind::Array),
                "Function" => return Ok(InstanceOfKind::Function),
                "Date" => return Ok(InstanceOfKind::Date),
                "Object" => return Ok(InstanceOfKind::Object),
                _ => {}
            }
        }
        Err(unsupported(id.span))
    }

    fn lower_new(&mut self, new_expr: &swc_ecma_ast::NewExpr) -> anyhow::Result<IrNode> {
        let Expr::Ident(callee) = new_expr.callee.as_ref() else {
            return Err(CompileError::spanned(
                new_expr.callee.span(),
                "unsupported `new` target: only the Error, TypeError, and RangeError \
                 constructors are supported",
            )
            .into());
        };
        let name = callee.sym.as_ref();
        // A `new A(args)` on a user class declared earlier takes the class
        // instantiation path; the builtin Error constructors fall through below.
        if self.shared.classes.borrow().contains_key(name) {
            return self.lower_new_class(name, callee.span, new_expr);
        }
        // `new Map()` / `new Set()`: a fresh empty collection handle. Like the
        // other builtins, `Map`/`Set` are not real bindings, so a user `let Map = ..`
        // shadows them. Only the empty constructor is in scope — arguments (e.g.
        // `new Map([[k, v]])`) are rejected.
        if matches!(name, "Map" | "Set") && self.resolve(name).is_none() {
            let args = new_expr.args.as_deref().unwrap_or(&[]);
            if !args.is_empty() {
                return Err(CompileError::spanned(
                    callee.span(),
                    format!(
                        "new {name}(..) with constructor arguments is unsupported (only the empty \
                         `new {name}()` is supported)"
                    ),
                )
                .into());
            }
            return Ok(IrNode::MapNew { set: name == "Set" });
        }
        // `new Promise(executor)`: the executor form of the Promise
        // constructor. Like `Map`/`Set`, `Promise` is not a real binding, so a
        // user `let Promise = ..` shadows it.
        if name == "Promise" && self.resolve("Promise").is_none() {
            return self.lower_new_promise(callee.span, new_expr);
        }
        // `new Date()` / `new Date(ms)`. Like the other builtins, `Date`
        // is not a real binding, so a user `let Date = ..` shadows it.
        if name == "Date" && self.resolve("Date").is_none() {
            return self.lower_new_date(callee.span, new_expr);
        }
        if !matches!(name, "Error" | "TypeError" | "RangeError") {
            return Err(CompileError::spanned(
                callee.span(),
                format!("unsupported constructor: {name}"),
            )
            .into());
        }
        let args = new_expr.args.as_deref().unwrap_or(&[]);
        // `message` is arg0 (spread rejected), or absent when no argument is given.
        let message = match args.first() {
            Some(arg) => {
                if arg.spread.is_some() {
                    return Err(CompileError::spanned(
                        arg.span(),
                        "spread arguments are unsupported",
                    )
                    .into());
                }
                Some(self.lower_expr(&arg.expr)?)
            }
            None => None,
        };
        // The hidden identity marker (`@@errorkind`): a number jsval keyed by a
        // reserved `@@`-prefixed key, so it stays invisible to `for-in` /
        // `Object.keys` / `JSON.stringify` yet lets `instanceof` and the value
        // renderer recognize the error kind. The `matches!` guard above proves
        // `name` is one of the three, so the `_` arm is `Error`.
        let kind = match name {
            "TypeError" => crate::abi::ERROR_KIND_TYPE,
            "RangeError" => crate::abi::ERROR_KIND_RANGE,
            _ => crate::abi::ERROR_KIND_ERROR,
        };
        // `stack` is the single-line `"<name>: <message>"` (just `<name>` when no
        // message was given — jsc keeps no runtime call frames). The message
        // sub-expression is teed into a fresh temp so it is evaluated EXACTLY
        // ONCE: a side-effecting `new Error(f())` must not run `f` twice.
        let (message_value, stack) = match message {
            Some(message) => {
                let msg_local = self.fresh();
                let message_value = IrNode::Assign {
                    local: msg_local,
                    value: Box::new(message),
                };
                let stack = IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::Str(format!("{name}: "))),
                    rhs: Box::new(IrNode::Local(msg_local)),
                };
                (message_value, stack)
            }
            None => (IrNode::Str(String::new()), IrNode::Str(name.to_owned())),
        };
        Ok(IrNode::Object(vec![
            ("name".to_owned(), IrNode::Str(name.to_owned())),
            ("message".to_owned(), message_value),
            ("stack".to_owned(), stack),
            (crate::abi::ERROR_KIND_KEY.to_owned(), IrNode::Num(kind)),
        ]))
    }

    /// Lower `new Date()` / `new Date(ms)` into an [`IrNode::DateNew`].
    /// The no-argument form seeds the payload from the host clock (`ms: None`); a
    /// single argument is coerced to a number (`Number(arg)`) so any operand stores
    /// its numeric value. Two-or-more arguments (the calendar-field form) and a
    /// spread argument are rejected — only the timestamp forms are in scope.
    fn lower_new_date(
        &mut self,
        span: Span,
        new_expr: &swc_ecma_ast::NewExpr,
    ) -> anyhow::Result<IrNode> {
        let args = new_expr.args.as_deref().unwrap_or(&[]);
        match args {
            [] => Ok(IrNode::DateNew { ms: None }),
            [arg] => {
                if arg.spread.is_some() {
                    return Err(CompileError::spanned(
                        arg.span(),
                        "spread arguments are unsupported",
                    )
                    .into());
                }
                let ms = IrNode::ConvertCall {
                    op: ConvertOp::ToNumber,
                    arg: Box::new(self.lower_expr(&arg.expr)?),
                };
                Ok(IrNode::DateNew {
                    ms: Some(Box::new(ms)),
                })
            }
            _ => Err(CompileError::spanned(
                span,
                "new Date(..) supports only new Date() or new Date(ms) (the \
                 calendar-field constructor is unsupported)",
            )
            .into()),
        }
    }

    /// Lower `new Promise(executor)` into an [`IrNode::PromiseCtor`]. The
    /// single argument is the executor function (spread / wrong arity rejected); a
    /// dedicated local is reserved to hold the fresh promise across the executor
    /// call so the surrounding scratch cannot clobber it.
    fn lower_new_promise(
        &mut self,
        span: Span,
        new_expr: &swc_ecma_ast::NewExpr,
    ) -> anyhow::Result<IrNode> {
        let args = new_expr.args.as_deref().unwrap_or(&[]);
        if args.len() != 1 {
            return Err(CompileError::spanned(
                span,
                format!(
                    "new Promise expects 1 argument (the executor), got {}",
                    args.len()
                ),
            )
            .into());
        }
        let arg = &args[0];
        if arg.spread.is_some() {
            return Err(
                CompileError::spanned(arg.span(), "spread arguments are unsupported").into(),
            );
        }
        let executor = self.lower_expr(&arg.expr)?;
        let p_local = self.fresh();
        Ok(IrNode::PromiseCtor {
            executor: Box::new(executor),
            p_local,
        })
    }

    /// Lower `new A(args)` on a user class `A` into an [`IrNode::New`]. The
    /// class binding `A` is read (a side-effect-free local/cell read) to reach its
    /// reserved constructor and prototype entries; the fresh instance is an object
    /// pre-slotted with the reserved prototype link (pointing at the class's shared
    /// prototype) and one `undefined` slot per field the constructor assigns, since
    /// the object model has no dynamic key growth. Spread arguments are rejected.
    fn lower_new_class(
        &mut self,
        name: &str,
        span: Span,
        new_expr: &swc_ecma_ast::NewExpr,
    ) -> anyhow::Result<IrNode> {
        let fields = {
            let classes = self.shared.classes.borrow();
            classes
                .get(name)
                .map(|info| info.fields.clone())
                .unwrap_or_default()
        };
        // Read the class binding twice (both are pure local/cell reads): once for
        // the shared prototype the instance links to, once for the constructor.
        let class_read = self.lower_ident(name, span)?;
        let proto = IrNode::Index {
            obj: Box::new(class_read.clone()),
            index: Box::new(IrNode::Str(crate::abi::CLASS_PROTOTYPE_KEY.to_owned())),
        };
        let ctor = IrNode::Index {
            obj: Box::new(class_read),
            index: Box::new(IrNode::Str(crate::abi::CLASS_CONSTRUCTOR_KEY.to_owned())),
        };
        // The fresh instance: the prototype link first, then a slot per field.
        let mut entries = vec![(crate::abi::CLASS_PROTOTYPE_KEY.to_owned(), proto)];
        for field in fields {
            entries.push((field, IrNode::Undefined));
        }
        let instance = IrNode::Object(entries);

        let mut args = Vec::new();
        for arg in new_expr.args.as_deref().unwrap_or(&[]) {
            if arg.spread.is_some() {
                return Err(
                    CompileError::spanned(arg.span(), "spread arguments are unsupported").into(),
                );
            }
            args.push(self.lower_expr(&arg.expr)?);
        }
        let instance_local = self.fresh();
        Ok(IrNode::New {
            instance: Box::new(instance),
            ctor: Box::new(ctor),
            args,
            instance_local,
        })
    }
}

/// One link of a flattened optional chain (see [`LowerCtx::lower_opt_chain`]),
/// borrowed from the swc AST. `optional` marks the `?.` links (the ones that
/// short-circuit); a member link carries its property, a call link its argument
/// list.
enum ChainLink<'a> {
    Member {
        prop: &'a MemberProp,
        optional: bool,
        span: Span,
    },
    Call {
        args: &'a [ExprOrSpread],
        optional: bool,
    },
}

/// Flatten an [`OptChainExpr`] into its root base expression plus the ordered
/// (base-to-outer) list of member/index/call links, recursing through nested
/// `?.` links whose base is itself an optional chain. `chain.optional` marks
/// whether *this* link is a `?.` (short-circuiting) one.
fn collect_chain_links<'a>(chain: &'a OptChainExpr, links: &mut Vec<ChainLink<'a>>) -> &'a Expr {
    let (inner, link) = match chain.base.as_ref() {
        OptChainBase::Member(m) => (
            m.obj.as_ref(),
            ChainLink::Member {
                prop: &m.prop,
                optional: chain.optional,
                span: m.span,
            },
        ),
        OptChainBase::Call(c) => (
            c.callee.as_ref(),
            ChainLink::Call {
                args: &c.args,
                optional: chain.optional,
            },
        ),
    };
    let root = match inner {
        Expr::OptChain(inner_chain) => collect_chain_links(inner_chain, links),
        other => other,
    };
    links.push(link);
    root
}

/// The static string key of an object property name: an identifier or
/// string-literal [`PropName`]. Computed (`[k]`) and numeric keys are rejected
/// with a span — the object model interns compile-time string keys only.
fn prop_name_key(name: &PropName) -> anyhow::Result<String> {
    match name {
        PropName::Ident(id) => Ok(id.sym.as_ref().to_owned()),
        PropName::Str(s) => Ok(s.value.to_string_lossy().into_owned()),
        _ => Err(CompileError::spanned(
            name.span(),
            "unsupported object key: only identifier and string keys are supported \
             (computed and numeric keys are not)",
        )
        .into()),
    }
}

/// Wrap a builtin member access's receiver in an [`IrNode::NullishCheck`] so a
/// `null`/`undefined` receiver throws `Cannot read properties of .. (reading
/// '<key>')` instead of reading through a bogus pointer.
fn nullish_check(value: IrNode, key: &str) -> IrNode {
    IrNode::NullishCheck {
        value: Box::new(value),
        key: key.to_owned(),
    }
}

/// The statically-known method name of a member callee: `obj.m` and the
/// string-literal computed form `obj["m"]` both name `m`; a dynamic computed key
/// `obj[k]` is only known at runtime, so it has no static name.
fn member_method_name(member: &swc_ecma_ast::MemberExpr) -> Option<String> {
    match &member.prop {
        MemberProp::Ident(id) => Some(id.sym.as_ref().to_owned()),
        MemberProp::Computed(c) => match c.expr.as_ref() {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string_lossy().into_owned()),
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    }
}

/// Whether the receiver's type is a statically-known primitive (string / number /
/// boolean / template string) or an array literal. A `null` receiver is not
/// listed: its method lookup throws a runtime `TypeError` instead. Such a receiver
/// can never carry a user-defined method, so a method name that reached the
/// generic user-method path (i.e. matched no built-in) is provably unknown.
fn static_primitive_receiver(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(p) => static_primitive_receiver(&p.expr),
        Expr::Lit(Lit::Str(_) | Lit::Num(_) | Lit::Bool(_)) | Expr::Tpl(_) | Expr::Array(_) => true,
        _ => false,
    }
}

/// A plain-English description of an unsupported expression, used for the
/// user-facing diagnostic message. Never renders the raw swc AST.
fn unsupported_expr_msg(expr: &Expr) -> &'static str {
    match expr {
        Expr::TaggedTpl(_) => "tagged template literals are not supported",
        Expr::This(_) => "`this` is not supported",
        Expr::Yield(_) => "yield expressions are not supported",
        Expr::Class(_) => "class expressions are not supported",
        Expr::Seq(_) => "comma (sequence) expressions are not supported",
        Expr::Lit(Lit::Regex(_)) => "regular expression literals are not supported",
        Expr::Lit(Lit::BigInt(_)) => "BigInt literals are not supported",
        _ => "this expression is not supported",
    }
}

/// Map a method name to its [`ReceiverOp`], or `None` when the name is not a
/// promise / generator instance method. Recognized purely syntactically from a
/// `<expr>.<name>(..)` member callee; the lowered node dispatches on the
/// receiver's runtime TYPE BYTE and falls back to a user method call.
fn receiver_method_op(name: &str) -> Option<ReceiverOp> {
    Some(match name {
        "then" => ReceiverOp::Then,
        "catch" => ReceiverOp::Catch,
        "finally" => ReceiverOp::Finally,
        "next" => ReceiverOp::Next,
        _ => return None,
    })
}
