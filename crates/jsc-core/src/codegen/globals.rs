//! The program-level global facts of DESIGN.md §6.4, computed once by a
//! whole-program syntactic scan before lowering: whether a global name is
//! `sealed` (its value is the intrinsic it starts as, so a read compiles to
//! the constant address), whether a namespace additionally never escapes
//! (so `N.m(..)` / `N.k` keep today's inline code), and whether the program is
//! intrinsics-sealed (no builtin prototype can be mutated, so the dirty-bit
//! guards of the syntactic receiver methods fold away).

use std::collections::HashSet;

use swc_ecma_ast::{
    AssignExpr, AssignTarget, BinExpr, BinaryOp, CallExpr, Callee, Class, Constructor, Expr,
    ForHead, Function, GetterProp, Ident, IdentName, MemberExpr, MemberProp, NewExpr, Program,
    SetterProp, SimpleAssignTarget, Str, UnaryExpr, UnaryOp, UpdateExpr,
};
use swc_ecma_visit::{Visit, VisitWith};

use super::op::inline_member;
use crate::ir::{BuiltinCtor, ErrorKind};

/// The global objects whose members have inline fast paths.
pub(super) const NAMESPACES: [&str; 6] = ["Math", "JSON", "console", "Object", "Promise", "Date"];

/// The names whose presence lets a program reach and mutate a builtin
/// prototype (DESIGN.md §6.4 intrinsics-sealed).
const PROTOTYPE_REACHING: [&str; 9] = [
    "prototype",
    "constructor",
    "__proto__",
    "getPrototypeOf",
    "setPrototypeOf",
    "defineProperty",
    "defineProperties",
    "assign",
    "Reflect",
];

/// Whether `callee(..)` keeps its syntactic lowering while `callee` names the
/// unshadowed builtin (the conversions and the Error family).
fn syntactic_call(callee: &str) -> bool {
    matches!(callee, "String" | "Number" | "Boolean") || ErrorKind::from_name(callee).is_some()
}

/// Whether `new callee(..)` with `args` arguments keeps its syntactic
/// lowering while `callee` names the unshadowed builtin (`Map`/`Set` only
/// without an iterable).
fn syntactic_new(callee: &str, args: usize) -> bool {
    matches!(callee, "Promise" | "Date")
        || (matches!(callee, "Map" | "Set") && args == 0)
        || ErrorKind::from_name(callee).is_some()
}

/// What the whole-program scan found (see the module docs).
#[derive(Default)]
pub(super) struct GlobalFacts {
    /// The program contains an escaping `globalThis` (anything but a static
    /// `globalThis.k` read or call), a top-level `this` or a `delete` of an
    /// identifier: the global object can change under any name.
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
    /// Some [`PROTOTYPE_REACHING`] name occurs.
    reaches_prototypes: bool,
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

    /// Whether the program is intrinsics-sealed (DESIGN.md §6.4): none of
    /// [`PROTOTYPE_REACHING`] occurs as an identifier, a member or property
    /// name, or a string literal (a class constructor's own key excepted), so
    /// no builtin prototype can be reached and mutated.
    pub(super) fn intrinsics_sealed(&self) -> bool {
        !self.reaches_prototypes
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
    /// call. An escaping `globalThis` (a bare use, a member write) opens the
    /// global object.
    fn read(&mut self, name: &str, escapes: bool) {
        self.name(name);
        if escapes && name == "globalThis" {
            self.facts.open = true;
        }
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
            member.prop.visit_with(self);
        } else {
            member.visit_with(self);
        }
    }

    /// Record a name that may reach a builtin prototype.
    fn name(&mut self, name: &str) {
        if PROTOTYPE_REACHING.contains(&name) {
            self.facts.reaches_prototypes = true;
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
            Expr::Ident(id) => self.read(id.sym.as_ref(), true),
            Expr::This(_) if self.fn_depth == 0 => self.facts.open = true,
            Expr::Member(member) => match Self::static_member_obj(member) {
                Some(name) if self.target => self.read(name, true),
                Some(name) => {
                    if NAMESPACES.contains(&name) {
                        self.facts.member_value = true;
                    }
                    self.read(name, false);
                    member.prop.visit_with(self);
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
            if let Expr::Member(member) = callee.as_ref() {
                member.prop.visit_with(self);
                // A namespace member without inline code is called through
                // its value.
                if let MemberProp::Ident(key) = &member.prop
                    && NAMESPACES.contains(&name)
                    && !inline_member(name, key.sym.as_ref())
                {
                    self.facts.member_value = true;
                }
            }
            call.args.visit_with(self);
        } else {
            call.visit_children_with(self);
        }
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        let args = new.args.as_ref().map_or(0, Vec::len);
        self.facts.max_call_args = self.facts.max_call_args.max(args);
        if let Expr::Ident(id) = new.callee.as_ref()
            && syntactic_new(id.sym.as_ref(), args)
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
        self.name(id.id.sym.as_ref());
        if self.target {
            self.facts.assigned.insert(id.id.sym.as_ref().to_owned());
        }
    }

    fn visit_ident(&mut self, id: &Ident) {
        self.name(id.sym.as_ref());
    }

    fn visit_ident_name(&mut self, id: &IdentName) {
        self.name(id.sym.as_ref());
    }

    fn visit_str(&mut self, s: &Str) {
        if let Some(s) = s.value.as_str() {
            self.name(s);
        }
    }

    fn visit_constructor(&mut self, ctor: &Constructor) {
        // The `constructor` key itself reaches nothing.
        ctor.params.visit_with(self);
        ctor.body.visit_with(self);
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
