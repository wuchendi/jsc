#[allow(clippy::wildcard_imports)]
use super::*;

/// The greatest function arity reachable through a statement, including nested
/// functions, arrows, methods, accessors, and classes, and every expression
/// position that can hold one (default parameter values, destructuring
/// defaults, computed keys, loop heads). Used to fix the uniform ABI's `K`
/// before any body is lowered. Statements outside the supported subset contribute
/// `0` (they error later during lowering).
pub(super) fn scan_stmt_arity(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Expr(e) => scan_expr_arity(&e.expr),
        Stmt::Decl(Decl::Var(v)) => scan_var_decl_arity(v),
        Stmt::Decl(Decl::Fn(f)) => scan_function_arity(&f.function),
        Stmt::Decl(Decl::Class(c)) => scan_class_arity(&c.class),
        Stmt::Return(r) => r.arg.as_deref().map_or(0, scan_expr_arity),
        Stmt::Throw(t) => scan_expr_arity(&t.arg),
        Stmt::If(i) => {
            let mut m = scan_expr_arity(&i.test).max(scan_stmt_arity(&i.cons));
            if let Some(alt) = &i.alt {
                m = m.max(scan_stmt_arity(alt));
            }
            m
        }
        Stmt::While(w) => scan_expr_arity(&w.test).max(scan_stmt_arity(&w.body)),
        Stmt::DoWhile(w) => scan_expr_arity(&w.test).max(scan_stmt_arity(&w.body)),
        Stmt::For(f) => {
            let mut m = 0;
            if let Some(VarDeclOrExpr::VarDecl(v)) = &f.init {
                m = m.max(scan_var_decl_arity(v));
            } else if let Some(VarDeclOrExpr::Expr(e)) = &f.init {
                m = m.max(scan_expr_arity(e));
            }
            if let Some(t) = &f.test {
                m = m.max(scan_expr_arity(t));
            }
            if let Some(u) = &f.update {
                m = m.max(scan_expr_arity(u));
            }
            m.max(scan_stmt_arity(&f.body))
        }
        Stmt::ForIn(f) => scan_for_head_arity(&f.left)
            .max(scan_expr_arity(&f.right))
            .max(scan_stmt_arity(&f.body)),
        Stmt::ForOf(f) => scan_for_head_arity(&f.left)
            .max(scan_expr_arity(&f.right))
            .max(scan_stmt_arity(&f.body)),
        Stmt::Labeled(l) => scan_stmt_arity(&l.body),
        Stmt::Switch(s) => {
            let mut m = scan_expr_arity(&s.discriminant);
            for case in &s.cases {
                if let Some(t) = &case.test {
                    m = m.max(scan_expr_arity(t));
                }
                m = m.max(scan_stmts_arity(&case.cons));
            }
            m
        }
        Stmt::Block(b) => scan_stmts_arity(&b.stmts),
        Stmt::Try(t) => {
            let mut m = scan_stmts_arity(&t.block.stmts);
            if let Some(h) = &t.handler {
                m = m.max(scan_stmts_arity(&h.body.stmts));
            }
            if let Some(f) = &t.finalizer {
                m = m.max(scan_stmts_arity(&f.stmts));
            }
            m
        }
        _ => 0,
    }
}

/// The greatest function arity reachable through a `var`/`let`/`const`
/// declaration: each binding pattern (its destructuring defaults) and
/// initializer.
fn scan_var_decl_arity(v: &swc_ecma_ast::VarDecl) -> usize {
    v.decls
        .iter()
        .map(|d| scan_pat_arity(&d.name).max(d.init.as_deref().map_or(0, scan_expr_arity)))
        .max()
        .unwrap_or(0)
}

/// The greatest function arity reachable through a for-in/for-of head.
fn scan_for_head_arity(head: &ForHead) -> usize {
    match head {
        ForHead::VarDecl(v) => scan_var_decl_arity(v),
        ForHead::Pat(p) => scan_pat_arity(p),
        ForHead::UsingDecl(_) => 0,
    }
}

/// A function's own param count, its parameter default values, and its body.
fn scan_function_arity(function: &swc_ecma_ast::Function) -> usize {
    let own = function.params.len();
    let params = function
        .params
        .iter()
        .map(|p| scan_pat_arity(&p.pat))
        .max()
        .unwrap_or(0);
    let body = function
        .body
        .as_ref()
        .map_or(0, |b| scan_stmts_arity(&b.stmts));
    own.max(params).max(body)
}

/// The greatest function arity reachable through a binding or assignment
/// pattern: its default values and computed keys.
fn scan_pat_arity(pat: &Pat) -> usize {
    match pat {
        Pat::Assign(a) => scan_pat_arity(&a.left).max(scan_expr_arity(&a.right)),
        Pat::Array(a) => scan_array_pat_arity(a),
        Pat::Object(o) => scan_object_pat_arity(o),
        Pat::Rest(r) => scan_pat_arity(&r.arg),
        Pat::Expr(e) => scan_expr_arity(e),
        Pat::Ident(_) | Pat::Invalid(_) => 0,
    }
}

