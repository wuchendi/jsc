//! `Uint8Array` base64 / hex (DESIGN.md §5.8.7): the statics
//! `Uint8Array.fromBase64` / `fromHex` and the prototype methods `toBase64`
//! / `toHex` / `setFromBase64` / `setFromHex`.
//!
//! Decoding runs in the guest over the input string's code units (one-byte
//! or two-byte) and never runs user code, so a target's bytes are written
//! as they are decoded: `setFrom*` keep the bytes decoded before a
//! malformed position, as the spec requires, and the statics decode twice
//! (once to count and validate, once into the fresh `Uint8Array`).

#[allow(clippy::wildcard_imports)]
use super::*;

use super::typed_array::{field, is_kind, m8, m32, new_uint8_array, type_error_if};
use crate::emit::binary::RAW_DATA;
use crate::emit::realm::Base64Op;

/// `[message, stack]` of a `TypeError` with `message`.
macro_rules! type_error {
    ($message:expr) => {
        [$message, concat!("TypeError: ", $message)]
    };
}

/// `[message, stack]` of an error of class `$class` with `message`.
macro_rules! error_text {
    ($class:literal, $message:literal) => {
        [$message, concat!($class, ": ", $message)]
    };
}

/// The input is not a string.
const NOT_STRING: CalleeErrorText<'static> = type_error!("The input must be a string");

/// `GetOptionsObject` over a value that is neither `undefined` nor an
/// object.
const OPTIONS_NOT_OBJECT: CalleeErrorText<'static> =
    type_error!("The options argument must be an object");

/// An `alphabet` option other than `"base64"` / `"base64url"`.
const BAD_ALPHABET: CalleeErrorText<'static> =
    type_error!("The alphabet option must be \"base64\" or \"base64url\"");

/// A `lastChunkHandling` option other than the three modes.
const BAD_LAST_CHUNK: CalleeErrorText<'static> = type_error!(
    "The lastChunkHandling option must be \"loose\", \"strict\" or \"stop-before-partial\""
);

/// `ValidateUint8Array` over another receiver.
const NOT_UINT8: CalleeErrorText<'static> = type_error!("The receiver must be a Uint8Array");

/// The receiver's buffer is detached or the view out of bounds.
const OUT_OF_BOUNDS: CalleeErrorText<'static> =
    type_error!("Cannot perform the operation on a detached or out-of-bounds Uint8Array");

/// Malformed base64 input.
const BAD_BASE64: CalleeErrorText<'static> = error_text!("SyntaxError", "Invalid base64 string");

/// Malformed hex input.
const BAD_HEX: CalleeErrorText<'static> = error_text!("SyntaxError", "Invalid hex string");

/// An encoding longer than the longest string.
const STRING_LENGTH: CalleeErrorText<'static> = error_text!("RangeError", "Invalid string length");

/// The option keys and recognized option values.
const OPTION_STRINGS: [&str; 10] = [
    "alphabet",
    "lastChunkHandling",
    "omitPadding",
    "base64",
    "base64url",
    "loose",
    "strict",
    "stop-before-partial",
    "read",
    "written",
];

/// The `maxLength` of a decode into a fresh array: larger than any string.
const UNBOUNDED: i32 = i32::MAX;

/// `lastChunkHandling` `"strict"`.
const STRICT: i32 = 1;

/// `lastChunkHandling` `"stop-before-partial"`.
const STOP: i32 = 2;

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    [
        NOT_STRING,
        OPTIONS_NOT_OBJECT,
        BAD_ALPHABET,
        BAD_LAST_CHUNK,
        NOT_UINT8,
        OUT_OF_BOUNDS,
        BAD_BASE64,
        BAD_HEX,
        STRING_LENGTH,
    ]
    .into_iter()
    .flatten()
    .chain(OPTION_STRINGS)
    .chain([""])
}

/// The locals of a decode or encode: i32 unless noted.
#[derive(Clone, Copy)]
struct Locals {
    /// The input string's block address (decode) or the source bytes'
    /// address (encode).
    s: u32,
    /// The input string's width (`1` two-byte).
    w: u32,
    /// The input length in code units (decode) or bytes (encode).
    len: u32,
    /// The destination address (`0` while only counting) or the output
    /// string's block (encode).
    dst: u32,
    /// The `maxLength` (decode) or the output length (encode).
    max: u32,
    /// The `base64url` alphabet.
    url: u32,
    /// `lastChunkHandling` (`0` loose, [`STRICT`], [`STOP`]); `omitPadding`
    /// when encoding.
    mode: u32,
    /// Code units consumed.
    read: u32,
    /// Bytes produced.
    written: u32,
    /// A malformed input was found.
    err: u32,
    /// The cursor.
    index: u32,
    /// The current code unit or sextet.
    c: u32,
    /// The pending chunk's bits.
    chunk: u32,
    /// The pending chunk's length in characters.
    chunk_len: u32,
    /// A temp.
    v: u32,
}

