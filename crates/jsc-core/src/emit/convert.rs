//! Object -> primitive conversion in the guest: `OrdinaryToPrimitive` and
//! the conversion helpers built on it (`ToString`, the `+` and relational
//! slow paths, `Array.prototype.join`). Every object reaching a conversion
//! site goes through [`emit_to_primitive`], so the host `__convert` import
//! only ever sees primitives (and, until `Function.prototype.toString`
//! lands, a function without a user `toString`).
//!
//! Each helper leaves a throw from a user `valueOf`/`toString` pending and
//! returns a harmless value; the emitting site propagates it.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// `ToPrimitive` hint `"default"` (`+`, `==`).
pub(super) const HINT_DEFAULT: i32 = 0;
/// `ToPrimitive` hint `"number"`.
pub(super) const HINT_NUMBER: i32 = 1;
/// `ToPrimitive` hint `"string"`.
pub(super) const HINT_STRING: i32 = 2;
/// Flag or-ed into a hint: skip the `@@toPrimitive` lookup and run
/// `OrdinaryToPrimitive` (`Date.prototype[@@toPrimitive]`).
pub(super) const HINT_ORDINARY: i32 = 4;

/// `__compare` op codes, in [`BinOp`] relational order.
pub(super) const REL_LT: i32 = 0;
pub(super) const REL_GT: i32 = 1;
pub(super) const REL_LE: i32 = 2;
pub(super) const REL_GE: i32 = 3;

/// The `TypeError` of an object neither of whose methods yields a primitive.
const NO_PRIMITIVE: CalleeErrorText<'static> = [
    "Cannot convert object to primitive value",
    "TypeError: Cannot convert object to primitive value",
];

/// The `TypeError` of a non-callable `@@toPrimitive` (`GetMethod`).
const TO_PRIMITIVE_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Symbol.toPrimitive is not a function",
    "TypeError: Symbol.toPrimitive is not a function",
];

/// The `TypeError` of `ToString(symbol)`.
const SYMBOL_TO_STRING: CalleeErrorText<'static> = [
    "Cannot convert a Symbol value to a string",
    "TypeError: Cannot convert a Symbol value to a string",
];

/// The `TypeError` of `ToNumber(symbol)`.
pub(super) const SYMBOL_TO_NUMBER: CalleeErrorText<'static> = [
    "Cannot convert a Symbol value to a number",
    "TypeError: Cannot convert a Symbol value to a number",
];

/// The interim `TypeError`s of a well-known symbol hook a consult site finds
/// before the stage that honours it (design/symbol-strings.md §9.4), in
/// [`hook_text`] order.
const HOOK_TEXTS: [CalleeErrorText<'static>; 4] = [
    [
        "Symbol.match is not supported yet",
        "TypeError: Symbol.match is not supported yet",
    ],
    [
        "Symbol.matchAll is not supported yet",
        "TypeError: Symbol.matchAll is not supported yet",
    ],
    [
        "Symbol.replace is not supported yet",
        "TypeError: Symbol.replace is not supported yet",
    ],
    [
        "Symbol.search is not supported yet",
        "TypeError: Symbol.search is not supported yet",
    ],
];

/// See [`HOOK_TEXTS`]: `Symbol.split`.
const HOOK_SPLIT: CalleeErrorText<'static> = [
    "Symbol.split is not supported yet",
    "TypeError: Symbol.split is not supported yet",
];

/// The interim `TypeError` of a `symbol` hook (see [`HOOK_TEXTS`]); `None`
/// for the symbols without an interim guard (`@@iterator`, honoured by the
/// iteration protocol; `@@toPrimitive` / `@@toStringTag`, honoured by
/// `ToPrimitive` / `Object.prototype.toString`; `@@hasInstance`, honoured by
/// `instanceof`; `@@species` / `@@isConcatSpreadable`, honoured by
/// `ArraySpeciesCreate` / `SpeciesConstructor` / `concat`; `@@asyncIterator`,
/// `@@unscopables`, without a consult site).
pub(super) fn hook_text(symbol: WellKnown) -> Option<CalleeErrorText<'static>> {
    let at = match symbol {
        WellKnown::Match => 0,
        WellKnown::MatchAll => 1,
        WellKnown::Replace => 2,
        WellKnown::Search => 3,
        WellKnown::Split => return Some(HOOK_SPLIT),
        WellKnown::Iterator
        | WellKnown::Species
        | WellKnown::IsConcatSpreadable
        | WellKnown::AsyncIterator
        | WellKnown::Unscopables
        | WellKnown::ToPrimitive
        | WellKnown::ToStringTag
        | WellKnown::HasInstance => return None,
    };
    Some(HOOK_TEXTS[at])
}

