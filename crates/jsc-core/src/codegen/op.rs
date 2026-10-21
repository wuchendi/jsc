#[allow(clippy::wildcard_imports)]
use super::*;

use swc_common::Span;

use crate::error::CompileError;

/// Map a supported swc [`BinaryOp`] to the crate's [`BinOp`], erroring on any
/// operator outside the numeric/comparison subset.
/// Return `true` when `callee` is the member expression `console.log` (a
/// `console` identifier base with a static `log` property). The caller
/// additionally checks that `console` resolves to no user binding before
/// treating the call as a host print.
pub(super) fn is_console_log(callee: &Expr) -> bool {
    let Expr::Member(member) = callee else {
        return false;
    };
    let Expr::Ident(obj) = member.obj.as_ref() else {
        return false;
    };
    matches!(&member.prop, MemberProp::Ident(prop)
        if obj.sym.as_ref() == "console" && prop.sym.as_ref() == "log")
}

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

/// Return the method name of a `Math.<name>` call callee, or `None` when the
/// callee is not such a member expression.
pub(super) fn math_method_name(callee: &Expr) -> Option<&str> {
    match callee {
        Expr::Member(member) => math_member_name(member),
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

/// Whether `namespace.key(..)` has inline code (design 3.2 row 2); any other
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

/// Lower the inline code of `namespace.key(args)` (an [`inline_member`]),
/// validating its argument count: a wrong arity is a compile error.
pub(super) fn lower_namespace_call(
    namespace: &str,
    key: &str,
    args: Vec<IrNode>,
    span: Span,
) -> anyhow::Result<IrNode> {
    match namespace {
        "Math" => lower_math_call(key, args, span),
        "JSON" => lower_json_call(key, args, span),
        "console" => Ok(IrNode::ConsoleLog { args }),
        "Object" => lower_object_call(key, args, span),
        "Promise" => lower_promise_static(key, args, span),
        _ if args.is_empty() => Ok(IrNode::DateNow),
        _ => Err(CompileError::spanned(span, "Date.now() takes no arguments").into()),
    }
}

/// Lower an inline `Math.<name>(args)` call to an [`IrNode::MathCall`],
/// validating its argument count. The unary ops take exactly one argument,
/// `pow` exactly two, and `min`/`max` any number; a wrong arity is a compile
/// error, matching the existing direct-call error style.
fn lower_math_call(name: &str, args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    let op = math_op(name).ok_or_else(|| anyhow!("Math.{name} has no inline code"))?;
    let expected: Option<usize> = match op {
        MathOp::Min | MathOp::Max => None,
        MathOp::Pow => Some(2),
        _ => Some(1),
    };
    if let Some(expected) = expected
        && args.len() != expected
    {
        return Err(CompileError::spanned(
            span,
            format!(
                "Math.{name} expects {expected} argument(s), got {}",
                args.len()
            ),
        )
        .into());
    }
    Ok(IrNode::MathCall { op, args })
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
        "String" => ConvertOp::ToString,
        "Number" => ConvertOp::ToNumber,
        "Boolean" => ConvertOp::ToBoolean,
        "parseInt" => ConvertOp::ParseInt,
        "parseFloat" => ConvertOp::ParseFloat,
        _ => return None,
    })
}

