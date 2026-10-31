//! `%String.prototype%` methods and the `String` statics. A generic method
//! takes `ToString(RequireObjectCoercible(this))` and coerces its arguments in
//! the guest (a user `toString` / `valueOf` may run, and throw), then works on
//! the code units at each string's width (one-byte Latin-1 or two-byte
//! UTF-16, DESIGN.md §5.2), producing canonical strings; only the
//! case mappings, `localeCompare` and `normalize` run a host `__method` op.
//! `toString` / `valueOf` are `thisStringValue`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// How a method coerces one argument before its body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Coerce {
    /// Unused: passed as is.
    None,
    /// `ToString` (`undefined` becomes `"undefined"`).
    String,
    /// `ToString` unless `undefined` (which selects the default).
    OptString,
    /// `ToNumber` unless `undefined` (which selects the default).
    Number,
}

/// The coercions of `op`'s first two arguments (`concat`, `matchAll`,
/// `replace` and `replaceAll` coerce in their own bodies).
fn coercions(op: StringOp) -> [Coerce; 2] {
    match op {
        StringOp::IndexOf
        | StringOp::Includes
        | StringOp::StartsWith
        | StringOp::EndsWith
        | StringOp::LastIndexOf => [Coerce::String, Coerce::Number],
        StringOp::Slice | StringOp::Substring => [Coerce::Number, Coerce::Number],
        StringOp::CharAt
        | StringOp::Repeat
        | StringOp::At
        | StringOp::CharCodeAt
        | StringOp::CodePointAt
        // `padStart` / `padEnd` coerce `fillString` in the body, after the
        // early return.
        | StringOp::PadStart
        | StringOp::PadEnd => [Coerce::Number, Coerce::None],
        StringOp::Split => [Coerce::OptString, Coerce::Number],
        StringOp::LocaleCompare => [Coerce::String, Coerce::None],
        StringOp::Normalize | StringOp::Search | StringOp::Match => {
            [Coerce::OptString, Coerce::None]
        }
        StringOp::ToUpperCase
        | StringOp::ToLowerCase
        | StringOp::ToLocaleUpperCase
        | StringOp::ToLocaleLowerCase
        | StringOp::Trim
        | StringOp::TrimStart
        | StringOp::TrimEnd
        | StringOp::Concat
        | StringOp::MatchAll
        | StringOp::Replace
        | StringOp::ReplaceAll
        | StringOp::IsWellFormed
        | StringOp::ToWellFormed => [Coerce::None, Coerce::None],
    }
}

/// The argument slots in coercion order: `split` coerces its `limit`
/// (`ToUint32`) before its separator (`ToString`).
fn coercion_order(op: StringOp) -> [u32; 2] {
    if op == StringOp::Split {
        [1, 0]
    } else {
        [0, 1]
    }
}

/// The host op flag of `op` when the call-site arguments `args` are already
/// of the types its coercions produce (string / number literals; a missing
/// argument is `undefined`), so that on a string receiver the builtin is
/// exactly its host op, inline; with whether that op may refuse its input
/// (`normalize`; then the call site falls back to the builtin, which throws).
pub(in crate::emit) fn inline_host_op(op: StringOp, args: &[IrNode]) -> Option<(i32, bool)> {
    let ready = |coerce: Coerce, arg: Option<&IrNode>| {
        matches!(
            (coerce, arg),
            (Coerce::None, _)
                | (Coerce::OptString | Coerce::Number, None)
                | (Coerce::String | Coerce::OptString, Some(IrNode::Str(_)))
                | (Coerce::Number, Some(IrNode::Num(_)))
        )
    };
    // Case mapping takes its Latin-1 fast path in the builtin.
    if case_upper(op).is_some() {
        return None;
    }
    let [first, second] = coercions(op);
    if !(ready(first, args.first()) && ready(second, args.get(1))) {
        return None;
    }
    // Only `normalize` with a form that is not a literal valid one may refuse.
    let may_fail = op == StringOp::Normalize
        && match args.first() {
            None => false,
            Some(IrNode::Str(form)) => !["NFC", "NFD", "NFKC", "NFKD"]
                .iter()
                .any(|valid| form == valid),
            Some(_) => true,
        };
    op.flag().map(|flag| (flag, may_fail))
}

/// Push i32 `1` when the `__method` result on the stack is the refusal
/// sentinel `METHOD_RANGE_ERROR`.
pub(in crate::emit) fn emit_is_method_refusal(function: &mut Function) {
    wasm!(function; I64Const(abi::METHOD_RANGE_ERROR.cast_signed()), I64Eq);
}

/// The `TypeError` text of `String.prototype.<name>` on a `null`/`undefined`
/// receiver.
macro_rules! nullish {
    ($name:literal) => {
        [
            concat!("String.prototype.", $name, " called on null or undefined"),
            concat!(
                "TypeError: String.prototype.",
                $name,
                " called on null or undefined"
            ),
        ]
    };
}

/// The `null`/`undefined` receiver text of `op`.
fn nullish_text(op: StringOp) -> CalleeErrorText<'static> {
    match op {
        StringOp::IndexOf => nullish!("indexOf"),
        StringOp::Includes => nullish!("includes"),
        StringOp::Slice => nullish!("slice"),
        StringOp::ToUpperCase => nullish!("toUpperCase"),
        StringOp::ToLowerCase => nullish!("toLowerCase"),
        StringOp::ToLocaleUpperCase => nullish!("toLocaleUpperCase"),
        StringOp::ToLocaleLowerCase => nullish!("toLocaleLowerCase"),
        StringOp::CharAt => nullish!("charAt"),
        StringOp::Trim => nullish!("trim"),
        StringOp::TrimStart => nullish!("trimStart"),
        StringOp::TrimEnd => nullish!("trimEnd"),
        StringOp::Repeat => nullish!("repeat"),
        StringOp::StartsWith => nullish!("startsWith"),
        StringOp::EndsWith => nullish!("endsWith"),
        StringOp::PadStart => nullish!("padStart"),
        StringOp::PadEnd => nullish!("padEnd"),
        StringOp::Substring => nullish!("substring"),
        StringOp::Split => nullish!("split"),
        StringOp::At => nullish!("at"),
        StringOp::CharCodeAt => nullish!("charCodeAt"),
        StringOp::CodePointAt => nullish!("codePointAt"),
        StringOp::Concat => nullish!("concat"),
        StringOp::LastIndexOf => nullish!("lastIndexOf"),
        StringOp::LocaleCompare => nullish!("localeCompare"),
        StringOp::Normalize => nullish!("normalize"),
        StringOp::Search => nullish!("search"),
        StringOp::Match => nullish!("match"),
        StringOp::MatchAll => nullish!("matchAll"),
        StringOp::Replace => nullish!("replace"),
        StringOp::ReplaceAll => nullish!("replaceAll"),
        StringOp::IsWellFormed => nullish!("isWellFormed"),
        StringOp::ToWellFormed => nullish!("toWellFormed"),
    }
}

/// Every op of this family.
const OPS: [StringOp; 32] = [
    StringOp::IndexOf,
    StringOp::Includes,
    StringOp::Slice,
    StringOp::ToUpperCase,
    StringOp::ToLowerCase,
    StringOp::ToLocaleUpperCase,
    StringOp::ToLocaleLowerCase,
    StringOp::CharAt,
    StringOp::Trim,
    StringOp::TrimStart,
    StringOp::TrimEnd,
    StringOp::Repeat,
    StringOp::StartsWith,
    StringOp::EndsWith,
    StringOp::PadStart,
    StringOp::PadEnd,
    StringOp::Substring,
    StringOp::Split,
    StringOp::At,
    StringOp::CharCodeAt,
    StringOp::CodePointAt,
    StringOp::Concat,
    StringOp::LastIndexOf,
    StringOp::LocaleCompare,
    StringOp::Normalize,
    StringOp::Search,
    StringOp::Match,
    StringOp::MatchAll,
    StringOp::Replace,
    StringOp::ReplaceAll,
    StringOp::IsWellFormed,
    StringOp::ToWellFormed,
];

/// `thisStringValue` failures.
const NOT_STRING_TO_STRING: CalleeErrorText<'static> = [
    "String.prototype.toString requires that 'this' be a String",
    "TypeError: String.prototype.toString requires that 'this' be a String",
];
const NOT_STRING_VALUE_OF: CalleeErrorText<'static> = [
    "String.prototype.valueOf requires that 'this' be a String",
    "TypeError: String.prototype.valueOf requires that 'this' be a String",
];

/// The `TypeError` of `search` / `match` with a pattern holding `RegExp` syntax.
const SEARCH_REGEXP: CalleeErrorText<'static> = [
    "String.prototype.search does not support RegExp patterns yet",
    "TypeError: String.prototype.search does not support RegExp patterns yet",
];
const MATCH_REGEXP: CalleeErrorText<'static> = [
    "String.prototype.match does not support RegExp patterns yet",
    "TypeError: String.prototype.match does not support RegExp patterns yet",
];

/// The `TypeError` of `matchAll`, whose result is a `RegExp` string iterator.
const MATCH_ALL_REGEXP: CalleeErrorText<'static> = [
    "String.prototype.matchAll needs RegExp, which is not supported yet",
    "TypeError: String.prototype.matchAll needs RegExp, which is not supported yet",
];

/// The `TypeError` of `includes` / `startsWith` / `endsWith` with an
/// `IsRegExp` argument.
const REGEXP_INCLUDES: CalleeErrorText<'static> = [
    "First argument to String.prototype.includes must not be a regular expression",
    "TypeError: First argument to String.prototype.includes must not be a regular expression",
];
const REGEXP_STARTS_WITH: CalleeErrorText<'static> = [
    "First argument to String.prototype.startsWith must not be a regular expression",
    "TypeError: First argument to String.prototype.startsWith must not be a regular expression",
];
const REGEXP_ENDS_WITH: CalleeErrorText<'static> = [
    "First argument to String.prototype.endsWith must not be a regular expression",
    "TypeError: First argument to String.prototype.endsWith must not be a regular expression",
];

