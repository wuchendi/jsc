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

/// Emit the `__alloc(size: i32) -> i32` bump allocator: return the current
/// `__heap_ptr`, then advance it by `size` rounded up to [`BLOCK_ALIGN`] (so
/// every block base stays 8-aligned), growing linear memory on demand, and write
/// the block's [`HEADER_SIZE`] header bytes 0 (the type byte the caller then
/// stores, plus the reserved `gc`/`flags`/`class` bytes). Every caller requests
/// at least a header, so `size > 0`.
///
/// The end offset is computed in `i64` (both operands zero-extended), so neither
/// the rounding nor the add can wrap; the fast path is one compare against
/// `memory.size * 64 KiB`. On
/// overflow the cold path grows by `max(needed, current)` pages (at least
/// doubling), falling back to exactly the needed pages. When even that fails
/// (the module's memory `maximum` is reached) it stores the sticky
/// [`abi::HEAP_PTR_OOM`] sentinel in `__heap_ptr` and traps; the run stage maps
/// the sentinel to `Uncaught RangeError: out of memory`. The sentinel also makes
/// every later allocation fail. The `i32` param is local 0; extra locals hold the
/// old pointer (i32), the end offset (i64), and the needed page count (i32).
pub(super) fn emit_alloc() -> Function {
    let mut function = Function::new(vec![
        (1, ValType::I32),
        (1, ValType::I64),
        (1, ValType::I32),
    ]);
    let (size, old, end, need) = (0, 1, 2, 3);
    // old = __heap_ptr; end = u64(old) + ((u64(size) + 7) & !7).
    function.instruction(&Instruction::GlobalGet(HEAP_PTR_GLOBAL));
    function.instruction(&Instruction::LocalTee(old));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::LocalGet(size));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(i64::from(BLOCK_ALIGN - 1)));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::I64Const(-i64::from(BLOCK_ALIGN)));
    function.instruction(&Instruction::I64And);
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::LocalTee(end));
    // if end > memory.size << 16: grow.
    function.instruction(&Instruction::MemorySize(0));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(16));
    function.instruction(&Instruction::I64Shl);
    function.instruction(&Instruction::I64GtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // need = ceil(end / 64 KiB) - memory.size (fits i32: end < 2^33).
    function.instruction(&Instruction::LocalGet(end));
    function.instruction(&Instruction::I64Const(0xFFFF));
    function.instruction(&Instruction::I64Add);
    function.instruction(&Instruction::I64Const(16));
    function.instruction(&Instruction::I64ShrU);
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::MemorySize(0));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(need));
    // Try memory.grow(max(need, memory.size)).
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::MemorySize(0));
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::MemorySize(0));
    function.instruction(&Instruction::I32GtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::MemoryGrow(0));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // Doubling failed: fall back to exactly `need` pages.
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::MemoryGrow(0));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    // Out of memory: store the sticky sentinel and trap.
    function.instruction(&Instruction::I32Const(abi::HEAP_PTR_OOM));
    function.instruction(&Instruction::GlobalSet(HEAP_PTR_GLOBAL));
    function.instruction(&Instruction::Unreachable);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // __heap_ptr = end; zero the header at old; return old.
    function.instruction(&Instruction::LocalGet(end));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::GlobalSet(HEAP_PTR_GLOBAL));
    function.instruction(&Instruction::LocalGet(old));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(old));
    function.instruction(&Instruction::End);
    function
}

/// Emit `__alloc(size)` for a block of kind `type_byte`, store the type byte,
/// and leave the base in local `base`.
fn emit_new_block(function: &mut Function, alloc: u32, base: u32, type_byte: i32) {
    function.instruction(&Instruction::Call(alloc));
    function.instruction(&Instruction::LocalTee(base));
    function.instruction(&Instruction::I32Const(type_byte));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

/// Emit the exported `__new_string(len: i32, two_byte: i32) -> i32`
/// constructor: a [`TYPE_STRING`] block of `len` code units — Latin-1 bytes,
/// or UTF-16LE units with `STR_FLAG_TWO_BYTE` when `two_byte` (`0`/`1`) —
/// whose data the host fills. Locals 0/1 are the params; local 2 the base.
pub(super) fn emit_new_string(alloc: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (len, two_byte, base) = (0, 1, 2);
    emit_alloc_str(
        &mut function,
        alloc,
        |f| wasm!(f; LocalGet(len)),
        |f| wasm!(f; LocalGet(two_byte)),
        base,
    );
    wasm!(function; LocalGet(base), End);
    function
}

/// The `STR_FLAG_TWO_BYTE` bit of a string block's flags byte.
const TWO_BYTE: i32 = abi::STR_FLAG_TWO_BYTE as i32;

/// A byte [`MemArg`] at `offset`.
fn mem8(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

/// A 2-byte-aligned `u16` [`MemArg`] at `offset`.
fn mem16(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 1,
        memory_index: 0,
    }
}

/// The string data offset as a [`MemArg`] offset.
const DATA: u64 = abi::STR_DATA_OFFSET as u64;

/// `s = __alloc(STR_DATA_OFFSET + (len << w))` as a [`TYPE_STRING`] block of
/// `len` code units (pushed by `len`), `TWO_BYTE` when `w` (pushes `0`/`1`),
/// leaving the base in i32 local `s`.
pub(super) fn emit_alloc_str(
    function: &mut Function,
    alloc: u32,
    len: impl Fn(&mut Function),
    w: impl Fn(&mut Function),
    s: u32,
) {
    len(function);
    w(function);
    wasm!(function; I32Shl, I32Const(STR_DATA_OFFSET), I32Add, Call(alloc), LocalTee(s));
    wasm!(function; I32Const((TWO_BYTE << 16) | TYPE_STRING), I32Const(TYPE_STRING));
    w(function);
    wasm!(function; Select, I32Store(mem32(0)), LocalGet(s));
    len(function);
    wasm!(function; I32Store(mem32(STR_LEN_OFFSET)));
}

/// Push `1` when the string block in i32 local `s` is `TWO_BYTE`, else `0`.
pub(super) fn emit_str_two_byte(function: &mut Function, s: u32) {
    wasm!(function;
        LocalGet(s), I32Load8U(mem8(u64::from(abi::HEADER_FLAGS_OFFSET))),
        I32Const(TWO_BYTE), I32And, I32Const(0), I32Ne,
    );
}

/// Push code unit `i` (i32 local) of the string block in i32 local `s`, read
/// at the width in i32 local `w` (`0` one-byte, `1` two-byte).
pub(super) fn emit_str_unit(function: &mut Function, s: u32, i: u32, w: u32) {
    wasm!(function;
        LocalGet(w),
        If(BlockType::Result(ValType::I32)),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        Else,
        LocalGet(s), LocalGet(i), I32Add, I32Load8U(mem8(DATA)),
        End,
    );
}

/// Push `1` when units `i` and `i + 1` (i32 local `i`; `i + 1` checked
/// against the length in i32 local `len`) of the two-byte string block in
/// i32 local `s` are a lead (`D800–DBFF`) and a trail (`DC00–DFFF`)
/// surrogate, else `0`.
pub(super) fn emit_surrogate_pair_at(function: &mut Function, s: u32, i: u32, len: u32) {
    wasm!(function;
        LocalGet(i), I32Const(1), I32Add, LocalGet(len), I32LtU,
        If(BlockType::Result(ValType::I32)),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        I32Const(0xFC00), I32And, I32Const(0xD800), I32Eq,
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA + 2)),
        I32Const(0xFC00), I32And, I32Const(0xDC00), I32Eq, I32And,
        Else, I32Const(0), End,
    );
}

