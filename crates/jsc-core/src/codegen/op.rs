#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::Span;

use crate::error::CompileError;

/// Return the property name of a `Math.<name>` member expression (`Math` a bare
/// identifier, `<name>` a plain — non-computed, non-private — property), or `None`
/// for any other shape. Used to recognize both the `Math.PI`/`Math.E` constant
/// reads and the `Math.<method>(..)` call callees syntactically. Whether
/// `Math` is actually the built-in (vs. a shadowing binding) is checked separately
/// by the caller via [`resolve`](Codegen::resolve).
pub(super) fn math_member_name(member: &swc_ecma_ast::MemberExpr) -> Option<&str> {
    let Expr::Ident(obj) = member.obj.as_ref() else {
        return None;
    };
    if obj.sym.as_ref() != "Math" {
        return None;
    }
    match &member.prop {
        MemberProp::Ident(prop) => Some(prop.sym.as_ref()),
        _ => None,
    }
}

/// The [`MathOp`] of the inline `Math.<name>` method, or `None`.
pub(super) fn math_op(name: &str) -> Option<MathOp> {
    Some(match name {
        "floor" => MathOp::Floor,
        "ceil" => MathOp::Ceil,
        "round" => MathOp::Round,
        "trunc" => MathOp::Trunc,
        "abs" => MathOp::Abs,
        "sqrt" => MathOp::Sqrt,
        "min" => MathOp::Min,
        "max" => MathOp::Max,
        "pow" => MathOp::Pow,
        _ => return None,
    })
}

/// Whether `namespace.key(..)` has inline code (DESIGN.md §6.4); any other
/// member is an ordinary property of the namespace object.
pub(super) fn inline_member(namespace: &str, key: &str) -> bool {
    match namespace {
        "Math" => math_op(key).is_some(),
        "JSON" => matches!(key, "stringify" | "parse"),
        "console" => key == "log",
        "Object" => object_op(key).is_some(),
        "Promise" => matches!(key, "resolve" | "reject"),
        "Date" => key == "now",
        _ => false,
    }
}

/// The number of arguments the inline code of `namespace.key` (an
/// [`inline_member`]) reads, `None` when it is variadic. The call site pads
/// missing arguments with `undefined` and evaluates (then ignores) extra ones.
pub(super) fn inline_arity(namespace: &str, key: &str) -> Option<usize> {
    match (namespace, key) {
        ("Math", "min" | "max") | ("console", _) => None,
        ("Math", "pow") => Some(2),
        ("Date", _) => Some(0),
        _ => Some(1),
    }
}

/// The inline code of `namespace.key(args)` (an [`inline_member`]) over
/// exactly its [`inline_arity`] arguments, or `None` for a member without
/// inline code.
pub(super) fn inline_call(namespace: &str, key: &str, args: Vec<IrNode>) -> Option<IrNode> {
    let mut rest = args.clone().into_iter();
    let first = Box::new(rest.next().unwrap_or(IrNode::Undefined));
    Some(match namespace {
        "Math" => IrNode::MathCall {
            op: math_op(key)?,
            args,
        },
        "JSON" if key == "stringify" => IrNode::JsonStringify { arg: first },
        "JSON" => IrNode::JsonParse { arg: first },
        "console" => IrNode::ConsoleLog { args },
        "Object" => IrNode::ObjectBuiltin {
            op: object_op(key)?,
            arg: first,
        },
        "Promise" if key == "resolve" => IrNode::PromiseResolve(first),
        "Promise" => IrNode::PromiseReject(first),
        "Date" => IrNode::DateNow,
        _ => return None,
    })
}

/// The `%Array.prototype%` methods with a guarded call-site fast path (the
/// array-side names of the string/array method families).
pub(super) const ARRAY_METHODS: [&str; 10] = [
    "push", "pop", "map", "filter", "reduce", "forEach", "indexOf", "includes", "join", "slice",
];

/// The `%String.prototype%` methods with a guarded call-site fast path (the
/// names not already in [`ARRAY_METHODS`]).
pub(super) const STRING_METHODS: [&str; 13] = [
    "toUpperCase",
    "toLowerCase",
    "charAt",
    "trim",
    "trimStart",
    "trimEnd",
    "repeat",
    "startsWith",
    "endsWith",
    "padStart",
    "padEnd",
    "substring",
    "split",
];

