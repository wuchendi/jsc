//! `Date.parse` and the string path of `new Date(value)` (ES2024 §21.4.3.2,
//! §21.4.2.1 step 4.b): a guest-side parser of exactly two families, anything
//! else is `NaN`:
//!
//! - the Date Time String Format (§21.4.1.32): `YYYY`, `YYYY-MM`,
//!   `YYYY-MM-DD` (or a `±YYYYYY` expanded year, `-000000` rejected),
//!   optionally followed by `THH:mm`, `THH:mm:ss` or `THH:mm:ss.sss` and a `Z`
//!   or `±HH:mm` offset. Fields are range-checked (`24:00:00.000` is the only
//!   hour-24 time). A date-only form is UTC; a date-time without offset is
//!   local time, which is UTC here (`LocalTZA = 0`).
//! - the exact `Date.prototype.toString` output (`Www Mmm DD YYYY HH:mm:ss
//!   GMT±hhmm`, optionally followed by a ` (..)` zone name) and the
//!   `toUTCString` output (`Www, DD Mmm YYYY HH:mm:ss GMT`), the year signed
//!   and at least four digits.
//!
//! The parsed fields go through the shared `MakeDay` / `MakeTime` /
//! `MakeDate` and `TimeClip`.

#[allow(clippy::wildcard_imports)]
use super::*;

use super::ctor::{assign, bin};
use super::date::{emit_clipped, make_date, make_day, make_time};

/// The parser state: the string block, its length and width, the cursor
/// and three i32 temps (all frame i32 locals).
#[derive(Clone, Copy)]
struct Cursor {
    s: u32,
    len: u32,
    w: u32,
    i: u32,
    /// The value of the last [`Self::digits`] / [`Self::name`].
    c: u32,
    n: u32,
    d: u32,
}

impl Cursor {
    /// Push the code unit at the cursor, or `-1` at the end.
    fn peek(self, function: &mut Function) {
        wasm!(function;
            LocalGet(self.i), LocalGet(self.len), I32LtS,
            If(BlockType::Result(ValType::I32)),
        );
        emit_str_unit(function, self.s, self.i, self.w);
        wasm!(function; Else, I32Const(-1), End);
    }

    /// Push whether the unit at the cursor is `unit`.
    fn at(self, function: &mut Function, unit: u8) {
        self.peek(function);
        wasm!(function; I32Const(i32::from(unit)), I32Eq);
    }

    /// Advance the cursor by one unit.
    fn bump(self, function: &mut Function) {
        wasm!(function; LocalGet(self.i), I32Const(1), I32Add, LocalSet(self.i));
    }

    /// Consume the units of `text`, else `NaN`.
    fn expect(self, function: &mut Function, text: &str) {
        for unit in text.bytes() {
            self.at(function, unit);
            wasm!(function; I32Eqz);
            fail_if(function);
            self.bump(function);
        }
    }

    /// Consume exactly `count` decimal digits into `c`, else `NaN`.
    fn digits(self, function: &mut Function, count: i32) {
        wasm!(function;
            I32Const(0), LocalSet(self.c), I32Const(count), LocalSet(self.n),
            Loop(BlockType::Empty),
        );
        self.peek(function);
        wasm!(function; I32Const(i32::from(b'0')), I32Sub, LocalTee(self.d), I32Const(9), I32GtU);
        fail_if(function);
        wasm!(function;
            LocalGet(self.c), I32Const(10), I32Mul, LocalGet(self.d), I32Add, LocalSet(self.c),
        );
        self.bump(function);
        wasm!(function;
            LocalGet(self.n), I32Const(1), I32Sub, LocalTee(self.n), BrIf(0),
            End,
        );
    }

    /// Consume four to six decimal digits into `c`, else `NaN`.
    fn year_digits(self, function: &mut Function) {
        wasm!(function;
            I32Const(0), LocalSet(self.c), I32Const(0), LocalSet(self.n),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(self.n), I32Const(6), I32GeS, BrIf(1),
        );
        self.peek(function);
        wasm!(function;
            I32Const(i32::from(b'0')), I32Sub, LocalTee(self.d), I32Const(9), I32GtU, BrIf(1),
            LocalGet(self.c), I32Const(10), I32Mul, LocalGet(self.d), I32Add, LocalSet(self.c),
        );
        self.bump(function);
        wasm!(function;
            LocalGet(self.n), I32Const(1), I32Add, LocalSet(self.n),
            Br(0),
            End, End,
            LocalGet(self.n), I32Const(4), I32LtS,
        );
        fail_if(function);
    }

