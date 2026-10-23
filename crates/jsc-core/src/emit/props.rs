//! Own-property tables, key interning, and the generic property algorithms
//! (DESIGN.md §5.3, §6.3).
//!
//! Every object-family block (arrays included: their named and sparse-index
//! properties) carries a `props` slot pointing at a [`TYPE_PROPS`] table of
//! `(key, attrs, value)` entries and a `proto` slot; keys are interned string
//! blocks compared by pointer. The helpers here are the only code that reads
//! or writes those tables:
//!
//! - interning: `__str_hash`, `__intern_slot`, `__intern_find`, `__intern`,
//!   plus the `ToPropertyKey` conversion `__to_key` and the array-index helpers
//!   `__index_string` / `__index_value`;
//! - the table primitives `__prop_find` (own lookup, materializing a lazy
//!   function first) and `__define` (`CreateDataProperty` / accessor install,
//!   growing the table), and `__fn_materialize`;
//! - the algorithms `__get`, `__set`, `__has`, `__delete`, `__own_keys`,
//!   `__for_in_keys`, and `__rest`.
//!
//! The compile-time half — the initial intern table holding every static
//! string — is built by [`intern_table`] and laid into the data segment.

// Wasm locals are named by their role in the emitted helper (o = object, e = entry, ...).
#![allow(clippy::many_single_char_names)]

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

mod descriptor;
pub(super) use descriptor::*;

/// Kind byte of an own-property table (see [`abi::TYPE_PROPS`]).
const TYPE_PROPS: i32 = abi::TYPE_PROPS as i32;
/// Object-family `props` slot offset (see [`abi::OBJ_PROPS_OFFSET`]).
const OBJ_PROPS: u64 = abi::OBJ_PROPS_OFFSET as u64;
/// Header `flags` byte offset (see [`abi::HEADER_FLAGS_OFFSET`]).
const FLAGS: u64 = abi::HEADER_FLAGS_OFFSET as u64;
/// Table field offsets (see [`abi::TYPE_PROPS`]).
const P_TFLAGS: u64 = abi::PROPS_TFLAGS_OFFSET as u64;
const P_COUNT: u64 = abi::PROPS_COUNT_OFFSET as u64;
const P_CAP: u64 = abi::PROPS_CAP_OFFSET as u64;
const P_LIVE: u64 = abi::PROPS_LIVE_OFFSET as u64;
const P_HEADER: i32 = abi::PROPS_HEADER_SIZE.cast_signed();
const ENTRY: i32 = abi::PROP_ENTRY_SIZE.cast_signed();
const E_KEY: u64 = abi::PROP_KEY_OFFSET as u64;
const E_ATTRS: u64 = abi::PROP_ATTRS_OFFSET as u64;
const E_VALUE: u64 = abi::PROP_VALUE_OFFSET as u64;
/// Property attribute bits (see [`abi::ATTR_WRITABLE`] ..).
const ATTR_WRITABLE: i32 = abi::ATTR_WRITABLE.cast_signed();
const ATTR_ENUMERABLE: i32 = abi::ATTR_ENUMERABLE.cast_signed();
const ATTR_CONFIGURABLE: i32 = abi::ATTR_CONFIGURABLE.cast_signed();
const ATTR_ACCESSOR: i32 = abi::ATTR_ACCESSOR.cast_signed();
const ATTRS_DEFAULT: i32 = abi::ATTRS_DEFAULT.cast_signed();
/// Table `tflags` bits.
const HAS_INDEX_KEYS: i32 = abi::PROPS_HAS_INDEX_KEYS as i32;
const HAS_NON_ENUM: i32 = abi::PROPS_HAS_NON_ENUM as i32;
const WATCHED: i32 = abi::PROPS_WATCHED as i32;
const P_WATCH_BIT: u64 = abi::PROPS_WATCH_BIT_OFFSET as u64;
/// String `flags` bits.
const STR_INTERNED: i32 = abi::STR_FLAG_INTERNED as i32;
const STR_ARRAY_INDEX: i32 = abi::STR_FLAG_ARRAY_INDEX as i32;
/// Object-family `flags` bit: not extensible.
const NOT_EXTENSIBLE: i32 = abi::OBJ_FLAG_NOT_EXTENSIBLE as i32;
/// Function `flags` bits (see [`abi::FUNC_FLAG_LAZY`] ..).
const FUNC_LAZY: i32 = abi::FUNC_FLAG_LAZY as i32;
const FUNC_CONSTRUCTOR: i32 = abi::FUNC_FLAG_CONSTRUCTOR as i32;
const FUNC_CLASS_CTOR: i32 = abi::FUNC_FLAG_CLASS_CTOR as i32;
/// The header word (as a little-endian u32) of a lazy function, under
/// [`LAZY_FN_MASK`]: kind byte `TYPE_FUNCTION`, `flags` bit `LAZY`.
const LAZY_FN_WORD: i32 = TYPE_FUNCTION | FUNC_LAZY << 16;
const LAZY_FN_MASK: i32 = 0xFF | FUNC_LAZY << 16;
/// Byte offset of a string's u32 length, and of its payload.
const LEN: u64 = HEADER_SIZE as u64;
const PAYLOAD_I32: i32 = HEADER_SIZE + 4;

/// The 32-bit FNV-1a offset basis and prime the intern table hashes with.
const FNV_OFFSET: u32 = 0x811C_9DC5;
const FNV_PRIME: u32 = 0x0100_0193;

/// `__own_keys` mode: the enumerable string keys.
pub(super) const OWN_KEYS: i32 = 0;
/// `__own_keys` mode: the values of the enumerable string keys.
pub(super) const OWN_VALUES: i32 = 1;
/// `__own_keys` mode: `[key, value]` pairs of the enumerable string keys.
pub(super) const OWN_ENTRIES: i32 = 2;
/// `__own_keys` mode: every string key, enumerable or not.
pub(super) const OWN_ALL: i32 = 3;

fn m8(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

fn m32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn m64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// The 32-bit FNV-1a hash of `bytes` (the compile-time twin of `__str_hash`).
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(FNV_OFFSET, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(FNV_PRIME)
    })
}

/// Whether `text` is a canonical array index: `"0"`, or a nonzero digit
/// followed by digits, with value `< 2^32 - 1` (the compile-time twin of
/// `__index_value`).
pub(super) fn is_array_index(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if bytes[0] == b'0' {
        return bytes.len() == 1;
    }
    text.parse::<u64>().is_ok_and(|v| v < u64::from(u32::MAX))
}

/// The compile-time intern table: a power-of-two array of u32 slots at
/// `base`, each `0` or the offset of an interned static string, filled by
/// open addressing on [`fnv1a`] with linear probing (load ≤ 1/2).
pub(super) struct InternTable {
    /// Byte offset of slot 0 in linear memory.
    pub(super) base: i32,
    /// The slot count (a power of two).
    pub(super) cap: i32,
    /// The occupied slot count.
    pub(super) count: i32,
    /// The slot contents, in slot order.
    pub(super) slots: Vec<u32>,
}

/// Build the initial intern table holding every static string in `offsets`,
/// placed at the 8-aligned `base`.
pub(super) fn intern_table(offsets: &HashMap<String, i32>, base: i32) -> InternTable {
    let cap = (offsets.len() * 2).max(8).next_power_of_two();
    let mut slots = vec![0u32; cap];
    let mut entries: Vec<(&String, i32)> = offsets.iter().map(|(s, o)| (s, *o)).collect();
    entries.sort_by_key(|(_, offset)| *offset);
    for (text, offset) in entries {
        let mut i = fnv1a(text.as_bytes()) as usize & (cap - 1);
        while slots[i] != 0 {
            i = (i + 1) & (cap - 1);
        }
        slots[i] = offset.cast_unsigned();
    }
    InternTable {
        base,
        cap: i32::try_from(cap).unwrap_or(i32::MAX),
        count: i32::try_from(offsets.len()).unwrap_or(i32::MAX),
        slots,
    }
}

/// The interned key constant a literal-key site passes: the static string's
/// data-segment offset (every static string is interned at compile time).
pub(super) fn static_key(strings: &HashMap<String, i32>, key: &str) -> i32 {
    strings.get(key).copied().unwrap_or(HEAP_BASE)
}

/// Push the interned `u32` key of the property key `index` whose jsval is
/// stashed in `local`: a static string key is a constant; any other key is
/// converted by `__to_key` (`insert` = intern on a miss, else `0`).
pub(super) fn emit_prop_key(
    function: &mut Function,
    index: &IrNode,
    local: u32,
    insert: bool,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    if let IrNode::Str(key) = index {
        wasm!(function; I32Const(static_key(strings, key)));
    } else {
        wasm!(function; LocalGet(local), I32Const(i32::from(insert)), Call(helpers.to_key));
    }
}

/// Evaluate the property key `key` and push its interned `u32` lookup key: a
/// static string key is a constant; any other key is converted by `__to_key`
/// without inserting (`0` when never interned, i.e. absent everywhere).
pub(super) fn emit_key_lookup(
    key: &IrNode,
    function: &mut Function,
    scratch: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    if let IrNode::Str(name) = key {
        wasm!(function; I32Const(static_key(strings, name)));
    } else {
        emit_node(key, function, scratch, strings, helpers);
        wasm!(function; I32Const(0), Call(helpers.to_key));
    }
}

/// Push `1` when the i32 kind byte in `local` is a string or an array (the two
/// kinds with exotic own keys; an array's `props` table stays empty), else `0`.
fn emit_is_string_or_array_kind(function: &mut Function, local: u32) {
    wasm!(function;
        LocalGet(local), I32Const(TYPE_STRING), I32Eq,
        LocalGet(local), I32Const(TYPE_ARRAY), I32Eq,
        I32Or,
    );
}

