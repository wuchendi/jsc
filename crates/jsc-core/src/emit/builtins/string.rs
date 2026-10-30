//! `%String.prototype%` methods and the `String` statics. A generic method
//! takes `ToString(RequireObjectCoercible(this))`, coerces its arguments in the
//! guest (a user `toString` / `valueOf` may run, and throw), then runs its host
//! `__method` op; `toString` / `valueOf` are `thisStringValue`. A host op that
//! refuses its input (a byte position over a non-ASCII string, a lone
//! surrogate, a `RegExp` pattern; DESIGN.md §13) returns a sentinel the builtin
//! throws as a named error.

#[allow(clippy::wildcard_imports)]
use super::*;

/// How a method coerces one argument before its host op.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Coerce {
    /// Unused: passed as is.
    None,
    /// `ToString` (`undefined` becomes `"undefined"`).
    String,
    /// `ToString` unless `undefined` (which selects the host default).
    OptString,
    /// `ToNumber` unless `undefined` (which selects the host default).
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
        | StringOp::CodePointAt => [Coerce::Number, Coerce::None],
        StringOp::PadStart | StringOp::PadEnd => [Coerce::Number, Coerce::OptString],
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
        | StringOp::ReplaceAll => [Coerce::None, Coerce::None],
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

/// Whether the host op of `op` may return a refusal sentinel for the
/// call-site arguments `args` (see [`emit_is_method_refusal`]).
fn may_refuse(op: StringOp, args: &[IrNode]) -> bool {
    match op {
        StringOp::ToUpperCase
        | StringOp::ToLowerCase
        | StringOp::ToLocaleUpperCase
        | StringOp::ToLocaleLowerCase
        | StringOp::Trim
        | StringOp::TrimStart
        | StringOp::TrimEnd
        | StringOp::Repeat
        | StringOp::LocaleCompare
        | StringOp::Concat
        | StringOp::MatchAll => false,
        // A position argument counts bytes.
        StringOp::Includes | StringOp::StartsWith | StringOp::EndsWith => args.len() > 1,
        // Only an empty separator / pattern splits characters.
        StringOp::Split | StringOp::Replace | StringOp::ReplaceAll => match args.first() {
            None => false,
            Some(IrNode::Str(s)) => s.is_empty(),
            Some(_) => true,
        },
        _ => true,
    }
}

/// The host op flag of `op` when the call-site arguments `args` are already
/// of the types its coercions produce (string / number literals; a missing
/// argument is `undefined`), so that on a string receiver the builtin is
/// exactly its host op, inline; with whether that op may refuse its input
/// (then the call site falls back to the builtin, which throws). `repeat`
/// (count check), `match` (result array) and the custom bodies never inline.
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
    if matches!(
        op,
        StringOp::Repeat
            | StringOp::Match
            | StringOp::Concat
            | StringOp::MatchAll
            | StringOp::Replace
            | StringOp::ReplaceAll
    ) {
        return None;
    }
    let [first, second] = coercions(op);
    if !(ready(first, args.first()) && ready(second, args.get(1))) {
        return None;
    }
    op.flag().map(|flag| (flag, may_refuse(op, args)))
}

/// Push i32 `1` when the `__method` result on the stack is a refusal
/// sentinel (`METHOD_UNSUPPORTED`, `METHOD_RANGE_ERROR` or
/// `METHOD_NEEDS_REGEXP`: `undefined | 1..=3`).
pub(in crate::emit) fn emit_is_method_refusal(function: &mut Function) {
    wasm!(function;
        I64Const(abi::METHOD_UNSUPPORTED.cast_signed()), I64Sub, I64Const(3), I64LtU,
    );
}

/// The `TypeError` texts of one `String.prototype` method: a
/// `null`/`undefined` receiver, and an input its byte-based host op cannot
/// handle yet.
struct Texts {
    nullish: CalleeErrorText<'static>,
    non_ascii: CalleeErrorText<'static>,
}

