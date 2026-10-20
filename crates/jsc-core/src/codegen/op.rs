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

/// Lower a recognized `Math.<name>(args)` call to an [`IrNode::MathCall`],
/// validating the method name and its argument count. The unary ops take
/// exactly one argument, `pow` exactly two, and `min`/`max` any number; an unknown
/// `Math.<name>` or a wrong arity is a compile error, matching the existing
/// direct-call error style.
pub(super) fn lower_math_call(name: &str, args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    let op = match name {
        "floor" => MathOp::Floor,
        "ceil" => MathOp::Ceil,
        "round" => MathOp::Round,
        "trunc" => MathOp::Trunc,
        "abs" => MathOp::Abs,
        "sqrt" => MathOp::Sqrt,
        "min" => MathOp::Min,
        "max" => MathOp::Max,
        "pow" => MathOp::Pow,
        _ => {
            return Err(
                CompileError::spanned(span, format!("unknown Math method: Math.{name}")).into(),
            );
        }
    };
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

/// Map a method name to its non-callback array/string method op, or `None`
/// when the name is not one of them. Recognized purely syntactically from a
/// `<expr>.<name>(..)` member callee (kept distinct from the push/pop and higher-order array
/// methods and from `x[i]` indexing); the receiver's array-vs-string kind is
/// resolved at runtime host-side.
pub(super) fn non_callback_method_op(name: &str) -> Option<MethodOp> {
    Some(match name {
        "indexOf" => MethodOp::IndexOf,
        "includes" => MethodOp::Includes,
        "join" => MethodOp::Join,
        "slice" => MethodOp::Slice,
        "toUpperCase" => MethodOp::ToUpper,
        "toLowerCase" => MethodOp::ToLower,
        "charAt" => MethodOp::CharAt,
        "trim" => MethodOp::Trim,
        "trimStart" => MethodOp::TrimStart,
        "trimEnd" => MethodOp::TrimEnd,
        "repeat" => MethodOp::Repeat,
        "startsWith" => MethodOp::StartsWith,
        "endsWith" => MethodOp::EndsWith,
        "padStart" => MethodOp::PadStart,
        "padEnd" => MethodOp::PadEnd,
        "substring" => MethodOp::Substring,
        "split" => MethodOp::Split,
        _ => return None,
    })
}

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

/// Lower a recognized `Promise.<name>(v)` static call: `Promise.resolve`
/// / `Promise.reject` each take an optional single value (a missing argument is
/// `undefined`). An unknown static (`Promise.all`/`race`/…) or a call passing
/// more than one argument is a compile error, matching the built-in-call error
/// style.
pub(super) fn lower_promise_static(
    name: &str,
    mut args: Vec<IrNode>,
    span: Span,
) -> anyhow::Result<IrNode> {
    if !matches!(name, "resolve" | "reject") {
        return Err(CompileError::spanned(
            span,
            format!("unsupported Promise method: Promise.{name}"),
        )
        .into());
    }
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

/// Lower a recognized `Object.<name>(arg)` call to an [`IrNode::ObjectBuiltin`],
/// validating the method name and its single-argument arity. An unknown
/// `Object.<name>` or a wrong arity is a compile error, matching the existing
/// built-in-call error style. Out-of-scope statics (`Object.assign`/`freeze`)
/// fall here and are rejected by name.
pub(super) fn lower_object_call(
    name: &str,
    args: Vec<IrNode>,
    span: Span,
) -> anyhow::Result<IrNode> {
    let op = match name {
        "keys" => ObjectBuiltinOp::Keys,
        "values" => ObjectBuiltinOp::Values,
        "entries" => ObjectBuiltinOp::Entries,
        _ => {
            return Err(CompileError::spanned(
                span,
                format!("unsupported Object method: Object.{name}"),
            )
            .into());
        }
    };
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

/// Lower a recognized `JSON.<name>(arg)` call. `JSON.stringify(v)` lowers to
/// an [`IrNode::JsonStringify`] (exactly one argument; the `replacer`/`space`
/// second/third arguments are unsupported and rejected); `JSON.parse(text)`
/// lowers to an [`IrNode::JsonParse`]. Any other `JSON.<name>` is rejected by
/// name.
pub(super) fn lower_json_call(name: &str, args: Vec<IrNode>, span: Span) -> anyhow::Result<IrNode> {
    match name {
        "stringify" => {
            if args.len() != 1 {
                return Err(CompileError::spanned(
                    span,
                    format!(
                        "JSON.stringify expects 1 argument(s), got {} (the replacer/space \
                         arguments are unsupported)",
                        args.len()
                    ),
                )
                .into());
            }
            let arg = args
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("JSON.stringify requires an argument"))?;
            Ok(IrNode::JsonStringify { arg: Box::new(arg) })
        }
        "parse" => {
            if args.len() != 1 {
                return Err(CompileError::spanned(
                    span,
                    format!(
                        "JSON.parse expects 1 argument(s), got {} (the reviver argument is \
                         unsupported)",
                        args.len()
                    ),
                )
                .into());
            }
            let arg = args
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("JSON.parse requires an argument"))?;
            Ok(IrNode::JsonParse { arg: Box::new(arg) })
        }
        _ => {
            Err(CompileError::spanned(span, format!("unsupported JSON method: JSON.{name}")).into())
        }
    }
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