    /// `NaN` unless `lo <= c <= hi`.
    fn check(self, function: &mut Function, lo: i32, hi: i32) {
        wasm!(function;
            LocalGet(self.c), I32Const(lo), I32LtS,
            LocalGet(self.c), I32Const(hi), I32GtS, I32Or,
        );
        fail_if(function);
    }

    /// Consume a three-letter name of `table` (three-byte entries) into `c`
    /// as its index, else `NaN`.
    fn name(self, function: &mut Function, table: &str) {
        wasm!(function; I32Const(0), LocalSet(self.n));
        for _ in 0..3 {
            self.peek(function);
            wasm!(function; LocalTee(self.d), I32Const(0xFF), I32GtU);
            fail_if(function);
            wasm!(function;
                LocalGet(self.n), I32Const(8), I32Shl, LocalGet(self.d), I32Or, LocalSet(self.n),
            );
            self.bump(function);
        }
        wasm!(function; I32Const(-1), LocalSet(self.c));
        for (index, name) in (0i32..).zip(table.as_bytes().chunks(3)) {
            let key = name.iter().fold(0i32, |key, &b| (key << 8) | i32::from(b));
            wasm!(function;
                LocalGet(self.n), I32Const(key), I32Eq,
                If(BlockType::Empty), I32Const(index), LocalSet(self.c), End,
            );
        }
        wasm!(function; LocalGet(self.c), I32Const(0), I32LtS);
        fail_if(function);
    }

    /// Store `c` (plus `bias`) as a number jsval into i64 `field`.
    fn store(self, function: &mut Function, field: u32, bias: i32) {
        wasm!(function;
            LocalGet(self.c), I32Const(bias), I32Add,
            F64ConvertI32S, I64ReinterpretF64, LocalSet(field),
        );
    }
}

/// Return `NaN` when the i32 on the stack is nonzero.
fn fail_if(function: &mut Function) {
    wasm!(function; If(BlockType::Empty), I64Const(QNAN_BITS), Return, End);
}

/// Push the small integral number jsval in i64 `field` as an i32.
fn field_i32(function: &mut Function, field: u32) {
    wasm!(function; LocalGet(field), F64ReinterpretI64, I32TruncSatF64S);
}

/// The parsed fields (number jsvals in i64 locals) and the offset east of
/// UTC in minutes (i32 local).
#[derive(Clone, Copy)]
struct Fields {
    year: u32,
    month: u32,
    day: u32,
    time: [u32; 4],
    offset: u32,
}

/// The `HH:mm:ss` of the text forms into the time fields.
fn text_time(function: &mut Function, p: Cursor, f: Fields) {
    p.digits(function, 2);
    p.check(function, 0, 23);
    p.store(function, f.time[0], 0);
    for field in [f.time[1], f.time[2]] {
        p.expect(function, ":");
        p.digits(function, 2);
        p.check(function, 0, 59);
        p.store(function, field, 0);
    }
}

/// A signed year of the text forms (`-` then four to six digits; `-0000`
/// rejected) into the year field.
fn text_year(function: &mut Function, p: Cursor, f: Fields, sign: u32) {
    p.at(function, b'-');
    wasm!(function; LocalTee(sign));
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    wasm!(function; End);
    p.year_digits(function);
    wasm!(function; LocalGet(sign), LocalGet(p.c), I32Eqz, I32And);
    fail_if(function);
    negate_if(function, p, sign);
    p.store(function, f.year, 0);
}

/// `c = sign ? -c : c`.
fn negate_if(function: &mut Function, p: Cursor, sign: u32) {
    wasm!(function;
        I32Const(0), LocalGet(p.c), I32Sub, LocalGet(p.c), LocalGet(sign), Select, LocalSet(p.c),
    );
}