impl Locals {
    fn new(frame: Frame) -> Self {
        Self {
            s: frame.w32(0),
            w: frame.w32(1),
            len: frame.w32(2),
            dst: frame.w32(3),
            max: frame.w32(4),
            url: frame.w32(5),
            mode: frame.w32(6),
            read: frame.w32(7),
            written: frame.y32(0),
            err: frame.y32(1),
            index: frame.y32(2),
            c: frame.y32(3),
            chunk: frame.x32(0),
            chunk_len: frame.x32(1),
            v: frame.i32(3),
        }
    }
}

/// Emit the body of the base64 / hex builtin `op` into `f`.
pub(super) fn emit(
    op: Base64Op,
    f: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        Base64Op::FromBase64 | Base64Op::FromHex => emit_from(op, f, frame, strings, helpers),
        Base64Op::SetFromBase64 | Base64Op::SetFromHex => {
            emit_set_from(op, f, frame, strings, helpers);
        }
        Base64Op::ToBase64 | Base64Op::ToHex => emit_to(op, f, frame, strings, helpers),
    }
}

/// Throw a `SyntaxError` with `text` and return.
fn throw_syntax_error(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    [message, stack]: CalleeErrorText<'_>,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let error = IrNode::error_object(
        ErrorKind::SyntaxError,
        Some(IrNode::Str(message.into())),
        IrNode::Str(stack.into()),
    );
    emit_throw_error(&error, function, frame.scratch(), strings, helpers);
}

/// Throw `text` (a `TypeError`) unless the jsval in i64 `v` is a string.
fn require_string(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    v: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_string(function, v);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, NOT_STRING, strings, helpers);
}

/// `ValidateUint8Array(this)`: read `this` into i64 `this` and throw unless
/// it is a typed array of kind `Uint8`.
fn this_uint8(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    this: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; GlobalGet(THIS_GLOBAL), LocalSet(this));
    is_kind(function, this, abi::TYPE_TYPEDARRAY);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(this), I32WrapI64, I32Load8U(m8(abi::TA_KIND_OFFSET)),
        I32Const(i32::from(ElementKind::Uint8.kind())), I32Ne,
        Else, I32Const(1), End,
    );
    type_error_if(function, frame, NOT_UINT8, strings, helpers);
}

/// Throw the out-of-bounds `TypeError` when the typed array in i64 `this`
/// is out of bounds or detached, else set i32 `len` to its length.
fn live_length(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (this, len): (u32, u32),
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(this), I32WrapI64, Call(helpers.ta_oob()));
    type_error_if(function, frame, OUT_OF_BOUNDS, strings, helpers);
    wasm!(function; LocalGet(this), I32WrapI64, Call(helpers.ta_length()), LocalSet(len));
}

/// Push the address of element `0` of the `Uint8Array` in i64 `ta`.
fn data_address(function: &mut FnBuilder<'_>, ta: u32) {
    field(function, ta, abi::TA_BUFFER_OFFSET);
    wasm!(function;
        I32Load(m32(abi::AB_DATA_OFFSET)), I32Const(RAW_DATA.cast_signed()), I32Add,
    );
    field(function, ta, abi::TA_BYTE_OFFSET_OFFSET);
    wasm!(function; I32Add);
}

/// `GetOptionsObject(options)` for the jsval in i64 `opts`: `undefined`
/// stays (every option then reads `undefined`), another non-object throws.
fn options_object(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    opts: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(opts), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    emit_is_object(function, opts);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, OPTIONS_NOT_OBJECT, strings, helpers);
    wasm!(function; End);
}

/// `Get(opts, key)` into i64 `out` (`undefined` without options; a throw
/// returns).
fn get_option(
    function: &mut FnBuilder<'_>,
    (opts, out): (u32, u32),
    key: &str,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(opts), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I64)),
        I64Const(UNDEFINED_BITS),
        Else,
        LocalGet(opts), I32Const(static_key(strings, key)), Call(helpers.get),
        End,
        LocalSet(out),
    );
    return_if_pending(function);
}