/// Push i32 `1` when `Get(v, symbol)` of the jsval in i64 `local` is not
/// `undefined` (`__symbol_get`: no lookup unless the chain holds a symbol
/// key).
pub(super) fn emit_has_hook(
    function: &mut Function,
    local: u32,
    symbol: WellKnown,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(local), I32Const(realm::symbol_addr(symbol, helpers)), Call(helpers.symbol_get()),
        I64Const(UNDEFINED_BITS), I64Ne,
    );
}

/// Throw the interim `TypeError` of `symbol` (design/symbol-strings.md §9.4)
/// when the jsval in i64 `local` has a `symbol` hook ([`emit_has_hook`]):
/// the consult site does not honour a user hook before its stage. Usable in
/// a user function, a builtin body (`scratch` = its frame's scratch) or a
/// helper.
pub(super) fn emit_hook_guard(
    function: &mut Function,
    local: u32,
    symbol: WellKnown,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    let Some(text) = hook_text(symbol) else {
        return;
    };
    emit_has_hook(function, local, symbol, helpers);
    wasm!(function; If(BlockType::Empty));
    enter_frame(helpers);
    emit_throw_not_a_function(function, text, scratch, strings, helpers);
    leave_frame(helpers);
    wasm!(function; End);
}

/// The default `join` separator.
const COMMA: &str = ",";

/// The `SymbolDescriptiveString` fragments `String(symbol)` splices around
/// the description.
pub(super) const SYMBOL_OPEN: &str = "Symbol(";
/// See [`SYMBOL_OPEN`].
pub(super) const SYMBOL_CLOSE: &str = ")";

/// The static strings of this family.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    HOOK_TEXTS
        .into_iter()
        .chain([HOOK_SPLIT])
        .flatten()
        .chain(STRINGS)
}

/// The fixed static strings of this family (see [`strings`]).
const STRINGS: [&str; 17] = [
    NO_PRIMITIVE[0],
    NO_PRIMITIVE[1],
    TO_PRIMITIVE_NOT_CALLABLE[0],
    TO_PRIMITIVE_NOT_CALLABLE[1],
    SYMBOL_TO_STRING[0],
    SYMBOL_TO_STRING[1],
    SYMBOL_TO_NUMBER[0],
    SYMBOL_TO_NUMBER[1],
    COMMA,
    SYMBOL_OPEN,
    SYMBOL_CLOSE,
    "",
    "toString",
    "valueOf",
    "default",
    "number",
    "string",
];

/// Return `ret` (pushed by `ret`) when the last call left an exception pending.
fn return_if_pending(function: &mut Function, ret: &Instruction<'_>) {
    wasm!(function; GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty));
    function.instruction(ret);
    wasm!(function; Return, End);
}

/// When the jsval in `v` is a symbol, leave the `TypeError` `text` pending and
/// return the value `ret` pushes (a helper returning other than `i64` cannot
/// unwind through [`emit_throw_error`]). The helper's locals must start with
/// [`type_error_helper_locals`] at `scratch`.
pub(super) fn throw_if_symbol(
    f: &mut Function,
    v: u32,
    text: CalleeErrorText<'_>,
    ret: &Instruction<'_>,
    scratch: u32,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
) {
    emit_is_symbol(f, v);
    wasm!(f; If(BlockType::Empty));
    emit_node(&callee_type_error(text), f, scratch, strings, helpers);
    wasm!(f;
        GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
    );
    f.instruction(ret);
    wasm!(f; Return, End);
}

