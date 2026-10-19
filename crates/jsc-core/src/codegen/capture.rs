#[allow(clippy::wildcard_imports)]
use super::*;

/// The unique id of a binding: the byte offset of its declaring identifier in
/// the source. Because a declaration identifier occupies a unique source span,
/// this identifies the binding independently of scope or traversal order, so
/// the capture-analysis pass and the lowering pass agree on which bindings are
/// captured without threading any shared counter between them.
pub(super) fn binding_id(ident: &swc_ecma_ast::BindingIdent) -> u32 {
    ident.id.span.lo.0
}

/// Compute the capture analysis: the set of captured binding ids and the
/// names of the top-level bindings referenced by top-level function declarations.
///
/// A binding is captured when it is referenced from inside a nested
/// function/arrow, i.e. the reference resolves across at least one function
/// boundary. Top-level function declarations are analyzed NESTED under the
/// `main` frame: a reference inside a declaration's body (or anything nested in
/// it) that resolves into `main`'s outermost scope records the name as a
/// **module binding**, which lowering stores in a module cell reachable from
/// every function. Top-level function NAMES are not entered as bindings, so a
/// call to a sibling top-level function is a static reference, not a capture.
/// Nested arrows and function expressions/declarations push a further scope.
pub(super) fn analyze_captures(top_stmts: &[&Stmt], fn_decls: &[&FnDecl]) -> CaptureAnalysis {
    let mut resolver = CaptureResolver::default();
    resolver.enter_function();
    for stmt in top_stmts {
        resolver.stmt(stmt);
    }
    // Analyze each top-level function body NESTED under `main` (frame 0), after
    // all of `main`'s own bindings are declared (so a captured `let`/`const`
    // resolves regardless of source order, matching function-declaration
    // hoisting). A reference reaching `main` records a module binding.
    resolver.in_top_fn = true;
    for fn_decl in fn_decls {
        resolver.enter_function();
        for param in &fn_decl.function.params {
            resolver.declare_param(&param.pat);
        }
        if let Some(block) = &fn_decl.function.body {
            for stmt in &block.stmts {
                resolver.stmt(stmt);
            }
        }
        resolver.exit_function();
    }
    resolver.exit_function();
    // Finalize the object shapes: keep only the candidates whose binding name is
    // never reassigned, so the recorded literal shape is provably fixed.
    let object_shapes = resolver
        .shape_candidates
        .into_iter()
        .filter(|(_, name, _)| !resolver.assigned.contains(name))
        .map(|(id, _, keys)| (id, keys))
        .collect();
    // The `var`-declared module bindings (hoisted, initialized to `undefined`).
    let mut module_vars = std::collections::HashSet::new();
    for stmt in top_stmts {
        if let Stmt::Decl(Decl::Var(v)) = stmt
            && v.kind == VarDeclKind::Var
        {
            for d in &v.decls {
                pat_names(&d.name, &mut module_vars);
            }
        }
    }
    module_vars.retain(|name| resolver.module_names.contains(name));
    CaptureAnalysis {
        captured: resolver.captured,
        module_vars,
        module_names: resolver.module_names,
        object_shapes,
    }
}

/// Collect every identifier a (possibly destructuring) binding pattern binds.
fn pat_names(pat: &Pat, names: &mut std::collections::HashSet<String>) {
    match pat {
        Pat::Ident(ident) => {
            names.insert(ident.id.sym.as_ref().to_owned());
        }
        Pat::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                pat_names(elem, names);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::KeyValue(kv) => pat_names(&kv.value, names),
                    ObjectPatProp::Assign(assign) => {
                        names.insert(assign.key.id.sym.as_ref().to_owned());
                    }
                    ObjectPatProp::Rest(rest) => pat_names(&rest.arg, names),
                }
            }
        }
        Pat::Rest(rest) => pat_names(&rest.arg, names),
        Pat::Assign(assign) => pat_names(&assign.left, names),
        Pat::Expr(_) | Pat::Invalid(_) => {}
    }
}