/// The `TypeError` of `matchAll` / `replaceAll` with an `IsRegExp` argument
/// whose `flags` lack `g`.
const MATCH_ALL_NON_GLOBAL: CalleeErrorText<'static> = [
    "String.prototype.matchAll called with a non-global RegExp argument",
    "TypeError: String.prototype.matchAll called with a non-global RegExp argument",
];
const REPLACE_ALL_NON_GLOBAL: CalleeErrorText<'static> = [
    "replaceAll must be called with a global RegExp",
    "TypeError: replaceAll must be called with a global RegExp",
];

/// The `TypeError`s of a non-callable `@@match` / `@@matchAll` /
/// `@@replace` / `@@search` / `@@split` method of a string-method argument.
const HOOK_NOT_CALLABLE: [CalleeErrorText<'static>; 5] = [
    [
        "Symbol.match is not a function",
        "TypeError: Symbol.match is not a function",
    ],
    [
        "Symbol.matchAll is not a function",
        "TypeError: Symbol.matchAll is not a function",
    ],
    [
        "Symbol.replace is not a function",
        "TypeError: Symbol.replace is not a function",
    ],
    [
        "Symbol.search is not a function",
        "TypeError: Symbol.search is not a function",
    ],
    [
        "Symbol.split is not a function",
        "TypeError: Symbol.split is not a function",
    ],
];

/// The key `matchAll` / `replaceAll` read off an `IsRegExp` argument, and
/// the flag it must hold.
const FLAGS_KEY: &str = "flags";
const GLOBAL_FLAG: &str = "g";

/// The `RangeError` text of `normalize` with an unknown form.
const NORMALIZE_FORM: [&str; 2] = [
    "The normalization form should be one of NFC, NFD, NFKC, NFKD.",
    "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD.",
];

/// The `RangeError` text prefixes of `fromCodePoint` with a value that is not
/// a code point (the value follows).
const INVALID_CODE_POINT: [&str; 2] = ["Invalid code point ", "RangeError: Invalid code point "];

/// The `TypeError` of `String.raw` on a `null`/`undefined` template or `raw`.
const NULLISH_TO_OBJECT: CalleeErrorText<'static> = [
    "Cannot convert undefined or null to object",
    "TypeError: Cannot convert undefined or null to object",
];

/// The `RangeError` text prefixes of `repeat` with a negative or infinite
/// count (the count follows).
const REPEAT_COUNT: [&str; 2] = ["Invalid count value: ", "RangeError: Invalid count value: "];

/// The `RangeError` of a `repeat` / `padStart` / `padEnd` result past
/// [`MAX_STRING_LENGTH`].
const STRING_LENGTH: [&str; 2] = ["Invalid string length", "RangeError: Invalid string length"];

/// The longest string a `repeat` / `padStart` / `padEnd` builds, in code
/// units (`2^29 - 24`, as V8).
const MAX_STRING_LENGTH: f64 = 536_870_888.0;

/// The own keys of a `match` result besides its element.
const MATCH_KEYS: [&str; 3] = ["index", "input", "groups"];

/// `StrWhiteSpaceChar` (`WhiteSpace` ∪ `LineTerminator`) as inclusive
/// code-unit ranges: the set `trim` / `trimStart` / `trimEnd` strip.
const WHITESPACE: [(i32, i32); 10] = [
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];

/// The units of `RegExp` pattern syntax: a `search` / `match` string pattern
/// holding one needs the absent `RegExp`; one without matches its own text.
const REGEXP_SYNTAX: &[u8] = b"\\^$.*+?()[]{}|";

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    OPS.into_iter()
        .flat_map(nullish_text)
        .chain(NOT_STRING_TO_STRING)
        .chain(NOT_STRING_VALUE_OF)
        .chain(SEARCH_REGEXP)
        .chain(MATCH_REGEXP)
        .chain(MATCH_ALL_REGEXP)
        .chain(REGEXP_INCLUDES)
        .chain(REGEXP_STARTS_WITH)
        .chain(REGEXP_ENDS_WITH)
        .chain(MATCH_ALL_NON_GLOBAL)
        .chain(REPLACE_ALL_NON_GLOBAL)
        .chain(HOOK_NOT_CALLABLE.into_iter().flatten())
        .chain([FLAGS_KEY, GLOBAL_FLAG])
        .chain(NORMALIZE_FORM)
        .chain(INVALID_CODE_POINT)
        .chain(NULLISH_TO_OBJECT)
        .chain(REPEAT_COUNT)
        .chain(STRING_LENGTH)
        .chain(MATCH_KEYS)
        .chain(["", " ", "raw"])
}

/// `String.prototype.toString()` / `valueOf()`: `thisStringValue(this)`.
pub(super) fn emit_this_value(
    value_of: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let text = if value_of {
        NOT_STRING_VALUE_OF
    } else {
        NOT_STRING_TO_STRING
    };
    let s = frame.i64(0);
    this_primitive(
        function,
        frame,
        s,
        abi::CLASS_STRING,
        emit_is_string,
        text,
        strings,
        helpers,
    );
    wasm!(function; LocalGet(s));
}

/// The generic `String.prototype.<op>(a0, a1)`: `ToString(this)` (a
/// `null`/`undefined` receiver throws), the argument coercions of
/// [`coercions`] in [`coercion_order`], `repeat`'s count check, then the
/// code-unit body ([`emit_units`]) or the host op, whose refusal sentinels
/// throw; `match` builds its result array. `concat`, `matchAll`, `replace`
/// and `replaceAll` continue in their own bodies after the receiver. Before
/// `ToString(this)`, an object first argument of `match` / `matchAll` /
/// `replace` / `replaceAll` / `search` / `split` with the method's
/// well-known symbol is called instead ([`emit_dispatch`]); after it,
/// `includes` / `startsWith` / `endsWith` refuse an `IsRegExp` argument.
pub(super) fn emit_method(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (s, a0, a1, r) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let count = frame.f64(0);
    if op == StringOp::Concat {
        // The spread argument array, before `ToString(this)` can republish it.
        wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(frame.x64(0)));
    }
    emit_receiver(op, function, frame, strings, helpers);
    match op {
        StringOp::Concat => return emit_concat(function, frame, s, helpers),
        StringOp::MatchAll => {
            return throw_type_error(function, frame, MATCH_ALL_REGEXP, strings, helpers);
        }
        StringOp::Replace | StringOp::ReplaceAll => {
            return emit_replace(op, function, frame, strings, helpers);
        }
        _ => {}
    }
    let [first, second] = coercions(op);
    for i in coercion_order(op) {
        let (coerce, out) = if i == 0 { (first, a0) } else { (second, a1) };
        frame.arg(function, i);
        wasm!(function; LocalSet(out));
        // Each coercion is skipped for a value already of its type.
        let coerce_string = |function: &mut Function| {
            is_string(function, out);
            wasm!(function; I32Eqz, If(BlockType::Empty));
            to_string(function, out, helpers);
            wasm!(function; End);
        };
        match coerce {
            Coerce::None => {}
            Coerce::String => coerce_string(function),
            Coerce::OptString | Coerce::Number => {
                wasm!(function;
                    LocalGet(out), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty),
                );
                if coerce == Coerce::Number {
                    emit_is_number(function, out);
                    wasm!(function;
                        I32Eqz, If(BlockType::Empty),
                        LocalGet(out), Call(helpers.to_number), LocalSet(out),
                    );
                    return_if_pending(function);
                    wasm!(function; End);
                } else {
                    coerce_string(function);
                }
                wasm!(function; End);
            }
        }
    }
    if op == StringOp::Repeat {
        // `ToIntegerOrInfinity(count)` below 0 or `+Infinity` is a RangeError.
        wasm!(function;
            LocalGet(a0), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty),
            LocalGet(a0), F64ReinterpretI64, F64Trunc, LocalTee(count),
            F64Const(Ieee64::from(0.0)), F64Lt,
            LocalGet(count), F64Const(Ieee64::from(f64::INFINITY)), F64Eq, I32Or,
            If(BlockType::Empty),
        );
        throw_range_error(function, frame, REPEAT_COUNT, Some(a0), strings, helpers);
        // A result past the longest string is a RangeError too.
        wasm!(function;
            End,
            LocalGet(s), I32WrapI64,
            I32Load(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }),
            F64ConvertI32U, LocalGet(count), F64Mul,
            F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt,
            If(BlockType::Empty),
        );
        throw_range_error(function, frame, STRING_LENGTH, None, strings, helpers);
        wasm!(function; End, End);
    }
    if matches!(op, StringOp::Search | StringOp::Match) {
        // `RegExpCreate(undefined)` is the empty pattern.
        wasm!(function; LocalGet(a0), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
        push_str(function, strings, "");
        wasm!(function; LocalSet(a0), End);
    }
    let Some(flag) = op.flag() else {
        emit_units(op, function, frame, strings, helpers);
        if op == StringOp::Match {
            wasm!(function; LocalSet(r));
            emit_match_result(function, frame, s, a0, r, strings, helpers);
        }
        return;
    };
    if let Some(upper) = case_upper(op) {
        emit_case_map(upper, op, flag, function, frame, strings, helpers);
        return;
    }
    emit_host_op(op, flag, function, frame, strings, helpers);
}

/// The receiver of [`emit_method`] into i64 temp 0: `this`, a
/// `null`/`undefined` one throws; the well-known symbol dispatch
/// ([`emit_dispatch`]); `ToString(this)`; then the `IsRegExp` refusal of
/// `includes` / `startsWith` / `endsWith`.
fn emit_receiver(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let s = frame.i64(0);
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(s));
    emit_is_nullish(function, s);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, nullish_text(op), strings, helpers);
    wasm!(function; End);
    if let Some(symbol) = dispatch_of(op) {
        emit_dispatch(op, symbol, function, frame, strings, helpers);
    }
    is_string(function, s);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    to_string(function, s, helpers);
    wasm!(function; End);
    if let Some(text) = regexp_refusal(op) {
        emit_refuse_regexp(text, function, frame, strings, helpers);
    }
}

/// Whether `op` is an upper (`true`) or lower (`false`) case mapping.
fn case_upper(op: StringOp) -> Option<bool> {
    match op {
        StringOp::ToUpperCase | StringOp::ToLocaleUpperCase => Some(true),
        StringOp::ToLowerCase | StringOp::ToLocaleLowerCase => Some(false),
        _ => None,
    }
}