/// Box the i32 heap block base on the stack as a heap jsval.
fn emit_box_heap(function: &mut Function) {
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Push the address of element `i` (i32 local) of the array whose handle base
/// is in i32 local `arr`, less [`ELEMS_DATA_OFFSET`] (the load/store `MemArg`
/// adds it).
fn emit_elem_addr(function: &mut Function, arr: u32, i: u32) {
    wasm!(function;
        LocalGet(arr), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalGet(i), I32Const(3), I32Shl, I32Add,
    );
}

/// Push `1` when element `i` (i32 local) of the array whose base is in i32
/// local `o` lives in its store — the array is not `SPARSE`, or it is and
/// `i < cap` — else `0` (the element, if any, is a table entry). `a` is i32
/// scratch.
fn emit_in_store(function: &mut Function, o: u32, i: u32, a: u32) {
    emit_is_sparse(function, o);
    wasm!(function;
        I32Eqz,
        If(BlockType::Result(ValType::I32)),
        I32Const(1),
        Else,
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Result(ValType::I32)),
        LocalGet(i), LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), I32LtU,
        Else, I32Const(0),
        End,
        End,
    );
}

/// Put the array whose base is in i32 local `o` in dictionary mode (DESIGN.md
/// §5.4) unless it already is: every present store element (`i < min(len,
/// cap)`) becomes an own data property of its index key with the default
/// attributes, the store is dropped (`elems = 0`), and the array is flagged
/// `SPARSE | DICTIONARY`. Every element then lives in the table, where it can
/// carry any attributes. `i`, `n`, `a` are i32 scratch.
pub(super) fn emit_dictionize(
    function: &mut Function,
    o: u32,
    i: u32,
    n: u32,
    a: u32,
    helpers: Helpers<'_>,
) {
    let elem = |function: &mut Function| {
        wasm!(function;
            LocalGet(a), LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(m64(ELEMS_DATA_OFFSET)),
        );
    };
    wasm!(function;
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_DICTIONARY), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Empty),
        // n = min(len, cap)
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), LocalTee(n),
        LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), LocalTee(i),
        LocalGet(n), LocalGet(i), I32LtU, Select, LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    elem(function);
    wasm!(function;
        I64Const(HOLE_BITS), I64Ne,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(i), Call(helpers.index_string), Call(helpers.intern),
    );
    elem(function);
    wasm!(function;
        I32Const(ATTRS_DEFAULT), Call(helpers.define),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(o), I32Const(0), I32Store(m32(ARRAY_ELEMS_OFFSET)),
        End,
        LocalGet(o),
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_SPARSE | ARRAY_DICTIONARY), I32Or,
        I32Store8(m8(FLAGS)),
        End,
    );
}

/// Push the u32 length of the string or array whose base is in i32 local `o`.
fn emit_exotic_len(function: &mut Function, o: u32) {
    wasm!(function;
        LocalGet(o), I32Load8U(m8(0)), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(o), I32Load(m32(LEN)),
        Else,
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)),
        End,
    );
}

/// Invoke the accessor function value in i64 local `callee` with `this` = the
/// i64 local `this`, passing `arg` (an i64 local) as its only argument when
/// given; the result stays on the stack. Only emitted when the program has
/// accessors (so the funcref table exists).
pub(super) fn emit_accessor_call(
    function: &mut Function,
    callee: u32,
    this: u32,
    arg: Option<u32>,
    base: u32,
    helpers: Helpers<'_>,
) {
    let mut pushed = 0;
    if let Some(arg) = arg
        && helpers.max_args >= 1
    {
        wasm!(function; LocalGet(arg));
        pushed = 1;
    }
    for _ in pushed..helpers.max_args {
        wasm!(function; I64Const(UNDEFINED_BITS));
    }
    wasm!(function;
        LocalGet(this), GlobalSet(THIS_GLOBAL),
        LocalGet(callee), I32WrapI64, LocalTee(base),
        I64Load(m64(FUNC_ENV_OFFSET)),
    );
    emit_argc(usize::from(arg.is_some()), function);
    wasm!(function;
        LocalGet(base), I32Load(m32(FUNC_INDEX_OFFSET)),
        CallIndirect { type_index: helpers.fn_type, table_index: 0 },
    );
}

/// Raise the `__prop_throw` `TypeError` of `mode` for the receiver jsval in
/// local `recv` and the interned key in i32 local `key`, then return from the
/// helper (the caller propagates the pending exception).
fn emit_prop_throw_return(
    function: &mut Function,
    recv: u32,
    key: u32,
    mode: i32,
    helpers: Helpers<'_>,
) {
    wasm!(function; LocalGet(recv), LocalGet(key));
    emit_box_heap(function);
    wasm!(function; I32Const(mode), Call(helpers.prop_throw), Return);
}

/// Set the dirty bit of the property table in the i32 local `table` when it
/// belongs to a watched intrinsic (DESIGN.md §6.4).
fn emit_mark_dirty(function: &mut Function, table: u32) {
    wasm!(function;
        LocalGet(table), I32Load8U(m8(P_TFLAGS)), I32Const(WATCHED), I32And,
        If(BlockType::Empty),
        GlobalGet(DIRTY_GLOBAL), I32Const(1), LocalGet(table), I32Load8U(m8(P_WATCH_BIT)), I32Shl,
        I32Or, GlobalSet(DIRTY_GLOBAL),
        End,
    );
}

/// `__str_hash(s: i32) -> i32`: the 32-bit FNV-1a hash of the string's bytes.
pub(super) fn emit_str_hash() -> Function {
    let mut f = Function::new(vec![(3, ValType::I32)]);
    let (s, h, p, end) = (0, 1, 2, 3);
    wasm!(f;
        I32Const(FNV_OFFSET.cast_signed()), LocalSet(h),
        LocalGet(s), I32Const(PAYLOAD_I32), I32Add, LocalTee(p),
        LocalGet(s), I32Load(m32(LEN)), I32Add, LocalSet(end),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(h), LocalGet(p), I32Load8U(m8(0)), I32Xor,
        I32Const(FNV_PRIME.cast_signed()), I32Mul, LocalSet(h),
        LocalGet(p), I32Const(1), I32Add, LocalSet(p),
        Br(0),
        End, End,
        LocalGet(h),
        End,
    );
    f
}

/// `__intern_slot(s: i32) -> i32`: the address of the intern-table slot that
/// holds the string with `s`'s content, or of the empty slot where it belongs.
pub(super) fn emit_intern_slot(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32)]);
    let (s, mask, i, a, c) = (0, 1, 2, 3, 4);
    wasm!(f;
        GlobalGet(INTERN_CAP_GLOBAL), I32Const(1), I32Sub, LocalSet(mask),
        LocalGet(s), Call(helpers.str_hash), LocalGet(mask), I32And, LocalSet(i),
        Loop(BlockType::Empty),
        GlobalGet(INTERN_BASE_GLOBAL), LocalGet(i), I32Const(2), I32Shl, I32Add, LocalTee(a),
        I32Load(m32(0)), LocalTee(c), I32Eqz,
        If(BlockType::Empty), LocalGet(a), Return, End,
        LocalGet(c), LocalGet(s), Call(helpers.str_eq),
        If(BlockType::Empty), LocalGet(a), Return, End,
        LocalGet(i), I32Const(1), I32Add, LocalGet(mask), I32And, LocalSet(i),
        Br(0),
        End,
        Unreachable,
        End,
    );
    f
}

/// `__intern_find(s: i32) -> i32`: the interned block with `s`'s content, or
/// `0` when no such string was ever interned (so no property has that key).
pub(super) fn emit_intern_find(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    wasm!(f; LocalGet(0), Call(helpers.intern_slot), I32Load(m32(0)), End);
    f
}

/// `__intern(s: i32) -> i32`: the interned block with `s`'s content, inserting
/// `s` itself (flagged `INTERNED`, plus `ARRAY_INDEX` for a canonical index
/// spelling) on a miss. The table doubles, rehashing into a fresh heap copy,
/// once more than half its slots are used.
pub(super) fn emit_intern(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32)]);
    let (s, a, c, old, cap, i) = (0, 1, 2, 3, 4, 5);
    wasm!(f;
        LocalGet(s), Call(helpers.intern_slot), LocalTee(a), I32Load(m32(0)), LocalTee(c),
        If(BlockType::Empty), LocalGet(c), Return, End,
        LocalGet(a), LocalGet(s), I32Store(m32(0)),
        // flags |= INTERNED | (is_index << 1)
        LocalGet(s),
        LocalGet(s), I32Load8U(m8(FLAGS)), I32Const(STR_INTERNED), I32Or,
        I32Const(STR_ARRAY_INDEX), I32Const(0),
        LocalGet(s), Call(helpers.index_value), I32Const(-1), I32Ne,
        Select, I32Or,
        I32Store8(m8(FLAGS)),
        // count += 1; grow when 2 * count > cap.
        GlobalGet(INTERN_COUNT_GLOBAL), I32Const(1), I32Add, GlobalSet(INTERN_COUNT_GLOBAL),
        GlobalGet(INTERN_COUNT_GLOBAL), I32Const(1), I32Shl, GlobalGet(INTERN_CAP_GLOBAL), I32GtU,
        If(BlockType::Empty),
        GlobalGet(INTERN_BASE_GLOBAL), LocalSet(old),
        GlobalGet(INTERN_CAP_GLOBAL), LocalSet(cap),
        LocalGet(cap), I32Const(3), I32Shl, Call(helpers.alloc), LocalTee(a),
        I32Const(0), LocalGet(cap), I32Const(3), I32Shl, MemoryFill(0),
        LocalGet(a), GlobalSet(INTERN_BASE_GLOBAL),
        LocalGet(cap), I32Const(1), I32Shl, GlobalSet(INTERN_CAP_GLOBAL),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(cap), I32GeU, BrIf(1),
        LocalGet(old), LocalGet(i), I32Const(2), I32Shl, I32Add, I32Load(m32(0)), LocalTee(c),
        If(BlockType::Empty),
        LocalGet(c), Call(helpers.intern_slot), LocalGet(c), I32Store(m32(0)),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
        LocalGet(s),
        End,
    );
    f
}