/// Lower a recognized conversion built-in call to an [`IrNode::ConvertCall`],
/// validating its argument count. All five take one argument; `parseInt`
/// additionally accepts a radix second argument. A wrong arity is a compile
/// error matching the existing direct-call error style.
pub(super) fn lower_convert_call(
    op: ConvertOp,
    name: &str,
    args: Vec<IrNode>,
    span: Span,
) -> anyhow::Result<IrNode> {
    // `parseInt(s, radix)` takes an optional radix; everything else takes
    // exactly one argument.
    let ok = if matches!(op, ConvertOp::ParseInt) {
        args.len() == 1 || args.len() == 2
    } else {
        args.len() == 1
    };
    if !ok {
        let expected = if matches!(op, ConvertOp::ParseInt) {
            "1 or 2"
        } else {
            "1"
        };
        return Err(CompileError::spanned(
            span,
            format!("{name} expects {expected} argument(s), got {}", args.len()),
        )
        .into());
    }
    // Take the first argument and the optional `parseInt` radix. The arity
    // check above guarantees at least one; `ok_or_else` keeps the error total
    // without an `expect`.
    let mut args = args.into_iter();
    let arg = args
        .next()
        .ok_or_else(|| anyhow!("{name} requires an argument"))?;
    Ok(IrNode::ConvertCall {
        op,
        arg: Box::new(arg),
        radix: args.next().map(Box::new),
    })
}

/// Return the method name of an `Object.<name>` call callee (`Object` a bare
/// identifier, `<name>` a plain property), or `None` for any other callee shape.
/// Like `Math`, whether `Object` is the built-in vs. a shadowing binding
/// is checked separately by the caller via [`resolve`](Codegen::resolve).
pub(super) fn object_method_name(callee: &Expr) -> Option<&str> {
    let Expr::Member(member) = callee else {
        return None;
    };
    let Expr::Ident(obj) = member.obj.as_ref() else {
        return None;
    };
    if obj.sym.as_ref() != "Object" {
        return None;
    }
    match &member.prop {
        MemberProp::Ident(prop) => Some(prop.sym.as_ref()),
        _ => None,
    }
}

/// Return the method name of a `Promise.<name>` call callee (`Promise` a bare
/// identifier, `<name>` a plain property), or `None` for any other callee shape.
/// Like `Object`/`Math`, whether `Promise` is the built-in vs. a
/// shadowing binding is checked separately by the caller via
/// [`resolve`](Codegen::resolve).
pub(super) fn promise_static_method_name(callee: &Expr) -> Option<&str> {
    let Expr::Member(member) = callee else {
        return None;
    };
    let Expr::Ident(obj) = member.obj.as_ref() else {
        return None;
    };
    if obj.sym.as_ref() != "Promise" {
        return None;
    }
    match &member.prop {
        MemberProp::Ident(prop) => Some(prop.sym.as_ref()),
        _ => None,
    }
}