/// `dst` = a fresh two-unit, two-byte string of units `i` and `i + 1` (i32
/// local `i`) of the two-byte string block in i32 local `s`.
pub(super) fn emit_str_pair(function: &mut Function, alloc: u32, s: u32, i: u32, dst: u32) {
    emit_alloc_str(
        function,
        alloc,
        |f| wasm!(f; I32Const(2)),
        |f| wasm!(f; I32Const(1)),
        dst,
    );
    wasm!(function;
        LocalGet(dst),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add,
        I32Load(MemArg { offset: DATA, align: 1, memory_index: 0 }),
        I32Store(mem32(DATA)),
    );
}

/// Store the code unit pushed by `unit` as unit `i` (i32 local) of the string
/// block in i32 local `s` of the width in i32 local `w`.
pub(super) fn emit_store_unit(
    function: &mut Function,
    s: u32,
    i: u32,
    w: u32,
    unit: impl Fn(&mut Function),
) {
    wasm!(function;
        LocalGet(w),
        If(BlockType::Empty),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add,
    );
    unit(function);
    wasm!(function; I32Store16(mem16(DATA)), Else, LocalGet(s), LocalGet(i), I32Add);
    unit(function);
    wasm!(function; I32Store8(mem8(DATA)), End);
}

/// [`emit_str_narrow`] the string block in i32 local `s` when the i32 local
/// `w` (its width) is set.
pub(super) fn emit_str_canonical(function: &mut Function, w: u32, s: u32, i: u32, n: u32) {
    wasm!(function; LocalGet(w), If(BlockType::Empty));
    emit_str_narrow(function, s, i, n);
    wasm!(function; End);
}

/// Narrow the `TWO_BYTE` string block in i32 local `s` in place to one-byte
/// when every code unit is `<= 0xFF` (the canonical form); `i` and `n` are
/// i32 scratch.
pub(super) fn emit_str_narrow(function: &mut Function, s: u32, i: u32, n: u32) {
    let flags = mem8(u64::from(abi::HEADER_FLAGS_OFFSET));
    wasm!(function;
        LocalGet(s), I32Load(mem32(STR_LEN_OFFSET)), LocalSet(n),
        Block(BlockType::Empty),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        I32Const(0xFF), I32GtU, BrIf(2),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        // Every unit fits a byte: unit `i` moves to byte `i` (`i <= 2i`, so
        // nothing unread is overwritten).
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(s), LocalGet(i), I32Add,
        LocalGet(s), LocalGet(i), I32Const(1), I32Shl, I32Add, I32Load16U(mem16(DATA)),
        I32Store8(mem8(DATA)),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(s), LocalGet(s), I32Load8U(flags), I32Const(!TWO_BYTE), I32And, I32Store8(flags),
        End,
    );
}

/// A 4-byte-aligned `u32` [`MemArg`] at `offset`.
fn mem32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

/// Replace the string block base on the stack with its u32 `len`.
pub(super) fn emit_load_str_len(function: &mut Function) {
    function.instruction(&Instruction::I32Load(mem32(STR_LEN_OFFSET)));
}

/// Replace the string block base on the stack with the address of its data.
pub(super) fn emit_str_data(function: &mut Function) {
    function.instruction(&Instruction::I32Const(STR_DATA_OFFSET));
    function.instruction(&Instruction::I32Add);
}

/// An 8-byte-aligned `i64` [`MemArg`] at `offset`.
fn mem64(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

/// Emit `store = __alloc(8 + 8 * cap)` as a [`TYPE_ELEMS`] block with its
/// `cap` written, leaving the base in local `store`.
fn emit_new_elems(function: &mut Function, alloc: u32, cap: u32, store: u32) {
    function.instruction(&Instruction::LocalGet(cap));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    emit_new_block(function, alloc, store, TYPE_ELEMS);
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::LocalGet(cap));
    function.instruction(&Instruction::I32Store(mem32(ELEMS_CAP_OFFSET)));
}

/// Emit the exported `__new_array(len: i32) -> i32` constructor: a
/// [`TYPE_ARRAY`] handle (`proto` = `%Array.prototype%`, the jsval `proto`)
/// with `len` set and, when `len > 0`, a `cap = len`
/// [`TYPE_ELEMS`] store whose every element is `undefined`. Local 0 is `len`;
/// locals 1/2/3 the handle base, the store base, and the element cursor.
pub(super) fn emit_new_array(alloc: u32, proto: i64) -> Function {
    let mut function = Function::new(vec![(3, ValType::I32)]);
    let (len, base, store, at) = (0, 1, 2, 3);
    function.instruction(&Instruction::I32Const(ARRAY_BLOCK_SIZE));
    function.instruction(&Instruction::Call(alloc));
    function.instruction(&Instruction::LocalSet(base));
    emit_object_prefix(&mut function, base, TYPE_ARRAY, proto);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::I32Store(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_new_elems(&mut function, alloc, len, store);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::I32Store(mem32(ARRAY_ELEMS_OFFSET)));
    // for (at = store; at < store + 8 * len; at += 8) *(at + 8) = undefined.
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::LocalSet(at));
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Store(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalGet(at));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(at));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::End);
    function
}