/// A string-valued option: its key, its values in order (the first is the
/// default) and the `TypeError` of any other value.
struct StringOption {
    key: &'static str,
    values: &'static [&'static str],
    text: CalleeErrorText<'static>,
}

/// The `alphabet` option.
const ALPHABET: StringOption = StringOption {
    key: "alphabet",
    values: &["base64", "base64url"],
    text: BAD_ALPHABET,
};

/// The `lastChunkHandling` option (index [`STRICT`], [`STOP`]).
const LAST_CHUNK: StringOption = StringOption {
    key: "lastChunkHandling",
    values: &["loose", "strict", "stop-before-partial"],
    text: BAD_LAST_CHUNK,
};

/// Set i32 `out` to the index in `option.values` of the option of `opts`
/// (`0` when `undefined`), throwing its `TypeError` for any other value;
/// `v` is an i64 temp.
fn string_option(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (opts, v, out): (u32, u32, u32),
    option: &StringOption,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let StringOption { key, values, text } = *option;
    get_option(function, (opts, v), key, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(out));
    wasm!(function; LocalGet(v), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    emit_is_string(function, v);
    wasm!(function; I32Eqz);
    type_error_if(function, frame, text, strings, helpers);
    // `out` = the matching index, else `-1`.
    wasm!(function; I32Const(-1), LocalSet(out));
    for (i, value) in (0i32..).zip(values) {
        wasm!(function;
            I32Const(i),
            LocalGet(out),
            LocalGet(v), I32WrapI64, I32Const(static_key(strings, value)), Call(helpers.str_eq),
            Select, LocalSet(out),
        );
    }
    wasm!(function; LocalGet(out), I32Const(0), I32LtS);
    type_error_if(function, frame, text, strings, helpers);
    wasm!(function; End);
}

/// Load the string jsval in i64 `string` into `l.s` / `l.w` / `l.len`.
fn load_string(function: &mut FnBuilder<'_>, string: u32, l: Locals) {
    wasm!(function;
        LocalGet(string), I32WrapI64, LocalTee(l.s),
        I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)), I32Const(i32::from(abi::STR_FLAG_TWO_BYTE)), I32And,
        I32Const(0), I32Ne, LocalSet(l.w),
        LocalGet(l.s), I32Load(m32(abi::STR_LEN_OFFSET)), LocalSet(l.len),
    );
}

/// Push code unit `l.index` of the input string.
fn unit(function: &mut FnBuilder<'_>, l: Locals) {
    let m16 = MemArg {
        offset: u64::from(abi::STR_DATA_OFFSET),
        align: 1,
        memory_index: 0,
    };
    wasm!(function;
        LocalGet(l.w),
        If(BlockType::Result(ValType::I32)),
        LocalGet(l.s), LocalGet(l.index), I32Const(1), I32Shl, I32Add, I32Load16U(m16),
        Else,
        LocalGet(l.s), LocalGet(l.index), I32Add, I32Load8U(m8(abi::STR_DATA_OFFSET)),
        End,
    );
}

/// Append the byte `byte` pushes to the output (stored only when `l.dst`
/// is set) and count it.
fn put(function: &mut FnBuilder<'_>, l: Locals, byte: impl Fn(&mut FnBuilder<'_>)) {
    wasm!(function; LocalGet(l.dst), If(BlockType::Empty), LocalGet(l.dst), LocalGet(l.written), I32Add);
    byte(function);
    wasm!(function;
        I32Store8(m8(0)), End,
        LocalGet(l.written), I32Const(1), I32Add, LocalSet(l.written),
    );
}

/// `SkipAsciiWhitespace`: advance `l.index` past TAB, LF, FF, CR and SPACE.
fn skip_whitespace(function: &mut FnBuilder<'_>, l: Locals) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(l.index), LocalGet(l.len), I32GeU, BrIf(1),
    );
    unit(function, l);
    wasm!(function;
        LocalTee(l.c), I32Const(0x20), I32Eq,
        LocalGet(l.c), I32Const(0x09), I32Eq, I32Or,
        LocalGet(l.c), I32Const(0x0A), I32Eq, I32Or,
        LocalGet(l.c), I32Const(0x0C), I32Eq, I32Or,
        LocalGet(l.c), I32Const(0x0D), I32Eq, I32Or,
        I32Eqz, BrIf(1),
        LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.index),
        Br(0),
        End, End,
    );
}

/// Set `l.err` and branch `depth` out (to the decode's end).
fn fail(function: &mut FnBuilder<'_>, l: Locals, depth: u32) {
    wasm!(function; I32Const(1), LocalSet(l.err), Br(depth));
}

