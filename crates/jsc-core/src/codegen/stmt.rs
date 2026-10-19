#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::{Spanned, SyntaxContext};

use crate::error::CompileError;

impl LowerCtx<'_> {
    /// Lower one statement, appending the resulting [`IrStmt`](s) to `out`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_stmt(&mut self, stmt: &Stmt, out: &mut Vec<IrStmt>) -> anyhow::Result<()> {
        // Inside a tracked top-level `try`, a compound statement completes with
        // `undefined` when its body produces no value (UpdateEmpty).
        if matches!(
            stmt,
            Stmt::If(_)
                | Stmt::While(_)
                | Stmt::DoWhile(_)
                | Stmt::For(_)
                | Stmt::ForOf(_)
                | Stmt::ForIn(_)
                | Stmt::Labeled(_)
                | Stmt::Switch(_)
        ) {
            self.reset_completion(out);
        }
        match stmt {
            Stmt::Expr(expr_stmt) => {
                // `yield* iterable;` delegates to an inner generator: it
                // desugars to a driving loop rather than a single expression.
                if let Expr::Yield(yield_expr) = expr_stmt.expr.as_ref()
                    && yield_expr.delegate
                {
                    return self.lower_yield_delegate(yield_expr, out);
                }
                let value = self.lower_expr(&expr_stmt.expr)?;
                out.push(IrStmt::Expr(match self.completion_local() {
                    Some(local) => IrNode::Assign {
                        local,
                        value: Box::new(value),
                    },
                    None => value,
                }));
                Ok(())
            }
            // `let`/`const`/`var` declarations. An uninitialized `let x;` binds
            // `undefined` (a `const` with no initializer is a parser error).
            Stmt::Decl(Decl::Var(var_decl)) => self.lower_var_decl(var_decl, out),
            // A nested `function foo(){ .. }` declaration reached outside a
            // hoisting statement list (see `lower_stmts`) lowers in place to a
            // first-class function value bound to `foo` in the current scope.
            Stmt::Decl(Decl::Fn(fn_decl)) => self.lower_nested_fn_decl(fn_decl, out),
            // A `class A { constructor(..){ .. } }` declaration lowers to a
            // class value (constructor function + shared prototype) bound to `A`.
            Stmt::Decl(Decl::Class(class_decl)) => self.lower_class_decl(class_decl, out),
            // A bare block `{ .. }` introduces a new lexical scope: its
            // statements lower straight into the current sequence (blocks do not
            // nest wasm locals), but its `let`/`const` bindings are confined to
            // the block. Inner bindings shadow outer ones and do not leak out.
            Stmt::Block(block) => {
                self.push_scope();
                let result = self.lower_stmts(&block.stmts, out);
                self.pop_scope();
                result
            }
            Stmt::If(if_stmt) => {
                let cond = self.lower_expr(&if_stmt.test)?;
                let then_body = self.lower_body(&if_stmt.cons)?;
                let else_body = match &if_stmt.alt {
                    Some(alt) => self.lower_body(alt)?,
                    None => Vec::new(),
                };
                out.push(IrStmt::If {
                    cond,
                    then_body,
                    else_body,
                });
                Ok(())
            }
            Stmt::While(while_stmt) => self.lower_while(while_stmt, None, out),
            Stmt::DoWhile(do_stmt) => self.lower_do_while(do_stmt, None, out),
            Stmt::For(for_stmt) => self.lower_for(for_stmt, None, out),
            Stmt::ForOf(for_of_stmt) => self.lower_for_of(for_of_stmt, None, out),
            Stmt::ForIn(for_in_stmt) => self.lower_for_in(for_in_stmt, None, out),
            Stmt::Labeled(labeled) => self.lower_labeled(labeled, out),
            Stmt::Switch(switch_stmt) => self.lower_switch(switch_stmt, out),
            // `break;` jumps to the end of the nearest enclosing switch/loop;
            // `break foo;` exits the loop labeled `foo` (which may be an outer
            // loop). An unknown label is a spanned error.
            Stmt::Break(break_stmt) => {
                let target =
                    self.loop_target(break_stmt.label.as_ref(), break_stmt.span(), false)?;
                out.push(IrStmt::Break(target));
                Ok(())
            }
            // `continue;` jumps to the update/next-iteration point of the nearest
            // enclosing loop; `continue foo;` continues the loop labeled `foo`
            // (which may be an outer loop). A `continue` outside any loop (e.g. in
            // a bare `switch`) is illegal, and an unknown label is a spanned error.
            Stmt::Continue(continue_stmt) => {
                let target =
                    self.loop_target(continue_stmt.label.as_ref(), continue_stmt.span(), true)?;
                out.push(IrStmt::Continue(target));
                Ok(())
            }
            Stmt::Return(ret_stmt) => {
                let value = match &ret_stmt.arg {
                    Some(expr) => Some(self.lower_expr(expr)?),
                    None => None,
                };
                out.push(IrStmt::Return(value));
                Ok(())
            }
            // `throw expr;`: lower the operand and wrap it in an
            // [`IrNode::Throw`], emitted as a bare expression statement. The node
            // diverges (it sets the pending-exception globals and unwinds), so any
            // statements after it in the same block are dead — matching JS.
            Stmt::Throw(throw_stmt) => {
                let value = self.lower_expr(&throw_stmt.arg)?;
                out.push(IrStmt::Expr(IrNode::Throw(Box::new(value))));
                Ok(())
            }
            // `try { .. } catch (e) { .. } finally { .. }` (either clause may be
            // absent, but not both), lowered by `lower_try`.
            Stmt::Try(try_stmt) => self.lower_try(try_stmt, out),
            // Attach the offending statement's source range so a diagnostic
            // renderer can point at it, and describe the construct in plain
            // English rather than leaking the raw swc AST debug form.
            other => Err(CompileError::spanned(other.span(), unsupported_stmt_msg(other)).into()),
        }
    }

    /// Lower a function body or block statement list into `out`, hoisting its
    /// function declarations: every declaration is bound and initialized before
    /// any statement of the list runs, so it can be called or read before its
    /// textual position, recurse through its own name, and be mutually
    /// recursive with its siblings.
    ///
    /// The declaration names are bound first (so the statements resolve them),
    /// then the other statements are lowered, and only then the declaration
    /// bodies — once every binding of the list is declared, so a body may
    /// capture a `let`/`const` declared later. Such a captured binding's cell is
    /// created up front (holding `undefined` until its declaration runs) so the
    /// hoisted closures share it; its declaration then stores into that cell.
    ///
    /// A generator (`env_backed`) frame keeps the in-order lowering.
    pub(super) fn lower_stmts(
        &mut self,
        stmts: &[Stmt],
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let fns: Vec<&FnDecl> = stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Decl(Decl::Fn(f)) => Some(f),
                _ => None,
            })
            .collect();
        if fns.is_empty() || self.frame().env_backed {
            for stmt in stmts {
                self.lower_stmt(stmt, out)?;
            }
            return Ok(());
        }
        let mut prefix = Vec::new();
        let fn_locals: Vec<u32> = fns
            .iter()
            .map(|f| self.declare(f.ident.sym.as_ref(), true, f.ident.span.lo.0))
            .collect();
        for &local in &fn_locals {
            if self.is_cell(local) {
                prefix.push(IrStmt::Decl {
                    local,
                    init: IrNode::CellNew(Box::new(IrNode::Undefined)),
                });
            }
        }
        let mut rest = Vec::new();
        for stmt in stmts {
            if !matches!(stmt, Stmt::Decl(Decl::Fn(_))) {
                self.lower_stmt(stmt, &mut rest)?;
            }
        }
        // Hoist the cell of every captured binding the list declares: create it
        // in the prefix and turn its declaration into a store into that cell.
        for local in self.innermost_cell_locals() {
            if fn_locals.contains(&local) {
                continue;
            }
            for stmt in &mut rest {
                let IrStmt::Decl {
                    local: decl_local,
                    init: IrNode::CellNew(value),
                } = stmt
                else {
                    continue;
                };
                if *decl_local != local {
                    continue;
                }
                let value = std::mem::replace(value.as_mut(), IrNode::Undefined);
                *stmt = IrStmt::Decl {
                    local: self.fresh(),
                    init: IrNode::CellSet {
                        cell: Box::new(IrNode::Local(local)),
                        value: Box::new(value),
                    },
                };
                prefix.push(IrStmt::Decl {
                    local,
                    init: IrNode::CellNew(Box::new(IrNode::Undefined)),
                });
                break;
            }
        }
        for (fn_decl, local) in fns.into_iter().zip(fn_locals) {
            let value = self.lower_nested_fn_value(fn_decl)?;
            // A store into the pre-created cell is bound as a `Decl` of a
            // scratch local so it never becomes `main`'s completion value.
            prefix.push(if self.is_cell(local) {
                IrStmt::Decl {
                    local: self.fresh(),
                    init: IrNode::CellSet {
                        cell: Box::new(IrNode::Local(local)),
                        value: Box::new(value),
                    },
                }
            } else {
                IrStmt::Decl { local, init: value }
            });
        }
        out.extend(prefix);
        out.extend(rest);
        Ok(())
    }

    /// Lower a `let`/`const`/`var` declaration. A declarator with no initializer
    /// (`let x;`) binds `undefined`; otherwise the initializer is lowered before
    /// the name is bound so a declaration cannot reference itself, and a captured
    /// binding is boxed into a cell.
    fn lower_var_decl(
        &mut self,
        var_decl: &swc_ecma_ast::VarDecl,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // `const` bindings are immutable; `let`/`var` are mutable.
        let mutable = var_decl.kind != VarDeclKind::Const;
        for declarator in &var_decl.decls {
            // A missing initializer (`let x;`) binds `undefined` (JS semantics); a
            // `const` with no initializer is already rejected by the parser.
            let init = match declarator.init.as_ref() {
                Some(init) => self.lower_expr(init)?,
                None => IrNode::Undefined,
            };
            match &declarator.name {
                // The `Pat::Ident` fast path: bind one local directly (a captured
                // binding is boxed so its cell holds the value).
                Pat::Ident(ident) => {
                    let local = self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident));
                    self.push_binding(local, init, out);
                }
                // A destructuring pattern (`let [a, b] = ..`) desugars to indexed
                // reads off a hidden temp holding the initializer.
                other => self.lower_binding_pattern(other, init, mutable, out)?,
            }
        }
        Ok(())
    }

    /// Lower a `class A { constructor(x){ this.v = x } }` declaration into
    /// a class value bound to `A`. The class value is a heap object holding two
    /// reserved entries: the lowered CONSTRUCTOR function value (an ordinary
    /// uniform-ABI function that reads `this`, reusing the function lifting and
    /// `this`-binding), and a single shared PROTOTYPE object holding the methods,
    /// allocated once here. The class's instance shape — the
    /// field names the constructor assigns via `this.<field> = ..` — is registered
    /// in [`Shared::classes`] so a later `new A(args)` can pre-slot the instance,
    /// since the object model has no dynamic key growth. The instance shape is the
    /// UNION of the constructor's `this.<field> = ..` assignments and the class's
    /// instance FIELD declarations, whose initializers run before the
    /// constructor body (see the field-desugar below).
    ///
    /// Instance getter/setter accessors (`get x(){}` / `set x(v){}`) are supported
    /// (P2b): they lower to a `TYPE_ACCESSOR` box on the shared prototype.
    /// Out-of-scope class forms are still rejected WITH SPANS: decorators, and
    /// unsupported members — methods that are async / generator / computed, a
    /// STATIC getter/setter, fields that are static / computed, and private /
    /// static-block forms. A class EXPRESSION (`let C = class{}`) never reaches
    /// here — it is rejected in expression position.
    #[allow(clippy::too_many_lines)]
    fn lower_class_decl(
        &mut self,
        class_decl: &ClassDecl,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let class = class_decl.class.as_ref();
        // `class C extends P` (E-series): the superclass must be a bare identifier
        // naming a class declared earlier, so its prototype/constructor/fields are
        // already recorded. `super` inside the body resolves to this name.
        let (parent, parent_span) = match &class.super_class {
            None => (None, class.span()),
            Some(super_expr) => {
                let Expr::Ident(id) = super_expr.as_ref() else {
                    return Err(CompileError::spanned(
                        super_expr.span(),
                        "a class may only `extends` a named class",
                    )
                    .into());
                };
                let parent_name = id.sym.as_ref();
                if !self.shared.classes.borrow().contains_key(parent_name) {
                    return Err(CompileError::spanned(
                        id.span,
                        format!("superclass `{parent_name}` is not a declared class"),
                    )
                    .into());
                }
                (Some(parent_name.to_owned()), id.span)
            }
        };
        if !class.decorators.is_empty() {
            return Err(
                CompileError::spanned(class.span(), "class decorators are not supported").into(),
            );
        }

        // Find the (at most one) constructor, collect the instance methods,
        // and collect the instance field declarations; every other member is
        // out of scope. Static members (`static m(){}` / `static f = init`) are
        // collected separately and stored directly on the class VALUE object below,
        // so `C.m()` / `C.f` read them off the class. Computed names and out-of-scope
        // method forms (getter/setter/async/generator) are rejected WITH SPANS here,
        // static or not; private fields arrive as `ClassMember::PrivateProp` and are
        // rejected by `reject_class_member`.
        let mut ctor = None;
        let mut methods: Vec<&ClassMethod> = Vec::new();
        let mut static_methods: Vec<&ClassMethod> = Vec::new();
        let mut accessors: Vec<&ClassMethod> = Vec::new();
        let mut field_props: Vec<&ClassProp> = Vec::new();
        let mut static_fields: Vec<&ClassProp> = Vec::new();
        for member in &class.body {
            match member {
                ClassMember::Constructor(c) => {
                    if ctor.is_some() {
                        return Err(CompileError::spanned(
                            c.span(),
                            "a class may have at most one constructor",
                        )
                        .into());
                    }
                    ctor = Some(c);
                }
                // A plain instance method is lowered onto the shared prototype below;
                // a `static` method is lowered onto the class value. An INSTANCE
                // getter/setter (`get x(){}` / `set x(v){}`) is an accessor property
                // (P2b), collected separately and stored as a `MakeAccessor` box on
                // the prototype. Every other out-of-scope form — a STATIC accessor,
                // an async/generator/computed method (accessor or not) — is rejected
                // with a span (static or not).
                ClassMember::Method(m) => {
                    if matches!(m.kind, MethodKind::Getter | MethodKind::Setter) && !m.is_static {
                        reject_out_of_scope_accessor(m)?;
                        accessors.push(m);
                    } else {
                        reject_out_of_scope_method(m)?;
                        if m.is_static {
                            static_methods.push(m);
                        } else {
                            methods.push(m);
                        }
                    }
                }
                // A plain instance field (`f = init` / `f;`) is initialized on
                // construction (below); a `static` field is stored on the class
                // value. Computed field names remain out of scope.
                ClassMember::ClassProp(p) => {
                    reject_out_of_scope_field(p)?;
                    if p.is_static {
                        static_fields.push(p);
                    } else {
                        field_props.push(p);
                    }
                }
                // A stray `;` in the class body binds nothing — harmless.
                ClassMember::Empty(_) => {}
                other => return Err(reject_class_member(other)),
            }
        }

        // Lower the constructor body to a first-class function value. Each instance
        // field `f = init` is desugared to a synthetic `this.f = init` statement
        // (an uninitialized `f;` to `this.f = undefined`), and these run BEFORE the
        // constructor body in class-body source order — so a constructor observes
        // the already-initialized fields. A class with fields but NO explicit
        // constructor synthesizes a default constructor whose body is exactly these
        // field initializers; a class with neither gets an empty function. The
        // combined body reads `this` through the `this`-binding machinery like any method, and
        // the instance shape (below) is the union of the field names and the
        // constructor's own `this.<f> = ..` assignments — dedup handled by
        // `collect_instance_fields`.
        let mut combined_stmts: Vec<Stmt> = Vec::with_capacity(field_props.len());
        for p in &field_props {
            combined_stmts.push(field_init_stmt(p)?);
        }
        let param_pats: Vec<&Pat> = match ctor {
            Some(c) => {
                let body = c
                    .body
                    .as_ref()
                    .ok_or_else(|| anyhow!("constructor requires a body"))?;
                combined_stmts.extend(body.stmts.iter().cloned());
                let mut pats = Vec::with_capacity(c.params.len());
                for param in &c.params {
                    match param {
                        ParamOrTsParamProp::Param(p) => pats.push(&p.pat),
                        ParamOrTsParamProp::TsParamProp(tp) => {
                            return Err(CompileError::spanned(
                                tp.span(),
                                "TypeScript parameter properties are not supported",
                            )
                            .into());
                        }
                    }
                }
                pats
            }
            None => Vec::new(),
        };
        // A derived class inherits the parent's instance fields (prepended, so the
        // shape is parent-then-own): `new C()` must pre-slot every field the whole
        // constructor chain assigns, since the object model has no dynamic growth.
        let own_fields = collect_instance_fields(&combined_stmts);
        let fields = match &parent {
            Some(p) => {
                let classes = self.shared.classes.borrow();
                let mut all = classes.get(p).map(|i| i.fields.clone()).unwrap_or_default();
                for f in own_fields {
                    if !all.contains(&f) {
                        all.push(f);
                    }
                }
                all
            }
            None => own_fields,
        };

        // Bind the class name and record its instance shape BEFORE lowering the
        // constructor and method bodies, so a `new C()` referencing the class from
        // inside any of them resolves to this binding. When a body captures the
        // name it is boxed into a cell (like a `let`); the cell is allocated empty
        // below and filled with the class value, so the ctor/method closures capture
        // a live cell pointer that a later `new C()` reads back.
        let name = class_decl.ident.sym.as_ref();
        let local = self.declare(name, true, class_decl.ident.span.lo.0);
        self.shared
            .classes
            .borrow_mut()
            .insert(name.to_owned(), ClassInfo { fields });

        // `super` inside the constructor/method bodies resolves to this parent; save
        // and restore the enclosing value so a nested class declaration is isolated.
        let prev_super = self.current_super.take();
        self.current_super.clone_from(&parent);

        // The constructor value. A derived class WITHOUT its own constructor reuses
        // the parent's constructor directly (an implicit `constructor(...a){ super(...a) }`):
        // reading `Parent["@@constructor"]` forwards the arguments and sets the parent's
        // fields on the pre-slotted instance. A subclass that declares fields but no
        // constructor would need the fields initialized too, which the alias skips, so
        // it must declare a constructor that calls `super()`.
        let ctor_value = if let Some(p) = &parent
            && ctor.is_none()
        {
            if !field_props.is_empty() {
                self.current_super = prev_super;
                return Err(CompileError::spanned(
                    class.span(),
                    "a subclass that declares fields must declare a constructor \
                     that calls super()",
                )
                .into());
            }
            self.class_member_read(p, parent_span, crate::abi::CLASS_CONSTRUCTOR_KEY)?
        } else {
            self.lower_lifted_function(&param_pats, BodyKind::Block(&combined_stmts))?
        };

        // Lower each instance method to a first-class function value that
        // reads `this` through the `this`-binding machinery, exactly like the constructor.
        // They are stored ONCE, on the class's single shared prototype object,
        // under the method name — never copied per instance. Pre-slotting the
        // prototype with one entry per method name (the `IrNode::Object` shape)
        // lets `__obj_set` write them and lets `new`'s prototype fallback read
        // them, since the object model has no dynamic key growth.
        let mut proto_entries = Vec::with_capacity(methods.len());
        for m in methods {
            let f = m.function.as_ref();
            let body = f
                .body
                .as_ref()
                .ok_or_else(|| anyhow!("class method requires a body"))?;
            let param_pats: Vec<&Pat> = f.params.iter().map(|p| &p.pat).collect();
            let value = self.lower_lifted_function(&param_pats, BodyKind::Block(&body.stmts))?;
            proto_entries.push((prop_key_name(&m.key)?, value));
        }
        // Instance accessors (P2b): a `get x(){}` / `set x(v){}` pair sharing the
        // property name `x` collapses to ONE `TYPE_ACCESSOR` box stored on the
        // prototype under `x`. Each half is an ordinary lifted function (a getter
        // takes no params; a setter takes one) that reads `this`/`super` exactly
        // like a method, so they are lowered here inside the same `current_super`
        // region. A member read/write that resolves to the box invokes the getter /
        // setter (see `emit_index_node` / `emit_index_set_node`); a missing half is
        // pre-slotted `undefined` (reading a set-only property yields `undefined`;
        // assigning a get-only property is a silent no-op).
        let mut accessor_names: Vec<String> = Vec::new();
        let mut accessor_get: Vec<IrNode> = Vec::new();
        let mut accessor_set: Vec<IrNode> = Vec::new();
        for m in accessors {
            let name = prop_key_name(&m.key)?;
            let f = m.function.as_ref();
            let body = f
                .body
                .as_ref()
                .ok_or_else(|| anyhow!("class accessor requires a body"))?;
            let param_pats: Vec<&Pat> = f.params.iter().map(|p| &p.pat).collect();
            let value = self.lower_lifted_function(&param_pats, BodyKind::Block(&body.stmts))?;
            let idx = if let Some(i) = accessor_names.iter().position(|n| n == &name) {
                i
            } else {
                accessor_names.push(name);
                accessor_get.push(IrNode::Undefined);
                accessor_set.push(IrNode::Undefined);
                accessor_names.len() - 1
            };
            if matches!(m.kind, MethodKind::Getter) {
                accessor_get[idx] = value;
            } else {
                accessor_set[idx] = value;
            }
        }
        for ((name, getter), setter) in accessor_names
            .into_iter()
            .zip(accessor_get)
            .zip(accessor_set)
        {
            proto_entries.push((
                name,
                IrNode::MakeAccessor {
                    getter: Box::new(getter),
                    setter: Box::new(setter),
                },
            ));
        }
        // A derived class links its shared prototype to the parent's prototype under
        // the reserved `@@prototype` key, so `__obj_get`'s prototype-fallback recursion
        // walks the whole chain: an instance's `obj.m` misses the subclass prototype's
        // own entries, then follows this link to resolve an inherited method. This is
        // ALSO the chain `instanceof` walks.
        if let Some(p) = &parent {
            let parent_proto =
                self.class_member_read(p, parent_span, crate::abi::CLASS_PROTOTYPE_KEY)?;
            proto_entries.push((crate::abi::CLASS_PROTOTYPE_KEY.to_owned(), parent_proto));
        }

        // Lower the static members onto the class VALUE itself (not the prototype),
        // so `C.m()` / `C.f` read them directly off the class object. A static method
        // is an ordinary lifted function value called with `this` bound to the class
        // (a plain `C.m()` method call); a static field's initializer is lowered as a
        // plain expression evaluated ONCE at class-definition time (an uninitialized
        // `static f;` stores `undefined`). Static members are NOT inherited by a
        // derived class (the class value has no constructor-side prototype link), and
        // a static initializer that references the class or `this` is a documented gap
        // (the class binding is not yet set while the class value is being built).
        let mut static_entries: Vec<(String, IrNode)> = Vec::new();
        for m in static_methods {
            let f = m.function.as_ref();
            let body = f
                .body
                .as_ref()
                .ok_or_else(|| anyhow!("static class method requires a body"))?;
            let param_pats: Vec<&Pat> = f.params.iter().map(|p| &p.pat).collect();
            let value = self.lower_lifted_function(&param_pats, BodyKind::Block(&body.stmts))?;
            static_entries.push((prop_key_name(&m.key)?, value));
        }
        // A static field whose initializer is a SIMPLE literal (`static x = 1`) is
        // evaluated inline in the class value. Any other initializer may reference
        // the class, an earlier static, or `this` — none of which exist while the
        // class value is being built — so it is PRE-SLOTTED `undefined` here and
        // assigned AFTER the class binding is created (`C.<key> = <init>`), in source
        // declaration order, so a later static observes earlier ones. An
        // uninitialized `static f;` is plain `undefined`.
        let mut deferred_statics: Vec<(String, &Expr)> = Vec::new();
        for p in static_fields {
            let key = prop_key_name(&p.key)?;
            match &p.value {
                Some(v) if matches!(v.as_ref(), Expr::Lit(_)) => {
                    let value = self.lower_expr(v)?;
                    static_entries.push((key, value));
                }
                Some(v) => {
                    static_entries.push((key.clone(), IrNode::Undefined));
                    deferred_statics.push((key, v));
                }
                None => static_entries.push((key, IrNode::Undefined)),
            }
        }

        // Done lowering the constructor and method bodies: restore the enclosing
        // `super` target.
        self.current_super = prev_super;

        // The class value: a heap object holding the constructor function, the single
        // shared prototype object (populated with the instance methods), and any
        // static members appended directly.
        let mut class_entries = vec![
            (crate::abi::CLASS_CONSTRUCTOR_KEY.to_owned(), ctor_value),
            (
                crate::abi::CLASS_PROTOTYPE_KEY.to_owned(),
                IrNode::Object(proto_entries),
            ),
        ];
        class_entries.extend(static_entries);
        let class_value = IrNode::Object(class_entries);

        // Emit the binding (declared above). When the name is captured, allocate the
        // cell empty first and store the class value into it, so the ctor/method
        // closures (built while evaluating `class_value`) capture the already-bound
        // cell pointer — a self-reference like `new C()` inside a method reads it
        // back. When uncaptured, the value binds directly.
        if let Some(slot) = self.module_slot_for_local(local) {
            // A module binding: the module cell is pre-allocated, so the ctor/method
            // bodies already reach it; just store the class value (as a `Decl`, so
            // it never becomes `main`'s completion value).
            out.push(IrStmt::Decl {
                local,
                init: IrNode::CellSet {
                    cell: Box::new(IrNode::ModuleCell(slot)),
                    value: Box::new(class_value),
                },
            });
        } else if self.is_cell(local) {
            out.push(IrStmt::Decl {
                local,
                init: IrNode::CellNew(Box::new(IrNode::Undefined)),
            });
            out.push(IrStmt::Expr(IrNode::CellSet {
                cell: Box::new(IrNode::Local(local)),
                value: Box::new(class_value),
            }));
        } else {
            out.push(IrStmt::Decl {
                local,
                init: class_value,
            });
        }

        // Assign the deferred static-field initializers now that the class binding
        // exists: `C.<key> = <init>`, in source declaration order. `this` inside an
        // initializer resolves to the class (via `static_init_class`), and a `C.<k>`
        // reference reads a static assigned by an earlier line.
        if !deferred_statics.is_empty() {
            self.static_init_class = Some(name.to_owned());
            for (key, init) in deferred_statics {
                let obj = self.lower_ident(name, class_decl.ident.span)?;
                let value = self.lower_expr(init)?;
                out.push(IrStmt::Expr(IrNode::IndexSet {
                    obj: Box::new(obj),
                    index: Box::new(IrNode::Str(key)),
                    value: Box::new(value),
                }));
            }
            self.static_init_class = None;
        }
        Ok(())
    }

    /// Desugar a binding pattern `pat`, bound to the already-lowered `source`
    /// value, into declaration statements appended to `out`. The plain
    /// `Pat::Ident` case binds one local (boxing a captured binding into a cell,
    /// like the fast path in [`Self::lower_var_decl`]); `Pat::Array` evaluates
    /// `source` ONCE into a hidden temp local and binds each element from an
    /// indexed read, handling holes (skipped indices), defaults (`[a = 9]`), a
    /// trailing rest (`[a, ...r]`), and nested patterns (`[[a]]`) by recursion.
    /// `mutable` is the binding mutability (`false` for `const`). Shared by
    /// parameter and object destructuring.
    pub(super) fn lower_binding_pattern(
        &mut self,
        pat: &Pat,
        source: IrNode,
        mutable: bool,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        match pat {
            Pat::Ident(ident) => {
                self.bind_ident(ident, source, mutable, out);
                Ok(())
            }
            Pat::Array(arr) => {
                // Evaluate the initializer ONCE into a hidden temp local, then read
                // each element out of it by index.
                let temp = self.fresh();
                out.push(IrStmt::Decl {
                    local: temp,
                    init: source,
                });
                for (i, elem) in arr.elems.iter().enumerate() {
                    // A hole (`let [, b] = ..`) skips its index.
                    let Some(elem) = elem else { continue };
                    // A trailing rest (`...r`) collects the remaining elements.
                    if let Pat::Rest(rest) = elem {
                        self.lower_array_rest(&rest.arg, temp, i, mutable, out)?;
                        continue;
                    }
                    // A default (`[a = 9]`) binds the default expression only when
                    // the slot reads back `undefined`.
                    let read = || IrNode::Index {
                        obj: Box::new(IrNode::Local(temp)),
                        index: Box::new(index_num(i)),
                    };
                    self.bind_with_default(elem, &read, mutable, out)?;
                }
                Ok(())
            }
            // An object pattern (`let {x, y: b, z = 7, ...rest} = ..`) evaluates
            // `source` ONCE into a hidden temp local, then binds each property from
            // a keyed read (`temp.key`); a trailing rest collects every remaining
            // own key into a new object.
            Pat::Object(obj) => {
                let temp = self.fresh();
                out.push(IrStmt::Decl {
                    local: temp,
                    init: source,
                });
                // The statically-known destructured keys, excluded from a rest.
                let mut bound_keys: Vec<String> = Vec::new();
                for prop in &obj.props {
                    match prop {
                        // `{key: pat}` (or renamed `{key: name}`): read `temp.key`
                        // and bind the value pattern (which may be a nested pattern
                        // or a `pat = default`).
                        ObjectPatProp::KeyValue(kv) => {
                            let key = prop_key_name(&kv.key)?;
                            bound_keys.push(key.clone());
                            let read = || IrNode::Index {
                                obj: Box::new(IrNode::Local(temp)),
                                index: Box::new(IrNode::Str(key.clone())),
                            };
                            self.bind_with_default(&kv.value, &read, mutable, out)?;
                        }
                        // Shorthand `{x}` or shorthand-with-default `{z = 7}`: the
                        // key IS the bound name; a default fills an `undefined` read.
                        ObjectPatProp::Assign(assign) => {
                            let key = assign.key.id.sym.as_ref().to_owned();
                            bound_keys.push(key.clone());
                            let read = || IrNode::Index {
                                obj: Box::new(IrNode::Local(temp)),
                                index: Box::new(IrNode::Str(key.clone())),
                            };
                            let value = match &assign.value {
                                Some(default) => {
                                    let default = self.lower_expr(default)?;
                                    IrNode::Ternary {
                                        cond: Box::new(IrNode::Binary {
                                            op: BinOp::Eq,
                                            lhs: Box::new(read()),
                                            rhs: Box::new(IrNode::Undefined),
                                        }),
                                        then_branch: Box::new(default),
                                        else_branch: Box::new(read()),
                                    }
                                }
                                None => read(),
                            };
                            self.bind_ident(&assign.key, value, mutable, out);
                        }
                        // `{...rest}`: a NEW object with every own key of `temp`
                        // except the destructured ones collected above.
                        ObjectPatProp::Rest(rest) => {
                            let exclude = IrNode::Array(
                                bound_keys.iter().cloned().map(IrNode::Str).collect(),
                            );
                            let value = IrNode::ObjectRest {
                                source: Box::new(IrNode::Local(temp)),
                                exclude: Box::new(exclude),
                            };
                            self.lower_binding_pattern(&rest.arg, value, mutable, out)?;
                        }
                    }
                }
                Ok(())
            }
            other => Err(CompileError::spanned(other.span(), "unsupported binding pattern").into()),
        }
    }

    /// Bind a single identifier `ident` to `value`, boxing a captured binding into
    /// a cell (the `Pat::Ident` fast path shared by [`Self::lower_var_decl`] and the
    /// destructuring desugars). `mutable` is the binding mutability (`false` for
    /// `const`).
    fn bind_ident(
        &mut self,
        ident: &swc_ecma_ast::BindingIdent,
        value: IrNode,
        mutable: bool,
        out: &mut Vec<IrStmt>,
    ) {
        let local = self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident));
        self.push_binding(local, value, out);
    }

    /// Emit the initializing statement for a freshly [`Self::declare`]d `local`
    /// holding `init`. An ordinary binding becomes a `Decl` (boxing a captured
    /// binding into a fresh cell); a generator local (an `env_backed` frame)
    /// instead stores into its pre-existing frame cell via `CellSet`, since
    /// the generator constructor already allocated every frame cell.
    pub(super) fn push_binding(&mut self, local: u32, init: IrNode, out: &mut Vec<IrStmt>) {
        if let Some(slot) = self.env_slot_for_local(local) {
            out.push(IrStmt::Expr(IrNode::CellSet {
                cell: Box::new(IrNode::EnvLoad(slot)),
                value: Box::new(init),
            }));
        } else if let Some(slot) = self.module_slot_for_local(local) {
            // A module binding initializes its pre-allocated module cell. Bound as
            // a `Decl` of its (otherwise unused) local, not an expression
            // statement, so it never becomes `main`'s completion value.
            out.push(IrStmt::Decl {
                local,
                init: IrNode::CellSet {
                    cell: Box::new(IrNode::ModuleCell(slot)),
                    value: Box::new(init),
                },
            });
        } else if self.is_cell(local) {
            out.push(IrStmt::Decl {
                local,
                init: IrNode::CellNew(Box::new(init)),
            });
        } else {
            out.push(IrStmt::Decl { local, init });
        }
    }

    /// Bind a destructuring element/property `target` to the value produced by
    /// `read`, applying a `Pat::Assign` default (`= expr`) when present — the
    /// default binds only when the read yields `undefined`. `read` is called up to
    /// twice (the `undefined` guard plus the value), so it must be side-effect-free
    /// (an indexed read off the hidden temp). Shared by the array-element and
    /// object-property desugars.
    fn bind_with_default(
        &mut self,
        target: &Pat,
        read: &dyn Fn() -> IrNode,
        mutable: bool,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let (target, value) = if let Pat::Assign(assign) = target {
            let default = self.lower_expr(&assign.right)?;
            let value = IrNode::Ternary {
                cond: Box::new(IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(read()),
                    rhs: Box::new(IrNode::Undefined),
                }),
                then_branch: Box::new(default),
                else_branch: Box::new(read()),
            };
            (assign.left.as_ref(), value)
        } else {
            (target, read())
        };
        self.lower_binding_pattern(target, value, mutable, out)
    }

    /// Collect a rest element `...target` from `temp[start..temp.length]` into a
    /// fresh array, then bind `target` to it. Emits a counted runtime loop
    /// (`temp.length` is dynamic) that pushes each remaining element onto a hidden
    /// array temp, mirroring the `for-of` index-loop shape.
    fn lower_array_rest(
        &mut self,
        target: &Pat,
        temp: u32,
        start: usize,
        mutable: bool,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // The rest array grows via `push`, so it must be a plain local.
        let rest = self.fresh();
        out.push(IrStmt::Decl {
            local: rest,
            init: IrNode::Array(Vec::new()),
        });
        let idx = self.fresh();
        out.push(IrStmt::Decl {
            local: idx,
            init: index_num(start),
        });
        let cond = IrNode::Binary {
            op: BinOp::Lt,
            lhs: Box::new(IrNode::Local(idx)),
            rhs: Box::new(IrNode::StrLen(Box::new(IrNode::Local(temp)))),
        };
        let body = vec![IrStmt::Expr(IrNode::ArrayPush {
            local: rest,
            value: Box::new(IrNode::Index {
                obj: Box::new(IrNode::Local(temp)),
                index: Box::new(IrNode::Local(idx)),
            }),
        })];
        let update = vec![IrStmt::Expr(IrNode::Assign {
            local: idx,
            value: Box::new(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(idx)),
                rhs: Box::new(IrNode::Num(1.0)),
            }),
        })];
        out.push(IrStmt::While {
            cond,
            body,
            update,
            label: None,
        });
        self.lower_binding_pattern(target, IrNode::Local(rest), mutable, out)
    }

    /// Lower a `switch (disc) { case E: .. default: .. }` statement. The
    /// discriminant is evaluated once into a fresh temp local; each clause body
    /// is a breakable context, and `break` inside it exits the switch.
    fn lower_switch(
        &mut self,
        switch_stmt: &SwitchStmt,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let disc = self.lower_expr(&switch_stmt.discriminant)?;
        let disc_local = self.fresh();
        self.breakable_inc();
        let mut cases = Vec::with_capacity(switch_stmt.cases.len());
        for case in &switch_stmt.cases {
            let test = match &case.test {
                Some(expr) => Some(self.lower_expr(expr)?),
                None => None,
            };
            let mut body = Vec::new();
            for inner in &case.cons {
                self.lower_stmt(inner, &mut body)?;
            }
            cases.push(IrCase { test, body });
        }
        self.breakable_dec();
        out.push(IrStmt::Switch {
            disc_local,
            disc,
            cases,
        });
        Ok(())
    }

    /// Lower a `try { .. } catch (e) { .. } finally { .. }` statement.
    /// The guarded block, the catch clause, and the finally clause
    /// each open their own lexical scope (block-scoped bindings do not leak),
    /// mirroring [`Stmt::Block`]. The catch parameter binds to a fresh local
    /// initialized from the in-flight exception value: its binding is emitted as
    /// the FIRST statement of the catch body — a `Decl` whose initializer reads
    /// [`IrNode::ExcValue`] (boxed into a cell when the binding is captured by a
    /// nested closure, exactly like a `let`).
    ///
    /// All four legal shapes lower here: `try`/`catch`, `try`/`finally`,
    /// `try`/`catch`/`finally`. The absent clause becomes `None` on
    /// [`IrStmt::Try`] (`catch_body` `None` = no `catch`, `finally_body` `None` =
    /// no `finally`). A `try` with neither `catch` nor `finally` is impossible —
    /// the parser never produces one — and is rejected defensively.
    fn lower_try(
        &mut self,
        try_stmt: &swc_ecma_ast::TryStmt,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        if try_stmt.handler.is_none() && try_stmt.finalizer.is_none() {
            // A `try` block always has at least a `catch` or a `finally`.
            return Err(anyhow!("try without catch or finally"));
        }

        // The completion value (for `-p`): a top-level `try` in `main` opens a
        // completion local that every expression statement of its block and catch
        // clause stores into; it is reset to `undefined` on entry (an empty `try`
        // yields `undefined`) and pushed as `main`'s value after the statement.
        let outermost =
            self.main_ctx && self.frames.len() == 1 && self.shared.completion.get().is_none();
        if outermost {
            let local = self.fresh();
            self.shared.completion.set(Some(local));
        }
        self.reset_completion(out);
        let result = self.lower_try_clauses(try_stmt, out);
        if outermost
            && let Some(local) = self.shared.completion.take()
            && result.is_ok()
        {
            out.push(IrStmt::Expr(IrNode::Local(local)));
        }
        result
    }

    /// Lower the clauses of a `try` statement (see [`Self::lower_try`]) and push
    /// the resulting [`IrStmt::Try`].
    fn lower_try_clauses(
        &mut self,
        try_stmt: &swc_ecma_ast::TryStmt,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // The guarded block: its own lexical scope, statements lowered inline
        // (blocks share the function's wasm locals, like `Stmt::Block`).
        self.push_scope();
        let mut body = Vec::new();
        let body_result = self.lower_stmts(&try_stmt.block.stmts, &mut body);
        self.pop_scope();
        body_result?;

        // The catch clause (when present): a fresh scope holding the (optional)
        // catch parameter, bound from the thrown value before the handler runs.
        let catch_body = match &try_stmt.handler {
            Some(handler) => {
                self.push_scope();
                let mut catch_body = Vec::new();
                let catch_result = (|| -> anyhow::Result<()> {
                    if let Some(param) = &handler.param {
                        let Pat::Ident(ident) = param else {
                            return Err(CompileError::spanned(
                                param.span(),
                                "unsupported catch parameter pattern",
                            )
                            .into());
                        };
                        let local = self.declare(ident.id.sym.as_ref(), true, binding_id(ident));
                        // The binding value is the in-flight exception; box it when
                        // the parameter is captured (mirrors a captured `let`).
                        self.push_binding(local, IrNode::ExcValue, &mut catch_body);
                    }
                    // The catch clause's value replaces the guarded block's.
                    self.reset_completion(&mut catch_body);
                    self.lower_stmts(&handler.body.stmts, &mut catch_body)
                })();
                self.pop_scope();
                catch_result?;
                Some(catch_body)
            }
            None => None,
        };

        // The finally clause (when present): its own lexical scope, like the body.
        // Its value is discarded, so completion tracking is suspended inside it.
        let finally_body = match &try_stmt.finalizer {
            Some(block) => {
                self.push_scope();
                let completion = self.shared.completion.take();
                let mut finally_body = Vec::new();
                let finally_result = self.lower_stmts(&block.stmts, &mut finally_body);
                self.shared.completion.set(completion);
                self.pop_scope();
                finally_result?;
                Some(finally_body)
            }
            None => None,
        };

        out.push(IrStmt::Try {
            body,
            catch_body,
            finally_body,
        });
        Ok(())
    }

    /// The `main` local the current expression statement's completion value is
    /// stored into: set only inside a top-level `try` (see [`Self::lower_try`])
    /// and only in `main`'s own frame — a nested function body never writes it.
    fn completion_local(&self) -> Option<u32> {
        if self.main_ctx && self.frames.len() == 1 {
            self.shared.completion.get()
        } else {
            None
        }
    }

    /// Reset the tracked completion value to `undefined` (a no-op when untracked).
    fn reset_completion(&self, out: &mut Vec<IrStmt>) {
        if let Some(local) = self.completion_local() {
            out.push(IrStmt::Expr(IrNode::Assign {
                local,
                value: Box::new(IrNode::Undefined),
            }));
        }
    }

    /// Lower a `while (cond) body`. `label`, when `Some`, is the loop id that a
    /// labeled `break`/`continue` targeting this loop carries (set by
    /// [`Self::lower_labeled`]); an unlabeled `while` passes `None`.
    fn lower_while(
        &mut self,
        while_stmt: &swc_ecma_ast::WhileStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let cond = self.lower_expr(&while_stmt.test)?;
        self.breakable_inc();
        self.loopable_inc();
        let body = self.lower_body(&while_stmt.body)?;
        self.loopable_dec();
        self.breakable_dec();
        out.push(IrStmt::While {
            cond,
            body,
            update: Vec::new(),
            label,
        });
        Ok(())
    }

    /// Lower a `do body while (cond)`, desugared to a `while` that always runs its
    /// body at least once. A fresh `_first` flag local is set truthy before the
    /// loop and cleared as the FIRST statement of the body, so the condition
    /// `_first || cond` enters unconditionally the first time and re-tests `cond`
    /// on every later iteration — including after a `continue`, which jumps to the
    /// condition once the flag is already cleared. `label` threads a labeled
    /// `break`/`continue` target as for [`Self::lower_while`].
    fn lower_do_while(
        &mut self,
        do_stmt: &swc_ecma_ast::DoWhileStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // `_first` is truthy only on entry; the body clears it, so `cond` (which
        // sits at the top of the desugared `while`) is skipped exactly once.
        let first = self.fresh();
        out.push(IrStmt::Decl {
            local: first,
            init: IrNode::Bool(true),
        });
        // `cond` is lowered in the loop's own scope (not the body's), matching JS:
        // the test cannot see the body's block-scoped bindings.
        let cond = IrNode::Logical {
            op: LogicalOp::Or,
            lhs: Box::new(IrNode::Local(first)),
            rhs: Box::new(self.lower_expr(&do_stmt.test)?),
        };
        self.breakable_inc();
        self.loopable_inc();
        let mut body = vec![IrStmt::Expr(IrNode::Assign {
            local: first,
            value: Box::new(IrNode::Bool(false)),
        })];
        self.lower_stmt(&do_stmt.body, &mut body)?;
        self.loopable_dec();
        self.breakable_dec();
        out.push(IrStmt::While {
            cond,
            body,
            update: Vec::new(),
            label,
        });
        Ok(())
    }

    /// Lower a labeled statement `name: loop`. A label names its immediately
    /// nested loop, and `break name`/`continue name` inside that loop (or a loop
    /// nested within it) target THIS loop. The label is in scope only while its
    /// loop body is lowered. A label on any non-loop statement is a spanned error.
    fn lower_labeled(
        &mut self,
        labeled: &swc_ecma_ast::LabeledStmt,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let name = labeled.label.sym.as_ref();
        let id = self.fresh_label();
        self.with_label(name, id, |ctx| match labeled.body.as_ref() {
            Stmt::While(while_stmt) => ctx.lower_while(while_stmt, Some(id), out),
            Stmt::DoWhile(do_stmt) => ctx.lower_do_while(do_stmt, Some(id), out),
            Stmt::For(for_stmt) => ctx.lower_for(for_stmt, Some(id), out),
            Stmt::ForOf(for_of) => ctx.lower_for_of(for_of, Some(id), out),
            Stmt::ForIn(for_in) => ctx.lower_for_in(for_in, Some(id), out),
            other => Err(CompileError::spanned(
                other.span(),
                "a label may only be applied to a loop",
            )
            .into()),
        })
    }

    /// Resolve the target of a `break`/`continue`. A `Some(label)` resolves to its
    /// loop id via [`Self::label_id`] (an unknown label is a spanned error); an
    /// unlabeled `break`/`continue` yields `None` after checking it is inside a
    /// valid enclosing context (`is_continue` selects the loop-only `continue`
    /// check and its message, `span` locates the diagnostic).
    fn loop_target(
        &self,
        label: Option<&swc_ecma_ast::Ident>,
        span: swc_common::Span,
        is_continue: bool,
    ) -> anyhow::Result<Option<u32>> {
        if let Some(label) = label {
            return self
                .label_id(label.sym.as_ref())
                .map(Some)
                .ok_or_else(|| CompileError::spanned(label.span, "undefined label").into());
        }
        let (in_context, msg) = if is_continue {
            (self.loopable() > 0, "illegal continue statement")
        } else {
            (self.breakable() > 0, "illegal break statement")
        };
        if in_context {
            Ok(None)
        } else {
            Err(CompileError::spanned(span, msg).into())
        }
    }

    /// Lower a C-style `for (init; cond; update) body`, desugared to a `while`
    /// whose `update` slot carries the update expression (so a `continue` still
    /// advances the loop variable — see [`IrStmt::While`]). An empty `cond`
    /// defaults to `true`. The `for` head opens a lexical scope, so a `let`/`const` init
    /// binding is block-scoped to the loop and is not visible after it
    /// (referencing it there is a compile error).
    ///
    /// Each iteration gets its own copy of a captured `let`/`const` binding
    /// (CreatePerIterationEnvironment): the binding's cell is replaced by a fresh
    /// cell holding the current value once after the init and again at the start
    /// of the `update` slot, before the update expression. Closures created in an
    /// iteration keep that iteration's cell, while the update (also run on
    /// `continue`) advances the next iteration's copy. Uncaptured bindings are
    /// plain locals and need no copy.
    fn lower_for(
        &mut self,
        for_stmt: &swc_ecma_ast::ForStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        self.push_scope();
        let result = (|| {
            let mut per_iteration = Vec::new();
            if let Some(init) = &for_stmt.init {
                match init {
                    VarDeclOrExpr::VarDecl(var_decl) => {
                        self.lower_stmt(&Stmt::Decl(Decl::Var(var_decl.clone())), out)?;
                        if var_decl.kind != VarDeclKind::Var {
                            per_iteration = self.innermost_cell_locals();
                        }
                    }
                    VarDeclOrExpr::Expr(expr) => {
                        out.push(IrStmt::Expr(self.lower_expr(expr)?));
                    }
                }
            }
            let cond = match &for_stmt.test {
                Some(test) => self.lower_expr(test)?,
                None => IrNode::Bool(true),
            };
            self.breakable_inc();
            self.loopable_inc();
            let body = self.lower_body(&for_stmt.body)?;
            self.loopable_dec();
            self.breakable_dec();
            // The update runs after the body AND on every `continue`, so it lives
            // in the loop's `update` slot rather than being appended to `body`.
            let copy = |local: u32| {
                IrStmt::Expr(IrNode::Assign {
                    local,
                    value: Box::new(IrNode::CellNew(Box::new(IrNode::CellGet(Box::new(
                        IrNode::Local(local),
                    ))))),
                })
            };
            out.extend(per_iteration.iter().copied().map(copy));
            let mut update: Vec<IrStmt> = per_iteration.iter().copied().map(copy).collect();
            if let Some(expr) = &for_stmt.update {
                update.push(IrStmt::Expr(self.lower_expr(expr)?));
            }
            out.push(IrStmt::While {
                cond,
                body,
                update,
                label,
            });
            Ok(())
        })();
        self.pop_scope();
        result
    }

    /// Lower a `for (const v of ITER) body` over an array or string, desugared to
    /// a counted index loop lowered to [`IrStmt::While`]. `ITER` is evaluated
    /// ONCE into a temp local, its `.length` cached ONCE into another temp, and an
    /// index counter walks `0..length`. Caching the length matches the C-style
    /// counted-loop desugar: for the array/string iterables supported here the
    /// length is fixed for the loop's duration, so re-reading it each iteration
    /// (as JS `for-of` semantically does) would observe no difference.
    ///
    /// The loop binding is a fresh block-scoped local rebound each iteration:
    /// `iter[i]` is assigned into it as the FIRST statement of the body (so a
    /// captured binding gets a fresh cell per iteration, like a `let`). The index
    /// increment lives in the loop's `update` slot so a `continue` still advances
    /// it (mirroring [`Self::lower_for`]). The `for-of` head opens a lexical
    /// scope, so the binding is not visible after the loop.
    ///
    /// Only `const`/`let` single-identifier heads over arrays/strings are
    /// supported; a destructuring/assignment head, `for await`, or `using` head is
    /// a spanned error.
    fn lower_for_of(
        &mut self,
        for_of: &swc_ecma_ast::ForOfStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        if for_of.is_await {
            return Err(
                CompileError::spanned(for_of.span(), "for-await-of is not supported").into(),
            );
        }
        // The head must declare a single `const`/`let` identifier binding.
        let (ident, mutable) = match &for_of.left {
            ForHead::VarDecl(var_decl) => {
                let [declarator] = var_decl.decls.as_slice() else {
                    return Err(CompileError::spanned(
                        var_decl.span(),
                        "for-of requires a single loop variable",
                    )
                    .into());
                };
                let Pat::Ident(ident) = &declarator.name else {
                    return Err(CompileError::spanned(
                        declarator.name.span(),
                        "unsupported for-of binding pattern",
                    )
                    .into());
                };
                (ident, var_decl.kind != VarDeclKind::Const)
            }
            other => {
                return Err(CompileError::spanned(
                    other.span(),
                    "for-of requires a `const`/`let` loop variable",
                )
                .into());
            }
        };

        // Reject a statically-known non-iterable head (`for (const x of 5)`, `of
        // {}`): only arrays and strings are supported here. Dynamic values
        // (identifiers, calls) carry no static type in the numeric IR, so a
        // non-iterable reaching the loop at runtime is undefined behavior, like an
        // out-of-bounds index elsewhere.
        if non_iterable_literal(&for_of.right) {
            return Err(CompileError::spanned(
                for_of.right.span(),
                "for-of iterable must be an array or string",
            )
            .into());
        }

        self.push_scope();
        let result = (|| {
            // Evaluate the iterable ONCE into a temp. At runtime it is either a
            // GENERATOR (driven via the iterator protocol) or an
            // array/string (the ordinary index walk); a runtime `IsGenerator` test
            // selects between the two — the index branch is unchanged.
            let iter = self.lower_expr(&for_of.right)?;
            let iter_local = self.fresh();
            out.push(IrStmt::Decl {
                local: iter_local,
                init: iter,
            });
            let gen_body =
                self.lower_for_of_generator(ident, mutable, iter_local, &for_of.body, label)?;
            let index_body =
                self.lower_for_of_index(ident, mutable, iter_local, &for_of.body, label)?;
            out.push(IrStmt::If {
                cond: IrNode::IsGenerator(Box::new(IrNode::Local(iter_local))),
                then_body: gen_body,
                else_body: index_body,
            });
            Ok(())
        })();
        self.pop_scope();
        result
    }

    /// Lower the ARRAY/STRING index branch of a `for-of` (the original loop): cache
    /// the length once, walk `0 <= i < length`, and bind the loop variable to
    /// `iter[i]` as the first body statement, with `i++` in the loop's `update` slot
    /// so a `continue` still advances it. Returns the branch statements.
    fn lower_for_of_index(
        &mut self,
        ident: &swc_ecma_ast::BindingIdent,
        mutable: bool,
        iter_local: u32,
        for_body: &Stmt,
        label: Option<u32>,
    ) -> anyhow::Result<Vec<IrStmt>> {
        let mut out = Vec::new();
        let len_local = self.fresh();
        out.push(IrStmt::Decl {
            local: len_local,
            init: IrNode::StrLen(Box::new(IrNode::Local(iter_local))),
        });
        let idx_local = self.fresh();
        out.push(IrStmt::Decl {
            local: idx_local,
            init: IrNode::Num(0.0),
        });
        let cond = IrNode::Binary {
            op: BinOp::Lt,
            lhs: Box::new(IrNode::Local(idx_local)),
            rhs: Box::new(IrNode::Local(len_local)),
        };
        let local = self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident));
        self.breakable_inc();
        self.loopable_inc();
        let mut body = Vec::new();
        let elem = IrNode::Index {
            obj: Box::new(IrNode::Local(iter_local)),
            index: Box::new(IrNode::Local(idx_local)),
        };
        self.push_binding(local, elem, &mut body);
        self.lower_stmt(for_body, &mut body)?;
        self.loopable_dec();
        self.breakable_dec();
        let update = vec![IrStmt::Expr(IrNode::Assign {
            local: idx_local,
            value: Box::new(IrNode::Binary {
                op: BinOp::Add,
                lhs: Box::new(IrNode::Local(idx_local)),
                rhs: Box::new(IrNode::Num(1.0)),
            }),
        })];
        out.push(IrStmt::While {
            cond,
            body,
            update,
            label,
        });
        Ok(out)
    }

    /// Lower the GENERATOR-drive branch of a `for-of`: consume the handle
    /// via the iterator protocol —
    ///
    /// ```text
    /// let $r = iter.next();
    /// while (true) { if ($r.done) break; const x = $r.value; BODY; }   // update: $r = iter.next()
    /// ```
    ///
    /// — with `$r` refreshed in the loop's `update` slot so a `continue` advances it.
    fn lower_for_of_generator(
        &mut self,
        ident: &swc_ecma_ast::BindingIdent,
        mutable: bool,
        iter_local: u32,
        for_body: &Stmt,
        label: Option<u32>,
    ) -> anyhow::Result<Vec<IrStmt>> {
        let next = || IrNode::GenResume {
            handle: Box::new(IrNode::Local(iter_local)),
            sent: Box::new(IrNode::Undefined),
        };
        let member = |r: u32, key: &str| IrNode::Index {
            obj: Box::new(IrNode::Local(r)),
            index: Box::new(IrNode::Str(key.to_owned())),
        };
        let mut out = Vec::new();
        let r_local = self.fresh();
        out.push(IrStmt::Decl {
            local: r_local,
            init: next(),
        });
        let local = self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident));
        self.breakable_inc();
        self.loopable_inc();
        let mut body = Vec::new();
        body.push(IrStmt::If {
            cond: member(r_local, "done"),
            then_body: vec![IrStmt::Break(None)],
            else_body: Vec::new(),
        });
        self.push_binding(local, member(r_local, "value"), &mut body);
        self.lower_stmt(for_body, &mut body)?;
        self.loopable_dec();
        self.breakable_dec();
        out.push(IrStmt::While {
            cond: IrNode::Bool(true),
            body,
            update: vec![IrStmt::Expr(IrNode::Assign {
                local: r_local,
                value: Box::new(next()),
            })],
            label,
        });
        Ok(out)
    }

    /// Lower a `for (const k in EXPR) body` over an object's own enumerable keys,
    /// desugared to a counted index loop lowered to [`IrStmt::While`] — the
    /// same shape as [`Self::lower_for_of`], but the loop binding is each KEY (a
    /// heap STRING) rather than each element. `EXPR` is evaluated ONCE into a temp
    /// local, its key count cached ONCE ([`IrNode::StrLen`] reads the u32 header,
    /// which is the entry count for an object and the length for an array/string),
    /// and an index counter walks `0..count`. Each iteration binds a fresh
    /// block-scoped local to [`IrNode::ObjectKey`] at the current index — the key
    /// heap-string for an object (insertion order), or the decimal index string
    /// (`"0"`, `"1"`, ..) for an array/string — as the FIRST body statement (so a
    /// captured binding gets a fresh cell per iteration, like a `let`). The index
    /// increment lives in the loop's `update` slot so a `continue` still advances it
    /// (mirroring [`Self::lower_for`]); the `for-in` head opens a lexical scope, so
    /// the binding is not visible after the loop.
    ///
    /// Only a single `const`/`let`/`var` identifier head is supported; a
    /// destructuring/assignment head is a spanned error. A statically-known
    /// non-enumerable operand (a number/boolean/null literal) is rejected with a
    /// span; objects, arrays, strings, and any dynamic value are allowed through.
    fn lower_for_in(
        &mut self,
        for_in: &swc_ecma_ast::ForInStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // The head must declare a single `const`/`let`/`var` identifier binding.
        let (ident, mutable) = match &for_in.left {
            ForHead::VarDecl(var_decl) => {
                let [declarator] = var_decl.decls.as_slice() else {
                    return Err(CompileError::spanned(
                        var_decl.span(),
                        "for-in requires a single loop variable",
                    )
                    .into());
                };
                let Pat::Ident(ident) = &declarator.name else {
                    return Err(CompileError::spanned(
                        declarator.name.span(),
                        "unsupported for-in binding pattern",
                    )
                    .into());
                };
                (ident, var_decl.kind != VarDeclKind::Const)
            }
            other => {
                return Err(CompileError::spanned(
                    other.span(),
                    "for-in requires a `const`/`let`/`var` loop variable",
                )
                .into());
            }
        };

        // Reject a statically-known non-enumerable head (`for (const k in 5)`):
        // only objects, arrays, and strings enumerate keys. Dynamic values carry no
        // static type in the numeric IR, so a non-enumerable reaching the loop at
        // runtime is undefined behavior, like an out-of-bounds index elsewhere.
        if non_enumerable_literal(&for_in.right) {
            return Err(CompileError::spanned(
                for_in.right.span(),
                "for-in operand must be an object, array, or string",
            )
            .into());
        }

        self.push_scope();
        let result = (|| {
            // Evaluate the operand ONCE into a temp, then cache its key count ONCE.
            let iter = self.lower_expr(&for_in.right)?;
            let iter_local = self.fresh();
            out.push(IrStmt::Decl {
                local: iter_local,
                init: iter,
            });
            let len_local = self.fresh();
            out.push(IrStmt::Decl {
                local: len_local,
                init: IrNode::StrLen(Box::new(IrNode::Local(iter_local))),
            });
            // The index counter, walking `0 <= i < count`.
            let idx_local = self.fresh();
            out.push(IrStmt::Decl {
                local: idx_local,
                init: IrNode::Num(0.0),
            });
            let cond = IrNode::Binary {
                op: BinOp::Lt,
                lhs: Box::new(IrNode::Local(idx_local)),
                rhs: Box::new(IrNode::Local(len_local)),
            };

            // The loop binding: a fresh block-scoped local, rebound each iteration
            // to the key at index `i` as the first statement of the body.
            let local = self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident));
            self.breakable_inc();
            self.loopable_inc();
            let mut body = Vec::new();
            let key = IrNode::ObjectKey {
                obj: Box::new(IrNode::Local(iter_local)),
                index: Box::new(IrNode::Local(idx_local)),
            };
            self.push_binding(local, key, &mut body);
            self.lower_stmt(&for_in.body, &mut body)?;
            self.loopable_dec();
            self.breakable_dec();

            // `i = i + 1` in the `update` slot so a `continue` still advances it.
            let update = vec![IrStmt::Expr(IrNode::Assign {
                local: idx_local,
                value: Box::new(IrNode::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(IrNode::Local(idx_local)),
                    rhs: Box::new(IrNode::Num(1.0)),
                }),
            })];
            out.push(IrStmt::While {
                cond,
                body,
                update,
                label,
            });
            Ok(())
        })();
        self.pop_scope();
        result
    }

    /// Lower a control-flow body (an `if`/`while` branch) into its own statement
    /// sequence. A `{ .. }` block flattens into its inner statements; any other
    /// single statement becomes a one-element sequence. Locals stay shared with
    /// the enclosing function.
    fn lower_body(&mut self, stmt: &Stmt) -> anyhow::Result<Vec<IrStmt>> {
        let mut body = Vec::new();
        self.lower_stmt(stmt, &mut body)?;
        Ok(body)
    }
}

