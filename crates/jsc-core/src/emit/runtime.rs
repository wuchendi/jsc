#[allow(clippy::wildcard_imports)]
use super::*;

/// Emit the `__alloc(size: i32) -> i32` bump allocator: return the current
/// `__heap_ptr`, then advance it by `size`, growing linear memory on demand.
///
/// The end offset is computed in `i64` (both operands zero-extended), so it can
/// never wrap; the fast path is one compare against `memory.size * 64 KiB`. On
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
    // old = __heap_ptr; end = u64(old) + u64(size).
    function.instruction(&Instruction::GlobalGet(HEAP_PTR_GLOBAL));
    function.instruction(&Instruction::LocalTee(old));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::LocalGet(size));
    function.instruction(&Instruction::I64ExtendI32U);
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
    // __heap_ptr = end; return old.
    function.instruction(&Instruction::LocalGet(end));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::GlobalSet(HEAP_PTR_GLOBAL));
    function.instruction(&Instruction::LocalGet(old));
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
///
/// When the program declares no user functions (`fn_type == 0`, so there is no
/// funcref table), no callback can exist, so the body degenerates to returning
/// `undefined` — keeping the exported function valid and total. The two `i64`
/// params occupy locals 0/1; one extra `i32` local holds the callee block base.
pub(super) fn emit_invoke1(helpers: Helpers<'_>) -> Function {
    let mut function = Function::new(vec![(1, ValType::I32)]);
    let (callee, arg, base) = (0, 1, 2);
    // No functions in the program: no table, so return `undefined` unconditionally.
    if helpers.fn_type == 0 {
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::End);
        return function;
    }
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
    // Trailing env argument: the callee's own environment (at +HEADER_SIZE+4).
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I64Load(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 3,
        memory_index: 0,
    }));
    // Argument count (env-last ABI): exactly one, as a number jsval.
    function.instruction(&Instruction::I64Const(1.0_f64.to_bits().cast_signed()));
    // Funcref-table index (at +HEADER_SIZE), on top for call_indirect.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(MemArg {
        offset: HEADER_SIZE as u64,
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

