#[allow(clippy::wildcard_imports)]
use super::*;

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

/// Emit the exported `__new_string(len: i32) -> i32` constructor: a
/// [`TYPE_STRING`] block `[0][0×3][len:u32][len bytes]` whose payload the host
/// fills. Local 0 is `len`; local 1 the base.
pub(super) fn emit_new_string(alloc: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (len, base) = (0, 1);
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::I32Add);
    emit_new_block(&mut function, alloc, base, TYPE_STRING);
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Store(MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::End);
    function
}

/// A 4-byte-aligned `u32` [`MemArg`] at `offset`.
fn mem32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
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
/// `a`/`b` are base offsets of two `[type=0][pad×3][len:u32 LE][utf8]` heap
/// strings. It reads each length header (at `+HEADER_SIZE`), allocates
/// `HEADER_SIZE + 4 + lenA + lenB` bytes via `__alloc` (at `alloc_index`), writes
/// the `TYPE_STRING` type byte and the combined length header, copies both UTF-8
/// payloads (from `+HEADER_SIZE+4`) into place with `memory.copy`, and returns the
/// fresh block's base offset. The two `i32` parameters occupy locals 0/1; three
/// extra `i32` locals hold `lenA`, `lenB`, and `dst`.
pub(super) fn emit_concat(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(3, ValType::I32)]);
    let (a, b, len_a, len_b, dst) = (0, 1, 2, 3, 4);
    // The u32 length header sits at `+HEADER_SIZE`; the UTF-8 payload at
    // `+HEADER_SIZE+4`.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let payload = HEADER_SIZE + 4;

    // lenA = load(a + HEADER_SIZE); lenB = load(b + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len_a));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len_b));

    // dst = __alloc(HEADER_SIZE + 4 + lenA + lenB).
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));

    // Write the TYPE_STRING type byte at dst (offset 0).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    // Store the combined length (lenA + lenB) as the u32 header at dst+HEADER_SIZE.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(header));

    // Copy lenA bytes from a+payload to dst+payload.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Copy lenB bytes from b+payload to dst+payload+lenA.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Return dst.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__str_eq(a: i32, b: i32) -> i32` byte-content string comparator.
