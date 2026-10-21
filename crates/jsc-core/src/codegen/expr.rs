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
            Expr::Arrow(arrow) => self.lower_arrow(arrow, ""),
            // A function expression `function (..) { .. }` lifts the same way as
            // an arrow, e.g. a closure factory's `return function(){ .. }`.
            Expr::Fn(fn_expr) => self.lower_fn_expr(fn_expr, ""),
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
                    && self.inline_namespace("Math") =>
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
            // `x instanceof RHS` (design 4.6): any right-hand side expression.
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
                        radix: None,
                    }),
                },
                // `typeof operand` inspects the operand's runtime type.
                // `typeof x` of an unresolvable `x` reads `undefined` instead
                // of throwing.
                UnaryOp::TypeOf => Ok(IrNode::TypeOf(Box::new(match self.lower_expr(&u.arg)? {
                    IrNode::GlobalGet { global, .. } if matches!(*u.arg, Expr::Ident(_)) => {
                        IrNode::GlobalGet {
                            global,
                            typeof_operand: true,
                        }
                    }
                    operand => operand,
                }))),
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
            // `static` field initializer `this` is the class itself; inside a
            // derived constructor it is in the TDZ until `super(..)`.
            Expr::This(_) => {
                if let Some((frame, id)) = self.static_init
                    && self.only_arrows_above(frame)
                {
                    return Ok(self.read_hidden(frame, id));
                }
                if let Some(fi) = self.derived_frame() {
                    return Ok(self.derived_this(fi));
                }
                // Only the top-level `main` frame binds no `this`: there it is
                // the global object.
                Ok(match self.ensure_this_local() {
                    Some(local) => IrNode::Local(local),
                    None => IrNode::Intrinsic(GlobalIntrinsic::Global),
                })
            }
            Expr::SuperProp(super_prop) => {
                let read = self.lower_super_prop(super_prop)?;
                Ok(read)
            }
            // A class expression; an anonymous one has the empty name here
            // (`lower_named_expr` names it after its binding).
            Expr::Class(class_expr) => match &class_expr.ident {
                Some(id) => self.lower_class(&class_expr.class, id.sym.as_ref(), true),
                None => self.lower_class(&class_expr.class, "", false),
            },
            // `new.target`: the enclosing non-arrow function's snapshot; outside
            // any function it is a SyntaxError.
            Expr::MetaProp(meta) if meta.kind == swc_ecma_ast::MetaPropKind::NewTarget => {
                // An escaped spelling (`new.t\u0061rget`) is longer than the
                // 10-byte keyword form and is a SyntaxError.
                if meta.span.hi.0 - meta.span.lo.0 != 10 {
                    return Err(CompileError::spanned(
                        meta.span,
                        "keywords in `new.target` cannot contain escapes",
                    )
                    .into());
                }
                self.resolve_new_target().ok_or_else(|| {
                    CompileError::spanned(meta.span, "`new.target` is only valid inside a function")
                        .into()
                })
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
        // `o.m?.(..)`: the callee `o.m` is a plain member expression; split it
        // into its base and a member link so the call binds `this = o`.
        if let (Expr::Member(member), Some(ChainLink::Call { .. })) = (root, links.first()) {
            links.insert(
                0,
                ChainLink::Member {
                    prop: &member.prop,
                    optional: false,
                    span: member.span,
                },
            );
            let base = self.lower_expr(&member.obj)?;
            return self.build_chain(base, &links);
        }
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
            let tail = self.build_link(IrNode::Local(temp), link, rest)?;
            Ok(nullish_short_circuit(temp, value, tail))
        } else {
            self.build_link(value, link, rest)
        }
    }

    /// Apply `link` to `obj` and fold the remaining `rest` of the chain. A member
    /// link directly followed by a call link is a method call binding `this =
    /// obj` (like `obj.m(..)`); an optional call `obj.m?.(..)` evaluates
    /// `obj.m` once and short-circuits the rest of the chain when it is nullish.
    fn build_link(
        &mut self,
        obj: IrNode,
        link: &ChainLink<'_>,
        rest: &[ChainLink<'_>],
    ) -> anyhow::Result<IrNode> {
        let (
            ChainLink::Member { prop, span, .. },
            Some((ChainLink::Call { args, optional }, after)),
        ) = (link, rest.split_first())
        else {
            let applied = self.apply_link(obj, link)?;
            return self.build_chain(applied, rest);
        };
        let args = self.lower_chain_args(args)?;
        if *optional {
            let this_local = self.fresh();
            let method = self.apply_link(
                IrNode::Assign {
                    local: this_local,
                    value: Box::new(obj),
                },
                link,
            )?;
            let callee = self.fresh();
            let call = IrNode::ThisCall {
                callee: Box::new(IrNode::Local(callee)),
                args,
                this_local,
                construct: false,
            };
            let tail = self.build_chain(call, after)?;
            return Ok(nullish_short_circuit(callee, method, tail));
        }
        let key = match prop {
            MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
            MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
            MemberProp::PrivateName(_) => {
                return Err(
                    CompileError::spanned(*span, "private fields are not supported").into(),
                );
            }
        };
        let obj_local = self.fresh();
        let call = IrNode::MethodCallUser {
            obj: Box::new(obj),
            key: Box::new(key),
            args,
            obj_local,
            not_callable: not_a_function(None),
        };
        self.build_chain(call, after)
    }

    /// Lower the arguments of an optional-chain call link (spread is unsupported).
    fn lower_chain_args(&mut self, args: &[ExprOrSpread]) -> anyhow::Result<Vec<IrNode>> {
        let mut lowered = Vec::with_capacity(args.len());
        for arg in args {
            if arg.spread.is_some() {
                return Err(
                    CompileError::spanned(arg.span(), "spread arguments are unsupported").into(),
                );
            }
            lowered.push(self.lower_expr(&arg.expr)?);
        }
        Ok(lowered)
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
            ChainLink::Call { args, .. } => Ok(IrNode::CallIndirect {
                callee: Box::new(obj),
                args: self.lower_chain_args(args)?,
                not_callable: not_a_function(None),
            }),
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
            Ok(IrNode::DeclFunc(sig.index - 1))
        } else {
            match name {
                "undefined" => Ok(IrNode::Undefined),
                "NaN" => Ok(IrNode::Num(f64::NAN)),
                "Infinity" => Ok(IrNode::Num(f64::INFINITY)),
                "arguments" => self
                    .resolve_arguments()
                    .map_err(|msg| CompileError::spanned(span, msg).into()),
                _ => Ok(self.global_read(name)),
            }
        }
    }

    /// Lower `expr` as the value of a binding or property named `name`: an
    /// anonymous function or arrow gets `name` as its `name` (`NamedEvaluation`);
    /// any other expression lowers as usual.
    pub(super) fn lower_named_expr(&mut self, expr: &Expr, name: &str) -> anyhow::Result<IrNode> {
        match expr {
            Expr::Arrow(arrow) => self.lower_arrow(arrow, name),
            Expr::Fn(fn_expr) if fn_expr.ident.is_none() => self.lower_fn_expr(fn_expr, name),
            Expr::Class(class_expr) if class_expr.ident.is_none() => {
                self.lower_class(&class_expr.class, name, false)
            }
            _ => self.lower_expr(expr),
        }
    }

    /// Lower a function expression to a constructor function value named by
    /// its own identifier, else by `name` (its optional identifier is not
    /// bound inside the body).
    fn lower_fn_expr(
        &mut self,
        fn_expr: &swc_ecma_ast::FnExpr,
        name: &str,
    ) -> anyhow::Result<IrNode> {
        let block = fn_expr
            .function
            .body
            .as_ref()
            .ok_or_else(|| anyhow!("function expression requires a body"))?;
        reject_unsupported_function(&fn_expr.function)?;
        let param_pats: Vec<&Pat> = fn_expr.function.params.iter().map(|p| &p.pat).collect();
        let name = fn_expr.ident.as_ref().map_or(name, |id| id.sym.as_ref());
        self.lower_lifted_function(
            &param_pats,
            BodyKind::Block(&block.stmts),
            name,
            FnKind::Constructor,
        )
    }

    /// Lower `super(args)` inside a derived class constructor (design 4.5):
    /// `__construct_with(parent, args, new.target)` — the parent allocates
    /// `this` with `proto = new.target.prototype` (or returns its own object) —
    /// then bind the result as `this` (a `ReferenceError` when already bound)
    /// and run the class's instance fields on it. Yields `this`.
    fn lower_super_ctor_call(
        &mut self,
        call: &swc_ecma_ast::CallExpr,
        span: Span,
    ) -> anyhow::Result<IrNode> {
        let fi = self.derived_frame();
        let scope = self
            .current_super
            .filter(|_| fi.is_some())
            .and_then(|scope| scope.parent.map(|parent| (scope, parent)));
        let (Some(fi), Some((scope, parent))) = (fi, scope) else {
            return Err(CompileError::spanned(
                span,
                "`super(..)` is only valid inside a derived class constructor",
            )
            .into());
        };
        let new_target = self.resolve_new_target().unwrap_or(IrNode::Undefined);
        let callee = self.read_hidden(scope.frame, parent);
        let (args, spread) = self.lower_construct_args(&call.args)?;
        let result = self.fresh();
        let construct = IrNode::Construct {
            callee: Box::new(callee),
            args,
            spread,
            callee_local: self.fresh(),
            this_local: self.fresh(),
            reserve: scope.reserve,
            not_constructor: CalleeError::not_a_constructor("Super constructor"),
            new_target: Some(Box::new(new_target)),
        };
        // The body's top-level `super(..)` statement, when no other call was
        // lowered before it, is the first to run: `this` cannot be bound yet,
        // and is definitely bound from here on.
        let top = fi == self.top();
        let supers = &mut self.frames[fi].supers;
        let top_level = top && supers.top_level == Some(call.span);
        let first = supers.lowered == 0 && top_level;
        supers.lowered += 1;
        let mut seq = vec![IrNode::Assign {
            local: result,
            value: Box::new(construct),
        }];
        if !first {
            let raw = self.derived_this_raw(fi);
            seq.push(guard(
                IrNode::IsHole(Box::new(raw)),
                reference_error("Super constructor may only be called once"),
            ));
        }
        seq.push(self.derived_bind_this(fi, IrNode::Local(result)));
        self.frames[fi].supers.this_bound |= top_level;
        if let Some(fields) = self.lower_field_inits(IrNode::Local(result))? {
            seq.push(fields);
        }
        seq.push(IrNode::Local(result));
        Ok(IrNode::Seq(seq))
    }

    /// The object `super.x` looks `x` up on: the home object's prototype — the
    /// parent's `prototype` for instance members, the parent itself for static
    /// ones — or `None` in a base class (whose home prototype is the not yet
    /// modelled `%Object.prototype%` / `%Function.prototype%`).
    fn super_home(&mut self, span: Span) -> anyhow::Result<Option<IrNode>> {
        let Some(scope) = self.current_super else {
            return Err(CompileError::spanned(span, "`super` is only valid inside a class").into());
        };
        let id = if scope.is_static {
            scope.parent
        } else {
            scope.proto_parent
        };
        Ok(id.map(|id| self.read_hidden(scope.frame, id)))
    }

    /// Lower the key of `super.x` / `super[k]`.
    fn lower_super_key(&mut self, super_prop: &SuperPropExpr) -> anyhow::Result<IrNode> {
        Ok(match &super_prop.prop {
            SuperProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
            SuperProp::Computed(c) => self.lower_expr(&c.expr)?,
        })
    }

    /// Prefix `node` with the read of `this` a `super.x` reference makes first
    /// (TDZ-checked in a derived constructor); returns the local holding it.
    fn super_this(&mut self, node: IrNode, span: Span) -> anyhow::Result<(IrNode, u32)> {
        if let Some(fi) = self.derived_frame() {
            let this = self.derived_this(fi);
            let local = self.fresh();
            let read = IrNode::Assign {
                local,
                value: Box::new(this),
            };
            return Ok((IrNode::Seq(vec![read, node]), local));
        }
        let Some(this_local) = self.ensure_this_local() else {
            return Err(CompileError::spanned(span, "`super` is not available here").into());
        };
        Ok((node, this_local))
    }

    /// Lower a `super.x` / `super[k]` read through the home object (see
    /// [`Self::super_home`]).
    fn lower_super_prop(&mut self, super_prop: &SuperPropExpr) -> anyhow::Result<IrNode> {
        let home = self.super_home(super_prop.span)?;
        let key = self.lower_super_key(super_prop)?;
        let read = match home {
            Some(home) => IrNode::Index {
                obj: Box::new(home),
                index: Box::new(key),
            },
            None => IrNode::Seq(vec![key, IrNode::Undefined]),
        };
        Ok(self.super_this(read, super_prop.span)?.0)
    }

    /// Lower `super.m(args)`: the method found through the home object (see
    /// [`Self::super_home`]), called with the CURRENT `this` (an
    /// [`IrNode::ThisCall`]).
    fn lower_super_method_call(
        &mut self,
        call: &swc_ecma_ast::CallExpr,
        super_prop: &SuperPropExpr,
    ) -> anyhow::Result<IrNode> {
        let home = self.super_home(super_prop.span)?;
        let key = self.lower_super_key(super_prop)?;
        let method = match home {
            Some(home) => IrNode::Index {
                obj: Box::new(home),
                index: Box::new(key),
            },
            None => IrNode::Seq(vec![key, IrNode::Undefined]),
        };
        let (callee, this_local) = self.super_this(method, super_prop.span)?;
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            if arg.spread.is_some() {
                return Err(
                    CompileError::spanned(arg.span(), "spread arguments are unsupported").into(),
                );
            }
            args.push(self.lower_expr(&arg.expr)?);
        }
        Ok(IrNode::ThisCall {
            callee: Box::new(callee),
            args,
            this_local,
            construct: false,
        })
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
        if is_console_log(callee_expr) && self.inline_namespace("console") {
            return Ok(IrNode::ConsoleLog { args });
        }
        // `Math.<name>(..)` is a built-in method, not a user-function call.
        // Like `console`, `Math` is not a real binding: recognize it syntactically
        // (only when nothing named `Math` shadows it) and lower to an `IrNode::MathCall`
        // whose arity is validated here.
        if let Some(name) = math_method_name(callee_expr)
            && self.inline_namespace("Math")
        {
            return lower_math_call(name, args, callee_expr.span());
        }
        // `recv.push(x)` / `recv.pop()` are growing array methods, dispatched
        // syntactically by method name (kept distinct from `x[i]` element
        // indexing). The receiver is any expression, evaluated once.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let MemberProp::Ident(id) = &member.prop
            && matches!(id.sym.as_ref(), "push" | "pop")
        {
            return self.lower_array_method(id.sym.as_ref(), &member.obj, args);
        }
        // `array.map/filter/reduce/forEach(callback[, init])` are higher-order array
        // methods, dispatched syntactically by method name (kept distinct
        // from `x[i]` element indexing). The receiver is any array expression (a
        // literal or a binding), evaluated once.
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
        // `Object.keys/values/entries(o)` keep their inline
        // `IrNode::ObjectBuiltin` (method name and arity validated here) while
        // `Object` is the unshadowed, sealed, non-escaping global (design 3.2);
        // otherwise the call reads the static from the `Object` value.
        if let Some(name) = object_method_name(callee_expr)
            && self.inline_namespace("Object")
        {
            return lower_object_call(name, args, callee_expr.span());
        }
        // `JSON.stringify(v)` / `JSON.parse(s)` keep their inline nodes while
        // `JSON` is inline (as `Object` above).
        if let Some(name) = json_method_name(callee_expr)
            && self.inline_namespace("JSON")
        {
            return lower_json_call(name, args, callee_expr.span());
        }
        // `Promise.resolve(v)` / `Promise.reject(v)`: the inline promise nodes
        // while `Promise` is inline (as `Object` above).
        if let Some(name) = promise_static_method_name(callee_expr)
            && self.inline_namespace("Promise")
        {
            return lower_promise_static(name, args, callee_expr.span());
        }
        // `Date.now()`: the host wall clock as a number, inline while `Date` is
        // inline (as `Object` above); any argument is rejected.
        if let Expr::Member(member) = callee_expr.as_ref()
            && let Expr::Ident(obj) = member.obj.as_ref()
            && obj.sym.as_ref() == "Date"
            && let MemberProp::Ident(id) = &member.prop
            && id.sym.as_ref() == "now"
            && self.inline_namespace("Date")
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
                not_callable: not_a_function(source_name(callee_expr).as_deref()),
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
                not_callable: CalleeError::not_a_function(name),
            })
        } else if let Some(sig) = self.shared.funcs.get(name) {
            // JS arity semantics: the uniform ABI pads missing arguments with
            // `undefined` (so defaults apply), and surplus arguments are evaluated
            // for their side effects and ignored (or gathered by a rest parameter).
            Ok(IrNode::Call {
                func: sig.index,
                args,
            })
        } else if let Some(kind) = ErrorKind::from_name(name)
            && self.shared.globals.sealed(name)
        {
            // `Error(m)` without `new` constructs too (design 4.7).
            Ok(self.lower_error_construct(kind, args))
        } else if let Some(op) = convert_builtin_op(name)
            && (!matches!(name, "String" | "Number" | "Boolean")
                || self.shared.globals.sealed(name))
        {
            // A conversion built-in `String/Number/Boolean/parseInt/parseFloat`.
            // Reached only when the name is NOT a binding or a user
            // function (checked above), so a user `let String = ..` / `function
            // String(){}` shadows it. Arity is validated here.
            lower_convert_call(op, name, args, ident.span())
        } else {
            // A global-object property, read (or thrown on) at run time.
            Ok(IrNode::CallIndirect {
                callee: Box::new(self.global_read(name)),
                args,
                not_callable: CalleeError::not_a_function(name),
            })
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
            not_callable: method_not_a_function(obj, op.name()),
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
            && self.inline_namespace("Math")
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
            // A member callee `obj.m(...xs)` is a method call binding `this =
            // obj`: the receiver is stored into a temp, then the method read from
            // it. Any other computed callee is an indirect spread call through the
            // funcref table.
            if let Expr::Member(member) = callee_expr {
                if spread_rejecting_member(callee_expr, member) {
                    return Err(CompileError::spanned(
                        call.span(),
                        "spread arguments are unsupported for this builtin method",
                    )
                    .into());
                }
                return self.lower_spread_method_call(member, args);
            }
            return Ok(IrNode::CallSpread {
                target: SpreadCallee::Indirect(
                    Box::new(self.lower_expr(callee_expr)?),
                    not_a_function(source_name(callee_expr).as_deref()),
                ),
                args,
            });
        };
        let name = ident.sym.as_ref();
        if let Some(resolved) = self.resolve(name) {
            // A binding holding a function value: an indirect spread call.
            Ok(IrNode::CallSpread {
                target: SpreadCallee::Indirect(
                    Box::new(Self::read_of(resolved.kind)),
                    CalleeError::not_a_function(name),
                ),
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
            Ok(IrNode::CallSpread {
                target: SpreadCallee::Indirect(
                    Box::new(self.global_read(name)),
                    CalleeError::not_a_function(name),
                ),
                args,
            })
        }
    }

    /// Lower a spread method call `obj.m(...args)` / `obj[k](...args)` (the
    /// argument array already built into `args`): JS order evaluates the
    /// receiver, then the member, then the arguments, and binds `this = obj`.
    fn lower_spread_method_call(
        &mut self,
        member: &swc_ecma_ast::MemberExpr,
        args: Box<IrNode>,
    ) -> anyhow::Result<IrNode> {
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
        let this_local = self.fresh();
        let callee = IrNode::Seq(vec![
            IrNode::Assign {
                local: this_local,
                value: Box::new(obj),
            },
            IrNode::Index {
                obj: Box::new(IrNode::Local(this_local)),
                index: Box::new(key),
            },
        ]);
        Ok(IrNode::CallSpread {
            target: SpreadCallee::Method {
                callee: Box::new(callee),
                this_local,
                not_callable: not_a_function(member_source_name(member).as_deref()),
            },
            args,
        })
    }

    /// Lower a growing array method call `recv.push(x)` / `recv.pop()`. The
    /// receiver is any expression, evaluated exactly once: a plain local is
    /// read in place, anything else (a captured or module binding, a member,
    /// a call result) is stashed in a fresh temp. The receiver is guarded
    /// against `null`/`undefined` (the member read), then an array receiver
    /// grows/shrinks in place; any other receiver has no such method, so the
    /// call throws `<recv>.<method> is not a function` once its argument is
    /// evaluated. Arities are validated here (`push` takes one argument, `pop`
    /// none).
    fn lower_array_method(
        &mut self,
        method: &str,
        obj: &Expr,
        args: Vec<IrNode>,
    ) -> anyhow::Result<IrNode> {
        let value = match method {
            "push" => {
                let [value] = <[IrNode; 1]>::try_from(args).map_err(|args| {
                    CompileError::spanned(
                        obj.span(),
                        format!("push expects 1 argument, got {}", args.len()),
                    )
                })?;
                Some(value)
            }
            "pop" => {
                if !args.is_empty() {
                    return Err(CompileError::spanned(
                        obj.span(),
                        format!("pop expects 0 arguments, got {}", args.len()),
                    )
                    .into());
                }
                None
            }
            _ => unreachable!("caller restricts the method to push/pop"),
        };
        let plain = match obj {
            Expr::Ident(id) => self.resolve(id.sym.as_ref()).and_then(|r| match r.kind {
                AccessKind::Plain(local) => Some(local),
                AccessKind::Cell(_) | AccessKind::Global(_) => None,
            }),
            _ => None,
        };
        let (local, recv) = if let Some(local) = plain {
            (local, IrNode::Local(local))
        } else {
            let tmp = self.fresh();
            let recv = IrNode::Assign {
                local: tmp,
                value: Box::new(self.lower_expr(obj)?),
            };
            (tmp, recv)
        };
        let not_array = method_not_a_function(obj, method);
        let grow = match value {
            Some(value) => IrNode::ArrayPush {
                local,
                value: Box::new(value),
                not_array: Some(not_array),
            },
            None => IrNode::ArrayPop { local, not_array },
        };
        Ok(IrNode::Seq(vec![nullish_check(recv, method), grow]))
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
            not_callable: method_not_a_function(obj, method),
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
            not_callable: method_not_a_function(obj, method),
        })
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
            not_callable: not_a_function(member_source_name(member).as_deref()),
        })
    }

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
            let resolved = self.resolve_target(name, ident.span())?;
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
            let resolved = self.resolve_target(name, ident.span())?;
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
                let resolved = self.resolve_target(name, ident.span())?;
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
                let rhs = if compound_op.is_none() {
                    self.lower_named_expr(&assign.right, name)?
                } else {
                    self.lower_expr(&assign.right)?
                };
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
            // key; `.length` on an array is the runtime's `length` write).
            AssignTarget::Simple(SimpleAssignTarget::Member(m)) => {
                let obj = self.lower_expr(&m.obj)?;
                let index = match &m.prop {
                    MemberProp::Computed(c) => self.lower_expr(&c.expr)?,
                    MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
                    MemberProp::PrivateName(_) => {
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
            // A destructuring assignment desugars to statements, so it is only
            // lowered in statement position (see `lower_stmt`).
            AssignTarget::Pat(_) => Err(CompileError::spanned(
                assign.left.span(),
                "destructuring assignment is only supported as an expression statement",
            )
            .into()),
            AssignTarget::Simple(_) => Err(CompileError::spanned(
                assign.left.span(),
                "unsupported assignment target",
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
        let resolved = self.resolve_target(name, ident.span())?;
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
            // A captured cell or a global: `x = ToNumber(x) + delta` (prefix
            // yields new); postfix yields the old value as `new - delta`.
            kind @ (AccessKind::Cell(_) | AccessKind::Global(_)) => {
                let delta = if inc { 1.0 } else { -1.0 };
                let new_value = IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::ConvertCall {
                        op: ConvertOp::ToNumber,
                        arg: Box::new(Self::read_of(kind.clone())),
                        radix: None,
                    }),
                    rhs: Box::new(IrNode::Num(delta)),
                };
                let store = Self::write_of(kind, new_value);
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
            MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().to_owned()),
            MemberProp::PrivateName(_) => {
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

    /// Lower an array literal `[e0, e1, ..]` into an [`IrNode::Array`], evaluating
    /// each element left-to-right; an elision (`[1, , 3]`) is an
    /// [`IrNode::ArrayHole`] (the parser drops a single trailing comma, so
    /// `[1,]` has one element). A literal with a spread element lowers to the
    /// runtime-built [`IrNode::ArraySpread`].
    ///
    /// # Errors
    ///
    /// Returns an error if any element expression is outside the supported subset.
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
                parts.push(match elem {
                    None => SpreadElem::Item(IrNode::ArrayHole),
                    Some(elem) if elem.spread.is_some() => {
                        SpreadElem::Spread(self.lower_expr(&elem.expr)?)
                    }
                    Some(elem) => SpreadElem::Item(self.lower_expr(&elem.expr)?),
                });
            }
            return Ok(IrNode::ArraySpread(parts));
        }
        let mut elems = Vec::with_capacity(arr.elems.len());
        for elem in &arr.elems {
            elems.push(match elem {
                None => IrNode::ArrayHole,
                Some(elem) => self.lower_expr(&elem.expr)?,
            });
        }
        Ok(IrNode::Array(elems))
    }

    /// Lower an object literal `{a: e0, "b": e1, [k]: e2, 1: e3, m(){..}, n, ..}`
    /// into an [`IrNode::Object`], evaluating each key and value left-to-right.
    /// Key-value entries, shorthand properties (`{n}` ≡ `{n: n}`), and
    /// method/shorthand-method properties (`{m(){..}}` ≡ `{m: function(){..}}`, a
    /// first-class function value carrying `this`) are in scope; identifier and
    /// string keys are static, computed and numeric keys are converted at run
    /// time. A getter/setter (`get x(){..}` / `set x(v){..}`) defines an
    /// accessor property; a getter and setter sharing a static key share one
    /// accessor entry. Spread and async/generator methods are rejected with
    /// spans.
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
                    let key = self.lower_prop_key(&kv.key)?;
                    let value = match &key {
                        PropKey::Static(name) => self.lower_named_expr(&kv.value, name)?,
                        _ => self.lower_expr(&kv.value)?,
                    };
                    entries.push((key, value));
                }
                // Shorthand `{n}` ≡ `{n: n}`: the value is the identifier read.
                Prop::Shorthand(ident) => {
                    let key = PropKey::Static(ident.sym.as_ref().to_owned());
                    let value = self.lower_ident(ident.sym.as_ref(), ident.span)?;
                    entries.push((key, value));
                }
                // A method / shorthand method `{m(){..}}` lifts to a first-class
                // function value (like an arrow) that carries `this`.
                Prop::Method(method) => {
                    reject_unsupported_function(&method.function)?;
                    let key = self.lower_prop_key(&method.key)?;
                    let block = method
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("object method requires a body"))?;
                    let param_pats: Vec<&Pat> =
                        method.function.params.iter().map(|p| &p.pat).collect();
                    let name = static_key_name(&key);
                    let value = self.lower_lifted_function(
                        &param_pats,
                        BodyKind::Block(&block.stmts),
                        name,
                        FnKind::Plain,
                    )?;
                    entries.push((key, value));
                }
                Prop::Getter(getter) => {
                    let key = self.lower_prop_key(&getter.key)?;
                    let block = getter
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("getter requires a body"))?;
                    let param_pats: Vec<&Pat> =
                        getter.function.params.iter().map(|p| &p.pat).collect();
                    let name = format!("get {}", static_key_name(&key));
                    let value = self.lower_lifted_function(
                        &param_pats,
                        BodyKind::Block(&block.stmts),
                        &name,
                        FnKind::Plain,
                    )?;
                    push_accessor_half(&mut entries, key, value, true);
                }
                Prop::Setter(setter) => {
                    let key = self.lower_prop_key(&setter.key)?;
                    let block = setter
                        .function
                        .body
                        .as_ref()
                        .ok_or_else(|| anyhow!("setter requires a body"))?;
                    let param_pats: Vec<&Pat> =
                        setter.function.params.iter().map(|p| &p.pat).collect();
                    let name = format!("set {}", static_key_name(&key));
                    let value = self.lower_lifted_function(
                        &param_pats,
                        BodyKind::Block(&block.stmts),
                        &name,
                        FnKind::Plain,
                    )?;
                    push_accessor_half(&mut entries, key, value, false);
                }
                Prop::Assign(_) => {
                    return Err(
                        CompileError::spanned(prop.span(), "unsupported object property").into(),
                    );
                }
            }
        }
        Ok(IrNode::Object {
            entries,
            proto: None,
            class: crate::abi::CLASS_ORDINARY,
            reserve: 0,
        })
    }

    /// Lower an object-literal property name: an identifier or string key is
    /// static; a numeric or computed key is evaluated (in source order) and
    /// converted with `ToPropertyKey` at run time.
    fn lower_prop_key(&mut self, name: &PropName) -> anyhow::Result<PropKey> {
        match name {
            PropName::Ident(id) => Ok(PropKey::Static(id.sym.as_ref().to_owned())),
            PropName::Str(s) => Ok(PropKey::Static(s.value.to_string_lossy().into_owned())),
            PropName::Num(n) => Ok(PropKey::Computed(IrNode::Num(n.value))),
            PropName::Computed(c) => Ok(PropKey::Computed(self.lower_expr(&c.expr)?)),
            PropName::BigInt(_) => Err(CompileError::spanned(
                name.span(),
                "unsupported object key: BigInt keys are not supported",
            )
            .into()),
        }
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
    /// Lower `left instanceof right` (design 4.6) into an [`IrNode::InstanceOf`].
    /// See [`Self::instanceof_kind`] for the right-hand-side dispatch.
    fn lower_instanceof(&mut self, left: &Expr, right: &Expr) -> anyhow::Result<IrNode> {
        let value = self.lower_expr(left)?;
        Ok(IrNode::InstanceOf {
            value: Box::new(value),
            kind: self.instanceof_kind(right)?,
        })
    }

    /// Resolve the right-hand side of an `instanceof` to an [`InstanceOfKind`].
    /// An unshadowed, sealed `Error`-family/`Array`/`Function`/`Date`/`Object`
    /// identifier takes its builtin test; any other expression — including a
    /// binding or declaration that shadows one of those names (DF11) — is
    /// `OrdinaryHasInstance` on its value.
    fn instanceof_kind(&mut self, right: &Expr) -> anyhow::Result<InstanceOfKind> {
        if let Expr::Ident(id) = right {
            let name = id.sym.as_ref();
            if self.resolve(name).is_none()
                && !self.shared.funcs.contains_key(name)
                && self.shared.globals.sealed(name)
            {
                if let Some(kind) = ErrorKind::from_name(name) {
                    return Ok(InstanceOfKind::Proto(Box::new(IrNode::ErrorProto(kind))));
                }
                match name {
                    "Array" => return Ok(InstanceOfKind::Array),
                    "Function" => return Ok(InstanceOfKind::Function),
                    "Date" => return Ok(InstanceOfKind::Date),
                    "Object" => return Ok(InstanceOfKind::Object),
                    _ => {}
                }
            }
        }
        Ok(InstanceOfKind::Ctor(Box::new(self.lower_expr(right)?)))
    }

    fn lower_new(&mut self, new_expr: &swc_ecma_ast::NewExpr) -> anyhow::Result<IrNode> {
        // The builtin constructors keep their dedicated paths while unshadowed
        // and sealed: a user `let Map = ..` or `function Error(){}` (DF11), or
        // a reassigned global, constructs through the general path below.
        if let Expr::Ident(callee) = new_expr.callee.as_ref()
            && self.resolve(callee.sym.as_ref()).is_none()
            && !self.shared.funcs.contains_key(callee.sym.as_ref())
            && self.shared.globals.sealed(callee.sym.as_ref())
        {
            let name = callee.sym.as_ref();
            // `new Map()` / `new Set()`: a fresh empty collection handle. Only
            // the empty constructor is in scope — arguments (e.g.
            // `new Map([[k, v]])`) are rejected.
            if matches!(name, "Map" | "Set") {
                let args = new_expr.args.as_deref().unwrap_or(&[]);
                if !args.is_empty() {
                    return Err(CompileError::spanned(
                        callee.span(),
                        format!(
                            "new {name}(..) with constructor arguments is unsupported (only the \
                             empty `new {name}()` is supported)"
                        ),
                    )
                    .into());
                }
                return Ok(IrNode::MapNew { set: name == "Set" });
            }
            // `new Promise(executor)`: the executor form of the Promise constructor.
            if name == "Promise" {
                return self.lower_new_promise(callee.span, new_expr);
            }
            // `new Date()` / `new Date(ms)`.
            if name == "Date" {
                return self.lower_new_date(callee.span, new_expr);
            }
            if let Some(kind) = ErrorKind::from_name(name) {
                let mut args = Vec::new();
                for arg in new_expr.args.as_deref().unwrap_or(&[]) {
                    if arg.spread.is_some() {
                        return Err(CompileError::spanned(
                            arg.span(),
                            "spread arguments are unsupported",
                        )
                        .into());
                    }
                    args.push(self.lower_expr(&arg.expr)?);
                }
                return Ok(self.lower_error_construct(kind, args));
            }
        }
        self.lower_construct(new_expr)
    }

    /// Lower `new X(args)` for any constructor expression `X` into an
    /// [`IrNode::Construct`] (design 4.5). Class identity is by binding: `X` is
    /// evaluated like any expression. When `X` names a recorded class, `this`'s
    /// property table is pre-sized for the fields its constructor chain assigns
    /// (capacity only; a rebound name just over-reserves).
    fn lower_construct(&mut self, new_expr: &swc_ecma_ast::NewExpr) -> anyhow::Result<IrNode> {
        let callee = self.lower_expr(&new_expr.callee)?;
        let reserve = match new_expr.callee.as_ref() {
            Expr::Ident(id) => self
                .shared
                .classes
                .borrow()
                .get(id.sym.as_ref())
                .map_or(0, |info| info.fields.len()),
            _ => 0,
        };
        let (args, spread) = self.lower_construct_args(new_expr.args.as_deref().unwrap_or(&[]))?;
        let name = source_name(&new_expr.callee);
        Ok(IrNode::Construct {
            callee: Box::new(callee),
            args,
            spread,
            callee_local: self.fresh(),
            this_local: self.fresh(),
            reserve: u32::try_from(reserve).unwrap_or(u32::MAX),
            not_constructor: CalleeError::not_a_constructor(
                name.as_deref().unwrap_or(CalleeError::EXPRESSION),
            ),
            new_target: None,
        })
    }

    /// Lower the arguments of a `new` / `super(..)` call: the argument list,
    /// or, when any argument is a spread, the argument array.
    fn lower_construct_args(
        &mut self,
        args: &[ExprOrSpread],
    ) -> anyhow::Result<(Vec<IrNode>, Option<Box<IrNode>>)> {
        let mut lowered = Vec::with_capacity(args.len());
        let mut parts = Vec::new();
        let has_spread = args.iter().any(|arg| arg.spread.is_some());
        for arg in args {
            let value = self.lower_expr(&arg.expr)?;
            if !has_spread {
                lowered.push(value);
            } else if arg.spread.is_some() {
                parts.push(SpreadElem::Spread(value));
            } else {
                parts.push(SpreadElem::Item(value));
            }
        }
        Ok((
            lowered,
            has_spread.then(|| Box::new(IrNode::ArraySpread(parts))),
        ))
    }

    /// Build the Error instance `<kind>(args)` / `new <kind>(args)` constructs:
    /// the first argument is the message (see [`Self::lower_error_object`]);
    /// surplus arguments are evaluated after it, left to right, for their side
    /// effects only.
    fn lower_error_construct(&mut self, kind: ErrorKind, args: Vec<IrNode>) -> IrNode {
        let mut args = args.into_iter();
        let mut message = args.next();
        let extra: Vec<IrNode> = args.collect();
        if extra.is_empty() {
            return self.lower_error_object(kind, message);
        }
        // A non-literal message is evaluated into a temp before the surplus
        // arguments run.
        let head = message
            .take_if(|message| !matches!(message, IrNode::Str(_) | IrNode::Undefined))
            .map(|value| {
                let local = self.fresh();
                message = Some(IrNode::Local(local));
                IrNode::Assign {
                    local,
                    value: Box::new(value),
                }
            });
        let error = self.lower_error_object(kind, message);
        IrNode::Seq(head.into_iter().chain(extra).chain([error]).collect())
    }

    /// Build the Error instance `new <kind>(message)` / `<kind>(message)`
    /// constructs (design 4.7): an own `message` = `ToString(message)` only when
    /// the argument is not `undefined`, and `stack` = `"<name>: <message>"`
    /// (just `<name>` for an empty or absent message — jsc keeps no runtime
    /// call frames). The argument is evaluated EXACTLY ONCE into a temp; a
    /// string literal or `undefined` resolves both at compile time.
    fn lower_error_object(&mut self, kind: ErrorKind, message: Option<IrNode>) -> IrNode {
        let name = kind.name();
        let stack_of = |text: &str| {
            IrNode::Str(if text.is_empty() {
                name.to_owned()
            } else {
                format!("{name}: {text}")
            })
        };
        match message {
            None | Some(IrNode::Undefined) => {
                IrNode::error_object(kind, None, IrNode::Str(name.to_owned()))
            }
            Some(IrNode::Str(text)) => {
                let stack = stack_of(&text);
                IrNode::error_object(kind, Some(IrNode::Str(text)), stack)
            }
            Some(message) => {
                let (arg, msg) = (self.fresh(), self.fresh());
                IrNode::error_from_argument(kind, IrNode::ErrorProto(kind), message, arg, msg)
            }
        }
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
                    radix: None,
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

/// `value` is evaluated once into `temp`; when it is `null`/`undefined` the
/// whole expression is `undefined`, otherwise `tail` (which reads `temp`).
fn nullish_short_circuit(temp: u32, value: IrNode, tail: IrNode) -> IrNode {
    // nullish(temp) ≡ `temp === null || temp === undefined`; the value is teed
    // into `temp` while computing the first comparison.
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
    IrNode::Ternary {
        cond: Box::new(is_nullish),
        then_branch: Box::new(IrNode::Undefined),
        else_branch: Box::new(tail),
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

/// The source name a not-a-function / not-a-constructor `TypeError` gives a
/// callee: an identifier, `this`, or a member chain of them with static keys
/// (`o.m` and `o['m']` both name `o.m`); `None` for anything else.
fn source_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.as_ref().to_owned()),
        Expr::This(_) => Some("this".to_owned()),
        Expr::Paren(p) => source_name(&p.expr),
        Expr::Member(member) => member_source_name(member),
        _ => None,
    }
}

/// [`source_name`] of a member expression `obj.m` / `obj['m']`.
fn member_source_name(member: &swc_ecma_ast::MemberExpr) -> Option<String> {
    Some(format!(
        "{}.{}",
        source_name(&member.obj)?,
        member_method_name(member)?
    ))
}

/// `<callee> is not a function` for a call whose callee is `name` (falling
/// back to `expression` when the callee has no source name).
fn not_a_function(name: Option<&str>) -> CalleeError {
    CalleeError::not_a_function(name.unwrap_or(CalleeError::EXPRESSION))
}

/// `<obj>.<method> is not a function` for a builtin-named method call's
/// user-method fallback.
fn method_not_a_function(obj: &Expr, method: &str) -> CalleeError {
    not_a_function(
        source_name(obj)
            .map(|obj| format!("{obj}.{method}"))
            .as_deref(),
    )
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
/// Whether a spread call through the member callee `callee` (= `member`) hits
/// a builtin dispatched by name with no user-method fallback (`console.log`,
/// the `Object`/`JSON`/`Promise` statics, the array/string methods,
/// `getTime`), which take no spread.
fn spread_rejecting_member(callee: &Expr, member: &swc_ecma_ast::MemberExpr) -> bool {
    is_console_log(callee)
        || object_method_name(callee).is_some()
        || json_method_name(callee).is_some()
        || promise_static_method_name(callee).is_some()
        || matches!(&member.prop, MemberProp::Ident(id) if {
            let name = id.sym.as_ref();
            matches!(name, "push" | "pop" | "map" | "filter" | "reduce" | "forEach" | "getTime")
                || non_callback_method_op(name).is_some()
        })
}

fn receiver_method_op(name: &str) -> Option<ReceiverOp> {
    Some(match name {
        "then" => ReceiverOp::Then,
        "catch" => ReceiverOp::Catch,
        "finally" => ReceiverOp::Finally,
        "next" => ReceiverOp::Next,
        _ => return None,
    })
}

/// The compile-time name of a static property key (`""` for a computed key):
/// the `name` given to a method or accessor defined under it.
fn static_key_name(key: &PropKey) -> &str {
    match key {
        PropKey::Static(name) | PropKey::Hidden(name) | PropKey::Fixed(name) => name,
        PropKey::Computed(_) => "",
    }
}

/// Append one accessor half (`getter` or setter function `value`) under `key`:
/// it completes the accessor entry of the same static key when one exists,
/// else starts a new accessor entry with the other half `undefined`.
pub(super) fn push_accessor_half(
    entries: &mut Vec<(PropKey, IrNode)>,
    key: PropKey,
    value: IrNode,
    getter: bool,
) {
    let existing = entries.iter_mut().rev().find_map(|(k, v)| match v {
        IrNode::MakeAccessor { getter, setter }
            if !matches!(k, PropKey::Computed(_)) && *k == key =>
        {
            Some((getter, setter))
        }
        _ => None,
    });
    if let Some((get, set)) = existing {
        **if getter { get } else { set } = value;
        return;
    }
    let (get, set) = if getter {
        (value, IrNode::Undefined)
    } else {
        (IrNode::Undefined, value)
    };
    entries.push((
        key,
        IrNode::MakeAccessor {
            getter: Box::new(get),
            setter: Box::new(set),
        },
    ));
}