/// Whether `expr` is a compile-time literal that is definitely NOT a string or
/// array — the only iterables P0.3 supports. An object literal and every
/// non-string literal (number/boolean/null/…) qualify; a string literal and a
/// template literal are strings, and anything dynamic (an identifier, a call, an
/// array literal) is allowed through. Used to reject an obviously non-iterable
/// `for-of` head with a spanned error.
fn non_iterable_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(paren) => non_iterable_literal(&paren.expr),
        Expr::Object(_) => true,
        Expr::Lit(lit) => !matches!(lit, Lit::Str(_)),
        _ => false,
    }
}

/// Whether `expr` is a compile-time literal that is definitely NOT enumerable by
/// `for-in` — a number/boolean/null/bigint/regex literal. An object enumerates its
/// keys, an array/string its index keys, and anything dynamic (an identifier, a
/// call, an object/array literal) is allowed through; only a primitive-scalar
/// literal is a statically-detectable non-enumerable operand. Used to reject an
/// obviously wrong `for-in` head (`for (const k in 5)`) with a spanned error.
fn non_enumerable_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Paren(paren) => non_enumerable_literal(&paren.expr),
        // A string literal enumerates its index keys ("0", "1", ..), like an array.
        Expr::Lit(lit) => !matches!(lit, Lit::Str(_)),
        _ => false,
    }
}