/// Map a bare global function name to its conversion built-in, or `None`
/// when the name is not a recognized conversion. Used at the call site only after
/// the name is confirmed to be neither a binding nor a user function, so these
/// stay shadowable.
pub(super) fn convert_builtin_op(name: &str) -> Option<ConvertOp> {
    Some(match name {
        "String" => ConvertOp::String,
        "Number" => ConvertOp::ToNumber,
        "Boolean" => ConvertOp::ToBoolean,
        "parseInt" => ConvertOp::ParseInt,
        "parseFloat" => ConvertOp::ParseFloat,
        _ => return None,
    })
}

/// The number of arguments the conversion `op` reads: `parseInt` takes a
/// radix, everything else one value.
pub(super) fn convert_arity(op: ConvertOp) -> usize {
    if op == ConvertOp::ParseInt { 2 } else { 1 }
}

/// The conversion `op` over exactly its [`convert_arity`] arguments; `empty`
/// when the call passed none, so `String()` is `""` and `Number()` is `0`
/// (not the conversion of `undefined`).
pub(super) fn convert_call(op: ConvertOp, empty: bool, args: Vec<IrNode>) -> IrNode {
    match op {
        ConvertOp::String if empty => return IrNode::Str("".into()),
        ConvertOp::ToNumber if empty => return IrNode::Num(0.0),
        _ => {}
    }
    let mut args = args.into_iter();
    IrNode::ConvertCall {
        op,
        arg: Box::new(args.next().unwrap_or(IrNode::Undefined)),
        radix: (op == ConvertOp::ParseInt)
            .then(|| Box::new(args.next().unwrap_or(IrNode::Undefined))),
    }
}

/// The [`ObjectBuiltinOp`] of the inline `Object.<name>` static, or `None`.
fn object_op(name: &str) -> Option<ObjectBuiltinOp> {
    Some(match name {
        "keys" => ObjectBuiltinOp::Keys,
        "values" => ObjectBuiltinOp::Values,
        "entries" => ObjectBuiltinOp::Entries,
        _ => return None,
    })
}

/// The `%Map.prototype%`, `%Set.prototype%`, `%Promise.prototype%` and
/// `%GeneratorPrototype%` methods with a guarded call-site fast path (the
/// `%Date.prototype%` ones are [`DateOp`]s).
const RECEIVER_METHODS: [&str; 10] = [
    "get", "set", "add", "has", "delete", "clear", "then", "catch", "finally", "next",
];

/// Whether `recv.name(..)` has a guarded fast path for a `Map`/`Set`/
/// `Promise`/`Date`/generator receiver ([`receiver_fast_paths`]).
pub(super) fn receiver_method(name: &str) -> bool {
    RECEIVER_METHODS.contains(&name) || date_fast_path(name).is_some()
}

/// The `Date` field method `name` with a call-site fast path (`toString` is
/// called through its builtin only).
fn date_fast_path(name: &str) -> Option<DateOp> {
    DateOp::from_name(name).filter(|op| *op != DateOp::ToString)
}

/// The inline fast paths of `recv.name(args)` on a `Map`/`Set`/`Promise`/
/// `Date`/generator receiver in local `recv` (DESIGN.md §6.4), each the
/// body of the builtin `name` of that family over the side-effect-free
/// `args`.
pub(super) fn receiver_fast_paths(
    name: &str,
    recv: u32,
    args: &[IrNode],
) -> Vec<(ProtoKind, IrNode)> {
    let arg = |i: usize| Box::new(args.get(i).cloned().unwrap_or(IrNode::Undefined));
    let collection = |kind, op| {
        let args = args.iter().take(2).cloned().collect();
        vec![(
            kind,
            IrNode::CollectionOp {
                op,
                handle: recv,
                args,
            },
        )]
    };
    let receiver = || Box::new(IrNode::Local(recv));
    match name {
        "get" => collection(ProtoKind::Map, MapOp::Get),
        "set" => collection(ProtoKind::Map, MapOp::Set),
        "add" => collection(ProtoKind::Set, MapOp::Add),
        "has" => collection(ProtoKind::Collection, MapOp::Has),
        "delete" => collection(ProtoKind::Collection, MapOp::Delete),
        "clear" => collection(ProtoKind::Collection, MapOp::Clear),
        "then" | "catch" => {
            let (on_fulfilled, on_rejected) = if name == "then" {
                (arg(0), arg(1))
            } else {
                (Box::new(IrNode::Undefined), arg(0))
            };
            let then = IrNode::PromiseThen {
                promise: receiver(),
                on_fulfilled,
                on_rejected,
            };
            vec![(ProtoKind::Promise, then)]
        }
        "finally" => {
            let finally = IrNode::PromiseFinally {
                promise: receiver(),
                on_finally: arg(0),
            };
            vec![(ProtoKind::Promise, finally)]
        }
        "next" => {
            let next = IrNode::GenResume {
                handle: receiver(),
                sent: arg(0),
            };
            vec![(ProtoKind::Generator, next)]
        }
        _ => date_fast_path(name)
            .map(|op| vec![(ProtoKind::Date, IrNode::DateField { op, recv })])
            .unwrap_or_default(),
    }
}

