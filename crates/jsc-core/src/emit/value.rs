#[allow(clippy::wildcard_imports)]
use super::*;

/// Push `1` if the jsval in `local` is a number, else `0`. A number is any value
/// without the full QNAN mask set, plus the canonical number NaN (`bits ==
/// QNAN`); every boxed non-number (boolean/null/undefined/type-string) has the
/// QNAN mask set with a nonzero tag and so is excluded.
pub(super) fn emit_is_number(function: &mut Function, local: u32) {
    // (bits & QNAN) != QNAN
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Ne);
    // | (bits == QNAN)  — the canonical number NaN
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I32Or);
}

/// Push `1` if the jsval in `local` is `null` or `undefined`, else `0`. Both are
/// canonical immediate bit-patterns ([`NULL_BITS`] / [`UNDEFINED_BITS`]), so this
/// is a pair of exact `i64` compares — no pointer/type-byte read. Used by loose
/// equality (`==`), where `null` and `undefined` are mutually equal and unequal
/// to everything else.
pub(super) fn emit_is_nullish(function: &mut Function, local: u32) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I32Or);
}

/// Push `1` if the jsval in `local` is a heap-string pointer, else `0`. A pointer
/// is a boxed value (full QNAN mask set) whose NaN-box tag is `0b101`. The QNAN
/// check is essential: a plain `f64` number can coincidentally carry tag bits
/// `0b101`, so a tag-only test would misread it as a pointer and read memory at a
/// bogus offset. Callers rely on this to gate `__str_eq` / `__str_cmp` and the
/// string-length load behind an `if`.
pub(super) fn emit_is_pointer(function: &mut Function, local: u32) {
    // is_boxed = (bits & QNAN) == QNAN
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::I64Eq);
    // & (tag == 0b101)
    emit_tag_eq(function, local, 0b101);
    function.instruction(&Instruction::I32And);
}

/// Push the u8 TYPE BYTE stored at the base of the heap block the jsval in `local`
/// points at (S13.1). The value MUST be a heap pointer — the caller is responsible
/// for gating this behind an [`emit_is_pointer`] check (a non-pointer's low bits
/// are not a valid heap offset). Leaves the type byte as an `i32` on the stack.
pub(super) fn emit_load_type_byte(function: &mut Function, local: u32) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

/// Push `1` if the jsval in `local` is a heap STRING, else `0` (S13.1). Because
/// all heap kinds now share the one pointer tag `0b101`, "is a string" is
/// `is_pointer(local) & (type_byte == TYPE_STRING)`. The type-byte memory read is
/// gated behind an `if` on [`emit_is_pointer`] so it never runs for a non-pointer
/// (whose wrapped low bits would be a bogus offset). Replaces the pre-refactor
/// tag-only string test used by `+`, the relational operators, and `===`.
pub(super) fn emit_is_string(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push `1` if the jsval in `local` is a heap OBJECT (type byte `2`), else `0`.
/// Like [`emit_is_string`], this is `is_pointer(local) & (type_byte ==
/// TYPE_OBJECT)`, with the type-byte read gated behind an `if` on
/// [`emit_is_pointer`] so it never runs for a non-pointer (whose wrapped low
/// bits would be a bogus offset). Used by the `instanceof` error-identity test,
/// whose operand may be any value (a non-object is simply not an instance).
pub(super) fn emit_is_object(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push `1` if the jsval in `local` is a heap FUNCTION value, else `0` (P3.4).
/// Like [`emit_is_string`], this is `is_pointer(local) & (type_byte ==
/// TYPE_FUNCTION)`, with the type-byte read gated behind an `if` on
/// [`emit_is_pointer`] so it never runs for a non-pointer. Used by `JSON.stringify`
/// to omit function values (as object values) or render them as `null` (as array
/// elements / at the top level).
pub(super) fn emit_is_function(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_FUNCTION));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push `1` if the jsval in `local` is a heap GENERATOR handle (type byte `9`),
/// else `0` (P4.2 M2). Same shape as [`emit_is_function`], guarding the type-byte
/// read behind [`emit_is_pointer`]. Used by `gen.next(v)` to reject a
/// non-generator receiver with a `TypeError`.
pub(super) fn emit_is_generator(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_GENERATOR));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}

/// Push `1` if the jsval in `local` is an ACCESSOR box (type byte `11`), else `0`
/// (P2b instance getters/setters). Same shape as [`emit_is_function`], guarding
/// the type-byte read behind [`emit_is_pointer`]. Used by the member read/write
/// paths to detect that a property resolved (through the prototype chain) to an
/// accessor and must be dispatched as a getter/setter call rather than a plain
/// data value. The operand may be ANY resolved value (a number, `undefined`, an
/// object, …), so the pointer gate is essential.
pub(super) fn emit_is_accessor(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, local);
    function.instruction(&Instruction::I32Const(TYPE_ACCESSOR));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
}