/// Extract the static string name of an object-pattern property key. Only
/// identifier and string-literal keys are supported (mirroring the object-literal
/// keys in [`LowerCtx::lower_object`]); a computed or numeric key is a spanned
/// error.
fn prop_key_name(key: &PropName) -> anyhow::Result<String> {
    match key {
        PropName::Ident(id) => Ok(id.sym.as_ref().to_owned()),
        PropName::Str(s) => Ok(s.value.to_string_lossy().into_owned()),
        _ => Err(CompileError::spanned(
            key.span(),
            "unsupported object-pattern key: only identifier and string keys are supported \
             (computed and numeric keys are not)",
        )
        .into()),
    }
}

/// A non-negative array index `i` as an [`IrNode::Num`], converted losslessly
/// (avoiding a precision-losing `as f64` cast).
fn index_num(i: usize) -> IrNode {
    IrNode::Num(f64::from(u32::try_from(i).unwrap_or(u32::MAX)))
}

/// Reject an out-of-scope class method form WITH a span. Plain instance
/// methods and `static` methods are supported; getters/setters, computed names,
/// and async/generator methods are not (static or not). An INSTANCE getter/setter
/// is peeled off before this call (P2b — see [`reject_out_of_scope_accessor`]), so
/// a getter/setter reaching here is a still-unsupported STATIC accessor. Returns
/// `Ok(())` for a supported method.
fn reject_out_of_scope_method(m: &ClassMethod) -> anyhow::Result<()> {
    let msg = if matches!(m.kind, MethodKind::Getter) {
        "static class getter properties are not supported"
    } else if matches!(m.kind, MethodKind::Setter) {
        "static class setter properties are not supported"
    } else if matches!(m.key, PropName::Computed(_)) {
        "computed class method names are not supported"
    } else if m.function.is_async {
        "async class methods are not supported"
    } else if m.function.is_generator {
        "generator class methods are not supported"
    } else {
        return Ok(());
    };
    Err(CompileError::spanned(m.span(), msg).into())
}