/// `__to_primitive(v: i64, hint: i32) -> i64`: `ToPrimitive`. A primitive is
/// returned as is. An object's `@@toPrimitive` (`GetMethod` through
/// `__symbol_get`, skipped when `hint` carries [`HINT_ORDINARY`]) is called
/// with `this = v` and the hint string (`"default"` / `"number"` /
/// `"string"`); a non-callable method or an object result throws the
/// `TypeError`. The builtins skip the call: `%Symbol.prototype%`'s returns
/// `thisSymbolValue(v)`, `%Date.prototype%`'s maps hint default to string.
/// Without one (`undefined` / `null`), `OrdinaryToPrimitive`: `valueOf`
/// then `toString` (hint string: `toString` first), each looked up through
/// the `proto` chain, skipped unless callable, its result skipped unless a
/// primitive; neither -> `TypeError`.
pub(super) fn emit_to_primitive(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let (v, hint, method, result, key, second, base) = (0, 1, 2, 3, 4, 5, 6);
    let scratch = 7;
    let mut locals = vec![(2, ValType::I64), (3, ValType::I32)];
    locals.extend(type_error_helper_locals());
    let mut f = Function::new(locals);
    helpers.unwind.borrow_mut().reset();
    emit_is_object(&mut f, v);
    wasm!(f; I32Eqz, If(BlockType::Empty), LocalGet(v), Return, End);
    let symbol = realm::method_bits(Builtin::Symbol(realm::SymbolOp::ToPrimitive), helpers);
    let date = realm::method_bits(
        Builtin::DateGeneric(realm::DateGenericOp::ToPrimitive),
        helpers,
    );
    let hint_str = |name| HEAP_BITS | i64::from(static_key(strings, name));
    wasm!(f;
        LocalGet(hint), I32Const(HINT_ORDINARY), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(v), I32Const(realm::symbol_addr(WellKnown::ToPrimitive, helpers)),
        Call(helpers.symbol_get()), LocalSet(method),
    );
    return_if_pending(&mut f, &Instruction::I64Const(UNDEFINED_BITS));
    emit_is_nullish(&mut f, method);
    wasm!(f;
        I32Eqz, If(BlockType::Empty),
        LocalGet(method), I64Const(symbol), I64Eq,
        If(BlockType::Empty),
    );
    emit_accessor_call(&mut f, method, v, None, base, helpers);
    wasm!(f;
        Return,
        End,
        LocalGet(method), I64Const(date), I64Eq,
        If(BlockType::Empty),
        I32Const(HINT_STRING), LocalGet(hint), LocalGet(hint), I32Eqz, Select, LocalSet(hint),
        Else,
    );
    emit_is_function(&mut f, method);
    wasm!(f; I32Eqz, If(BlockType::Empty));
    emit_throw_not_a_function(&mut f, TO_PRIMITIVE_NOT_CALLABLE, scratch, strings, helpers);
    wasm!(f;
        End,
        I64Const(hint_str("string")), I64Const(hint_str("number")),
        LocalGet(hint), I32Const(HINT_STRING), I32Eq, Select,
        I64Const(hint_str("default")), LocalGet(hint), Select, LocalSet(result),
    );
    emit_accessor_call(&mut f, method, v, Some(result), base, helpers);
    wasm!(f; LocalSet(result));
    return_if_pending(&mut f, &Instruction::I64Const(UNDEFINED_BITS));
    emit_is_object(&mut f, result);
    wasm!(f; If(BlockType::Empty));
    emit_throw_not_a_function(&mut f, NO_PRIMITIVE, scratch, strings, helpers);
    wasm!(f; End, LocalGet(result), Return, End, End, End);
    let (to_string, value_of) = (
        static_key(strings, "toString"),
        static_key(strings, "valueOf"),
    );
    wasm!(f;
        LocalGet(hint), I32Const(HINT_ORDINARY - 1), I32And, LocalSet(hint),
        I32Const(to_string), I32Const(value_of),
        LocalGet(hint), I32Const(HINT_STRING), I32Eq, Select, LocalSet(key),
        I32Const(value_of), I32Const(to_string),
        LocalGet(hint), I32Const(HINT_STRING), I32Eq, Select, LocalSet(second),
    );
    for pass in 0..2 {
        if pass == 1 {
            wasm!(f; LocalGet(second), LocalSet(key));
        }
        wasm!(f; LocalGet(v), LocalGet(key), Call(helpers.get), LocalSet(method));
        return_if_pending(&mut f, &Instruction::I64Const(UNDEFINED_BITS));
        emit_is_function(&mut f, method);
        wasm!(f; If(BlockType::Empty));
        emit_accessor_call(&mut f, method, v, None, base, helpers);
        wasm!(f; LocalSet(result));
        return_if_pending(&mut f, &Instruction::I64Const(UNDEFINED_BITS));
        emit_is_object(&mut f, result);
        wasm!(f; I32Eqz, If(BlockType::Empty), LocalGet(result), Return, End, End);
    }
    emit_throw_not_a_function(&mut f, NO_PRIMITIVE, scratch, strings, helpers);
    wasm!(f; End);
    f
}