/// The `toString` / `toUTCString` forms, after the weekday name.
fn emit_text(function: &mut Function, p: Cursor, f: Fields, sign: u32) {
    p.at(function, b',');
    wasm!(function; If(BlockType::Empty));
    // `toUTCString`: `, DD Mmm YYYY HH:mm:ss GMT`.
    p.expect(function, ", ");
    p.digits(function, 2);
    p.check(function, 1, 31);
    p.store(function, f.day, 0);
    p.expect(function, " ");
    p.name(function, DATE_MONTHS);
    p.store(function, f.month, 0);
    p.expect(function, " ");
    text_year(function, p, f, sign);
    p.expect(function, " ");
    text_time(function, p, f);
    p.expect(function, " GMT");
    wasm!(function; Else);
    // `toString`: ` Mmm DD YYYY HH:mm:ss GMT±hhmm[ (..)]`.
    p.expect(function, " ");
    p.name(function, DATE_MONTHS);
    p.store(function, f.month, 0);
    p.expect(function, " ");
    p.digits(function, 2);
    p.check(function, 1, 31);
    p.store(function, f.day, 0);
    p.expect(function, " ");
    text_year(function, p, f, sign);
    p.expect(function, " ");
    text_time(function, p, f);
    p.expect(function, " GMT");
    p.at(function, b'-');
    wasm!(function; LocalSet(sign));
    p.at(function, b'+');
    wasm!(function; LocalGet(sign), I32Or, I32Eqz);
    fail_if(function);
    p.bump(function);
    p.digits(function, 4);
    // hhmm: hours 00-23, minutes 00-59.
    wasm!(function;
        LocalGet(p.c), I32Const(100), I32RemU, I32Const(59), I32GtU,
        LocalGet(p.c), I32Const(2359), I32GtU, I32Or,
    );
    fail_if(function);
    wasm!(function;
        LocalGet(p.c), I32Const(100), I32DivU, I32Const(60), I32Mul,
        LocalGet(p.c), I32Const(100), I32RemU, I32Add, LocalSet(p.c),
    );
    negate_if(function, p, sign);
    wasm!(function; LocalGet(p.c), LocalSet(f.offset));
    // An optional ` (<zone name>)` running to the end.
    wasm!(function; LocalGet(p.i), LocalGet(p.len), I32LtS, If(BlockType::Empty));
    p.expect(function, " (");
    wasm!(function;
        LocalGet(p.i), LocalGet(p.len), I32Const(1), I32Sub, I32GtS,
    );
    fail_if(function);
    wasm!(function; LocalGet(p.len), I32Const(1), I32Sub, LocalSet(p.i));
    p.expect(function, ")");
    wasm!(function; End, End);
}

/// The Date Time String Format.
fn emit_iso(function: &mut Function, p: Cursor, f: Fields, sign: u32) {
    // The year: `±YYYYYY` (not `-000000`) or `YYYY`.
    p.at(function, b'-');
    wasm!(function; LocalTee(sign));
    p.at(function, b'+');
    wasm!(function; I32Or, If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 6);
    wasm!(function; LocalGet(sign), LocalGet(p.c), I32Eqz, I32And);
    fail_if(function);
    negate_if(function, p, sign);
    wasm!(function; Else);
    p.digits(function, 4);
    wasm!(function; End);
    p.store(function, f.year, 0);
    // `-MM` then `-DD`.
    p.at(function, b'-');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 2);
    p.check(function, 1, 12);
    p.store(function, f.month, -1);
    p.at(function, b'-');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 2);
    p.check(function, 1, 31);
    p.store(function, f.day, 0);
    wasm!(function; End, End);
    // `THH:mm[:ss[.sss]]` and an optional `Z` / `±HH:mm`.
    p.at(function, b'T');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 2);
    p.check(function, 0, 24);
    p.store(function, f.time[0], 0);
    p.expect(function, ":");
    p.digits(function, 2);
    p.check(function, 0, 59);
    p.store(function, f.time[1], 0);
    p.at(function, b':');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 2);
    p.check(function, 0, 59);
    p.store(function, f.time[2], 0);
    p.at(function, b'.');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 3);
    p.store(function, f.time[3], 0);
    wasm!(function; End, End);
    // Hour 24 only as `24:00:00.000`.
    field_i32(function, f.time[0]);
    wasm!(function; I32Const(24), I32Eq);
    for &field in &f.time[1..] {
        field_i32(function, field);
    }
    wasm!(function; I32Or, I32Or, I32Const(0), I32Ne, I32And);
    fail_if(function);
    p.at(function, b'Z');
    wasm!(function; If(BlockType::Empty));
    p.bump(function);
    wasm!(function; Else);
    p.at(function, b'-');
    wasm!(function; LocalTee(sign));
    p.at(function, b'+');
    wasm!(function; I32Or, If(BlockType::Empty));
    p.bump(function);
    p.digits(function, 2);
    p.check(function, 0, 23);
    wasm!(function; LocalGet(p.c), I32Const(60), I32Mul, LocalSet(f.offset));
    p.expect(function, ":");
    p.digits(function, 2);
    p.check(function, 0, 59);
    wasm!(function; LocalGet(p.c), LocalGet(f.offset), I32Add, LocalSet(p.c));
    negate_if(function, p, sign);
    wasm!(function; LocalGet(p.c), LocalSet(f.offset), End, End, End);
}

