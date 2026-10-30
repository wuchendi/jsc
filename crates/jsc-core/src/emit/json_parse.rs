//! In-wasm `JSON.parse` — a recursive-descent parser emitted as a family of
//! runtime helpers.
//!
//! # Objects
//!
//! A parsed object is a fresh `TYPE_OBJECT` whose members are interned
//! (`__intern`) and defined (`__define`) in source order, so a literal key
//! access like `parsed.name` finds them by key pointer like any other property.
//!
//! # Cursor state
//!
//! The parse walks the input via three module globals set once at entry:
//! [`JSON_DATA_GLOBAL`] (the data offset, its low bit set for a two-byte
//! input — the offset itself is 8-aligned), [`JSON_POS_GLOBAL`] (the current
//! code-unit index), and [`JSON_LEN_GLOBAL`] (the code-unit length). The helpers
//! recurse (`__json_value` → `__json_array`/`__json_object` → `__json_value`),
//! reading/advancing these globals. A parse runs synchronously to completion, so
//! nothing re-enters and disturbs them.
//!
//! # Errors
//!
//! Malformed input calls `__json_throw`, which
//! stashes an `Error` in `$exc_value`, sets `$exc_pending`, and unwinds via
//! the exception-unwinding mechanism. Each helper checks `$exc_pending` after a recursive call and
//! propagates; the emit-context [`emit_json_parse_node`] issues the final
//! post-call `emit_propagate_if_pending` so a `try`/`catch` around `JSON.parse`
//! catches it.
//!
//! # v1 cuts (documented)
//!
//! String bodies and `\uXXXX` escapes (lone surrogates included) decode to
//! code units; each parsed string over-allocates its worst-case decoded
//! length as a two-byte block on the bump heap (never reclaimed, §10), then
//! narrows to one-byte when every unit is `<= 0xFF`. No `ToPrimitive`/reviver.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The i32 `MemArg` for a single-byte load/store at a computed address.
fn byte_mem() -> MemArg {
    MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }
}

/// The u32 length-header `MemArg` (`+STR_LEN_OFFSET` from a string block base).
fn len_header() -> MemArg {
    MemArg {
        offset: STR_LEN_OFFSET,
        align: 2,
        memory_index: 0,
    }
}

/// Push the input's code unit at the index `pos` pushes, read at the input's
/// width (the low bit of `$__json_data`). The caller must have established
/// the index is in range.
fn unit_at(function: &mut Function, pos: impl Fn(&mut Function)) {
    function.instruction(&Instruction::GlobalGet(JSON_DATA_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::GlobalGet(JSON_DATA_GLOBAL));
    function.instruction(&Instruction::I32Const(-2));
    function.instruction(&Instruction::I32And);
    pos(function);
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load16U(MemArg {
        offset: 0,
        align: 1,
        memory_index: 0,
    }));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::GlobalGet(JSON_DATA_GLOBAL));
    pos(function);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte_mem()));
    function.instruction(&Instruction::End);
}

/// Push the code unit at the cursor. The caller must have established the
/// position is in range.
fn cur_unit(function: &mut Function) {
    unit_at(function, |f| {
        f.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    });
}

/// Push `1` if the cursor is at/past the end (`$__json_pos >= $__json_len`).
fn at_end(function: &mut Function) {
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::GlobalGet(JSON_LEN_GLOBAL));
    function.instruction(&Instruction::I32GeU);
}

/// Emit `$__json_pos += 1`.
fn advance(function: &mut Function) {
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::GlobalSet(JSON_POS_GLOBAL));
}

/// Emit `__json_throw()` then `return` — abort the current helper with a syntax
/// error in flight. `__json_throw` leaves an `i64` dummy on the stack that the
/// `return` yields; the caller's `$exc_pending` check then propagates it. Valid
/// only in helpers that return `i64`.
fn throw_return(function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::Call(helpers.json_throw));
    function.instruction(&Instruction::Return);
}