/// Reject an out-of-scope INSTANCE accessor form WITH a span (P2b). A plain
/// identifier/string-named `get x(){}` / `set x(v){}` is supported; a COMPUTED
/// accessor name (`get [k](){}`) is not. (A getter/setter is never async or a
/// generator in valid syntax, so no such check is needed.) Returns `Ok(())` for a
/// supported instance accessor.
fn reject_out_of_scope_accessor(m: &ClassMethod) -> anyhow::Result<()> {
    if matches!(m.key, PropName::Computed(_)) {
        return Err(CompileError::spanned(
            m.span(),
            "computed class accessor names are not supported",
        )
        .into());
    }
    Ok(())
}

/// Build the spanned rejection for a non-method, non-constructor, non-field class
/// member. Private / static-block forms are out of scope entirely. Each carries
/// the member's span. (Methods and plain instance fields are handled in
/// [`LowerCtx::lower_class_decl`] directly — supported or rejected there.)
fn reject_class_member(member: &ClassMember) -> anyhow::Error {
    let (span, msg) = match member {
        ClassMember::PrivateMethod(m) => (m.span(), "private class methods are not supported"),
        ClassMember::PrivateProp(p) => (p.span(), "private class fields are not supported"),
        ClassMember::StaticBlock(b) => (b.span(), "class static blocks are not supported"),
        other => (other.span(), "unsupported class member"),
    };
    CompileError::spanned(span, msg).into()
}