/// `toUpperCase` / `toLowerCase` (and the `toLocale*` twins) over the string
/// in i64 temp 0: a one-byte receiver whose mapping stays one-byte at the
/// same length (lowercasing always; uppercasing unless it holds `µ`, `ß` or
/// `ÿ`) maps per unit in the guest, anything else runs the host op `flag`.
fn emit_case_map(
    upper: bool,
    op: StringOp,
    flag: i32,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [i, c, ok, dst] = [locals[3], locals[4], locals[5], locals[6]];
    s.open(function, frame.i64(0));
    wasm!(function; LocalGet(s.w), I32Eqz, LocalSet(ok));
    if upper {
        wasm!(function; LocalGet(ok), If(BlockType::Empty), I32Const(0), LocalSet(i));
        count_loop(function, i, s.n, |function| {
            s.unit_at(function, i);
            wasm!(function;
                LocalTee(c), I32Const(0xB5), I32Eq,
                LocalGet(c), I32Const(0xDF), I32Eq, I32Or,
                LocalGet(c), I32Const(0xFF), I32Eq, I32Or,
                If(BlockType::Empty), I32Const(0), LocalSet(ok), Br(2), End,
            );
        });
        wasm!(function; End);
    }
    // A mapped unit and its case pair are `0x20` apart: `A-Z` / `a-z` and
    // `U+00C0..=U+00DE` / `U+00E0..=U+00FE` but `×` / `÷`.
    let (letters, accented, sign) = if upper {
        (0x61, 0xE0, 0xF7)
    } else {
        (0x41, 0xC0, 0xD7)
    };
    let d = Str { p: dst, ..s };
    wasm!(function; LocalGet(ok), If(BlockType::Result(ValType::I64)));
    emit_alloc_str(
        function,
        helpers.alloc,
        |f| wasm!(f; LocalGet(s.n)),
        |f| wasm!(f; I32Const(0)),
        dst,
    );
    wasm!(function; I32Const(0), LocalSet(i));
    count_loop(function, i, s.n, |function| {
        d.store(
            function,
            |f| wasm!(f; LocalGet(i)),
            |f| {
                s.unit_at(f, i);
                wasm!(f;
                    LocalTee(c), LocalGet(c), I32Const(letters), I32Sub, I32Const(26), I32LtU,
                    LocalGet(c), I32Const(accented), I32Sub, I32Const(0x1F), I32LtU,
                    LocalGet(c), I32Const(sign), I32Ne, I32And, I32Or,
                    I32Const(5), I32Shl,
                );
                if upper {
                    wasm!(f; I32Sub);
                } else {
                    wasm!(f; I32Add);
                }
            },
        );
    });
    wasm!(function; LocalGet(dst));
    box_heap(function);
    wasm!(function; Else);
    emit_host_op(op, flag, function, frame, strings, helpers);
    wasm!(function; End);
}

/// The host op `flag` of `op` over the receiver string in i64 temp 0 and the
/// coerced arguments in i64 temps 1 and 2; `normalize`'s refusal sentinel
/// throws.
fn emit_host_op(
    op: StringOp,
    flag: i32,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (s, a0, a1, r) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    wasm!(function;
        LocalGet(s), I32Const(flag), LocalGet(a0), LocalGet(a1), Call(helpers.method), LocalSet(r),
    );
    if op == StringOp::Normalize {
        wasm!(function;
            LocalGet(r), I64Const(abi::METHOD_RANGE_ERROR.cast_signed()), I64Eq,
            If(BlockType::Empty),
        );
        throw_range_error(function, frame, NORMALIZE_FORM, None, strings, helpers);
        wasm!(function; End);
    }
    wasm!(function; LocalGet(r));
}

/// The well-known symbol `op` dispatches to on an object first argument.
fn dispatch_of(op: StringOp) -> Option<WellKnown> {
    match op {
        StringOp::Match => Some(WellKnown::Match),
        StringOp::MatchAll => Some(WellKnown::MatchAll),
        StringOp::Replace | StringOp::ReplaceAll => Some(WellKnown::Replace),
        StringOp::Search => Some(WellKnown::Search),
        StringOp::Split => Some(WellKnown::Split),
        _ => None,
    }
}

/// The `TypeError` of `includes` / `startsWith` / `endsWith` with an
/// `IsRegExp` argument.
fn regexp_refusal(op: StringOp) -> Option<CalleeErrorText<'static>> {
    match op {
        StringOp::Includes => Some(REGEXP_INCLUDES),
        StringOp::StartsWith => Some(REGEXP_STARTS_WITH),
        StringOp::EndsWith => Some(REGEXP_ENDS_WITH),
        _ => None,
    }
}

/// The `TypeError` of a non-callable `symbol` method (`GetMethod`).
fn not_callable_text(symbol: WellKnown) -> CalleeErrorText<'static> {
    match symbol {
        WellKnown::MatchAll => HOOK_NOT_CALLABLE[1],
        WellKnown::Replace => HOOK_NOT_CALLABLE[2],
        WellKnown::Search => HOOK_NOT_CALLABLE[3],
        WellKnown::Split => HOOK_NOT_CALLABLE[4],
        _ => HOOK_NOT_CALLABLE[0],
    }
}

/// Push i32 `1` when `IsRegExp` of the object in i64 `v`:
/// `ToBoolean(Get(v, @@match))` (no object has `[[RegExpMatcher]]`); the
/// `@@match` value is left in i64 `m`. A throw returns.
fn emit_is_regexp(
    function: &mut Function,
    frame: Frame,
    v: u32,
    m: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(v), I32Const(realm::symbol_addr(WellKnown::Match, helpers)),
        Call(helpers.symbol_get()), LocalSet(m),
    );
    return_if_pending(function);
    let truthy = IrNode::ConvertCall {
        op: ConvertOp::ToBoolean,
        arg: Box::new(IrNode::Local(m)),
        radix: None,
    };
    emit_node(&truthy, function, frame.scratch(), strings, helpers);
    wasm!(function; I64Const(TRUE_BITS), I64Eq);
}

/// `includes` / `startsWith` / `endsWith`: an object first argument that
/// `IsRegExp` throws `text`.
fn emit_refuse_regexp(
    text: CalleeErrorText<'static>,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (a0, m) = (frame.i64(1), frame.i64(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    emit_is_object(function, a0);
    wasm!(function; If(BlockType::Empty));
    emit_is_regexp(function, frame, a0, m, strings, helpers);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, text, strings, helpers);
    wasm!(function; End, End);
}

/// The well-known symbol dispatch of `op` (`symbol` = [`dispatch_of`]) on an
/// object first argument `a0`, with `O` = the receiver in i64 temp 0 (not
/// yet `ToString`ed): `matchAll` / `replaceAll` first require an `IsRegExp`
/// argument's `ToString(flags)` to hold `g`; then `GetMethod(a0, symbol)`
/// (`undefined` / `null` falls through to the string path, a non-callable
/// value throws) is called with `this = a0` and `«O»` (`«O, a1»` for
/// `replace` / `replaceAll` / `split`), whose result is returned. A
/// primitive argument skips the lookup.
fn emit_dispatch(
    op: StringOp,
    symbol: WellKnown,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (s, a0, a1, m) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    emit_is_object(function, a0);
    wasm!(function; If(BlockType::Empty));
    if let Some(text) = match op {
        StringOp::MatchAll => Some(MATCH_ALL_NON_GLOBAL),
        StringOp::ReplaceAll => Some(REPLACE_ALL_NON_GLOBAL),
        _ => None,
    } {
        let flags = frame.x64(1);
        let locals = i32_locals(frame);
        let (fv, g) = (Str::at(&locals, 0), Str::at(&locals, 3));
        let [from, at, j, ok] = [locals[9], locals[10], locals[11], locals[12]];
        emit_is_regexp(function, frame, a0, m, strings, helpers);
        wasm!(function;
            If(BlockType::Empty),
            LocalGet(a0), I32Const(static_key(strings, FLAGS_KEY)), Call(helpers.get),
            LocalSet(flags),
        );
        return_if_pending(function);
        // `RequireObjectCoercible(flags)`, then `ToString(flags)` must hold `g`.
        emit_is_nullish(function, flags);
        wasm!(function; If(BlockType::Empty));
        throw_type_error(function, frame, text, strings, helpers);
        wasm!(function; End);
        is_string(function, flags);
        wasm!(function; I32Eqz, If(BlockType::Empty));
        to_string(function, flags, helpers);
        wasm!(function; End);
        push_str(function, strings, GLOBAL_FLAG);
        wasm!(function; LocalSet(m));
        fv.open(function, flags);
        g.open(function, m);
        wasm!(function; I32Const(0), LocalSet(from));
        emit_index_of(function, fv, g, from, at, [j, ok]);
        wasm!(function; LocalGet(at), I32Const(-1), I32Eq, If(BlockType::Empty));
        throw_type_error(function, frame, text, strings, helpers);
        wasm!(function; End, End);
    }
    wasm!(function;
        LocalGet(a0), I32Const(realm::symbol_addr(symbol, helpers)), Call(helpers.symbol_get()),
        LocalSet(m),
    );
    return_if_pending(function);
    emit_is_nullish(function, m);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_function(function, m);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, not_callable_text(symbol), strings, helpers);
    wasm!(function; End);
    let args: &[u32] = if matches!(
        op,
        StringOp::Replace | StringOp::ReplaceAll | StringOp::Split
    ) {
        frame.arg(function, 1);
        wasm!(function; LocalSet(a1));
        &[s, a1]
    } else {
        &[s]
    };
    call_with(function, frame, m, a0, args, helpers);
    wasm!(function; Return, End, End);
}

