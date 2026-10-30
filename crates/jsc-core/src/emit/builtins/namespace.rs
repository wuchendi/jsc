//! `Math` and `console` methods and the constructor statics
//! (`Object.keys/values/entries`, `Array.isArray`, `Date.now`): each wraps
//! the IR node its inline fast path lowers to (design 3.2), applied to the
//! argument slots, so a call through the function value computes exactly
//! what the inline code does. The constructors' bodies are
//! IR fragments too ([`super::ctor`]).

#[allow(clippy::wildcard_imports)]
use super::*;

/// The IR body of a namespace method over the `K = k` argument slots, or
/// `None` for a builtin of another family.
pub(in crate::emit) fn fragment(builtin: Builtin, k: u32) -> Option<IrNode> {
    let arg = |i: u32| {
        if i < k {
            IrNode::Local(i)
        } else {
            past_k(k, i)
        }
    };
    Some(match builtin {
        Builtin::Math(op @ (MathOp::Min | MathOp::Max | MathOp::Hypot)) => variadic(
            k,
            IrNode::MathSpread {
                op,
                args: Box::new(IrNode::SpreadArgsGlobal),
            },
            |args| IrNode::MathCall { op, args },
        ),
        Builtin::Math(op @ (MathOp::Pow | MathOp::Atan2 | MathOp::Imul)) => IrNode::MathCall {
            op,
            args: vec![arg(0), arg(1)],
        },
        Builtin::Math(op @ MathOp::Random) => IrNode::MathCall {
            op,
            args: Vec::new(),
        },
        Builtin::Math(op) => IrNode::MathCall {
            op,
            args: vec![arg(0)],
        },
        Builtin::ConsoleLog => variadic(
            k,
            IrNode::ConsoleLogSpread {
                args: Box::new(IrNode::SpreadArgsGlobal),
            },
            |args| IrNode::ConsoleLog { args },
        ),
        Builtin::ObjectStatic(op) => IrNode::ObjectBuiltin {
            op,
            arg: Box::new(arg(0)),
        },
        Builtin::IsArray => IrNode::InstanceOf {
            value: Box::new(arg(0)),
            kind: InstanceOfKind::Array,
        },
        Builtin::DateNow => IrNode::DateNow,
        Builtin::Ctor(ctor) => super::ctor::fragment(ctor, k),
        _ => return None,
    })
}

/// Argument `i` past the `K` slots: the element of the spread argument
/// array a call with more than `K` arguments published, else `undefined`.
fn past_k(k: u32, i: u32) -> IrNode {
    IrNode::Ternary {
        cond: Box::new(argc_above(k, i)),
        then_branch: Box::new(IrNode::Index {
            obj: Box::new(IrNode::SpreadArgsGlobal),
            index: Box::new(IrNode::Num(f64::from(i))),
        }),
        else_branch: Box::new(IrNode::Undefined),
    }
}

/// Whether the decoded argument count exceeds `n`.
fn argc_above(k: u32, n: u32) -> IrNode {
    IrNode::Binary {
        op: BinOp::Gt,
        lhs: Box::new(IrNode::ArgCount(k + 1)),
        rhs: Box::new(IrNode::Num(f64::from(n))),
    }
}

/// A variadic body: `spread` (over the published spread argument array,
/// which holds every argument) when the call passed more than `K`
/// arguments, else [`by_argc`].
fn variadic(k: u32, spread: IrNode, body: impl Fn(Vec<IrNode>) -> IrNode) -> IrNode {
    IrNode::Ternary {
        cond: Box::new(argc_above(k, k)),
        then_branch: Box::new(spread),
        else_branch: Box::new(by_argc(k, body)),
    }
}

/// A variadic body: `body` over the first `min(argc, K)` argument slots,
/// selected by a test chain on the `argc` parameter (local `K + 1`).
fn by_argc(k: u32, body: impl Fn(Vec<IrNode>) -> IrNode) -> IrNode {
    let args = |n: u32| (0..n).map(IrNode::Local).collect();
    (1..=k).fold(body(args(0)), |fewer, n| IrNode::Ternary {
        cond: Box::new(IrNode::Binary {
            op: BinOp::Ge,
            lhs: Box::new(IrNode::Local(k + 1)),
            rhs: Box::new(IrNode::Num(f64::from(n))),
        }),
        then_branch: Box::new(body(args(n))),
        else_branch: Box::new(fewer),
    })
}
