//! The constructor bodies (DESIGN.md §6.4, §7.6, §7.7) as IR fragments over the
//! argument slots. A constructed call carries the construct marker in `argc`
//! (decoded by [`IrNode::ArgCount`]); `SELF_ALLOC` constructors allocate their
//! own kind of object with `proto = Get(new.target, "prototype")` (falling back
//! to the realm prototype), so builtin parents work under `extends`.

#[allow(clippy::wildcard_imports)]
use super::*;

use super::date::date_fields_time;

/// The fragment builder of one body: the `K` argument slots, `argc` and the
/// i64 temps of a [`Frame`].
pub(super) struct Body {
    /// The uniform ABI's positional arity `K`.
    pub(super) k: u32,
    /// The rooted local holding a snapshot of `$__spread_args`, read instead
    /// of the global past the `K` slots.
    spread: Option<u32>,
}

impl Body {
    /// The body under `K = k`, reading arguments past the slots from
    /// `$__spread_args` itself.
    pub(super) const fn new(k: u32) -> Self {
        Self::with_spread(k, None)
    }

    /// The body under `K = k` reading arguments past the slots from the
    /// snapshot in local `spread`, when given.
    pub(super) const fn with_spread(k: u32, spread: Option<u32>) -> Self {
        Self { k, spread }
    }

    /// The body under `K = k` of a constructor that reads arguments
    /// `0..reads` after user code may run (`Get(new.target, "prototype")`),
    /// with the assignment that must run first: when one is past the `K`
    /// slots, `$__spread_args` captured into the rooted `Frame::x64(1)`.
    fn snapshotted(k: u32, reads: u32) -> (Self, Option<IrNode>) {
        if reads <= k {
            return (Self::new(k), None);
        }
        let local = Frame::new(k).x64(1);
        let capture = IrNode::Assign {
            local,
            value: Box::new(IrNode::SpreadArgsGlobal),
        };
        (Self::with_spread(k, Some(local)), Some(capture))
    }

    /// The published spread argument array: the snapshot, else the global.
    fn spread_args(&self) -> IrNode {
        self.spread.map_or(IrNode::SpreadArgsGlobal, IrNode::Local)
    }

    /// Argument `i`: slot `i`, or past the `K` slots the element of the
    /// argument array a spread call or construct with more arguments
    /// published (`undefined` past the argument count). Without a snapshot,
    /// read it before any user code runs: a nested spread call replaces that
    /// array.
    pub(super) fn arg(&self, i: u32) -> IrNode {
        if i < self.k {
            return IrNode::Local(i);
        }
        ternary(
            IrNode::Binary {
                op: BinOp::Gt,
                lhs: Box::new(IrNode::ArgCount(self.k + 1)),
                rhs: Box::new(IrNode::Num(f64::from(i))),
            },
            IrNode::Index {
                obj: Box::new(self.spread_args()),
                index: Box::new(IrNode::Num(f64::from(i))),
            },
            IrNode::Undefined,
        )
    }

