//! `Math`, `JSON` and `console` methods and the constructor statics
//! (`Object.keys/values/entries`, `Array.isArray`, `Promise.resolve/reject`,
//! `Date.now`): each wraps the IR node its inline fast path lowers to (design
//! 3.2), applied to the argument slots, so a call through the function value
//! computes exactly what the inline code does. The constructors' bodies are
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
            IrNode::Undefined
        }
    };
    Some(match builtin {
        Builtin::Math(op @ (MathOp::Min | MathOp::Max)) => {
            by_argc(k, |args| IrNode::MathCall { op, args })
        }
        Builtin::Math(op @ MathOp::Pow) => IrNode::MathCall {
            op,
            args: vec![arg(0), arg(1)],
        },
        Builtin::Math(op) => IrNode::MathCall {
            op,
            args: vec![arg(0)],
        },
        Builtin::JsonStringify => IrNode::JsonStringify {
            arg: Box::new(arg(0)),
        },
        Builtin::JsonParse => IrNode::JsonParse {
            arg: Box::new(arg(0)),
        },
        Builtin::ConsoleLog => by_argc(k, |args| IrNode::ConsoleLog { args }),
        Builtin::ObjectStatic(op) => IrNode::ObjectBuiltin {
            op,
            arg: Box::new(arg(0)),
        },
        Builtin::IsArray => IrNode::InstanceOf {
            value: Box::new(arg(0)),
            kind: InstanceOfKind::Array,
        },
        Builtin::PromiseResolve => IrNode::PromiseResolve(Box::new(arg(0))),
        Builtin::PromiseReject => IrNode::PromiseReject(Box::new(arg(0))),
        Builtin::DateNow => IrNode::DateNow,
        Builtin::Ctor(ctor) => super::ctor::fragment(ctor, k),
        _ => return None,
    })
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
