//! `%Date.prototype%` methods, `Date.UTC` and the time-value arithmetic
//! (`MakeTime` / `MakeDay` / `MakeDate`, ES2024 §21.4.1) the constructor
//! shares. The local time zone is UTC (`LocalTZA = 0`): a local setter is its
//! `setUTC*` twin.

#[allow(clippy::wildcard_imports)]
use super::*;

use super::ctor::{Body, assign, bin, floor_div, num, sum, ternary, trunc};

/// The `TypeError` of a `Date.prototype` method on a non-Date receiver.
const NOT_DATE: CalleeErrorText<'static> = [
    "this is not a Date object.",
    "TypeError: this is not a Date object.",
];

/// The `TypeError` of `Date.prototype[@@toPrimitive]` on a non-object.
const TO_PRIMITIVE_NOT_OBJECT: CalleeErrorText<'static> = [
    "Date.prototype [ @@toPrimitive ] called on non-object",
    "TypeError: Date.prototype [ @@toPrimitive ] called on non-object",
];

/// The `TypeError` of `Date.prototype[@@toPrimitive]` with a hint other
/// than `"default"`, `"number"` or `"string"`.
const INVALID_HINT: CalleeErrorText<'static> = ["Invalid hint", "TypeError: Invalid hint"];

/// The static strings of this family (the method texts included).
pub(super) const STRINGS: [&str; 15] = [
    NOT_DATE[0],
    NOT_DATE[1],
    TO_PRIMITIVE_NOT_OBJECT[0],
    TO_PRIMITIVE_NOT_OBJECT[1],
    INVALID_HINT[0],
    INVALID_HINT[1],
    "default",
    "number",
    "string",
    INVALID_DATE,
    INVALID_TIME_VALUE[0],
    INVALID_TIME_VALUE[1],
    DATE_WEEKDAYS,
    DATE_MONTHS,
    DATE_TAIL,
];

/// Read `this` into `this` and throw unless it is a Date.
fn this_date(
    function: &mut Function,
    frame: Frame,
    this: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_pointer(function, this);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, this);
    wasm!(function;
        I32Const(TYPE_DATE), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, NOT_DATE, strings, helpers);
    wasm!(function; End);
}

/// The Date method `op`: its field of the Date receiver.
pub(super) fn emit_method(
    op: DateOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let this = frame.i64(0);
    this_date(function, frame, this, strings, helpers);
    let node = IrNode::DateField { op, recv: this };
    emit_node(&node, function, frame.scratch(), strings, helpers);
}

/// The IR fragments the bodies emit, over the locals of `K = k`, for the
/// static-string scan.
pub(super) fn fragments(k: u32) -> Vec<IrNode> {
    let frame = Frame { k };
    let mut nodes = vec![
        to_json_object(frame),
        to_iso_string_call(frame),
        utc_time(frame),
    ];
    nodes.extend(SET_OPS.map(|op| set_time(op, frame)));
    nodes
}

/// Every setter body.
const SET_OPS: [DateSetOp; 9] = [
    DateSetOp::Milliseconds,
    DateSetOp::Seconds,
    DateSetOp::Minutes,
    DateSetOp::Hours,
    DateSetOp::Date,
    DateSetOp::Month,
    DateSetOp::FullYear,
    DateSetOp::Time,
    DateSetOp::Year,
];

/// Milliseconds per day.
const MS_PER_DAY: f64 = 86_400_000.0;

/// The time-of-day fields, in `MakeTime` order.
const TIME_FIELDS: [DateOp; 4] = [
    DateOp::GetHours,
    DateOp::GetMinutes,
    DateOp::GetSeconds,
    DateOp::GetMilliseconds,
];

/// The calendar fields, in `MakeDay` order.
const DAY_FIELDS: [DateOp; 3] = [DateOp::GetFullYear, DateOp::GetMonth, DateOp::GetDate];

/// `ToNumber(value)`.
fn to_number(value: IrNode) -> IrNode {
    IrNode::ConvertCall {
        op: ConvertOp::ToNumber,
        arg: Box::new(value),
        radix: None,
    }
}

/// Whether the number in local `l` is `NaN`.
fn is_nan(l: u32) -> IrNode {
    bin(BinOp::Ne, IrNode::Local(l), IrNode::Local(l))
}