/// Call the function in i64 local `callee` with `this` = i64 local `this`
/// and the arguments `args` (i64 locals), leaving its result; arguments past
/// `K` go in a published spread argument array.
fn call_with(
    function: &mut Function,
    frame: Frame,
    callee: u32,
    this: u32,
    args: &[u32],
    helpers: Helpers<'_>,
) {
    for slot in 0..frame.k {
        if let Some(&local) = args.get(slot as usize) {
            wasm!(function; LocalGet(local));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    if args.len() > frame.k as usize {
        let spread = frame.scratch() + 35;
        let n = i32::try_from(args.len()).unwrap_or(i32::MAX);
        wasm!(function;
            I32Const(n), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
            LocalSet(spread),
        );
        for (j, &arg) in (0u64..).zip(args) {
            wasm!(function; LocalGet(spread), I32WrapI64);
            emit_array_elems(function);
            wasm!(function; LocalGet(arg), I64Store(elem_slot(j)));
        }
        wasm!(function; LocalGet(spread), GlobalSet(SPREAD_ARGS_GLOBAL));
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    invoke(
        function,
        callee,
        |function| emit_argc(args.len(), function),
        helpers,
    );
}

/// Throw a `RangeError` with `text` (`[message, stack]`), followed by the
/// jsval in i64 local `value` when given.
fn throw_range_error(
    function: &mut Function,
    frame: Frame,
    text: [&str; 2],
    value: Option<u32>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let part = |prefix: &str| match value {
        Some(local) => IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str(prefix.into())),
            rhs: Box::new(IrNode::Local(local)),
        },
        None => IrNode::Str(prefix.into()),
    };
    let error = IrNode::error_object(ErrorKind::RangeError, Some(part(text[0])), part(text[1]));
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
}

/// The `match` result of the `RegExp`-free pattern in `p` at the index in `r`
/// over the string in `s`: `null` for `-1`, else `[p]` with the own data
/// properties `index`, `input` and `groups` (`undefined`).
fn emit_match_result(
    function: &mut Function,
    frame: Frame,
    s: u32,
    p: u32,
    r: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let result = frame.x64(0);
    wasm!(function;
        LocalGet(r), I64Const((-1.0f64).to_bits().cast_signed()), I64Eq,
        If(BlockType::Empty), I64Const(NULL_BITS), Return, End,
    );
    let array = IrNode::Array(vec![IrNode::Local(p)]);
    emit_node(&array, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(result));
    for (key, value) in MATCH_KEYS.into_iter().zip([r, s]) {
        wasm!(function;
            LocalGet(result), I32WrapI64, I32Const(static_key(strings, key)), LocalGet(value),
            I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
        );
    }
    wasm!(function;
        LocalGet(result), I32WrapI64, I32Const(static_key(strings, MATCH_KEYS[2])),
        I64Const(UNDEFINED_BITS), I32Const(abi::ATTRS_DEFAULT.cast_signed()), Call(helpers.define),
        LocalGet(result),
    );
}

/// Append the string in i64 `v` to the string in i64 `acc`.
fn append(function: &mut Function, acc: u32, v: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(acc), I32WrapI64, LocalGet(v), I32WrapI64, Call(helpers.concat),
        I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(acc),
    );
}

/// `concat(...args)` over the string in `s`: append `ToString` of each
/// argument in order (the spread argument array is captured in `x64(0)`).
fn emit_concat(function: &mut Function, frame: Frame, s: u32, helpers: Helpers<'_>) {
    let (v, spread) = (frame.i64(1), frame.x64(0));
    let (n, i) = (frame.i32(0), frame.i32(1));
    push_argc(function, frame);
    wasm!(function; LocalSet(n), I32Const(0), LocalSet(i));
    count_loop(function, i, n, |function| {
        arg_at(function, frame, i, spread, v);
        to_string(function, v, helpers);
        append(function, s, v, helpers);
    });
    wasm!(function; LocalGet(s));
}

/// The i32 locals the code-unit bodies use: the frame's i32 temps, then the
/// i32 locals of its IR scratch layout ([`Frame::scratch`]), which only an IR
/// fragment clobbers — a thrown error (which returns) or `match`'s result
/// array (built last).
fn i32_locals(frame: Frame) -> [u32; 18] {
    let x = frame.scratch();
    [
        frame.i32(0),
        frame.i32(1),
        frame.i32(2),
        frame.i32(3),
        frame.x32(0),
        frame.x32(1),
        frame.y32(0),
        frame.y32(1),
        frame.y32(2),
        frame.y32(3),
        x + 3,
        x + 7,
        x + 8,
        x + 9,
        x + 10,
        x + 11,
        x + 12,
        x + 21,
    ]
}

/// A one-byte data [`MemArg`].
fn mem8() -> MemArg {
    MemArg {
        offset: u64::from(abi::STR_DATA_OFFSET),
        align: 0,
        memory_index: 0,
    }
}

/// A two-byte data [`MemArg`].
fn mem16() -> MemArg {
    MemArg {
        offset: u64::from(abi::STR_DATA_OFFSET),
        align: 1,
        memory_index: 0,
    }
}

/// The i32 views of a string: its block base `p`, width `w` (`0` one-byte,
/// `1` two-byte) and length `n` in code units.
#[derive(Clone, Copy)]
struct Str {
    p: u32,
    w: u32,
    n: u32,
}

impl Str {
    /// Three consecutive locals of `locals` from `at`.
    fn at(locals: &[u32; 18], at: usize) -> Self {
        Self {
            p: locals[at],
            w: locals[at + 1],
            n: locals[at + 2],
        }
    }

    /// Set the views from the string jsval in i64 `v`.
    fn open(self, function: &mut Function, v: u32) {
        wasm!(function; LocalGet(v), I32WrapI64, LocalSet(self.p));
        emit_str_two_byte(function, self.p);
        wasm!(function; LocalSet(self.w), LocalGet(self.p));
        emit_load_str_len(function);
        wasm!(function; LocalSet(self.n));
    }

    /// Push the code unit at the index `index` pushes.
    fn unit(self, function: &mut Function, index: impl Fn(&mut Function)) {
        wasm!(function; LocalGet(self.w), If(BlockType::Result(ValType::I32)), LocalGet(self.p));
        index(function);
        wasm!(function; I32Const(1), I32Shl, I32Add, I32Load16U(mem16()), Else, LocalGet(self.p));
        index(function);
        wasm!(function; I32Add, I32Load8U(mem8()), End);
    }

    /// Push the code unit at the index in i32 local `i`.
    fn unit_at(self, function: &mut Function, i: u32) {
        emit_str_unit(function, self.p, i, self.w);
    }

    /// Push the address of the unit at the index `index` pushes.
    fn addr(self, function: &mut Function, index: impl Fn(&mut Function)) {
        wasm!(function; LocalGet(self.p));
        index(function);
        wasm!(function;
            LocalGet(self.w), I32Shl, I32Add, I32Const(abi::STR_DATA_OFFSET.cast_signed()), I32Add,
        );
    }

    /// Store the unit `unit` pushes at the index `index` pushes.
    fn store(
        self,
        function: &mut Function,
        index: impl Fn(&mut Function),
        unit: impl Fn(&mut Function),
    ) {
        wasm!(function; LocalGet(self.w), If(BlockType::Empty), LocalGet(self.p));
        index(function);
        wasm!(function; I32Const(1), I32Shl, I32Add);
        unit(function);
        wasm!(function; I32Store16(mem16()), Else, LocalGet(self.p));
        index(function);
        wasm!(function; I32Add);
        unit(function);
        wasm!(function; I32Store8(mem8()), End);
    }
}

/// Box the i32 block base on the stack as a heap jsval.
fn box_heap(function: &mut Function) {
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Push the signed i32 local `x` as a number jsval.
fn push_number(function: &mut Function, x: u32) {
    wasm!(function; LocalGet(x), F64ConvertI32S, I64ReinterpretF64);
}

/// Box the i32 `0` / `1` on the stack as `false` / `true`.
fn box_bool(function: &mut Function) {
    wasm!(function; I64ExtendI32U, I64Const(abi::FALSE.cast_signed()), I64Or);
}

/// i32 `out` = `ToIntegerOrInfinity` of the number jsval in i64 `v`
/// (`undefined` selects what `default` pushes), saturated to the i32 range
/// (`NaN` is `0`).
fn emit_int(function: &mut Function, v: u32, default: impl Fn(&mut Function), out: u32) {
    wasm!(function;
        LocalGet(v), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Result(ValType::I32)),
    );
    default(function);
    wasm!(function; Else, LocalGet(v), F64ReinterpretI64, I32TruncSatF64S, End, LocalSet(out));
}

/// Clamp the signed i32 local `x` to `0..=hi` (an i32 local).
fn emit_clamp(function: &mut Function, x: u32, hi: u32) {
    wasm!(function;
        LocalGet(x), I32Const(0), LocalGet(x), I32Const(0), I32GtS, Select, LocalSet(x),
        LocalGet(x), LocalGet(hi), LocalGet(x), LocalGet(hi), I32LtS, Select, LocalSet(x),
    );
}

/// The relative index in i32 local `x` against the length `n` (i32 local):
/// a negative one counts from the end; clamped to `0..=n`.
fn emit_relative(function: &mut Function, x: u32, n: u32) {
    wasm!(function;
        LocalGet(x), I32Const(0), I32LtS,
        If(BlockType::Empty), LocalGet(x), LocalGet(n), I32Add, LocalSet(x), End,
    );
    emit_clamp(function, x, n);
}

/// Push `ToUint16` / `ToUint32` (`modulus` `2^16` / `2^32`) of the number
/// jsval in i64 `v` as an i32: the truncated value modulo `modulus`, exact
/// for every double (`x` is f64 scratch); `NaN` and the infinities are `0`.
fn emit_to_uint(function: &mut Function, v: u32, modulus: f64, x: u32) {
    wasm!(function;
        LocalGet(v), F64ReinterpretI64, F64Trunc, LocalTee(x),
        LocalGet(x), F64Const(Ieee64::from(modulus)), F64Div, F64Floor,
        F64Const(Ieee64::from(modulus)), F64Mul, F64Sub,
        I32TruncSatF64U,
    );
}

/// Push i32 `1` when the code unit in i32 local `c` is in `ranges`
/// (inclusive).
fn emit_unit_in(function: &mut Function, c: u32, ranges: impl IntoIterator<Item = (i32, i32)>) {
    wasm!(function; I32Const(0));
    for (lo, hi) in ranges {
        if lo == hi {
            wasm!(function; LocalGet(c), I32Const(lo), I32Eq, I32Or);
        } else {
            wasm!(function; LocalGet(c), I32Const(lo), I32Sub, I32Const(hi - lo + 1), I32LtU, I32Or);
        }
    }
}

