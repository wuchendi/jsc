#[allow(clippy::wildcard_imports)]
use super::*;

/// Write the object-family prefix of the block whose base is in i32 local
/// `base`: one i64 store at offset 0 lays down the header word (type byte
/// `type_byte`, zero `gc`/`flags`/`class` bytes) together with a zero `props`
/// slot (at [`abi::OBJ_PROPS_OFFSET`], no own properties), then the `proto`
/// slot (at [`OBJ_PROTO_OFFSET`]) gets the jsval `proto` (`null` or an
/// intrinsic). Stack neutral.
pub(super) fn emit_object_prefix(function: &mut Function, base: u32, type_byte: i32, proto: i64) {
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Const(i64::from(type_byte)));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Const(proto));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: OBJ_PROTO_OFFSET,
        align: 3,
        memory_index: 0,
    }));
}

/// Rebox the `f64` on top of the stack as a number jsval (via `__box_f64`).
/// Every site that turns an `f64` (arithmetic, `Math.*`, a stored `Date` payload,
/// a host-imported clock) back into a jsval goes through here, so the number NaN
/// is ALWAYS exactly the canonical [`abi::QNAN`] bits: wasm arithmetic may yield
/// any NaN (`0/0` sets the sign bit on x86; a NaN operand such as `undefined`
/// propagates its tag payload), and a raw reinterpret would leak those bits as a
/// different value. Integer conversions (`f64.convert_i32_u`, ...) can never be
/// NaN and reinterpret directly.
pub(super) fn emit_box_f64(function: &mut Function, helpers: Helpers<'_>) {
    function.instruction(&Instruction::Call(helpers.box_f64));
}

/// Emit the `__box_f64(x: f64) -> i64` helper backing [`emit_box_f64`]:
/// `x == x ? reinterpret(x) : QNAN`.
pub(super) fn emit_box_f64_helper() -> Function {
    let mut function = Function::new(vec![]);
    function.instruction(&Instruction::LocalGet(0));
    function.instruction(&Instruction::I64ReinterpretF64);
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::LocalGet(0));
    function.instruction(&Instruction::LocalGet(0));
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::End);
    function
}

/// Replace the jsval in `local` with its JS `ToNumber` (a number jsval). A number
/// is left untouched (the fast path is one compare); anything else goes through
/// the shared `__to_number` helper, whose `valueOf`/`toString` call may throw
/// (propagated here). Every numeric operator coerces its stashed operands with
/// this before unboxing them.
pub(super) fn emit_coerce_number(function: &mut Function, local: u32, helpers: Helpers<'_>) {
    emit_is_number(function, local);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    enter_frame(helpers);
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::Call(helpers.to_number));
    function.instruction(&Instruction::LocalSet(local));
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    function.instruction(&Instruction::End);
}

/// Emit the `__to_number(v: i64) -> i64` helper backing [`emit_coerce_number`]
/// and `Number(x)`: `ToPrimitive` (hint number, in the guest) then the host
/// `__convert` to-number op on the primitive (`StringToNumber` rules). A throw
/// from a user `valueOf`/`toString` stays pending and yields `NaN`.
pub(super) fn emit_to_number_helper(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![]);
    let v = 0;
    emit_is_object(&mut function, v);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32Const(HINT_NUMBER));
    function.instruction(&Instruction::Call(helpers.to_primitive()));
    function.instruction(&Instruction::LocalSet(v));
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I64Const(QNAN_BITS));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_NUMBER));
    function.instruction(&Instruction::Call(helpers.convert));
    function.instruction(&Instruction::End);
    function
}

/// Push `1` if the jsval in `local` is a number, else `0`. Every boxed
/// non-number (boolean/null/undefined/heap pointer) has a clear sign bit, the
/// QNAN mask set and a nonzero tag, so its bits read as a signed `i64` strictly
/// greater than `QNAN | 0x0000_FFFF_FFFF_FFFF`. A number is every value at or
/// below that bound: ordinary `f64`s and the canonical NaN (the only NaN a
/// number jsval can hold — see [`emit_box_f64`]).
pub(super) fn emit_is_number(function: &mut Function, local: u32) {
    function.instruction(&Instruction::LocalGet(local));
    function.instruction(&Instruction::I64Const(QNAN_BITS | 0x0000_FFFF_FFFF_FFFF));
    function.instruction(&Instruction::I64LeS);
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
/// points at. The value MUST be a heap pointer — the caller is responsible
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

/// Push `1` if the jsval in `local` is a heap STRING, else `0`. Because
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

/// Push `1` if the jsval in `local` is a heap FUNCTION value, else `0`.
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
/// else `0`. Same shape as [`emit_is_function`], guarding the type-byte
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