/// Reject an out-of-scope class-field form WITH a span. Plain instance
/// fields (`f = init` / `f;`) and `static` fields are supported; computed field
/// names are not (static or not). (Private fields arrive as
/// [`ClassMember::PrivateProp`] and are rejected by [`reject_class_member`].)
/// Returns `Ok(())` for a supported field.
fn reject_out_of_scope_field(p: &ClassProp) -> anyhow::Result<()> {
    let msg = if matches!(p.key, PropName::Computed(_)) {
        "computed class field names are not supported"
    } else {
        return Ok(());
    };
    Err(CompileError::spanned(p.span(), msg).into())
}

/// Desugar an instance field declaration `f = init` (or an uninitialized `f;`)
/// into the synthetic statement `this.f = <init>` (`this.f = undefined` when the
/// field carries no initializer). These synthetic statements are prepended to the
/// constructor body so field initializers run — in class-body source order —
/// BEFORE the constructor body, and reuse the ordinary `this.<field> = ..`
/// lowering (member write in the constructor's `this` context) and field-shape
/// scan. The field name is extracted via [`prop_key_name`] (identifier/string
/// keys; a computed name is already rejected upstream by
/// [`reject_out_of_scope_field`], a numeric key is rejected here).
fn field_init_stmt(prop: &ClassProp) -> anyhow::Result<Stmt> {
    let span = prop.span();
    let name = prop_key_name(&prop.key)?;
    // An uninitialized field (`f;`) initializes to `undefined`.
    let value: Box<Expr> = match &prop.value {
        Some(v) => v.clone(),
        None => Box::new(Expr::Ident(Ident {
            span,
            ctxt: SyntaxContext::empty(),
            sym: "undefined".into(),
            optional: false,
        })),
    };
    let target = MemberExpr {
        span,
        obj: Box::new(Expr::This(ThisExpr { span })),
        prop: MemberProp::Ident(IdentName {
            span,
            sym: name.into(),
        }),
    };
    Ok(Stmt::Expr(ExprStmt {
        span,
        expr: Box::new(Expr::Assign(AssignExpr {
            span,
            op: AssignOp::Assign,
            left: AssignTarget::Simple(SimpleAssignTarget::Member(target)),
            right: value,
        })),
    }))
}