/// `__index_value(s: i32) -> i32`: the value of the string `s` when it is a
/// canonical array index (`"0"`, or a nonzero digit then digits, `< 2^32 - 1`),
/// else `-1` (`2^32 - 1`, never an index).
pub(super) fn emit_index_value() -> Function {
    let mut f = Function::new(vec![(3, ValType::I32), (1, ValType::I64)]);
    let (s, len, p, c, v) = (0, 1, 2, 3, 4);
    wasm!(f;
        LocalGet(s), I32Load(m32(LEN)), LocalTee(len),
        I32Eqz, LocalGet(len), I32Const(10), I32GtU, I32Or,
        If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(s), I32Const(PAYLOAD_I32), I32Add, LocalTee(p),
        I32Load8U(m8(0)), I32Const(i32::from(b'0')), I32Eq,
        If(BlockType::Empty),
        I32Const(0), I32Const(-1), LocalGet(len), I32Const(1), I32Eq, Select, Return,
        End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(len), I32Eqz, BrIf(1),
        LocalGet(p), I32Load8U(m8(0)), I32Const(i32::from(b'0')), I32Sub, LocalTee(c),
        I32Const(9), I32GtU,
        If(BlockType::Empty), I32Const(-1), Return, End,
        LocalGet(v), I64Const(10), I64Mul, LocalGet(c), I64ExtendI32U, I64Add, LocalSet(v),
        LocalGet(p), I32Const(1), I32Add, LocalSet(p),
        LocalGet(len), I32Const(1), I32Sub, LocalSet(len),
        Br(0),
        End, End,
        I32Const(-1), LocalGet(v), I32WrapI64,
        LocalGet(v), I64Const(i64::from(u32::MAX)), I64GeU, Select,
        End,
    );
    f
}

/// `__index_string(n: i32) -> i32`: a fresh string holding the decimal digits
/// of the unsigned `n`.
pub(super) fn emit_index_string(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(3, ValType::I32)]);
    let (n, len, t, p) = (0, 1, 2, 3);
    wasm!(f;
        I32Const(1), LocalSet(len), LocalGet(n), LocalSet(t),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(t), I32Const(10), I32LtU, BrIf(1),
        LocalGet(t), I32Const(10), I32DivU, LocalSet(t),
        LocalGet(len), I32Const(1), I32Add, LocalSet(len),
        Br(0),
        End, End,
        LocalGet(len), Call(helpers.new_string), LocalTee(t),
        LocalGet(len), I32Add, I32Const(PAYLOAD_I32), I32Add, LocalSet(p),
        Loop(BlockType::Empty),
        LocalGet(p), I32Const(1), I32Sub, LocalTee(p),
        LocalGet(n), I32Const(10), I32RemU, I32Const(i32::from(b'0')), I32Add,
        I32Store8(m8(0)),
        LocalGet(n), I32Const(10), I32DivU, LocalTee(n),
        BrIf(0),
        End,
        LocalGet(t),
        End,
    );
    f
}

/// `__to_key(v: i64, insert: i32) -> i32`: `ToPropertyKey` of `v` as an interned
/// key. An interned string is its own key; an integer `0 <= v < 2^32 - 1` is
/// formatted in-guest; anything else goes through `__to_string` (an object's
/// user `toString` may throw: the exception stays pending and the key is `""`). The
/// resulting string is then interned (`insert`) or looked up (`0` on a miss).
/// A lookup still interns a canonical array index, so an array or string
/// element is found by an index key that no property ever used.
pub(super) fn emit_to_key(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32), (1, ValType::F64)]);
    let (v, insert, s, r, d) = (0, 1, 2, 3, 4);
    emit_is_string(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, LocalTee(s), I32Load8U(m8(FLAGS)), I32Const(STR_INTERNED), I32And,
        If(BlockType::Empty), LocalGet(s), Return, End,
        Else,
    );
    emit_is_number(&mut f, v);
    wasm!(f;
        If(BlockType::Result(ValType::I32)),
        LocalGet(v), F64ReinterpretI64, LocalTee(d), F64Const(0.0.into()), F64Ge,
        LocalGet(d), F64Const(f64::from(u32::MAX).into()), F64Lt, I32And,
        LocalGet(d), F64Trunc, LocalGet(d), F64Eq, I32And,
        Else, I32Const(0),
        End,
        If(BlockType::Empty),
        LocalGet(d), I32TruncSatF64U, Call(helpers.index_string), LocalSet(s),
        Else,
        LocalGet(v), Call(helpers.to_str()), LocalSet(s),
        End,
        End,
        LocalGet(insert),
        If(BlockType::Result(ValType::I32)),
        LocalGet(s), Call(helpers.intern),
        Else,
        LocalGet(s), Call(helpers.intern_find), LocalTee(r),
        If(BlockType::Result(ValType::I32)),
        LocalGet(r),
        Else,
        LocalGet(s), Call(helpers.index_value), I32Const(-1), I32Ne,
        If(BlockType::Result(ValType::I32)), LocalGet(s), Call(helpers.intern), Else, I32Const(0), End,
        End,
        End,
        End,
    );
    f
}

/// `__prop_find(o: i32, key: i32) -> i32`: the address of the own entry of the
/// object-family block `o` whose key is `key`, or `0` (also for `key == 0`, so
/// a missed key never matches a tombstone). A `LAZY` function (which has no
/// table yet) is materialized first, so every property operation sees its
/// `length`/`name`/`prototype`.
pub(super) fn emit_prop_find(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (o, key, e, end) = (0, 1, 2, 3);
    wasm!(f;
        LocalGet(key), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(e), I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(0)), I32Const(LAZY_FN_MASK), I32And, I32Const(LAZY_FN_WORD), I32Ne,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(o), Call(helpers.define + 1),
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalSet(e),
        End,
        LocalGet(e), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(e), I32Add,
        I32Const(P_HEADER), I32Add, LocalSet(end),
        LocalGet(e), I32Const(P_HEADER), I32Add, LocalSet(e),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(e), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(e), I32Load(m32(E_KEY)), LocalGet(key), I32Eq,
        If(BlockType::Empty), LocalGet(e), Return, End,
        LocalGet(e), I32Const(ENTRY), I32Add, LocalSet(e),
        Br(0),
        End, End,
        I32Const(0),
        End,
    );
    f
}

/// `__define(o: i32, key: i32, v: i64, attrs: i32)`: define the own property
/// `key` of the object-family block `o` with the complete descriptor
/// `(v, attrs)` (`CreateDataProperty` / `DefinePropertyOrThrow` / accessor
/// install): overwrite in place when present, else append. Redefining a
/// non-configurable property throws `Cannot redefine property: k`, and adding
/// to a `NOT_EXTENSIBLE` object throws `Cannot define property k, object is
/// not extensible` (both left pending, nothing changed). A missing table
/// starts at capacity 4; a full one is rebuilt with capacity `max(4, 2 *
/// live)` (the same capacity when tombstones free half), keeping the live
/// entries in order (and the table's watch flag and bit), and `o`'s `props`
/// slot is rewritten. A watched table sets its dirty bit when the program
/// tracks them.
pub(super) fn emit_define(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32)]);
    let (o, key, v, attrs) = (0, 1, 2, 3);
    let (e, p, q, n, src, end, cap) = (4, 5, 6, 7, 8, 9, 10);
    // Throw the keyed `__prop_throw` TypeError of `mode` for `o` / `key`.
    let throw = |f: &mut Function, mode: i32| {
        wasm!(f; LocalGet(o));
        emit_box_heap(f);
        wasm!(f; LocalGet(key));
        emit_box_heap(f);
        wasm!(f; I32Const(mode), Call(helpers.prop_throw), Return);
    };
    wasm!(f;
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(e), I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        If(BlockType::Empty),
    );
    throw(&mut f, PROP_THROW_DEFINE_NOT_EXTENSIBLE);
    wasm!(f;
        End,
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(p), I32Eqz,
        If(BlockType::Empty),
        // A fresh table: kind 12, count 0, cap 4, live 0.
        I32Const(P_HEADER + 4 * ENTRY), Call(helpers.alloc), LocalTee(p),
        I64Const(i64::from(TYPE_PROPS)), I64Store(m64(0)),
        LocalGet(p), I64Const(4), I64Store(m64(P_CAP)),
        LocalGet(o), LocalGet(p), I32Store(m32(OBJ_PROPS)),
        Else,
        LocalGet(p), I32Load(m32(P_COUNT)), LocalGet(p), I32Load(m32(P_CAP)), LocalTee(cap), I32Eq,
        If(BlockType::Empty),
        // cap' = 2 * live <= cap ? cap : max(4, 2 * live)
        LocalGet(cap),
        LocalGet(p), I32Load(m32(P_LIVE)), I32Const(1), I32Shl, LocalTee(n),
        I32Const(4), LocalGet(n), I32Const(4), I32GtU, Select,
        LocalGet(n), LocalGet(cap), I32LeU, Select, LocalSet(cap),
        LocalGet(cap), I32Const(4), I32Shl, I32Const(P_HEADER), I32Add, Call(helpers.alloc), LocalTee(q),
        LocalGet(p), I32Load(m32(0)), I32Store(m32(0)),
        LocalGet(q), LocalGet(cap), I32Store(m32(P_CAP)),
        // Copy the live entries in slot order.
        LocalGet(p), I32Const(P_HEADER), I32Add, LocalTee(src),
        LocalGet(p), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, I32Add, LocalSet(end),
        LocalGet(q), I32Const(P_HEADER), I32Add, LocalSet(n),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(src), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(src), I32Load(m32(E_KEY)),
        If(BlockType::Empty),
        LocalGet(n), LocalGet(src), I64Load(m64(0)), I64Store(m64(0)),
        LocalGet(n), LocalGet(src), I64Load(m64(E_VALUE)), I64Store(m64(E_VALUE)),
        LocalGet(n), I32Const(ENTRY), I32Add, LocalSet(n),
        End,
        LocalGet(src), I32Const(ENTRY), I32Add, LocalSet(src),
        Br(0),
        End, End,
        LocalGet(q), LocalGet(p), I32Load(m32(P_LIVE)), I32Store(m32(P_COUNT)),
        LocalGet(q), LocalGet(p), I32Load(m32(P_LIVE)), I32Store(m32(P_LIVE)),
        LocalGet(o), LocalGet(q), I32Store(m32(OBJ_PROPS)),
        LocalGet(q), LocalSet(p),
        End,
        End,
        // Append at slot `count`.
        LocalGet(p), I32Load(m32(P_COUNT)), LocalTee(n), I32Const(4), I32Shl, LocalGet(p), I32Add,
        I32Const(P_HEADER), I32Add, LocalTee(e), LocalGet(key), I32Store(m32(E_KEY)),
        LocalGet(p), LocalGet(n), I32Const(1), I32Add, I32Store(m32(P_COUNT)),
        LocalGet(p), LocalGet(p), I32Load(m32(P_LIVE)), I32Const(1), I32Add, I32Store(m32(P_LIVE)),
        // An array-index key sets HAS_INDEX_KEYS.
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(p), LocalGet(p), I32Load8U(m8(P_TFLAGS)), I32Const(HAS_INDEX_KEYS), I32Or,
        I32Store8(m8(P_TFLAGS)),
        End,
        Else,
        LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz,
        If(BlockType::Empty),
    );
    throw(&mut f, PROP_THROW_REDEFINE);
    wasm!(f;
        End,
        End,
        LocalGet(e), LocalGet(attrs), I32Store(m32(E_ATTRS)),
        LocalGet(e), LocalGet(v), I64Store(m64(E_VALUE)),
        // A non-enumerable property sets HAS_NON_ENUM.
        LocalGet(attrs), I32Const(ATTR_ENUMERABLE), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(p),
        LocalGet(p), I32Load8U(m8(P_TFLAGS)), I32Const(HAS_NON_ENUM), I32Or,
        I32Store8(m8(P_TFLAGS)),
        End,
    );
    if helpers.dirty_tracking {
        wasm!(f; LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalSet(p));
        emit_mark_dirty(&mut f, p);
    }
    wasm!(f; End);
    f
}