/// Lower an inline `Promise.resolve(v)` / `Promise.reject(v)` static call:
/// each takes an optional single value (a missing argument is `undefined`); a
/// call passing more than one argument is a compile error, matching the
/// built-in-call error style.
fn lower_promise_static(name: &str, mut args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    if args.len() > 1 {
        return Err(CompileError::spanned(
            span,
            format!("Promise.{name} expects 0-1 argument(s), got {}", args.len()),
        )
        .into());
    }
    let value = Box::new(args.pop().unwrap_or(IrNode::Undefined));
    Ok(if name == "resolve" {
        IrNode::PromiseResolve(value)
    } else {
        IrNode::PromiseReject(value)
    })
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

/// Lower an inline `Object.keys/values/entries(arg)` call to an
/// [`IrNode::ObjectBuiltin`], validating its single-argument arity.
fn lower_object_call(name: &str, args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    let op = object_op(name).ok_or_else(|| anyhow!("Object.{name} has no inline code"))?;
    if args.len() != 1 {
        return Err(CompileError::spanned(
            span,
            format!("Object.{name} expects 1 argument(s), got {}", args.len()),
        )
        .into());
    }
    let arg = args
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Object.{name} requires an argument"))?;
    Ok(IrNode::ObjectBuiltin {
        op,
        arg: Box::new(arg),
    })
}

/// Return the method name of a `JSON.<name>` call callee (`JSON` a bare
/// identifier, `<name>` a plain property), or `None` for any other callee shape.
/// Like `Object`, whether `JSON` is the built-in vs. a shadowing binding
/// is checked separately by the caller via [`resolve`](Codegen::resolve).
pub(super) fn json_method_name(callee: &Expr) -> Option<&str> {
    let Expr::Member(member) = callee else {
        return None;
    };
    let Expr::Ident(obj) = member.obj.as_ref() else {
        return None;
    };
    if obj.sym.as_ref() != "JSON" {
        return None;
    }
    match &member.prop {
        MemberProp::Ident(prop) => Some(prop.sym.as_ref()),
        _ => None,
    }
}

/// Lower an inline `JSON.<name>(arg)` call. `JSON.stringify(v)` lowers to
/// an [`IrNode::JsonStringify`] (exactly one argument; the `replacer`/`space`
/// second/third arguments are unsupported and rejected); `JSON.parse(text)`
/// lowers to an [`IrNode::JsonParse`].
fn lower_json_call(name: &str, args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    let unsupported = if name == "stringify" {
        "the replacer/space arguments are"
    } else {
        "the reviver argument is"
    };
    let [arg] = <[IrNode; 1]>::try_from(args).map_err(|args| {
        CompileError::spanned(
            span,
            format!(
                "JSON.{name} expects 1 argument(s), got {} ({unsupported} unsupported)",
                args.len()
            ),
        )
    })?;
    let arg = Box::new(arg);
    Ok(if name == "stringify" {
        IrNode::JsonStringify { arg }
    } else {
        IrNode::JsonParse { arg }
    })
}

/// Map a method name to its [`MapOp`], or `None` when the name is not a
/// recognized `Map`/`Set` method. Recognized purely syntactically from a
/// `<expr>.<name>(..)` member callee; because these names can also be ordinary
/// object/class methods, the lowered node dispatches on the receiver's runtime TYPE
/// BYTE and falls back to a user method call when it is not a map/set.
pub(super) fn map_set_method_op(name: &str) -> Option<MapOp> {
    Some(match name {
        "set" => MapOp::Set,
        "get" => MapOp::Get,
        "has" => MapOp::Has,
        "add" => MapOp::Add,
        "delete" => MapOp::Delete,
        _ => return None,
    })
}

/// Map a method name to its [`DateOp`], or `None` when the name is not
/// a recognized `Date` field method. Recognized purely syntactically from a
/// `<expr>.<name>(..)` member callee; because these names can also be ordinary
/// object/class methods, the lowered node dispatches on the receiver's runtime
/// TYPE BYTE and falls back to a user method call when it is not a `Date`.
/// `getTime` is handled separately (its own [`IrNode::DateGetTime`]).
pub(super) fn date_method_op(name: &str) -> Option<DateOp> {
    Some(match name {
        "valueOf" => DateOp::ValueOf,
        "getFullYear" => DateOp::GetFullYear,
        "getMonth" => DateOp::GetMonth,
        "getDate" => DateOp::GetDate,
        "getDay" => DateOp::GetDay,
        "getHours" => DateOp::GetHours,
        "getMinutes" => DateOp::GetMinutes,
        "getSeconds" => DateOp::GetSeconds,
        "toISOString" => DateOp::ToIsoString,
        _ => return None,
    })
}

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
        _ => return Err(CompileError::spanned(span, "unsupported operator").into()),
    })
}

/// Map a swc compound-assignment [`AssignOp`] to the arithmetic [`BinOp`] its
/// desugaring applies, returning `None` for the plain `=` assignment.
///
/// # Errors
///
/// Returns an error for any compound operator outside the arithmetic subset
/// (bitwise, shift, and logical compound assignments are unsupported).
pub(super) fn map_assign_op(op: AssignOp, span: Span) -> anyhow::Result<Option<BinOp>> {
    Ok(match op {
        AssignOp::Assign => None,
        AssignOp::AddAssign => Some(BinOp::Add),
        AssignOp::SubAssign => Some(BinOp::Sub),
        AssignOp::MulAssign => Some(BinOp::Mul),
        AssignOp::DivAssign => Some(BinOp::Div),
        AssignOp::ModAssign => Some(BinOp::Mod),
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