impl CaptureResolver {
    fn enter_function(&mut self) {
        self.frames.push(vec![HashMap::new()]);
    }

    fn exit_function(&mut self) {
        self.frames.pop();
    }

    pub(super) fn push_scope(&mut self) {
        if let Some(frame) = self.frames.last_mut() {
            frame.push(HashMap::new());
        }
    }

    pub(super) fn pop_scope(&mut self) {
        if let Some(frame) = self.frames.last_mut() {
            frame.pop();
        }
    }

    pub(super) fn declare(&mut self, name: &str, id: u32) {
        if let Some(scope) = self.frames.last_mut().and_then(|f| f.last_mut()) {
            scope.insert(name.to_owned(), id);
        }
    }

    /// Declare a function/arrow parameter binding for capture analysis: a plain
    /// `Pat::Ident` or the inner ident of a trailing rest parameter (`...rest`),
    /// so a nested closure referencing the rest binding marks it captured.
    fn declare_param(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(ident) => self.declare(ident.id.sym.as_ref(), binding_id(ident)),
            Pat::Rest(rest) => {
                if let Pat::Ident(ident) = rest.arg.as_ref() {
                    self.declare(ident.id.sym.as_ref(), binding_id(ident));
                }
            }
            _ => {}
        }
    }

    /// Declare every binding introduced by a (possibly destructuring) pattern,
    /// mirroring [`LowerCtx::lower_binding_pattern`] so a nested closure that
    /// references a destructured binding marks it captured. A default expression
    /// is analyzed in place (it may reference outer bindings). Holes and rest
    /// targets recurse; unsupported pattern kinds simply bind nothing (lowering
    /// rejects them with a span).
    fn declare_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(ident) => self.declare(ident.id.sym.as_ref(), binding_id(ident)),
            Pat::Array(arr) => {
                for elem in arr.elems.iter().flatten() {
                    self.declare_pat(elem);
                }
            }
            Pat::Object(obj) => {
                for prop in &obj.props {
                    match prop {
                        ObjectPatProp::KeyValue(kv) => self.declare_pat(&kv.value),
                        ObjectPatProp::Assign(assign) => {
                            if let Some(default) = &assign.value {
                                self.expr(default);
                            }
                            self.declare(assign.key.id.sym.as_ref(), binding_id(&assign.key));
                        }
                        ObjectPatProp::Rest(rest) => self.declare_pat(&rest.arg),
                    }
                }
            }
            Pat::Rest(rest) => self.declare_pat(&rest.arg),
            Pat::Assign(assign) => {
                self.expr(&assign.right);
                self.declare_pat(&assign.left);
            }
            _ => {}
        }
    }

    /// Record a `let`/`const` binding initialized directly with an all-static-key
    /// object literal as an object-shape candidate (static-shape unknown-method
    /// guard). Only simple identifier bindings qualify; `var` is excluded because
    /// its hoisting/redeclaration makes a single fixed shape unsound. The candidate
    /// is dropped later if the name is ever reassigned (see [`analyze_captures`]).
    fn record_shape_candidate(&mut self, kind: VarDeclKind, name: &Pat, init: &Expr) {
        if !matches!(kind, VarDeclKind::Let | VarDeclKind::Const) {
            return;
        }
        let Pat::Ident(ident) = name else { return };
        if let Some(keys) = object_literal_keys(strip_parens(init)) {
            self.shape_candidates
                .push((binding_id(ident), ident.id.sym.as_ref().to_owned(), keys));
        }
    }

    /// Record every simple-identifier target of a destructuring assignment
    /// (`[o] = ..`, `({o} = ..)`) as reassigned, so a rebound binding cannot keep a
    /// trusted object shape.
    fn mark_assign_target_pat(&mut self, pat: &AssignTargetPat) {
        match pat {
            AssignTargetPat::Array(arr) => self.mark_array_pat(arr),
            AssignTargetPat::Object(obj) => self.mark_object_pat(obj),
            AssignTargetPat::Invalid(_) => {}
        }
    }

    /// Record every simple-identifier binding within an assignment/destructuring
    /// pattern as reassigned.
    fn mark_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(ident) => {
                self.assigned.insert(ident.id.sym.as_ref().to_owned());
            }
            Pat::Array(arr) => self.mark_array_pat(arr),
            Pat::Object(obj) => self.mark_object_pat(obj),
            Pat::Rest(rest) => self.mark_pat(&rest.arg),
            Pat::Assign(assign) => self.mark_pat(&assign.left),
            Pat::Expr(expr) => {
                if let Expr::Ident(ident) = expr.as_ref() {
                    self.assigned.insert(ident.sym.as_ref().to_owned());
                }
            }
            Pat::Invalid(_) => {}
        }
    }

    fn mark_array_pat(&mut self, arr: &ArrayPat) {
        for elem in arr.elems.iter().flatten() {
            self.mark_pat(elem);
        }
    }

    fn mark_object_pat(&mut self, obj: &ObjectPat) {
        for prop in &obj.props {
            match prop {
                ObjectPatProp::KeyValue(kv) => self.mark_pat(&kv.value),
                ObjectPatProp::Assign(assign) => {
                    self.assigned.insert(assign.key.id.sym.as_ref().to_owned());
                }
                ObjectPatProp::Rest(rest) => self.mark_pat(&rest.arg),
            }
        }
    }

    /// Resolve a name reference; if it binds in an enclosing function frame,
    /// mark that binding captured. When the reference reaches the `main` frame
    /// (0) from within a top-level function declaration, the name is also
    /// recorded as a module binding.
    fn reference(&mut self, name: &str) {
        let top = self.frames.len().saturating_sub(1);
        for (fi, frame) in self.frames.iter().enumerate().rev() {
            for scope in frame.iter().rev() {
                if let Some(&id) = scope.get(name) {
                    if fi != top {
                        self.captured.insert(id);
                        if fi == 0 && self.in_top_fn && !self.module_names.iter().any(|n| n == name)
                        {
                            self.module_names.push(name.to_owned());
                        }
                    }
                    return;
                }
            }
        }
    }

    /// Analyze a statement for scoping and references, mirroring
    /// [`LowerCtx::lower_stmt`].
    #[allow(clippy::too_many_lines)]
    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Expr(e) => self.expr(&e.expr),
            Stmt::Decl(Decl::Var(v)) => {
                for d in &v.decls {
                    if let Some(init) = &d.init {
                        self.expr(init);
                        self.record_shape_candidate(v.kind, &d.name, init);
                    }
                    self.declare_pat(&d.name);
                }
            }
            Stmt::Decl(Decl::Fn(f)) => {
                // A nested function declaration: analyze its body in a fresh
                // frame, then bind its name in the enclosing scope.
                self.function(
                    &f.function.params,
                    f.function.body.as_ref().map(|b| &b.stmts),
                );
                self.declare(f.ident.sym.as_ref(), f.ident.span.lo.0);
            }
            Stmt::Decl(Decl::Class(c)) => self.class_decl(c),
            Stmt::Block(b) => {
                self.push_scope();
                for s in &b.stmts {
                    self.stmt(s);
                }
                self.pop_scope();
            }
            Stmt::If(i) => {
                self.expr(&i.test);
                self.stmt(&i.cons);
                if let Some(alt) = &i.alt {
                    self.stmt(alt);
                }
            }
            Stmt::While(w) => {
                self.expr(&w.test);
                self.stmt(&w.body);
            }
            Stmt::DoWhile(w) => {
                self.stmt(&w.body);
                self.expr(&w.test);
            }
            Stmt::Labeled(l) => self.stmt(&l.body),
            Stmt::ForIn(f) => self.loop_head(&f.left, &f.right, &f.body),
            Stmt::For(f) => {
                self.push_scope();
                if let Some(VarDeclOrExpr::VarDecl(v)) = &f.init {
                    for d in &v.decls {
                        if let Some(init) = &d.init {
                            self.expr(init);
                        }
                        if let Pat::Ident(ident) = &d.name {
                            self.declare(ident.id.sym.as_ref(), binding_id(ident));
                        }
                    }
                } else if let Some(VarDeclOrExpr::Expr(e)) = &f.init {
                    self.expr(e);
                }
                if let Some(t) = &f.test {
                    self.expr(t);
                }
                if let Some(u) = &f.update {
                    self.expr(u);
                }
                self.stmt(&f.body);
                self.pop_scope();
            }
            Stmt::ForOf(f) => self.loop_head(&f.left, &f.right, &f.body),
            Stmt::Switch(s) => {
                self.expr(&s.discriminant);
                for case in &s.cases {
                    if let Some(t) = &case.test {
                        self.expr(t);
                    }
                    for s in &case.cons {
                        self.stmt(s);
                    }
                }
            }
            Stmt::Return(r) => {
                if let Some(e) = &r.arg {
                    self.expr(e);
                }
            }
            Stmt::Throw(t) => self.expr(&t.arg),
            Stmt::Try(t) => {
                // The guarded block is its own scope.
                self.analyze_scoped(&t.block.stmts);
                if let Some(handler) = &t.handler {
                    // The catch clause scope binds the (optional) parameter before
                    // its body, so a nested closure referencing it marks it captured.
                    self.push_scope();
                    if let Some(Pat::Ident(ident)) = &handler.param {
                        self.declare(ident.id.sym.as_ref(), binding_id(ident));
                    }
                    for s in &handler.body.stmts {
                        self.stmt(s);
                    }
                    self.pop_scope();
                }
                // The finally block is its own scope.
                if let Some(finalizer) = &t.finalizer {
                    self.analyze_scoped(&finalizer.stmts);
                }
            }
            _ => {}
        }
    }

    /// Analyze a class declaration: bind the class name in the enclosing
    /// scope FIRST, so a `new C()` inside the constructor or a method body resolves
    /// to it and is marked captured across the function boundary. Each
    /// constructor and instance-method body is then a fresh function frame (like a
    /// nested fn declaration), so a binding it references from an enclosing scope
    /// (including the class name) is captured.
    fn class_decl(&mut self, c: &ClassDecl) {
        self.declare(c.ident.sym.as_ref(), c.ident.span.lo.0);
        // A derived class's constructor/methods implicitly reference the superclass
        // binding through `super` (E-series): mark it referenced inside each body
        // frame so a captured parent is boxed into a cell the lifted body can read,
        // exactly like a `new Parent()` reference would.
        let super_name = c.class.super_class.as_ref().and_then(|e| match e.as_ref() {
            Expr::Ident(id) => Some(id.sym.as_ref().to_owned()),
            _ => None,
        });
        for member in &c.class.body {
            match member {
                ClassMember::Constructor(ctor) => {
                    self.enter_function();
                    for param in &ctor.params {
                        if let ParamOrTsParamProp::Param(p) = param {
                            self.declare_param(&p.pat);
                        }
                    }
                    if let Some(sup) = &super_name {
                        self.reference(sup);
                    }
                    if let Some(body) = &ctor.body {
                        for s in &body.stmts {
                            self.stmt(s);
                        }
                    }
                    self.exit_function();
                }
                // A method body is a fresh function frame, mirroring the
                // constructor. Inlined (rather than the `function` helper) so a
                // derived class can reference its superclass inside the frame.
                ClassMember::Method(m) => {
                    self.enter_function();
                    for param in &m.function.params {
                        self.declare_param(&param.pat);
                    }
                    if let Some(sup) = &super_name {
                        self.reference(sup);
                    }
                    if let Some(body) = &m.function.body {
                        for s in &body.stmts {
                            self.stmt(s);
                        }
                    }
                    self.exit_function();
                }
                // A field initializer runs in its own `this`-binding scope, so a
                // binding it references from an enclosing scope is captured.
                ClassMember::ClassProp(p) => {
                    if let Some(value) = &p.value {
                        self.enter_function();
                        self.expr(value);
                        self.exit_function();
                    }
                }
                _ => {}
            }
        }
    }

    /// Analyze a `for-of` / `for-in` loop, mirroring [`LowerCtx::lower_for_of`] /
    /// [`LowerCtx::lower_for_in`]: the head opens a scope holding the loop
    /// binding, rebound each iteration, so a nested closure referencing it marks
    /// it captured.
    fn loop_head(&mut self, left: &ForHead, right: &Expr, body: &Stmt) {
        self.push_scope();
        self.expr(right);
        if let ForHead::VarDecl(v) = left
            && let [declarator] = v.decls.as_slice()
            && let Pat::Ident(ident) = &declarator.name
        {
            self.declare(ident.id.sym.as_ref(), binding_id(ident));
        }
        self.stmt(body);
        self.pop_scope();
    }

    /// Analyze `stmts` inside a fresh lexical scope (a plain block or a `try` /
    /// `finally` body), so block-scoped bindings do not leak outward.
    fn analyze_scoped(&mut self, stmts: &[Stmt]) {
        self.push_scope();
        for s in stmts {
            self.stmt(s);
        }
        self.pop_scope();
    }

    /// Analyze an expression's references, entering a fresh frame for arrows and
    /// function expressions.
    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.reference(ident.sym.as_ref()),
            Expr::Paren(p) => self.expr(&p.expr),
            Expr::Arrow(a) => {
                let body = match a.body.as_ref() {
                    ArrowFunctionBody::FunctionBody(b) => AnalyzeBody::Block(&b.stmts),
                    ArrowFunctionBody::Expr(e) => AnalyzeBody::Expr(e),
                };
                self.arrow_or_fn(&a.params, body);
            }
            Expr::Fn(f) => {
                self.function(
                    &f.function.params,
                    f.function.body.as_ref().map(|b| &b.stmts),
                );
            }
            Expr::Assign(a) => {
                if let AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) = &a.left {
                    self.reference(ident.id.sym.as_ref());
                    self.assigned.insert(ident.id.sym.as_ref().to_owned());
                } else if let AssignTarget::Simple(SimpleAssignTarget::Member(m)) = &a.left {
                    self.expr(&m.obj);
                    if let MemberProp::Computed(c) = &m.prop {
                        self.expr(&c.expr);
                    }
                } else if let AssignTarget::Pat(pat) = &a.left {
                    // A destructuring assignment (`[o] = ..`, `({o} = ..)`) rebinds
                    // its targets — record every simple-ident target as reassigned.
                    self.mark_assign_target_pat(pat);
                }
                self.expr(&a.right);
            }
            Expr::Update(u) => {
                if let Expr::Ident(ident) = u.arg.as_ref() {
                    self.assigned.insert(ident.sym.as_ref().to_owned());
                }
                self.expr(&u.arg);
            }
            Expr::Bin(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expr::Unary(u) => self.expr(&u.arg),
            Expr::Cond(c) => {
                self.expr(&c.test);
                self.expr(&c.cons);
                self.expr(&c.alt);
            }
            Expr::Call(c) => {
                if let Callee::Expr(e) = &c.callee {
                    self.expr(e);
                }
                for arg in &c.args {
                    self.expr(&arg.expr);
                }
            }
            Expr::Member(m) => {
                self.expr(&m.obj);
                if let MemberProp::Computed(c) = &m.prop {
                    self.expr(&c.expr);
                }
            }
            // `new A(args)`: reference the class binding (so a captured `A`
            // is marked) and analyze each argument.
            Expr::New(n) => {
                self.expr(&n.callee);
                if let Some(args) = &n.args {
                    for arg in args {
                        self.expr(&arg.expr);
                    }
                }
            }
            // `await e`: descend into the awaited expression so a closure
            // nested under it (e.g. `new Promise((res) => ...)`) still has its
            // captures analyzed. Without this the awaited sub-tree is invisible to
            // capture analysis and a closure's captured binding is never boxed.
            Expr::Await(a) => self.expr(&a.arg),
            Expr::Yield(y) => {
                if let Some(arg) = &y.arg {
                    self.expr(arg);
                }
            }
            Expr::Tpl(t) => {
                for e in &t.exprs {
                    self.expr(e);
                }
            }
            Expr::Seq(s) => {
                for e in &s.exprs {
                    self.expr(e);
                }
            }
            Expr::OptChain(c) => self.opt_chain(c),
            Expr::Array(a) => {
                for e in a.elems.iter().flatten() {
                    self.expr(&e.expr);
                }
            }
            Expr::Object(o) => {
                for prop in &o.props {
                    if let PropOrSpread::Prop(prop) = prop {
                        match prop.as_ref() {
                            Prop::KeyValue(kv) => self.expr(&kv.value),
                            // Shorthand `{n}` references the outer binding `n`.
                            Prop::Shorthand(ident) => self.reference(ident.sym.as_ref()),
                            // A method body is a fresh function frame, so a
                            // binding it references from an enclosing scope is
                            // captured — mirror the arrow/function analysis.
                            Prop::Method(method) => self.function(
                                &method.function.params,
                                method.function.body.as_ref().map(|b| &b.stmts),
                            ),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Analyze an optional chain's base (`o?.p`, `o?.[k]`, `f?.(args)`).
    fn opt_chain(&mut self, chain: &OptChainExpr) {
        match chain.base.as_ref() {
            OptChainBase::Member(m) => {
                self.expr(&m.obj);
                if let MemberProp::Computed(k) = &m.prop {
                    self.expr(&k.expr);
                }
            }
            OptChainBase::Call(call) => {
                self.expr(&call.callee);
                for arg in &call.args {
                    self.expr(&arg.expr);
                }
            }
        }
    }

    /// Analyze an arrow (expression- or block-bodied) in a fresh function frame.
    fn arrow_or_fn(&mut self, params: &[Pat], body: AnalyzeBody<'_>) {
        self.enter_function();
        for param in params {
            self.declare_param(param);
        }
        match body {
            AnalyzeBody::Block(stmts) => {
                for s in stmts {
                    self.stmt(s);
                }
            }
            AnalyzeBody::Expr(e) => self.expr(e),
        }
        self.exit_function();
    }

    /// Analyze a `function`-keyword function (declaration or expression) in a
    /// fresh function frame.
    fn function(&mut self, params: &[swc_ecma_ast::Param], body: Option<&Vec<Stmt>>) {
        self.enter_function();
        for param in params {
            self.declare_param(&param.pat);
        }
        if let Some(stmts) = body {
            for s in stmts {
                self.stmt(s);
            }
        }
        self.exit_function();
    }
}

/// The own keys of an object literal when every property has a statically-known
/// string key, or `None` when any property makes the shape non-enumerable at
/// compile time (a spread, a computed/numeric key, or a getter/setter). Mirrors
/// the key set [`LowerCtx::lower_object`] materializes, so a name absent here is
/// provably absent from the runtime object (the object model has no dynamic key
/// growth).
pub(super) fn object_literal_keys(expr: &Expr) -> Option<Vec<String>> {
    let Expr::Object(obj) = expr else { return None };
    let mut keys = Vec::with_capacity(obj.props.len());
    for prop in &obj.props {
        let PropOrSpread::Prop(prop) = prop else {
            return None;
        };
        let key = match prop.as_ref() {
            Prop::KeyValue(kv) => static_prop_key(&kv.key)?,
            Prop::Shorthand(ident) => ident.sym.as_ref().to_owned(),
            Prop::Method(method) => static_prop_key(&method.key)?,
            Prop::Getter(_) | Prop::Setter(_) | Prop::Assign(_) => return None,
        };
        keys.push(key);
    }
    Some(keys)
}

/// The static string key of a property name (an identifier or string literal),
/// or `None` for a computed or numeric key that is not known at compile time.
fn static_prop_key(name: &PropName) -> Option<String> {
    match name {
        PropName::Ident(id) => Some(id.sym.as_ref().to_owned()),
        PropName::Str(s) => Some(s.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Strip redundant parentheses from an expression, so a parenthesized object
/// literal initializer (`const o = ({..})`) is still recognized as a shape.
pub(super) fn strip_parens(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(p) => strip_parens(&p.expr),
        other => other,
    }
}