/// Collect the instance field names a constructor assigns via `this.<name> = ..`,
/// in first-seen order with duplicates removed. The object model cannot
/// grow keys at runtime, so `new` must pre-slot exactly these fields on the fresh
/// instance (a superset is harmless — surplus slots read back `undefined`, which
/// is the correct JS value for an unset field). The scan walks the constructor's
/// statements and expressions but does NOT descend into nested `function`/arrow
/// bodies: a nested `function` rebinds `this`, and an arrow-captured `this.f = ..`
/// running later is a documented gap.
fn collect_instance_fields(stmts: &[Stmt]) -> Vec<String> {
    let mut fields = Vec::new();
    for stmt in stmts {
        collect_fields_stmt(stmt, &mut fields);
    }
    fields
}

/// Record a `this.<name> = ..` assignment target's field name (deduplicated).
fn record_this_field(target: &AssignTarget, fields: &mut Vec<String>) {
    let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = target else {
        return;
    };
    if !matches!(m.obj.as_ref(), Expr::This(_)) {
        return;
    }
    let name = match &m.prop {
        MemberProp::Ident(id) => Some(id.sym.as_ref().to_owned()),
        MemberProp::Computed(c) => match c.expr.as_ref() {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string_lossy().into_owned()),
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    };
    if let Some(name) = name
        && !fields.contains(&name)
    {
        fields.push(name);
    }
}