/// `NaN` unless the day field is within its month (leap years included).
fn check_day(function: &mut Function, p: Cursor, f: Fields) {
    let (year, month) = (p.n, p.d);
    field_i32(function, f.year);
    wasm!(function; LocalSet(year));
    field_i32(function, f.month);
    wasm!(function;
        I32Const(1), I32Add, LocalSet(month),
        LocalGet(month), I32Const(2), I32Eq,
        If(BlockType::Result(ValType::I32)),
        // 28 + leap(year).
        I32Const(28),
        LocalGet(year), I32Const(4), I32RemS, I32Eqz,
        LocalGet(year), I32Const(100), I32RemS, I32Const(0), I32Ne,
        LocalGet(year), I32Const(400), I32RemS, I32Eqz, I32Or, I32And,
        I32Add,
        Else,
        // 30 + ((month + month / 8) & 1).
        I32Const(30),
        LocalGet(month), LocalGet(month), I32Const(3), I32ShrU, I32Add, I32Const(1), I32And,
        I32Add,
        End,
    );
    field_i32(function, f.day);
    wasm!(function; I32LtS);
    fail_if(function);
}

/// The internal parser body: `this` is a string; returns the `TimeClip`ped
/// time value it denotes, or `NaN`.
pub(super) fn emit_parse_string(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let p = Cursor {
        s: frame.i32(0),
        len: frame.i32(1),
        w: frame.i32(2),
        i: frame.i32(3),
        c: frame.x32(0),
        n: frame.x32(1),
        d: frame.y32(0),
    };
    let sign = frame.y32(1);
    let f = Fields {
        year: frame.y64(0),
        month: frame.y64(1),
        day: frame.y64(2),
        time: [frame.y64(3), frame.y64(4), frame.y64(5), frame.x64(0)],
        offset: frame.y32(2),
    };
    let offset_ms = frame.x64(1);
    wasm!(function;
        GlobalGet(THIS_GLOBAL), I32WrapI64, LocalTee(p.s),
        I32Load(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }),
        LocalSet(p.len),
    );
    emit_str_two_byte(function, p.s);
    wasm!(function; LocalSet(p.w), I32Const(0), LocalSet(p.i), I32Const(0), LocalSet(f.offset));
    for (field, value) in [(f.month, 0.0), (f.day, 1.0)]
        .into_iter()
        .chain(f.time.map(|l| (l, 0.0)))
    {
        wasm!(function; I64Const(f64::to_bits(value).cast_signed()), LocalSet(field));
    }
    // A leading digit or sign is the Date Time String Format, else a text form
    // starting with its weekday name.
    p.peek(function);
    wasm!(function;
        I32Const(i32::from(b'0')), I32Sub, I32Const(9), I32LeU,
    );
    p.at(function, b'-');
    wasm!(function; I32Or);
    p.at(function, b'+');
    wasm!(function; I32Or, If(BlockType::Empty));
    emit_iso(function, p, f, sign);
    wasm!(function; Else);
    p.name(function, DATE_WEEKDAYS);
    emit_text(function, p, f, sign);
    wasm!(function; End);
    // Nothing may follow.
    wasm!(function; LocalGet(p.i), LocalGet(p.len), I32Ne);
    fail_if(function);
    check_day(function, p, f);
    wasm!(function;
        LocalGet(f.offset), F64ConvertI32S, F64Const(Ieee64::from(60_000.0)), F64Mul,
        I64ReinterpretF64, LocalSet(offset_ms),
    );
    emit_node(
        &parse_time(frame, f, offset_ms),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    emit_clipped(function, frame.i64(2), helpers, |_| {});
}

/// `MakeDate(MakeDay(year, month, day), MakeTime(..)) - offset` of the
/// parsed fields, unclipped (clobbering the year and month fields).
fn parse_time(frame: Frame, f: Fields, offset_ms: u32) -> IrNode {
    let t = frame.i64(1);
    let mut seq: Vec<IrNode> = make_day(f.year, f.month, f.day, t).into();
    seq.push(assign(t, make_date(IrNode::Local(t), make_time(f.time))));
    seq.push(bin(BinOp::Sub, IrNode::Local(t), IrNode::Local(offset_ms)));
    IrNode::Seq(seq)
}

/// `Date.parse(string)`: `ToString(string)`, then the internal parser with
/// it as `this`.
pub(super) fn emit_parse(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let string = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; Call(helpers.to_str()), LocalSet(frame.i32(0)));
    return_if_pending(function);
    wasm!(function;
        LocalGet(frame.i32(0)), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(string),
    );
    emit_date_parse_call(function, string, helpers);
}