/// After a recursive `__json_*` call, propagate an in-flight exception by
/// returning the pending `undefined` dummy (the caller re-checks and unwinds one
/// frame further). Valid only in helpers that return `i64`.
fn propagate_if_pending(function: &mut Function) {
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
}

/// Push the boxed heap jsval of the i32 block base in `local`.
fn box_local(function: &mut Function, local: u32) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
}

/// Verify the next `word.len()` bytes at the cursor equal `word`, advance past
/// them, and push `result_bits`; throw + return on any mismatch or out-of-range.
/// Used for the `true`/`false`/`null` literals. Leaves an `i64` (the literal's
/// jsval).
fn expect_word(function: &mut Function, word: &[u8], result_bits: i64, helpers: Helpers<'_>) {
    let n = i32::try_from(word.len()).unwrap_or(i32::MAX);
    // if $__json_pos + n > $__json_len: throw.
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::I32Const(n));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::GlobalGet(JSON_LEN_GLOBAL));
    function.instruction(&Instruction::I32GtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(function, helpers);
    function.instruction(&Instruction::End);
    // Verify each unit.
    for (i, byte) in word.iter().enumerate() {
        unit_at(function, |f| {
            f.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
            f.instruction(&Instruction::I32Const(i32::try_from(i).unwrap_or(i32::MAX)));
            f.instruction(&Instruction::I32Add);
        });
        function.instruction(&Instruction::I32Const(i32::from(*byte)));
        function.instruction(&Instruction::I32Ne);
        function.instruction(&Instruction::If(BlockType::Empty));
        throw_return(function, helpers);
        function.instruction(&Instruction::End);
    }
    // Advance past the word and yield its jsval.
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::I32Const(n));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::GlobalSet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::I64Const(result_bits));
}

/// `__json_ws()`: advance the cursor past JSON whitespace (space/tab/LF/CR).
pub(super) fn emit_json_ws() -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let c = 0;
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    at_end(&mut function);
    function.instruction(&Instruction::BrIf(1));
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));
    // is_ws = c==32 | c==9 | c==10 | c==13.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(32));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(9));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(10));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(13));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::I32Or);
    // if !is_ws: break.
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::BrIf(1));
    advance(&mut function);
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function
}

/// `__json_throw() -> i64`: stash a `SyntaxError` in `$exc_value`, set the
/// pending flag + clear any routed completion, and return a dummy `undefined`.
pub(super) fn emit_json_throw(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    // The full `emit_main` scratch-local layout (starting at local 0, since this
    // helper has no params) so `emit_node` can build the error object exactly as
    // it would in `main`.
    let mut function = Function::new(vec![
        (3, ValType::I64),
        (1, ValType::I32),
        (3, ValType::I64),
        (1, ValType::I32),
        (5, ValType::I32),
        (1, ValType::I64),
        (7, ValType::I64),
        (1, ValType::I32),
        (2, ValType::I64),
    ]);
    let scratch = 0;
    emit_node(
        &json_syntax_error(),
        &mut function,
        scratch,
        strings,
        helpers,
    );
    function.instruction(&Instruction::GlobalSet(EXC_VALUE_GLOBAL));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::GlobalSet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(CF_KIND_GLOBAL));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function
}