/// Walk a statement for `this.<name> = ..` field assignments (see
/// [`collect_instance_fields`]); recurses through control flow but not into
/// nested function/arrow bodies.
fn collect_fields_stmt(stmt: &Stmt, fields: &mut Vec<String>) {
    match stmt {
        Stmt::Expr(e) => collect_fields_expr(&e.expr, fields),
        Stmt::Decl(Decl::Var(v)) => {
            for d in &v.decls {
                if let Some(init) = &d.init {
                    collect_fields_expr(init, fields);
                }
            }
        }
        Stmt::Return(r) => {
            if let Some(e) = &r.arg {
                collect_fields_expr(e, fields);
            }
        }
        Stmt::Throw(t) => collect_fields_expr(&t.arg, fields),
        Stmt::If(i) => {
            collect_fields_expr(&i.test, fields);
            collect_fields_stmt(&i.cons, fields);
            if let Some(alt) = &i.alt {
                collect_fields_stmt(alt, fields);
            }
        }
        Stmt::While(w) => {
            collect_fields_expr(&w.test, fields);
            collect_fields_stmt(&w.body, fields);
        }
        Stmt::DoWhile(w) => {
            collect_fields_expr(&w.test, fields);
            collect_fields_stmt(&w.body, fields);
        }
        Stmt::For(f) => {
            if let Some(VarDeclOrExpr::Expr(e)) = &f.init {
                collect_fields_expr(e, fields);
            } else if let Some(VarDeclOrExpr::VarDecl(v)) = &f.init {
                for d in &v.decls {
                    if let Some(init) = &d.init {
                        collect_fields_expr(init, fields);
                    }
                }
            }
            if let Some(t) = &f.test {
                collect_fields_expr(t, fields);
            }
            if let Some(u) = &f.update {
                collect_fields_expr(u, fields);
            }
            collect_fields_stmt(&f.body, fields);
        }
        Stmt::ForOf(f) => {
            collect_fields_expr(&f.right, fields);
            collect_fields_stmt(&f.body, fields);
        }
        Stmt::ForIn(f) => {
            collect_fields_expr(&f.right, fields);
            collect_fields_stmt(&f.body, fields);
        }
        Stmt::Block(b) => {
            for s in &b.stmts {
                collect_fields_stmt(s, fields);
            }
        }
        Stmt::Switch(s) => {
            collect_fields_expr(&s.discriminant, fields);
            for case in &s.cases {
                if let Some(t) = &case.test {
                    collect_fields_expr(t, fields);
                }
                for s in &case.cons {
                    collect_fields_stmt(s, fields);
                }
            }
        }
        Stmt::Try(t) => {
            for s in &t.block.stmts {
                collect_fields_stmt(s, fields);
            }
            if let Some(h) = &t.handler {
                for s in &h.body.stmts {
                    collect_fields_stmt(s, fields);
                }
            }
            if let Some(f) = &t.finalizer {
                for s in &f.stmts {
                    collect_fields_stmt(s, fields);
                }
            }
        }
        _ => {}
    }
}