/// Emit the exported `__new_promise() -> i32` constructor: a pending
/// [`abi::TYPE_PROMISE`] block with prototype `proto` (`%Promise.prototype%`),
/// `state = PENDING`, and
/// `value = undefined`. Local 0 is the base.
pub(super) fn emit_new_promise(alloc: u32, proto: i64) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let base = 0;
    function.instruction(&Instruction::I32Const(PROMISE_BLOCK_SIZE));
    function.instruction(&Instruction::Call(alloc));
    function.instruction(&Instruction::LocalSet(base));
    emit_object_prefix(&mut function, base, i32::from(abi::TYPE_PROMISE), proto);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Const(abi::PROMISE_PENDING));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: PROMISE_STATE_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: PROMISE_VALUE_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__invoke1(fn: i64, arg: i64) -> i64` callback trampoline.
///
/// The host promise engine calls this exported function re-entrantly to run a
/// `.then`/`.catch`/`.finally` reaction's guest callback `fn` with the settled
/// value `arg`. It mirrors [`super::emit_call_indirect`]'s env-last ABI: push the
/// single real argument into slot `a0` (padding the remaining `K-1` positional
/// slots with `undefined`), reset `this`, push the callee's own environment, the
/// argument count (`1`), and the funcref-table index, then `call_indirect`. The
/// callback's result is left on the stack; a throw inside it sets `__exc_pending`
/// (which the host checks after the call), so no propagation is emitted here.
/// The two `i64` params occupy locals 0/1; one extra `i32` local holds the
/// callee block base.
pub(super) fn emit_invoke1(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (callee, arg, base) = (0, 1, 2);
    // base = i32.wrap(callee).
    function.instruction(&Instruction::LocalGet(callee));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(base));
    // K padded positional slots: `arg` fills `a0`, the rest are `undefined`.
    let k = helpers.max_args;
    if k >= 1 {
        function.instruction(&Instruction::LocalGet(arg));
    }
    for _ in 1..k {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    // A callback runs as a plain call: bind `this = undefined`.
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::GlobalSet(THIS_GLOBAL));
    // Trailing env argument: the callee's own environment (at FUNC_ENV_OFFSET).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: FUNC_ENV_OFFSET,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI): exactly one, as a number jsval.
    function.instruction(&Instruction::I64Const(1.0_f64.to_bits().cast_signed()));
    // Funcref-table index (at FUNC_INDEX_OFFSET), on top for call_indirect.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: FUNC_INDEX_OFFSET,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::CallIndirect {
        type_index: helpers.fn_type,
        table_index: 0,
    });
    function.instruction(&Instruction::End);
    function
}

/// Emit a `new Promise` settler function: the `resolve` (`op` =
/// `PROMISE_OP_SETTLE_FULFILL`) or `reject` (`PROMISE_OP_SETTLE_REJECT`) half of
/// an executor's callback pair.
///
/// It shares the uniform function ABI `(a0..a_{K-1}, env, argc) -> i64` and is
/// called through the funcref table like any function value, with the TARGET
/// PROMISE carried in its `env` slot (local `K`) — `new Promise` builds the
/// function value that way instead of a closure environment. The settler reads
/// the bound promise from `env` and its first argument (`a0`, or `undefined` when
/// the ABI has no positional slot) as the settlement value, calls
/// `__promise(op, promise, value)` to drive the host settle logic, and returns
/// `undefined`. `K = helpers.max_args`; no extra locals are declared.
pub(super) fn emit_settler(op: i32, helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![]);
    let k = helpers.max_args;
    // __promise(op, env /* the bound promise */, value): push op, then P, then v.
    function.instruction(&Instruction::I32Const(op));
    function.instruction(&Instruction::LocalGet(k)); // env slot = the bound promise
    if k >= 1 {
        function.instruction(&Instruction::LocalGet(0)); // a0 = the settlement value
    } else {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    }
    function.instruction(&Instruction::Call(helpers.promise));
    function.instruction(&Instruction::Drop);
    // A settler returns `undefined` (its call result is ignored anyway).
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__concat(a: i32, b: i32) -> i32` string concatenator.
///
/// An empty operand returns the other one (strings are immutable). Otherwise
/// it allocates a block of `lenA + lenB` code units, two-byte iff either
/// operand is (so the result is canonical), and copies both operands' units
/// into place: `memory.copy` when an operand has the result's width, a
/// widening unit loop for a one-byte operand of a two-byte result. Params
/// occupy locals 0/1; extra `i32` locals hold `lenA`, `lenB`, the result
/// width, `dst`, and the loop index.
pub(super) fn emit_concat(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(5, ValType::I32)]);
    let (a, b, len_a, len_b, w, dst, i) = (0, 1, 2, 3, 4, 5, 6);
    wasm!(function; LocalGet(a));
    emit_load_str_len(&mut function);
    wasm!(function;
        LocalTee(len_a), I32Eqz, If(BlockType::Empty), LocalGet(b), Return, End,
        LocalGet(b),
    );
    emit_load_str_len(&mut function);
    wasm!(function; LocalTee(len_b), I32Eqz, If(BlockType::Empty), LocalGet(a), Return, End);
    emit_str_two_byte(&mut function, a);
    emit_str_two_byte(&mut function, b);
    wasm!(function; I32Or, LocalSet(w));
    emit_alloc_str(
        &mut function,
        alloc_index,
        |f| wasm!(f; LocalGet(len_a), LocalGet(len_b), I32Add),
        |f| wasm!(f; LocalGet(w)),
        dst,
    );
    // Copy `len` units of `src` to unit offset `at` (pushed) of `dst`.
    let copy = |f: &mut Function, src: u32, len: u32, at: &dyn Fn(&mut Function)| {
        wasm!(f; LocalGet(w));
        emit_str_two_byte(f, src);
        wasm!(f; I32Eq, If(BlockType::Empty), LocalGet(dst));
        at(f);
        wasm!(f;
            LocalGet(w), I32Shl, I32Add, I32Const(STR_DATA_OFFSET), I32Add,
            LocalGet(src), I32Const(STR_DATA_OFFSET), I32Add,
            LocalGet(len), LocalGet(w), I32Shl,
            MemoryCopy { src_mem: 0, dst_mem: 0 },
            Else,
            // A one-byte operand of a two-byte result: widen each unit.
            I32Const(0), LocalSet(i),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
            LocalGet(dst),
        );
        at(f);
        wasm!(f;
            LocalGet(i), I32Add, I32Const(1), I32Shl, I32Add,
            LocalGet(src), LocalGet(i), I32Add, I32Load8U(mem8(DATA)),
            I32Store16(mem16(DATA)),
            LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            Br(0),
            End, End,
            End,
        );
    };
    copy(&mut function, a, len_a, &|f| wasm!(f; I32Const(0)));
    copy(&mut function, b, len_b, &|f| wasm!(f; LocalGet(len_a)));
    wasm!(function; LocalGet(dst), End);
    function
}