/// Append the bytes of a final partial chunk of `l.chunk_len` (2 or 3)
/// characters in `l.chunk`.
fn put_partial(function: &mut FnBuilder<'_>, l: Locals) {
    wasm!(function; LocalGet(l.chunk_len), I32Const(2), I32Eq, If(BlockType::Empty));
    put(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(4), I32ShrU),
    );
    wasm!(function; Else);
    put(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(10), I32ShrU),
    );
    put(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(2), I32ShrU),
    );
    wasm!(function; End);
}

/// Map the code unit in `l.c` to its sextet (`-1` when it is not in the
/// alphabet `l.url` selects).
fn sextet(function: &mut FnBuilder<'_>, l: Locals) {
    // base64url: `-` / `_` stand for `+` / `/`, which are then invalid.
    wasm!(function;
        LocalGet(l.url), If(BlockType::Empty),
        I32Const(0), LocalGet(l.c),
        LocalGet(l.c), I32Const(0x2B), I32Eq, LocalGet(l.c), I32Const(0x2F), I32Eq, I32Or,
        Select, LocalSet(l.c),
        I32Const(0x2B), LocalGet(l.c), LocalGet(l.c), I32Const(0x2D), I32Eq, Select, LocalSet(l.c),
        I32Const(0x2F), LocalGet(l.c), LocalGet(l.c), I32Const(0x5F), I32Eq, Select, LocalSet(l.c),
        End,
        I32Const(-1), LocalSet(l.v),
    );
    for (lo, count, add) in [
        (0x41, 26, -0x41),
        (0x61, 26, 26 - 0x61),
        (0x30, 10, 52 - 0x30),
        (0x2B, 1, 62 - 0x2B),
        (0x2F, 1, 63 - 0x2F),
    ] {
        wasm!(function;
            LocalGet(l.c), I32Const(add), I32Add,
            LocalGet(l.v),
            LocalGet(l.c), I32Const(lo), I32Sub, I32Const(count), I32LtU,
            Select, LocalSet(l.v),
        );
    }
    wasm!(function; LocalGet(l.v), LocalSet(l.c));
}

/// `FromBase64(string, alphabet, lastChunkHandling, maxLength)` over the
/// loaded string, the alphabet in `l.url`, the mode in `l.mode` and the
/// `maxLength` in `l.max`, writing to `l.dst` when set: leaves `l.read`,
/// `l.written` and `l.err`.
fn decode_base64(function: &mut FnBuilder<'_>, l: Locals) {
    for local in [l.read, l.written, l.err, l.index, l.chunk, l.chunk_len] {
        wasm!(function; I32Const(0), LocalSet(local));
    }
    // Depths below count the enclosing constructs up to this block.
    wasm!(function;
        Block(BlockType::Empty),
        LocalGet(l.max), I32Eqz, BrIf(0),
        Loop(BlockType::Empty),
    );
    skip_whitespace(function, l);
    // The end of the input.
    wasm!(function;
        LocalGet(l.index), LocalGet(l.len), I32Eq, If(BlockType::Empty),
        LocalGet(l.chunk_len), If(BlockType::Empty),
        LocalGet(l.mode), I32Const(STOP), I32Eq, BrIf(3),
        LocalGet(l.mode), I32Const(STRICT), I32Eq,
        LocalGet(l.chunk_len), I32Const(1), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    fail(function, l, 4);
    wasm!(function; End);
    put_partial(function, l);
    wasm!(function;
        End,
        LocalGet(l.len), LocalSet(l.read), Br(2),
        End,
    );
    unit(function, l);
    wasm!(function;
        LocalSet(l.c),
        LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.index),
        // Padding: the last chunk.
        LocalGet(l.c), I32Const(0x3D), I32Eq, If(BlockType::Empty),
        LocalGet(l.chunk_len), I32Const(2), I32LtU, If(BlockType::Empty),
    );
    fail(function, l, 3);
    wasm!(function; End);
    skip_whitespace(function, l);
    wasm!(function;
        LocalGet(l.chunk_len), I32Const(2), I32Eq, If(BlockType::Empty),
        LocalGet(l.index), LocalGet(l.len), I32Eq, If(BlockType::Empty),
        LocalGet(l.mode), I32Const(STOP), I32Ne, LocalSet(l.err), Br(4),
        End,
    );
    unit(function, l);
    wasm!(function; I32Const(0x3D), I32Eq, If(BlockType::Empty));
    wasm!(function; LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.index));
    skip_whitespace(function, l);
    wasm!(function;
        End,
        End,
        LocalGet(l.index), LocalGet(l.len), I32LtU, If(BlockType::Empty),
    );
    fail(function, l, 3);
    // Strict: the unused bits of the last character must be zero.
    wasm!(function;
        End,
        LocalGet(l.mode), I32Const(STRICT), I32Eq,
        LocalGet(l.chunk), I32Const(15), I32And,
        LocalGet(l.chunk), I32Const(3), I32And,
        LocalGet(l.chunk_len), I32Const(2), I32Eq, Select,
        I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
    );
    fail(function, l, 3);
    wasm!(function; End);
    put_partial(function, l);
    wasm!(function;
        LocalGet(l.len), LocalSet(l.read), Br(2),
        End,
    );
    sextet(function, l);
    wasm!(function; LocalGet(l.c), I32Const(0), I32LtS, If(BlockType::Empty));
    fail(function, l, 2);
    wasm!(function;
        End,
        // Stop before a chunk that would not fit.
        LocalGet(l.max), LocalGet(l.written), I32Sub, LocalTee(l.v), I32Const(1), I32Eq,
        LocalGet(l.chunk_len), I32Const(2), I32Eq, I32And,
        LocalGet(l.v), I32Const(2), I32Eq,
        LocalGet(l.chunk_len), I32Const(3), I32Eq, I32And,
        I32Or, BrIf(1),
        LocalGet(l.chunk), I32Const(6), I32Shl, LocalGet(l.c), I32Or, LocalSet(l.chunk),
        LocalGet(l.chunk_len), I32Const(1), I32Add, LocalTee(l.chunk_len),
        I32Const(4), I32Eq, If(BlockType::Empty),
    );
    for shift in [16, 8, 0] {
        put(
            function,
            l,
            |f| wasm!(f; LocalGet(l.chunk), I32Const(shift), I32ShrU),
        );
    }
    wasm!(function;
        I32Const(0), LocalSet(l.chunk), I32Const(0), LocalSet(l.chunk_len),
        LocalGet(l.index), LocalSet(l.read),
        LocalGet(l.written), LocalGet(l.max), I32Eq, BrIf(2),
        End,
        Br(0),
        End,
        End,
    );
}