/// Emit the `__obj_get(obj_ptr: i32, key_ptr: i32) -> i64` property reader.
///
/// `obj_ptr` is the byte offset of a `[n:u32][key_ptr:i64, val:i64]..` heap
/// object; `key_ptr` is the byte offset of the `[len:u32][utf8]` key string to
/// look up. It reads the `n` header, then linear-scans entries `0..n`: entry `k`
/// lives at `obj_ptr + 16*k`, its key jsval at offset `+4` and its value jsval at
/// offset `+12`. The stored key's low 32 bits are the candidate key string
/// offset, compared by byte content against `key_ptr` via `__str_eq` (at
/// `str_eq_index`). The first match's value jsval is returned. On an own miss, the
/// PROTOTYPE FALLBACK kicks in when `proto_key` is `Some(off)` — the
/// interned byte offset of the reserved `@@prototype` key: a second scan reads the
/// object's own `@@prototype` slot, and if it holds a heap pointer (a class's
/// shared prototype object) the lookup recurses into it via `self_index` (this
/// function's own wasm index), so `instance.method` resolves on the prototype. Own
/// properties always win — the fallback runs only after the own scan fails.
/// Recursion terminates because a prototype object carries no `@@prototype` of its
/// own (the miss there yields `undefined`, stopping the chain). If no key resolves
/// — own or through the chain — the `undefined` bits are returned. When
/// `proto_key` is `None` (a program with no class, so `@@prototype` is never
/// interned) the fallback is omitted entirely, leaving the
/// plain own-key lookup. Params occupy locals 0/1; two extra `i32` locals hold
/// `n` and the loop index `k`, plus (with the fallback) one `i64` local for the
/// resolved prototype value. Modeled on [`emit_str_eq`].
// A single flat instruction stream (the own scan plus the optional prototype-
// fallback scan), so the length reflects the two loops rather than real branching.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_obj_get(str_eq_index: u32, self_index: u32, proto_key: Option<i32>) -> Function {
    let locals = if proto_key.is_some() {
        vec![(2, ValType::I32), (1, ValType::I64)]
    } else {
        vec![(2, ValType::I32)]
    };
    let mut function = Function::new(locals);
    let (obj_ptr, key_ptr, n, k) = (0, 1, 2, 3);
    // The resolved prototype value (i64), used only when the fallback is emitted.
    let proto = 4;
    // The block header shifts every slot by HEADER_SIZE: `n` at `+HEADER_SIZE`,
    // entry `k` key at `+HEADER_SIZE+4+16*k`, its value at `+8` from the key.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let key_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    let val_slot = MemArg {
        offset: (HEADER_SIZE + 12) as u64,
        align: 0,
        memory_index: 0,
    };

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));

    // Scan entries; `k` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if k >= n { break -> no match }.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // candidate_key = i32.wrap(load_i64(obj_ptr + 16*k + 4)).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I32WrapI64);
    // if __str_eq(candidate_key, key_ptr) { return load_i64(obj_ptr + 16*k + 12) }.
    function.instruction(&Instruction::LocalGet(key_ptr));
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(val_slot));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // Own miss. If the fallback is enabled, resolve the key through the object's
    // `@@prototype` chain before giving up.
    if let Some(proto_key) = proto_key {
        // proto = undefined; then a second own-only scan sets it to the object's
        // `@@prototype` value (leaving it `undefined` if the object has none).
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::LocalSet(proto));
        function.instruction(&Instruction::I32Const(0));
        function.instruction(&Instruction::LocalSet(k));
        function.instruction(&Instruction::Block(BlockType::Empty));
        function.instruction(&Instruction::Loop(BlockType::Empty));
        // if k >= n { break -> no prototype }.
        function.instruction(&Instruction::LocalGet(k));
        function.instruction(&Instruction::LocalGet(n));
        function.instruction(&Instruction::I32GeU);
        function.instruction(&Instruction::BrIf(1));
        // candidate_key = i32.wrap(load_i64(obj_ptr + 16*k + 4)).
        function.instruction(&Instruction::LocalGet(obj_ptr));
        function.instruction(&Instruction::LocalGet(k));
        function.instruction(&Instruction::I32Const(16));
        function.instruction(&Instruction::I32Mul);
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::I64Load(key_slot));
        function.instruction(&Instruction::I32WrapI64);
        // if __str_eq(candidate_key, proto_key) { proto = value; leave the scan }.
        function.instruction(&Instruction::I32Const(proto_key));
        function.instruction(&Instruction::Call(str_eq_index));
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::LocalGet(obj_ptr));
        function.instruction(&Instruction::LocalGet(k));
        function.instruction(&Instruction::I32Const(16));
        function.instruction(&Instruction::I32Mul);
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::I64Load(val_slot));
        function.instruction(&Instruction::LocalSet(proto));
        function.instruction(&Instruction::Br(2)); // proto found; exit the scan block
        function.instruction(&Instruction::End);
        // k += 1; continue.
        function.instruction(&Instruction::LocalGet(k));
        function.instruction(&Instruction::I32Const(1));
        function.instruction(&Instruction::I32Add);
        function.instruction(&Instruction::LocalSet(k));
        function.instruction(&Instruction::Br(0));
        function.instruction(&Instruction::End); // loop
        function.instruction(&Instruction::End); // block
        // A prototype was found (its value is not `undefined`): recurse into it for
        // the SAME key. A prototype object has no `@@prototype`, so its own miss
        // returns `undefined` and the chain stops.
        function.instruction(&Instruction::LocalGet(proto));
        function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
        function.instruction(&Instruction::I64Ne);
        function.instruction(&Instruction::If(BlockType::Empty));
        function.instruction(&Instruction::LocalGet(proto));
        function.instruction(&Instruction::I32WrapI64);
        function.instruction(&Instruction::LocalGet(key_ptr));
        function.instruction(&Instruction::Call(self_index));
        function.instruction(&Instruction::Return);
        function.instruction(&Instruction::End);
    }

    // No key matched (own or through the prototype chain): yield `undefined`.
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_set(obj_ptr: i32, key_ptr: i32, val: i64)` property writer.
///
/// `obj_ptr` is the byte offset of a `[n:u32][key_ptr:i64, val:i64]..` heap
/// object; `key_ptr` is the byte offset of the `[len:u32][utf8]` key to store
/// under. It reads the `n` header, then linear-scans entries `0..n` exactly like
/// [`emit_obj_get`]: entry `k` lives at `obj_ptr + 16*k`, its key jsval at offset
/// `+4`, its value jsval at offset `+12`. On the first key whose byte content
/// equals `key_ptr` (via `__str_eq` at `str_eq_index`), `val` is stored into that
/// entry's value slot and the scan stops. If no key matches, nothing is written
/// (existing keys only — growth is deferred). Params occupy locals 0/1/2; two
/// extra `i32` locals hold `n` and the loop index `k`.
pub(super) fn emit_obj_set(str_eq_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (obj_ptr, key_ptr, val, n, k) = (0, 1, 2, 3, 4);
    // The block header shifts every slot by HEADER_SIZE: `n` at `+HEADER_SIZE`,
    // entry `k` key at `+HEADER_SIZE+4+16*k`, its value at `+8` from the key.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let key_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    let val_slot = MemArg {
        offset: (HEADER_SIZE + 12) as u64,
        align: 0,
        memory_index: 0,
    };

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));

    // Scan entries; `k` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if k >= n { break -> no match, no-op }.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // candidate_key = i32.wrap(load_i64(obj_ptr + 16*k + 4)).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I32WrapI64);
    // if __str_eq(candidate_key, key_ptr) { store val at obj_ptr + 16*k + 12; stop }.
    function.instruction(&Instruction::LocalGet(key_ptr));
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I64Store(val_slot));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_rest(obj_ptr: i32, excl_ptr: i32) -> i32` object-rest builder.
///
/// `obj_ptr` is the base offset of a `[n:u32][(key_ptr:i64, val:i64) × n]` heap
/// object; `excl_ptr` is the base offset of a `[len:u32][key_ptr:i64 × len]` heap
/// array of the destructured key strings to EXCLUDE. It bump-allocates a fresh
/// object of worst-case capacity `n`, then linear-scans the source entries: an
/// entry whose key is byte-equal (via `__str_eq` at `str_eq_index`) to any
/// excluded key is skipped, every other `(key, val)` pair is copied into the new
/// block, and the surviving count `m` is written as the new `n` header. The
/// abandoned surplus slots (when keys were excluded) are harmless under the bump
/// allocator. Returns the new block base. The two `i32` params occupy locals
/// 0/1; eight extra `i32` locals hold `n`, `excl`, `dst`, `m`, `k`, `key`,
/// `excluded`, and `j`. Modeled on [`emit_obj_get`].
// A single flat instruction stream (two nested scan loops), so the length is the
// instruction count rather than any real branching complexity.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_obj_rest(alloc_index: u32, str_eq_index: u32) -> Function {
    let mut function = Function::new(vec![(8, ValType::I32)]);
    let (obj_ptr, excl_ptr, n, excl, dst) = (0, 1, 2, 3, 4);
    let (m, k, key, excluded, j) = (5, 6, 7, 8, 9);
    // Block header: `n`/`len` at `+HEADER_SIZE`; object entry `k` key at
    // `+HEADER_SIZE+4+16*k`, its value at `+8` from the key; array element `j` at
    // `+HEADER_SIZE+4+8*j`.
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let key_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    let val_slot = MemArg {
        offset: (HEADER_SIZE + 12) as u64,
        align: 0,
        memory_index: 0,
    };
    let elem_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };

    // n = load(obj_ptr + HEADER_SIZE); excl = load(excl_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    function.instruction(&Instruction::LocalGet(excl_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(excl));

    // dst = __alloc(HEADER_SIZE + 4 + 16*n).
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));

    // Write the TYPE_OBJECT type byte at dst (offset 0). `m`/`k` start at 0.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    // Scan source entries `0..n`.
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // key = i32.wrap(load_i64(obj_ptr + 16*k + key_slot)).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(key));
    // excluded = 0; j = 0.
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(excluded));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(j));
    // Scan excluded keys `0..excl`, setting `excluded` on a byte-content match.
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(j));
    function.instruction(&Instruction::LocalGet(excl));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // if __str_eq(key, i32.wrap(load_i64(excl_ptr + 8*j + elem_slot))) { excluded = 1 }.
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::LocalGet(excl_ptr));
    function.instruction(&Instruction::LocalGet(j));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::LocalSet(excluded));
    function.instruction(&Instruction::End);
    // j += 1; continue the inner loop.
    function.instruction(&Instruction::LocalGet(j));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(j));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if j < excl
    function.instruction(&Instruction::End); // inner loop
    // if !excluded: copy the (key, val) pair into dst entry m; m += 1.
    function.instruction(&Instruction::LocalGet(excluded));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    // store key: dst + 16*m + key_slot = load_i64(obj_ptr + 16*k + key_slot).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I64Store(key_slot));
    // store val: dst + 16*m + val_slot = load_i64(obj_ptr + 16*k + val_slot).
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(val_slot));
    function.instruction(&Instruction::I64Store(val_slot));
    // m += 1.
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(m));
    function.instruction(&Instruction::End); // if !excluded
    // k += 1; continue the outer loop.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // outer loop

    // Store the surviving count `m` as the new u32 `n` header, then return dst.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__array_push(base: i32, value: i64) -> i32` array grower.
