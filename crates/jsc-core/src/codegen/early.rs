//! Early errors of the strict-mode static semantics that swc's parser does
//! not report, checked over the whole program before lowering:
//!
//! - a name declared twice lexically in one scope, or both lexically and by
//!   `var` (a block, a `switch` case block, a function body, the script);
//!   in a block every function declaration is lexical, at a function's (or
//!   the script's) top level it is var-scoped;
//! - a parameter redeclared lexically in its function's body, a catch
//!   parameter redeclared lexically in its block (or by `var` when it is a
//!   destructuring pattern), a duplicate name in a catch parameter, and a
//!   duplicate parameter name of any function (every function is strict);
//! - an `async function` declaration in statement position (`if (x) async
//!   function f(){}`), and a labelled function declaration (`l: function
//!   f(){}`);
//! - `await` (escaped or not) as an identifier inside an async body;
//! - two `__proto__: v` entries in one object literal (a destructuring
//!   assignment pattern is an `ObjectPat`, not visited here);
//! - in a class static block (a function-like scope for the lexical checks):
//!   `arguments` outside a nested non-arrow function, `super(..)` likewise;
//!   `await` as an identifier, `await` / `yield` expressions, `for await`,
//!   `return`, and `break` / `continue` / labels crossing the block, outside
//!   nested functions and arrows;
//! - a private name `#x` no enclosing class body declares (a class heritage
//!   sees only the enclosing classes' names), a private name declared twice in
//!   one class body (except one getter plus one setter of the same
//!   static-ness), `delete` of a private reference (parenthesised or the end
//!   of an optional chain), and a bare `#x` anywhere but left of `in`;
//! - a private name whose first character is not an `IdentifierStart` (swc
//!   checks a leading `\u` escape only as an `IdentifierPart`, so `#\u200Dx`
//!   with a ZWJ / ZWNJ gets through).

use std::collections::HashMap;