/// Walk an expression for `this.<name> = ..` field assignments (see
/// [`collect_instance_fields`]); does not descend into nested function/arrow
/// bodies (those rebind or merely capture `this`).
fn collect_fields_expr(expr: &Expr, fields: &mut Vec<String>) {
    match expr {
        Expr::Assign(a) => {
            record_this_field(&a.left, fields);
            collect_fields_expr(&a.right, fields);
        }
        Expr::Paren(p) => collect_fields_expr(&p.expr, fields),
        Expr::Bin(b) => {
            collect_fields_expr(&b.left, fields);
            collect_fields_expr(&b.right, fields);
        }
        Expr::Unary(u) => collect_fields_expr(&u.arg, fields),
        Expr::Update(u) => collect_fields_expr(&u.arg, fields),
        Expr::Cond(c) => {
            collect_fields_expr(&c.test, fields);
            collect_fields_expr(&c.cons, fields);
            collect_fields_expr(&c.alt, fields);
        }
        Expr::Seq(s) => {
            for e in &s.exprs {
                collect_fields_expr(e, fields);
            }
        }
        Expr::Call(c) => {
            if let Callee::Expr(e) = &c.callee {
                collect_fields_expr(e, fields);
            }
            for arg in &c.args {
                collect_fields_expr(&arg.expr, fields);
            }
        }
        Expr::Member(m) => {
            collect_fields_expr(&m.obj, fields);
            if let MemberProp::Computed(c) = &m.prop {
                collect_fields_expr(&c.expr, fields);
            }
        }
        Expr::Array(a) => {
            for e in a.elems.iter().flatten() {
                collect_fields_expr(&e.expr, fields);
            }
        }
        Expr::Object(o) => {
            for prop in &o.props {
                if let PropOrSpread::Prop(prop) = prop
                    && let Prop::KeyValue(kv) = prop.as_ref()
                {
                    collect_fields_expr(&kv.value, fields);
                }
            }
        }
        _ => {}
    }
}

/// A plain-English description of an unsupported statement, used for the
/// user-facing diagnostic message. Never renders the raw swc AST.
fn unsupported_stmt_msg(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::With(_) => "with statements are not supported",
        Stmt::Debugger(_) => "debugger statements are not supported",
        _ => "this statement is not supported",
    }
}