/// `__to_string(v: i64) -> i32`: `ToString(v)` as a heap-string base. A
/// string is itself; an object is first converted with hint string; a symbol
/// throws the `TypeError`; the remaining primitives are formatted by the
/// host. A pending throw returns the empty string.
pub(super) fn emit_to_string(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let (v, scratch) = (0, 1);
    let mut f = Function::new(type_error_helper_locals());
    helpers.unwind.borrow_mut().reset();
    let empty = Instruction::I32Const(static_key(strings, ""));
    emit_is_string(&mut f, v);
    wasm!(f; If(BlockType::Result(ValType::I32)), LocalGet(v), I32WrapI64, Else);
    emit_is_object(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32Const(HINT_STRING), Call(helpers.to_primitive()), LocalSet(v),
    );
    return_if_pending(&mut f, &empty);
    wasm!(f; End);
    throw_if_symbol(
        &mut f,
        v,
        SYMBOL_TO_STRING,
        &empty,
        scratch,
        strings,
        helpers,
    );
    emit_is_string(&mut f, v);
    wasm!(f;
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), I32WrapI64,
        Else,
        LocalGet(v), I32Const(CONVERT_OP_TO_STRING), Call(helpers.convert), I32WrapI64,
        End,
        End,
        End,
    );
    f
}

/// Set i32 local `kind` to the type byte of the heap value in `x`, or `-1`
/// for a non-heap value (so `kind == 0` is a string, `kind > 0` an object or
/// a symbol).
fn load_kind(function: &mut Function, x: u32, kind: u32) {
    emit_is_pointer(function, x);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, x);
    wasm!(function; Else, I32Const(-1), End, LocalSet(kind));
}

/// `__add(a: i64, b: i64) -> i64`: the `+` operator once either operand is
/// a heap value: `ToPrimitive` both (hint default; only objects call out, a
/// symbol comes back as is), then concatenate the `ToString`s when either
/// primitive is a string, else add the `ToNumber`s (a symbol throws in
/// either conversion).
pub(super) fn emit_add_helper(helpers: Helpers<'_>) -> Function {
    let (a, b, ka, kb, lhs, num) = (0, 1, 2, 3, 4, 5);
    let mut f = Function::new(vec![(3, ValType::I32), (1, ValType::F64)]);
    let undefined = Instruction::I64Const(UNDEFINED_BITS);
    for (x, kind) in [(a, ka), (b, kb)] {
        load_kind(&mut f, x, kind);
        wasm!(f;
            LocalGet(kind), I32Const(TYPE_STRING), I32GtS,
            If(BlockType::Empty),
            LocalGet(x), I32Const(HINT_DEFAULT), Call(helpers.to_primitive()), LocalSet(x),
        );
        return_if_pending(&mut f, &undefined);
        load_kind(&mut f, x, kind);
        wasm!(f; End);
    }
    // Both operands are primitives now: a string is its own `ToString`, the
    // rest go through `__to_string` (a symbol throws, the host formats the
    // others).
    let primitive_to_string = |f: &mut Function, x: u32, kind: u32| {
        wasm!(f;
            LocalGet(kind), I32Eqz,
            If(BlockType::Result(ValType::I32)),
            LocalGet(x), I32WrapI64,
            Else,
            LocalGet(x), Call(helpers.to_str()),
            End,
        );
    };
    wasm!(f; LocalGet(ka), I32Eqz, LocalGet(kb), I32Eqz, I32Or, If(BlockType::Result(ValType::I64)));
    primitive_to_string(&mut f, a, ka);
    wasm!(f; LocalSet(lhs));
    return_if_pending(&mut f, &undefined);
    wasm!(f; LocalGet(lhs));
    primitive_to_string(&mut f, b, kb);
    wasm!(f; Call(helpers.concat), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Else);
    wasm!(f; LocalGet(a), Call(helpers.to_number), F64ReinterpretI64, LocalSet(num));
    return_if_pending(&mut f, &undefined);
    wasm!(f;
        LocalGet(num), LocalGet(b), Call(helpers.to_number), F64ReinterpretI64,
        F64Add, Call(helpers.box_f64),
        End,
        End,
    );
    f
}