/// `__fn_materialize(o: i32)`: clear the `LAZY` flag of the function block `o`
/// and define its own properties from its `__fn_info` entry (DESIGN.md §5.5):
/// `length` and `name` (configurable only); and for a plain `CONSTRUCTOR`
/// (not a class, whose definition installs its own `prototype`), `prototype`
/// (writable only) = a fresh object whose non-enumerable `constructor` is `o`.
/// No own `caller` / `arguments`: every function is strict, so they resolve to
/// the `%Function.prototype%` `%ThrowTypeError%` accessors.
pub(super) fn emit_fn_materialize(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (o, i, p) = (0, 1, 2);
    // Finish a `__define(target, key, value, attrs)` whose first three
    // operands are on the stack.
    let define = |f: &mut Function, attrs: i32| wasm!(f; I32Const(attrs), Call(helpers.define));
    let key = |name: &str| static_key(strings, name);
    wasm!(f;
        LocalGet(o),
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(!FUNC_LAZY & 0xFF), I32And,
        I32Store8(m8(FLAGS)),
        LocalGet(o), I32Load(m32(FUNC_INDEX_OFFSET)), I32Const(abi::FN_INFO_ENTRY_SIZE.cast_signed()),
        I32Mul, I32Const(helpers.fn_info), I32Add, LocalSet(i),
        LocalGet(o), I32Const(key(INDEX_LENGTH_KEY)),
        LocalGet(i), I32Load(m32(abi::FN_INFO_LENGTH_OFFSET.into())), F64ConvertI32U, I64ReinterpretF64,
    );
    define(&mut f, ATTR_CONFIGURABLE);
    wasm!(f;
        LocalGet(o), I32Const(key("name")),
        LocalGet(i), I32Load(m32(abi::FN_INFO_NAME_OFFSET.into())),
    );
    emit_box_heap(&mut f);
    define(&mut f, ATTR_CONFIGURABLE);
    wasm!(f;
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(FUNC_CONSTRUCTOR | FUNC_CLASS_CTOR), I32And,
        I32Const(FUNC_CONSTRUCTOR), I32Eq,
        If(BlockType::Empty),
        I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalSet(p),
    );
    emit_object_prefix(
        &mut f,
        p,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    wasm!(f; LocalGet(p), I32Const(key("constructor")), LocalGet(o));
    emit_box_heap(&mut f);
    define(&mut f, ATTR_WRITABLE | ATTR_CONFIGURABLE);
    wasm!(f; LocalGet(o), I32Const(key("prototype")), LocalGet(p));
    emit_box_heap(&mut f);
    define(&mut f, ATTR_WRITABLE);
    wasm!(f; End, End);
    f
}

/// Materialize the `LAZY` function whose block base is in i32 local `o` (a
/// no-op for any other block), so its own `length` / `name` / `prototype`
/// exist before a whole-table walk or an integrity change.
pub(super) fn emit_materialize(function: &mut Function, o: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(o), I32Load(m32(0)), I32Const(LAZY_FN_MASK), I32And, I32Const(LAZY_FN_WORD), I32Eq,
        If(BlockType::Empty), LocalGet(o), Call(helpers.define + 1), End,
    );
}

/// Emit the string/array exotic own lookup of the interned `key` (i32 local)
/// on the block base in i32 local `o` whose kind byte is in i32 local `t`:
/// `length` and an in-range canonical index return their value (an array hole
/// reads `undefined`, a `SPARSE` array's element comes from `__sparse_get`);
/// any other key falls through (an array's named keys live in its own table,
/// a string's inherited ones on `%String.prototype%`). A key of `0` (never
/// interned) returns `undefined`. `p` is i64 scratch.
#[allow(clippy::too_many_arguments)]
fn emit_exotic_get(
    function: &mut Function,
    o: u32,
    t: u32,
    key: u32,
    i: u32,
    p: u32,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    wasm!(function;
        LocalGet(key), I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty),
    );
    emit_exotic_len(function, o);
    wasm!(function;
        F64ConvertI32U, I64ReinterpretF64, Return,
        End,
    );
    emit_exotic_index(function, key, i, helpers);
    emit_exotic_len(function, o);
    wasm!(function;
        I32LtU,
        If(BlockType::Empty),
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(o), LocalGet(i), Call(helpers.str_index),
    );
    emit_box_heap(function);
    wasm!(function; Else);
    emit_elem_load(function, o, i, helpers.sparse_get());
    wasm!(function; LocalSet(p));
    emit_elem_read(function, p);
    wasm!(function;
        End,
        Return,
        End,
    );
}

/// Push the index of the interned `key` (i32 local, nonzero) into i32 local
/// `i`, leaving it on the stack: its value when it is a canonical array index
/// (`ARRAY_INDEX` flag), else `-1` (`2^32 - 1`, never an index).
fn emit_exotic_index(function: &mut Function, key: u32, i: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(key), Call(helpers.index_value),
        I32Const(-1),
        LocalGet(key), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And, Select,
        LocalTee(i),
    );
}

/// `__get(recv: i64, key: i32) -> i64`: `OrdinaryGet` of the interned `key` on
/// any jsval. A string or array resolves its exotic own keys (`length`,
/// indices); an object-family block (an array for its named keys) scans its
/// own table and then each `proto`, calling an accessor's getter with
/// `this = recv` (`undefined` when it has none). A string, number or boolean
/// continues at its realm prototype without allocating a wrapper;
/// `null`/`undefined` read `undefined`.
pub(super) fn emit_get(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (recv, key, o, e, t, i, p) = (0, 1, 2, 3, 4, 5, 6);
    let base = t;
    let proto_base = |ctor| {
        let bits = intrinsic_bits(realm::prototype_of(ctor), helpers);
        Instruction::I32Const(u32::try_from(bits & 0xffff_ffff).unwrap_or(0).cast_signed())
    };
    emit_is_pointer(&mut f, recv);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(recv), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), LocalSet(t),
    );
    emit_is_string_or_array_kind(&mut f, t);
    wasm!(f; If(BlockType::Empty));
    emit_exotic_get(&mut f, o, t, key, i, p, strings, helpers);
    wasm!(f;
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Empty), proto_base(BuiltinCtor::String), LocalSet(o), End,
        End,
        Else,
    );
    emit_is_number(&mut f, recv);
    wasm!(f;
        If(BlockType::Empty),
        proto_base(BuiltinCtor::Number), LocalSet(o),
        Else,
        LocalGet(recv), I64Const(TRUE_BITS), I64Eq, LocalGet(recv), I64Const(FALSE_BITS), I64Eq, I32Or,
        I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        proto_base(BuiltinCtor::Boolean), LocalSet(o),
        End,
    );
    wasm!(f;
        End,
        Loop(BlockType::Empty),
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(e),
        If(BlockType::Empty),
    );
    if helpers.has_accessors {
        wasm!(f;
            LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_ACCESSOR), I32And,
            If(BlockType::Empty),
            LocalGet(e), I64Load(m64(E_VALUE)), I32WrapI64, I64Load(m64(ACCESSOR_GET_OFFSET)),
            LocalSet(p),
        );
        emit_is_function(&mut f, p);
        wasm!(f; I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End);
        emit_accessor_call(&mut f, p, recv, None, base, helpers);
        wasm!(f; Return, End);
    }
    wasm!(f;
        LocalGet(e), I64Load(m64(E_VALUE)), Return,
        End,
        LocalGet(o), I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(p),
    );
    emit_is_pointer(&mut f, p);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End,
        LocalGet(p), I32WrapI64, LocalSet(o),
        Br(0),
        End,
        Unreachable,
        End,
    );
    f
}