/// i32 `dst` = a fresh canonical string of the units of `src` from the index
/// `from` pushes, as many as `count` pushes (in range); `[dst, n, i]` are
/// i32 scratch, `n` holding the count.
fn emit_substring(
    function: &mut Function,
    alloc: u32,
    src: Str,
    from: impl Fn(&mut Function),
    count: impl Fn(&mut Function),
    [dst, n, i]: [u32; 3],
) {
    count(function);
    wasm!(function; LocalSet(n));
    emit_alloc_str(
        function,
        alloc,
        |f| wasm!(f; LocalGet(n)),
        |f| wasm!(f; LocalGet(src.w)),
        dst,
    );
    wasm!(function; LocalGet(dst), I32Const(abi::STR_DATA_OFFSET.cast_signed()), I32Add);
    src.addr(function, from);
    wasm!(function;
        LocalGet(n), LocalGet(src.w), I32Shl, MemoryCopy { src_mem: 0, dst_mem: 0 },
    );
    emit_str_canonical(function, src.w, dst, i, n);
}

/// [`emit_substring`] appended to the string in i64 `acc` (nothing for an
/// empty range).
fn emit_append_units(
    function: &mut Function,
    helpers: Helpers<'_>,
    acc: u32,
    src: Str,
    from: impl Fn(&mut Function),
    count: impl Fn(&mut Function),
    tmp: [u32; 3],
) {
    count(function);
    wasm!(function; If(BlockType::Empty));
    emit_substring(function, helpers.alloc, src, from, count, tmp);
    wasm!(function; LocalGet(acc), I32WrapI64, LocalGet(tmp[0]), Call(helpers.concat));
    box_heap(function);
    wasm!(function; LocalSet(acc), End);
}

/// Copy the `n` (i32 local) units of `src` to the index `at` pushes of the
/// block `dst`, at least as wide: `memory.copy` at the same width, else a
/// widening unit loop (`i` is i32 scratch).
fn emit_copy_units(
    function: &mut Function,
    dst: Str,
    at: impl Fn(&mut Function),
    src: Str,
    n: u32,
    i: u32,
) {
    wasm!(function; LocalGet(dst.w), LocalGet(src.w), I32Eq, If(BlockType::Empty));
    dst.addr(function, &at);
    src.addr(function, |f| wasm!(f; I32Const(0)));
    wasm!(function;
        LocalGet(n), LocalGet(src.w), I32Shl, MemoryCopy { src_mem: 0, dst_mem: 0 },
        Else, I32Const(0), LocalSet(i),
    );
    count_loop(function, i, n, |function| {
        dst.store(
            function,
            |f| {
                at(f);
                wasm!(f; LocalGet(i), I32Add);
            },
            |f| src.unit_at(f, i),
        );
    });
    wasm!(function; End);
}

/// i32 `ok` = whether the units of `t` occur in `s` at the index in i32
/// local `k` (`k + t.n <= s.n`); `j` is i32 scratch.
fn emit_matches_at(function: &mut Function, s: Str, k: u32, t: Str, j: u32, ok: u32) {
    wasm!(function;
        I32Const(1), LocalSet(ok), I32Const(0), LocalSet(j),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(j), LocalGet(t.n), I32GeU, BrIf(1),
    );
    s.unit(function, |f| wasm!(f; LocalGet(k), LocalGet(j), I32Add));
    t.unit_at(function, j);
    wasm!(function;
        I32Ne, If(BlockType::Empty), I32Const(0), LocalSet(ok), Br(2), End,
        LocalGet(j), I32Const(1), I32Add, LocalSet(j),
        Br(0),
        End, End,
    );
}

/// `StringIndexOf(s, t, from)` into i32 `out`: the first index `>= from`
/// (an i32 local in `0..=s.n`) where `t` occurs, else `-1`; `[j, ok]` are
/// i32 scratch.
fn emit_index_of(function: &mut Function, s: Str, t: Str, from: u32, out: u32, [j, ok]: [u32; 2]) {
    wasm!(function;
        LocalGet(from), LocalSet(out),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(out), LocalGet(t.n), I32Add, LocalGet(s.n), I32GtU,
        If(BlockType::Empty), I32Const(-1), LocalSet(out), Br(2), End,
    );
    emit_matches_at(function, s, out, t, j, ok);
    wasm!(function;
        LocalGet(ok), BrIf(1),
        LocalGet(out), I32Const(1), I32Add, LocalSet(out),
        Br(0),
        End, End,
    );
}

/// The last index `<= from` (an i32 local with `from + t.n <= s.n`) where
/// `t` occurs in `s` into i32 `out`, else `-1`; `[j, ok]` are i32 scratch.
fn emit_last_index_of(
    function: &mut Function,
    s: Str,
    t: Str,
    from: u32,
    out: u32,
    [j, ok]: [u32; 2],
) {
    wasm!(function;
        LocalGet(from), LocalSet(out),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(out), I32Const(0), I32LtS, BrIf(1),
    );
    emit_matches_at(function, s, out, t, j, ok);
    wasm!(function;
        LocalGet(ok), BrIf(1),
        LocalGet(out), I32Const(1), I32Sub, LocalSet(out),
        Br(0),
        End, End,
    );
}

/// Run `lone` at each index `i` (i32 local) of a lone surrogate of the
/// two-byte string `s` — a lead not followed by a trail, or a trail not
/// after a lead (the surrogate rule of DESIGN.md §5.2); `c`
/// is i32 scratch.
fn emit_lone_surrogates(
    function: &mut Function,
    s: Str,
    i: u32,
    c: u32,
    lone: impl FnOnce(&mut Function),
) {
    let next = |f: &mut Function| wasm!(f; LocalGet(i), I32Const(1), I32Add);
    wasm!(function; I32Const(0), LocalSet(i));
    count_loop(function, i, s.n, |function| {
        s.unit_at(function, i);
        wasm!(function;
            LocalTee(c), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq, If(BlockType::Empty),
            LocalGet(c), I32Const(0xDC00), I32LtU,
        );
        next(function);
        wasm!(function;
            LocalGet(s.n), I32LtU, I32And, If(BlockType::Result(ValType::I32)),
        );
        s.unit(function, next);
        wasm!(function;
            I32Const(0xFC00), I32And, I32Const(0xDC00), I32Eq,
            Else, I32Const(0), End,
            // A pair: step over its trail.
            If(BlockType::Empty),
        );
        next(function);
        wasm!(function; LocalSet(i), Else);
        lone(function);
        wasm!(function; End, End);
    });
}

/// The code-unit body of `op` over the receiver string in i64 temp 0 and the
/// coerced arguments in i64 temps 1 and 2, leaving the result jsval (for
/// `match`, the index).
fn emit_units(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    s.open(function, frame.i64(0));
    match op {
        StringOp::CharAt | StringOp::At | StringOp::CharCodeAt | StringOp::CodePointAt => {
            emit_unit_at(op, function, frame, strings, helpers);
        }
        StringOp::Slice | StringOp::Substring => emit_slice(op, function, frame, helpers),
        StringOp::Trim | StringOp::TrimStart | StringOp::TrimEnd => {
            emit_trim(op, function, frame, helpers);
        }
        StringOp::Repeat => emit_repeat(function, frame, strings, helpers),
        StringOp::PadStart | StringOp::PadEnd => emit_pad(op, function, frame, strings, helpers),
        StringOp::IndexOf
        | StringOp::Includes
        | StringOp::StartsWith
        | StringOp::EndsWith
        | StringOp::LastIndexOf
        | StringOp::Search
        | StringOp::Match => emit_search(op, function, frame, strings, helpers),
        StringOp::Split => emit_split(function, frame, helpers),
        StringOp::IsWellFormed | StringOp::ToWellFormed => {
            emit_well_formed(op, function, frame, helpers);
        }
        _ => {}
    }
}

/// `charAt` / `at` / `charCodeAt` / `codePointAt(pos)`: the one-unit
/// string, unit or code point at `ToIntegerOrInfinity(pos)` (`at` counts a
/// negative index from the end); out of range `""` / `undefined` / `NaN` /
/// `undefined`.
fn emit_unit_at(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [pos, unit, next, trail] = [locals[3], locals[4], locals[5], locals[6]];
    emit_int(function, frame.i64(1), |f| wasm!(f; I32Const(0)), pos);
    if op == StringOp::At {
        wasm!(function;
            LocalGet(pos), I32Const(0), I32LtS,
            If(BlockType::Empty), LocalGet(pos), LocalGet(s.n), I32Add, LocalSet(pos), End,
        );
    }
    wasm!(function; LocalGet(pos), LocalGet(s.n), I32LtU, If(BlockType::Result(ValType::I64)));
    match op {
        StringOp::CharAt | StringOp::At => {
            wasm!(function; LocalGet(s.p), LocalGet(pos), Call(helpers.str_index));
            box_heap(function);
        }
        StringOp::CodePointAt => {
            s.unit_at(function, pos);
            // A lead followed by a trail is a pair; only a two-byte string
            // holds surrogates.
            wasm!(function;
                LocalTee(unit), I32Const(0xFC00), I32And, I32Const(0xD800), I32Eq, LocalGet(s.w), I32And,
                LocalGet(pos), I32Const(1), I32Add, LocalTee(next), LocalGet(s.n), I32LtU, I32And,
                If(BlockType::Empty),
            );
            s.unit_at(function, next);
            wasm!(function;
                LocalTee(trail), I32Const(0xFC00), I32And, I32Const(0xDC00), I32Eq,
                If(BlockType::Empty),
                // (unit - 0xD800) * 0x400 + (trail - 0xDC00) + 0x10000
                LocalGet(unit), I32Const(10), I32Shl, LocalGet(trail), I32Add,
                I32Const(0x035F_DC00), I32Sub, LocalSet(unit),
                End, End,
                LocalGet(unit), F64ConvertI32U, I64ReinterpretF64,
            );
        }
        _ => {
            s.unit_at(function, pos);
            wasm!(function; F64ConvertI32U, I64ReinterpretF64);
        }
    }
    wasm!(function; Else);
    match op {
        StringOp::CharAt => push_str(function, strings, ""),
        StringOp::CharCodeAt => wasm!(function; I64Const(abi::QNAN.cast_signed())),
        _ => wasm!(function; I64Const(UNDEFINED_BITS)),
    }
    wasm!(function; End);
}