/// Set `out` to the hex digit value of the code unit in `src` (`-1` when it
/// is not a hex digit).
fn hex_value(function: &mut FnBuilder<'_>, src: u32, out: u32) {
    wasm!(function; I32Const(-1), LocalSet(out));
    for (lo, count, add) in [
        (0x30, 10, -0x30),
        (0x61, 6, 10 - 0x61),
        (0x41, 6, 10 - 0x41),
    ] {
        wasm!(function;
            LocalGet(src), I32Const(add), I32Add,
            LocalGet(out),
            LocalGet(src), I32Const(lo), I32Sub, I32Const(count), I32LtU,
            Select, LocalSet(out),
        );
    }
}

/// `FromHex(string, maxLength)` over the loaded string with the
/// `maxLength` in `l.max`, writing to `l.dst` when set: leaves `l.read`,
/// `l.written` and `l.err`.
fn decode_hex(function: &mut FnBuilder<'_>, l: Locals) {
    for local in [l.read, l.written, l.err] {
        wasm!(function; I32Const(0), LocalSet(local));
    }
    wasm!(function;
        Block(BlockType::Empty),
        LocalGet(l.len), I32Const(1), I32And, If(BlockType::Empty),
    );
    fail(function, l, 1);
    wasm!(function;
        End,
        Loop(BlockType::Empty),
        LocalGet(l.read), LocalGet(l.len), I32GeU,
        LocalGet(l.written), LocalGet(l.max), I32GeU, I32Or, BrIf(1),
        LocalGet(l.read), LocalSet(l.index),
    );
    unit(function, l);
    wasm!(function; LocalSet(l.v));
    hex_value(function, l.v, l.c);
    wasm!(function; LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.index));
    unit(function, l);
    wasm!(function; LocalSet(l.v));
    hex_value(function, l.v, l.chunk);
    wasm!(function;
        LocalGet(l.c), LocalGet(l.chunk), I32Or, I32Const(0), I32LtS, If(BlockType::Empty),
    );
    fail(function, l, 2);
    wasm!(function;
        End,
        LocalGet(l.read), I32Const(2), I32Add, LocalSet(l.read),
    );
    put(
        function,
        l,
        |f| wasm!(f; LocalGet(l.c), I32Const(4), I32Shl, LocalGet(l.chunk), I32Or),
    );
    wasm!(function; Br(0), End, End);
}