/// `__set(recv: i64, key: i32, v: i64)`: `OrdinarySet` (strict) of the interned
/// `key` on the object-family block `recv`. The first own-or-inherited hit
/// decides: an accessor calls its setter with `this = recv` (a getter-only one
/// throws), a non-writable data property throws, an own writable one is
/// overwritten; otherwise an own `0b0111` data property is added (a
/// non-extensible object throws). An own overwrite on a watched table sets its
/// dirty bit when the program tracks them (an add does so in `__define`). A
/// throw leaves the exception pending.
pub(super) fn emit_set(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32), (1, ValType::I64)]);
    let (recv, key, v) = (0, 1, 2);
    let (o, q, e, own, a, s) = (3, 4, 5, 6, 7, 8);
    wasm!(f;
        LocalGet(recv), I32WrapI64, LocalTee(o), LocalSet(q),
        I32Const(1), LocalSet(own),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(q), LocalGet(key), Call(helpers.prop_find), LocalTee(e), BrIf(1),
        LocalGet(q), I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(s),
    );
    emit_is_pointer(&mut f, s);
    wasm!(f;
        I32Eqz, BrIf(1),
        LocalGet(s), I32WrapI64, LocalSet(q),
        I32Const(0), LocalSet(own),
        Br(0),
        End, End,
        LocalGet(e),
        If(BlockType::Empty),
        LocalGet(e), I32Load(m32(E_ATTRS)), LocalSet(a),
    );
    if helpers.has_accessors {
        wasm!(f;
            LocalGet(a), I32Const(ATTR_ACCESSOR), I32And,
            If(BlockType::Empty),
            LocalGet(e), I64Load(m64(E_VALUE)), I32WrapI64, I64Load(m64(ACCESSOR_SET_OFFSET)),
            LocalSet(s),
        );
        emit_is_function(&mut f, s);
        wasm!(f; If(BlockType::Empty));
        emit_accessor_call(&mut f, s, recv, Some(v), q, helpers);
        wasm!(f; Drop, Return, End);
        emit_prop_throw_return(&mut f, recv, key, PROP_THROW_GETTER_ONLY, helpers);
        wasm!(f; End);
    }
    wasm!(f;
        LocalGet(a), I32Const(ATTR_WRITABLE), I32And, I32Eqz,
        If(BlockType::Empty),
    );
    emit_prop_throw_return(&mut f, recv, key, PROP_THROW_READ_ONLY, helpers);
    wasm!(f;
        End,
        LocalGet(own),
        If(BlockType::Empty),
        LocalGet(e), LocalGet(v), I64Store(m64(E_VALUE)),
    );
    if helpers.dirty_tracking {
        wasm!(f; LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalSet(a));
        emit_mark_dirty(&mut f, a);
    }
    wasm!(f;
        Return,
        End,
        End,
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(NOT_EXTENSIBLE), I32And,
        If(BlockType::Empty),
    );
    emit_prop_throw_return(&mut f, recv, key, PROP_THROW_NOT_EXTENSIBLE, helpers);
    wasm!(f;
        End,
        LocalGet(o), LocalGet(key), LocalGet(v), I32Const(ATTRS_DEFAULT), Call(helpers.define),
        End,
    );
    f
}

/// `__has(key: i32, obj: i64) -> i32`: `HasProperty` of the interned `key`
/// (`0` = a key never interned, absent everywhere): a string/array's exotic
/// own keys (`length`, an in-range index whose store slot is not a hole),
/// else the own table (an array's named keys, and a `SPARSE` array's
/// elements past its store) and then each `proto`. A primitive
/// `obj` throws the `in` `TypeError` (left pending, result `0`).
pub(super) fn emit_has(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    emit_has_lookup(strings, helpers, false)
}

/// `__has_own(key: i32, obj: i64) -> i32`: [`emit_has`] without the `proto`
/// walk (`HasOwnProperty`, backing `Object.prototype.hasOwnProperty`).
pub(super) fn emit_has_own(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    emit_has_lookup(strings, helpers, true)
}

/// The body of [`emit_has`] (`own` = `false`) and [`emit_has_own`].
fn emit_has_lookup(strings: &HashMap<String, i32>, helpers: Helpers<'_>, own: bool) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (key, obj, o, t, i, a, p) = (0, 1, 2, 3, 4, 5, 6);
    if !own {
        // `key in primitive` is a TypeError.
        emit_is_object(&mut f, obj);
        wasm!(f;
            I32Eqz,
            If(BlockType::Empty),
            LocalGet(obj), LocalGet(obj), I32Const(PROP_THROW_IN), Call(helpers.prop_throw),
            I32Const(0), Return,
            End,
        );
    }
    wasm!(f; LocalGet(key), I32Eqz, If(BlockType::Empty), I32Const(0), Return, End);
    emit_is_pointer(&mut f, obj);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), LocalSet(t),
    );
    emit_is_string_or_array_kind(&mut f, t);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty), I32Const(1), Return, End,
    );
    emit_exotic_index(&mut f, key, i, helpers);
    emit_exotic_len(&mut f, o);
    wasm!(f;
        I32LtU,
        If(BlockType::Empty),
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Empty), I32Const(1), Return, End,
    );
    emit_in_store(&mut f, o, i, a);
    wasm!(f; If(BlockType::Empty));
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), I64Const(HOLE_BITS), I64Ne, Return,
        End,
        End,
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Empty), I32Const(0), Return, End,
        End,
        Loop(BlockType::Empty),
        LocalGet(o), LocalGet(key), Call(helpers.prop_find),
        If(BlockType::Empty), I32Const(1), Return, End,
    );
    if own {
        wasm!(f; I32Const(0), Return);
    }
    wasm!(f;
        LocalGet(o), I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(p),
    );
    emit_is_pointer(&mut f, p);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(p), I32WrapI64, LocalSet(o),
        Br(0),
        End,
        Unreachable,
        End,
    );
    f
}

/// `__delete(obj: i64, key: i32)`: delete the own property `key` of an
/// object-family `obj` by tombstoning its entry (key `0`, value `undefined`,
/// `live` decremented); a non-configurable property throws. On an array an
/// in-range index becomes a hole and `length` (non-configurable) throws; its
/// named keys (and a `SPARSE` array's elements past its store) are table
/// entries. A primitive or string is left unchanged. A delete on a watched
/// table sets its dirty bit when the program tracks them.
pub(super) fn emit_delete(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32)]);
    let (obj, key, o, e, t, i) = (0, 1, 2, 3, 4, 5);
    emit_is_pointer(&mut f, obj);
    wasm!(f;
        I32Eqz, If(BlockType::Empty), Return, End,
        LocalGet(key), I32Eqz, If(BlockType::Empty), Return, End,
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), LocalTee(t),
        I32Const(TYPE_STRING), I32Eq, If(BlockType::Empty), Return, End,
        LocalGet(t), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        LocalGet(key), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty),
    );
    emit_prop_throw_return(&mut f, obj, key, PROP_THROW_DELETE, helpers);
    wasm!(f; End);
    emit_exotic_index(&mut f, key, i, helpers);
    wasm!(f;
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32LtU,
        If(BlockType::Empty),
    );
    // A store slot (any index of a dense array, `i < cap` of a SPARSE one)
    // becomes a hole; a SPARSE element past the store is a table entry.
    emit_is_sparse(&mut f, o);
    wasm!(f;
        If(BlockType::Result(ValType::I32)),
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(e),
        If(BlockType::Result(ValType::I32)),
        LocalGet(i), LocalGet(e), I32Load(m32(ELEMS_CAP_OFFSET)), I32LtU,
        Else, I32Const(0),
        End,
        Else, I32Const(1),
        End,
        If(BlockType::Empty),
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Const(HOLE_BITS), I64Store(m64(ELEMS_DATA_OFFSET)), Return,
        End,
        End,
        End,
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(e), I32Eqz,
        If(BlockType::Empty), Return, End,
        LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz,
        If(BlockType::Empty),
    );
    emit_prop_throw_return(&mut f, obj, key, PROP_THROW_DELETE, helpers);
    wasm!(f;
        End,
        LocalGet(e), I64Const(0), I64Store(m64(0)),
        LocalGet(e), I64Const(UNDEFINED_BITS), I64Store(m64(E_VALUE)),
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(e),
        LocalGet(e), I32Load(m32(P_LIVE)), I32Const(1), I32Sub, I32Store(m32(P_LIVE)),
    );
    if helpers.dirty_tracking {
        emit_mark_dirty(&mut f, e);
    }
    wasm!(f; End);
    f
}

/// `__array_set(recv: i64, key: i64, v: i64)`: the slow path of a member store
/// on the array `recv` (the inline path stores an in-range number index). An
/// array index — a number key or a canonical index string (DF9) — takes the
/// element path; `length` is `__array_set_length`; every other key is an
/// ordinary property set (`__set`). An index at or past the length grows the
/// array densely when `i < max(2 * cap, len + 1024)` (store capacity
/// `max(4, 2 * cap, i + 1)`, the gap `len..i` filled with holes, `len = i + 1`);
/// a larger gap makes the array `SPARSE`, and every element store on a
/// `SPARSE` array is `__sparse_set`. A throw leaves the exception pending.
pub(super) fn emit_array_set(strings: &HashMap<String, i32>, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, ValType::I32), (1, ValType::F64)]);
    let (recv, key, v) = (0, 1, 2);
    let (o, i, k, len, a, d) = (3, 4, 5, 6, 7, 8);
    wasm!(f;
        LocalGet(recv), I32WrapI64, LocalSet(o),
        Block(BlockType::Empty),
    );
    // A number key that is an array index: i = key.
    emit_is_number(&mut f, key);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(key), F64ReinterpretI64, LocalTee(d), F64Const(0.0.into()), F64Ge,
        LocalGet(d), F64Const(f64::from(u32::MAX).into()), F64Lt, I32And,
        LocalGet(d), F64Trunc, LocalGet(d), F64Eq, I32And,
        If(BlockType::Empty), LocalGet(d), I32TruncSatF64U, LocalSet(i), Br(2), End,
        End,
        // Any other key: ToPropertyKey; a canonical index string is the index.
        LocalGet(key), I32Const(1), Call(helpers.to_key), LocalTee(k),
        I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty), LocalGet(k), Call(helpers.index_value), LocalSet(i), Br(1), End,
        LocalGet(k), I32Const(static_key(strings, INDEX_LENGTH_KEY)), I32Eq,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(v), Call(helpers.array_set_length()), Return,
        End,
        LocalGet(recv), LocalGet(k), LocalGet(v), Call(helpers.set), Return,
        End,
    );
    // Element `i`: a SPARSE array stores through `__sparse_set`.
    emit_is_sparse(&mut f, o);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(o), LocalGet(i), LocalGet(v), Call(helpers.sparse_set()), Return,
        End,
        // In range -> store.
        LocalGet(i), LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), LocalTee(len), I32LtU,
        If(BlockType::Empty),
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        LocalGet(v), I64Store(m64(ELEMS_DATA_OFFSET)), Return,
        End,
        // Past the length: dense only below max(2 * cap, len + 1024).
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Result(ValType::I32)),
        LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), I32Const(1), I32Shl,
        Else, I32Const(0),
        End,
        LocalTee(a),
        LocalGet(len), I32Const(1024), I32Add, LocalTee(k),
        LocalGet(a), LocalGet(k), I32GtU, Select,
        LocalGet(i), I32LeU,
        If(BlockType::Empty),
    );
    // Too far: the array turns SPARSE and stores the element there.
    emit_sparsify(&mut f, o, k, a);
    wasm!(f;
        LocalGet(o), LocalGet(i), LocalGet(v), Call(helpers.sparse_set()), Return,
        End,
        LocalGet(o), LocalGet(i), I32Const(1), I32Add, Call(helpers.array_reserve()),
        // Fill len..i with holes, then store element i.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(len), LocalGet(i), I32GeU, BrIf(1),
    );
    emit_elem_addr(&mut f, o, len);
    wasm!(f;
        I64Const(HOLE_BITS), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(len), I32Const(1), I32Add, LocalSet(len),
        Br(0),
        End, End,
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        LocalGet(v), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(o), LocalGet(i), I32Const(1), I32Add, I32Store(m32(ARRAY_LEN_OFFSET)),
        End,
    );
    f
}

