//! `Symbol`, `Symbol.for` / `Symbol.keyFor` and the `%Symbol.prototype%`
//! methods (DESIGN.md §7.9): a symbol is a fresh
//! `TYPE_SYMBOL` block; registered symbols live in the hidden realm registry
//! block, keyed by their interned key string.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::emit::realm::SymbolOp;

/// `new Symbol()`: `Symbol` has `[[Construct]]` only for `IsConstructor`.
const NOT_A_CONSTRUCTOR: CalleeErrorText<'static> = [
    "Symbol is not a constructor",
    "TypeError: Symbol is not a constructor",
];

/// `Symbol.keyFor` of a non-symbol.
const KEY_FOR_NOT_SYMBOL: CalleeErrorText<'static> = [
    "Symbol.keyFor argument is not a symbol",
    "TypeError: Symbol.keyFor argument is not a symbol",
];

/// `thisSymbolValue` failures.
const NOT_SYMBOL_TO_STRING: CalleeErrorText<'static> = [
    "Symbol.prototype.toString requires that 'this' be a Symbol",
    "TypeError: Symbol.prototype.toString requires that 'this' be a Symbol",
];
const NOT_SYMBOL_VALUE_OF: CalleeErrorText<'static> = [
    "Symbol.prototype.valueOf requires that 'this' be a Symbol",
    "TypeError: Symbol.prototype.valueOf requires that 'this' be a Symbol",
];
const NOT_SYMBOL_DESCRIPTION: CalleeErrorText<'static> = [
    "Symbol.prototype.description requires that 'this' be a Symbol",
    "TypeError: Symbol.prototype.description requires that 'this' be a Symbol",
];
const NOT_SYMBOL_TO_PRIMITIVE: CalleeErrorText<'static> = [
    "Symbol.prototype [ @@toPrimitive ] requires that 'this' be a Symbol",
    "TypeError: Symbol.prototype [ @@toPrimitive ] requires that 'this' be a Symbol",
];

/// The pieces of `SymbolDescriptiveString`.
const OPEN: &str = "Symbol(";
const CLOSE: &str = ")";
const EMPTY: &str = "Symbol()";

/// Every static string the bodies reference.
pub(super) const STRINGS: [&str; 15] = [
    NOT_A_CONSTRUCTOR[0],
    NOT_A_CONSTRUCTOR[1],
    KEY_FOR_NOT_SYMBOL[0],
    KEY_FOR_NOT_SYMBOL[1],
    NOT_SYMBOL_TO_STRING[0],
    NOT_SYMBOL_TO_STRING[1],
    NOT_SYMBOL_VALUE_OF[0],
    NOT_SYMBOL_VALUE_OF[1],
    NOT_SYMBOL_DESCRIPTION[0],
    NOT_SYMBOL_DESCRIPTION[1],
    NOT_SYMBOL_TO_PRIMITIVE[0],
    NOT_SYMBOL_TO_PRIMITIVE[1],
    OPEN,
    CLOSE,
    EMPTY,
];

/// Push i32 `1` when the jsval in `local` is a symbol.
fn is_symbol(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    wasm!(function; I32Const(TYPE_SYMBOL), I32Eq, Else, I32Const(0), End);
}

/// Push the jsval of a fresh symbol block with header `flags` and the
/// description jsval in i64 `description` (i32 `block` is a temp).
fn new_symbol(
    function: &mut Function,
    flags: u8,
    description: u32,
    block: u32,
    helpers: Helpers<'_>,
) {
    let header = i32::from(abi::TYPE_SYMBOL) | (i32::from(flags) << 16);
    wasm!(function;
        I32Const(abi::SYMBOL_BLOCK_SIZE.cast_signed()), Call(helpers.alloc), LocalTee(block),
        I32Const(header), I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }),
        LocalGet(block), I32Const(0), I32Store(MemArg { offset: 4, align: 2, memory_index: 0 }),
        LocalGet(block), LocalGet(description),
        I64Store(MemArg { offset: u64::from(abi::SYMBOL_DESCRIPTION_OFFSET), align: 3, memory_index: 0 }),
        LocalGet(block), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
    );
}

/// Push the description jsval of the symbol in i64 `symbol`.
fn load_description(function: &mut Function, symbol: u32) {
    wasm!(function;
        LocalGet(symbol), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::SYMBOL_DESCRIPTION_OFFSET), align: 3, memory_index: 0 }),
    );
}

/// `Symbol(description)`: a fresh symbol whose description is `undefined`
/// or `ToString(description)`; `new Symbol()` throws.
pub(super) fn emit_ctor(
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (description, block) = (frame.i64(0), frame.i32(0));
    emit_is_construct_call(function, frame.argc());
    wasm!(function; If(BlockType::Empty));
    throw_type_error(function, frame, NOT_A_CONSTRUCTOR, strings, helpers);
    wasm!(function; End);
    frame.arg(function, 0);
    wasm!(function;
        LocalTee(description), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
        LocalGet(description), Call(helpers.to_str()), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        LocalSet(description),
    );
    return_if_pending(function);
    wasm!(function; End);
    new_symbol(function, 0, description, block, helpers);
}

