//! Early errors of the strict-mode static semantics that swc's parser does
//! not report, checked over the whole program before lowering:
//!
//! - a name declared twice lexically in one scope, or both lexically and by
//!   `var` (a block, a `switch` case block, a function body, the script);
//!   in a block every function declaration is lexical, at a function's (or
//!   the script's) top level it is var-scoped;
//! - a parameter redeclared lexically in its function's body, a catch
//!   parameter redeclared lexically in its block, and a duplicate parameter
//!   name of any function (every function is strict);
//! - an `async function` declaration in statement position (`if (x) async
//!   function f(){}`);
//! - `await` (escaped or not) as an identifier inside an async body.

use std::collections::HashMap;

use swc_common::Span;
use swc_ecma_ast::{
    ArrowExpr, ArrowFunctionBody, BlockStmt, CatchClause, Constructor, Decl, FnExpr, ForHead,
    Function, FunctionBody, Ident, ObjectPatProp, ParamOrTsParamProp, Pat, Program, Stmt,
    SwitchStmt, VarDeclKind, VarDeclOrExpr,
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
        self.function(f.params.iter().map(|p| &p.pat), f.body.as_ref(), f.is_async);
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
            ArrowFunctionBody::FunctionBody(block) => {
                self.function(a.params.iter(), Some(block), a.is_async);
            }
            ArrowFunctionBody::Expr(expr) => {
                self.function(a.params.iter(), None, a.is_async);
                let outer_async = std::mem::replace(&mut self.in_async, a.is_async);
                expr.visit_with(self);
                self.in_async = outer_async;
            }
        }
    }

    fn visit_constructor(&mut self, c: &Constructor) {
        let params = c.params.iter().filter_map(|p| match p {
            ParamOrTsParamProp::Param(p) => Some(&p.pat),
            ParamOrTsParamProp::TsParamProp(_) => None,
        });
        self.function(params, c.body.as_ref(), false);
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
            Stmt::Labeled(l) => self.statement_position(&l.body),
            _ => {}
        }
        stmt.visit_children_with(self);
    }

    fn visit_ident(&mut self, ident: &Ident) {
        if self.in_async && ident.sym.as_ref() == "await" {
            self.fail(
                ident.span,
                "`await` cannot be used as an identifier in an async context".to_owned(),
            );
        }
    }
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
fn var_names(stmt: &Stmt, out: &mut Vec<(String, Span)>) {
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