/// Push the relational result for `op` (i32 local) from the four candidate
/// comparisons `lt`, `gt`, `le`, `ge` each pushes.
fn pick_relational(function: &mut Function, op: u32, cmps: [&dyn Fn(&mut Function); 4]) {
    let [lt, gt, le, ge] = cmps;
    lt(function);
    gt(function);
    wasm!(function; LocalGet(op), I32Const(REL_LT), I32Eq, Select);
    le(function);
    ge(function);
    wasm!(function; LocalGet(op), I32Const(REL_LE), I32Eq, Select);
    wasm!(function; LocalGet(op), I32Const(REL_LE), I32LtU, Select);
}

/// `__compare(a: i64, b: i64, op: i32) -> i32`: the relational operators
/// once an operand is not a number: `ToPrimitive` both (hint number), then
/// two strings compare by bytes and anything else by `ToNumber` (which
/// throws for a symbol).
pub(super) fn emit_compare(helpers: Helpers<'_>) -> Function {
    let (a, b, op, ord, lhs, rhs) = (0, 1, 2, 3, 4, 5);
    let mut f = Function::new(vec![(1, ValType::I32), (2, ValType::F64)]);
    for v in [a, b] {
        wasm!(f; LocalGet(v), I32Const(HINT_NUMBER), Call(helpers.to_primitive()), LocalSet(v));
        return_if_pending(&mut f, &Instruction::I32Const(0));
    }
    emit_is_string(&mut f, a);
    emit_is_string(&mut f, b);
    wasm!(f;
        I32And,
        If(BlockType::Result(ValType::I32)),
        LocalGet(a), I32WrapI64, LocalGet(b), I32WrapI64, Call(helpers.str_cmp), LocalSet(ord),
    );
    let int = |cmp: Instruction<'static>| move |function: &mut Function| wasm!(function; LocalGet(ord), I32Const(0), cmp.clone());
    pick_relational(
        &mut f,
        op,
        [
            &int(Instruction::I32LtS),
            &int(Instruction::I32GtS),
            &int(Instruction::I32LeS),
            &int(Instruction::I32GeS),
        ],
    );
    wasm!(f;
        Else,
        LocalGet(a), Call(helpers.to_number), F64ReinterpretI64, LocalSet(lhs),
    );
    return_if_pending(&mut f, &Instruction::I32Const(0));
    wasm!(f; LocalGet(b), Call(helpers.to_number), F64ReinterpretI64, LocalSet(rhs));
    return_if_pending(&mut f, &Instruction::I32Const(0));
    let float = |cmp: Instruction<'static>| move |function: &mut Function| wasm!(function; LocalGet(lhs), LocalGet(rhs), cmp.clone());
    pick_relational(
        &mut f,
        op,
        [
            &float(Instruction::F64Lt),
            &float(Instruction::F64Gt),
            &float(Instruction::F64Le),
            &float(Instruction::F64Ge),
        ],
    );
    wasm!(f; End, End);
    f
}