use swc_common::{Span, Spanned};
use swc_ecma_ast::{
    ArrowExpr, ArrowFunctionBody, AwaitExpr, BinExpr, BinaryOp, BlockStmt, BreakStmt, CallExpr,
    Callee, CatchClause, Class, ClassMember, Constructor, ContinueStmt, Decl, Expr, FnExpr,
    ForHead, Function, FunctionBody, Ident, Key, MemberProp, MethodKind, ObjectLit, ObjectPatProp,
    OptChainBase, ParamOrTsParamProp, Pat, PrivateName, Program, Prop, PropOrSpread, ReturnStmt,
    StaticBlock, Stmt, SwitchStmt, UnaryExpr, UnaryOp, VarDeclKind, VarDeclOrExpr, YieldExpr,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::error::CompileError;

/// Check `program` for the early errors listed in the module docs.
pub(super) fn check(program: &Program) -> anyhow::Result<()> {
    let mut checker = Checker::default();
    match program {
        Program::Script(script) => checker.body(&[], script.body.iter()),
        Program::Module(module) => {
            checker.body(&[], module.body.iter().filter_map(|item| item.as_stmt()));
        }
    }
    checker.error.map_or(Ok(()), |e| Err(e.into()))
}

#[derive(Default)]
struct Checker {
    /// The first early error found.
    error: Option<CompileError>,
    /// Whether the code being visited is an async function body.
    in_async: bool,
    /// Inside a class static block and not inside a nested non-arrow
    /// function: `arguments` and `super(..)` are errors.
    static_fn: bool,
    /// Inside a class static block and not inside a nested function or arrow:
    /// the block body's labels and statement nesting.
    static_body: Option<StaticBody>,
    /// The private names (`"#x"`) declared by each enclosing class body,
    /// innermost last.
    private_names: Vec<Vec<String>>,
}

/// What a class body has declared so far under one private name.
#[derive(Clone, Copy)]
enum PrivateDeclared {
    /// A lone getter (`true` when static), which one setter may complete.
    Getter(bool),
    /// A lone setter (`true` when static), which one getter may complete.
    Setter(bool),
    /// A field, method, auto-accessor or complete getter/setter pair.
    Complete,
}

/// The jump targets visible in a class static block's own body (none from
/// outside it).
#[derive(Default)]
struct StaticBody {
    /// The enclosing labels.
    labels: Vec<String>,
    /// The enclosing loops.
    loops: u32,
    /// The enclosing `switch` statements.
    switches: u32,
}

impl Checker {
    fn fail(&mut self, span: Span, msg: String) {
        if self.error.is_none() {
            self.error = Some(CompileError::spanned(span, msg));
        }
    }

    /// Check a statement list forming a lexical scope against itself, then
    /// against the bound names `outer` (parameters / a catch parameter).
    /// `block` selects block semantics (function declarations are lexical).
    fn scope<'a>(
        &mut self,
        stmts: impl Iterator<Item = &'a Stmt> + Clone,
        block: bool,
        outer: &[(String, Span)],
    ) {
        let mut lexical: HashMap<String, Span> = HashMap::new();
        for (name, span) in lexical_names(stmts.clone(), block) {
            if lexical.insert(name.clone(), span).is_some() {
                self.fail(span, already_declared(&name));
            }
        }
        let mut vars = Vec::new();
        for stmt in stmts {
            var_names(stmt, &mut vars);
            if !block && let Stmt::Decl(Decl::Fn(f)) = stmt {
                vars.push((f.ident.sym.to_string(), f.ident.span));
            }
        }
        for (name, _) in vars.iter().chain(outer) {
            if let Some(&span) = lexical.get(name) {
                self.fail(span, already_declared(name));
            }
        }
    }

    /// Check and visit a function (or script) body with parameters `params`.
    fn body<'a>(
        &mut self,
        params: &[(String, Span)],
        stmts: impl Iterator<Item = &'a Stmt> + Clone,
    ) {
        self.scope(stmts.clone(), false, params);
        for stmt in stmts {
            stmt.visit_with(self);
        }
    }

    /// Check and visit a function-like with parameter patterns `params` and
    /// block `body`, rejecting duplicate parameters.
    fn function<'a>(
        &mut self,
        params: impl Iterator<Item = &'a Pat>,
        body: Option<&FunctionBody>,
        is_async: bool,
    ) {
        let outer_async = std::mem::replace(&mut self.in_async, is_async);
        let mut names = Vec::new();
        for pat in params {
            pat.visit_with(self);
            pat_names(pat, &mut names);
        }
        let mut seen = std::collections::HashSet::new();
        for (name, span) in &names {
            if !seen.insert(name) {
                self.fail(*span, format!("duplicate parameter name `{name}`"));
            }
        }
        if let Some(body) = body {
            self.body(&names, body.stmts.iter());
        }
        self.in_async = outer_async;
    }

    /// Visit a nested function (`arrow` unset) or arrow with `visit`, outside
    /// the enclosing static block's restrictions it crosses.
    fn nested(&mut self, arrow: bool, visit: impl FnOnce(&mut Self)) {
        let outer_body = self.static_body.take();
        let outer_fn = self.static_fn;
        if !arrow {
            self.static_fn = false;
        }
        visit(self);
        self.static_body = outer_body;
        self.static_fn = outer_fn;
    }

    /// Reject `what` when inside a static block's own body.
    fn static_body_only(&mut self, span: Span, what: &str) {
        if self.static_body.is_some() {
            self.fail(
                span,
                format!("{what} is not allowed in a class static block"),
            );
        }
    }

    /// Reject an `async function` declaration as the body of a statement.
    fn statement_position(&mut self, stmt: &Stmt) {
        if let Stmt::Decl(Decl::Fn(f)) = stmt
            && f.function.is_async
        {
            self.fail(
                f.function.span,
                "async function declarations are not allowed in statement position".to_owned(),
            );
        }
    }
}

impl Visit for Checker {
    fn visit_function(&mut self, f: &Function) {
        self.nested(false, |this| {
            this.function(f.params.iter().map(|p| &p.pat), f.body.as_ref(), f.is_async);
        });
    }

    fn visit_static_block(&mut self, b: &StaticBlock) {
        let outer_async = std::mem::replace(&mut self.in_async, false);
        let outer_body = self.static_body.replace(StaticBody::default());
        let outer_fn = std::mem::replace(&mut self.static_fn, true);
        self.body(&[], b.body.stmts.iter());
        self.in_async = outer_async;
        self.static_body = outer_body;
        self.static_fn = outer_fn;
    }

    fn visit_call_expr(&mut self, c: &CallExpr) {
        if self.static_fn && matches!(c.callee, Callee::Super(_)) {
            self.fail(
                c.span,
                "`super()` is not allowed in a class static block".to_owned(),
            );
        }
        c.visit_children_with(self);
    }

    fn visit_await_expr(&mut self, a: &AwaitExpr) {
        self.static_body_only(a.span, "`await`");
        a.visit_children_with(self);
    }

    fn visit_yield_expr(&mut self, y: &YieldExpr) {
        self.static_body_only(y.span, "`yield`");
        y.visit_children_with(self);
    }

    fn visit_return_stmt(&mut self, r: &ReturnStmt) {
        self.static_body_only(r.span, "`return`");
        r.visit_children_with(self);
    }