/// Read the alphabet and (for base64) `lastChunkHandling` options of the
/// options jsval in i64 `opts` into `l.url` / `l.mode`; `v` is an i64 temp.
fn decode_options(
    function: &mut FnBuilder<'_>,
    frame: Frame,
    (opts, v): (u32, u32),
    l: Locals,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    options_object(function, frame, opts, strings, helpers);
    string_option(
        function,
        frame,
        (opts, v, l.url),
        &ALPHABET,
        strings,
        helpers,
    );
    string_option(
        function,
        frame,
        (opts, v, l.mode),
        &LAST_CHUNK,
        strings,
        helpers,
    );
}

/// `Uint8Array.fromBase64(string, options)` / `Uint8Array.fromHex(string)`:
/// the string check, the options (base64), a counting decode that throws
/// the `SyntaxError` of a malformed input, then a fresh `Uint8Array` the
/// second decode fills.
fn emit_from(
    op: Base64Op,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let base64 = op == Base64Op::FromBase64;
    let (string, opts, v, out, buffer, proto) = (
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
        frame.y64(5),
        frame.z64(0),
    );
    let l = Locals::new(frame);
    frame.arg(function, 0);
    wasm!(function; LocalSet(string));
    frame.arg(function, 1);
    wasm!(function; LocalSet(opts));
    require_string(function, frame, string, strings, helpers);
    if base64 {
        decode_options(function, frame, (opts, v), l, strings, helpers);
    }
    let decode = |function: &mut FnBuilder<'_>| {
        load_string(function, string, l);
        wasm!(function; I32Const(UNBOUNDED), LocalSet(l.max));
        if base64 {
            decode_base64(function, l);
        } else {
            decode_hex(function, l);
        }
    };
    wasm!(function; I32Const(0), LocalSet(l.dst));
    decode(function);
    wasm!(function; LocalGet(l.err), If(BlockType::Empty));
    throw_syntax_error(
        function,
        frame,
        if base64 { BAD_BASE64 } else { BAD_HEX },
        strings,
        helpers,
    );
    wasm!(function; End);
    new_uint8_array(
        function,
        frame,
        (l.written, buffer, proto, frame.i32(0), frame.i32(1), out),
        helpers,
    );
    wasm!(function; LocalGet(l.written), If(BlockType::Empty));
    data_address(function, out);
    wasm!(function; LocalSet(l.dst));
    decode(function);
    wasm!(function; End, LocalGet(out));
}

/// `Uint8Array.prototype.setFromBase64(string, options)` /
/// `setFromHex(string)`: `ValidateUint8Array(this)`, the string check, the
/// options (base64), then an out-of-bounds or detached target throws; the
/// decode writes at most the target's length, a malformed input throws its
/// `SyntaxError` after the bytes before it are written, else `{ read,
/// written }`.
fn emit_set_from(
    op: Base64Op,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let base64 = op == Base64Op::SetFromBase64;
    let (this, string, opts, v, written) = (
        frame.y64(0),
        frame.y64(1),
        frame.y64(2),
        frame.y64(3),
        frame.y64(4),
    );
    let read = frame.z64(1);
    let l = Locals::new(frame);
    this_uint8(function, frame, this, strings, helpers);
    frame.arg(function, 0);
    wasm!(function; LocalSet(string));
    frame.arg(function, 1);
    wasm!(function; LocalSet(opts));
    require_string(function, frame, string, strings, helpers);
    if base64 {
        decode_options(function, frame, (opts, v), l, strings, helpers);
    }
    live_length(function, frame, (this, l.max), strings, helpers);
    data_address(function, this);
    wasm!(function; LocalSet(l.dst));
    load_string(function, string, l);
    if base64 {
        decode_base64(function, l);
    } else {
        decode_hex(function, l);
    }
    wasm!(function; LocalGet(l.err), If(BlockType::Empty));
    throw_syntax_error(
        function,
        frame,
        if base64 { BAD_BASE64 } else { BAD_HEX },
        strings,
        helpers,
    );
    wasm!(function;
        End,
        LocalGet(l.read), F64ConvertI32U, I64ReinterpretF64, LocalSet(read),
        LocalGet(l.written), F64ConvertI32U, I64ReinterpretF64, LocalSet(written),
    );
    let result = IrNode::Object {
        entries: vec![
            (PropKey::Static("read".into()), IrNode::Local(read)),
            (PropKey::Static("written".into()), IrNode::Local(written)),
        ],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    };
    emit_node(&result, function, frame.scratch(), strings, helpers);
}