/// [`scan_pat_arity`] for an array pattern's elements.
fn scan_array_pat_arity(pat: &ArrayPat) -> usize {
    pat.elems
        .iter()
        .flatten()
        .map(scan_pat_arity)
        .max()
        .unwrap_or(0)
}

/// [`scan_pat_arity`] for an object pattern's properties.
fn scan_object_pat_arity(pat: &ObjectPat) -> usize {
    pat.props
        .iter()
        .map(|p| match p {
            ObjectPatProp::KeyValue(kv) => {
                scan_prop_name_arity(&kv.key).max(scan_pat_arity(&kv.value))
            }
            ObjectPatProp::Assign(a) => a.value.as_deref().map_or(0, scan_expr_arity),
            ObjectPatProp::Rest(r) => scan_pat_arity(&r.arg),
        })
        .max()
        .unwrap_or(0)
}

/// A computed property name's key expression (`{ [expr]: .. }`).
fn scan_prop_name_arity(key: &PropName) -> usize {
    match key {
        PropName::Computed(c) => scan_expr_arity(&c.expr),
        _ => 0,
    }
}

/// The greatest function arity reachable through a class: its constructor AND
/// every method/getter/setter (instance or static) lift to functions, so each
/// one's own param count, parameter defaults, and body must widen `K` like any
/// function/arrow (a setter matters especially — it is invoked with one
/// argument by `obj.x = v`, an assignment no call-site arity scan covers); the
/// `extends` expression and the (instance or static) field initializers are
/// scanned too.
fn scan_class_arity(class: &swc_ecma_ast::Class) -> usize {
    let heritage = class.super_class.as_deref().map_or(0, scan_expr_arity);
    class
        .body
        .iter()
        .map(|member| match member {
            ClassMember::Constructor(ctor) => {
                let own = ctor.params.len();
                let params = ctor
                    .params
                    .iter()
                    .map(|p| match p {
                        ParamOrTsParamProp::Param(p) => scan_pat_arity(&p.pat),
                        ParamOrTsParamProp::TsParamProp(_) => 0,
                    })
                    .max()
                    .unwrap_or(0);
                let body = ctor.body.as_ref().map_or(0, |b| scan_stmts_arity(&b.stmts));
                own.max(params).max(body)
            }
            ClassMember::Method(m) => scan_function_arity(&m.function),
            ClassMember::ClassProp(p) => p.value.as_deref().map_or(0, scan_expr_arity),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
        .max(heritage)
}

/// The greatest function arity reachable across a statement list.
fn scan_stmts_arity(stmts: &[Stmt]) -> usize {
    stmts.iter().map(scan_stmt_arity).max().unwrap_or(0)
}

/// The greatest arity a call (plain or optional) needs: its callee, its
/// arguments, and its argument count.
fn scan_call_arity(callee: Option<&Expr>, args: &[ExprOrSpread]) -> usize {
    let mut m = callee.map_or(0, scan_expr_arity);
    for arg in args {
        m = m.max(scan_expr_arity(&arg.expr));
    }
    // The uniform ABI pads every call to `K` positional slots, so a call
    // passing more arguments than any function's declared arity must still
    // widen `K` — otherwise a trailing rest parameter would receive
    // only `K` of them, the surplus having been dropped at the call site.
    m = m.max(args.len());
    // An array higher-order method invokes its callback with extra
    // positional args beyond the callback's own arity — `(element, index)`
    // for map/filter/forEach and `(acc, element, index)` for reduce. Widen
    // the uniform ABI so those pushes never overflow the shared funcref
    // signature (a `call_indirect` type mismatch would trap). Detected
    // syntactically; over-counting under a shadowed name is harmless.
    if let Some(Expr::Member(mem)) = callee
        && let MemberProp::Ident(id) = &mem.prop
    {
        m = m.max(match id.sym.as_ref() {
            "reduce" => 3,
            "map" | "filter" | "forEach" => 2,
            // A `JSON.stringify` replacer / `JSON.parse` reviver is
            // called with `(key, value)`.
            "stringify" | "parse" if args.len() > 1 => 2,
            _ => 0,
        });
    }
    m
}

/// The greatest arity reachable through a member expression: its object and
/// any computed key.
fn scan_member_arity(m: &swc_ecma_ast::MemberExpr) -> usize {
    let mut r = scan_expr_arity(&m.obj);
    if let MemberProp::Computed(c) = &m.prop {
        r = r.max(scan_expr_arity(&c.expr));
    }
    r
}

/// The greatest function arity reachable through an expression, including any
/// function, arrow, method, accessor, or class it contains (each one's own
/// param count, parameter defaults, and body).
fn scan_expr_arity(expr: &Expr) -> usize {
    match expr {
        Expr::Arrow(a) => {
            let own = a.params.len();
            let params = a.params.iter().map(scan_pat_arity).max().unwrap_or(0);
            let body = match &*a.body {
                ArrowFunctionBody::FunctionBody(b) => scan_stmts_arity(&b.stmts),
                ArrowFunctionBody::Expr(e) => scan_expr_arity(e),
            };
            own.max(params).max(body)
        }
        Expr::Fn(f) => scan_function_arity(&f.function),
        Expr::Class(c) => scan_class_arity(&c.class),
        Expr::Paren(p) => scan_expr_arity(&p.expr),
        Expr::Bin(b) => scan_expr_arity(&b.left).max(scan_expr_arity(&b.right)),
        Expr::Unary(u) => scan_expr_arity(&u.arg),
        Expr::Update(u) => scan_expr_arity(&u.arg),
        Expr::Cond(c) => scan_expr_arity(&c.test)
            .max(scan_expr_arity(&c.cons))
            .max(scan_expr_arity(&c.alt)),
        // The target's computed member key or destructuring defaults, then the
        // assigned value.
        Expr::Assign(a) => {
            let target = match &a.left {
                AssignTarget::Simple(SimpleAssignTarget::Member(m)) => scan_member_arity(m),
                AssignTarget::Pat(AssignTargetPat::Array(p)) => scan_array_pat_arity(p),
                AssignTarget::Pat(AssignTargetPat::Object(p)) => scan_object_pat_arity(p),
                _ => 0,
            };
            target.max(scan_expr_arity(&a.right))
        }
        Expr::Call(c) => match &c.callee {
            Callee::Expr(e) => scan_call_arity(Some(e), &c.args),
            _ => scan_call_arity(None, &c.args),
        },
        Expr::Member(m) => scan_member_arity(m),
        // `a?.b`, `a?.[k]`, `f?.(args)`: an optional call pads to `K` like a
        // plain call.
        Expr::OptChain(chain) => match chain.base.as_ref() {
            OptChainBase::Member(m) => scan_member_arity(m),
            OptChainBase::Call(c) => scan_call_arity(Some(&c.callee), &c.args),
        },
        // `new A(args)` invokes the constructor through the uniform ABI, so
        // `K` must cover the argument count (like a plain call) and any arrow arg.
        Expr::New(n) => {
            let mut m = scan_expr_arity(&n.callee);
            if let Some(args) = &n.args {
                m = m.max(args.len());
                for arg in args {
                    m = m.max(scan_expr_arity(&arg.expr));
                }
            }
            // `new Promise(executor)` invokes the executor with TWO
            // synthetic arguments (`resolve`, `reject`) through the uniform ABI, so
            // `K` must be at least 2 regardless of the executor's declared arity.
            // Over-counting under a shadowed `Promise` is harmless (widens `K`).
            if let Expr::Ident(id) = n.callee.as_ref()
                && id.sym.as_ref() == "Promise"
            {
                m = m.max(2);
            }
            m
        }
        // `await e`: scan the awaited expression so a `new Promise` executor
        // (or any function) nested under it still widens the uniform ABI arity.
        Expr::Await(a) => scan_expr_arity(&a.arg),
        Expr::Yield(y) => y.arg.as_deref().map_or(0, scan_expr_arity),
        Expr::Tpl(t) => t
            .exprs
            .iter()
            .map(|e| scan_expr_arity(e))
            .max()
            .unwrap_or(0),
        Expr::Array(a) => a
            .elems
            .iter()
            .flatten()
            .map(|e| scan_expr_arity(&e.expr))
            .max()
            .unwrap_or(0),
        Expr::Object(o) => o
            .props
            .iter()
            .filter_map(|p| match p {
                PropOrSpread::Prop(prop) => match prop.as_ref() {
                    Prop::KeyValue(kv) => {
                        Some(scan_prop_name_arity(&kv.key).max(scan_expr_arity(&kv.value)))
                    }
                    // A method or accessor lifts to a function; its own param
                    // count, defaults, and body must widen `K` like any
                    // arrow/function (a setter is invoked with one argument by
                    // `obj.x = v`, which no call-site scan covers).
                    Prop::Method(m) => {
                        Some(scan_prop_name_arity(&m.key).max(scan_function_arity(&m.function)))
                    }
                    Prop::Getter(g) => {
                        Some(scan_prop_name_arity(&g.key).max(scan_function_arity(&g.function)))
                    }
                    Prop::Setter(set) => {
                        Some(scan_prop_name_arity(&set.key).max(scan_function_arity(&set.function)))
                    }
                    _ => None,
                },
                PropOrSpread::Spread(_) => None,
            })
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}
