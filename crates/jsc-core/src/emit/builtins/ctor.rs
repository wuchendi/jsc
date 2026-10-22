//! The constructor bodies (DESIGN.md §6.4, §7.6, §7.7) as IR fragments over the
//! argument slots. A constructed call carries the construct marker in `argc`
//! (decoded by [`IrNode::ArgCount`]); `SELF_ALLOC` constructors allocate their
//! own kind of object with `proto = Get(new.target, "prototype")` (falling back
//! to the realm prototype), so builtin parents work under `extends`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The fragment builder of one body: the `K` argument slots, `argc` and the
/// i64 temps of a [`Frame`].
struct Body {
    k: u32,
}

impl Body {
    /// Argument `i` (`undefined` past the `K` slots).
    fn arg(&self, i: u32) -> IrNode {
        if i < self.k {
            IrNode::Local(i)
        } else {
            IrNode::Undefined
        }
    }

    /// Temp local `n` (`0..4`).
    fn temp(&self, n: u32) -> u32 {
        self.k + 2 + n
    }

    /// Whether this call is a `new` / `super(..)` (the construct marker).
    fn constructing(&self) -> IrNode {
        IrNode::IsConstructCall(self.k + 1)
    }

    /// Whether the decoded argument count is `n`.
    fn argc_is(&self, n: u32) -> IrNode {
        IrNode::Binary {
            op: BinOp::Eq,
            lhs: Box::new(IrNode::ArgCount(self.k + 1)),
            rhs: Box::new(IrNode::Num(f64::from(n))),
        }
    }

    /// The new instance's prototype (`GetPrototypeFromConstructor`):
    /// `new.target.prototype` when constructing and that is an object, else
    /// `ctor.prototype` (non-writable, so always the realm's). Uses temp 0.
    fn proto(&self, ctor: BuiltinCtor) -> IrNode {
        let own = IrNode::Intrinsic(GlobalIntrinsic::Ctor(ctor));
        let prototype = |obj: IrNode| IrNode::Index {
            obj: Box::new(obj),
            index: Box::new(IrNode::Str("prototype".to_owned())),
        };
        let p = self.temp(0);
        IrNode::Seq(vec![
            IrNode::Assign {
                local: p,
                value: Box::new(prototype(ternary(
                    self.constructing(),
                    IrNode::NewTargetGlobal,
                    own.clone(),
                ))),
            },
            ternary(
                IrNode::IsObject(Box::new(IrNode::Local(p))),
                IrNode::Local(p),
                prototype(own),
            ),
        ])
    }

    /// `alloc` (yielding the fresh instance, into temp 1) with its `proto`
    /// replaced by [`Self::proto`] (computed first, into temp 2).
    fn with_proto(&self, ctor: BuiltinCtor, alloc: IrNode) -> IrNode {
        let (obj, proto) = (self.temp(1), self.temp(2));
        IrNode::Seq(vec![
            IrNode::Assign {
                local: proto,
                value: Box::new(self.proto(ctor)),
            },
            IrNode::Assign {
                local: obj,
                value: Box::new(alloc),
            },
            IrNode::SetProto {
                obj: Box::new(IrNode::Local(obj)),
                proto: Box::new(IrNode::Local(proto)),
            },
        ])
    }

    /// `constructed` under `new`, else the "requires 'new'" `TypeError`.
    fn requires_new(&self, ctor: BuiltinCtor, constructed: IrNode) -> IrNode {
        let message = format!("Constructor {} requires 'new'", ctor.name());
        ternary(self.constructing(), constructed, throw_type_error(&message))
    }
}

/// `cond ? then_branch : else_branch`.
fn ternary(cond: IrNode, then_branch: IrNode, else_branch: IrNode) -> IrNode {
    IrNode::Ternary {
        cond: Box::new(cond),
        then_branch: Box::new(then_branch),
        else_branch: Box::new(else_branch),
    }
}

/// Throw a fresh `TypeError` with `message`.
fn throw_type_error(message: &str) -> IrNode {
    IrNode::Throw(Box::new(IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Str(message.to_owned())),
        IrNode::Str(format!("TypeError: {message}")),
    )))
}

/// Whether `value` is `undefined` or `null`.
fn is_nullish(value: IrNode) -> IrNode {
    IrNode::Binary {
        op: BinOp::LooseEq,
        lhs: Box::new(value),
        rhs: Box::new(IrNode::Null),
    }
}

/// The body of `ctor` over the `K = k` argument slots.
pub(super) fn fragment(ctor: BuiltinCtor, k: u32) -> IrNode {
    let body = Body { k };
    match ctor {
        BuiltinCtor::Object => object(&body),
        BuiltinCtor::Function => throw_type_error("the Function constructor is not supported"),
        BuiltinCtor::Array => array(&body),
        BuiltinCtor::String => {
            conversion(&body, ctor, ConvertOp::ToString, IrNode::Str(String::new()))
        }
        BuiltinCtor::Number => conversion(&body, ctor, ConvertOp::ToNumber, IrNode::Num(0.0)),
        BuiltinCtor::Boolean => conversion(&body, ctor, ConvertOp::ToBoolean, IrNode::Bool(false)),
        BuiltinCtor::Error(kind) => {
            let proto = body.temp(1);
            IrNode::Seq(vec![
                IrNode::Assign {
                    local: proto,
                    value: Box::new(body.proto(ctor)),
                },
                IrNode::error_from_argument(
                    kind,
                    IrNode::Local(proto),
                    body.arg(0),
                    body.temp(2),
                    body.temp(3),
                ),
            ])
        }
        // The allocation only: `map_set::emit_ctor` adds the iterable's values.
        BuiltinCtor::Map | BuiltinCtor::Set => {
            let map = body.with_proto(
                ctor,
                IrNode::MapNew {
                    set: ctor == BuiltinCtor::Set,
                },
            );
            body.requires_new(ctor, map)
        }
        BuiltinCtor::Promise => {
            let promise = IrNode::PromiseCtor {
                executor: Box::new(body.arg(0)),
                p_local: body.temp(3),
            };
            body.requires_new(ctor, body.with_proto(ctor, promise))
        }
        BuiltinCtor::Date => date(&body),
    }
}