/// Emit the `__str_eq(a: i32, b: i32) -> i32` content string comparator.
///
/// Returns `1` when the two strings hold the same code units, else `0`: the
/// same block is equal; differing `TWO_BYTE` flags (canonical strings of
/// different widths never match) or lengths fail immediately; otherwise the
/// `len << two_byte` data bytes are compared 8 at a time with `i64.load`,
/// then the tail byte by byte. Params occupy locals 0/1; extra `i32` locals
/// hold the data byte count and the byte offset.
pub(super) fn emit_str_eq() -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (a, b, n, p) = (0, 1, 2, 3);
    let flags = mem8(u64::from(abi::HEADER_FLAGS_OFFSET));
    wasm!(function;
        LocalGet(a), LocalGet(b), I32Eq, If(BlockType::Empty), I32Const(1), Return, End,
        LocalGet(a), I32Load8U(flags), LocalGet(b), I32Load8U(flags), I32Xor,
        I32Const(TWO_BYTE), I32And,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(a), I32Load(mem32(STR_LEN_OFFSET)), LocalTee(n),
        LocalGet(b), I32Load(mem32(STR_LEN_OFFSET)), I32Ne,
        If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(n),
    );
    emit_str_two_byte(&mut function, a);
    wasm!(function;
        I32Shl, LocalSet(n),
        // Eight bytes at a time; `p` starts at 0.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), I32Const(8), I32Add, LocalGet(n), I32GtU, BrIf(1),
        LocalGet(a), LocalGet(p), I32Add, I64Load(mem64(DATA)),
        LocalGet(b), LocalGet(p), I32Add, I64Load(mem64(DATA)),
        I64Ne, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(p), I32Const(8), I32Add, LocalSet(p),
        Br(0),
        End, End,
        // The tail.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(a), LocalGet(p), I32Add, I32Load8U(mem8(DATA)),
        LocalGet(b), LocalGet(p), I32Add, I32Load8U(mem8(DATA)),
        I32Ne, If(BlockType::Empty), I32Const(0), Return, End,
        LocalGet(p), I32Const(1), I32Add, LocalSet(p),
        Br(0),
        End, End,
        I32Const(1),
        End,
    );
    function
}

/// Emit the `__str_cmp(a: i32, b: i32) -> i32` code-unit order comparator.
///
/// Returns `-1` / `0` / `1`: the code units are compared over
/// `min(lenA, lenB)`, and the first differing pair decides the sign (unsigned
/// unit order); if the shared prefix is equal, the shorter string is "less"
/// (sign of `lenA - lenB`). Two one-byte strings compare bytes; otherwise each
/// side's units are read at its own width. Params occupy locals 0/1; extra
/// `i32` locals hold `lenA`, `lenB`, `min`, the loop index, the two current
/// units, and the two widths.
pub(super) fn emit_str_cmp() -> Function {
    let mut function = Function::new(vec![(8, ValType::I32)]);
    let (a, b, len_a, len_b, min, i) = (0, 1, 2, 3, 4, 5);
    let (unit_a, unit_b, w_a, w_b) = (6, 7, 8, 9);
    wasm!(function;
        LocalGet(a), I32Load(mem32(STR_LEN_OFFSET)), LocalSet(len_a),
        LocalGet(b), I32Load(mem32(STR_LEN_OFFSET)), LocalSet(len_b),
        LocalGet(len_a), LocalGet(len_b), LocalGet(len_a), LocalGet(len_b), I32LtU, Select,
        LocalSet(min),
    );
    emit_str_two_byte(&mut function, a);
    wasm!(function; LocalSet(w_a));
    emit_str_two_byte(&mut function, b);
    wasm!(function; LocalSet(w_b));
    // Compare the shared prefix, returning on the first differing pair.
    let prefix = |f: &mut Function, load: &dyn Fn(&mut Function, u32, u32)| {
        wasm!(f;
            I32Const(0), LocalSet(i),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), LocalGet(min), I32GeU, BrIf(1),
        );
        load(f, a, w_a);
        wasm!(f; LocalSet(unit_a));
        load(f, b, w_b);
        wasm!(f;
            LocalTee(unit_b), LocalGet(unit_a), I32Ne,
            If(BlockType::Empty),
            I32Const(-1), I32Const(1), LocalGet(unit_a), LocalGet(unit_b), I32LtU, Select, Return,
            End,
            LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            Br(0),
            End, End,
        );
    };
    wasm!(function; LocalGet(w_a), LocalGet(w_b), I32Or, If(BlockType::Empty));
    prefix(&mut function, &|f, s, w| emit_str_unit(f, s, i, w));
    wasm!(function; Else);
    prefix(&mut function, &|f, s, _| {
        wasm!(f; LocalGet(s), LocalGet(i), I32Add, I32Load8U(mem8(DATA)));
    });
    wasm!(function;
        End,
        // Prefixes equal: sign(lenA - lenB) = (lenA > lenB) - (lenA < lenB).
        LocalGet(len_a), LocalGet(len_b), I32GtU,
        LocalGet(len_a), LocalGet(len_b), I32LtU,
        I32Sub,
        End,
    );
    function
}

/// Emit the `__str_index(ptr: i32, i: i32) -> i32` one-unit string builder.
///
/// `ptr` is a string block; `i` is a code-unit index assumed in bounds
/// (`0 <= i < len`). It reads unit `i` at the string's width and returns a
/// fresh one-unit string in canonical form: one-byte when the unit is
/// `<= 0xFF`, else two-byte. The two `i32` parameters occupy locals 0/1;
/// extra `i32` locals hold `dst`, the unit, and the width.
pub(super) fn emit_str_index(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(3, ValType::I32)]);
    let (ptr, i, dst, unit, w) = (0, 1, 2, 3, 4);
    emit_str_two_byte(&mut function, ptr);
    wasm!(function; LocalSet(w));
    emit_str_unit(&mut function, ptr, i, w);
    wasm!(function; LocalTee(unit), I32Const(0xFF), I32GtU, LocalSet(w));
    emit_alloc_str(
        &mut function,
        alloc_index,
        |f| wasm!(f; I32Const(1)),
        |f| wasm!(f; LocalGet(w)),
        dst,
    );
    wasm!(function; I32Const(0), LocalSet(i));
    emit_store_unit(&mut function, dst, i, w, |f| wasm!(f; LocalGet(unit)));
    wasm!(function; LocalGet(dst), End);
    function
}

/// Emit the `__array_reserve(base: i32, need: i32)` store grower.
///
/// When the array handle's capacity (the `cap` of its [`TYPE_ELEMS`] store, `0`
/// with no store) is below `need`, it allocates a fresh store of
/// `cap' = max(need, 4, 2 * cap)` slots, `memory.copy`s the `len` live slots
/// into it, and repoints the handle's `elems`; the handle never moves. Locals
/// 0/1 are the params; extra `i32` locals hold the old store, `cap`, and the
/// new store.
pub(super) fn emit_array_reserve(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(3, ValType::I32)]);
    let (base, need, old, cap, store) = (0, 1, 2, 3, 4);

    // old = base.elems; cap = old ? old.cap : 0.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalTee(old));
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(old));
    function.instruction(&Instruction::I32Load(mem32(ELEMS_CAP_OFFSET)));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalTee(cap));
    // if cap >= need: nothing to do.
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);

    // cap = max(4, 2 * cap); cap = max(cap, need).
    function.instruction(&Instruction::LocalGet(cap));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::LocalTee(cap));
    function.instruction(&Instruction::I32Const(4));
    function.instruction(&Instruction::LocalGet(cap));
    function.instruction(&Instruction::I32Const(4));
    function.instruction(&Instruction::I32GtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalTee(cap));
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::LocalGet(cap));
    function.instruction(&Instruction::LocalGet(need));
    function.instruction(&Instruction::I32GtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(cap));
    emit_new_elems(&mut function, alloc_index, cap, store);

    // if old: memory.copy(store + 8, old + 8, 8 * len).
    function.instruction(&Instruction::LocalGet(old));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(old));
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    function.instruction(&Instruction::End);

    // base.elems = store.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::I32Store(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__array_push(base: i32, value: i64) -> i32` in-place appender:
/// `__array_reserve(base, len + 1)`, store `value` at slot `len` of the
/// element store, bump `len`, and return the (unchanged) handle base; a
/// `SPARSE` array appends through `__sparse_set` (function `sparse_set`). The
/// two params occupy locals 0/1; an extra `i32` local holds `len`.
pub(super) fn emit_array_push(reserve_index: u32, sparse_set: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (base, value, len) = (0, 1, 2);

    // SPARSE: __sparse_set(base, len, value); return base.
    emit_is_sparse(&mut function, base);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalGet(value));
    function.instruction(&Instruction::Call(sparse_set));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);

    // len = base.len; __array_reserve(base, len + 1).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalTee(len));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(reserve_index));

    // base.elems[len] = value.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(value));
    function.instruction(&Instruction::I64Store(mem64(ELEMS_DATA_OFFSET)));

    // base.len = len + 1; return base.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__array_concat(dst: i32, src: i32) -> i32` in-place joiner:
/// append `src`'s `slen` elements after `dst`'s (a spread reads each with
/// `Get`, so a hole is appended as the element inherited through the
/// prototype chain, `undefined` when there is none), write the combined
/// length, and return `dst`. A dense `src` is copied slot by slot into
/// `__array_reserve(dst, dlen + slen)` until its first hole; from there (and
/// for a `SPARSE` `src` from the start) each element is read by index —
/// `__sparse_get` (function `sparse_get`), a hole through `__proto_elem`
/// (function `proto_elem`), so a getter that changes `src` is seen — and
/// appended by `__array_push`. A throwing getter returns `dst` with the
/// exception pending. The two params occupy locals 0/1; extra `i32` locals
/// hold `dlen`, `slen`, the two element cursors and the index `k`, an `i64`
/// local the element.
pub(super) fn emit_array_concat(reserve_index: u32, sparse_get: u32, proto_elem: u32) -> Function {
    let mut function = Function::new(vec![
        (4, ValType::I32),
        (1, ValType::I64),
        (1, ValType::I32),
    ]);
    let (dst, src, dlen, slen, to, from, elem, k) = (0, 1, 2, 3, 4, 5, 6, 7);
    let ret_dst = |function: &mut Function| {
        function.instruction(&Instruction::LocalGet(dst));
        function.instruction(&Instruction::Return);
    };

    // dlen = dst.len; slen = src.len; an empty `src` leaves `dst` as is.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalSet(dlen));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalTee(slen));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    ret_dst(&mut function);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Block(BlockType::Empty));
    // A SPARSE `src` is read by index from k = 0.
    emit_is_sparse(&mut function, src);
    function.instruction(&Instruction::BrIf(0));
    // __array_reserve(dst, dlen + slen).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(reserve_index));
    // to = dst.elems + 8 * dlen; from = src.elems; then the slot copies (the
    // slot MemArg adds ELEMS_DATA_OFFSET).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(to));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalSet(from));
    // dst.len = dlen + slen.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::I64Load(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalTee(elem));
    // The first hole: dst.len = dlen + k, then the by-index loop from k.
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::Br(2));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(to));
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::I64Store(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalGet(to));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(to));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(from));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalTee(k));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::BrIf(0));
    function.instruction(&Instruction::End);
    ret_dst(&mut function);
    function.instruction(&Instruction::End);
    emit_concat_by_index(
        &mut function,
        (dst, src, slen, elem, k),
        reserve_index + 1,
        sparse_get,
        proto_elem,
    );
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// The by-index half of [`emit_array_concat`] over its locals `(dst, src,
/// slen, elem, k)`: for `k < slen`, `__array_push(dst, Get(src, k))` — the
/// element via `__sparse_get`, a hole (or an index past a length a getter
/// shrank) through `__proto_elem`; a throwing getter returns `dst`.
fn emit_concat_by_index(
    function: &mut Function,
    (dst, src, slen, elem, k): (u32, u32, u32, u32, u32),
    array_push: u32,
    sparse_get: u32,
    proto_elem: u32,
) {
    // By index: for (; k < slen; k++) __array_push(dst, Get(src, k)).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // An index past a length a getter shrank is absent from `src` itself.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::Call(sparse_get));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalTee(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::Call(proto_elem));
    function.instruction(&Instruction::LocalSet(elem));
    function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(dst));
    emit_elem_read(function, elem);
    function.instruction(&Instruction::Call(array_push));
    function.instruction(&Instruction::Drop);
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
}