/// Map a supported swc [`BinaryOp`] to the crate's [`BinOp`], erroring on any
/// operator outside the numeric/comparison subset.
pub(super) fn map_binary_op(op: BinaryOp, span: Span) -> anyhow::Result<BinOp> {
    Ok(match op {
        BinaryOp::Add => BinOp::Add,
        BinaryOp::Sub => BinOp::Sub,
        BinaryOp::Mul => BinOp::Mul,
        BinaryOp::Div => BinOp::Div,
        BinaryOp::Mod => BinOp::Mod,
        BinaryOp::Lt => BinOp::Lt,
        BinaryOp::Gt => BinOp::Gt,
        BinaryOp::LtEq => BinOp::Le,
        BinaryOp::GtEq => BinOp::Ge,
        BinaryOp::EqEqEq => BinOp::Eq,
        BinaryOp::NotEqEq => BinOp::Ne,
        BinaryOp::EqEq => BinOp::LooseEq,
        BinaryOp::NotEq => BinOp::LooseNe,
        BinaryOp::BitAnd => BinOp::BitAnd,
        BinaryOp::BitOr => BinOp::BitOr,
        BinaryOp::BitXor => BinOp::BitXor,
        BinaryOp::LShift => BinOp::Shl,
        BinaryOp::RShift => BinOp::Shr,
        BinaryOp::ZeroFillRShift => BinOp::UShr,
        _ => return Err(CompileError::spanned(span, "unsupported operator").into()),
    })
}

/// Map a swc compound-assignment [`AssignOp`] to the arithmetic, bitwise, or
/// shift [`BinOp`] its desugaring applies, returning `None` for the plain `=`
/// assignment.
///
/// # Errors
///
/// Returns an error for any other compound operator (`**=`, and the logical
/// compounds, which [`map_logical_assign_op`] handles).
pub(super) fn map_assign_op(op: AssignOp, span: Span) -> anyhow::Result<Option<BinOp>> {
    Ok(match op {
        AssignOp::Assign => None,
        AssignOp::AddAssign => Some(BinOp::Add),
        AssignOp::SubAssign => Some(BinOp::Sub),
        AssignOp::MulAssign => Some(BinOp::Mul),
        AssignOp::DivAssign => Some(BinOp::Div),
        AssignOp::ModAssign => Some(BinOp::Mod),
        AssignOp::BitAndAssign => Some(BinOp::BitAnd),
        AssignOp::BitOrAssign => Some(BinOp::BitOr),
        AssignOp::BitXorAssign => Some(BinOp::BitXor),
        AssignOp::LShiftAssign => Some(BinOp::Shl),
        AssignOp::RShiftAssign => Some(BinOp::Shr),
        AssignOp::ZeroFillRShiftAssign => Some(BinOp::UShr),
        _ => return Err(CompileError::spanned(span, "unsupported assignment operator").into()),
    })
}

/// Map a swc logical compound-assignment [`AssignOp`] (`??=`/`||=`/`&&=`) to the
/// crate's [`LogicalOp`], returning `None` for any non-logical assignment (plain
/// `=` and the arithmetic compounds, handled by [`map_assign_op`]).
pub(super) fn map_logical_assign_op(op: AssignOp) -> Option<LogicalOp> {
    match op {
        AssignOp::NullishAssign => Some(LogicalOp::Nullish),
        AssignOp::OrAssign => Some(LogicalOp::Or),
        AssignOp::AndAssign => Some(LogicalOp::And),
        _ => None,
    }
}

/// Map a swc short-circuit logical [`BinaryOp`] to the crate's [`LogicalOp`],
/// returning `None` for any non-logical operator (which is handled as an eager
/// [`BinOp`] instead).
pub(super) fn map_logical_op(op: BinaryOp) -> Option<LogicalOp> {
    match op {
        BinaryOp::LogicalAnd => Some(LogicalOp::And),
        BinaryOp::LogicalOr => Some(LogicalOp::Or),
        BinaryOp::NullishCoalescing => Some(LogicalOp::Nullish),
        _ => None,
    }
}
