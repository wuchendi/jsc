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

    /// Argument `i`, past the `K` slots the element of the argument array a
    /// spread call or construct with more arguments published (`undefined`
    /// past the argument count). Read it before any user code runs: a nested
    /// spread call replaces that array.
    fn spread_arg(&self, i: u32) -> IrNode {
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
                obj: Box::new(IrNode::SpreadArgsGlobal),
                index: Box::new(IrNode::Num(f64::from(i))),
            },
            IrNode::Undefined,
        )
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
        BuiltinCtor::Error(ErrorKind::AggregateError) => aggregate_error(&body),
        BuiltinCtor::Error(kind) => {
            let (proto, options) = (body.temp(1), body.temp(3));
            let error = IrNode::error_from_argument(
                kind,
                IrNode::Local(proto),
                body.arg(0),
                body.temp(0),
                body.temp(2),
            );
            IrNode::Seq(vec![
                IrNode::Assign {
                    local: options,
                    value: Box::new(body.spread_arg(1)),
                },
                IrNode::Assign {
                    local: proto,
                    value: Box::new(body.proto(ctor)),
                },
                IrNode::install_error_cause(error, IrNode::Local(options), proto, body.temp(0)),
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

/// `AggregateError(errors, message, options)` (called or constructed): the
/// arguments captured first (a spread call's past the `K` slots too), then
/// the Error instance of `message` over `GetPrototypeFromConstructor`,
/// `InstallErrorCause` from `options`, then an own non-enumerable `errors`
/// array of the values iterating `errors` yields (a `TypeError` for a
/// non-iterable).
fn aggregate_error(body: &Body) -> IrNode {
    let ctor = BuiltinCtor::Error(ErrorKind::AggregateError);
    let (obj, proto) = (body.temp(0), body.temp(1));
    let frame = Frame { k: body.k };
    let (errors, message, options) = (frame.y64(0), frame.y64(1), frame.y64(2));
    let capture = |local: u32, i: u32| IrNode::Assign {
        local,
        value: Box::new(body.spread_arg(i)),
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
                PropKey::Hidden("errors".to_owned()),
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

/// `new Date()` / `new Date(ms)` / `new Date(year, month, ..)`; called,
/// `Date()` is the current time as `Date.prototype.toString` renders it.
fn date(body: &Body) -> IrNode {
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
    ternary(
        body.constructing(),
        body.with_proto(BuiltinCtor::Date, alloc),
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
/// local time zone): every field captured first (a spread call's past the
/// `K` slots too) and converted with `ToNumber` in order (absent `date` is
/// `1`, absent time fields `0`), then `MakeDate(MakeDay(MakeFullYear(year),
/// month, date), MakeTime(h, min, s, ms))` into a fresh Date, whose
/// `TimeClip` turns a non-finite or out-of-range value into an invalid date.
/// Uses temp 3.
fn date_fields(body: &Body) -> IrNode {
    let frame = Frame { k: body.k };
    let f: [u32; 7] = [
        frame.y64(0),
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
        frame.y64(5),
        frame.x64(0),
    ];
    let t = body.temp(3);
    let mut seq: Vec<IrNode> = (0..7u32)
        .zip(f)
        .map(|(i, l)| {
            let default = if i == 2 { 1.0 } else { 0.0 };
            let present = bin(BinOp::Gt, IrNode::ArgCount(body.k + 1), num(f64::from(i)));
            assign(l, ternary(present, body.spread_arg(i), num(default)))
        })
        .collect();
    seq.extend(f.map(|l| {
        let arg = Box::new(IrNode::Local(l));
        assign(
            l,
            IrNode::ConvertCall {
                op: ConvertOp::ToNumber,
                arg,
                radix: None,
            },
        )
    }));
    seq.extend(make_day(f[0], f[1], f[2], t));
    // MakeDate(day, MakeTime(h, min, s, ms)).
    seq.push(assign(
        t,
        sum([
            bin(BinOp::Mul, IrNode::Local(t), num(86_400_000.0)),
            bin(BinOp::Mul, trunc(f[3]), num(3_600_000.0)),
            bin(BinOp::Mul, trunc(f[4]), num(60_000.0)),
            bin(BinOp::Mul, trunc(f[5]), num(1000.0)),
            trunc(f[6]),
        ]),
    ));
    seq.push(IrNode::DateNew {
        arg: Some(Box::new(IrNode::Local(t))),
    });
    IrNode::Seq(seq)
}

/// `MakeDay(MakeFullYear(year), month, date)` over the number locals into
/// local `t` (clobbering `year` and `month`): the month index carries into
/// the year, then `days_from_civil` of the month's first day over a
/// March-based year (`year` becomes the year of era, `month` the March-based
/// month, `t` the era), plus `date - 1`. NaN and infinities propagate.
fn make_day(year: u32, month: u32, date: u32, t: u32) -> [IrNode; 8] {
    let local = IrNode::Local;
    let two_digit = IrNode::Logical {
        op: LogicalOp::And,
        lhs: Box::new(bin(BinOp::Ge, trunc(year), num(0.0))),
        rhs: Box::new(bin(BinOp::Le, trunc(year), num(99.0))),
    };
    let before_march = || bin(BinOp::Lt, local(month), num(2.0));
    let full_year = ternary(
        two_digit,
        bin(BinOp::Add, num(1900.0), trunc(year)),
        trunc(year),
    );
    let carry = bin(BinOp::Mul, num(12.0), floor_div(trunc(month), 12.0));
    [
        assign(year, full_year),
        assign(
            year,
            bin(BinOp::Add, local(year), floor_div(trunc(month), 12.0)),
        ),
        assign(month, bin(BinOp::Sub, trunc(month), carry)),
        assign(
            year,
            ternary(
                before_march(),
                bin(BinOp::Sub, local(year), num(1.0)),
                local(year),
            ),
        ),
        assign(
            month,
            ternary(
                before_march(),
                bin(BinOp::Add, local(month), num(10.0)),
                bin(BinOp::Sub, local(month), num(2.0)),
            ),
        ),
        assign(t, floor_div(local(year), 400.0)),
        assign(
            year,
            bin(
                BinOp::Sub,
                local(year),
                bin(BinOp::Mul, local(t), num(400.0)),
            ),
        ),
        assign(
            t,
            sum([
                bin(BinOp::Mul, local(t), num(146_097.0)),
                bin(BinOp::Mul, local(year), num(365.0)),
                floor_div(local(year), 4.0),
                bin(BinOp::Sub, num(0.0), floor_div(local(year), 100.0)),
                floor_div(
                    bin(
                        BinOp::Add,
                        bin(BinOp::Mul, num(153.0), local(month)),
                        num(2.0),
                    ),
                    5.0,
                ),
                num(-719_469.0),
                trunc(date),
            ]),
        ),
    ]
}

/// The number literal `n`.
fn num(n: f64) -> IrNode {
    IrNode::Num(n)
}

/// `lhs op rhs`.
fn bin(op: BinOp, lhs: IrNode, rhs: IrNode) -> IrNode {
    IrNode::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

/// `Math.trunc` of the number in local `l`.
fn trunc(l: u32) -> IrNode {
    IrNode::MathCall {
        op: MathOp::Trunc,
        args: vec![IrNode::Local(l)],
    }
}

/// `Math.floor(n / d)`.
fn floor_div(n: IrNode, d: f64) -> IrNode {
    IrNode::MathCall {
        op: MathOp::Floor,
        args: vec![bin(BinOp::Div, n, num(d))],
    }
}

/// `l = value`.
fn assign(l: u32, value: IrNode) -> IrNode {
    IrNode::Assign {
        local: l,
        value: Box::new(value),
    }
}

/// The left-to-right sum of `terms`.
fn sum<const N: usize>(terms: [IrNode; N]) -> IrNode {
    terms
        .into_iter()
        .reduce(|acc, term| bin(BinOp::Add, acc, term))
        .unwrap_or(IrNode::Num(0.0))
}