/// Emit the `__json_escape(str_ptr: i32) -> str_base: i32` helper: build a
/// FRESH heap string that is the JSON-quoted + escaped form
/// (`QuoteJSONString`) of the string at `str_ptr` — a leading `"`, each source
/// code unit emitted verbatim except `"`/`\` and the whitespace controls
/// (`\b \t \n \f \r`) which become their two-char backslash escapes, and
/// any other unit `< 0x20` and any lone surrogate which become `\uXXXX`
/// (lowercase hex; a well-formed surrogate pair is copied verbatim), then a
/// trailing `"`.
///
/// The output has the source's width and is bump-allocated at the worst case
/// (`6*len + 2` units, the `\uXXXX` expansion) in a SINGLE pass; the `len`
/// header is written back as the actual escaped length `o`, so any surplus
/// tail is harmless unused heap. A two-byte result is then narrowed when
/// every unit fits a byte (escaped lone surrogates leave only ASCII). Param
/// occupies local 0; `i32` locals hold `len`, `dst`, the read index `i`, the
/// write cursor `o`, the current unit `b`, the two-char escape char `esc`
/// (`0` when the unit has none), and the width `w`.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_json_escape(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(7, ValType::I32)]);
    let (str_ptr, len, dst, i, o, b, esc, w) = (0, 1, 2, 3, 4, 5, 6, 7);
    // Append the unit pushed by `unit` at `o`; o += 1.
    let put = |f: &mut Function, unit: &dyn Fn(&mut Function)| {
        emit_store_unit(f, dst, o, w, unit);
        wasm!(f; LocalGet(o), I32Const(1), I32Add, LocalSet(o));
    };
    let put_const = |f: &mut Function, c: u8| put(f, &|f| wasm!(f; I32Const(i32::from(c))));

    wasm!(function; LocalGet(str_ptr));
    emit_load_str_len(&mut function);
    wasm!(function; LocalSet(len));
    emit_str_two_byte(&mut function, str_ptr);
    wasm!(function; LocalSet(w));
    emit_alloc_str(
        &mut function,
        alloc_index,
        |f| wasm!(f; LocalGet(len), I32Const(6), I32Mul, I32Const(2), I32Add),
        |f| wasm!(f; LocalGet(w)),
        dst,
    );
    // Opening quote; `i`/`o` start at 0.
    put_const(&mut function, b'"');

    // Scan the source units `0..len`.
    wasm!(function;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
    );
    emit_str_unit(&mut function, str_ptr, i, w);
    // esc = 0, then set it for each two-char escape unit (each unit matches at
    // most one, so independent `if`s do not conflict).
    wasm!(function; LocalSet(b), I32Const(0), LocalSet(esc));
    for (unit, escchar) in [
        (b'"', b'"'),
        (b'\\', b'\\'),
        (0x08, b'b'),
        (0x09, b't'),
        (0x0a, b'n'),
        (0x0c, b'f'),
        (0x0d, b'r'),
    ] {
        wasm!(function;
            LocalGet(b), I32Const(i32::from(unit)), I32Eq,
            If(BlockType::Empty), I32Const(i32::from(escchar)), LocalSet(esc), End,
        );
    }
    // if esc != 0 { '\' esc } else if b is a surrogate { pair verbatim, or
    // \uXXXX } else if b < 0x20 { \uXXXX } else { b }.
    wasm!(function; LocalGet(esc), If(BlockType::Empty));
    put_const(&mut function, b'\\');
    put(&mut function, &|f| wasm!(f; LocalGet(esc)));
    wasm!(function;
        Else,
        // esc = 1 when `b` is copied verbatim.
        LocalGet(b), I32Const(0x20), I32GeU, LocalSet(esc),
        LocalGet(b), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq,
        If(BlockType::Empty),
        I32Const(0), LocalSet(esc),
        // A lead followed by a trail is a well-formed pair.
        LocalGet(b), I32Const(0xDC00), I32LtU,
        LocalGet(i), I32Const(1), I32Add, LocalGet(len), I32LtU, I32And,
        If(BlockType::Empty),
        LocalGet(i), I32Const(1), I32Add, LocalSet(esc),
    );
    emit_str_unit(&mut function, str_ptr, esc, w);
    wasm!(function;
        I32Const(0xFC00), I32And, I32Const(0xDC00), I32Eq,
        If(BlockType::Empty),
    );
    // The lead goes out now; the trail is the verbatim unit of this step.
    put(&mut function, &|f| wasm!(f; LocalGet(b)));
    wasm!(function; LocalGet(esc), LocalSet(i));
    emit_str_unit(&mut function, str_ptr, i, w);
    wasm!(function;
        LocalSet(b), I32Const(1), LocalSet(esc),
        Else, I32Const(0), LocalSet(esc),
        End,
        End,
        End,
        LocalGet(esc),
        If(BlockType::Empty),
    );
    put(&mut function, &|f| wasm!(f; LocalGet(b)));
    wasm!(function; Else);
    // `\u` then four lowercase hex digits.
    put_const(&mut function, b'\\');
    put_const(&mut function, b'u');
    for shift in [12, 8, 4, 0] {
        put(&mut function, &|f| {
            wasm!(f;
                LocalGet(b), I32Const(shift), I32ShrU, I32Const(0xf), I32And,
                I32Const(i32::from(b'0')), I32Const(i32::from(b'a') - 10),
                LocalGet(b), I32Const(shift), I32ShrU, I32Const(0xf), I32And, I32Const(10), I32LtU,
                Select, I32Add,
            );
        });
    }
    wasm!(function;
        End, // verbatim
        End, // esc != 0
        // i += 1; continue.
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
    );

    // Closing quote, then write the actual escaped length `o` as the u32 header.
    put_const(&mut function, b'"');
    wasm!(function; LocalGet(dst), LocalGet(o), I32Store(mem32(STR_LEN_OFFSET)));
    emit_str_canonical(&mut function, w, dst, i, b);
    wasm!(function; LocalGet(dst), End);
    function
}

/// Emit the `__json_str(v: i64) -> str_base: i32` primitive serializer of
/// `JSON.stringify` (`__json_rep` serializes arrays and objects). Returns the
/// base offset of a heap string holding `v`'s JSON form:
///
/// - number: `__convert(v, TO_STRING)` when finite (reusing the EXISTING host
///   import — no new import), else the token `"null"` (JSON renders NaN/Infinity
///   as null);
/// - `true`/`false`/`null`: the matching keyword token; `undefined`: `"null"`;
/// - string: `__json_escape` (`escape_index`);
/// - any other heap kind: `"null"`.
///
/// Param `v` occupies local 0; one `i32` local holds the string accumulator.
pub(super) fn emit_json_str(convert_index: u32, escape_index: u32, tokens: JsonTokens) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let v = 0;
    let acc = 1;

    // if is_pointer(v) { string ? escape : "null" } else { primitives }.
    emit_is_pointer(&mut function, v);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::LocalSet(acc));
    emit_load_type_byte(&mut function, v);
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(escape_index));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::Else);
    // --- non-heap: null / true / false / undefined / number ---
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I64Const(NULL_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I64Const(TRUE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.true_ptr));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I64Const(FALSE_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.false_ptr));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);
    // number: finite (x - x == 0) ? __convert(v, TO_STRING) : "null".
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Sub);
    function.instruction(&Instruction::F64Const(Ieee64::from(0.0)));
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_STRING));
    function.instruction(&Instruction::Call(convert_index));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::End); // finite
    function.instruction(&Instruction::End); // undefined
    function.instruction(&Instruction::End); // false
    function.instruction(&Instruction::End); // true
    function.instruction(&Instruction::End); // null
    function.instruction(&Instruction::End); // is_pointer

    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__same_value_zero(a: i64, b: i64) -> i32` Map/Set key-equality
/// helper.
///
/// `SameValueZero`, the key comparison of the `__map_*` linear scans: two heap
/// STRINGS compare by byte content (via `__str_eq` at `str_eq_index`), and
/// everything else matches on `f64` equality OR identical raw bits. Two numbers
/// hence match when `f64`-equal (`+0` and `-0`) or both NaN — the number NaN is
/// always the canonical bits (see [`emit_box_f64`]); a non-number's `f64` view
/// is a NaN that never compares equal, so it falls to identity (different types
/// differ in tag, distinct heap objects/arrays are reference-unequal). The string
/// path is guarded by an `if` so `__str_eq` never runs on a non-pointer. The two
/// `i64` params occupy locals 0/1; no extra locals.
pub(super) fn emit_same_value_zero(str_eq_index: u32) -> Function {
    let mut function = Function::new(vec![]);
    let (a, b) = (0, 1);
    // both strings ? __str_eq : (f64_eq | identity bits).
    emit_is_string(&mut function, a);
    emit_is_string(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__loose_eq(a: i64, b: i64) -> i32` loose-equality (`==`) helper.