///
/// The bump allocator has no free list, so growing a fixed-size array block means
/// allocating a fresh, larger block and abandoning the old one. It reads the u32
/// `len` header at `base + HEADER_SIZE`, bump-allocates `HEADER_SIZE + 4 + 8*(len +
/// 1)` bytes via `__alloc` (at `alloc_index`), `memory.copy`s the entire old block
/// prefix (`TYPE_ARRAY` byte + `len` header + `len` element slots) into it, appends
/// `value` at the new last slot (`dst + HEADER_SIZE + 4 + 8*len`), bumps the length
/// header to `len + 1`, and returns the new block base. The two params occupy
/// locals 0/1; two extra `i32` locals hold `len` and `dst`.
pub(super) fn emit_array_push(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (base, value, len, dst) = (0, 1, 2, 3);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let payload = (HEADER_SIZE + 4) as u64;

    // len = load u32 header at base + HEADER_SIZE.
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));

    // dst = __alloc(HEADER_SIZE + 4 + 8*(len + 1)); leave dst in its local.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));

    // memory.copy(dst, base, HEADER_SIZE + 4 + 8*len): the type byte, the u32 len
    // header, and every existing element slot.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(base));
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Append value at dst + HEADER_SIZE + 4 + 8*len.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(value));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: payload,
        align: 0,
        memory_index: 0,
    }));

    // Bump the u32 len header to len + 1.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(header));

    // Return the new block base.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__array_concat(dst: i32, src: i32) -> i32` array joiner.