    fn visit_break_stmt(&mut self, b: &BreakStmt) {
        if let Some(body) = &self.static_body {
            let found = match &b.label {
                Some(label) => body.labels.iter().any(|l| *l == *label.sym),
                None => body.loops + body.switches > 0,
            };
            if !found {
                self.fail(
                    b.span,
                    "`break` must target a statement inside the class static block".to_owned(),
                );
            }
        }
    }

    fn visit_continue_stmt(&mut self, c: &ContinueStmt) {
        if let Some(body) = &self.static_body {
            let found = match &c.label {
                Some(label) => body.labels.iter().any(|l| *l == *label.sym),
                None => body.loops > 0,
            };
            if !found {
                self.fail(
                    c.span,
                    "`continue` must target a loop inside the class static block".to_owned(),
                );
            }
        }
    }

    fn visit_fn_expr(&mut self, f: &FnExpr) {
        // An async function expression's own name is in its async context.
        if let Some(ident) = &f.ident {
            let outer_async = std::mem::replace(&mut self.in_async, f.function.is_async);
            ident.visit_with(self);
            self.in_async = outer_async;
        }
        f.function.visit_with(self);
    }

    fn visit_arrow_expr(&mut self, a: &ArrowExpr) {
        match a.body.as_ref() {
            ArrowFunctionBody::FunctionBody(block) => self.nested(true, |this| {
                this.function(a.params.iter(), Some(block), a.is_async);
            }),
            ArrowFunctionBody::Expr(expr) => self.nested(true, |this| {
                this.function(a.params.iter(), None, a.is_async);
                let outer_async = std::mem::replace(&mut this.in_async, a.is_async);
                expr.visit_with(this);
                this.in_async = outer_async;
            }),
        }
    }

    fn visit_constructor(&mut self, c: &Constructor) {
        let params = c.params.iter().filter_map(|p| match p {
            ParamOrTsParamProp::Param(p) => Some(&p.pat),
            ParamOrTsParamProp::TsParamProp(_) => None,
        });
        self.nested(false, |this| this.function(params, c.body.as_ref(), false));
    }

    fn visit_block_stmt(&mut self, b: &BlockStmt) {
        self.scope(b.stmts.iter(), true, &[]);
        b.visit_children_with(self);
    }

    fn visit_switch_stmt(&mut self, s: &SwitchStmt) {
        self.scope(s.cases.iter().flat_map(|c| c.cons.iter()), true, &[]);
        s.visit_children_with(self);
    }