/// `MakeFullYear` of the number in local `year`, in place: an integral year
/// in `0..=99` is `1900 + year`.
fn make_full_year(year: u32) -> IrNode {
    let two_digit = IrNode::Logical {
        op: LogicalOp::And,
        lhs: Box::new(bin(BinOp::Ge, trunc(year), num(0.0))),
        rhs: Box::new(bin(BinOp::Le, trunc(year), num(99.0))),
    };
    assign(
        year,
        ternary(
            two_digit,
            bin(BinOp::Add, num(1900.0), trunc(year)),
            trunc(year),
        ),
    )
}

/// `MakeTime(h, min, s, ms)` of the number locals: `((h * msPerHour + min *
/// msPerMinute) + s * msPerSecond) + ms` over their integer parts. A
/// non-finite field makes it non-finite, which `TimeClip` turns into `NaN`.
fn make_time([h, min, s, ms]: [u32; 4]) -> IrNode {
    sum([
        bin(BinOp::Mul, trunc(h), num(3_600_000.0)),
        bin(BinOp::Mul, trunc(min), num(60_000.0)),
        bin(BinOp::Mul, trunc(s), num(1000.0)),
        trunc(ms),
    ])
}

/// `MakeDate(day, time)`: `day * msPerDay + time`.
fn make_date(day: IrNode, time: IrNode) -> IrNode {
    bin(BinOp::Add, bin(BinOp::Mul, day, num(MS_PER_DAY)), time)
}