/// Store the code unit `char` pushes at `l.index` of the output string
/// (block `l.dst`) and advance.
fn put_char(function: &mut FnBuilder<'_>, l: Locals, char: impl Fn(&mut FnBuilder<'_>)) {
    wasm!(function; LocalGet(l.dst), LocalGet(l.index), I32Add);
    char(function);
    wasm!(function;
        I32Store8(m8(abi::STR_DATA_OFFSET)),
        LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.index),
    );
}

/// Store the base64 character (of the alphabet `l.url` selects) of the low
/// six bits of the i32 `x` pushes; `l.v` and `l.c` are temps.
fn put_sextet(function: &mut FnBuilder<'_>, l: Locals, x: impl Fn(&mut FnBuilder<'_>)) {
    x(function);
    wasm!(function;
        I32Const(63), I32And, LocalTee(l.v), I32Const(0x41), I32Add, LocalSet(l.c),
        LocalGet(l.v), I32Const(0x61 - 26), I32Add, LocalGet(l.c),
        LocalGet(l.v), I32Const(26), I32GeU, Select, LocalSet(l.c),
        LocalGet(l.v), I32Const(0x30 - 52), I32Add, LocalGet(l.c),
        LocalGet(l.v), I32Const(52), I32GeU, Select, LocalSet(l.c),
        I32Const(0x2D), I32Const(0x2B), LocalGet(l.url), Select, LocalGet(l.c),
        LocalGet(l.v), I32Const(62), I32Eq, Select, LocalSet(l.c),
        I32Const(0x5F), I32Const(0x2F), LocalGet(l.url), Select, LocalGet(l.c),
        LocalGet(l.v), I32Const(63), I32Eq, Select, LocalSet(l.c),
    );
    put_char(function, l, |f| wasm!(f; LocalGet(l.c)));
}

/// Push byte `l.read + k` of the source.
fn src_byte(function: &mut FnBuilder<'_>, l: Locals, k: u32) {
    wasm!(function; LocalGet(l.s), LocalGet(l.read), I32Add, I32Load8U(m8(k)));
}

/// Encode the `l.len` source bytes at `l.s` as base64 into the output
/// string `l.dst`, padded unless `l.mode` (`omitPadding`).
fn encode_base64(function: &mut FnBuilder<'_>, l: Locals) {
    wasm!(function;
        I32Const(0), LocalSet(l.read), I32Const(0), LocalSet(l.index),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(l.read), I32Const(3), I32Add, LocalGet(l.len), I32GtU, BrIf(1),
    );
    src_byte(function, l, 0);
    wasm!(function; I32Const(16), I32Shl);
    src_byte(function, l, 1);
    wasm!(function; I32Const(8), I32Shl, I32Or);
    src_byte(function, l, 2);
    wasm!(function; I32Or, LocalSet(l.chunk));
    for shift in [18, 12, 6, 0] {
        put_sextet(
            function,
            l,
            |f| wasm!(f; LocalGet(l.chunk), I32Const(shift), I32ShrU),
        );
    }
    wasm!(function;
        LocalGet(l.read), I32Const(3), I32Add, LocalSet(l.read),
        Br(0), End, End,
        // The final one or two bytes.
        LocalGet(l.len), LocalGet(l.read), I32Sub, LocalTee(l.chunk_len), If(BlockType::Empty),
    );
    src_byte(function, l, 0);
    wasm!(function;
        I32Const(16), I32Shl, LocalSet(l.chunk),
        LocalGet(l.chunk_len), I32Const(2), I32Eq, If(BlockType::Empty),
    );
    src_byte(function, l, 1);
    wasm!(function; I32Const(8), I32Shl, LocalGet(l.chunk), I32Or, LocalSet(l.chunk), End);
    for shift in [18, 12] {
        put_sextet(
            function,
            l,
            |f| wasm!(f; LocalGet(l.chunk), I32Const(shift), I32ShrU),
        );
    }
    wasm!(function; LocalGet(l.chunk_len), I32Const(2), I32Eq, If(BlockType::Empty));
    put_sextet(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(6), I32ShrU),
    );
    wasm!(function;
        End,
        LocalGet(l.mode), I32Eqz, If(BlockType::Empty),
        LocalGet(l.chunk_len), I32Const(1), I32Eq, If(BlockType::Empty),
    );
    put_char(function, l, |f| wasm!(f; I32Const(0x3D)));
    wasm!(function; End);
    put_char(function, l, |f| wasm!(f; I32Const(0x3D)));
    wasm!(function; End, End);
}