    /// Temp local `n` (`0..4`).
    pub(super) fn temp(&self, n: u32) -> u32 {
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
            index: Box::new(IrNode::Str("prototype".into())),
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
pub(super) fn ternary(cond: IrNode, then_branch: IrNode, else_branch: IrNode) -> IrNode {
    IrNode::Ternary {
        cond: Box::new(cond),
        then_branch: Box::new(then_branch),
        else_branch: Box::new(else_branch),
    }
}

/// `node` after the snapshot `capture` of [`Body::snapshotted`], if any.
fn after(capture: Option<IrNode>, node: IrNode) -> IrNode {
    match capture {
        Some(capture) => IrNode::Seq(vec![capture, node]),
        None => node,
    }
}

/// Throw a fresh `TypeError` with `message`.
fn throw_type_error(message: &str) -> IrNode {
    IrNode::Throw(Box::new(IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Str(message.into())),
        IrNode::Str(format!("TypeError: {message}").into()),
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
    let body = Body::new(k);
    match ctor {
        BuiltinCtor::Object => object(&body),
        BuiltinCtor::Function => throw_type_error("the Function constructor is not supported"),
        BuiltinCtor::Array => array(k),
        BuiltinCtor::String => conversion(
            &body,
            ctor,
            [ConvertOp::String, ConvertOp::ToString],
            IrNode::Str("".into()),
        ),
        BuiltinCtor::Number => conversion(&body, ctor, [ConvertOp::ToNumber; 2], IrNode::Num(0.0)),
        BuiltinCtor::Boolean => {
            conversion(&body, ctor, [ConvertOp::ToBoolean; 2], IrNode::Bool(false))
        }
        BuiltinCtor::Error(ErrorKind::AggregateError) => aggregate_error(&body),
        // The message is read after `GetPrototypeFromConstructor`.
        BuiltinCtor::Error(kind) => {
            let (body, capture) = Body::snapshotted(k, 1);
            let (proto, options) = (body.temp(1), body.temp(3));
            let error = IrNode::error_from_argument(
                kind,
                IrNode::Local(proto),
                body.arg(0),
                body.temp(0),
                body.temp(2),
            );
            let steps = vec![
                IrNode::Assign {
                    local: options,
                    value: Box::new(body.arg(1)),
                },
                IrNode::Assign {
                    local: proto,
                    value: Box::new(body.proto(ctor)),
                },
                IrNode::install_error_cause(error, IrNode::Local(options), proto, body.temp(0)),
            ];
            IrNode::Seq(capture.into_iter().chain(steps).collect())
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
        // The prototype only: `weak::emit_ctor` allocates the handle and adds
        // the iterable's values (`weak::emit_weak_ref_ctor` /
        // `weak::emit_finreg_ctor` check their argument first).
        BuiltinCtor::WeakMap
        | BuiltinCtor::WeakSet
        | BuiltinCtor::WeakRef
        | BuiltinCtor::FinalizationRegistry => body.requires_new(ctor, body.proto(ctor)),
        // The "requires 'new'" check only: `array_buffer::emit_ctor` converts
        // the arguments before it reads the prototype, `typed_array::emit_ctor`
        // runs the overloads (DESIGN.md §5.8.7).
        BuiltinCtor::ArrayBuffer | BuiltinCtor::Typed(_) => {
            body.requires_new(ctor, IrNode::Undefined)
        }
        // Abstract: calling or constructing it always throws.
        BuiltinCtor::TypedArray => {
            throw_type_error("Abstract class TypedArray not directly constructable")
        }
        // Under `K = 0` the executor of a spread construct (a subclass's
        // implicit `super(...args)`, a species construct) is in the spread
        // argument array, read after the `new.target.prototype` lookup.
        BuiltinCtor::Promise => {
            let (body, capture) = Body::snapshotted(k, 1);
            let promise = IrNode::PromiseCtor {
                executor: Box::new(body.arg(0)),
                p_local: body.temp(3),
            };
            body.requires_new(ctor, after(capture, body.with_proto(ctor, promise)))
        }
        BuiltinCtor::Date => date(k),
        // A hand-written body (`symbol::emit_ctor`).
        BuiltinCtor::Symbol => IrNode::Undefined,
        // Abstract: only a subclass `new.target` constructs (the allocated
        // `this`, `proto` from `new.target`, is the instance).
        BuiltinCtor::Iterator => ternary(
            IrNode::Logical {
                op: LogicalOp::And,
                lhs: Box::new(body.constructing()),
                rhs: Box::new(IrNode::Binary {
                    op: BinOp::Ne,
                    lhs: Box::new(IrNode::NewTargetGlobal),
                    rhs: Box::new(IrNode::Intrinsic(GlobalIntrinsic::Ctor(ctor))),
                }),
            },
            IrNode::Undefined,
            throw_type_error("Abstract class Iterator not directly constructable"),
        ),
    }
}

/// `AggregateError(errors, message, options)` (called or constructed): the
/// arguments captured first (a spread call's past the `K` slots too), then
/// the Error instance of `message` over `GetPrototypeFromConstructor`,
/// `InstallErrorCause` from `options`, then an own non-enumerable `errors`
/// array of the values iterating `errors` yields (a `TypeError` for a
/// non-iterable).
fn aggregate_error(body: &Body) -> IrNode {
    let ctor = BuiltinCtor::Error(ErrorKind::AggregateError);
    let (obj, proto) = (body.temp(0), body.temp(1));
    let frame = Frame::new(body.k);
    let (errors, message, options) = (frame.y64(0), frame.y64(1), frame.y64(2));
    let capture = |local: u32, i: u32| IrNode::Assign {
        local,
        value: Box::new(body.arg(i)),
    };
    let error = IrNode::error_from_argument(
        ErrorKind::AggregateError,
        IrNode::Local(proto),
        IrNode::Local(message),
        body.temp(2),
        body.temp(3),
    );
    IrNode::Seq(vec![
        capture(errors, 0),
        capture(message, 1),
        capture(options, 2),
        IrNode::Assign {
            local: proto,
            value: Box::new(body.proto(ctor)),
        },
        IrNode::install_error_cause(error, IrNode::Local(options), obj, proto),
        IrNode::DefineProps {
            obj: Box::new(IrNode::Local(obj)),
            local: proto,
            entries: vec![(
                PropKey::Hidden("errors".into()),
                IrNode::ArraySpread(vec![SpreadElem::Spread(IrNode::Local(errors))]),
            )],
        },
    ])
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
/// through a spread call, whose published argument array is copied). Every
/// argument is read after `GetPrototypeFromConstructor`, so the published
/// array is captured first.
fn array(k: u32) -> IrNode {
    let (body, capture) = Body::snapshotted(k, k + 1);
    let body = &body;
    let arr = body.temp(3);
    let with_length = IrNode::Seq(vec![
        IrNode::Assign {
            local: arr,
            value: Box::new(IrNode::Array(Vec::new())),
        },
        IrNode::IndexSet {
            obj: Box::new(IrNode::Local(arr)),
            index: Box::new(IrNode::Str(INDEX_LENGTH_KEY.into())),
            value: Box::new(body.arg(0)),
        },
        IrNode::Local(arr),
    ]);
    let is_number = IrNode::Binary {
        op: BinOp::Eq,
        lhs: Box::new(IrNode::TypeOf(Box::new(body.arg(0)))),
        rhs: Box::new(IrNode::Str("number".into())),
    };
    let one = ternary(is_number, with_length, IrNode::Array(vec![body.arg(0)]));
    let spread = IrNode::ArraySpread(vec![SpreadElem::List(body.spread_args())]);
    let elements = (0..=body.k).rev().fold(spread, |more, n| {
        ternary(
            body.argc_is(n),
            IrNode::Array((0..n).map(|i| body.arg(i)).collect()),
            more,
        )
    });
    let array = body.with_proto(BuiltinCtor::Array, ternary(body.argc_is(1), one, elements));
    after(capture, array)
}

/// `String(v)` / `Number(v)` / `Boolean(v)`: the conversion `call` of the
/// first argument (`empty` without one); under `new`, the wrapper object
/// (design 2.4) of its conversion `construct`, with `proto` from `new.target`
/// (`String(symbol)` describes the symbol, `new String(symbol)` throws).
fn conversion(
    body: &Body,
    ctor: BuiltinCtor,
    [call, construct]: [ConvertOp; 2],
    empty: IrNode,
) -> IrNode {
    let value = |op, empty| {
        let convert = IrNode::ConvertCall {
            op,
            arg: Box::new(body.arg(0)),
            radix: None,
        };
        ternary(body.argc_is(0), empty, convert)
    };
    let v = body.temp(3);
    let wrapper = IrNode::Seq(vec![
        IrNode::Assign {
            local: v,
            value: Box::new(value(construct, empty.clone())),
        },
        body.with_proto(ctor, IrNode::ToObject(Box::new(IrNode::Local(v)))),
    ]);
    ternary(body.constructing(), wrapper, value(call, empty))
}

/// `new Date()` / `new Date(ms)` / `new Date(year, month, ..)`; called,
/// `Date()` is the current time as `Date.prototype.toString` renders it. The
/// arguments (up to seven) are read after `GetPrototypeFromConstructor`.
fn date(k: u32) -> IrNode {
    let (body, capture) = Body::snapshotted(k, 7);
    let body = &body;
    let alloc = ternary(
        body.argc_is(0),
        IrNode::DateNew { arg: None },
        ternary(
            body.argc_is(1),
            IrNode::DateNew {
                arg: Some(Box::new(body.arg(0))),
            },
            date_fields(body),
        ),
    );
    let now = body.temp(3);
    let constructed = body.with_proto(BuiltinCtor::Date, alloc);
    ternary(
        body.constructing(),
        after(capture, constructed),
        IrNode::Seq(vec![
            IrNode::Assign {
                local: now,
                value: Box::new(IrNode::DateNew { arg: None }),
            },
            IrNode::DateField {
                op: DateOp::ToString,
                recv: now,
            },
        ]),
    )
}

/// The calendar-field form over at least two arguments (UTC: there is no
/// local time zone): [`date_fields_time`] into temp 3, then a fresh Date of
/// it, whose `TimeClip` turns a non-finite or out-of-range value into an
/// invalid date.
fn date_fields(body: &Body) -> IrNode {
    let t = body.temp(3);
    let mut seq = date_fields_time(body, t);
    seq.push(IrNode::DateNew {
        arg: Some(Box::new(IrNode::Local(t))),
    });
    IrNode::Seq(seq)
}

/// The number literal `n`.
pub(super) fn num(n: f64) -> IrNode {
    IrNode::Num(n)
}

/// `lhs op rhs`.
pub(super) fn bin(op: BinOp, lhs: IrNode, rhs: IrNode) -> IrNode {
    IrNode::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// `Math.trunc` of the number in local `l`.
pub(super) fn trunc(l: u32) -> IrNode {
    IrNode::MathCall {
        op: MathOp::Trunc,
        args: vec![IrNode::Local(l)],
    }
}

/// `Math.floor(n / d)`.
pub(super) fn floor_div(n: IrNode, d: f64) -> IrNode {
    IrNode::MathCall {
        op: MathOp::Floor,
        args: vec![bin(BinOp::Div, n, num(d))],
    }
}

/// `l = value`.
pub(super) fn assign(l: u32, value: IrNode) -> IrNode {
    IrNode::Assign {
        local: l,
        value: Box::new(value),
    }
}

/// The left-to-right sum of `terms`.
pub(super) fn sum<const N: usize>(terms: [IrNode; N]) -> IrNode {
    terms
        .into_iter()
        .reduce(|acc, term| bin(BinOp::Add, acc, term))
        .unwrap_or(IrNode::Num(0.0))
}