/// `__json_string() -> i64`: parse a JSON string token (the cursor is at the
/// opening `"`) into a fresh heap string of code units, handling the simple
/// escapes `\" \\ \/ \b \f \n \r \t` and `\uXXXX` (any unit, a lone
/// surrogate included; a pair is two escapes). The units are decoded into a
/// two-byte block, narrowed to one-byte when every unit is `<= 0xFF`. A
/// malformed escape or an unterminated string throws.
pub(super) fn emit_json_string(alloc_index: u32, helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![(7, ValType::I32)]);
    let (cap, str_base, wp, c, e, out, d) = (0, 1, 2, 3, 4, 5, 6);

    // Consume the opening quote.
    advance(&mut function);
    // cap = len - pos (an upper bound on the decoded length); over-allocate a
    // two-byte block. `wp` (the write index) starts at 0.
    function.instruction(&Instruction::GlobalGet(JSON_LEN_GLOBAL));
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(cap));
    emit_alloc_str(
        &mut function,
        alloc_index,
        |f| {
            f.instruction(&Instruction::LocalGet(cap));
        },
        |f| {
            f.instruction(&Instruction::I32Const(1));
        },
        str_base,
    );

    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if at_end: unterminated string -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    // c = cur_unit; pos++.
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));
    advance(&mut function);
    // if c == '"' (34): closing quote consumed -> break.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(34));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::BrIf(1));
    // if c == '\\' (92): decode an escape into c.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(92));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // e = cur_unit; pos++ (the escape char). Out-of-range -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(e));
    advance(&mut function);
    // `\u`: four hex digits; any other escape maps to its unit or throws.
    function.instruction(&Instruction::LocalGet(e));
    function.instruction(&Instruction::I32Const(i32::from(b'u')));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_hex4(&mut function, out, d, helpers);
    function.instruction(&Instruction::Else);
    emit_escape_byte(&mut function, e, helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalSet(c));
    function.instruction(&Instruction::End);
    // store16(str_base + DATA + 2*wp, c); wp++.
    function.instruction(&Instruction::LocalGet(str_base));
    function.instruction(&Instruction::LocalGet(wp));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Store16(MemArg {
        offset: u64::from(abi::STR_DATA_OFFSET),
        align: 1,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(wp));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(wp));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    // Write the decoded unit count as the u32 length header, then narrow.
    function.instruction(&Instruction::LocalGet(str_base));
    function.instruction(&Instruction::LocalGet(wp));
    function.instruction(&Instruction::I32Store(len_header()));
    emit_str_narrow(&mut function, str_base, c, d);
    // Box and return.
    box_local(&mut function, str_base);
    function.instruction(&Instruction::End);
    function
}

/// Decode the four hex digits of a `\uXXXX` escape at the cursor (advancing
/// past them), pushing the code unit; throw on a missing or non-hex digit.
/// `out` and `d` are i32 scratch.
fn emit_hex4(function: &mut Function, out: u32, d: u32, helpers: Helpers<'_>) {
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(out));
    for _ in 0..4 {
        at_end(function);
        function.instruction(&Instruction::If(BlockType::Empty));
        throw_return(function, helpers);
        function.instruction(&Instruction::End);
        cur_unit(function);
        function.instruction(&Instruction::LocalSet(d));
        advance(function);
        // d = d - '0' when a decimal digit, else (d | 0x20) - 'a' + 10 when
        // `a`-`f` / `A`-`F`, else throw.
        function.instruction(&Instruction::LocalGet(d));
        function.instruction(&Instruction::I32Const(i32::from(b'0')));
        function.instruction(&Instruction::I32Sub);
        function.instruction(&Instruction::I32Const(10));
        function.instruction(&Instruction::I32LtU);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        function.instruction(&Instruction::LocalGet(d));
        function.instruction(&Instruction::I32Const(i32::from(b'0')));
        function.instruction(&Instruction::I32Sub);
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::LocalGet(d));
        function.instruction(&Instruction::I32Const(0x20));
        function.instruction(&Instruction::I32Or);
        function.instruction(&Instruction::I32Const(i32::from(b'a')));
        function.instruction(&Instruction::I32Sub);
        function.instruction(&Instruction::LocalTee(d));
        function.instruction(&Instruction::I32Const(6));
        function.instruction(&Instruction::I32GeU);
        function.instruction(&Instruction::If(BlockType::Empty));
        throw_return(function, helpers);
        function.instruction(&Instruction::End);
        function.instruction(&Instruction::LocalGet(d));
        function.instruction(&Instruction::I32Const(10));
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::End);
        // out = out << 4 | digit.
        function.instruction(&Instruction::LocalGet(out));
        function.instruction(&Instruction::I32Const(4));
        function.instruction(&Instruction::I32Shl);
        function.instruction(&Instruction::I32Or);
        function.instruction(&Instruction::LocalSet(out));
    }
    function.instruction(&Instruction::LocalGet(out));
}