///
/// Like [`emit_array_push`], the bump allocator has no free list, so appending
/// one array's elements to another means allocating a fresh block. It reads the
/// u32 `len` headers of both source blocks, bump-allocates `HEADER_SIZE + 4 +
/// 8*(dlen + slen)` bytes via `__alloc` (at `alloc_index`), `memory.copy`s the
/// `dst` prefix (`TYPE_ARRAY` byte + `dlen` header + `dlen` element slots) into
/// it, `memory.copy`s `src`'s `slen` element slots right after them, writes the
/// combined length header, and returns the new block base. The two params occupy
/// locals 0/1; three extra `i32` locals hold `dlen`, `slen`, and `nb`.
pub(super) fn emit_array_concat(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(3, ValType::I32)]);
    let (dst, src, dlen, slen, nb) = (0, 1, 2, 3, 4);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let prefix = HEADER_SIZE + 4;

    // dlen = load u32 header at dst + HEADER_SIZE; slen = same for src.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(dlen));
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(slen));

    // nb = __alloc(HEADER_SIZE + 4 + 8*(dlen + slen)); leave nb in its local.
    function.instruction(&Instruction::I32Const(prefix));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(nb));

    // memory.copy(nb, dst, HEADER_SIZE + 4 + 8*dlen): the type byte, the u32 len
    // header, and every `dst` element slot.
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(prefix));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // memory.copy(nb + HEADER_SIZE + 4 + 8*dlen, src + HEADER_SIZE + 4, 8*slen):
    // append `src`'s element slots right after `dst`'s.
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::I32Const(prefix));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(src));
    function.instruction(&Instruction::I32Const(prefix));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // Write the combined u32 len header (dlen + slen) at nb + HEADER_SIZE.
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::LocalGet(dlen));
    function.instruction(&Instruction::LocalGet(slen));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(header));

    // Return the new block base.
    function.instruction(&Instruction::LocalGet(nb));
    function.instruction(&Instruction::End);
    function
}

/// The four common object/array block `MemArg`s: the u32 `n`/`len`
/// header at `+HEADER_SIZE`; an object entry's key at `+HEADER_SIZE+4` and value
/// at `+HEADER_SIZE+12` (16 bytes apart); an array element at `+HEADER_SIZE+4`.
fn obj_block_memargs() -> (MemArg, MemArg, MemArg, MemArg) {
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let key_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    let val_slot = MemArg {
        offset: (HEADER_SIZE + 12) as u64,
        align: 0,
        memory_index: 0,
    };
    let elem_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    (header, key_slot, val_slot, elem_slot)
}

/// Emit `entry_addr = obj_ptr + 16*k` — the base of object entry `k` (its key at
/// `+key_slot`, value at `+val_slot`) — leaving the address on the stack.
fn emit_obj_entry_addr(function: &mut Function, obj_ptr: u32, k: u32) {
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
}

/// Emit `dst + 8*m` — the base of array slot `m` (its element at `+elem_slot`) —
/// leaving the address on the stack.
fn emit_array_slot_addr(function: &mut Function, dst: u32, m: u32) {
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
}

/// Emit the reserved-key guard `i32.wrap(key)`'s first payload byte `!= '@'`,
/// leaving a `1`/`0` i32 on the stack. The reserved hidden object keys
/// (`@@prototype`/`@@constructor`) are the only keys beginning with `@`, and no
/// JS identifier may start with it, so a `'@'` first byte marks a key that
/// `Object.keys`/`values`/`entries` must skip. `key` (an i64 heap-string jsval)
/// is wrapped to its pointer and its first byte read at `+HEADER_SIZE+4`.
fn emit_key_not_reserved(function: &mut Function, key: u32) {
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::I32Load8U(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::I32Const(i32::from(b'@')));
    function.instruction(&Instruction::I32Ne);
}

/// Emit the `__obj_keys(obj_ptr: i32) -> array_base: i32` enumerator.
///
/// Bump-allocates a fresh `TYPE_ARRAY` block of capacity `n` (the object's entry
/// count), linear-scans the source entries `0..n`, and appends each non-reserved
/// key heap-string jsval into the array in insertion order, tracking the surviving
/// count `m` written back as the array's u32 `len` header. Reserved hidden keys
/// (`@@`-prefixed) are skipped via [`emit_key_not_reserved`]. Modeled on
/// [`emit_obj_rest`]'s filtered single-pass copy.
pub(super) fn emit_obj_keys(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32), (1, ValType::I64)]);
    let (obj_ptr, n, dst, k, m) = (0, 1, 2, 3, 4);
    let key = 5;
    let (header, key_slot, _val_slot, elem_slot) = obj_block_memargs();

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    // dst = __alloc(HEADER_SIZE + 4 + 8*n); m = 0.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));
    // Write the TYPE_ARRAY type byte at dst (offset 0). `m`/`k` start at 0.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    // Scan source entries `0..n`.
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // key = load_i64(obj_ptr + 16*k + key_slot).
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::LocalSet(key));
    // if key is not reserved: store it at dst slot m; m += 1.
    emit_key_not_reserved(&mut function, key);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_array_slot_addr(&mut function, dst, m);
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I64Store(elem_slot));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(m));
    function.instruction(&Instruction::End); // if !reserved
    // k += 1; continue the loop.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // loop

    // Store the surviving count `m` as the u32 `len` header, then return dst.
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_values(obj_ptr: i32) -> array_base: i32` enumerator.
///
/// Like [`emit_obj_keys`] but each surviving entry contributes its VALUE jsval
/// (loaded from the `+val_slot`) rather than its key; the key is still read to run
/// the reserved-key guard.
pub(super) fn emit_obj_values(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32), (2, ValType::I64)]);
    let (obj_ptr, n, dst, k, m) = (0, 1, 2, 3, 4);
    let (key, val) = (5, 6);
    let (header, key_slot, val_slot, elem_slot) = obj_block_memargs();

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    // dst = __alloc(HEADER_SIZE + 4 + 8*n); m = 0.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // key = load key_slot; val = load val_slot (both at obj_ptr + 16*k).
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::LocalSet(key));
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(val_slot));
    function.instruction(&Instruction::LocalSet(val));
    // if key is not reserved: store val at dst slot m; m += 1.
    emit_key_not_reserved(&mut function, key);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_array_slot_addr(&mut function, dst, m);
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I64Store(elem_slot));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(m));
    function.instruction(&Instruction::End); // if !reserved
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // loop

    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_entries(obj_ptr: i32) -> array_base: i32` enumerator.