/// Fill the store slots `from..to` (i32 locals; `from` ends at `to`) of the
/// array whose i32 base is in local `o` with holes.
fn emit_fill_holes(function: &mut Function, o: u32, from: u32, to: u32) {
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(from), LocalGet(to), I32GeU, BrIf(1),
    );
    emit_elem_addr(function, o, from);
    wasm!(function;
        I64Const(HOLE_BITS), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(from), I32Const(1), I32Add, LocalSet(from),
        Br(0),
        End, End,
    );
}

/// Mark the dense array whose i32 base is in local `o` `SPARSE`, filling its
/// store slots `len..cap` with holes (every slot of a sparse store is an
/// element or a hole). `j`/`a` are i32 scratch.
fn emit_sparsify(function: &mut Function, o: u32, j: u32, a: u32) {
    wasm!(function;
        LocalGet(o), LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_SPARSE), I32Or,
        I32Store8(m8(FLAGS)),
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(j),
        LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), LocalSet(a),
    );
    emit_fill_holes(function, o, j, a);
    wasm!(function; End);
}

/// `__sparse_get(o: i32, i: i32) -> i64`: element `i` of the `SPARSE` array
/// `o`, or [`HOLE_BITS`] when absent — its store slot when `i < cap`, else the
/// value of its own table's array-index key whose value is `i` (an accessor
/// element calls its getter with `this = o`, `undefined` without one).
pub(super) fn emit_sparse_get(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(4, ValType::I32), (2, ValType::I64)]);
    let (o, i, a, e, end, k) = (0, 1, 2, 3, 4, 5);
    let (g, this) = (6, 7);
    wasm!(f;
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Empty),
        LocalGet(i), LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), I32LtU,
        If(BlockType::Empty),
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), Return,
        End,
        End,
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(e), I32Eqz,
        If(BlockType::Empty), I64Const(HOLE_BITS), Return, End,
        LocalGet(e), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(e), I32Add,
        I32Const(P_HEADER), I32Add, LocalSet(end),
        LocalGet(e), I32Const(P_HEADER), I32Add, LocalSet(e),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(e), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(e), I32Load(m32(E_KEY)), LocalTee(k),
        If(BlockType::Empty),
        LocalGet(k), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
        If(BlockType::Empty),
        LocalGet(k), Call(helpers.index_value), LocalGet(i), I32Eq,
        If(BlockType::Empty),
    );
    if helpers.has_accessors {
        wasm!(f;
            LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_ACCESSOR), I32And,
            If(BlockType::Empty),
            LocalGet(e), I64Load(m64(E_VALUE)), I32WrapI64, I64Load(m64(ACCESSOR_GET_OFFSET)),
            LocalSet(g),
        );
        emit_is_function(&mut f, g);
        wasm!(f; I32Eqz, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End, LocalGet(o));
        emit_box_heap(&mut f);
        wasm!(f; LocalSet(this));
        emit_accessor_call(&mut f, g, this, None, k, helpers);
        wasm!(f; Return, End);
    }
    wasm!(f;
        LocalGet(e), I64Load(m64(E_VALUE)), Return,
        End,
        End,
        End,
        LocalGet(e), I32Const(ENTRY), I32Add, LocalSet(e),
        Br(0),
        End, End,
        I64Const(HOLE_BITS),
        End,
    );
    f
}

/// `__sparse_set(o: i32, i: i32, v: i64)`: the strict `[[Set]]` of element
/// `i` of the `SPARSE` array `o` — a store slot when `i < cap`, else `__set`
/// of the interned index key `i` (so a read-only or accessor element and a
/// non-extensible array behave as on any object) — then raise `length` to
/// `i + 1` when `i` is at or past it (a push at the maximum length `2^32 - 1`
/// leaves it, since `i + 1` does not fit). Growing an array whose `length`
/// is read-only throws `Cannot add property i, object is not extensible`. A
/// throw leaves the exception pending and changes nothing.
pub(super) fn emit_sparse_set(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, ValType::I32)]);
    let (o, i, v, a, k) = (0, 1, 2, 3, 4);
    wasm!(f;
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Result(ValType::I32)),
        LocalGet(i), LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), I32LtU,
        Else, I32Const(0),
        End,
        If(BlockType::Empty),
    );
    emit_elem_addr(&mut f, o, i);
    wasm!(f;
        LocalGet(v), I64Store(m64(ELEMS_DATA_OFFSET)),
        Else,
        LocalGet(i), Call(helpers.index_string), Call(helpers.intern), LocalSet(k),
        LocalGet(i), LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_LENGTH_READONLY), I32And, I32Const(0), I32Ne,
        I32And,
        If(BlockType::Empty),
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalGet(k));
    emit_box_heap(&mut f);
    wasm!(f;
        I32Const(PROP_THROW_NOT_EXTENSIBLE), Call(helpers.prop_throw), Return,
        End,
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f;
        LocalGet(k), LocalGet(v), Call(helpers.set),
        GlobalGet(EXC_PENDING_GLOBAL), If(BlockType::Empty), Return, End,
        End,
        LocalGet(i), LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), I32GeU,
        LocalGet(i), I32Const(-1), I32Ne, I32And,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(i), I32Const(1), I32Add, I32Store(m32(ARRAY_LEN_OFFSET)),
        End,
        End,
    );
    f
}

