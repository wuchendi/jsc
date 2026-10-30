#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::{Spanned, SyntaxContext};

use crate::error::CompileError;

use super::expr::push_accessor_half;

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
                // `[a, o.b] = rhs;` / `({k: o.p} = rhs);` desugars to a sequence of
                // statements, so it is lowered here rather than as an expression.
                if let Expr::Assign(assign) = strip_parens(&expr_stmt.expr)
                    && assign.op == AssignOp::Assign
                    && let AssignTarget::Pat(pat) = &assign.left
                {
                    return self.lower_destructuring_assign(pat, &assign.right, out);
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
                // `return e` in an async generator awaits `e`.
                let value = if self.frame().machine == Some(Machine::AsyncGenerator) {
                    value.map(generator::await_suspend)
                } else {
                    value
                };
                let value = if self.frame().derived() {
                    Some(self.derived_return(value))
                } else {
                    value
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
            // A stray `;` is an empty statement.
            Stmt::Empty(_) => Ok(()),
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
            .map(|f| {
                self.frame().var_ids.insert(f.ident.span.lo.0);
                self.declare(f.ident.sym.as_ref(), true, f.ident.span.lo.0)
            })
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
            let init = match (declarator.init.as_ref(), &declarator.name) {
                (Some(init), Pat::Ident(ident)) => {
                    self.lower_named_expr(init, ident.id.sym.as_ref())?
                }
                (Some(init), _) => self.lower_expr(init)?,
                (None, _) => IrNode::Undefined,
            };
            // A `var` is hoisted (DESIGN.md §7.1): without an initializer it
            // does nothing, with one it assigns the hoisted binding.
            if var_decl.kind == VarDeclKind::Var && declarator.init.is_none() {
                continue;
            }
            self.binding_var = var_decl.kind == VarDeclKind::Var;
            let result = self.lower_binding_pattern(&declarator.name, init, mutable, out);
            self.binding_var = false;
            result?;
        }
        Ok(())
    }

    /// Lower a `class C {..}` declaration: the class value (see
    /// [`Self::lower_class`]) bound to `C` in the current scope.
    fn lower_class_decl(
        &mut self,
        class_decl: &ClassDecl,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let name = class_decl.ident.sym.as_ref();
        let local = self.declare(name, true, class_decl.ident.span.lo.0);
        let value = self.lower_class(&class_decl.class, name, true, None)?;
        self.push_binding(local, value, out);
        Ok(())
    }

    /// Lower a class (declaration or expression) to an expression yielding the
    /// class value (DESIGN.md §5.5, §7.6): the constructor function carrying a
    /// non-writable `prototype` (`constructor` plus the instance methods and
    /// accessors) and the static members as own properties. `name` is the
    /// class's `name`; with `inner_name` it is also bound, immutably, inside the
    /// class body (a declaration or a named class expression).
    ///
    /// `extends <expr>` evaluates `<expr>` once: it must be a constructor or
    /// `null` (else a `TypeError`); `C.[[Prototype]]` is the parent and
    /// `C.prototype.[[Prototype]]` is `parent.prototype`. Both are held in
    /// hidden by-value bindings that `super(..)` / `super.x` read (class
    /// identity by binding, not by name). A derived constructor is
    /// `SELF_ALLOC`: `super(..)` constructs `this`, then runs the instance
    /// fields; a base constructor runs them on entry. Static fields are
    /// defined after the class value exists, with `this` = the class.
    ///
    /// A computed member name (method, accessor, or field, static or not) is
    /// evaluated once, in source order, right after the heritage — before any
    /// field initializer runs — and converted with `ToPropertyKey` into a
    /// hidden local; a computed static method or accessor named `"prototype"`
    /// throws a `TypeError` there. `name_key` is the local holding the
    /// computed key an anonymous class is defined under (`{[k]: class {}}`):
    /// its `name` is set from it before the static members are defined.
    ///
    /// Out-of-scope forms are rejected WITH SPANS: decorators, private
    /// members, static blocks.
    #[allow(clippy::too_many_lines)]
    pub(super) fn lower_class(
        &mut self,
        class: &Class,
        name: &str,
        inner_name: bool,
        name_key: Option<u32>,
    ) -> anyhow::Result<IrNode> {
        if !class.decorators.is_empty() {
            return Err(
                CompileError::spanned(class.span(), "class decorators are not supported").into(),
            );
        }

        // Collect the (at most one) constructor, the methods and accessors
        // (instance or static), and the fields; every other member is out of
        // scope. Each member is held with its index in the body, which keys
        // its computed name (`keys`, below).
        let mut ctor = None;
        let mut methods: Vec<(usize, &ClassMethod)> = Vec::new();
        let mut static_methods: Vec<(usize, &ClassMethod)> = Vec::new();
        let mut field_props: Vec<(usize, &ClassProp)> = Vec::new();
        let mut static_fields: Vec<(usize, &ClassProp)> = Vec::new();
        for (i, member) in class.body.iter().enumerate() {
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
                ClassMember::Method(m) => {
                    check_member_name(&m.key, m.is_static, false)?;
                    if m.is_static {
                        static_methods.push((i, m));
                    } else {
                        methods.push((i, m));
                    }
                }
                ClassMember::ClassProp(p) => {
                    check_member_name(&p.key, p.is_static, true)?;
                    if p.is_static {
                        static_fields.push((i, p));
                    } else {
                        field_props.push((i, p));
                    }
                }
                // A stray `;` in the class body binds nothing — harmless.
                ClassMember::Empty(_) => {}
                other => return Err(reject_class_member(other)),
            }
        }

        let mut seq = Vec::new();
        // The class scope: the inner name binding (immutable).
        self.push_scope();
        let inner = inner_name.then(|| self.declare(name, false, class.span.lo.0));
        if let Some(local) = inner
            && self.is_cell(local)
            && self.env_slot_for_local(local).is_none()
        {
            seq.push(IrNode::Assign {
                local,
                value: Box::new(IrNode::CellNew(Box::new(IrNode::Undefined))),
            });
        }

        // `extends <expr>`: the parent constructor and its `prototype`.
        let heritage = match &class.super_class {
            None => None,
            Some(super_expr) => {
                let value = self.lower_expr(super_expr)?;
                let (parent, parent_id) = self.hidden_local();
                let (proto, proto_id) = self.hidden_local();
                seq.push(IrNode::ClassHeritage {
                    value: Box::new(value),
                    parent,
                    proto,
                });
                Some((parent, parent_id, proto, proto_id))
            }
        };

        // The computed member names, by member index: each evaluated once, in
        // source order, into a hidden local (read by the constructor's field
        // initializers through its id).
        let mut keys: Vec<Option<(u32, u32)>> = Vec::with_capacity(class.body.len());
        for member in &class.body {
            let (key, static_method) = match member {
                ClassMember::Method(m) => (&m.key, m.is_static),
                ClassMember::ClassProp(p) => (&p.key, false),
                _ => {
                    keys.push(None);
                    continue;
                }
            };
            let PropName::Computed(computed) = key else {
                keys.push(None);
                continue;
            };
            let value = self.lower_expr(&computed.expr)?;
            let (local, id) = self.hidden_local();
            seq.push(IrNode::Assign {
                local,
                value: Box::new(IrNode::ToPropertyKey(Box::new(value))),
            });
            // A static method or accessor is defined right here in the
            // evaluation order, so one named `prototype` throws now. (A static
            // field named `prototype` throws when it is defined, from
            // `__define` of the non-configurable `prototype`.)
            if static_method {
                seq.push(IrNode::Ternary {
                    cond: Box::new(IrNode::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(IrNode::Local(local)),
                        rhs: Box::new(IrNode::Str("prototype".into())),
                    }),
                    then_branch: Box::new(IrNode::Throw(Box::new(type_error(IrNode::Str(
                        "Classes may not have a static property named 'prototype'".into(),
                    ))))),
                    else_branch: Box::new(IrNode::Undefined),
                });
            }
            keys.push(Some((local, id)));
        }

        // The instance shape (an optimisation): the field declarations and the
        // constructor's `this.<f> = ..` assignments, after a recorded parent's.
        let ctor_stmts: &[Stmt] = ctor
            .and_then(|c| c.body.as_ref())
            .map_or(&[], |b| b.stmts.as_slice());
        let mut fields = match class.super_class.as_deref().map(strip_parens) {
            Some(Expr::Ident(id)) => self
                .shared
                .classes
                .borrow()
                .get(id.sym.as_ref())
                .map(|info| info.fields.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let own = field_props
            .iter()
            .filter_map(|(_, p)| prop_key_name(&p.key).ok())
            .chain(collect_instance_fields(ctor_stmts));
        for f in own {
            if !fields.contains(&f) {
                fields.push(f);
            }
        }
        let reserve = u32::try_from(fields.len()).unwrap_or(u32::MAX);
        self.shared
            .classes
            .borrow_mut()
            .insert(name.to_owned(), ClassInfo { fields });

        let prev_super = self.current_super.replace(SuperScope {
            frame: self.top(),
            parent: heritage.map(|h| h.1),
            proto_parent: heritage.map(|h| h.3),
            is_static: false,
            reserve,
        });

        // The constructor function value — the class value itself. A derived
        // class without a constructor gets `constructor(...args) { super(...args) }`.
        let frame = self.top();
        let prev_fields = std::mem::replace(
            &mut self.class_fields,
            field_props
                .iter()
                .map(|&(i, p)| (p.clone(), keys[i].map(|(_, id)| (frame, id))))
                .collect(),
        );
        let kind = if heritage.is_some() {
            FnKind::DerivedClass
        } else {
            FnKind::Class
        };
        let ctor_value = match ctor {
            Some(c) => {
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
                self.lower_lifted_function(&pats, BodyKind::Block(ctor_stmts), name, kind)?
            }
            None if heritage.is_some() => {
                let (rest, stmts) = default_derived_ctor();
                self.lower_lifted_function(&[&rest], BodyKind::Block(&stmts), name, kind)?
            }
            None => self.lower_lifted_function(&[], BodyKind::Block(&[]), name, kind)?,
        };
        self.class_fields = prev_fields;

        // The prototype: `constructor`, then the methods and accessors as
        // non-enumerable properties; a `get`/`set` pair sharing a name
        // collapses to one accessor property.
        let (class_local, class_id) = self.hidden_local();
        let mut proto_entries = vec![(
            PropKey::Hidden("constructor".into()),
            IrNode::Local(class_local),
        )];
        for (i, m) in methods {
            self.lower_class_method(m, keys[i].map(|k| k.0), &mut proto_entries)?;
        }
        let mut class_entries = vec![(
            PropKey::Fixed("prototype".into()),
            IrNode::Object {
                entries: proto_entries,
                proto: heritage.map(|h| Box::new(IrNode::Local(h.2))),
                class: crate::abi::CLASS_ORDINARY,
                reserve: 0,
            },
        )];
        // Static methods and accessors: own non-enumerable properties of the
        // class, whose home object is the class (`super.x` reads the parent).
        if let Some(scope) = self.current_super.as_mut() {
            scope.is_static = true;
        }
        for (i, m) in static_methods {
            self.lower_class_method(m, keys[i].map(|k| k.0), &mut class_entries)?;
        }
        let ctor_value = match name_key {
            Some(key) => IrNode::SetFunctionName {
                func: Box::new(ctor_value),
                key,
                prefix: "",
                local: self.fresh(),
            },
            None => ctor_value,
        };
        seq.push(IrNode::DefineProps {
            obj: Box::new(ctor_value),
            local: class_local,
            entries: class_entries,
        });
        if let Some((parent, ..)) = heritage {
            seq.push(IrNode::SetProto {
                obj: Box::new(IrNode::Local(class_local)),
                proto: Box::new(IrNode::Local(parent)),
            });
        }
        if let Some(local) = inner {
            seq.push(self.init_binding(local, IrNode::Local(class_local)));
        }

        // Static fields, in source order, defined (CreateDataProperty) once the
        // class value and its inner binding exist; `this` is the class.
        let prev_static = self.static_init.replace((self.top(), class_id));
        for (i, p) in static_fields {
            let entry = if let Some((key, _)) = keys[i] {
                let value = match &p.value {
                    Some(init) => self.lower_computed_named_expr(init, key)?,
                    None => IrNode::Undefined,
                };
                (PropKey::Computed(IrNode::Local(key)), value)
            } else {
                let key = prop_key_name(&p.key)?;
                let value = match &p.value {
                    Some(init) => self.lower_key_named_expr(init, &key, p.key.span())?,
                    None => IrNode::Undefined,
                };
                (PropKey::Static(key), value)
            };
            let local = self.fresh();
            seq.push(IrNode::DefineProps {
                obj: Box::new(IrNode::Local(class_local)),
                local,
                entries: vec![entry],
            });
        }
        self.static_init = prev_static;
        self.current_super = prev_super;
        self.pop_scope();
        seq.push(IrNode::Local(class_local));
        Ok(IrNode::Seq(seq))
    }

    /// Lower a class method or accessor `m` to a function value appended to
    /// `entries` (a prototype's or the class's) as a non-enumerable property.
    /// `computed` is the local holding its converted computed name, which
    /// names the function at run time.
    fn lower_class_method(
        &mut self,
        m: &ClassMethod,
        computed: Option<u32>,
        entries: &mut Vec<(PropKey, IrNode)>,
    ) -> anyhow::Result<()> {
        let (key, name) = if let Some(local) = computed {
            (PropKey::HiddenComputed(IrNode::Local(local)), String::new())
        } else {
            let key = prop_key_name(&m.key)?;
            let name = key_fn_name(&key, m.key.span())?;
            (PropKey::Hidden(key), name)
        };
        let (accessor, prefix) = match m.kind {
            MethodKind::Getter => (Some(true), "get "),
            MethodKind::Setter => (Some(false), "set "),
            MethodKind::Method => (None, ""),
        };
        let value = self.lower_method(&m.function, &format!("{prefix}{name}"))?;
        let value = self.set_computed_name(value, computed, prefix);
        match accessor {
            Some(getter) => push_accessor_half(entries, key, value, getter),
            None => entries.push((key, value)),
        }
        Ok(())
    }

    /// Initialize the freshly declared binding `local` (not a module binding)
    /// to `value`, as an expression: its generator env cell, its boxed cell
    /// (allocated beforehand), or the plain local.
    fn init_binding(&mut self, local: u32, value: IrNode) -> IrNode {
        if let Some(slot) = self.env_slot_for_local(local) {
            IrNode::CellSet {
                cell: Box::new(IrNode::EnvLoad(slot)),
                value: Box::new(value),
            }
        } else if self.is_cell(local) {
            IrNode::CellSet {
                cell: Box::new(IrNode::Local(local)),
                value: Box::new(value),
            }
        } else {
            IrNode::Assign {
                local,
                value: Box::new(value),
            }
        }
    }

    /// The value a derived constructor's `return value` returns (DESIGN.md §7.6):
    /// an object as is; `undefined` (or no value) the bound `this` (a
    /// `ReferenceError` before `super(..)`); anything else a `TypeError`.
    fn derived_return(&mut self, value: Option<IrNode>) -> IrNode {
        let fi = self.top();
        let Some(value) = value else {
            return self.derived_this(fi);
        };
        let tmp = self.fresh();
        IrNode::Seq(vec![
            IrNode::Assign {
                local: tmp,
                value: Box::new(value),
            },
            IrNode::Ternary {
                cond: Box::new(IrNode::IsObject(Box::new(IrNode::Local(tmp)))),
                then_branch: Box::new(IrNode::Local(tmp)),
                else_branch: Box::new(IrNode::Ternary {
                    cond: Box::new(IrNode::Binary {
                        op: BinOp::Eq,
                        lhs: Box::new(IrNode::Local(tmp)),
                        rhs: Box::new(IrNode::Undefined),
                    }),
                    then_branch: Box::new(self.derived_this(fi)),
                    else_branch: Box::new(IrNode::Throw(Box::new(type_error(IrNode::Str(
                        "Derived constructors may only return object or undefined".into(),
                    ))))),
                }),
            },
        ])
    }

    /// Desugar a binding pattern `pat`, bound to the already-lowered `source`
    /// value, into declaration statements appended to `out`. The plain
    /// `Pat::Ident` case binds one local (boxing a captured binding into a cell,
    /// like the fast path in [`Self::lower_var_decl`]); `Pat::Array` opens
    /// `source`'s iterator and steps it lazily per element
    /// ([`Self::destructure_array`]): a hole steps without a value, an element
    /// binds the stepped value (`undefined` once done; a default `[a = 9]`
    /// fills `undefined`), a trailing rest (`[a, ...r]`) drains, and a nested
    /// pattern (`[[a]]`) recurses.
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
                // Only a default, a nested pattern or a `var` write (a global
                // property) can throw between steps and need the close.
                let guard = self.binding_var
                    || arr
                        .elems
                        .iter()
                        .flatten()
                        .any(|elem| !matches!(elem, Pat::Ident(_) | Pat::Rest(_)));
                self.destructure_array(source, guard, out, |this, rec, out| {
                    for elem in &arr.elems {
                        match elem {
                            None => {
                                this.push_effect(IrNode::IteratorStep(Box::new(rec.clone())), out);
                            }
                            Some(Pat::Rest(rest)) => {
                                let value = IrNode::Local(this.collect_array_rest(rec, out));
                                this.lower_binding_pattern(&rest.arg, value, mutable, out)?;
                            }
                            Some(elem) => {
                                let value = this.step_value(rec, out);
                                this.bind_with_default(
                                    elem,
                                    &|| IrNode::Local(value),
                                    mutable,
                                    out,
                                )?;
                            }
                        }
                    }
                    Ok(())
                })
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
                // An empty pattern reads nothing, but still requires an
                // object-coercible source (`({}) = null` throws a TypeError).
                if obj.props.is_empty() {
                    out.push(IrStmt::Expr(IrNode::NullishCheck {
                        value: Box::new(IrNode::Local(temp)),
                        key: String::new(),
                    }));
                }
                // The statically-known destructured keys, excluded from a rest.
                let mut bound_keys: Vec<JsStr> = Vec::new();
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
                            let key = JsStr::from(assign.key.id.sym.as_ref());
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
    pub(super) fn bind_ident(
        &mut self,
        ident: &swc_ecma_ast::BindingIdent,
        value: IrNode,
        mutable: bool,
        out: &mut Vec<IrStmt>,
    ) {
        let name = ident.id.sym.as_ref();
        let id = binding_id(ident);
        if let Some((local, cell)) = self.bind_declared_param(name, id) {
            if cell {
                out.push(IrStmt::Expr(IrNode::CellSet {
                    cell: Box::new(IrNode::Local(local)),
                    value: Box::new(value),
                }));
            } else {
                self.push_binding(local, value, out);
            }
            return;
        }
        if self.binding_var {
            // Bound as a `Decl` of a scratch local so the assignment never
            // becomes `main`'s completion value (a `var` statement has none).
            let target = self.var_target(name);
            let local = self.fresh();
            out.push(IrStmt::Decl {
                local,
                init: Self::write_of(target.kind, value),
            });
            return;
        }
        let local = self.declare(name, mutable, id);
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
    /// (a read of a hidden temp). Shared by the array-element and
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

    /// Destructure `source` through the iterator protocol
    /// (DESIGN.md §7.8): open it into a fresh [`IterRecord`],
    /// let `body` step the record lazily per element, then close it unless
    /// done (an empty pattern closes without stepping). With `guard` (an
    /// element that can throw), the elements run as
    ///
    /// ```text
    /// try { BODY } catch (e) { IteratorClose(rec, throw); throw e }
    /// finally { IteratorClose(rec) }
    /// ```
    ///
    /// so a throw from a default, nested pattern or target closes the
    /// iterator, while a throw from the iterator itself does not.
    fn destructure_array(
        &mut self,
        source: IrNode,
        guard: bool,
        out: &mut Vec<IrStmt>,
        body: impl FnOnce(&mut Self, &IterRecord, &mut Vec<IrStmt>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let rec = self.fresh_iter_record(out);
        let open = IrNode::GetIterator {
            value: Box::new(source),
            rec: rec.clone(),
            sealed: self.shared.globals.intrinsics_sealed(),
        };
        self.push_effect(open, out);
        let mut elems = Vec::new();
        body(self, &rec, &mut elems)?;
        let close = |throwing| IrNode::IteratorClose {
            rec: rec.clone(),
            throwing,
        };
        if !guard {
            out.extend(elems);
            self.push_effect(close(false), out);
            return Ok(());
        }
        let exc = self.fresh();
        out.push(IrStmt::Try {
            body: elems,
            catch_body: Some(vec![
                IrStmt::Decl {
                    local: exc,
                    init: IrNode::ExcValue,
                },
                IrStmt::Expr(close(true)),
                IrStmt::Expr(IrNode::Throw(Box::new(IrNode::Local(exc)))),
            ]),
            finally_body: Some(vec![IrStmt::Expr(close(false))]),
        });
        Ok(())
    }

    /// Emit `node` for its effect only, as the `Decl` of a scratch local so it
    /// never becomes `main`'s completion value.
    fn push_effect(&mut self, node: IrNode, out: &mut Vec<IrStmt>) {
        let local = self.fresh();
        out.push(IrStmt::Decl { local, init: node });
    }

    /// Step `rec` once into a fresh local holding the produced value, or
    /// `undefined` once the record is done, and return that local.
    fn step_value(&mut self, rec: &IterRecord, out: &mut Vec<IrStmt>) -> u32 {
        let value = self.fresh();
        out.push(IrStmt::Decl {
            local: value,
            init: IrNode::Ternary {
                cond: Box::new(IrNode::IteratorStep(Box::new(rec.clone()))),
                then_branch: Box::new(IrNode::IteratorValue(Box::new(rec.clone()))),
                else_branch: Box::new(IrNode::Undefined),
            },
        });
        value
    }

    /// Emit the loop draining `rec` into a fresh array local and return that
    /// local (shared by rest bindings and rest assignment targets).
    fn collect_array_rest(&mut self, rec: &IterRecord, out: &mut Vec<IrStmt>) -> u32 {
        // The rest array grows via `push`, so it must be a plain local.
        let rest = self.fresh();
        out.push(IrStmt::Decl {
            local: rest,
            init: IrNode::Array(Vec::new()),
        });
        out.push(IrStmt::While {
            cond: IrNode::IteratorStep(Box::new(rec.clone())),
            body: vec![IrStmt::Expr(IrNode::ArrayPush {
                local: rest,
                value: Box::new(IrNode::IteratorValue(Box::new(rec.clone()))),
            })],
            update: Vec::new(),
            label: None,
        });
        rest
    }

    /// Lower a destructuring ASSIGNMENT statement `pattern = rhs` (object or array
    /// assignment pattern). The RHS is evaluated once into a hidden temp, each
    /// target is assigned in source order, and the statement's value is the RHS.
    fn lower_destructuring_assign(
        &mut self,
        pat: &AssignTargetPat,
        rhs: &Expr,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let init = self.lower_expr(rhs)?;
        let temp = self.fresh();
        out.push(IrStmt::Decl { local: temp, init });
        match pat {
            // The statement's value stays the RHS itself.
            AssignTargetPat::Array(arr) => self.assign_array_pat(arr, IrNode::Local(temp), out)?,
            AssignTargetPat::Object(obj) => self.assign_object_pat(obj, temp, out)?,
            AssignTargetPat::Invalid(invalid) => {
                return Err(
                    CompileError::spanned(invalid.span(), "invalid assignment target").into(),
                );
            }
        }
        // The statement's value is the RHS (stored into a tracked completion).
        out.push(IrStmt::Expr(match self.completion_local() {
            Some(local) => IrNode::Assign {
                local,
                value: Box::new(IrNode::Local(temp)),
            },
            None => IrNode::Local(temp),
        }));
        Ok(())
    }

    /// Assign `value` to a destructuring-assignment `target`: a nested pattern
    /// recurses through a hidden temp; a simple target (identifier or member) is
    /// resolved and written.
    fn assign_to_target(
        &mut self,
        target: &Pat,
        value: IrNode,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        match target {
            Pat::Array(arr) => self.assign_array_pat(arr, value, out),
            Pat::Object(obj) => {
                let temp = self.fresh();
                out.push(IrStmt::Decl {
                    local: temp,
                    init: value,
                });
                self.assign_object_pat(obj, temp, out)
            }
            _ => {
                let target = self.prepare_assign_ref(target, out)?;
                out.push(IrStmt::Expr(Self::write_assign_ref(target, value)));
                Ok(())
            }
        }
    }

    /// Assign each element of an array assignment pattern from `source`'s
    /// iterator, stepped lazily ([`Self::destructure_array`]): a hole steps
    /// without a value, an element's target reference is evaluated before its
    /// step, and a trailing rest drains the remaining values.
    fn assign_array_pat(
        &mut self,
        arr: &ArrayPat,
        source: IrNode,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        // Any target (its reference, default or write) can throw.
        let guard = arr.elems.iter().any(Option::is_some);
        self.destructure_array(source, guard, out, |this, rec, out| {
            for elem in &arr.elems {
                match elem {
                    None => this.push_effect(IrNode::IteratorStep(Box::new(rec.clone())), out),
                    Some(Pat::Rest(rest)) => this.assign_element(&rest.arg, out, |this, out| {
                        IrNode::Local(this.collect_array_rest(rec, out))
                    })?,
                    Some(elem) => this.assign_element(elem, out, |this, out| {
                        IrNode::Local(this.step_value(rec, out))
                    })?,
                }
            }
            Ok(())
        })
    }

    /// Assign each property of an object assignment pattern from `temp.key`; a
    /// trailing rest collects every remaining own key into a new object.
    fn assign_object_pat(
        &mut self,
        obj: &ObjectPat,
        temp: u32,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let read = |key: &JsStr| IrNode::Index {
            obj: Box::new(IrNode::Local(temp)),
            index: Box::new(IrNode::Str(key.clone())),
        };
        let mut bound_keys: Vec<JsStr> = Vec::new();
        for prop in &obj.props {
            match prop {
                ObjectPatProp::KeyValue(kv) => {
                    let key = prop_key_name(&kv.key)?;
                    let value = read(&key);
                    bound_keys.push(key);
                    self.assign_element(&kv.value, out, |_, _| value)?;
                }
                // Shorthand `{x}` / `{x = 7}` assigns the binding named by the key.
                ObjectPatProp::Assign(assign) => {
                    let key = JsStr::from(assign.key.id.sym.as_ref());
                    let target = self.assignable_binding(&assign.key.id)?;
                    let value = self.apply_default(read(&key), assign.value.as_deref(), out)?;
                    bound_keys.push(key);
                    out.push(IrStmt::Expr(Self::write_assign_ref(target, value)));
                }
                ObjectPatProp::Rest(rest) => {
                    let exclude =
                        IrNode::Array(bound_keys.iter().cloned().map(IrNode::Str).collect());
                    self.assign_element(&rest.arg, out, |_, _| IrNode::ObjectRest {
                        source: Box::new(IrNode::Local(temp)),
                        exclude: Box::new(exclude),
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Assign one pattern element (optionally `target = default`) from the value
    /// produced by `fetch`. A simple target's reference (member object, then key)
    /// is evaluated BEFORE the value is fetched, matching JS evaluation order.
    fn assign_element(
        &mut self,
        target: &Pat,
        out: &mut Vec<IrStmt>,
        fetch: impl FnOnce(&mut Self, &mut Vec<IrStmt>) -> IrNode,
    ) -> anyhow::Result<()> {
        let (target, default) = match target {
            Pat::Assign(assign) => (assign.left.as_ref(), Some(assign.right.as_ref())),
            other => (other, None),
        };
        if matches!(target, Pat::Array(_) | Pat::Object(_)) {
            let read = fetch(self, out);
            let value = self.apply_default(read, default, out)?;
            return self.assign_to_target(target, value, out);
        }
        let target = self.prepare_assign_ref(target, out)?;
        let read = fetch(self, out);
        let value = self.apply_default(read, default, out)?;
        out.push(IrStmt::Expr(Self::write_assign_ref(target, value)));
        Ok(())
    }

    /// Apply an optional destructuring default: `read` is stashed once and the
    /// default replaces it only when it is `undefined`. The result is stashed in
    /// a temp too, so a `yield` in the default sits in a declaration (which the
    /// generator segmentation diagnoses) rather than inside a member store.
    fn apply_default(
        &mut self,
        read: IrNode,
        default: Option<&Expr>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<IrNode> {
        let Some(default) = default else {
            return Ok(read);
        };
        let value = self.fresh();
        out.push(IrStmt::Decl {
            local: value,
            init: read,
        });
        let default = self.lower_expr(default)?;
        let result = self.fresh();
        out.push(IrStmt::Decl {
            local: result,
            init: IrNode::Ternary {
                cond: Box::new(IrNode::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(IrNode::Local(value)),
                    rhs: Box::new(IrNode::Undefined),
                }),
                then_branch: Box::new(default),
                else_branch: Box::new(IrNode::Local(value)),
            },
        });
        Ok(IrNode::Local(result))
    }

    /// Evaluate the reference of a simple destructuring-assignment target: an
    /// assignable binding, or a member whose object (and computed key) are
    /// stashed in hidden temps so they run exactly once, before the value.
    fn prepare_assign_ref(
        &mut self,
        target: &Pat,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<AssignRef> {
        let expr = match target {
            Pat::Ident(ident) => return self.assignable_binding(&ident.id),
            Pat::Expr(expr) => strip_parens(expr),
            other => {
                return Err(CompileError::spanned(
                    other.span(),
                    "unsupported destructuring assignment target",
                )
                .into());
            }
        };
        match expr {
            Expr::Ident(ident) => self.assignable_binding(ident),
            Expr::Member(m) => {
                let init = self.lower_expr(&m.obj)?;
                let obj = self.fresh();
                out.push(IrStmt::Decl { local: obj, init });
                let key = match &m.prop {
                    MemberProp::Computed(c) => {
                        let init = self.lower_expr(&c.expr)?;
                        let key = self.fresh();
                        out.push(IrStmt::Decl { local: key, init });
                        IrNode::Local(key)
                    }
                    MemberProp::Ident(id) => IrNode::Str(id.sym.as_ref().into()),
                    MemberProp::PrivateName(_) => {
                        return Err(CompileError::spanned(
                            m.span(),
                            "unsupported assignment target",
                        )
                        .into());
                    }
                };
                Ok(AssignRef::Member { obj, key })
            }
            other => Err(CompileError::spanned(
                other.span(),
                "unsupported destructuring assignment target",
            )
            .into()),
        }
    }

    /// Resolve `ident` as an assignable (declared, non-`const`) binding.
    fn assignable_binding(&mut self, ident: &Ident) -> anyhow::Result<AssignRef> {
        let name = ident.sym.as_ref();
        let resolved = self.resolve_target(name, ident.span())?;
        if !resolved.mutable {
            return Err(CompileError::spanned(
                ident.span(),
                format!("cannot assign to constant binding: {name}"),
            )
            .into());
        }
        Ok(AssignRef::Binding(resolved.kind))
    }

    /// The store of `value` into a prepared assignment reference.
    fn write_assign_ref(target: AssignRef, value: IrNode) -> IrNode {
        match target {
            AssignRef::Binding(kind) => Self::write_of(kind, value),
            AssignRef::Member { obj, key } => IrNode::IndexSet {
                obj: Box::new(IrNode::Local(obj)),
                index: Box::new(key),
                value: Box::new(value),
            },
        }
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

    /// Lower a `for (const v of ITER) body` through the iterator protocol
    /// (DESIGN.md §7.8): `ITER` is evaluated once and opened
    /// into a fresh [`IterRecord`] ([`IrNode::GetIterator`], whose guarded
    /// fast paths walk a clean array, string, `Map`, `Set` or generator
    /// directly), then
    ///
    /// ```text
    /// try {
    ///   while (IteratorStep(rec)) { const v = IteratorValue(rec); BODY }
    /// } catch (e) { IteratorClose(rec, throw); throw e }
    /// finally { IteratorClose(rec) }
    /// ```
    ///
    /// so a `break`, `return`, labelled jump or throw out of the body (or the
    /// head's destructuring) closes the iterator, while a done record or a
    /// throw from the iterator itself does not ([`IrNode::IteratorClose`]
    /// skips a done record; the `catch` close leaves the record done for the
    /// `finally`).
    ///
    /// The loop binding is a fresh block-scoped local rebound each iteration
    /// as the FIRST statement of the body (so a captured binding gets a fresh
    /// cell per iteration, like a `let`). The `for-of` head opens a lexical
    /// scope, so the binding is not visible after the loop.
    ///
    /// `for await (const v of ITER) body` (DESIGN.md §7.8) has the same shape over an
    /// async record ([`IrNode::OpenIterator`] with `is_async`): each step
    /// awaits `next()` ([`IrNode::AsyncIteratorComplete`] over the awaited
    /// [`IrNode::AsyncIteratorNext`]) and each close awaits `return()`
    /// ([`Self::async_iterator_close`]). A throw from a step leaves the
    /// record stepping, so neither close calls `return()`.
    ///
    /// The head is a `const`/`let` identifier or destructuring binding (a
    /// duplicate name is a spanned error), a `var` head assigning its hoisted
    /// binding(s), or a member / destructuring assignment target (`o.x`,
    /// `[o.a, b]`) written each iteration; a bare identifier assignment head,
    /// several declarators, or a `using` head is a spanned error.
    fn lower_for_of(
        &mut self,
        for_of: &swc_ecma_ast::ForOfStmt,
        label: Option<u32>,
        out: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        let head = for_of_head(&for_of.left)?;

        self.push_scope();
        let result = (|| {
            let value = self.lower_expr(&for_of.right)?;
            let rec = self.fresh_iter_record(out);
            out.push(IrStmt::Expr(if for_of.is_await {
                IrNode::OpenIterator {
                    value: Box::new(value),
                    rec: rec.clone(),
                    is_async: true,
                }
            } else {
                IrNode::GetIterator {
                    value: Box::new(value),
                    rec: rec.clone(),
                    sealed: self.shared.globals.intrinsics_sealed(),
                }
            }));
            let target = self.declare_for_of_head(head);
            self.breakable_inc();
            self.loopable_inc();
            let mut body = Vec::new();
            let bound = self
                .bind_for_of_target(target, IrNode::IteratorValue(rec.clone()), &mut body)
                .and_then(|()| self.lower_for_of_body(&for_of.body, &mut body));
            self.loopable_dec();
            self.breakable_dec();
            bound?;
            let (cond, throw_close, normal_close) = if for_of.is_await {
                let step = self.lower_await(IrNode::AsyncIteratorNext(rec.clone()));
                let cond = IrNode::AsyncIteratorComplete {
                    rec: rec.clone(),
                    result: Box::new(step),
                };
                let throw_close = self.async_iterator_close(&rec, true);
                (cond, throw_close, self.async_iterator_close(&rec, false))
            } else {
                let close = |throwing| {
                    vec![IrStmt::Expr(IrNode::IteratorClose {
                        rec: rec.clone(),
                        throwing,
                    })]
                };
                (IrNode::IteratorStep(rec.clone()), close(true), close(false))
            };
            let exc = self.fresh();
            let mut catch_body = vec![IrStmt::Decl {
                local: exc,
                init: IrNode::ExcValue,
            }];
            catch_body.extend(throw_close);
            catch_body.push(IrStmt::Expr(IrNode::Throw(Box::new(IrNode::Local(exc)))));
            out.push(IrStmt::Try {
                body: vec![IrStmt::While {
                    cond,
                    body,
                    update: Vec::new(),
                    label,
                }],
                catch_body: Some(catch_body),
                finally_body: Some(normal_close),
            });
            Ok(())
        })();
        self.pop_scope();
        result
    }

    /// `AsyncIteratorClose(rec, completion)` (DESIGN.md §7.8):
    /// `r = AsyncIteratorReturn(rec)`, then unless `r` is the hole (a done or
    /// stepping record, or no `return` method) await it. A normal completion
    /// checks the awaited result is an object; a throw completion discards
    /// any rejection (its caller rethrows the original exception).
    fn async_iterator_close(&mut self, rec: &IterRecord, throwing: bool) -> Vec<IrStmt> {
        let result = self.fresh();
        let awaited = self.lower_await(IrNode::Local(result));
        let close = if throwing {
            IrStmt::Try {
                body: vec![IrStmt::Expr(awaited)],
                catch_body: Some(Vec::new()),
                finally_body: None,
            }
        } else {
            IrStmt::Expr(IrNode::IteratorResultCheck(Box::new(awaited)))
        };
        vec![
            IrStmt::Decl {
                local: result,
                init: IrNode::AsyncIteratorReturn {
                    rec: Box::new(rec.clone()),
                    throwing,
                },
            },
            IrStmt::If {
                cond: IrNode::IsHole(Box::new(IrNode::Local(result))),
                then_body: Vec::new(),
                else_body: vec![close],
            },
        ]
    }

    /// Declare the four fresh locals of an [`IterRecord`] (each initialized
    /// to `undefined`, so a generator body promotes them like any temporary
    /// it writes) and return the record over them.
    pub(super) fn fresh_iter_record(&mut self, out: &mut Vec<IrStmt>) -> Box<IterRecord> {
        let mut place = || {
            let local = self.fresh();
            out.push(IrStmt::Decl {
                local,
                init: IrNode::Undefined,
            });
            IrNode::Local(local)
        };
        Box::new(IterRecord {
            iter: place(),
            next: place(),
            state: place(),
            result: place(),
        })
    }

    /// Lower a `for-of` body; an empty body (`for (o.x of xs);`) lowers to
    /// nothing.
    fn lower_for_of_body(&mut self, for_body: &Stmt, body: &mut Vec<IrStmt>) -> anyhow::Result<()> {
        if matches!(for_body, Stmt::Empty(_)) {
            return Ok(());
        }
        self.lower_stmt(for_body, body)
    }

    /// Declare a `for-of` head's loop binding; an assignment head declares nothing.
    fn declare_for_of_head<'a>(&mut self, head: ForOfHead<'a>) -> ForOfTarget<'a> {
        match head {
            ForOfHead::Decl(ident, mutable) => {
                ForOfTarget::Local(self.declare(ident.id.sym.as_ref(), mutable, binding_id(ident)))
            }
            ForOfHead::Pattern(pat, mutable) => ForOfTarget::Pattern(pat, mutable),
            ForOfHead::Var(pat) => ForOfTarget::Var(pat),
            ForOfHead::Assign(pat) => ForOfTarget::Assign(pat),
        }
    }

    /// Bind (or assign) the current element `elem` to a `for-of` target as the
    /// first body statement(s).
    fn bind_for_of_target(
        &mut self,
        target: ForOfTarget<'_>,
        elem: IrNode,
        body: &mut Vec<IrStmt>,
    ) -> anyhow::Result<()> {
        match target {
            ForOfTarget::Local(local) => {
                self.push_binding(local, elem, body);
                Ok(())
            }
            ForOfTarget::Pattern(pat, mutable) => {
                self.lower_binding_pattern(pat, elem, mutable, body)
            }
            ForOfTarget::Var(pat) => {
                self.binding_var = true;
                let result = self.lower_binding_pattern(pat, elem, true, body);
                self.binding_var = false;
                result
            }
            ForOfTarget::Assign(pat) => self.assign_to_target(pat, elem, body),
        }
    }

    /// Lower a `for (const k in EXPR) body` over the enumerable string keys of an
    /// object and its prototype chain, desugared to a counted index loop lowered
    /// to [`IrStmt::While`] — the same shape as [`Self::lower_for_of`] over the key
    /// snapshot. `EXPR` is evaluated ONCE and its keys snapshotted ONCE into a
    /// fresh array ([`ObjectBuiltinOp::ForInKeys`]: own keys in own-key order, then
    /// each prototype's not-yet-seen keys; the decimal index strings for an
    /// array/string), its length cached, and an index counter walks `0..count`.
    /// Each iteration binds a fresh block-scoped local to the key at the current
    /// index as the FIRST body statement (so a captured binding gets a fresh cell
    /// per iteration, like a `let`). The index
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
        let (ident, pat, kind) = match &for_in.left {
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
                (ident, &declarator.name, var_decl.kind)
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
            // Snapshot the operand's keys ONCE into a temp, then cache the count.
            let iter = IrNode::ObjectBuiltin {
                op: ObjectBuiltinOp::ForInKeys,
                arg: Box::new(self.lower_expr(&for_in.right)?),
            };
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
            // to the key at index `i` as the first statement of the body; a
            // `var` head assigns the hoisted binding instead.
            let target = if kind == VarDeclKind::Var {
                ForOfTarget::Var(pat)
            } else {
                ForOfTarget::Local(self.declare(
                    ident.id.sym.as_ref(),
                    kind != VarDeclKind::Const,
                    binding_id(ident),
                ))
            };
            self.breakable_inc();
            self.loopable_inc();
            let mut body = Vec::new();
            let key = IrNode::Index {
                obj: Box::new(IrNode::Local(iter_local)),
                index: Box::new(IrNode::Local(idx_local)),
            };
            self.bind_for_of_target(target, key, &mut body)?;
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
pub(super) fn prop_key_name(key: &PropName) -> anyhow::Result<JsStr> {
    match key {
        PropName::Ident(id) => Ok(id.sym.as_ref().into()),
        PropName::Str(s) => Ok(lit_str(s)),
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
/// The evaluated reference of a simple destructuring-assignment target.
enum AssignRef {
    /// An assignable binding.
    Binding(AccessKind),
    /// A member `obj[key]`: the object stashed in a local, the key either a
    /// static string or a stashed local.
    Member { obj: u32, key: IrNode },
}

/// The head of a `for-of` loop: a fresh `const`/`let` binding or
/// destructuring pattern (with its mutability), or an assignment target
/// written each iteration.
#[derive(Clone, Copy)]
enum ForOfHead<'a> {
    Decl(&'a swc_ecma_ast::BindingIdent, bool),
    Pattern(&'a Pat, bool),
    Var(&'a Pat),
    Assign(&'a Pat),
}

/// A declared `for-of` head: the per-iteration binding local, the binding
/// pattern (declared per iteration), or the assignment target.
#[derive(Clone, Copy)]
enum ForOfTarget<'a> {
    Local(u32),
    Pattern(&'a Pat, bool),
    Var(&'a Pat),
    Assign(&'a Pat),
}

/// Classify a `for-of` head: a single `const`/`let` identifier binding, a
/// declared destructuring pattern, a `var` head, or a member / destructuring
/// assignment target (`o.x`, `[o.a, o.b]`).
fn for_of_head(left: &ForHead) -> anyhow::Result<ForOfHead<'_>> {
    Ok(match left {
        ForHead::VarDecl(var_decl) => {
            let [declarator] = var_decl.decls.as_slice() else {
                return Err(CompileError::spanned(
                    var_decl.span(),
                    "for-of requires a single loop variable",
                )
                .into());
            };
            let mutable = var_decl.kind != VarDeclKind::Const;
            match &declarator.name {
                // A `var` head assigns the hoisted binding(s).
                pat if var_decl.kind == VarDeclKind::Var => ForOfHead::Var(pat),
                Pat::Ident(ident) => ForOfHead::Decl(ident, mutable),
                pat @ (Pat::Array(_) | Pat::Object(_)) => {
                    let mut names = Vec::new();
                    super::capture::pat_names(pat, &mut names);
                    let mut seen = std::collections::HashSet::new();
                    if let Some(dup) = names.iter().find(|name| !seen.insert(name.as_str())) {
                        return Err(CompileError::spanned(
                            pat.span(),
                            format!("duplicate binding in for-of head: {dup}"),
                        )
                        .into());
                    }
                    ForOfHead::Pattern(pat, mutable)
                }
                other => {
                    return Err(CompileError::spanned(
                        other.span(),
                        "unsupported for-of binding pattern",
                    )
                    .into());
                }
            }
        }
        // A bare identifier head (`for (x of ..)`) stays rejected.
        ForHead::Pat(pat) if !is_ident_target(pat) => ForOfHead::Assign(pat),
        other => {
            return Err(CompileError::spanned(
                other.span(),
                "for-of requires a `const`/`let` loop variable",
            )
            .into());
        }
    })
}

/// Whether an assignment-target pattern is a bare identifier (`x` / `(x)`).
fn is_ident_target(pat: &Pat) -> bool {
    match pat {
        Pat::Ident(_) => true,
        Pat::Expr(expr) => matches!(strip_parens(expr), Expr::Ident(_)),
        _ => false,
    }
}

/// The class-member name early errors (`SyntaxError`s) of a literal key: a
/// static member named `prototype`, a field named `constructor`, and an
/// identifier starting with a character that cannot start one (ZWJ / ZWNJ,
/// accepted by the lexer when escaped). A computed name is not checked here:
/// a computed static `"prototype"` is a run-time `TypeError`
/// ([`LowerCtx::lower_class`]), and a computed `"constructor"` is an ordinary
/// method or field.
fn check_member_name(key: &PropName, is_static: bool, field: bool) -> anyhow::Result<()> {
    let name = match key {
        PropName::Ident(id) => {
            if id.sym.starts_with(['\u{200c}', '\u{200d}']) {
                return Err(CompileError::spanned(key.span(), "invalid identifier start").into());
            }
            id.sym.as_ref().into()
        }
        PropName::Str(s) => lit_str(s),
        _ => return Ok(()),
    };
    let msg = if is_static && name == "prototype" {
        "classes may not have a static property named 'prototype'"
    } else if field && name == "constructor" {
        "classes may not have a field named 'constructor'"
    } else {
        return Ok(());
    };
    Err(CompileError::spanned(key.span(), msg).into())
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

/// The name of a derived class's default-constructor rest binding (not
/// spellable in source).
pub(super) const DEFAULT_CTOR_ARGS: &str = "%args";

/// The parameter and body of a derived class's default constructor,
/// `constructor(...args) { super(...args) }`. The rest binding's name cannot
/// be spelled in source, so field initializers never see it.
fn default_derived_ctor() -> (Pat, Vec<Stmt>) {
    use swc_common::DUMMY_SP;
    use swc_ecma_ast::{BindingIdent, CallExpr, RestPat, Super};
    let args = Ident::new_no_ctxt(DEFAULT_CTOR_ARGS.into(), DUMMY_SP);
    let rest = Pat::Rest(RestPat {
        span: DUMMY_SP,
        dot3_token: DUMMY_SP,
        arg: Box::new(Pat::Ident(BindingIdent::from(args.clone()))),
        type_ann: None,
    });
    let call = Expr::Call(CallExpr {
        span: DUMMY_SP,
        ctxt: SyntaxContext::empty(),
        callee: Callee::Super(Super { span: DUMMY_SP }),
        args: vec![ExprOrSpread {
            spread: Some(DUMMY_SP),
            expr: Box::new(Expr::Ident(args)),
        }],
        type_args: None,
    });
    let stmt = Stmt::Expr(ExprStmt {
        span: DUMMY_SP,
        expr: Box::new(call),
    });
    (rest, vec![stmt])
}

/// Collect the instance field names a constructor assigns via `this.<name> = ..`,
/// in first-seen order with duplicates removed: `new` reserves property-table
/// capacity for them (a miss only costs a table regrowth). The scan walks the constructor's
/// statements and expressions but does NOT descend into nested `function`/arrow
/// bodies: a nested `function` rebinds `this`, and an arrow-captured `this.f = ..`
/// running later is a documented gap.
fn collect_instance_fields(stmts: &[Stmt]) -> Vec<JsStr> {
    let mut fields = Vec::new();
    for stmt in stmts {
        collect_fields_stmt(stmt, &mut fields);
    }
    fields
}

/// Record a `this.<name> = ..` assignment target's field name (deduplicated).
fn record_this_field(target: &AssignTarget, fields: &mut Vec<JsStr>) {
    let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = target else {
        return;
    };
    if !matches!(m.obj.as_ref(), Expr::This(_)) {
        return;
    }
    let name = match &m.prop {
        MemberProp::Ident(id) => Some(JsStr::from(id.sym.as_ref())),
        MemberProp::Computed(c) => match c.expr.as_ref() {
            Expr::Lit(Lit::Str(s)) => Some(lit_str(s)),
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
fn collect_fields_stmt(stmt: &Stmt, fields: &mut Vec<JsStr>) {
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
fn collect_fields_expr(expr: &Expr, fields: &mut Vec<JsStr>) {
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