///
/// Like [`emit_obj_keys`] but each surviving entry contributes a FRESH two-element
/// `[key, value]` array (bump-allocated inside the loop) whose reboxed heap jsval
/// is stored into the outer array. The outer array's `8*n`-byte element region is
/// reserved up front, so the inner arrays — allocated afterward at higher
/// addresses — never overlap it.
pub(super) fn emit_obj_entries(alloc_index: u32) -> Function {
    let mut function = Function::new(vec![(5, ValType::I32), (2, ValType::I64)]);
    let (obj_ptr, n, dst, k, m) = (0, 1, 2, 3, 4);
    let inner = 5;
    let (key, val) = (6, 7);
    let (header, key_slot, val_slot, elem_slot) = obj_block_memargs();
    let type_byte = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    // dst = __alloc(HEADER_SIZE + 4 + 8*n); m = 0.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(dst));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));

    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::LocalSet(key));
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(val_slot));
    function.instruction(&Instruction::LocalSet(val));
    // if key is not reserved: build [key, value] and store it at dst slot m.
    emit_key_not_reserved(&mut function, key);
    function.instruction(&Instruction::If(BlockType::Empty));
    // inner = __alloc(HEADER_SIZE + 4 + 16); TYPE_ARRAY byte; len = 2.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4 + 16));
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(inner));
    function.instruction(&Instruction::LocalGet(inner));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(type_byte));
    function.instruction(&Instruction::LocalGet(inner));
    function.instruction(&Instruction::I32Const(2));
    function.instruction(&Instruction::I32Store(header));
    // inner[0] = key; inner[1] = val.
    function.instruction(&Instruction::LocalGet(inner));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I64Store(elem_slot));
    function.instruction(&Instruction::LocalGet(inner));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I64Store(MemArg {
        offset: (HEADER_SIZE + 4 + 8) as u64,
        align: 0,
        memory_index: 0,
    }));
    // dst slot m = rebox(inner) = (i64.extend_i32_u inner) | HEAP_BITS.
    emit_array_slot_addr(&mut function, dst, m);
    function.instruction(&Instruction::LocalGet(inner));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(elem_slot));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(m));
    function.instruction(&Instruction::End); // if !reserved
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // loop

    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::LocalGet(m));
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::LocalGet(dst));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_has(obj_ptr: i32, key_ptr: i32) -> i32` own-key probe.
///
/// `obj_ptr` is the base offset of a `[n:u32][(key_ptr:i64, val:i64) × n]` heap
/// object; `key_ptr` is the base offset of the `[len:u32][utf8]` key to look for.
/// It reads the `n` header and linear-scans entries `0..n` exactly like
/// [`emit_obj_get`]'s own scan, returning `1` on the first key whose byte content
/// equals `key_ptr` (via `__str_eq` at `str_eq_index`) and `0` if none match.
/// OWN KEYS ONLY — the `@@prototype` chain is not walked (`in` is an own-key
/// test). Params occupy locals 0/1; two extra `i32` locals hold `n` and the loop
/// index `k`.
pub(super) fn emit_obj_has(str_eq_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (obj_ptr, key_ptr, n, k) = (0, 1, 2, 3);
    let (header, key_slot, _val_slot, _elem_slot) = obj_block_memargs();

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));

    // Scan entries; `k` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if k >= n { break -> no match }.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // candidate_key = i32.wrap(load_i64(obj_ptr + 16*k + key_slot)).
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I32WrapI64);
    // if __str_eq(candidate_key, key_ptr) { return 1 }.
    function.instruction(&Instruction::LocalGet(key_ptr));
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

    // No key matched: absent.
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__obj_delete(obj_ptr: i32, key_ptr: i32)` property remover.
///
/// `obj_ptr` is the base offset of a `[n:u32][(key_ptr:i64, val:i64) × n]` heap
/// object; `key_ptr` is the base offset of the `[len:u32][utf8]` key to remove.
/// It reads the `n` header and linear-scans entries `0..n` like [`emit_obj_has`];
/// on the first key whose byte content equals `key_ptr` (via `__str_eq` at
/// `str_eq_index`) it COMPACTS the entry out — `memory.copy` shifts the trailing
/// entries `k+1..n` down by one 16-byte slot and the `n` header is decremented —
/// then stops. A miss is a no-op. Because the entry is physically removed, a later
/// `__obj_get` misses (`undefined`), `__obj_has` returns `0`, and every
/// index-based enumerator (`for-in`, `__obj_keys`/`values`/`entries`) sees the
/// smaller count and never visits it. Params occupy locals 0/1; two extra `i32`
/// locals hold `n` and the loop index `k`.
pub(super) fn emit_obj_delete(str_eq_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (obj_ptr, key_ptr, n, k) = (0, 1, 2, 3);
    let (header, key_slot, _val_slot, _elem_slot) = obj_block_memargs();
    // The first entry slot starts at obj_ptr + HEADER_SIZE + 4; each entry is 16
    // bytes (key_ptr:i64, val:i64).
    let entries_base = HEADER_SIZE + 4;

    // n = load(obj_ptr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));

    // Scan entries; `k` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if k >= n { break -> no match, no-op }.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // candidate_key = i32.wrap(load_i64(obj_ptr + 16*k + key_slot)).
    emit_obj_entry_addr(&mut function, obj_ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::I32WrapI64);
    // if __str_eq(candidate_key, key_ptr) { compact entry k out; return }.
    function.instruction(&Instruction::LocalGet(key_ptr));
    function.instruction(&Instruction::Call(str_eq_index));
    function.instruction(&Instruction::If(BlockType::Empty));
    // memory.copy(dst = obj_ptr + entries_base + 16*k,
    //             src = obj_ptr + entries_base + 16*(k+1),
    //             len = 16*(n - 1 - k)): shift the trailing entries down one slot.
    // dst = obj_ptr + 16*k + entries_base.
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(entries_base));
    function.instruction(&Instruction::I32Add);
    // src = obj_ptr + 16*(k+1) + entries_base (the next entry).
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(entries_base));
    function.instruction(&Instruction::I32Add);
    // len = 16*(n - 1 - k) (0 when deleting the last entry — a valid empty copy).
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::I32Const(16));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    // Decrement the n header: store n - 1.
    function.instruction(&Instruction::LocalGet(obj_ptr));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::I32Store(header));
    function.instruction(&Instruction::Return);
    function.instruction(&Instruction::End);
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::End); // block

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