/// Store the lowercase hex digit of the nibble `d` pushes; `l.v` is the
/// temp.
fn put_hex_digit(function: &mut FnBuilder<'_>, l: Locals, d: impl Fn(&mut FnBuilder<'_>)) {
    d(function);
    wasm!(function; LocalSet(l.v));
    put_char(function, l, |f| {
        wasm!(f;
            LocalGet(l.v), I32Const(0x30), I32Add,
            LocalGet(l.v), I32Const(9), I32GtU, I32Const(0x61 - 0x3A), I32Mul, I32Add,
        );
    });
}

/// Encode the `l.len` source bytes at `l.s` as lowercase hex into the
/// output string `l.dst`.
fn encode_hex(function: &mut FnBuilder<'_>, l: Locals) {
    wasm!(function;
        I32Const(0), LocalSet(l.read), I32Const(0), LocalSet(l.index),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(l.read), LocalGet(l.len), I32GeU, BrIf(1),
    );
    src_byte(function, l, 0);
    wasm!(function; LocalSet(l.chunk));
    put_hex_digit(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(4), I32ShrU),
    );
    put_hex_digit(
        function,
        l,
        |f| wasm!(f; LocalGet(l.chunk), I32Const(15), I32And),
    );
    wasm!(function;
        LocalGet(l.read), I32Const(1), I32Add, LocalSet(l.read),
        Br(0), End, End,
    );
}

/// `Uint8Array.prototype.toBase64(options)` / `toHex()`:
/// `ValidateUint8Array(this)`, the options (base64: `alphabet`, then
/// `omitPadding` by `ToBoolean`), then an out-of-bounds or detached
/// receiver throws, else the encoding of its current bytes (a result past
/// the longest string is a `RangeError`).
fn emit_to(
    op: Base64Op,
    function: &mut FnBuilder<'_>,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let base64 = op == Base64Op::ToBase64;
    let (this, opts, v) = (frame.y64(0), frame.y64(2), frame.y64(3));
    let l = Locals::new(frame);
    this_uint8(function, frame, this, strings, helpers);
    wasm!(function; I32Const(0), LocalSet(l.url), I32Const(0), LocalSet(l.mode));
    if base64 {
        frame.arg(function, 0);
        wasm!(function; LocalSet(opts));
        options_object(function, frame, opts, strings, helpers);
        string_option(
            function,
            frame,
            (opts, v, l.url),
            &ALPHABET,
            strings,
            helpers,
        );
        get_option(function, (opts, v), "omitPadding", strings, helpers);
        let truthy = IrNode::ConvertCall {
            op: ConvertOp::ToBoolean,
            arg: Box::new(IrNode::Local(v)),
            radix: None,
        };
        emit_node(&truthy, function, frame.scratch(), strings, helpers);
        wasm!(function; I64Const(TRUE_BITS), I64Eq, LocalSet(l.mode));
    }
    live_length(function, frame, (this, l.len), strings, helpers);
    if base64 {
        // `4 * floor(n / 3)`, plus `4` (padded) or `rem + 1` (unpadded)
        // for a remainder.
        wasm!(function;
            LocalGet(l.len), I32Const(3), I32DivU, I32Const(2), I32Shl,
            LocalGet(l.len), I32Const(3), I32RemU, LocalTee(l.chunk_len), I32Const(1), I32Add,
            I32Const(4), LocalGet(l.mode), Select,
            I32Const(0), LocalGet(l.chunk_len), Select,
            I32Add, LocalSet(l.max),
        );
    } else {
        wasm!(function; LocalGet(l.len), I32Const(1), I32Shl, LocalSet(l.max));
    }
    wasm!(function;
        LocalGet(l.max), F64ConvertI32U, F64Const(Ieee64::from(MAX_STRING_LENGTH)), F64Gt,
        If(BlockType::Empty),
    );
    super::typed_array::throw_range_error(function, frame, STRING_LENGTH, strings, helpers);
    wasm!(function;
        End,
        LocalGet(l.max), I32Eqz, If(BlockType::Empty),
        I64Const(HEAP_BITS | i64::from(static_key(strings, ""))), Return,
        End,
        LocalGet(l.max), I32Const(0), Call(helpers.new_string), LocalSet(l.dst),
    );
    return_if_pending(function);
    data_address(function, this);
    wasm!(function; LocalSet(l.s));
    if base64 {
        encode_base64(function, l);
    } else {
        encode_hex(function, l);
    }
    wasm!(function; LocalGet(l.dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}