/// `__array_set_length(o: i32, v: i64)`: the `length` write `o.length = v` on
/// the array `o` (`ArraySetLength`). A read-only `length` throws `Cannot
/// assign to read only property 'length'` before any conversion. `n =
/// ToUint32(v)`; when `ToNumber(v) != n` it raises the `RangeError` "Invalid
/// array length" (left pending) and changes nothing. A shrink (`n < len`)
/// turns the slots `n..len` into holes and keeps the store (so a regrowth and
/// a GC walk see no stale element); a `SPARSE` array also drops its index
/// keys `>= n`, except that a non-configurable element `k >= n` stops the
/// deletion: the length becomes `k + 1` (the highest such `k`) and `Cannot
/// delete property 'k'` is left pending. A growth past the length fills holes
/// densely while `n - 1 < max(2 * cap, len + 1024)` and otherwise makes the
/// array `SPARSE` without allocating the gap. Then `len = n`.
#[allow(clippy::too_many_lines)] // one straight-line helper body
pub(super) fn emit_array_set_length(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let (o, v) = (0, 1);
    // The error object is built in the shared helper scratch layout after the
    // two params; this helper's own locals follow it.
    let scratch = 2;
    let mut locals = type_error_helper_locals();
    let base = scratch + locals.iter().map(|(count, _)| count).sum::<u32>();
    locals.extend([(9, ValType::I32), (1, ValType::F64)]);
    let mut f = Function::new(locals);
    let (n, len, a, cap, j, e, end) = (
        base,
        base + 1,
        base + 2,
        base + 3,
        base + 4,
        base + 5,
        base + 6,
    );
    let (m, stopped) = (base + 7, base + 8);
    let d = base + 9;
    let k = j;
    let length_key = static_key(strings, INDEX_LENGTH_KEY);
    // Walk the live array-index entries of the table in `a` (nonzero): `body`
    // runs with the entry address in `e` and its key in `k`.
    let index_entries = |f: &mut Function, body: &dyn Fn(&mut Function)| {
        wasm!(f;
            LocalGet(a), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(a), I32Add,
            I32Const(P_HEADER), I32Add, LocalSet(end),
            LocalGet(a), I32Const(P_HEADER), I32Add, LocalSet(e),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(e), LocalGet(end), I32GeU, BrIf(1),
            LocalGet(e), I32Load(m32(E_KEY)), LocalTee(k),
            If(BlockType::Empty),
            LocalGet(k), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And,
            If(BlockType::Empty),
        );
        body(f);
        wasm!(f;
            End,
            End,
            LocalGet(e), I32Const(ENTRY), I32Add, LocalSet(e),
            Br(0),
            End, End,
        );
    };
    // A read-only `length` rejects any write.
    wasm!(f;
        LocalGet(o), I32Load8U(m8(FLAGS)), I32Const(ARRAY_LENGTH_READONLY), I32And,
        If(BlockType::Empty),
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f;
        I64Const(HEAP_BITS | i64::from(length_key)),
        I32Const(PROP_THROW_READ_ONLY), Call(helpers.prop_throw), Return,
        End,
    );
    // d = ToNumber(v); a value that is not an integer in 0..2^32 is invalid.
    emit_is_number(&mut f, v);
    wasm!(f;
        If(BlockType::Result(ValType::I64)),
        LocalGet(v),
        Else,
        LocalGet(v), Call(helpers.to_number),
        End,
        F64ReinterpretI64, LocalTee(d), F64Const(0.0.into()), F64Ge,
        LocalGet(d), F64Const(4_294_967_296.0.into()), F64Lt, I32And,
        LocalGet(d), F64Trunc, LocalGet(d), F64Eq, I32And,
        I32Eqz,
        If(BlockType::Empty),
    );
    let error = IrNode::error_object(
        ErrorKind::RangeError,
        Some(IrNode::Str(ARRAY_LENGTH_MSG.to_owned())),
        IrNode::Str(ARRAY_LENGTH_STACK.to_owned()),
    );
    emit_node(&error, &mut f, scratch, strings, helpers);
    wasm!(f;
        GlobalSet(EXC_VALUE_GLOBAL),
        I32Const(1), GlobalSet(EXC_PENDING_GLOBAL),
        I32Const(0), GlobalSet(CF_KIND_GLOBAL),
        Return,
        End,
        LocalGet(d), I32TruncSatF64U, LocalSet(n),
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(len),
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(a),
        If(BlockType::Result(ValType::I32)), LocalGet(a), I32Load(m32(ELEMS_CAP_OFFSET)), Else, I32Const(0), End,
        LocalSet(cap),
    );
    // SPARSE: a shrink first raises n past the highest non-configurable
    // element at or above it, then holes the store slots n..min(len, cap) and
    // drops the index keys >= n; the length is set either way.
    emit_is_sparse(&mut f, o);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(n), LocalGet(len), I32LtU,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(a),
        If(BlockType::Empty),
        LocalGet(n), LocalSet(stopped),
    );
    index_entries(&mut f, &|f| {
        wasm!(f;
            LocalGet(k), Call(helpers.index_value), LocalTee(m), LocalGet(n), I32GeU,
            LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_CONFIGURABLE), I32And, I32Eqz,
            I32And,
            If(BlockType::Empty), LocalGet(m), I32Const(1), I32Add, LocalSet(n), End,
        );
    });
    wasm!(f;
        LocalGet(n), LocalGet(stopped), I32Ne, LocalSet(stopped),
        End,
        LocalGet(n), LocalSet(j),
        LocalGet(len), LocalGet(cap), LocalGet(len), LocalGet(cap), I32LtU, Select, LocalSet(end),
    );
    emit_fill_holes(&mut f, o, j, end);
    wasm!(f;
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalTee(a),
        If(BlockType::Empty),
    );
    index_entries(&mut f, &|f| {
        wasm!(f;
            LocalGet(k), Call(helpers.index_value), LocalGet(n), I32GeU,
            If(BlockType::Empty),
            LocalGet(e), I64Const(0), I64Store(m64(0)),
            LocalGet(e), I64Const(UNDEFINED_BITS), I64Store(m64(E_VALUE)),
            LocalGet(a), LocalGet(a), I32Load(m32(P_LIVE)), I32Const(1), I32Sub, I32Store(m32(P_LIVE)),
            End,
        );
    });
    wasm!(f;
        End,
        End,
        LocalGet(o), LocalGet(n), I32Store(m32(ARRAY_LEN_OFFSET)),
        LocalGet(stopped),
        If(BlockType::Empty),
        LocalGet(o),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalGet(n), I32Const(1), I32Sub, Call(helpers.index_string), Call(helpers.intern));
    emit_box_heap(&mut f);
    wasm!(f;
        I32Const(PROP_THROW_DELETE), Call(helpers.prop_throw),
        End,
        Return,
        End,
        // Dense shrink (or no change): hole the slots n..len.
        LocalGet(n), LocalGet(len), I32LeU,
        If(BlockType::Empty),
        LocalGet(n), LocalSet(j),
    );
    emit_fill_holes(&mut f, o, j, len);
    wasm!(f;
        LocalGet(o), LocalGet(n), I32Store(m32(ARRAY_LEN_OFFSET)), Return,
        End,
        // Dense growth below max(2 * cap, len + 1024), else SPARSE.
        LocalGet(n), I32Const(1), I32Sub,
        LocalGet(cap), I32Const(1), I32Shl, LocalTee(j),
        LocalGet(len), I32Const(1024), I32Add, LocalTee(end),
        LocalGet(j), LocalGet(end), I32GtU, Select,
        I32LtU,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(n), Call(helpers.array_reserve()),
        LocalGet(len), LocalSet(j),
    );
    emit_fill_holes(&mut f, o, j, n);
    wasm!(f; Else);
    emit_sparsify(&mut f, o, j, a);
    wasm!(f;
        End,
        LocalGet(o), LocalGet(n), I32Store(m32(ARRAY_LEN_OFFSET)),
        End,
    );
    f
}

/// `__own_keys(obj: i64, mode: i32) -> i64`: a fresh array of `obj`'s own
/// string keys in own-key order (2.6) — integer keys ascending (only when the
/// table has `HAS_INDEX_KEYS`), then the other keys in slot order — filtered
/// to the enumerable ones unless `mode` is [`OWN_ALL`], and mapped to values
/// ([`OWN_VALUES`]) or `[key, value]` pairs ([`OWN_ENTRIES`]). A string lists
/// its indices; an array its present (non-hole) store indices, then its table
/// keys (a `SPARSE` array's elements past the store sort first as integer
/// keys); a primitive has no own keys.
#[allow(clippy::too_many_lines)] // one straight-line helper body
pub(super) fn emit_own_keys(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(11, ValType::I32), (2, ValType::I64)]);
    let (obj, mode) = (0, 1);
    let (o, t, n, out, j, p, e, end, k, i, x) = (2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
    let (kv, val) = (13, 14);
    let new_array = helpers.new_string + 1;
    // include(e) = key != 0 && (mode == ALL || attrs & ENUMERABLE)
    let include = |f: &mut Function| {
        wasm!(f;
            LocalGet(e), I32Load(m32(E_KEY)), LocalTee(k), I32Const(0), I32Ne,
            LocalGet(mode), I32Const(OWN_ALL), I32Eq,
            LocalGet(e), I32Load(m32(E_ATTRS)), I32Const(ATTR_ENUMERABLE), I32And,
            I32Const(0), I32Ne, I32Or,
            I32And,
        );
    };
    // is_index(k) = (has_index & ARRAY_INDEX flag of k), with has_index in `x`.
    let is_index = |f: &mut Function| {
        wasm!(f; LocalGet(k), I32Load8U(m8(FLAGS)), I32Const(STR_ARRAY_INDEX), I32And, LocalGet(x), I32And);
    };
    // Walk the entries: e = p + 16 .. end.
    let entry_loop_head = |f: &mut Function| {
        wasm!(f;
            LocalGet(p), I32Const(P_HEADER), I32Add, LocalSet(e),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(e), LocalGet(end), I32GeU, BrIf(1),
        );
    };
    let entry_loop_tail = |f: &mut Function| {
        wasm!(f;
            LocalGet(e), I32Const(ENTRY), I32Add, LocalSet(e),
            Br(0),
            End, End,
        );
    };

    emit_is_pointer(&mut f, obj);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty), I32Const(0), Call(new_array),
    );
    emit_box_heap(&mut f);
    wasm!(f;
        Return, End,
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), LocalSet(t),
    );
    // The index keys `0..n` (as decimal strings) into `out[j..]`, skipping
    // holes when `skip_holes`.
    let index_keys = |f: &mut Function, skip_holes: bool| {
        wasm!(f;
            I32Const(0), LocalSet(i),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        );
        if skip_holes {
            emit_elem_addr(f, o, i);
            wasm!(f; I64Load(m64(ELEMS_DATA_OFFSET)), I64Const(HOLE_BITS), I64Ne, If(BlockType::Empty));
        }
        emit_elem_addr(f, out, j);
        wasm!(f; LocalGet(i), Call(helpers.index_string));
        emit_box_heap(f);
        wasm!(f;
            I64Store(m64(ELEMS_DATA_OFFSET)),
            LocalGet(j), I32Const(1), I32Add, LocalSet(j),
        );
        if skip_holes {
            wasm!(f; End);
        }
        wasm!(f;
            LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            Br(0),
            End, End,
        );
    };
    wasm!(f;
        I32Const(0), LocalSet(j),
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(LEN)), LocalTee(n), Call(new_array), LocalSet(out),
    );
    index_keys(&mut f, false);
    wasm!(f;
        Else,
        LocalGet(o), I32Load(m32(OBJ_PROPS)), LocalSet(p),
        LocalGet(t), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        // An array: room for every element plus every live table entry. A
        // SPARSE array's store part is `0..min(len, cap)` (its other elements
        // are table keys).
        LocalGet(o), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
    );
    emit_is_sparse(&mut f, o);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(o), I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalTee(i),
        If(BlockType::Result(ValType::I32)), LocalGet(i), I32Load(m32(ELEMS_CAP_OFFSET)), Else, I32Const(0), End,
        LocalTee(i), LocalGet(n), LocalGet(i), LocalGet(n), I32LtU, Select, LocalSet(n),
        End,
        LocalGet(n),
        LocalGet(p), I32Load(m32(P_LIVE)), I32Const(0), LocalGet(p), Select, I32Add,
        Call(new_array), LocalSet(out),
    );
    index_keys(&mut f, true);
    wasm!(f;
        Else,
        LocalGet(p), I32Eqz,
        If(BlockType::Empty), I32Const(0), Call(new_array),
    );
    emit_box_heap(&mut f);
    wasm!(f;
        Return, End,
        LocalGet(p), I32Load(m32(P_LIVE)), Call(new_array), LocalSet(out),
        End,
        LocalGet(p),
        If(BlockType::Empty),
        LocalGet(p), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(p), I32Add,
        I32Const(P_HEADER), I32Add, LocalSet(end),
        LocalGet(p), I32Load8U(m8(P_TFLAGS)), I32Const(HAS_INDEX_KEYS), I32And,
        I32Const(0), I32Ne, I32Const(STR_ARRAY_INDEX), I32Mul, LocalSet(x),
        LocalGet(x),
        If(BlockType::Empty),
    );
    // Pass 1: insertion-sort the included index keys by value into out[0..j].
    entry_loop_head(&mut f);
    include(&mut f);
    is_index(&mut f);
    wasm!(f;
        I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
        LocalGet(j), LocalSet(i),
        LocalGet(k), Call(helpers.index_value), LocalSet(n),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), I32Eqz, BrIf(1),
    );
    // if index_value(out[i - 1]) <= n: break; else out[i] = out[i - 1].
    wasm!(f; LocalGet(out), I32Load(m32(ARRAY_ELEMS_OFFSET)),
        LocalGet(i), I32Const(1), I32Sub, I32Const(3), I32Shl, I32Add,
        I64Load(m64(ELEMS_DATA_OFFSET)), LocalTee(kv), I32WrapI64, Call(helpers.index_value),
        LocalGet(n), I32LeU, BrIf(1),
    );
    emit_elem_addr(&mut f, out, i);
    wasm!(f;
        LocalGet(kv), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(i), I32Const(1), I32Sub, LocalSet(i),
        Br(0),
        End, End,
    );
    emit_elem_addr(&mut f, out, i);
    wasm!(f; LocalGet(k));
    emit_box_heap(&mut f);
    wasm!(f;
        I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(j), I32Const(1), I32Add, LocalSet(j),
        End,
    );
    entry_loop_tail(&mut f);
    wasm!(f; End);
    // Pass 2: the other included keys in slot order.
    entry_loop_head(&mut f);
    include(&mut f);
    is_index(&mut f);
    wasm!(f;
        I32Eqz, I32And,
        If(BlockType::Empty),
    );
    emit_elem_addr(&mut f, out, j);
    wasm!(f; LocalGet(k));
    emit_box_heap(&mut f);
    wasm!(f;
        I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(j), I32Const(1), I32Add, LocalSet(j),
        End,
    );
    entry_loop_tail(&mut f);
    wasm!(f;
        End,
        LocalGet(out), LocalGet(j), I32Store(m32(ARRAY_LEN_OFFSET)),
        End,
        // Values / entries: map each key in place.
        LocalGet(mode), I32Const(OWN_VALUES), I32Eq,
        LocalGet(mode), I32Const(OWN_ENTRIES), I32Eq, I32Or,
        If(BlockType::Empty),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(j), I32GeU, BrIf(1),
    );
    emit_elem_addr(&mut f, out, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), LocalSet(kv),
        LocalGet(t), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Result(ValType::I64)),
        LocalGet(o), LocalGet(i), Call(helpers.str_index),
    );
    emit_box_heap(&mut f);
    // An array's element key (a fresh, uninterned index string) loads the
    // element; a table key goes through `__get`.
    wasm!(f;
        Else,
        LocalGet(t), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Result(ValType::I32)),
        LocalGet(kv), I32WrapI64, Call(helpers.index_value), LocalTee(n), I32Const(-1), I32Ne,
        Else, I32Const(0),
        End,
        If(BlockType::Result(ValType::I64)),
    );
    emit_elem_load(&mut f, o, n, helpers.sparse_get());
    wasm!(f;
        Else,
        LocalGet(obj), LocalGet(kv), I32WrapI64, Call(helpers.get),
        End,
        End,
        LocalSet(val),
        LocalGet(mode), I32Const(OWN_ENTRIES), I32Eq,
        If(BlockType::Empty),
        I32Const(2), Call(new_array), LocalTee(x),
        I32Load(m32(ARRAY_ELEMS_OFFSET)), LocalGet(kv), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(x), I32Load(m32(ARRAY_ELEMS_OFFSET)),
        LocalGet(val), I64Store(m64(ELEMS_DATA_OFFSET + 8)),
        LocalGet(x),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(val), End);
    emit_elem_addr(&mut f, out, i);
    wasm!(f;
        LocalGet(val), I64Store(m64(ELEMS_DATA_OFFSET)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
        LocalGet(out),
    );
    emit_box_heap(&mut f);
    wasm!(f; End);
    f
}

