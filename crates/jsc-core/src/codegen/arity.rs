#[allow(clippy::wildcard_imports)]
use super::*;

/// The greatest function arity reachable through a statement, including nested
/// arrow functions and function declarations. Used to fix the uniform ABI's `K`
/// before any body is lowered. Statements outside the supported subset contribute
/// `0` (they error later during lowering).
pub(super) fn scan_stmt_arity(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Expr(e) => scan_expr_arity(&e.expr),
        Stmt::Decl(Decl::Var(v)) => v
            .decls
            .iter()
            .filter_map(|d| d.init.as_deref())
            .map(scan_expr_arity)
            .max()
            .unwrap_or(0),
        Stmt::Decl(Decl::Fn(f)) => {
            let own = f.function.params.len();
            let body = f
                .function
                .body
                .as_ref()
                .map_or(0, |b| scan_stmts_arity(&b.stmts));
            own.max(body)
        }
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
        Stmt::For(f) => {
            let mut m = 0;
            if let Some(VarDeclOrExpr::VarDecl(v)) = &f.init {
                m = m.max(
                    v.decls
                        .iter()
                        .filter_map(|d| d.init.as_deref())
                        .map(scan_expr_arity)
                        .max()
                        .unwrap_or(0),
                );
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

/// The greatest function arity reachable through a class: its constructor AND
/// every method/getter/setter lift to functions, so each one's own param count
/// and body must widen `K` like any function/arrow (a setter matters
/// especially — it is invoked with one argument by `obj.x = v`, an assignment
/// no call-site arity scan covers); the `extends` expression and the field
/// initializers are scanned too.
fn scan_class_arity(class: &swc_ecma_ast::Class) -> usize {
    let heritage = class.super_class.as_deref().map_or(0, scan_expr_arity);
    class
        .body
        .iter()
        .map(|member| match member {
            ClassMember::Constructor(ctor) => {
                let own = ctor.params.len();
                let body = ctor.body.as_ref().map_or(0, |b| scan_stmts_arity(&b.stmts));
                own.max(body)
            }
            ClassMember::Method(m) => {
                let own = m.function.params.len();
                let body = m
                    .function
                    .body
                    .as_ref()
                    .map_or(0, |b| scan_stmts_arity(&b.stmts));
                own.max(body)
            }
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

/// The greatest function arity reachable through an expression, including any
/// arrow function it contains (the arrow's own param count and its body).
#[allow(clippy::too_many_lines)]
fn scan_expr_arity(expr: &Expr) -> usize {
    match expr {
        Expr::Arrow(a) => {
            let own = a.params.len();
            let body = match &*a.body {
                ArrowFunctionBody::FunctionBody(b) => scan_stmts_arity(&b.stmts),
                ArrowFunctionBody::Expr(e) => scan_expr_arity(e),
            };
            own.max(body)
        }
        Expr::Fn(f) => {
            let own = f.function.params.len();
            let body = f
                .function
                .body
                .as_ref()
                .map_or(0, |b| scan_stmts_arity(&b.stmts));
            own.max(body)
        }
        Expr::Class(c) => scan_class_arity(&c.class),
        Expr::Paren(p) => scan_expr_arity(&p.expr),
        Expr::Bin(b) => scan_expr_arity(&b.left).max(scan_expr_arity(&b.right)),
        Expr::Unary(u) => scan_expr_arity(&u.arg),
        Expr::Update(u) => scan_expr_arity(&u.arg),
        Expr::Cond(c) => scan_expr_arity(&c.test)
            .max(scan_expr_arity(&c.cons))
            .max(scan_expr_arity(&c.alt)),
        Expr::Assign(a) => scan_expr_arity(&a.right),
        Expr::Call(c) => {
            let mut m = match &c.callee {
                Callee::Expr(e) => scan_expr_arity(e),
                _ => 0,
            };
            for arg in &c.args {
                m = m.max(scan_expr_arity(&arg.expr));
            }
            // The uniform ABI pads every call to `K` positional slots, so a call
            // passing more arguments than any function's declared arity must still
            // widen `K` — otherwise a trailing rest parameter would receive
            // only `K` of them, the surplus having been dropped at the call site.
            m = m.max(c.args.len());
            // An array higher-order method invokes its callback with extra
            // positional args beyond the callback's own arity — `(element, index)`
            // for map/filter/forEach and `(acc, element, index)` for reduce. Widen
            // the uniform ABI so those pushes never overflow the shared funcref
            // signature (a `call_indirect` type mismatch would trap). Detected
            // syntactically; over-counting under a shadowed name is harmless.
            if let Callee::Expr(e) = &c.callee
                && let Expr::Member(mem) = e.as_ref()
                && let MemberProp::Ident(id) = &mem.prop
            {
                m = m.max(match id.sym.as_ref() {
                    "reduce" => 3,
                    "map" | "filter" | "forEach" => 2,
                    // A `JSON.stringify` replacer / `JSON.parse` reviver is
                    // called with `(key, value)`.
                    "stringify" | "parse" if c.args.len() > 1 => 2,
                    _ => 0,
                });
            }
            m
        }
        Expr::Member(m) => {
            let mut r = scan_expr_arity(&m.obj);
            if let MemberProp::Computed(c) = &m.prop {
                r = r.max(scan_expr_arity(&c.expr));
            }
            r
        }
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
                    Prop::KeyValue(kv) => Some(scan_expr_arity(&kv.value)),
                    // A method lifts to a function; its own param count and
                    // body must widen `K` like any arrow/function.
                    Prop::Method(method) => {
                        let own = method.function.params.len();
                        let body = method
                            .function
                            .body
                            .as_ref()
                            .map_or(0, |b| scan_stmts_arity(&b.stmts));
                        Some(own.max(body))
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