/// Emit the recursive `__json_str(v: i64) -> str_base: i32` serializer backing
/// `JSON.stringify`. Returns the base offset of a heap string holding `v`'s
/// JSON form, composing pieces with `__concat` (`concat_index`):
///
/// - number: `__convert(v, TO_STRING)` when finite (reusing the EXISTING host
///   import — no new import), else the token `"null"` (JSON renders NaN/Infinity
///   as null);
/// - `true`/`false`/`null`: the matching keyword token; `undefined`: `"null"`
///   (its array-element rendering — object values / the top level pre-skip it);
/// - string: `__json_escape` (`escape_index`);
/// - array: `"[" + elems joined "," + "]"`, each element serialized by a recursive
///   `__json_str` (`self_index`) call, with `undefined`/function elements rendered
///   as `"null"`;
/// - object: `"{" + key:value pairs joined "," + "}"`, skipping reserved
///   `@@`-prefixed keys and omitting entries whose value is `undefined`/a function;
///   keys are escaped like strings and values serialized recursively;
/// - any other heap kind (a function value): `"null"`.
///
/// Param `v` occupies local 0; six `i32` locals hold the wrapped pointer, type
/// byte, entry count, loop index, the first-element flag, and the string
/// accumulator; three `i64` locals hold the current array element / object key /
/// object value.
#[allow(clippy::too_many_lines)]
pub(super) fn emit_json_str(
    self_index: u32,
    concat_index: u32,
    convert_index: u32,
    escape_index: u32,
    tokens: JsonTokens,
) -> Function {
    let mut function = Function::new(vec![(6, ValType::I32), (3, ValType::I64)]);
    let v = 0;
    let (ptr, tb, n, k, first, acc) = (1, 2, 3, 4, 5, 6);
    let (elem, key, val) = (7, 8, 9);
    let (header, key_slot, val_slot, elem_slot) = obj_block_memargs();

    // Emit `acc = concat(acc, <piece already on stack>)` — the piece expression
    // must already be pushed by the caller right before invoking this.
    let concat_into_acc = |function: &mut Function| {
        function.instruction(&Instruction::Call(concat_index));
        function.instruction(&Instruction::LocalSet(acc));
    };

    // if is_pointer(v) { heap kinds } else { number / bool / null / undefined }.
    emit_is_pointer(&mut function, v);
    function.instruction(&Instruction::If(BlockType::Empty));
    // ptr = wrap(v); tb = type_byte(v).
    function.instruction(&Instruction::LocalGet(v));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(ptr));
    emit_load_type_byte(&mut function, v);
    function.instruction(&Instruction::LocalSet(tb));

    // --- string: acc = __json_escape(ptr) ---
    function.instruction(&Instruction::LocalGet(tb));
    function.instruction(&Instruction::I32Const(TYPE_STRING));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::Call(escape_index));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::Else);

    // --- array: "[" + elems joined "," + "]" ---
    function.instruction(&Instruction::LocalGet(tb));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.lbrack));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::LocalSet(first));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // elem = load_i64(ptr + 8*k + elem_slot).
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I64Load(elem_slot));
    function.instruction(&Instruction::LocalSet(elem));
    // Separator: if !first { acc = concat(acc, ",") }.
    function.instruction(&Instruction::LocalGet(first));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::I32Const(tokens.comma));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(first));
    // acc = concat(acc, (undefined||function) ? "null" : __json_str(elem)).
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    emit_is_function(&mut function, elem);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::Else);
    function.instruction(&Instruction::LocalGet(elem));
    function.instruction(&Instruction::Call(self_index));
    function.instruction(&Instruction::End);
    concat_into_acc(&mut function);
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::I32Const(tokens.rbrack));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::Else);

    // --- object: "{" + key:value pairs joined "," + "}" ---
    function.instruction(&Instruction::LocalGet(tb));
    function.instruction(&Instruction::I32Const(TYPE_OBJECT));
    function.instruction(&Instruction::I32Eq);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::I32Const(tokens.lbrace));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(n));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::LocalSet(first));
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::LocalGet(n));
    function.instruction(&Instruction::I32LtU);
    function.instruction(&Instruction::If(BlockType::Empty));
    // key = load key_slot; val = load val_slot (both at ptr + 16*k).
    emit_obj_entry_addr(&mut function, ptr, k);
    function.instruction(&Instruction::I64Load(key_slot));
    function.instruction(&Instruction::LocalSet(key));
    emit_obj_entry_addr(&mut function, ptr, k);
    function.instruction(&Instruction::I64Load(val_slot));
    function.instruction(&Instruction::LocalSet(val));
    // include = key_not_reserved & !(val undefined || val function).
    emit_key_not_reserved(&mut function, key);
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::I64Const(UNDEFINED_BITS));
    function.instruction(&Instruction::I64Eq);
    emit_is_function(&mut function, val);
    function.instruction(&Instruction::I32Or);
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::I32And);
    function.instruction(&Instruction::If(BlockType::Empty));
    // Separator: if !first { acc = concat(acc, ",") }.
    function.instruction(&Instruction::LocalGet(first));
    function.instruction(&Instruction::I32Eqz);
    function.instruction(&Instruction::If(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::I32Const(tokens.comma));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::I32Const(0));
    function.instruction(&Instruction::LocalSet(first));
    // acc = concat(acc, escape(key)); acc = concat(acc, ":");
    // acc = concat(acc, __json_str(val)).
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::Call(escape_index));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::I32Const(tokens.colon));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::Call(self_index));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::End); // if include
    // k += 1; continue.
    function.instruction(&Instruction::LocalGet(k));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(k));
    function.instruction(&Instruction::Br(1));
    function.instruction(&Instruction::End); // if k < n
    function.instruction(&Instruction::End); // loop
    function.instruction(&Instruction::LocalGet(acc));
    function.instruction(&Instruction::I32Const(tokens.rbrace));
    concat_into_acc(&mut function);
    function.instruction(&Instruction::Else);
    // --- any other heap kind (function value): "null" ---
    function.instruction(&Instruction::I32Const(tokens.null));
    function.instruction(&Instruction::LocalSet(acc));
    function.instruction(&Instruction::End); // if OBJECT
    function.instruction(&Instruction::End); // if ARRAY
    function.instruction(&Instruction::End); // if STRING

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
/// (at `convert_index`) and compare as `f64`; anything else compares by raw
/// identity bits (two distinct objects differ; the same object is equal; an
/// object vs a primitive differs). Object↔primitive `ToPrimitive` coercion is
/// deliberately NOT modeled (so `[] == 0` is `false`, a documented v1 cut). The
/// two `i64` params occupy locals 0/1; they are mutated in place for the boolean
/// normalization, so no extra locals are needed.
pub(super) fn emit_loose_eq(str_eq_index: u32, convert_index: u32) -> Function {
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

    // Fallback: raw identity bits.
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

/// Emit the `__str_to_array(s: i64) -> i64` string-spread helper: build a fresh
/// `TYPE_ARRAY` whose elements are the source string's characters as one-byte
/// heap strings (`[..."abc"]` → `["a", "b", "c"]`). The character walk is
/// byte-indexed (matching for-of / indexing over a string), so multi-byte UTF-8
/// is split per byte — the same limitation the rest of the string surface has.
/// `s` is a heap-string jsval; the returned jsval is a boxed heap array. Fixes
/// the pre-existing garbage-value bug where `[...str]` fell through to
/// `__array_concat` (which reinterpreted the string's UTF-8 bytes as jsvals).
pub(super) fn emit_str_to_array(alloc_index: u32, str_index_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (s, ptr, len, arr, i) = (0, 1, 2, 3, 4);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };

    // ptr = wrap(s); len = load_u32(ptr + HEADER_SIZE) (the string's byte length).
    function.instruction(&Instruction::LocalGet(s));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(ptr));
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));

    // arr = __alloc(HEADER_SIZE + 4 + 8*len); write TYPE_ARRAY + u32 len header.
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::Call(alloc_index));
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Const(TYPE_ARRAY));
    function.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32Store(header));

    // Fill element k = box(__str_index(ptr, k)) for k in 0..len; `i` starts at 0.
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // address = arr + 8*i (element slot at MemArg offset HEADER_SIZE + 4).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    // value = HEAP_BITS | extend_u(__str_index(ptr, i)).
    function.instruction(&Instruction::LocalGet(ptr));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::Call(str_index_index));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    }));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::LocalSet(i));
    function.instruction(&Instruction::Br(0));
    function.instruction(&Instruction::End);
    function.instruction(&Instruction::End);

    // Rebox the array block base as a heap jsval.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    // Close the function body.
    function.instruction(&Instruction::End);
    function
}