/// Map a simple JSON escape char (in i32 local `e`) to its decoded unit, pushing
/// it as an i32; throw on any other escape. Leaves one i32.
fn emit_escape_byte(function: &mut Function, e: u32, helpers: Helpers<'_>) {
    // A chain of `e == <escape> ? <byte> : …`, throwing in the final else.
    let cases: [(i32, i32); 8] = [
        (34, 34),  // \"  -> "
        (92, 92),  // \\  -> \
        (47, 47),  // \/  -> /
        (98, 8),   // \b  -> backspace
        (102, 12), // \f  -> form feed
        (110, 10), // \n  -> LF
        (114, 13), // \r  -> CR
        (116, 9),  // \t  -> tab
    ];
    for (escape, byte) in cases {
        function.instruction(&Instruction::LocalGet(e));
        function.instruction(&Instruction::I32Const(escape));
        function.instruction(&Instruction::I32Eq);
        function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        function.instruction(&Instruction::I32Const(byte));
        function.instruction(&Instruction::Else);
    }
    // Any other escape: throw. After the return this branch is unreachable, so
    // it satisfies the `i32` result type polymorphically.
    throw_return(function, helpers);
    for _ in cases {
        function.instruction(&Instruction::End);
    }
}

/// `__json_number() -> i64`: scan a JSON number token at the cursor into a
/// substring, then coerce it via the `__convert` `ToNumber` import (at
/// `convert_index`). A token with no number chars throws.
pub(super) fn emit_json_number(
    alloc_index: u32,
    convert_index: u32,
    helpers: Helpers<'_>,
) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (start, len2, str_base, c) = (0, 1, 2, 3);

    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::LocalSet(start));
    // Advance while the byte is a number char: [0-9] '-' '+' '.' 'e' 'E'.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    at_end(&mut function);
    function.instruction(&Instruction::BrIf(1));
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));
    // in_set = (c>=48 & c<=57) | c==45 | c==43 | c==46 | c==101 | c==69.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(48));
    function.instruction(&Instruction::I32GeS);
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(57));
    function.instruction(&Instruction::I32LeS);
    function.instruction(&Instruction::I32And);
    for byte in [45, 43, 46, 101, 69] {
        function.instruction(&Instruction::LocalGet(c));
        function.instruction(&Instruction::I32Const(byte));
        function.instruction(&Instruction::I32Eq);
        function.instruction(&Instruction::I32Or);
    }
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::BrIf(1));
    advance(&mut function);
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    // if pos == start: no number chars -> throw.
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::LocalGet(start));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);

    // len2 = pos - start; str_base = __alloc(STR_DATA_OFFSET+len2); copy the
    // (ASCII) token units as bytes.
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::LocalGet(start));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(len2));
    function.instruction(&Instruction::I32Const(STR_DATA_OFFSET));
    function.instruction(&Instruction::LocalGet(len2));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(str_base));
    function.instruction(&Instruction::LocalGet(str_base));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Store8(byte_mem()));
    function.instruction(&Instruction::LocalGet(str_base));
    function.instruction(&Instruction::LocalGet(len2));
    function.instruction(&Instruction::I32Store(len_header()));
    // for c in 0..len2: store8(str_base + STR_DATA_OFFSET + c, unit(start + c)).
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(c));
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::LocalGet(len2));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(str_base));
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Add);
    unit_at(&mut function, |f| {
        f.instruction(&Instruction::LocalGet(start));
        f.instruction(&Instruction::LocalGet(c));
        f.instruction(&Instruction::I32Add);
    });
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: u64::from(abi::STR_DATA_OFFSET),
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(c));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // ToNumber(boxed str).
    box_local(&mut function, str_base);
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_NUMBER));
    function.instruction(&Instruction::Call(convert_index));
    function.instruction(&Instruction::End);
    function
}