/// Push `1` when the i64 jsval in local `needle` is an element of the array
/// whose base is in i32 local `arr`, else `0` (`i`/`n` are i32 scratch).
pub(super) fn emit_array_contains(function: &mut Function, arr: u32, needle: u32, i: u32, n: u32) {
    wasm!(function;
        LocalGet(arr), I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Result(ValType::I32)), Loop(BlockType::Empty),
        I32Const(0), LocalGet(i), LocalGet(n), I32GeU, BrIf(1), Drop,
    );
    emit_elem_addr(function, arr, i);
    wasm!(function;
        I64Load(m64(ELEMS_DATA_OFFSET)), LocalGet(needle), I64Eq,
        If(BlockType::Empty), I32Const(1), Br(2), End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End,
        Unreachable,
        End,
    );
}

/// `__for_in_keys(obj: i64) -> i64`: the `for-in` key snapshot — `obj`'s own
/// enumerable string keys, then each prototype's enumerable keys not already
/// seen (a key seen at a nearer level, enumerable or not, shadows it). An
/// object whose prototypes have no enumerable property (an ordinary object
/// over `%Object.prototype%`) is just its own keys.
pub(super) fn emit_for_in_keys(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(6, ValType::I32), (4, ValType::I64)]);
    let obj = 0;
    let (o, ks, n, i, j, m) = (1, 2, 3, 4, 5, 6);
    let (out, seen, cur, k) = (7, 8, 9, 10);
    let new_array = helpers.new_string + 1;
    emit_is_pointer(&mut f, obj);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(obj), I32WrapI64, LocalTee(o), I32Load8U(m8(0)), LocalSet(i),
    );
    emit_is_string_or_array_kind(&mut f, i);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(cur),
        // j = whether some prototype may contribute a key: a string/array
        // (exotic keys) or a table holding a live enumerable entry.
        I32Const(0), LocalSet(j),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    emit_is_pointer(&mut f, cur);
    wasm!(f;
        I32Eqz, BrIf(1),
        LocalGet(cur), I32WrapI64, LocalTee(m), I32Load8U(m8(0)), LocalSet(i),
    );
    emit_is_string_or_array_kind(&mut f, i);
    wasm!(f;
        If(BlockType::Empty), I32Const(1), LocalSet(j), Br(2), End,
        LocalGet(m), I32Load(m32(OBJ_PROPS)), LocalTee(n),
        If(BlockType::Empty),
        LocalGet(n), I32Load(m32(P_COUNT)), I32Const(4), I32Shl, LocalGet(n), I32Add,
        I32Const(P_HEADER), I32Add, LocalSet(i),
        LocalGet(n), I32Const(P_HEADER), I32Add, LocalSet(ks),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(ks), LocalGet(i), I32GeU, BrIf(1),
        LocalGet(ks), I32Load(m32(E_KEY)), I32Const(0), I32Ne,
        LocalGet(ks), I32Load(m32(E_ATTRS)), I32Const(ATTR_ENUMERABLE), I32And, I32Const(0), I32Ne,
        I32And,
        If(BlockType::Empty), I32Const(1), LocalSet(j), Br(5), End,
        LocalGet(ks), I32Const(ENTRY), I32Add, LocalSet(ks),
        Br(0),
        End, End,
        End,
        LocalGet(m), I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(cur),
        Br(0),
        End, End,
        LocalGet(j),
        If(BlockType::Empty),
        I32Const(0), Call(new_array),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(out), I32Const(0), Call(new_array));
    emit_box_heap(&mut f);
    wasm!(f;
        LocalSet(seen),
        LocalGet(obj), LocalSet(cur),
        Block(BlockType::Empty), Loop(BlockType::Empty),
    );
    emit_is_pointer(&mut f, cur);
    wasm!(f;
        I32Eqz, BrIf(1),
        LocalGet(cur), I32Const(OWN_ALL), Call(helpers.own_keys), I32WrapI64, LocalTee(ks),
        I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    emit_elem_addr(&mut f, ks, i);
    wasm!(f; I64Load(m64(ELEMS_DATA_OFFSET)), LocalSet(k), LocalGet(seen), I32WrapI64, LocalSet(o));
    emit_array_contains(&mut f, o, k, j, m);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(o), LocalGet(k), Call(helpers.array_push), Drop,
        LocalGet(cur), I32WrapI64, LocalGet(k), I32WrapI64, Call(helpers.prop_find),
        I32Load(m32(E_ATTRS)), I32Const(ATTR_ENUMERABLE), I32And,
        If(BlockType::Empty),
        LocalGet(out), I32WrapI64, LocalGet(k), Call(helpers.array_push), Drop,
        End,
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(cur), I32WrapI64, I64Load(m64(OBJ_PROTO_OFFSET)), LocalSet(cur),
        Br(0),
        End, End,
        LocalGet(out), Return,
        End,
        End,
        End,
        LocalGet(obj), I32Const(OWN_KEYS), Call(helpers.own_keys),
        End,
    );
    f
}

/// `__rest(src: i64, excl: i64) -> i64`: the object-rest copy — a fresh
/// object defining each own enumerable property of `src` (in own-key order)
/// whose key is not an element of the `excl` array of interned key strings.
pub(super) fn emit_rest(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, ValType::I32), (1, ValType::I64)]);
    let (src, excl) = (0, 1);
    let (dst, ks, n, i, x, j, m) = (2, 3, 4, 5, 6, 7, 8);
    let k = 9;
    wasm!(f;
        I32Const(abi::OBJ_PREFIX_SIZE.cast_signed()), Call(helpers.alloc), LocalSet(dst),
    );
    emit_object_prefix(
        &mut f,
        dst,
        TYPE_OBJECT,
        intrinsic_bits(Intrinsic::ObjectPrototype, helpers),
    );
    wasm!(f;
        LocalGet(src), I32Const(OWN_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(ks),
        I32Load(m32(ARRAY_LEN_OFFSET)), LocalSet(n),
        LocalGet(excl), I32WrapI64, LocalSet(x),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    emit_elem_addr(&mut f, ks, i);
    wasm!(f;
        I64Load(m64(ELEMS_DATA_OFFSET)), I32Const(1), Call(helpers.to_key), LocalTee(j),
    );
    emit_box_heap(&mut f);
    wasm!(f; LocalSet(k));
    emit_array_contains(&mut f, x, k, j, m);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty),
        LocalGet(dst), LocalGet(k), I32WrapI64,
        LocalGet(src), LocalGet(k), I32WrapI64, Call(helpers.get),
        I32Const(ATTRS_DEFAULT), Call(helpers.define),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(dst),
    );
    emit_box_heap(&mut f);
    wasm!(f; End);
    f
}