///
/// `a`/`b` are byte offsets of two `[len:u32 LE][utf8]` heap strings. Returns `1`
/// when the two payloads are byte-for-byte equal, else `0`: unequal lengths fail
/// immediately, otherwise the bytes `[a+4..]` and `[b+4..]` are compared one at a
/// time. Params occupy locals 0/1; two extra `i32` locals hold the shared length
/// and the loop index.
pub(super) fn emit_str_eq() -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (a, b, len, i) = (0, 1, 2, 3);
    // The u32 length header sits at `+HEADER_SIZE`; the UTF-8 payload at
    // `+HEADER_SIZE+4` (block header).
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let payload = HEADER_SIZE + 4;

    // len = load(a + HEADER_SIZE); if len != load(b + HEADER_SIZE) { return 0 }.
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);

    // Compare bytes; `i` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if i >= len { break -> all bytes matched }.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // if byte(a + payload + i) != byte(b + payload + i) { return 0 }.
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // i += 1; continue.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__str_cmp(a: i32, b: i32) -> i32` lexicographic byte comparator.
///
/// Returns `-1` / `0` / `1`: the bytes are compared over `min(lenA, lenB)`, and
/// the first differing pair decides the sign (unsigned byte order); if the shared
/// prefix is equal, the shorter string is "less" (sign of `lenA - lenB`). Params
/// occupy locals 0/1; extra `i32` locals hold `lenA`, `lenB`, `min`, the loop
/// index, and the two current bytes.
pub(super) fn emit_str_cmp() -> Function {
    let mut function = Function::new(vec![(6, ValType::I32)]);
    let (a, b, len_a, len_b, min, i) = (0, 1, 2, 3, 4, 5);
    let (byte_a, byte_b) = (6, 7);
    // The u32 length header sits at `+HEADER_SIZE`; the UTF-8 payload at
    // `+HEADER_SIZE+4` (block header).
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let payload = HEADER_SIZE + 4;

    // lenA = load(a + HEADER_SIZE); lenB = load(b + HEADER_SIZE); min = min(..).
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len_a));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len_b));
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::LocalSet(min));

    // Compare the shared prefix byte by byte; `i` starts at 0.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if i >= min { break -> prefixes equal }.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(min));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // byteA = byte(a + payload + i); byteB = byte(b + payload + i).
    function.instruction(&Instruction::LocalGet(a));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte));
    function.instruction(&Instruction::LocalSet(byte_a));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte));
    function.instruction(&Instruction::LocalSet(byte_b));
    // if byteA != byteB { return byteA < byteB ? -1 : 1 }.
    function.instruction(&Instruction::LocalGet(byte_a));
    function.instruction(&Instruction::LocalGet(byte_b));
    function.instruction(&Instruction::I32Ne);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(-1));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::LocalGet(byte_a));
    function.instruction(&Instruction::LocalGet(byte_b));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // i += 1; continue.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // Prefixes equal: sign(lenA - lenB) = (lenA > lenB) - (lenA < lenB).
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::I32GtU);
    function.instruction(&Instruction::LocalGet(len_a));
    function.instruction(&Instruction::LocalGet(len_b));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__str_index(ptr: i32, i: i32) -> i32` one-char string builder.
///
/// `ptr` is the byte offset of a `[len:u32 LE][utf8]` heap string; `i` is a byte
/// index assumed in bounds (`0 <= i < len`). It reads the byte at `ptr + 4 + i`,
/// allocates a fresh `4 + 1` byte blob via `__alloc` (at `alloc_index`), writes a
/// u32 length header of `1`, stores the byte at `dst + 4`, and returns `dst`. The
/// two `i32` parameters occupy locals 0/1; one extra `i32` local holds `dst`.
pub(super) fn emit_str_index(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (ptr, i, dst) = (0, 1, 2);
    // The u32 length header sits at `+HEADER_SIZE`; the UTF-8 payload at
    // `+HEADER_SIZE+4` (block header).
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    let payload = HEADER_SIZE + 4;

    // dst = __alloc(HEADER_SIZE + 4 + 1).
    function.instruction(&Instruction::I32Const(payload + 1));
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));

    // Write the TYPE_STRING type byte at dst (offset 0).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Store8(byte));

    // Store the u32 length header (1) at dst + HEADER_SIZE.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Store(header));

    // Store byte(ptr + payload + i) at dst + payload.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte));
    function.instruction(&Instruction::I32Store8(byte));

    // Return dst.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
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
/// `__array_reserve(dst, dlen + slen)`, append `src`'s `slen` elements after
/// `dst`'s (a spread reads each through the iterator, so a hole is appended as
/// `undefined`), write the combined length, and return `dst`. A `SPARSE`
/// `src` is read element by element through `__sparse_get` (function
/// `sparse_get`) and appended by `__array_push`. The two params occupy locals
/// 0/1; extra `i32` locals hold `dlen`, `slen`, and the two element cursors,
/// an `i64` local the element.
pub(super) fn emit_array_concat(reserve_index: u32, sparse_get: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (dst, src, dlen, slen, to, from, elem) = (0, 1, 2, 3, 4, 5, 6);

    // dlen = dst.len; slen = src.len; an empty `src` leaves `dst` as is.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalSet(dlen));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalTee(slen));
    function.instruction(&Instruction::If(BlockType::Empty));
    // A SPARSE `src`: for (from = 0; from < slen; from++)
    // __array_push(dst, read(__sparse_get(src, from))); return dst.
    emit_is_sparse(&mut function, src);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::Call(sparse_get));
    function.instruction(&Instruction::LocalSet(elem));
    emit_elem_read(&mut function, elem);
    function.instruction(&Instruction::Call(reserve_index + 1));
    function.instruction(&Instruction::Drop);
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(from));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // __array_reserve(dst, dlen + slen).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(reserve_index));
    // to = dst.elems + 8 * dlen; from = src.elems; then `slen` element copies
    // (the slot MemArg adds ELEMS_DATA_OFFSET).
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
    // dst.len = dlen + slen (slen counts down below).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(to));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::I64Load(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalSet(elem));
    emit_elem_read(&mut function, elem);
    function.instruction(&Instruction::I64Store(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalGet(to));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(to));
    function.instruction(&Instruction::LocalGet(from));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(from));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalTee(slen));
    function.instruction(&Instruction::BrIf(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// A one-byte store [`MemArg`] (`offset 0`, `align 0`) for the JSON string builder.
fn byte_store() -> MemArg {
    MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }
}

/// Emit `store8(dst + payload + o, c); o += 1` — append the constant byte `c` to a
/// heap-string block being built at `dst`, advancing the write cursor `o`.
fn json_put_const(function: &mut Function, dst: u32, o: u32, payload: i32, c: i32) {
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(c));
    function.instruction(&Instruction::I32Store8(byte_store()));
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(o));
}

/// Emit `store8(dst + payload + o, <local val>); o += 1` — like [`json_put_const`]
/// but the byte comes from local `val`.
fn json_put_local(function: &mut Function, dst: u32, o: u32, payload: i32, val: u32) {
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I32Store8(byte_store()));
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(o));
}

/// Emit the `__json_escape(str_ptr: i32) -> str_base: i32` helper: build a
/// FRESH heap string that is the JSON-quoted + escaped form of the `[len][utf8]`
/// string at `str_ptr` — a leading `"`, each source byte emitted verbatim except
/// `"`/`\` and the whitespace controls (`\b \t \n \f \r`) which become their
/// two-char backslash escapes and any other byte `< 0x20` which becomes `\u00XX`
/// (lowercase hex), then a trailing `"`.
///
/// The output block is bump-allocated at the worst case (`6*len + 2` payload
/// bytes, the `\u00XX` expansion) in a SINGLE pass; the `len` header is written
/// back as the actual escaped length `o`, so any surplus tail bytes are harmless
/// unused heap. Params occupy local 0; six `i32` locals hold `len`, `dst`, the
/// read index `i`, the write cursor `o`, the current byte `b`, and the two-char
/// escape char `esc` (`0` when the byte has none).
#[allow(clippy::too_many_lines)]
pub(super) fn emit_json_escape(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(6, ValType::I32)]);
    let (str_ptr, len, dst, i, o, b) = (0, 1, 2, 3, 4, 5);
    let esc = 6;
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let payload = HEADER_SIZE + 4;

    // len = load(str_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(str_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));
    // dst = __alloc(HEADER_SIZE + 4 + 6*len + 2); worst-case `\u00XX` expansion
    // plus the two surrounding quotes.
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(6));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Const(payload + 2));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));
    // Write the TYPE_STRING type byte at dst (offset 0). `i`/`o` start at 0.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Store8(byte_store()));

    // Opening quote.
    json_put_const(&mut function, dst, o, payload, i32::from(b'"'));

    // Scan the source bytes `0..len`.
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // b = load8u(str_ptr + payload + i).
    function.instruction(&Instruction::LocalGet(str_ptr));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Load8U(byte_store()));
    function.instruction(&Instruction::LocalSet(b));
    // esc = 0, then set it for each two-char escape byte (each byte matches at
    // most one, so independent `if`s do not conflict).
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(esc));
    for (byte, escchar) in [
        (b'"', b'"'),
        (b'\\', b'\\'),
        (0x08, b'b'),
        (0x09, b't'),
        (0x0a, b'n'),
        (0x0c, b'f'),
        (0x0d, b'r'),
    ] {
        function.instruction(&Instruction::LocalGet(b));
        function.instruction(&Instruction::I32Const(i32::from(byte)));
        function.instruction(&Instruction::I32Eq);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::I32Const(i32::from(escchar)));
        function.instruction(&Instruction::LocalSet(esc));
        function.instruction(&Instruction::End);
    }
    // if esc != 0 { write '\' then esc } else if b < 0x20 { \u00XX } else { b }.
    function.instruction(&Instruction::LocalGet(esc));
    function.instruction(&Instruction::If(BlockType::Empty));
    json_put_const(&mut function, dst, o, payload, i32::from(b'\\'));
    json_put_local(&mut function, dst, o, payload, esc);
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(0x20));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // `\u00` prefix, then the two hex nibbles of `b` (< 0x20, so the high nibble
    // is 0 or 1).
    json_put_const(&mut function, dst, o, payload, i32::from(b'\\'));
    json_put_const(&mut function, dst, o, payload, i32::from(b'u'));
    json_put_const(&mut function, dst, o, payload, i32::from(b'0'));
    json_put_const(&mut function, dst, o, payload, i32::from(b'0'));
    // High nibble digit: '0' + (b >> 4).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(i32::from(b'0')));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(4));
    function.instruction(&Instruction::I32ShrU);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store8(byte_store()));
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(o));
    // Low nibble digit: lo = b & 0xf; digit = lo + (lo < 10 ? '0' : 'a'-10).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(payload));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(0xf));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::I32Const(i32::from(b'0')));
    function.instruction(&Instruction::I32Const(i32::from(b'a') - 10));
    function.instruction(&Instruction::LocalGet(b));
    function.instruction(&Instruction::I32Const(0xf));
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::I32Const(10));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::Select);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store8(byte_store()));
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(o));
    function.instruction(&Instruction::Else);
    // Ordinary byte: copy verbatim.
    json_put_local(&mut function, dst, o, payload, b);
    function.instruction(&Instruction::End); // if b < 0x20
    function.instruction(&Instruction::End); // if esc != 0
    // i += 1; continue.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if i < len
    function.instruction(&Instruction::End); // loop

    // Closing quote, then write the actual escaped length `o` as the u32 header.
    json_put_const(&mut function, dst, o, payload, i32::from(b'"'));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(o));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
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
/// again (a user `valueOf`/`toString` throw leaves it pending and yields `0`);
/// anything else compares by raw identity bits (two distinct objects differ;
/// the same object is equal). The two `i64` params occupy locals 0/1; they are
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
/// `TYPE_ARRAY` whose elements are the source string's characters as one-byte
/// heap strings (`[..."abc"]` → `["a", "b", "c"]`). The character walk is
/// byte-indexed (matching for-of / indexing over a string), so multi-byte UTF-8
/// is split per byte — the same limitation the rest of the string surface has.
/// `s` is a heap-string jsval; the returned jsval is a boxed heap array. Fixes
/// the pre-existing garbage-value bug where `[...str]` fell through to
/// `__array_concat` (which reinterpreted the string's UTF-8 bytes as jsvals).
pub(super) fn emit_str_to_array(new_array_index: u32, str_index_index: u32) -> Function {
    let mut function = Function::new(vec![(5, ValType::I32)]);
    let (s, ptr, len, arr, i, store) = (0, 1, 2, 3, 4, 5);

    // ptr = wrap(s); len = load_u32(ptr + HEADER_SIZE) (the string's byte length).
    function.instruction(&Instruction::LocalGet(s));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalTee(ptr));
    function.instruction(&Instruction::I32Load(mem32(HEADER_SIZE as u64)));
    function.instruction(&Instruction::LocalTee(len));
    // arr = __new_array(len); store = arr.elems.
    function.instruction(&Instruction::Call(new_array_index));
    function.instruction(&Instruction::LocalTee(arr));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::LocalSet(store));

    // Fill element k = box(__str_index(ptr, k)) for k in 0..len; `i` starts at 0.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // address = store + 8*i (element slot at MemArg offset ELEMS_DATA_OFFSET).
    function.instruction(&Instruction::LocalGet(store));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(3));
    function.instruction(&Instruction::I32Shl);
    function.instruction(&Instruction::I32Add);
    // value = HEAP_BITS | extend_u(__str_index(ptr, i)).
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::Call(str_index_index));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(mem64(ELEMS_DATA_OFFSET)));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    // Rebox the array handle base as a heap jsval.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_find(handle: i32, key: i64) -> i32` Map/Set lookup.
///
/// `handle` is the base of a `TYPE_MAP`/`TYPE_SET` block; its `entries` slot (at
/// [`MAP_ENTRIES_OFFSET`]) holds the backing array jsval. Reads the array's
/// `len` and element store, then linear-scans the EVEN element slots (the keys) `i = 0, 2, 4, ..`,
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
    // if __same_value_zero(load_i64(slot), key) { return slot }.
    function.instruction(&Instruction::LocalGet(slot));
    function.instruction(&Instruction::I64Load(elem));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(svz_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(slot));
    function.instruction(&Instruction::Return);
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
/// `array_push_index`) and bumps the u32 `size` header by one. `set.add` reuses
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
/// On a hit it COMPACTS the two-slot `(key, value)` entry out of the backing array —
/// `memory.copy` shifts every trailing pair down one entry (16 bytes) — decrements
/// the backing's u32 `len` by 2 and the handle's u32 `size` header by 1, and
/// returns `1`. Params occupy locals 0/1; four extra `i32` locals hold the found
/// address, the backing base, its length, and the shift byte count.
pub(super) fn emit_map_delete(map_find_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (handle, key, addr, arr, len, count) = (0, 1, 2, 3, 4, 5);
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

    // arr = wrap(load_i64(handle + entries_slot)); len = arr.len.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I64Load(entries_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_LEN_OFFSET)));
    function.instruction(&Instruction::LocalSet(len));

    // count = (arr.elems + ELEMS_DATA_OFFSET + 8*len) - (addr + 16): trailing
    // bytes to move.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(mem32(ARRAY_ELEMS_OFFSET)));
    function.instruction(&Instruction::I32Const(abi::ELEMS_DATA_OFFSET.cast_signed()));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalSet(count));

    // memory.copy(dst = addr, src = addr + 16, count): shift trailing pairs down.
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::LocalGet(addr));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(count));
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // len -= 2 (drop the entry's two slots).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(2));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::I32Store(mem32(ARRAY_LEN_OFFSET)));
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