/// `slice(start, end)` (relative indices) / `substring(start, end)`
/// (clamped, swapped when reversed): the units between, `end` defaulting to
/// the length.
fn emit_slice(op: StringOp, function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [a, b, lo] = [locals[3], locals[4], locals[5]];
    let tmp = [locals[6], locals[7], locals[8]];
    emit_int(function, frame.i64(1), |f| wasm!(f; I32Const(0)), a);
    emit_int(function, frame.i64(2), |f| wasm!(f; LocalGet(s.n)), b);
    if op == StringOp::Slice {
        emit_relative(function, a, s.n);
        emit_relative(function, b, s.n);
        wasm!(function; LocalGet(a), LocalSet(lo));
        // count = max(end - start, 0)
        wasm!(function;
            LocalGet(b), LocalGet(a), I32Sub, LocalTee(b), I32Const(0), LocalGet(b), I32Const(0),
            I32GtS, Select, LocalSet(b),
        );
    } else {
        emit_clamp(function, a, s.n);
        emit_clamp(function, b, s.n);
        // lo = min(a, b), count = max(a, b) - lo
        wasm!(function;
            LocalGet(a), LocalGet(b), LocalGet(a), LocalGet(b), I32LtS, Select, LocalSet(lo),
            LocalGet(a), LocalGet(b), I32Add, LocalGet(lo), I32Const(1), I32Shl, I32Sub, LocalSet(b),
        );
    }
    emit_substring(
        function,
        helpers.alloc,
        s,
        |f| wasm!(f; LocalGet(lo)),
        |f| wasm!(f; LocalGet(b)),
        tmp,
    );
    wasm!(function; LocalGet(tmp[0]));
    box_heap(function);
}

/// `trim` / `trimStart` / `trimEnd()`: the units between the leading and/or
/// trailing [`WHITESPACE`]; the receiver itself when nothing is stripped.
fn emit_trim(op: StringOp, function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [start, end, unit, last] = [locals[3], locals[4], locals[5], locals[6]];
    let tmp = [locals[7], locals[8], locals[9]];
    wasm!(function; I32Const(0), LocalSet(start), LocalGet(s.n), LocalSet(end));
    if op != StringOp::TrimEnd {
        wasm!(function;
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(start), LocalGet(s.n), I32GeU, BrIf(1),
        );
        s.unit_at(function, start);
        wasm!(function; LocalSet(unit));
        emit_unit_in(function, unit, WHITESPACE);
        wasm!(function;
            I32Eqz, BrIf(1),
            LocalGet(start), I32Const(1), I32Add, LocalSet(start),
            Br(0),
            End, End,
        );
    }
    if op != StringOp::TrimStart {
        wasm!(function;
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(end), LocalGet(start), I32LeU, BrIf(1),
            LocalGet(end), I32Const(1), I32Sub, LocalSet(last),
        );
        s.unit_at(function, last);
        wasm!(function; LocalSet(unit));
        emit_unit_in(function, unit, WHITESPACE);
        wasm!(function;
            I32Eqz, BrIf(1),
            LocalGet(last), LocalSet(end),
            Br(0),
            End, End,
        );
    }
    wasm!(function;
        LocalGet(end), LocalGet(start), I32Sub, LocalTee(end), LocalGet(s.n), I32Eq,
        If(BlockType::Result(ValType::I64)), LocalGet(frame.i64(0)), Else,
    );
    emit_substring(
        function,
        helpers.alloc,
        s,
        |f| wasm!(f; LocalGet(start)),
        |f| wasm!(f; LocalGet(end)),
        tmp,
    );
    wasm!(function; LocalGet(tmp[0]));
    box_heap(function);
    wasm!(function; End);
}

/// `repeat(count)` past its count checks: `""` for an empty receiver or a
/// zero count, else the receiver once, then the result so far copied onto
/// its end until it is `count` times as long.
fn emit_repeat(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [count, total, dst, done] = [locals[3], locals[4], locals[5], locals[6]];
    let d = Str {
        p: dst,
        w: s.w,
        n: total,
    };
    emit_int(function, frame.i64(1), |f| wasm!(f; I32Const(0)), count);
    wasm!(function;
        LocalGet(s.n), I32Eqz, LocalGet(count), I32Eqz, I32Or,
        If(BlockType::Result(ValType::I64)),
    );
    push_str(function, strings, "");
    wasm!(function; Else, LocalGet(s.n), LocalGet(count), I32Mul, LocalSet(total));
    emit_alloc_str(
        function,
        helpers.alloc,
        |f| wasm!(f; LocalGet(total)),
        |f| wasm!(f; LocalGet(s.w)),
        dst,
    );
    emit_copy_units(function, d, |f| wasm!(f; I32Const(0)), s, s.n, done);
    wasm!(function;
        LocalGet(s.n), LocalSet(done),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(done), LocalGet(total), I32GeU, BrIf(1),
    );
    d.addr(function, |f| wasm!(f; LocalGet(done)));
    d.addr(function, |f| wasm!(f; I32Const(0)));
    // chunk = min(done, total - done)
    wasm!(function;
        LocalGet(total), LocalGet(done), I32Sub, LocalTee(count), LocalGet(done),
        LocalGet(count), LocalGet(done), I32LtU, Select, LocalTee(count),
        LocalGet(s.w), I32Shl, MemoryCopy { src_mem: 0, dst_mem: 0 },
        LocalGet(done), LocalGet(count), I32Add, LocalSet(done),
        Br(0),
        End, End,
        LocalGet(dst),
    );
    box_heap(function);
    wasm!(function; End);
}

/// `padStart` / `padEnd(maxLength, fillString)` (`StringPad` order): the
/// receiver when `ToLength(maxLength)` does not exceed its length, before
/// `fillString` is coerced, or when the filler (default `" "`) is empty; a length past [`MAX_STRING_LENGTH`] is a `RangeError`;
/// else the filler repeated (and cut) to the missing units before / after
/// the receiver, narrowed.
fn emit_pad(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let t = Str::at(&locals, 3);
    let [total, width, dst, fill, idx, spare] = [
        locals[6], locals[7], locals[8], locals[9], locals[10], locals[11],
    ];
    let (recv, a0, a1, max) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.f64(0));
    let d = Str {
        p: dst,
        w: width,
        n: total,
    };
    wasm!(function;
        LocalGet(a0), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Result(ValType::F64)),
        F64Const(Ieee64::from(0.0)), Else, LocalGet(a0), F64ReinterpretI64, F64Trunc, End,
        LocalTee(max), LocalGet(s.n), F64ConvertI32U, F64Gt, I32Eqz,
        If(BlockType::Empty), LocalGet(recv), Return, End,
        LocalGet(a1), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty),
    );
    push_str(function, strings, " ");
    wasm!(function; LocalSet(a1), Else);
    is_string(function, a1);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    to_string(function, a1, helpers);
    wasm!(function; End, End);
    t.open(function, a1);
    wasm!(function;
        LocalGet(t.n), I32Eqz, If(BlockType::Empty), LocalGet(recv), Return, End,
        LocalGet(max), F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt, If(BlockType::Empty),
    );
    throw_range_error(function, frame, STRING_LENGTH, None, strings, helpers);
    wasm!(function;
        End,
        LocalGet(max), I32TruncSatF64U, LocalTee(total), LocalGet(s.n), I32Sub, LocalSet(fill),
        LocalGet(s.w), LocalGet(t.w), I32Or, LocalSet(width),
    );
    emit_alloc_str(
        function,
        helpers.alloc,
        |f| wasm!(f; LocalGet(total)),
        |f| wasm!(f; LocalGet(width)),
        dst,
    );
    let (fill_at, recv_at) = if op == StringOp::PadStart {
        (None, Some(fill))
    } else {
        (Some(s.n), None)
    };
    let at = |f: &mut Function, local: Option<u32>| {
        f.instruction(&local.map_or(Instruction::I32Const(0), Instruction::LocalGet));
    };
    wasm!(function; I32Const(0), LocalSet(idx));
    count_loop(function, idx, fill, |function| {
        d.store(
            function,
            |f| {
                at(f, fill_at);
                wasm!(f; LocalGet(idx), I32Add);
            },
            |f| t.unit(f, |f| wasm!(f; LocalGet(idx), LocalGet(t.n), I32RemU)),
        );
    });
    emit_copy_units(function, d, |f| at(f, recv_at), s, s.n, idx);
    emit_str_canonical(function, width, dst, idx, spare);
    wasm!(function; LocalGet(dst));
    box_heap(function);
}