/// `__array_join(recv: i64, sep: i64) -> i64`: `Array.prototype.join` on an
/// array receiver (`undefined` for any other receiver): `ToString(sep)`
/// (`","` when `undefined`), then each element's `ToString` (`null`,
/// `undefined` and absent elements are empty; a hole reads through the
/// prototype chain) joined by it. An array already being
/// joined (a cycle) joins as the empty string.
pub(super) fn emit_array_join(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let (recv, sep_v, arr, len, i, acc, sep, e) = (0, 1, 2, 3, 4, 5, 6, 7);
    let mut f = Function::new(vec![(5, ValType::I32), (1, ValType::I64)]);
    let empty = static_key(strings, "");
    let flags = MemArg {
        offset: u64::from(abi::HEADER_FLAGS_OFFSET),
        align: 0,
        memory_index: 0,
    };
    let joining = i32::from(abi::ARRAY_FLAG_JOINING);
    emit_is_pointer(&mut f, recv);
    wasm!(f; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(&mut f, recv);
    wasm!(f;
        I32Const(TYPE_ARRAY), I32Ne,
        Else, I32Const(1), End,
        If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(recv), I32WrapI64, LocalSet(arr),
        LocalGet(sep_v), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)),
        I32Const(static_key(strings, COMMA)),
        Else,
        LocalGet(sep_v), Call(helpers.to_str()),
        End,
        LocalSet(sep),
    );
    return_if_pending(&mut f, &Instruction::I64Const(UNDEFINED_BITS));
    wasm!(f;
        LocalGet(arr), I32Load8U(flags), I32Const(joining), I32And,
        If(BlockType::Empty), I64Const(HEAP_BITS | i64::from(empty)), Return, End,
        LocalGet(arr), LocalGet(arr), I32Load8U(flags), I32Const(joining), I32Or, I32Store8(flags),
        LocalGet(arr), I32Load(MemArg { offset: ARRAY_LEN_OFFSET, align: 2, memory_index: 0 }),
        LocalSet(len),
        I32Const(empty), LocalSet(acc),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(i),
        If(BlockType::Empty), LocalGet(acc), LocalGet(sep), Call(helpers.concat), LocalSet(acc), End,
        // A hole, or an element past a length shrunk by an earlier element's
        // conversion, reads through the prototype chain.
        LocalGet(i), LocalGet(arr),
        I32Load(MemArg { offset: ARRAY_LEN_OFFSET, align: 2, memory_index: 0 }),
        I32LtU,
        If(BlockType::Result(ValType::I64)),
    );
    emit_elem_load(&mut f, arr, i, helpers.sparse_get());
    wasm!(f;
        Else, I64Const(HOLE_BITS), End, LocalTee(e), I64Const(HOLE_BITS), I64Eq,
        If(BlockType::Empty),
        LocalGet(arr), LocalGet(i), Call(helpers.proto_elem()), LocalSet(e),
    );
    let unwind = |f: &mut Function| {
        wasm!(f;
            GlobalGet(EXC_PENDING_GLOBAL),
            If(BlockType::Empty),
            LocalGet(arr), LocalGet(arr), I32Load8U(flags), I32Const(!joining), I32And, I32Store8(flags),
            I64Const(UNDEFINED_BITS), Return,
            End,
        );
    };
    unwind(&mut f);
    wasm!(f; End, LocalGet(e), I64Const(HOLE_BITS), I64Eq);
    emit_is_nullish(&mut f, e);
    wasm!(f;
        I32Or, I32Eqz,
        If(BlockType::Empty),
        LocalGet(acc), LocalGet(e), Call(helpers.to_str()), Call(helpers.concat), LocalSet(acc),
    );
    unwind(&mut f);
    wasm!(f;
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(arr), LocalGet(arr), I32Load8U(flags), I32Const(!joining), I32And, I32Store8(flags),
        LocalGet(acc), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        End,
    );
    f
}

/// Replace an object key in i64 `local` by its `ToPrimitive(hint string)` (a
/// primitive whose `ToPropertyKey` calls no user code: a `Symbol` wrapper
/// yields its symbol), propagating a user `toString`/`valueOf` throw, so a key
/// read and written by one expression (`o[k] op= v`, `o[k]++`) is converted
/// once.
pub(super) fn emit_key_to_primitive(function: &mut Function, local: u32, helpers: Helpers<'_>) {
    emit_is_object(function, local);
    wasm!(function; If(BlockType::Empty));
    enter_frame(helpers);
    wasm!(function;
        LocalGet(local), I32Const(HINT_STRING), Call(helpers.to_primitive()), LocalSet(local),
    );
    emit_propagate_if_pending(function, helpers);
    leave_frame(helpers);
    wasm!(function; End);
}