/// `MakeDay(year, month, date)` over the number locals into local `t`
/// (clobbering `year` and `month`): the month index carries into the year,
/// then `days_from_civil` of the month's first day over a March-based year
/// (`year` becomes the year of era, `month` the March-based month, `t` the
/// era), plus `date - 1`. NaN and infinities propagate.
fn make_day(year: u32, month: u32, date: u32, t: u32) -> [IrNode; 7] {
    let local = IrNode::Local;
    let before_march = || bin(BinOp::Lt, local(month), num(2.0));
    let carry = bin(BinOp::Mul, num(12.0), floor_div(trunc(month), 12.0));
    [
        assign(
            year,
            bin(BinOp::Add, trunc(year), floor_div(trunc(month), 12.0)),
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

/// The time value of the calendar-field arguments `(year, month[, date[,
/// hours[, minutes[, seconds[, ms]]]]])` (the `Date` constructor's field form
/// and `Date.UTC`) into local `t`, unclipped: every field captured first (a
/// spread call's past the `K` slots too) and converted with `ToNumber` in
/// order (absent `year` is `undefined`, absent `month` `0`, `date` `1`, time
/// fields `0`), then `MakeDate(MakeDay(MakeFullYear(year), month, date),
/// MakeTime(h, min, s, ms))`.
pub(super) fn date_fields_time(body: &Body, t: u32) -> Vec<IrNode> {
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
    let mut seq: Vec<IrNode> = (0..7u32)
        .zip(f)
        .map(|(i, l)| {
            if i == 0 {
                return assign(l, body.spread_arg(0));
            }
            let default = if i == 2 { 1.0 } else { 0.0 };
            let present = bin(BinOp::Gt, IrNode::ArgCount(body.k + 1), num(f64::from(i)));
            assign(l, ternary(present, body.spread_arg(i), num(default)))
        })
        .collect();
    seq.extend(f.map(|l| assign(l, to_number(IrNode::Local(l)))));
    seq.push(make_full_year(f[0]));
    seq.extend(make_day(f[0], f[1], f[2], t));
    seq.push(assign(
        t,
        make_date(IrNode::Local(t), make_time([f[3], f[4], f[5], f[6]])),
    ));
    seq
}

/// The unclipped `Date.UTC(..)` time value ([`date_fields_time`] in temp 3).
fn utc_time(frame: Frame) -> IrNode {
    let body = Body { k: frame.k };
    let t = body.temp(3);
    let mut seq = date_fields_time(&body, t);
    seq.push(IrNode::Local(t));
    IrNode::Seq(seq)
}

/// `TimeClip` of the number jsval on the stack into i64 `out`, then that
/// clipped number boxed on the stack, after `store` (emitted between).
fn emit_clipped(
    function: &mut Function,
    out: u32,
    helpers: Helpers<'_>,
    store: impl FnOnce(&mut Function),
) {
    wasm!(function; F64ReinterpretI64);
    emit_time_clip(function, out);
    store(function);
    wasm!(function; LocalGet(out), F64ReinterpretI64);
    emit_box_f64(function, helpers);
}

/// `Date.UTC(year[, month[, date[, hours[, minutes[, seconds[, ms]]]]]])`:
/// `TimeClip` of the [`utc_time`] — a number, nothing allocated.
pub(super) fn emit_utc(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(
        &utc_time(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    emit_clipped(function, frame.i64(1), helpers, |_| {});
}

/// The unclipped new time value of the setter `op` over the Date in i64 temp
/// 0 (ES2024 §21.4.4.20-28, Annex B.2.3.2), with `t` (its time value, read
/// first) in further temp 0. The fields the arguments replace are captured
/// (absent optional ones default to `t`'s fields, read before any user code
/// runs) and then converted with `ToNumber` in order, before anything looks
/// at `t`; a `NaN` `t` is `+0` for `setFullYear` / `setYear` (the caller
/// returns `NaN` for the others). `setTime(time)` is `ToNumber(time)`.
fn set_time(op: DateSetOp, frame: Frame) -> IrNode {
    let body = Body { k: frame.k };
    let (this, t) = (frame.i64(0), frame.y64(0));
    let (fields, first, count): (&[DateOp], usize, usize) = match op {
        DateSetOp::Time => return to_number(body.spread_arg(0)),
        DateSetOp::Milliseconds => (&TIME_FIELDS, 3, 1),
        DateSetOp::Seconds => (&TIME_FIELDS, 2, 2),
        DateSetOp::Minutes => (&TIME_FIELDS, 1, 3),
        DateSetOp::Hours => (&TIME_FIELDS, 0, 4),
        DateSetOp::Date => (&DAY_FIELDS, 2, 1),
        DateSetOp::Month => (&DAY_FIELDS, 1, 2),
        DateSetOp::FullYear => (&DAY_FIELDS, 0, 3),
        DateSetOp::Year => (&DAY_FIELDS, 0, 1),
    };
    let nan_is_zero = matches!(op, DateSetOp::FullYear | DateSetOp::Year);
    let f = [frame.y64(1), frame.y64(2), frame.y64(3), frame.y64(4)];
    let mut seq = vec![assign(
        t,
        IrNode::DateField {
            op: DateOp::GetTime,
            recv: this,
        },
    )];
    for (j, (&field, &l)) in fields.iter().zip(&f).enumerate() {
        let mut default = IrNode::DateField {
            op: field,
            recv: this,
        };
        if nan_is_zero {
            let zero = if field == DateOp::GetDate { 1.0 } else { 0.0 };
            default = ternary(is_nan(t), num(zero), default);
        }
        let value = if j < first || j >= first + count {
            default
        } else {
            let i = u32::try_from(j - first).unwrap_or(0);
            if i == 0 {
                body.spread_arg(0)
            } else {
                let present = bin(BinOp::Gt, IrNode::ArgCount(body.k + 1), num(f64::from(i)));
                ternary(present, body.spread_arg(i), default)
            }
        };
        seq.push(assign(l, value));
    }
    for &l in &f[first..first + count] {
        seq.push(assign(l, to_number(IrNode::Local(l))));
    }
    if fields.len() == TIME_FIELDS.len() {
        let day = floor_div(IrNode::Local(t), MS_PER_DAY);
        seq.push(make_date(day, make_time(f)));
    } else {
        if op == DateSetOp::Year {
            seq.push(make_full_year(f[0]));
        }
        if nan_is_zero {
            seq.push(assign(t, ternary(is_nan(t), num(0.0), IrNode::Local(t))));
        }
        let day = frame.y64(5);
        seq.extend(make_day(f[0], f[1], f[2], day));
        let local = || IrNode::Local(t);
        let time_within_day = bin(
            BinOp::Sub,
            local(),
            bin(BinOp::Mul, floor_div(local(), MS_PER_DAY), num(MS_PER_DAY)),
        );
        seq.push(make_date(IrNode::Local(day), time_within_day));
    }
    IrNode::Seq(seq)
}

/// The `Date.prototype` setter `op`: `thisTimeValue` (a `TypeError` for a
/// non-Date), then [`set_time`]; a `NaN` time value read before the argument
/// conversions returns `NaN` without a store (except `setFullYear` /
/// `setYear` / `setTime`), else the `TimeClip`ped value is stored into the
/// Date and returned.
pub(super) fn emit_set(
    op: DateSetOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, t, out) = (frame.i64(0), frame.y64(0), frame.i64(1));
    this_date(function, frame, this, strings, helpers);
    emit_node(
        &set_time(op, frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function; LocalSet(out));
    if !matches!(op, DateSetOp::FullYear | DateSetOp::Year | DateSetOp::Time) {
        wasm!(function;
            LocalGet(t), F64ReinterpretI64, LocalGet(t), F64ReinterpretI64, F64Ne,
            If(BlockType::Empty), LocalGet(t), Return, End,
        );
    }
    wasm!(function; LocalGet(out));
    emit_clipped(function, out, helpers, |function| {
        wasm!(function;
            LocalGet(this), I32WrapI64, LocalGet(out), F64ReinterpretI64,
            F64Store(date_ms_slot()),
        );
    });
}

/// `ToObject(this)` over the frame's i64 temp 0 (throwing for a nullish
/// `this`).
fn to_json_object(frame: Frame) -> IrNode {
    IrNode::ToObject(Box::new(IrNode::NullishCheck {
        value: Box::new(IrNode::Local(frame.i64(0))),
        key: "toJSON".to_owned(),
    }))
}

/// `Invoke(O, "toISOString")` over the frame's i64 temp 0.
fn to_iso_string_call(frame: Frame) -> IrNode {
    IrNode::MethodCallUser {
        obj: Box::new(IrNode::Local(frame.i64(0))),
        key: Box::new(IrNode::Str("toISOString".into())),
        args: vec![],
        obj_local: frame.i64(2),
        callee_local: frame.x64(0),
        not_callable: CalleeError::not_a_function("toISOString"),
    }
}

/// `Date.prototype.toJSON(key)`: `O = ToObject(this)`; `null` when
/// `ToPrimitive(O, number)` is a non-finite number, else
/// `Invoke(O, "toISOString")`. Generic over any receiver.
pub(super) fn emit_to_json(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (o, tv) = (frame.i64(0), frame.i64(1));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(o));
    emit_node(
        &to_json_object(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function;
        LocalSet(o),
        LocalGet(o), I32Const(HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(tv),
    );
    return_if_pending(function);
    emit_is_number(function, tv);
    wasm!(function;
        If(BlockType::Empty),
        // `tv - tv` is `0` exactly for a finite `tv`.
        LocalGet(tv), F64ReinterpretI64, LocalGet(tv), F64ReinterpretI64, F64Sub,
        F64Const(Ieee64::from(0.0)), F64Ne,
        If(BlockType::Empty), I64Const(NULL_BITS), Return, End,
        End,
    );
    emit_node(
        &to_iso_string_call(frame),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
}

/// `Date.prototype[@@toPrimitive](hint)`: `this` must be an object; hint
/// `"string"` / `"default"` try `toString` first, `"number"` `valueOf`
/// first (`OrdinaryToPrimitive`, `__to_primitive` with [`HINT_ORDINARY`]);
/// any other hint throws the `TypeError`. Generic over any object receiver.
pub(super) fn emit_to_primitive(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (this, arg, hint) = (frame.i64(0), frame.i64(1), frame.i32(0));
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    emit_is_object(function, this);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, TO_PRIMITIVE_NOT_OBJECT, strings, helpers);
    wasm!(function; End, I32Const(-1), LocalSet(hint));
    frame.arg(function, 0);
    wasm!(function; LocalSet(arg));
    emit_is_string(function, arg);
    wasm!(function; If(BlockType::Empty));
    for (name, value) in [
        ("string", HINT_STRING),
        ("default", HINT_STRING),
        ("number", HINT_NUMBER),
    ] {
        wasm!(function;
            LocalGet(arg), I32WrapI64, I32Const(static_key(strings, name)), Call(helpers.str_eq),
            If(BlockType::Empty), I32Const(value), LocalSet(hint), End,
        );
    }
    wasm!(function; End, LocalGet(hint), I32Const(0), I32LtS, If(BlockType::Empty));
    throw_type_error(function, frame, INVALID_HINT, strings, helpers);
    wasm!(function;
        End,
        LocalGet(this), LocalGet(hint), I32Const(HINT_ORDINARY), I32Or,
        Call(helpers.to_primitive()),
    );
}