/// The searches over the string argument in i64 temp 1 (a number position
/// or `undefined` in i64 temp 2): `indexOf` / `includes` from the clamped
/// position (default `0`); `startsWith` at it; `endsWith` ending at it
/// (default the length); `lastIndexOf` at or before it (`NaN` and the
/// default: the end); `search` / `match` from `0` after the `RegExp`-syntax
/// check.
fn emit_search(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let t = Str::at(&locals, 3);
    let [k, out, c] = [locals[6], locals[7], locals[8]];
    let scratch = [locals[9], locals[10]];
    let a1 = frame.i64(2);
    t.open(function, frame.i64(1));
    match op {
        StringOp::Search | StringOp::Match => {
            let text = if op == StringOp::Search {
                SEARCH_REGEXP
            } else {
                MATCH_REGEXP
            };
            wasm!(function; I32Const(0), LocalSet(k));
            count_loop(function, k, t.n, |function| {
                t.unit_at(function, k);
                wasm!(function; LocalSet(c));
                emit_unit_in(
                    function,
                    c,
                    REGEXP_SYNTAX.iter().map(|&b| (b.into(), b.into())),
                );
                wasm!(function; If(BlockType::Empty));
                throw_type_error(function, frame, text, strings, helpers);
                wasm!(function; End);
            });
            wasm!(function; I32Const(0), LocalSet(k));
        }
        StringOp::EndsWith => {
            emit_int(function, a1, |f| wasm!(f; LocalGet(s.n)), k);
            emit_clamp(function, k, s.n);
        }
        StringOp::LastIndexOf => {
            wasm!(function;
                LocalGet(a1), I64Const(abi::QNAN.cast_signed()), I64Eq,
                LocalGet(a1), I64Const(UNDEFINED_BITS), I64Eq, I32Or,
                If(BlockType::Result(ValType::I32)), LocalGet(s.n),
                Else, LocalGet(a1), F64ReinterpretI64, I32TruncSatF64S, End, LocalSet(k),
            );
        }
        _ => {
            emit_int(function, a1, |f| wasm!(f; I32Const(0)), k);
            emit_clamp(function, k, s.n);
        }
    }
    match op {
        StringOp::StartsWith => {
            wasm!(function;
                LocalGet(k), LocalGet(t.n), I32Add, LocalGet(s.n), I32GtU,
                If(BlockType::Result(ValType::I64)), I64Const(abi::FALSE.cast_signed()), Else,
            );
            emit_matches_at(function, s, k, t, scratch[0], scratch[1]);
            wasm!(function; LocalGet(scratch[1]));
            box_bool(function);
            wasm!(function; End);
        }
        StringOp::EndsWith => {
            wasm!(function;
                LocalGet(k), LocalGet(t.n), I32Sub, LocalTee(k), I32Const(0), I32LtS,
                If(BlockType::Result(ValType::I64)), I64Const(abi::FALSE.cast_signed()), Else,
            );
            emit_matches_at(function, s, k, t, scratch[0], scratch[1]);
            wasm!(function; LocalGet(scratch[1]));
            box_bool(function);
            wasm!(function; End);
        }
        StringOp::LastIndexOf => {
            // The last index a match fits at: `s.n - t.n` (none when negative).
            wasm!(function;
                LocalGet(s.n), LocalGet(t.n), I32Sub, LocalTee(c), I32Const(0), I32LtS,
                If(BlockType::Result(ValType::I64)),
                I64Const((-1.0f64).to_bits().cast_signed()), Else,
            );
            emit_clamp(function, k, c);
            emit_last_index_of(function, s, t, k, out, scratch);
            push_number(function, out);
            wasm!(function; End);
        }
        _ => {
            emit_index_of(function, s, t, k, out, scratch);
            if op == StringOp::Includes {
                wasm!(function; LocalGet(out), I32Const(-1), I32Ne);
                box_bool(function);
            } else {
                push_number(function, out);
            }
        }
    }
}

/// `split(separator, limit)`: at most `ToUint32(limit)` (default `2^32 - 1`)
/// pieces — none for `0`; the receiver for an `undefined` separator; each
/// unit for `""`; else the units between the separator's occurrences.
fn emit_split(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let t = Str::at(&locals, 3);
    let [lim, a, pos, cnt] = [locals[6], locals[7], locals[8], locals[9]];
    let scratch = [locals[10], locals[11]];
    let tmp = [locals[12], locals[13], locals[14]];
    let (recv, sep, limit, list) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let push = |function: &mut Function, value: &dyn Fn(&mut Function)| {
        wasm!(function; LocalGet(list), I32WrapI64);
        value(function);
        wasm!(function; Call(helpers.array_push), Drop);
    };
    let piece = |function: &mut Function| {
        push(function, &|f| {
            wasm!(f; LocalGet(tmp[0]));
            box_heap(f);
        });
    };
    wasm!(function;
        LocalGet(limit), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Result(ValType::I32)),
        I32Const(-1), Else,
    );
    emit_to_uint(function, limit, 4_294_967_296.0, frame.f64(0));
    wasm!(function;
        End, LocalSet(lim),
        I32Const(0), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        LocalSet(list),
        LocalGet(lim), I32Eqz, If(BlockType::Empty), LocalGet(list), Return, End,
        LocalGet(sep), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty),
    );
    push(function, &|f| wasm!(f; LocalGet(recv)));
    wasm!(function; LocalGet(list), Return, End);
    t.open(function, sep);
    wasm!(function; LocalGet(t.n), I32Eqz, If(BlockType::Empty));
    // One piece per unit, up to the limit.
    wasm!(function;
        LocalGet(s.n), LocalGet(lim), LocalGet(s.n), LocalGet(lim), I32LtU, Select, LocalSet(cnt),
        I32Const(0), LocalSet(a),
    );
    count_loop(function, a, cnt, |function| {
        push(function, &|f| {
            wasm!(f; LocalGet(s.p), LocalGet(a), Call(helpers.str_index));
            box_heap(f);
        });
    });
    wasm!(function;
        LocalGet(list), Return, End,
        I32Const(0), LocalSet(a), I32Const(0), LocalSet(cnt),
    );
    emit_index_of(function, s, t, a, pos, scratch);
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pos), I32Const(-1), I32Eq, BrIf(1),
    );
    emit_substring(
        function,
        helpers.alloc,
        s,
        |f| wasm!(f; LocalGet(a)),
        |f| wasm!(f; LocalGet(pos), LocalGet(a), I32Sub),
        tmp,
    );
    piece(function);
    wasm!(function;
        LocalGet(cnt), I32Const(1), I32Add, LocalTee(cnt), LocalGet(lim), I32Eq,
        If(BlockType::Empty), LocalGet(list), Return, End,
        LocalGet(pos), LocalGet(t.n), I32Add, LocalSet(a),
    );
    emit_index_of(function, s, t, a, pos, scratch);
    wasm!(function; Br(0), End, End);
    emit_substring(
        function,
        helpers.alloc,
        s,
        |f| wasm!(f; LocalGet(a)),
        |f| wasm!(f; LocalGet(s.n), LocalGet(a), I32Sub),
        tmp,
    );
    piece(function);
    wasm!(function; LocalGet(list));
}

/// `isWellFormed()`: whether the receiver holds no lone surrogate;
/// `toWellFormed()`: a copy with each lone surrogate replaced by U+FFFD (the
/// receiver itself when one-byte, which holds no surrogate).
fn emit_well_formed(op: StringOp, function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let locals = i32_locals(frame);
    let s = Str::at(&locals, 0);
    let [i, c, ok, dst] = [locals[3], locals[4], locals[5], locals[6]];
    wasm!(function; LocalGet(s.w), I32Eqz, If(BlockType::Result(ValType::I64)));
    if op == StringOp::IsWellFormed {
        wasm!(function; I64Const(abi::TRUE.cast_signed()), Else, I32Const(1), LocalSet(ok));
        emit_lone_surrogates(function, s, i, c, |f| wasm!(f; I32Const(0), LocalSet(ok)));
        wasm!(function; LocalGet(ok));
        box_bool(function);
    } else {
        let d = Str { p: dst, ..s };
        wasm!(function; LocalGet(frame.i64(0)), Else);
        emit_alloc_str(
            function,
            helpers.alloc,
            |f| wasm!(f; LocalGet(s.n)),
            |f| wasm!(f; I32Const(1)),
            dst,
        );
        emit_copy_units(function, d, |f| wasm!(f; I32Const(0)), s, s.n, i);
        emit_lone_surrogates(function, d, i, c, |f| {
            d.store(f, |f| wasm!(f; LocalGet(i)), |f| wasm!(f; I32Const(0xFFFD)));
        });
        wasm!(function; LocalGet(dst));
        box_heap(function);
    }
    wasm!(function; End);
}