/// The [`Texts`] of `String.prototype.<name>`.
macro_rules! texts {
    ($name:literal) => {
        Texts {
            nullish: [
                concat!("String.prototype.", $name, " called on null or undefined"),
                concat!(
                    "TypeError: String.prototype.",
                    $name,
                    " called on null or undefined"
                ),
            ],
            non_ascii: [
                concat!(
                    "String.prototype.",
                    $name,
                    " does not support non-ASCII strings yet"
                ),
                concat!(
                    "TypeError: String.prototype.",
                    $name,
                    " does not support non-ASCII strings yet"
                ),
            ],
        }
    };
}

/// The [`Texts`] of `op`.
fn texts(op: StringOp) -> Texts {
    match op {
        StringOp::IndexOf => texts!("indexOf"),
        StringOp::Includes => texts!("includes"),
        StringOp::Slice => texts!("slice"),
        StringOp::ToUpperCase => texts!("toUpperCase"),
        StringOp::ToLowerCase => texts!("toLowerCase"),
        StringOp::ToLocaleUpperCase => texts!("toLocaleUpperCase"),
        StringOp::ToLocaleLowerCase => texts!("toLocaleLowerCase"),
        StringOp::CharAt => texts!("charAt"),
        StringOp::Trim => texts!("trim"),
        StringOp::TrimStart => texts!("trimStart"),
        StringOp::TrimEnd => texts!("trimEnd"),
        StringOp::Repeat => texts!("repeat"),
        StringOp::StartsWith => texts!("startsWith"),
        StringOp::EndsWith => texts!("endsWith"),
        StringOp::PadStart => texts!("padStart"),
        StringOp::PadEnd => texts!("padEnd"),
        StringOp::Substring => texts!("substring"),
        StringOp::Split => texts!("split"),
        StringOp::At => texts!("at"),
        StringOp::CharCodeAt => texts!("charCodeAt"),
        StringOp::CodePointAt => texts!("codePointAt"),
        StringOp::Concat => texts!("concat"),
        StringOp::LastIndexOf => texts!("lastIndexOf"),
        StringOp::LocaleCompare => texts!("localeCompare"),
        StringOp::Normalize => texts!("normalize"),
        StringOp::Search => texts!("search"),
        StringOp::Match => texts!("match"),
        StringOp::MatchAll => texts!("matchAll"),
        StringOp::Replace => texts!("replace"),
        StringOp::ReplaceAll => texts!("replaceAll"),
    }
}

/// Every op of this family.
const OPS: [StringOp; 30] = [
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

/// The `RangeError` text of `normalize` with an unknown form.
const NORMALIZE_FORM: [&str; 2] = [
    "The normalization form should be one of NFC, NFD, NFKC, NFKD.",
    "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD.",
];

/// The `TypeError`s of `fromCharCode` / `fromCodePoint` building a lone
/// surrogate, which the UTF-8 string representation cannot hold yet.
const FROM_CHAR_CODE_SURROGATE: CalleeErrorText<'static> = [
    "String.fromCharCode cannot build a lone surrogate yet",
    "TypeError: String.fromCharCode cannot build a lone surrogate yet",
];
const FROM_CODE_POINT_SURROGATE: CalleeErrorText<'static> = [
    "String.fromCodePoint cannot build a lone surrogate yet",
    "TypeError: String.fromCodePoint cannot build a lone surrogate yet",
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

/// The `RangeError` of a `repeat` result past [`MAX_STRING_LENGTH`].
const REPEAT_LENGTH: [&str; 2] = ["Invalid string length", "RangeError: Invalid string length"];

/// The longest string a `repeat` builds, in bytes (`2^29 - 24`, as V8).
const MAX_STRING_LENGTH: f64 = 536_870_888.0;

/// The own keys of a `match` result besides its element.
const MATCH_KEYS: [&str; 3] = ["index", "input", "groups"];

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    OPS.into_iter()
        .flat_map(|op| {
            let t = texts(op);
            t.nullish.into_iter().chain(t.non_ascii)
        })
        .chain(NOT_STRING_TO_STRING)
        .chain(NOT_STRING_VALUE_OF)
        .chain(SEARCH_REGEXP)
        .chain(MATCH_REGEXP)
        .chain(MATCH_ALL_REGEXP)
        .chain(NORMALIZE_FORM)
        .chain(FROM_CHAR_CODE_SURROGATE)
        .chain(FROM_CODE_POINT_SURROGATE)
        .chain(INVALID_CODE_POINT)
        .chain(NULLISH_TO_OBJECT)
        .chain(REPEAT_COUNT)
        .chain(REPEAT_LENGTH)
        .chain(MATCH_KEYS)
        .chain(["", "raw"])
}