/// Emit the `__map_find(handle: i32, key: i64) -> i32` Map/Set lookup.
///
/// `handle` is the base of a `TYPE_MAP`/`TYPE_SET` block; its `entries` slot (at
/// `+HEADER_SIZE+4`) holds the backing array jsval. Reads the array's u32 `len`
/// header, then linear-scans the EVEN element slots (the keys) `i = 0, 2, 4, ..`,
/// returning the byte ADDRESS of the first slot whose value is `__same_value_zero` (at
/// `svz_index`) to `key`, or `-1` on a miss. The value slot sits 8 bytes past the
/// returned address. Params occupy locals 0/1; four extra `i32` locals hold the
/// backing base, its length, the loop index, and the current slot address.
pub(super) fn emit_map_find(svz_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (handle, key, arr, len, i, slot) = (0, 1, 2, 3, 4, 5);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let entries_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
        memory_index: 0,
    };
    let elem = MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    // arr = wrap(load_i64(handle + entries_slot)); len = load_u32(arr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I64Load(entries_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));

    // Scan even key slots; `i` starts at 0 (the fresh local's default).
    function.instruction(&Instruction::Block(BlockType::Empty));
    function.instruction(&Instruction::Loop(BlockType::Empty));
    // if i >= len { break -> no match }.
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::LocalGet(len));
    function.instruction(&Instruction::I32GeU);
    function.instruction(&Instruction::BrIf(1));
    // slot = arr + HEADER_SIZE + 4 + 8*i.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(i));
    function.instruction(&Instruction::I32Const(8));
    function.instruction(&Instruction::I32Mul);
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
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
/// `key` then `val` to the growable backing via `__array_push` (at
/// `array_push_index`) — reboxing and REWRITING the handle's `entries` slot in place
/// since the backing base moves on growth — and bumps the u32 `size` header by one.
/// `set.add` reuses this with `val = undefined`. Params occupy locals 0/1/2; two
/// extra `i32` locals hold the found address and the backing base.
pub(super) fn emit_map_set(map_find_index: u32, array_push_index: u32) -> Function {
    let mut function = Function::new(vec![(2, ValType::I32)]);
    let (handle, key, val, addr, arr) = (0, 1, 2, 3, 4);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let entries_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
        align: 0,
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
    // arr = __array_push(arr, key); arr = __array_push(arr, val).
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(key));
    function.instruction(&Instruction::Call(array_push_index));
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::LocalGet(val));
    function.instruction(&Instruction::Call(array_push_index));
    function.instruction(&Instruction::LocalSet(arr));
    // Rewrite the handle's entries slot with the reboxed (grown) backing base.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I64ExtendI32U);
    function.instruction(&Instruction::I64Const(HEAP_BITS));
    function.instruction(&Instruction::I64Or);
    function.instruction(&Instruction::I64Store(entries_slot));
    // size += 1.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Add);
    function.instruction(&Instruction::I32Store(header));
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
/// the backing's u32 `len` header by 2 and the handle's u32 `size` header by 1, and
/// returns `1`. Params occupy locals 0/1; four extra `i32` locals hold the found
/// address, the backing base, its length, and the shift byte count.
pub(super) fn emit_map_delete(map_find_index: u32) -> Function {
    let mut function = Function::new(vec![(4, ValType::I32)]);
    let (handle, key, addr, arr, len, count) = (0, 1, 2, 3, 4, 5);
    let header = MemArg {
        offset: HEADER_SIZE as u64,
        align: 2,
        memory_index: 0,
    };
    let entries_slot = MemArg {
        offset: (HEADER_SIZE + 4) as u64,
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

    // arr = wrap(load_i64(handle + entries_slot)); len = load_u32(arr + HEADER_SIZE).
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I64Load(entries_slot));
    function.instruction(&Instruction::I32WrapI64);
    function.instruction(&Instruction::LocalSet(arr));
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::LocalSet(len));

    // count = (arr + HEADER_SIZE + 4 + 8*len) - (addr + 16): trailing bytes to move.
    function.instruction(&Instruction::LocalGet(arr));
    function.instruction(&Instruction::I32Const(HEADER_SIZE + 4));
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
    function.instruction(&Instruction::I32Store(header));
    // size -= 1.
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::LocalGet(handle));
    function.instruction(&Instruction::I32Load(header));
    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::I32Sub);
    function.instruction(&Instruction::I32Store(header));

    function.instruction(&Instruction::I32Const(1));
    function.instruction(&Instruction::End);
    function
}