/// `__json_value() -> i64`: skip whitespace, then dispatch on the current byte to
/// the object / array / string / literal / number parser.
pub(super) fn emit_json_value(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let c = 0;
    function.instruction(&Instruction::Call(helpers.json_ws));
    // if at_end: unexpected end of input -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));

    // '{' -> object.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(123));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::Call(helpers.json_object));
    function.instruction(&Instruction::Else);
    // '[' -> array.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(91));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::Call(helpers.json_array));
    function.instruction(&Instruction::Else);
    // '"' -> string.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(34));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::Call(helpers.json_string));
    function.instruction(&Instruction::Else);
    // 't' -> true.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(116));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    expect_word(&mut function, b"true", TRUE_BITS, helpers);
    function.instruction(&Instruction::Else);
    // 'f' -> false.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(102));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    expect_word(&mut function, b"false", FALSE_BITS, helpers);
    function.instruction(&Instruction::Else);
    // 'n' -> null.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(110));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    expect_word(&mut function, b"null", NULL_BITS, helpers);
    function.instruction(&Instruction::Else);
    // Otherwise a number (digit / '-').
    function.instruction(&Instruction::Call(helpers.json_number));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function
}

/// `__json_array() -> i64`: parse a JSON array (cursor at `[`) into a fresh heap
/// array, growing it element by element via `__array_push`.
pub(super) fn emit_json_array(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![
        (1, ValType::I32),
        (1, ValType::I64),
        (1, ValType::I32),
    ]);
    let (arr, elem, c) = (0, 1, 2);

    advance(&mut function); // consume '['
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Call(helpers.new_array()));
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::Call(helpers.json_ws));
    // Empty array: if next is ']', consume it and return the empty array.
    at_end(&mut function);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    cur_unit(&mut function);
    function.instruction(&Instruction::I32Const(93));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    advance(&mut function);
    box_local(&mut function, arr);
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // element = json_value(); propagate a throw.
    function.instruction(&Instruction::Call(helpers.json_value));
    function.instruction(&Instruction::LocalSet(elem));
    propagate_if_pending(&mut function);
    // __array_push(arr, element).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::Call(helpers.array_push));
    function.instruction(&Instruction::Drop);
    function.instruction(&Instruction::Call(helpers.json_ws));
    // if at_end: unterminated -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    // c = cur_unit; pos++.
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));
    advance(&mut function);
    // if c == ']' (93): break.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(93));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::BrIf(1));
    // if c != ',' (44): unexpected -> throw.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(44));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    box_local(&mut function, arr);
    function.instruction(&Instruction::End);
    function
}