///
/// Implements JS Abstract Equality for jsc's value types, falling back to
/// identity for objects. Booleans are first normalized to numbers (`true`→`1`,
/// `false`→`0`). Then: if either operand is `null`/`undefined`, the result is
/// `1` iff BOTH are (nullish values equal each other and nothing else); two
/// numbers compare as `f64`; two strings by byte content (via `__str_eq`); a
/// number and a string coerce the string via the `__convert` `ToNumber` import
/// and compare as `f64`; an object against a (non-nullish) primitive is
/// replaced by its `ToPrimitive` (hint default, `__to_primitive`) and compared
/// again (a user `valueOf`/`toString` throw leaves it pending and yields `0`;
/// a symbol is a primitive here, so `Object(sym) == sym` holds);
/// anything else compares by raw identity bits (two distinct objects differ;
/// the same object is equal; a symbol equals only itself, never a number or
/// string, without coercion). The two `i64` params occupy locals 0/1; they are
/// mutated in place, so no extra locals are needed.
pub(super) fn emit_loose_eq(helpers: Helpers<'_>) -> Function {
    let (str_eq_index, convert_index) = (helpers.str_eq, helpers.convert);
    let mut function = Function::new(vec![]);
    let (a, b) = (0, 1);
    let one_bits = 1.0f64.to_bits().cast_signed();
    let zero_bits = 0.0f64.to_bits().cast_signed();

    // Normalize each boolean operand to a number (true->1.0, false->0.0) so the
    // number/string coercion paths below subsume boolean comparisons.
    for operand in [a, b] {
        function.instruction(&Instruction::LocalGet(operand));
        function.instruction(&Instruction::I64Const(TRUE_BITS));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::I64Const(one_bits));
        function.instruction(&Instruction::LocalSet(operand));
        function.instruction(&Instruction::Else);
        function.instruction(&Instruction::LocalGet(operand));
        function.instruction(&Instruction::I64Const(FALSE_BITS));
        function.instruction(&Instruction::I64Eq);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::I64Const(zero_bits));
        function.instruction(&Instruction::LocalSet(operand));
        function.instruction(&Instruction::End);
        function.instruction(&Instruction::End);
    }

    // If either operand is nullish, equality holds iff BOTH are.
    emit_is_nullish(&mut function, a);
    emit_is_nullish(&mut function, b);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    emit_is_nullish(&mut function, a);
    emit_is_nullish(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::Else);

    // Both numbers -> f64 equality.
    emit_is_number(&mut function, a);
    emit_is_number(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::Else);

    // Both strings -> byte-content equality.
    emit_is_string(&mut function, a);
    emit_is_string(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::Else);

    // number == string: coerce the string to a number, compare as f64.
    emit_is_number(&mut function, a);
    emit_is_string(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_NUMBER));
    function.instruction(&Instruction::Call(convert_index));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::Else);

    // string == number: coerce the string to a number, compare as f64.
    emit_is_string(&mut function, a);
    emit_is_number(&mut function, b);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Const(CONVERT_OP_TO_NUMBER));
    function.instruction(&Instruction::Call(convert_index));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::F64ReinterpretI64);
    function.instruction(&Instruction::F64Eq);
    function.instruction(&Instruction::Else);

    // Fallback: an object against a non-nullish primitive compares its
    // `ToPrimitive` again; otherwise raw identity bits.
    emit_loose_eq_to_primitive(&mut function, helpers);
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I64Eq);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // Close the function body.
    function.instruction(&Instruction::End);
    function
}

/// The object-vs-primitive arm of [`emit_loose_eq`]: an object against a
/// non-nullish primitive is replaced by its `ToPrimitive` (hint default) and
/// `__loose_eq` returns the comparison of the result (`0` when a user
/// `valueOf`/`toString` threw).
fn emit_loose_eq_to_primitive(function: &mut Function, helpers: Helpers<'_>) {
    let (a, b) = (0, 1);
    for (obj, prim) in [(a, b), (b, a)] {
        emit_is_object(function, obj);
        emit_is_object(function, prim);
        function.instruction(&Instruction::I32Eqz);
        function.instruction(&Instruction::I32And);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::LocalGet(obj));
        function.instruction(&Instruction::I32Const(HINT_DEFAULT));
        function.instruction(&Instruction::Call(helpers.to_primitive()));
        function.instruction(&Instruction::LocalSet(obj));
        function.instruction(&Instruction::GlobalGet(EXC_PENDING_GLOBAL));
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::Return);
        function.instruction(&Instruction::End);
        function.instruction(&Instruction::LocalGet(a));
        function.instruction(&Instruction::LocalGet(b));
        function.instruction(&Instruction::Call(helpers.loose_eq));
        function.instruction(&Instruction::Return);
        function.instruction(&Instruction::End);
    }
}

/// Emit the `__str_to_array(s: i64) -> i64` string-spread helper: build a fresh
/// `TYPE_ARRAY` whose elements are the source string's code points as strings
/// (`[..."a😀"]` → `["a", "😀"]`), the walk for-of / spread / `Array.from`
/// share: a lead `D800–DBFF` followed by a trail `DC00–DFFF` yields a
/// two-unit string, any other unit a one-unit string (`__str_index`; a
/// one-byte string has no surrogates). `s` is a heap-string jsval; the
/// returned jsval is a boxed heap array of at most `len` elements.
pub(super) fn emit_str_to_array(
    new_array_index: u32,
    str_index_index: u32,
    alloc_index: u32,
) -> Function {
    let mut function = Function::new(vec![(8, ValType::I32)]);
    let (s, ptr, len, arr, i, store, k, w, dst) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
    // ptr = wrap(s); len = its code-unit length; arr = __new_array(len);
    // store = arr.elems.
    wasm!(function; LocalGet(s), I32WrapI64, LocalTee(ptr));
    emit_load_str_len(&mut function);
    wasm!(function;
        LocalTee(len), Call(new_array_index), LocalTee(arr),
        I32Load(mem32(ARRAY_ELEMS_OFFSET)), LocalSet(store),
    );
    emit_str_two_byte(&mut function, ptr);
    wasm!(function;
        LocalSet(w),
        // Element k = the code point at unit i; `i` and `k` start at 0.
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(w),
        If(BlockType::Result(ValType::I32)),
    );
    emit_surrogate_pair_at(&mut function, ptr, i, len);
    wasm!(function; Else, I32Const(0), End, If(BlockType::Empty));
    emit_str_pair(&mut function, alloc_index, ptr, i, dst);
    wasm!(function;
        LocalGet(i), I32Const(2), I32Add, LocalSet(i),
        Else,
        LocalGet(ptr), LocalGet(i), Call(str_index_index), LocalSet(dst),
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        End,
        // store[k] = box(dst); k += 1.
        LocalGet(store), LocalGet(k), I32Const(3), I32Shl, I32Add,
        LocalGet(dst), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        I64Store(mem64(ELEMS_DATA_OFFSET)),
        LocalGet(k), I32Const(1), I32Add, LocalSet(k),
        Br(0),
        End, End,
        // The array holds `k` code points (a pair is one element).
        LocalGet(arr), LocalGet(k), I32Store(mem32(ARRAY_LEN_OFFSET)),
        // Rebox the array handle base as a heap jsval.
        LocalGet(arr), I64ExtendI32U, I64Const(HEAP_BITS), I64Or,
        End,
    );
    function
}