    fn visit_catch_clause(&mut self, c: &CatchClause) {
        let mut names = Vec::new();
        if let Some(param) = &c.param {
            pat_names(param, &mut names);
        }
        let mut seen = std::collections::HashSet::new();
        for (name, span) in &names {
            if !seen.insert(name.as_str()) {
                self.fail(*span, already_declared(name));
            }
        }
        // A destructuring parameter also conflicts with a `var` of its block
        // (B.3.4 relaxes this only for a plain identifier parameter).
        if let Some(param) = &c.param
            && !matches!(param, Pat::Ident(_))
        {
            let mut vars = Vec::new();
            c.body.stmts.iter().for_each(|s| var_names(s, &mut vars));
            for (name, span) in vars {
                if seen.contains(name.as_str()) {
                    self.fail(span, already_declared(&name));
                }
            }
        }
        self.scope(c.body.stmts.iter(), true, &names);
        c.visit_children_with(self);
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::If(i) => {
                self.statement_position(&i.cons);
                if let Some(alt) = &i.alt {
                    self.statement_position(alt);
                }
            }
            Stmt::While(w) => self.statement_position(&w.body),
            Stmt::DoWhile(w) => self.statement_position(&w.body),
            Stmt::For(f) => self.statement_position(&f.body),
            Stmt::ForIn(f) => self.statement_position(&f.body),
            Stmt::ForOf(f) => self.statement_position(&f.body),
            Stmt::Labeled(l) => {
                // A labelled function declaration is sloppy-mode-only (B.3.1);
                // every function here is strict.
                if let Stmt::Decl(Decl::Fn(f)) = l.body.as_ref() {
                    self.fail(
                        f.function.span,
                        "In strict mode code, functions can only be declared at top level or inside a block".to_owned(),
                    );
                }
                self.statement_position(&l.body);
            }
            _ => {}
        }
        if self.static_body.is_none() {
            stmt.visit_children_with(self);
            return;
        }
        // Inside a static block's own body: track the jump targets.
        let label = match stmt {
            Stmt::Labeled(l) => Some(&l.label),
            _ => None,
        };
        if let Some(label) = label
            && self
                .static_body
                .as_ref()
                .is_some_and(|body| body.labels.iter().any(|l| *l == *label.sym))
        {
            self.fail(
                label.span,
                format!("label `{}` has already been declared", label.sym),
            );
        }
        if let Stmt::ForOf(f) = stmt
            && f.is_await
        {
            self.static_body_only(f.span, "`for await`");
        }
        let (loops, switches) = match stmt {
            Stmt::While(_) | Stmt::DoWhile(_) | Stmt::For(_) | Stmt::ForIn(_) | Stmt::ForOf(_) => {
                (1, 0)
            }
            Stmt::Switch(_) => (0, 1),
            _ => (0, 0),
        };
        if let Some(body) = &mut self.static_body {
            body.loops += loops;
            body.switches += switches;
            body.labels.extend(label.map(|l| l.sym.to_string()));
        }
        stmt.visit_children_with(self);
        if let Some(body) = &mut self.static_body {
            body.loops -= loops;
            body.switches -= switches;
            if label.is_some() {
                body.labels.pop();
            }
        }
    }

    fn visit_class(&mut self, c: &Class) {
        // The heritage is evaluated in the enclosing private environment.
        c.decorators.visit_with(self);
        c.super_class.visit_with(self);
        let mut names = Vec::new();
        let mut declared: HashMap<String, PrivateDeclared> = HashMap::new();
        for member in &c.body {
            let (key, kind, is_static) = match member {
                ClassMember::PrivateProp(p) => (&p.key, None, p.is_static),
                ClassMember::PrivateMethod(m) => (&m.key, Some(m.kind), m.is_static),
                ClassMember::AutoAccessor(a) => match &a.key {
                    Key::Private(key) => (key, None, a.is_static),
                    Key::Public(_) => continue,
                },
                _ => continue,
            };
            let name = format!("#{}", key.name);
            let prev = declared.get(&name).copied();
            let next = match (kind, prev) {
                (Some(MethodKind::Getter), None) => PrivateDeclared::Getter(is_static),
                (Some(MethodKind::Setter), None) => PrivateDeclared::Setter(is_static),
                (_, None) => PrivateDeclared::Complete,
                (Some(MethodKind::Getter), Some(PrivateDeclared::Setter(s)))
                | (Some(MethodKind::Setter), Some(PrivateDeclared::Getter(s)))
                    if s == is_static =>
                {
                    PrivateDeclared::Complete
                }
                _ => {
                    self.fail(key.span, format!("duplicate private name {name}"));
                    PrivateDeclared::Complete
                }
            };
            if prev.is_none() {
                names.push(name.clone());
            }
            declared.insert(name, next);
        }
        self.private_names.push(names);
        c.body.visit_with(self);
        self.private_names.pop();
    }

    fn visit_private_name(&mut self, p: &PrivateName) {
        let name = format!("#{}", p.name);
        if !p.name.chars().next().is_some_and(Ident::is_valid_start) {
            self.fail(p.span, format!("invalid private name {name}"));
        }
        if !self.private_names.iter().any(|names| names.contains(&name)) {
            self.fail(p.span, format!("private name {name} is not defined"));
        }
    }

    fn visit_expr(&mut self, e: &Expr) {
        // `#x` is an expression only as the left operand of `in`
        // (`visit_bin_expr`).
        if let Expr::PrivateName(p) = e {
            self.fail(p.span, format!("unexpected private name #{}", p.name));
        }
        e.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, b: &BinExpr) {
        if b.op == BinaryOp::In
            && let Expr::PrivateName(p) = b.left.as_ref()
        {
            self.visit_private_name(p);
            b.right.visit_with(self);
            return;
        }
        b.visit_children_with(self);
    }

    fn visit_unary_expr(&mut self, u: &UnaryExpr) {
        if u.op == UnaryOp::Delete && is_private_reference(&u.arg) {
            self.fail(u.arg.span(), "private fields cannot be deleted".to_owned());
        }
        u.visit_children_with(self);
    }

    fn visit_object_lit(&mut self, o: &ObjectLit) {
        let mut seen = false;
        for prop in &o.props {
            if let PropOrSpread::Prop(prop) = prop
                && let Prop::KeyValue(kv) = prop.as_ref()
                && super::expr::is_proto_key(&kv.key)
            {
                if seen {
                    self.fail(
                        kv.key.span(),
                        "duplicate __proto__ fields are not allowed in object literals".to_owned(),
                    );
                }
                seen = true;
            }
        }
        o.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if self.in_async && ident.sym.as_ref() == "await" {
            self.fail(
                ident.span,
                "`await` cannot be used as an identifier in an async context".to_owned(),
            );
        }
        if self.static_body.is_some() && ident.sym.as_ref() == "await" {
            self.fail(
                ident.span,
                "`await` cannot be used as an identifier in a class static block".to_owned(),
            );
        }
        if self.static_fn && ident.sym.as_ref() == "arguments" {
            self.fail(
                ident.span,
                "`arguments` is not allowed in a class static block".to_owned(),
            );
        }
    }
}