/// `String.prototype.toString()` / `valueOf()`: `thisStringValue(this)`.
pub(super) fn emit_this_value(
    value_of: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
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
/// [`coercions`] in [`coercion_order`], `repeat`'s count check, then the host
/// op, whose refusal sentinels throw; `match` builds its result array.
/// `concat`, `matchAll`, `replace` and `replaceAll` continue in their own
/// bodies after the receiver. An object first argument whose chain holds the
/// method's well-known symbol ([`hook_of`], `__symbol_get`) throws the interim
/// `TypeError` after the receiver (Y4 dispatches to it,
/// design/symbol-strings.md §9.4).
pub(super) fn emit_method(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, a0, a1, r) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let count = frame.f64(0);
    let texts = texts(op);
    if op == StringOp::Concat {
        // The spread argument array, before `ToString(this)` can republish it.
        wasm!(function; GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(frame.x64(0)));
    }
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(s));
    is_string(function, s);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_nullish(function, s);
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, texts.nullish, strings, helpers);
    wasm!(function; End);
    to_string(function, s, helpers);
    wasm!(function; End);
    arg_hook_guard(op, function, frame, a0, strings, helpers);
    let flag = match (op, op.flag()) {
        (StringOp::Concat, _) => return emit_concat(function, frame, s, helpers),
        (StringOp::MatchAll, _) | (_, None) => {
            return throw_type_error(function, frame, MATCH_ALL_REGEXP, strings, helpers);
        }
        (StringOp::Replace | StringOp::ReplaceAll, _) => {
            return emit_replace(op, function, frame, strings, helpers);
        }
        (_, Some(flag)) => flag,
    };
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
        throw_range_error(function, frame, REPEAT_LENGTH, None, strings, helpers);
        wasm!(function; End, End);
    }
    if op == StringOp::Match {
        // `RegExpCreate(undefined)` is the empty pattern.
        wasm!(function; LocalGet(a0), I64Const(UNDEFINED_BITS), I64Eq, If(BlockType::Empty));
        push_str(function, strings, "");
        wasm!(function; LocalSet(a0), End);
    }
    host_call(function, s, flag, a0, a1, helpers);
    wasm!(function; LocalSet(r));
    if may_refuse(op, &[IrNode::Undefined, IrNode::Undefined]) {
        refusal_throws(op, function, frame, r, strings, helpers);
    }
    if op == StringOp::Match {
        emit_match_result(function, frame, s, a0, r, strings, helpers);
    } else {
        wasm!(function; LocalGet(r));
    }
}

/// Throw the interim `TypeError` when `op`'s first argument (read into i64
/// `a0`) is an object whose chain holds `op`'s [`hook_of`] symbol.
fn arg_hook_guard(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    a0: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let Some(symbol) = hook_of(op) else {
        return;
    };
    frame.arg(function, 0);
    wasm!(function; LocalSet(a0));
    emit_is_object(function, a0);
    wasm!(function; If(BlockType::Empty));
    emit_hook_guard(function, a0, symbol, frame.scratch(), strings, helpers);
    wasm!(function; End);
    return_if_pending(function);
}

/// The well-known symbol `op` consults on an object first argument: `@@match`
/// (`IsRegExp` of `includes` / `startsWith` / `endsWith`, and `match`),
/// `@@matchAll`, `@@replace`, `@@search`, `@@split`.
fn hook_of(op: StringOp) -> Option<WellKnown> {
    match op {
        StringOp::Includes | StringOp::StartsWith | StringOp::EndsWith | StringOp::Match => {
            Some(WellKnown::Match)
        }
        StringOp::MatchAll => Some(WellKnown::MatchAll),
        StringOp::Replace | StringOp::ReplaceAll => Some(WellKnown::Replace),
        StringOp::Search => Some(WellKnown::Search),
        StringOp::Split => Some(WellKnown::Split),
        _ => None,
    }
}

/// Push the `__method` result of host op `flag` over the jsvals in i64
/// locals `recv`, `a0` and `a1`.
fn host_call(
    function: &mut Function,
    recv: u32,
    flag: i32,
    a0: u32,
    a1: u32,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(recv), I32Const(flag), LocalGet(a0), LocalGet(a1), Call(helpers.method),
    );
}