/// Emit the `__map_find(handle: i32, key: i64) -> i32` Map/Set lookup.
///
/// `handle` is the base of a `TYPE_MAP`/`TYPE_SET` block; its `entries` slot (at
/// [`MAP_ENTRIES_OFFSET`]) holds the backing array jsval. Reads the array's
/// `len` and element store, then linear-scans the EVEN element slots (the keys) `i = 0, 2, 4, ..`,
/// skipping tombstones (a `HOLE` key, design/symbol-strings.md §5.5) and
/// returning the byte ADDRESS of the first slot whose value is `__same_value_zero` (at
/// `svz_index`) to `key`, or `-1` on a miss. The value slot sits 8 bytes past the
/// returned address. Params occupy locals 0/1; four extra `i32` locals hold the
/// backing element store, its length, the loop index, and the current slot
/// address.
pub(super) fn emit_map_find(svz_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (handle, key, store, len, i, slot) = (0, 1, 2, 3, 4, 5);
    let entries_slot = MemArg {
        offset: MAP_ENTRIES_OFFSET,
        align: 3,
        memory_index: 0,
    };
    let elem = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // arr = wrap(load_i64(handle + entries_slot)); len = arr.len; store = arr.elems.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I64Load(entries_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(store));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalSet(len));
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalSet(store));

    // Scan even key slots; `i` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if i >= len { break -> no match }.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // slot = store + ELEMS_DATA_OFFSET + 8*i.
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(slot));
    // if load_i64(slot) != HOLE && __same_value_zero(load_i64(slot), key) { return slot }.
    function.instruction(&Instruction::LocalGet(slot));
    function.instruction(&Instruction::I64Load(elem));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(slot));
    function.instruction(&Instruction::I64Load(elem));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(svz_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(slot));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    // i += 2 (skip the value slot); continue.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(2));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // No key matched.
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_set(handle: i32, key: i64, val: i64)` Map/Set insert-or-update.
/// Looks the key up with `__map_find` (at `map_find_index`): on a hit it
/// overwrites the value slot (8 bytes past the found key slot); on a miss it appends
/// `key` then `val` to the backing array in place via `__array_push` (at
/// `array_push_index`) and bumps the u32 `size` header by one (a key deleted
/// before is not found, so re-adding it appends). `set.add` reuses
/// this with `val = undefined`. Params occupy locals 0/1/2; two extra `i32`
/// locals hold the found address and the backing array base.
pub(super) fn emit_map_set(map_find_index: u32, array_push_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (handle, key, val, addr, arr) = (0, 1, 2, 3, 4);
    let entries_slot = MemArg {
        offset: MAP_ENTRIES_OFFSET,
        align: 3,
        memory_index: 0,
    };
    let size_slot = MemArg {
        offset: MAP_SIZE_OFFSET,
        align: 2,
        memory_index: 0,
    };
    let val_at = MemArg {
        offset: 8,
        align: 0,
        memory_index: 0,
    };

    // addr = __map_find(handle, key).
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(map_find_index));
    function.instruction(&Instruction::LocalSet(addr));
    // if addr != -1 { store val at addr + 8; return }.
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I64Store(val_at));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);

    // Miss: append (key, val) to the backing array.
    // arr = wrap(load_i64(handle + entries_slot)).
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I64Load(entries_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    // __array_push(arr, key); __array_push(arr, val).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(array_push_index));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::Call(array_push_index));
    function.instruction(&Instruction::Drop);
    // size += 1.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32Load(size_slot));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(size_slot));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_get(handle: i32, key: i64) -> i64` Map value reader: the
/// value stored 8 bytes past the key slot `__map_find` (at `map_find_index`)
/// resolves, or the `undefined` bits on a miss. Params occupy locals 0/1; one extra
/// `i32` local holds the found address.
pub(super) fn emit_map_get(map_find_index: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (handle, key, addr) = (0, 1, 2);
    let val_at = MemArg {
        offset: 8,
        align: 0,
        memory_index: 0,
    };
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(map_find_index));
    function.instruction(&Instruction::LocalTee(addr));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I64Load(val_at));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_has(handle: i32, key: i64) -> i32` Map/Set membership probe:
/// `1` iff `__map_find` (at `map_find_index`) hits, else `0`. Params occupy
/// locals 0/1; no extra locals.
pub(super) fn emit_map_has(map_find_index: u32) -> Function {
    let mut function = Function::new(vec![]);
    function.instruction(&Instruction::LocalGet(0));
    function.instruction(&Instruction::LocalGet(1));
    function.instruction(&Instruction::Call(map_find_index));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_delete(handle: i32, key: i64) -> i32` Map/Set remover.
///
/// Looks the key up with `__map_find` (at `map_find_index`); on a miss returns `0`.
/// On a hit it TOMBSTONES the entry in place (design/symbol-strings.md §5.5):
/// the key slot becomes `HOLE` and the value slot `undefined`, so the backing
/// keeps every other entry at its index and a live iterator or `forEach`
/// continues at the next one. The handle's u32 `size` header (the live count)
/// drops by 1; returns `1`. Params occupy locals 0/1; one extra `i32` local
/// holds the found address.
pub(super) fn emit_map_delete(map_find_index: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (handle, key, addr) = (0, 1, 2);
    let size_slot = MemArg {
        offset: MAP_SIZE_OFFSET,
        align: 2,
        memory_index: 0,
    };
    let slot = |offset| MemArg {
        offset,
        align: 0,
        memory_index: 0,
    };

    // addr = __map_find(handle, key); if addr == -1 { return 0 }.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(map_find_index));
    function.instruction(&Instruction::LocalTee(addr));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);

    // key slot = HOLE; value slot = undefined.
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I64Const(HOLE_BITS));
    function.instruction(&Instruction::I64Store(slot(0)));
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Store(slot(8)));
    // size -= 1.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32Load(size_slot));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::I32Store(size_slot));

    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::End);
    function
}