/// Whether `expr`, parentheses stripped, is a private member reference
/// `o.#x` or an optional chain ending in one (`o?.#x`, `o?.a.#x`).
fn is_private_reference(mut expr: &Expr) -> bool {
    while let Expr::Paren(p) = expr {
        expr = &p.expr;
    }
    let member = match expr {
        Expr::Member(m) => m,
        Expr::OptChain(chain) => match chain.base.as_ref() {
            OptChainBase::Member(m) => m,
            OptChainBase::Call(_) => return false,
        },
        _ => return false,
    };
    matches!(member.prop, MemberProp::PrivateName(_))
}

fn already_declared(name: &str) -> String {
    format!("`{name}` has already been declared")
}

/// The lexically declared names of a statement list (`let`/`const`/`class`,
/// and function declarations when `block`), in source order.
fn lexical_names<'a>(stmts: impl Iterator<Item = &'a Stmt>, block: bool) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::Decl(Decl::Var(v)) if v.kind != VarDeclKind::Var => {
                for d in &v.decls {
                    pat_names(&d.name, &mut out);
                }
            }
            Stmt::Decl(Decl::Class(c)) => out.push((c.ident.sym.to_string(), c.ident.span)),
            Stmt::Decl(Decl::Fn(f)) if block => out.push((f.ident.sym.to_string(), f.ident.span)),
            _ => {}
        }
    }
    out
}

/// Collect the `var`-declared names of `stmt`, descending into nested
/// statements but not into functions.
pub(super) fn var_names(stmt: &Stmt, out: &mut Vec<(String, Span)>) {
    let var_decl = |v: &swc_ecma_ast::VarDecl, out: &mut Vec<(String, Span)>| {
        if v.kind == VarDeclKind::Var {
            for d in &v.decls {
                pat_names(&d.name, out);
            }
        }
    };
    match stmt {
        Stmt::Decl(Decl::Var(v)) => var_decl(v, out),
        Stmt::Block(b) => b.stmts.iter().for_each(|s| var_names(s, out)),
        Stmt::If(i) => {
            var_names(&i.cons, out);
            if let Some(alt) = &i.alt {
                var_names(alt, out);
            }
        }
        Stmt::While(w) => var_names(&w.body, out),
        Stmt::DoWhile(w) => var_names(&w.body, out),
        Stmt::For(f) => {
            if let Some(VarDeclOrExpr::VarDecl(v)) = &f.init {
                var_decl(v, out);
            }
            var_names(&f.body, out);
        }
        Stmt::ForIn(swc_ecma_ast::ForInStmt { left, body, .. })
        | Stmt::ForOf(swc_ecma_ast::ForOfStmt { left, body, .. }) => {
            if let ForHead::VarDecl(v) = left {
                var_decl(v, out);
            }
            var_names(body, out);
        }
        Stmt::Labeled(l) => var_names(&l.body, out),
        Stmt::Try(t) => {
            t.block.stmts.iter().for_each(|s| var_names(s, out));
            if let Some(h) = &t.handler {
                h.body.stmts.iter().for_each(|s| var_names(s, out));
            }
            if let Some(f) = &t.finalizer {
                f.stmts.iter().for_each(|s| var_names(s, out));
            }
        }
        Stmt::Switch(s) => s
            .cases
            .iter()
            .flat_map(|c| c.cons.iter())
            .for_each(|s| var_names(s, out)),
        _ => {}
    }
}

/// Collect the names (with spans) a binding pattern binds.
fn pat_names(pat: &Pat, out: &mut Vec<(String, Span)>) {
    match pat {
        Pat::Ident(ident) => out.push((ident.id.sym.to_string(), ident.id.span)),
        Pat::Array(arr) => arr.elems.iter().flatten().for_each(|e| pat_names(e, out)),
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => pat_names(&kv.value, out),
                    ObjectPatProp::Assign(a) => out.push((a.key.id.sym.to_string(), a.key.id.span)),
                    ObjectPatProp::Rest(r) => pat_names(&r.arg, out),
                }
            }
        }
        Pat::Rest(r) => pat_names(&r.arg, out),
        Pat::Assign(a) => pat_names(&a.left, out),
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}