/// Throw the error a refusal sentinel in i64 `r` stands for: the method's
/// non-ASCII `TypeError`, `normalize`'s form `RangeError`, or the `RegExp`
/// `TypeError` of `search` / `match`.
fn refusal_throws(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    r: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let when = |function: &mut Function, sentinel: u64| {
        wasm!(function; LocalGet(r), I64Const(sentinel.cast_signed()), I64Eq, If(BlockType::Empty));
    };
    when(function, abi::METHOD_UNSUPPORTED);
    throw_type_error(function, frame, texts(op).non_ascii, strings, helpers);
    wasm!(function; End);
    if op == StringOp::Normalize {
        when(function, abi::METHOD_RANGE_ERROR);
        throw_range_error(function, frame, NORMALIZE_FORM, None, strings, helpers);
        wasm!(function; End);
    }
    let regexp = match op {
        StringOp::Search => SEARCH_REGEXP,
        StringOp::Match => MATCH_REGEXP,
        _ => return,
    };
    when(function, abi::METHOD_NEEDS_REGEXP);
    throw_type_error(function, frame, regexp, strings, helpers);
    wasm!(function; End);
}

/// Throw a `RangeError` with `text` (`[message, stack]`), followed by the
/// jsval in i64 local `value` when given.
fn throw_range_error(
    function: &mut Function,
    frame: Frame,
    text: [&str; 2],
    value: Option<u32>,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let part = |prefix: &str| match value {
        Some(local) => IrNode::Binary {
            op: BinOp::Add,
            lhs: Box::new(IrNode::Str(prefix.to_owned())),
            rhs: Box::new(IrNode::Local(local)),
        },
        None => IrNode::Str(prefix.to_owned()),
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
    strings: &HashMap<String, i32>,
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

/// `replace` / `replaceAll(searchValue, replaceValue)` over the string in
/// i64 temp 0 with a string pattern (`ToString(searchValue)`): a callable
/// `replaceValue` is called with `(match, position, string)` for each match
/// (UTF-16 positions by the host `indexOf`), its `ToString`ed results
/// spliced in by the host; any other is `ToString`ed and expanded by the host
/// (`GetSubstitution`).
#[allow(clippy::too_many_lines)]
fn emit_replace(
    op: StringOp,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (s, search, replacer, r) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (list, v) = (frame.x64(0), frame.x64(1));
    let (len, next) = (frame.i32(0), frame.i32(1));
    let all = op == StringOp::ReplaceAll;
    let non_ascii = texts(op).non_ascii;
    frame.arg(function, 0);
    wasm!(function; LocalSet(search));
    to_string(function, search, helpers);
    frame.arg(function, 1);
    wasm!(function; LocalSet(replacer));
    let flag = if all {
        abi::METHOD_OP_REPLACE_ALL
    } else {
        abi::METHOD_OP_REPLACE
    };
    // The host splices the template, or the replacer's results (an array).
    let splice = |function: &mut Function| {
        host_call(function, s, flag, search, replacer, helpers);
        wasm!(function; LocalTee(r), I64Const(abi::METHOD_UNSUPPORTED.cast_signed()), I64Eq);
        wasm!(function; If(BlockType::Empty));
        throw_type_error(function, frame, non_ascii, strings, helpers);
        wasm!(function; End, LocalGet(r));
    };
    emit_is_function(function, replacer);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    to_string(function, replacer, helpers);
    splice(function);
    wasm!(function; Return, End);
    // The match positions (UTF-16 offsets): `indexOf(search, from)` from 0,
    // each next one `max(1, len)` past the last; a result before `from` is
    // the clamped end.
    let index_of = |function: &mut Function, from: &dyn Fn(&mut Function)| {
        wasm!(function; LocalGet(s), I32Const(abi::METHOD_OP_INDEX_OF), LocalGet(search));
        from(function);
        wasm!(function; Call(helpers.method), LocalSet(r));
    };
    wasm!(function; LocalGet(search), I32WrapI64);
    wasm!(function;
        I32Load(MemArg { offset: u64::from(abi::STR_LEN_OFFSET), align: 2, memory_index: 0 }),
        LocalSet(len),
        I32Const(0), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        LocalSet(list),
    );
    index_of(
        function,
        &|function| wasm!(function; I64Const(0.0f64.to_bits().cast_signed())),
    );
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(r), I64Const((-1.0f64).to_bits().cast_signed()), I64Eq, BrIf(1),
    );
    // The replacer over `(match, position, string)` with `this = undefined`;
    // its `ToString` result joins the list.
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
    wasm!(function;
        LocalGet(list), I32WrapI64, LocalGet(v), Call(helpers.array_push), Drop,
    );
    if all {
        wasm!(function;
            LocalGet(r), F64ReinterpretI64, I32TruncSatF64U,
            LocalGet(len), I32Const(1), LocalGet(len), I32Const(1), I32GtU, Select,
            I32Add, LocalSet(next),
        );
        index_of(function, &|function| {
            wasm!(function; LocalGet(next), F64ConvertI32U, I64ReinterpretF64);
        });
        wasm!(function;
            LocalGet(r), F64ReinterpretI64, LocalGet(next), F64ConvertI32U, F64Lt, BrIf(1),
            Br(0),
        );
    }
    wasm!(function; End, End, LocalGet(list), LocalSet(replacer));
    splice(function);
}

/// `String.fromCharCode(...codeUnits)` / `String.fromCodePoint(...codePoints)`:
/// `ToNumber` of each argument in order (`fromCodePoint` throws a
/// `RangeError` at the first value that is not an integral `0..=0x10FFFF`),
/// collected into a fresh array the host turns into the string; a lone
/// surrogate throws a `TypeError`.
pub(super) fn emit_from_char_code(
    code_point: bool,
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (v, result, list, spread) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.x64(0));
    let (n, i) = (frame.i32(0), frame.i32(1));
    let point = frame.f64(0);
    // The spread argument array, before a `valueOf` can republish it.
    wasm!(function;
        GlobalGet(SPREAD_ARGS_GLOBAL), LocalSet(spread),
        I32Const(0), Call(helpers.new_array()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        LocalSet(list),
    );
    push_argc(function, frame);
    wasm!(function; LocalSet(n), I32Const(0), LocalSet(i));
    count_loop(function, i, n, |function| {
        arg_at(function, frame, i, spread, v);
        emit_is_number(function, v);
        wasm!(function;
            I32Eqz, If(BlockType::Empty),
            LocalGet(v), Call(helpers.to_number), LocalSet(v),
        );
        return_if_pending(function);
        wasm!(function; End);
        if code_point {
            wasm!(function;
                LocalGet(v), F64ReinterpretI64, LocalTee(point), LocalGet(point), F64Trunc, F64Ne,
                LocalGet(point), F64Const(Ieee64::from(0.0)), F64Lt, I32Or,
                LocalGet(point), F64Const(Ieee64::from(1_114_111.0)), F64Gt, I32Or,
                If(BlockType::Empty),
            );
            throw_range_error(
                function,
                frame,
                INVALID_CODE_POINT,
                Some(v),
                strings,
                helpers,
            );
            wasm!(function; End);
        }
        wasm!(function;
            LocalGet(list), I32WrapI64, LocalGet(v), Call(helpers.array_push), Drop,
        );
    });
    let (flag, surrogate) = if code_point {
        (abi::METHOD_OP_FROM_CODE_POINT, FROM_CODE_POINT_SURROGATE)
    } else {
        (abi::METHOD_OP_FROM_CHAR_CODE, FROM_CHAR_CODE_SURROGATE)
    };
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(v));
    host_call(function, list, flag, v, v, helpers);
    wasm!(function;
        LocalTee(result), I64Const(abi::METHOD_UNSUPPORTED.cast_signed()), I64Eq,
        If(BlockType::Empty),
    );
    throw_type_error(function, frame, surrogate, strings, helpers);
    wasm!(function; End, LocalGet(result));
}

/// `String.raw(template, ...substitutions)`: `raw = ToObject(ToObject(
/// template).raw)`, then each `ToString(raw[i])` for `i < LengthOfArrayLike(
/// raw)`, interleaved with `ToString` of the substitutions while they last.
pub(super) fn emit_raw(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
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