/// `__json_object() -> i64`: parse a JSON object (cursor at `{`) into a fresh
/// `TYPE_OBJECT`, defining each member (key interned) through `__define` in
/// source order — a duplicate key overwrites the earlier slot in place.
pub(super) fn emit_json_object(alloc_index: u32, helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![
        (1, ValType::I32),
        (2, ValType::I64),
        (1, ValType::I32),
    ]);
    let (obj, k, v, c) = (0, 1, 2, 3);

    advance(&mut function); // consume '{'
    function.instruction(&Instruction::I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()));
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(obj));
    emit_object_prefix(
        &mut function,
        obj,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    function.instruction(&Instruction::Call(helpers.json_ws));
    // Empty object: if next is '}', consume it and skip the member loop.
    at_end(&mut function);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    cur_unit(&mut function);
    function.instruction(&Instruction::I32Const(125));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::If(BlockType::Empty));
    advance(&mut function);
    function.instruction(&Instruction::Else);
    // Member loop.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::Call(helpers.json_ws));
    // if at_end: unterminated -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    // Key must be a string: if cur != '"' -> throw.
    cur_unit(&mut function);
    function.instruction(&Instruction::I32Const(34));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    // k = json_string(); propagate a throw.
    function.instruction(&Instruction::Call(helpers.json_string));
    function.instruction(&Instruction::LocalSet(k));
    propagate_if_pending(&mut function);
    function.instruction(&Instruction::Call(helpers.json_ws));
    // Expect ':' -> throw if at_end or not ':'.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    cur_unit(&mut function);
    function.instruction(&Instruction::I32Const(58));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    advance(&mut function); // consume ':'
    // v = json_value(); propagate a throw.
    function.instruction(&Instruction::Call(helpers.json_value));
    function.instruction(&Instruction::LocalSet(v));
    propagate_if_pending(&mut function);
    // __define(obj, __intern(k), v, ATTRS_DEFAULT).
    function.instruction(&Instruction::LocalGet(obj));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(helpers.intern));
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32Const(abi::ATTRS_DEFAULT.cast_signed()));
    function.instruction(&Instruction::Call(helpers.define));
    function.instruction(&Instruction::Call(helpers.json_ws));
    // if at_end: unterminated -> throw.
    at_end(&mut function);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    // c = cur_unit; pos++.
    cur_unit(&mut function);
    function.instruction(&Instruction::LocalSet(c));
    advance(&mut function);
    // if c == '}' (125): break.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(125));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::BrIf(1));
    // if c != ',' (44) -> throw.
    function.instruction(&Instruction::LocalGet(c));
    function.instruction(&Instruction::I32Const(44));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    function.instruction(&Instruction::LocalGet(obj));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::End);
    function
}

/// `__json_parse(s: i64) -> i64`: the parse entry. `s` is a heap string; set the
/// cursor globals, parse one value, require no trailing content, and return the
/// value (or leave a syntax error in flight).
pub(super) fn emit_json_parse(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32), (1, ValType::I64)]);
    let (s, base, result) = (0, 1, 2);

    // base = wrap(s); DATA = (base + STR_DATA_OFFSET) | two_byte;
    // LEN = load_u32(base+STR_LEN_OFFSET); POS = 0.
    function.instruction(&Instruction::LocalGet(s));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Const(STR_DATA_OFFSET));
    function.instruction(&Instruction::I32Add);
    emit_str_two_byte(&mut function, base);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::GlobalSet(JSON_DATA_GLOBAL));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(len_header()));
    function.instruction(&Instruction::GlobalSet(JSON_LEN_GLOBAL));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::GlobalSet(JSON_POS_GLOBAL));

    // result = json_value(); propagate a throw.
    function.instruction(&Instruction::Call(helpers.json_value));
    function.instruction(&Instruction::LocalSet(result));
    propagate_if_pending(&mut function);
    // Skip trailing whitespace; any remaining content is a syntax error.
    function.instruction(&Instruction::Call(helpers.json_ws));
    function.instruction(&Instruction::GlobalGet(JSON_POS_GLOBAL));
    function.instruction(&Instruction::GlobalGet(JSON_LEN_GLOBAL));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    throw_return(&mut function, helpers);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(result));
    function.instruction(&Instruction::End);
    function
}

/// Emit a `JSON.parse(arg)` node (the emit-context dispatch): coerce `arg` with
/// `ToString` (`__to_string`: an object's user `toString` / `valueOf` may run,
/// a symbol throws), call `__json_parse`, then a post-call
/// `emit_propagate_if_pending` so a `try`/`catch` around the call catches a
/// conversion or syntax error.
pub(super) fn emit_json_parse_node(
    arg: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_node(arg, function, scratch, strings, helpers);
    function.instruction(&Instruction::Call(helpers.to_str()));
    emit_propagate_if_pending(function, helpers);
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::Call(helpers.json_parse));
    emit_propagate_if_pending(function, helpers);
}