/// `Object(v)`: an object argument as is; `undefined`/`null` a fresh plain
/// object (the allocated `this` under `new`); a primitive its wrapper object;
/// a subclass `new.target` gets its allocated `this`.
fn object(body: &Body) -> IrNode {
    let subclass = IrNode::Logical {
        op: LogicalOp::And,
        lhs: Box::new(body.constructing()),
        rhs: Box::new(IrNode::Binary {
            op: BinOp::Ne,
            lhs: Box::new(IrNode::NewTargetGlobal),
            rhs: Box::new(IrNode::Intrinsic(GlobalIntrinsic::Ctor(
                BuiltinCtor::Object,
            ))),
        }),
    };
    let fresh = ternary(
        body.constructing(),
        IrNode::ThisGlobal,
        IrNode::Object {
            entries: Vec::new(),
            proto: None,
            class: abi::CLASS_ORDINARY,
            reserve: 0,
        },
    );
    ternary(
        subclass,
        IrNode::ThisGlobal,
        ternary(
            IrNode::IsObject(Box::new(body.arg(0))),
            body.arg(0),
            ternary(
                is_nullish(body.arg(0)),
                fresh,
                IrNode::ToObject(Box::new(body.arg(0))),
            ),
        ),
    )
}

/// `Array(..)` / `new Array(..)`: one number argument is the length (a
/// `length` write, so an invalid one throws the `RangeError` and a large one
/// is sparse); otherwise the arguments are the elements (more than `K` only
/// through a spread call, whose published argument array is copied).
fn array(body: &Body) -> IrNode {
    let arr = body.temp(3);
    let with_length = IrNode::Seq(vec![
        IrNode::Assign {
            local: arr,
            value: Box::new(IrNode::Array(Vec::new())),
        },
        IrNode::IndexSet {
            obj: Box::new(IrNode::Local(arr)),
            index: Box::new(IrNode::Str(INDEX_LENGTH_KEY.to_owned())),
            value: Box::new(body.arg(0)),
        },
        IrNode::Local(arr),
    ]);
    let is_number = IrNode::Binary {
        op: BinOp::Eq,
        lhs: Box::new(IrNode::TypeOf(Box::new(body.arg(0)))),
        rhs: Box::new(IrNode::Str("number".to_owned())),
    };
    let one = ternary(is_number, with_length, IrNode::Array(vec![body.arg(0)]));
    let spread = IrNode::ArraySpread(vec![SpreadElem::Spread(IrNode::SpreadArgsGlobal)]);
    let elements = (0..=body.k).rev().fold(spread, |more, n| {
        ternary(
            body.argc_is(n),
            IrNode::Array((0..n).map(|i| body.arg(i)).collect()),
            more,
        )
    });
    body.with_proto(BuiltinCtor::Array, ternary(body.argc_is(1), one, elements))
}

/// `String(v)` / `Number(v)` / `Boolean(v)`: the conversion of the first
/// argument (`empty` without one); under `new`, its wrapper object (design
/// 2.4) with `proto` from `new.target`.
fn conversion(body: &Body, ctor: BuiltinCtor, op: ConvertOp, empty: IrNode) -> IrNode {
    let convert = IrNode::ConvertCall {
        op,
        arg: Box::new(body.arg(0)),
        radix: None,
    };
    let value = ternary(body.argc_is(0), empty, convert);
    let v = body.temp(3);
    let wrapper = IrNode::Seq(vec![
        IrNode::Assign {
            local: v,
            value: Box::new(value.clone()),
        },
        body.with_proto(ctor, IrNode::ToObject(Box::new(IrNode::Local(v)))),
    ]);
    ternary(body.constructing(), wrapper, value)
}

/// `new Date()` / `new Date(ms)`; called, `Date()` is the current time as
/// `Date.prototype.toString` renders it. The calendar-field form is not supported.
fn date(body: &Body) -> IrNode {
    let ms = IrNode::ConvertCall {
        op: ConvertOp::ToNumber,
        arg: Box::new(body.arg(0)),
        radix: None,
    };
    let alloc = ternary(
        body.argc_is(0),
        IrNode::DateNew { ms: None },
        ternary(
            body.argc_is(1),
            IrNode::DateNew {
                ms: Some(Box::new(ms)),
            },
            throw_type_error("the Date calendar-field constructor is not supported"),
        ),
    );
    let now = body.temp(3);
    ternary(
        body.constructing(),
        body.with_proto(BuiltinCtor::Date, alloc),
        IrNode::Seq(vec![
            IrNode::Assign {
                local: now,
                value: Box::new(IrNode::DateNew { ms: None }),
            },
            IrNode::DateField {
                op: DateOp::ToString,
                recv: now,
            },
        ]),
    )
}