/// `replace` / `replaceAll(searchValue, replaceValue)` over the string in
/// i64 temp 0 with a string pattern (`ToString(searchValue)`): each match
/// (the first, or every one, the next searched `max(1, len)` past the last)
/// is replaced by `ToString` of a callable `replaceValue` called with
/// `(match, position, string)`, else by the `ToString`ed `replaceValue` with
/// its `GetSubstitution` patterns expanded ([`emit_substitution`]).
fn emit_replace(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (s, search, replacer, r) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (acc, v) = (frame.x64(0), frame.x64(1));
    let locals = i32_locals(frame);
    let (sv, t) = (Str::at(&locals, 0), Str::at(&locals, 3));
    let [pos, end, from] = [locals[9], locals[10], locals[11]];
    let tmp = [locals[15], locals[16], locals[17]];
    frame.arg(function, 0);
    wasm!(function; LocalSet(search));
    to_string(function, search, helpers);
    frame.arg(function, 1);
    wasm!(function; LocalSet(replacer));
    emit_is_function(function, replacer);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    to_string(function, replacer, helpers);
    wasm!(function; End);
    sv.open(function, s);
    t.open(function, search);
    wasm!(function; I32Const(0), LocalSet(end), I32Const(0), LocalSet(from));
    push_str(function, strings, "");
    wasm!(function; LocalSet(acc));
    emit_index_of(function, sv, t, from, pos, [tmp[0], tmp[1]]);
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(pos), I32Const(-1), I32Eq, BrIf(1),
    );
    emit_append_units(
        function,
        helpers,
        acc,
        sv,
        |f| wasm!(f; LocalGet(end)),
        |f| wasm!(f; LocalGet(pos), LocalGet(end), I32Sub),
        tmp,
    );
    wasm!(function; LocalGet(pos), LocalGet(t.n), I32Add, LocalSet(end));
    emit_is_function(function, replacer);
    wasm!(function; If(BlockType::Empty));
    // The replacer over `(match, position, string)` with `this = undefined`;
    // its `ToString` result is appended.
    push_number(function, pos);
    wasm!(function; LocalSet(r));
    let args = [search, r, s];
    for slot in 0..frame.k {
        if let Some(&local) = args.get(slot as usize) {
            wasm!(function; LocalGet(local));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    wasm!(function; I64Const(UNDEFINED_BITS), GlobalSet(THIS_GLOBAL));
    let count = args.len().min(frame.k as usize);
    invoke(
        function,
        replacer,
        |function| emit_argc(count, function),
        helpers,
    );
    wasm!(function; LocalSet(v));
    return_if_pending(function);
    to_string(function, v, helpers);
    append(function, acc, v, helpers);
    wasm!(function; Else);
    emit_substitution(function, frame, helpers);
    wasm!(function; End);
    if op == StringOp::ReplaceAll {
        wasm!(function;
            LocalGet(pos), LocalGet(t.n), I32Const(1), LocalGet(t.n), I32Const(1), I32GtU, Select,
            I32Add, LocalTee(from), LocalGet(sv.n), I32GtU, BrIf(1),
        );
        emit_index_of(function, sv, t, from, pos, [tmp[0], tmp[1]]);
        wasm!(function; Br(0));
    }
    wasm!(function; End, End);
    emit_append_units(
        function,
        helpers,
        acc,
        sv,
        |f| wasm!(f; LocalGet(end)),
        |f| wasm!(f; LocalGet(sv.n), LocalGet(end), I32Sub),
        tmp,
    );
    wasm!(function; LocalGet(acc));
}

/// Append `GetSubstitution` of the template string in i64 temp 2 for the
/// match `[pos, end)` of [`emit_replace`] to its result: `$$` is `$`, `$&`
/// the match, `` $` `` the units before it, `$'` those after it; every other
/// `$` (a capture reference, with none captured, or `$<`) stays literal.
fn emit_substitution(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let acc = frame.x64(0);
    let locals = i32_locals(frame);
    let (sv, t, tm) = (
        Str::at(&locals, 0),
        Str::at(&locals, 3),
        Str::at(&locals, 6),
    );
    let [pos, end, k, lit, c] = [locals[9], locals[10], locals[11], locals[12], locals[13]];
    let tmp = [locals[15], locals[16], locals[17]];
    let append_units = |function: &mut Function,
                        src: Str,
                        from: &dyn Fn(&mut Function),
                        count: &dyn Fn(&mut Function)| {
        emit_append_units(function, helpers, acc, src, from, count, tmp);
    };
    let when = |function: &mut Function, unit: u8| {
        wasm!(function; LocalGet(c), I32Const(unit.into()), I32Eq, If(BlockType::Empty));
    };
    tm.open(function, frame.i64(2));
    wasm!(function;
        I32Const(0), LocalSet(k), I32Const(0), LocalSet(lit),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(k), I32Const(1), I32Add, LocalGet(tm.n), I32GeU, BrIf(1),
    );
    // c = the unit after a `$` at k, else 0.
    tm.unit_at(function, k);
    wasm!(function; I32Const(i32::from(b'$')), I32Eq, If(BlockType::Result(ValType::I32)));
    tm.unit(function, |f| wasm!(f; LocalGet(k), I32Const(1), I32Add));
    wasm!(function; Else, I32Const(0), End, LocalSet(c));
    emit_unit_in(function, c, b"$&`'".iter().map(|&b| (b.into(), b.into())));
    wasm!(function; If(BlockType::Empty));
    // The literal units before the pattern (with the `$` of `$$`).
    append_units(function, tm, &|f| wasm!(f; LocalGet(lit)), &|f| {
        wasm!(f;
            LocalGet(k), LocalGet(lit), I32Sub,
            LocalGet(c), I32Const(i32::from(b'$')), I32Eq, I32Add,
        );
    });
    when(function, b'&');
    wasm!(function; LocalGet(acc), I32WrapI64, LocalGet(t.p), Call(helpers.concat));
    box_heap(function);
    wasm!(function; LocalSet(acc), End);
    when(function, b'`');
    append_units(
        function,
        sv,
        &|f| wasm!(f; I32Const(0)),
        &|f| wasm!(f; LocalGet(pos)),
    );
    wasm!(function; End);
    when(function, b'\'');
    append_units(function, sv, &|f| wasm!(f; LocalGet(end)), &|f| {
        wasm!(f; LocalGet(sv.n), LocalGet(end), I32Sub);
    });
    wasm!(function;
        End,
        LocalGet(k), I32Const(2), I32Add, LocalTee(k), LocalSet(lit),
        Else,
        LocalGet(k), I32Const(1), I32Add, LocalSet(k),
        End,
        Br(0),
        End, End,
    );
    append_units(function, tm, &|f| wasm!(f; LocalGet(lit)), &|f| {
        wasm!(f; LocalGet(tm.n), LocalGet(lit), I32Sub);
    });
}

/// `String.fromCharCode(...codeUnits)` / `String.fromCodePoint(...codePoints)`:
/// `ToNumber` of each argument in order (`fromCodePoint` throws a
/// `RangeError` at the first value that is not an integral `0..=0x10FFFF`),
/// written as its code unit (`ToUint16`) or as the code point's one or two
/// UTF-16 units (a lone surrogate code point is its own unit) into a two-byte
/// block of room enough, which ends at the units written and is narrowed.
pub(super) fn emit_from_char_code(
    code_point: bool,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (value, spread) = (frame.i64(0), frame.x64(0));
    let locals = i32_locals(frame);
    let [argc, idx, len, dst, width, unit] = [
        locals[0], locals[1], locals[2], locals[3], locals[4], locals[5],
    ];
    let point = frame.f64(0);
    let d = Str {
        p: dst,
        w: width,
        n: len,
    };
    // The spread argument array, before a `valueOf` can republish it.
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(spread));
    push_argc(function, frame);
    wasm!(function;
        LocalSet(argc), I32Const(0), LocalSet(idx), I32Const(0), LocalSet(len), I32Const(1), LocalSet(width),
    );
    emit_alloc_str(
        function,
        helpers.alloc,
        |f| wasm!(f; LocalGet(argc), I32Const(i32::from(code_point)), I32Shl),
        |f| wasm!(f; I32Const(1)),
        dst,
    );
    count_loop(function, idx, argc, |function| {
        arg_at(function, frame, idx, spread, value);
        emit_is_number(function, value);
        wasm!(function;
            I32Eqz, If(BlockType::Empty),
            LocalGet(value), Call(helpers.to_number), LocalSet(value),
        );
        return_if_pending(function);
        wasm!(function; End);
        if code_point {
            wasm!(function;
                LocalGet(value), F64ReinterpretI64, LocalTee(point), LocalGet(point), F64Trunc, F64Ne,
                LocalGet(point), F64Const(Ieee64::from(0.0)), F64Lt, I32Or,
                LocalGet(point), F64Const(Ieee64::from(1_114_111.0)), F64Gt, I32Or,
                If(BlockType::Empty),
            );
            throw_range_error(
                function,
                frame,
                INVALID_CODE_POINT,
                Some(value),
                strings,
                helpers,
            );
            wasm!(function;
                End,
                LocalGet(point), I32TruncSatF64U, LocalTee(unit), I32Const(0xFFFF), I32GtU,
                If(BlockType::Empty),
            );
            // A supplementary code point: its lead, then its trail.
            d.store(
                function,
                |f| wasm!(f; LocalGet(len)),
                |f| {
                    wasm!(f;
                        LocalGet(unit), I32Const(0x1_0000), I32Sub, I32Const(10), I32ShrU,
                        I32Const(0xD800), I32Or,
                    );
                },
            );
            wasm!(function;
                LocalGet(len), I32Const(1), I32Add, LocalSet(len),
                LocalGet(unit), I32Const(0x3FF), I32And, I32Const(0xDC00), I32Or, LocalSet(unit),
                End,
            );
        } else {
            emit_to_uint(function, value, 65_536.0, point);
            wasm!(function; LocalSet(unit));
        }
        d.store(
            function,
            |f| wasm!(f; LocalGet(len)),
            |f| wasm!(f; LocalGet(unit)),
        );
        wasm!(function; LocalGet(len), I32Const(1), I32Add, LocalSet(len));
    });
    wasm!(function;
        LocalGet(dst), LocalGet(len),
        I32Store(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }),
    );
    emit_str_narrow(function, dst, idx, unit);
    wasm!(function; LocalGet(dst));
    box_heap(function);
}

/// `String.raw(template, ...substitutions)`: `raw = ToObject(ToObject(
/// template).raw)`, then each `ToString(raw[i])` for `i < LengthOfArrayLike(
/// raw)`, interleaved with `ToString` of the substitutions while they last.
pub(super) fn emit_raw(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (raw, v, acc, spread) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let (len, i, key, n) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    // The spread argument array, before a getter or `toString` can republish it.
    wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(spread));
    let to_object = |function: &mut Function| {
        emit_is_nullish(function, raw);
        wasm!(function; If(BlockType::Empty));
        throw_type_error(function, frame, NULLISH_TO_OBJECT, strings, helpers);
        wasm!(function; End, LocalGet(raw), Call(helpers.to_object()), LocalSet(raw));
    };
    frame.arg(function, 0);
    wasm!(function; LocalSet(raw));
    to_object(function);
    wasm!(function; LocalGet(raw), I32Const(static_key(strings, "raw")), Call(helpers.get));
    wasm!(function; LocalSet(raw));
    return_if_pending(function);
    to_object(function);
    // `LengthOfArrayLike(raw)`: NaN and negatives are 0.
    wasm!(function;
        LocalGet(raw), I32Const(static_key(strings, INDEX_LENGTH_KEY)), Call(helpers.get),
    );
    return_if_pending(function);
    wasm!(function; Call(helpers.to_number));
    return_if_pending(function);
    wasm!(function; F64ReinterpretI64, I32TruncSatF64U, LocalSet(len));
    push_str(function, strings, "");
    wasm!(function; LocalSet(acc), I32Const(0), LocalSet(i));
    push_argc(function, frame);
    wasm!(function; LocalSet(n));
    count_loop(function, i, len, |function| {
        wasm!(function;
            LocalGet(i), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
            LocalSet(key),
            LocalGet(raw), LocalGet(key), Call(helpers.get), LocalSet(v),
        );
        return_if_pending(function);
        to_string(function, v, helpers);
        append(function, acc, v, helpers);
        // A substitution follows every literal but the last, while any is left.
        wasm!(function;
            LocalGet(i), I32Const(1), I32Add, LocalTee(key),
            LocalGet(len), I32LtU, LocalGet(key), LocalGet(n), I32LtU, I32And,
            If(BlockType::Empty),
        );
        arg_at(function, frame, key, spread, v);
        to_string(function, v, helpers);
        append(function, acc, v, helpers);
        wasm!(function; End);
    });
    wasm!(function; LocalGet(acc));
}

/// Push `1` when the jsval in `local` is a string: the heap tag in its top 16
/// bits, then the kind byte.
fn is_string(function: &mut Function, local: u32) {
    wasm!(function;
        LocalGet(local), I64Const(48), I64ShrU, I64Const(HEAP_BITS >> 48), I64Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64,
        I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }),
        I32Const(TYPE_STRING), I32Eq,
        Else, I32Const(0), End,
    );
}

/// Replace the jsval in i64 local `v` by its `ToString` (a heap string), returning when a
/// user `toString` throws.
pub(super) fn to_string(function: &mut Function, v: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(v), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(v),
    );
    return_if_pending(function);
}