/// Emit the body of the `Symbol` static or `%Symbol.prototype%` builtin `op`.
pub(super) fn emit(
    op: SymbolOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    match op {
        SymbolOp::For => emit_for(function, frame, helpers),
        SymbolOp::KeyFor => emit_key_for(function, frame, strings, helpers),
        _ => emit_method(op, function, frame, strings, helpers),
    }
}

/// `Symbol.for(key)`: the registered symbol of `ToString(key)` (interned),
/// created with `REGISTERED` and that key as its description on a miss.
fn emit_for(function: &mut Function, frame: Frame, helpers: Helpers<'_>) {
    let (description, key, entry, block) = (frame.i64(0), frame.i32(0), frame.i32(1), frame.i32(2));
    let registry = intrinsic_bits(realm::Intrinsic::SymbolRegistry, helpers);
    frame.arg(function, 0);
    wasm!(function; Call(helpers.to_str()), LocalSet(key));
    return_if_pending(function);
    wasm!(function;
        LocalGet(key), Call(helpers.intern), LocalSet(key),
        I64Const(registry), I32WrapI64, LocalGet(key), Call(helpers.prop_find), LocalTee(entry),
        If(BlockType::Empty),
        LocalGet(entry),
        I64Load(MemArg { offset: u64::from(abi::PROP_VALUE_OFFSET), align: 3, memory_index: 0 }),
        Return,
        End,
        LocalGet(key), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, LocalSet(description),
    );
    new_symbol(
        function,
        abi::SYMBOL_FLAG_REGISTERED,
        description,
        block,
        helpers,
    );
    wasm!(function;
        LocalSet(description),
        I64Const(registry), I32WrapI64, LocalGet(key), LocalGet(description), I32Const(0),
        Call(helpers.define),
        LocalGet(description),
    );
}

/// `Symbol.keyFor(sym)`: the key of a registered symbol, `undefined` for any
/// other symbol, a `TypeError` for a non-symbol.
fn emit_key_for(function: &mut Function, frame: Frame, strings: &StrOffsets, helpers: Helpers<'_>) {
    let symbol = frame.i64(0);
    frame.arg(function, 0);
    wasm!(function; LocalSet(symbol));
    is_symbol(function, symbol);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    throw_type_error(function, frame, KEY_FOR_NOT_SYMBOL, strings, helpers);
    wasm!(function;
        End,
        LocalGet(symbol), I32WrapI64, I32Load8U(MemArg { offset: u64::from(abi::HEADER_FLAGS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(i32::from(abi::SYMBOL_FLAG_REGISTERED)), I32And,
        If(BlockType::Result(ValType::I64)),
    );
    load_description(function, symbol);
    wasm!(function; Else, I64Const(UNDEFINED_BITS), End);
}

/// A `%Symbol.prototype%` method over `thisSymbolValue(this)` (a symbol or a
/// `CLASS_SYMBOL` wrapper's `[[SymbolData]]`, else a `TypeError`):
/// `toString` (`SymbolDescriptiveString`), `valueOf` / `[@@toPrimitive]`
/// (the symbol) or the `description` getter.
fn emit_method(
    op: SymbolOp,
    function: &mut Function,
    frame: Frame,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let (symbol, description) = (frame.i64(0), frame.i64(1));
    let text = match op {
        SymbolOp::ToString => NOT_SYMBOL_TO_STRING,
        SymbolOp::Description => NOT_SYMBOL_DESCRIPTION,
        SymbolOp::ToPrimitive => NOT_SYMBOL_TO_PRIMITIVE,
        _ => NOT_SYMBOL_VALUE_OF,
    };
    this_primitive(
        function,
        frame,
        symbol,
        abi::CLASS_SYMBOL,
        is_symbol,
        text,
        strings,
        helpers,
    );
    match op {
        SymbolOp::ToString => {
            load_description(function, symbol);
            wasm!(function;
                LocalTee(description), I64Const(UNDEFINED_BITS), I64Eq,
                If(BlockType::Result(ValType::I64)),
            );
            push_str(function, strings, EMPTY);
            wasm!(function;
                Else,
                I32Const(static_key(strings, OPEN)), LocalGet(description), I32WrapI64,
                Call(helpers.concat),
                I32Const(static_key(strings, CLOSE)), Call(helpers.concat),
                I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
                End,
            );
        }
        SymbolOp::Description => load_description(function, symbol),
        _ => wasm!(function; LocalGet(symbol)),
    }
}
