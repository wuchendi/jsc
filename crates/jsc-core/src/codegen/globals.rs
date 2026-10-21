//! The program-level global facts of design 3.2, computed once by a
//! whole-program syntactic scan before lowering: whether a global name is
//! `sealed` (its value is the intrinsic it starts as, so a read compiles to
//! the constant address) and whether a namespace additionally never escapes
//! (so `N.m(..)` / `N.k` keep today's inline code).

use std::collections::HashSet;

use swc_ecma_ast::{
    AssignExpr, AssignTarget, BinExpr, BinaryOp, CallExpr, Callee, Class, Expr, ForHead, Function,
    GetterProp, MemberExpr, MemberProp, NewExpr, Program, SetterProp, SimpleAssignTarget,
    UnaryExpr, UnaryOp, UpdateExpr,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::ir::{BuiltinCtor, ErrorKind};

/// The global objects whose members have inline fast paths.
const NAMESPACES: [&str; 6] = ["Math", "JSON", "console", "Object", "Promise", "Date"];

/// Whether `callee(..)` keeps its syntactic lowering while `callee` names the
/// unshadowed builtin (the conversions and the Error family).
fn syntactic_call(callee: &str) -> bool {
    matches!(callee, "String" | "Number" | "Boolean") || ErrorKind::from_name(callee).is_some()
}

/// Whether `new callee(..)` keeps its syntactic lowering while `callee` names
/// the unshadowed builtin.
fn syntactic_new(callee: &str) -> bool {
    matches!(callee, "Map" | "Set" | "Promise" | "Date") || ErrorKind::from_name(callee).is_some()
}

/// What the whole-program scan found (see the module docs).
#[derive(Default)]
pub(super) struct GlobalFacts {
    /// The program contains `globalThis`, a top-level `this` or a `delete`
    /// of an identifier: the global object can change under any name.
    open: bool,
    /// Names written as identifiers anywhere (assignment, update,
    /// destructuring or loop-head target).
    assigned: HashSet<String>,
    /// Names read other than as the object of a static `N.k` read or a
    /// `N.m(..)` call (a bare use, a computed `N[k]`, a member write).
    escaped: HashSet<String>,
    /// Names read in any form.
    referenced: HashSet<String>,
    /// Some namespace member is read as a value rather than called.
    member_value: bool,
    /// The widest argument list of any call or `new`.
    max_call_args: usize,
}

impl GlobalFacts {
    /// Scan `program`.
    pub(super) fn scan(program: &Program) -> Self {
        let mut scan = Scan {
            facts: Self::default(),
            fn_depth: 0,
            target: false,
        };
        program.visit_with(&mut scan);
        scan.facts
    }

    /// `sealed(name)` for an unshadowed global `name`.
    pub(super) fn sealed(&self, name: &str) -> bool {
        !self.open && !self.assigned.contains(name)
    }

    /// Whether `name.m(..)` / `name.k` keep the inline namespace code.
    pub(super) fn inline(&self, name: &str) -> bool {
        self.sealed(name) && !self.escaped.contains(name)
    }

    /// Whether a builtin namespace method or constructor can be called
    /// through its value. The builtins see their arguments in the `K`
    /// positional slots, so `K` must then cover the widest call
    /// ([`Self::call_arity`]).
    pub(super) fn reaches_builtins(&self) -> bool {
        self.open
            || self.member_value
            || NAMESPACES
                .iter()
                .any(|n| self.referenced.contains(*n) && !self.inline(n))
            || BuiltinCtor::ALL
                .iter()
                .any(|c| self.escaped.contains(c.name()))
    }

    /// The widest argument list of any call or `new`.
    pub(super) fn call_arity(&self) -> usize {
        self.max_call_args
    }
}

/// The scanning visitor.
struct Scan {
    facts: GlobalFacts,
    /// Nesting depth of `this`-binding functions (arrows excluded).
    fn_depth: u32,
    /// Inside a destructuring or loop-head assignment target.
    target: bool,
}

impl Scan {
    /// Record a read of `name`; `escapes` when not a static member read or
    /// call.
    fn read(&mut self, name: &str, escapes: bool) {
        self.facts.referenced.insert(name.to_owned());
        if escapes {
            self.facts.escaped.insert(name.to_owned());
        }
    }

    /// The identifier object of a static `N.k` member.
    fn static_member_obj(member: &MemberExpr) -> Option<&str> {
        match (member.obj.as_ref(), &member.prop) {
            (Expr::Ident(obj), MemberProp::Ident(_)) => Some(obj.sym.as_ref()),
            _ => None,
        }
    }

    /// A write to the member `member`: its identifier object escapes.
    fn member_write(&mut self, member: &MemberExpr) {
        if let Expr::Ident(obj) = member.obj.as_ref() {
            self.read(obj.sym.as_ref(), true);
        } else {
            member.visit_with(self);
        }
    }

    fn nested<T: VisitWith<Self> + ?Sized>(&mut self, node: &T) {
        self.fn_depth += 1;
        node.visit_children_with(self);
        self.fn_depth -= 1;
    }
}

impl Visit for Scan {
    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(id) => {
                let name = id.sym.as_ref();
                if name == "globalThis" {
                    self.facts.open = true;
                }
                self.read(name, true);
            }
            Expr::This(_) if self.fn_depth == 0 => self.facts.open = true,
            Expr::Member(member) => match Self::static_member_obj(member) {
                Some(name) if self.target => self.read(name, true),
                Some(name) => {
                    if NAMESPACES.contains(&name) {
                        self.facts.member_value = true;
                    }
                    self.read(name, false);
                }
                None => expr.visit_children_with(self),
            },
            _ => expr.visit_children_with(self),
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        self.facts.max_call_args = self.facts.max_call_args.max(call.args.len());
        let Callee::Expr(callee) = &call.callee else {
            call.visit_children_with(self);
            return;
        };
        let name = match callee.as_ref() {
            Expr::Member(member) => Self::static_member_obj(member),
            Expr::Ident(id) if syntactic_call(id.sym.as_ref()) => Some(id.sym.as_ref()),
            _ => None,
        };
        if let Some(name) = name {
            self.read(name, false);
            call.args.visit_with(self);
        } else {
            call.visit_children_with(self);
        }
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        let args = new.args.as_ref().map_or(0, Vec::len);
        self.facts.max_call_args = self.facts.max_call_args.max(args);
        if let Expr::Ident(id) = new.callee.as_ref()
            && syntactic_new(id.sym.as_ref())
        {
            self.read(id.sym.as_ref(), false);
            new.args.visit_with(self);
        } else {
            new.visit_children_with(self);
        }
    }

    fn visit_bin_expr(&mut self, bin: &BinExpr) {
        // A builtin right-hand side of `instanceof` keeps its prototype walk.
        if bin.op == BinaryOp::InstanceOf
            && let Expr::Ident(id) = bin.right.as_ref()
        {
            self.read(id.sym.as_ref(), false);
            bin.left.visit_with(self);
        } else {
            bin.visit_children_with(self);
        }
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(id)) => {
                self.facts.assigned.insert(id.id.sym.as_ref().to_owned());
            }
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => self.member_write(member),
            AssignTarget::Simple(other) => other.visit_with(self),
            AssignTarget::Pat(pat) => {
                let outer = std::mem::replace(&mut self.target, true);
                pat.visit_with(self);
                self.target = outer;
            }
        }
        assign.right.visit_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        match update.arg.as_ref() {
            Expr::Ident(id) => {
                self.facts.assigned.insert(id.sym.as_ref().to_owned());
            }
            Expr::Member(member) => self.member_write(member),
            other => other.visit_with(self),
        }
    }

    fn visit_unary_expr(&mut self, unary: &UnaryExpr) {
        match (unary.op, unary.arg.as_ref()) {
            (UnaryOp::Delete, Expr::Ident(_)) => self.facts.open = true,
            (UnaryOp::Delete, Expr::Member(member)) => self.member_write(member),
            _ => unary.visit_children_with(self),
        }
    }

    fn visit_for_head(&mut self, head: &ForHead) {
        let outer = std::mem::replace(&mut self.target, matches!(head, ForHead::Pat(_)));
        head.visit_children_with(self);
        self.target = outer;
    }

    fn visit_binding_ident(&mut self, id: &swc_ecma_ast::BindingIdent) {
        if self.target {
            self.facts.assigned.insert(id.id.sym.as_ref().to_owned());
        }
    }

    fn visit_function(&mut self, function: &Function) {
        self.nested(function);
    }

    fn visit_getter_prop(&mut self, getter: &GetterProp) {
        self.nested(getter);
    }

    fn visit_setter_prop(&mut self, setter: &SetterProp) {
        self.nested(setter);
    }

    fn visit_class(&mut self, class: &Class) {
        class.super_class.visit_with(self);
        self.fn_depth += 1;
        class.body.visit_with(self);
        self.fn_depth -= 1;
    }
}
