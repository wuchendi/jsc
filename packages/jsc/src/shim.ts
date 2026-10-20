/**
 * The JS runtime shim: the `importObject` that backs the emitted module's host
 * env imports (`print` / `__convert` / `__method` / `__promise` / `__then` /
 * `__now` / `__parse_int`). A port of `crates/jsc-core/src/run/harness.rs`
 * (import wiring + out-of-memory guard), `run/host.rs` (handlers), and
 * `run/promise.rs` (the promise engine + microtask queue). Every heap block is
 * allocated through the guest's exported constructors (`__new_string` /
 * `__new_array` / `__new_promise`), never by hand. It
 * duplicates Rust host semantics against the emitted module's memory (see
 * README, "Host parity").
 */

import {
  ASYNC_STEP_EXPORT_NAME,
  CONVERT_OP_PARSE_FLOAT,
  CONVERT_OP_TO_NUMBER,
  CONVERT_OP_TO_STRING,
  EXC_PENDING_EXPORT_NAME,
  EXC_VALUE_EXPORT_NAME,
  FALSE,
  GEN_DONE_OFFSET,
  HEADER_SIZE,
  HEAP,
  HEAP_PTR_EXPORT_NAME,
  HEAP_PTR_OOM,
  INVOKE_EXPORT_NAME,
  MEMORY_EXPORT_NAME,
  MEMORY_MAX_PAGES,
  METHOD_OP_CHAR_AT,
  METHOD_OP_ENDS_WITH,
  METHOD_OP_INCLUDES,
  METHOD_OP_INDEX_OF,
  METHOD_OP_JOIN,
  METHOD_OP_PAD_END,
  METHOD_OP_PAD_START,
  METHOD_OP_REPEAT,
  METHOD_OP_SLICE,
  METHOD_OP_SPLIT,
  METHOD_OP_STARTS_WITH,
  METHOD_OP_SUBSTRING,
  METHOD_OP_TO_LOWER,
  METHOD_OP_TO_UPPER,
  METHOD_OP_TRIM,
  METHOD_OP_TRIM_END,
  METHOD_OP_TRIM_START,
  NEW_ARRAY_EXPORT_NAME,
  NEW_PROMISE_EXPORT_NAME,
  NEW_STRING_EXPORT_NAME,
  PRINT_FLAG_END,
  PRINT_FLAG_SEP,
  PROMISE_BLOCK_SIZE,
  PROMISE_FULFILLED,
  PROMISE_OP_ASYNC_SPAWN,
  PROMISE_OP_AWAIT,
  PROMISE_OP_FINALLY,
  PROMISE_OP_NEW,
  PROMISE_OP_REJECT,
  PROMISE_OP_RESOLVE,
  PROMISE_OP_SETTLE_FULFILL,
  PROMISE_OP_SETTLE_REJECT,
  PROMISE_PENDING,
  PROMISE_REJECTED,
  PROMISE_STATE_OFFSET,
  PROMISE_VALUE_OFFSET,
  QNAN,
  TRUE,
  TYPE_ARRAY,
  TYPE_FUNCTION,
  TYPE_PROMISE,
  TYPE_STRING,
  U32_MASK,
  UNDEFINED,
} from './abi.js'
import {
  asU64,
  decode,
  f64ToBits,
  formatValue,
  parseFloatStr,
  parseIntStr,
  valueToNumber,
} from './value.js'

/** The wired shim: the `importObject`, the accumulated stdout, and instance binding. */
export interface Shim {
  /** Passed as the import object to `WebAssembly.instantiate`. */
  imports: WebAssembly.Imports
  /** Everything `console.log` printed, in execution order (mirrors `run_capturing`). */
  stdout: () => string
  /** Bind the instantiated module so the imports can read its memory and call its constructors. */
  bindInstance: (instance: WebAssembly.Instance) => void
  /**
   * Drain the microtask queue to empty, then throw
   * `Error("Uncaught RangeError: out of memory")` if the heap is exhausted, or
   * `Error("Uncaught (in promise) <value>")` for the first rejected promise that
   * never got a handler (mirrors `harness.rs` / `promise.rs::drain_after_main`).
   */
  drainAfterMain: () => void
  /** Whether `__heap_ptr` holds the out-of-memory sentinel (mirrors `host.rs::heap_exhausted`). */
  heapExhausted: () => boolean
}

/**
 * A reaction registered on a still-pending promise (mirrors
 * `promise.rs::Reaction`): the two handlers (`undefined` = passthrough), the
 * chained result promise's block base, `finally` semantics, and — for an
 * `async function` continuation — the body's generator handle.
 */
interface Reaction {
  onFulfilled: bigint
  onRejected: bigint
  result: number
  finally: boolean
  asyncGen: bigint | null
}

/**
 * A queued microtask (mirrors `promise.rs::Job`): the selected callback, the
 * source settlement, and the chained promise to settle from the outcome.
 */
interface Job {
  callback: bigint
  state: number
  value: bigint
  result: number
  finally: boolean
  asyncGen: bigint | null
}

/** The linear-memory cap in bytes; constructor lengths are clamped to it (mirrors `host.rs`). */
const MEMORY_MAX_BYTES = MEMORY_MAX_PAGES * 0x1_0000

/** The rendered error for an exhausted heap (mirrors `harness.rs::OUT_OF_MEMORY`). */
export const OUT_OF_MEMORY = 'Uncaught RangeError: out of memory'

const utf8Encode = new TextEncoder()
const utf8Decoder = new TextDecoder()

/** Decode UTF-8 bytes, lossily replacing invalid sequences (matches `from_utf8_lossy`). */
function utf8Decode(bytes: Uint8Array): string {
  return utf8Decoder.decode(bytes)
}

/** Build a fresh shim. One per `run()` call so stdout does not bleed across runs. */
export function createShim(): Shim {
  let instance: WebAssembly.Instance | null = null
  let out = ''

  /** The module's linear memory, or `undefined` before binding / when absent. */
  function memory(): WebAssembly.Memory | undefined {
    return instance?.exports[MEMORY_EXPORT_NAME] as
      | WebAssembly.Memory
      | undefined
  }

  /** A live view of linear memory (recreated each call: the buffer detaches on grow). */
  function memView(): Uint8Array {
    const mem = memory()
    return mem ? new Uint8Array(mem.buffer) : new Uint8Array(0)
  }

  /** An exported global by name, or `undefined` when absent. */
  function exportGlobal(name: string): WebAssembly.Global | undefined {
    return instance?.exports[name] as WebAssembly.Global | undefined
  }

  /** Whether `__heap_ptr` holds the out-of-memory sentinel (mirrors `host.rs::heap_exhausted`). */
  function heapExhausted(): boolean {
    const global = exportGlobal(HEAP_PTR_EXPORT_NAME)
    return global !== undefined && global.value === HEAP_PTR_OOM
  }

  /**
   * Pass a host import's result through, or throw the out-of-memory error when
   * the import (or an earlier allocation) left the sentinel in `__heap_ptr`
   * (mirrors `harness.rs::oom_guard`).
   */
  function oomGuard(bits: bigint): bigint {
    if (heapExhausted()) throw new Error(OUT_OF_MEMORY)
    return bits
  }

  /**
   * Call the guest constructor export `name(...args) -> base` and return the new
   * block's base offset, or `null` when the export is missing or the call traps
   * — an out-of-memory trap leaves the sentinel for {@link oomGuard} (mirrors
   * `host.rs::guest_new`).
   */
  function guestNew(name: string, ...args: number[]): number | null {
    const ctor = instance?.exports[name] as
      | ((...a: number[]) => number)
      | undefined
    if (!ctor) return null
    try {
      return ctor(...args) >>> 0
    } catch {
      return null
    }
  }

  /**
   * Allocate a heap STRING block holding `text` through `__new_string`, filling
   * only the payload (mirrors `host.rs::alloc_string`).
   */
  function allocString(text: string): bigint | null {
    const bytes = utf8Encode.encode(text)
    const base = guestNew(
      NEW_STRING_EXPORT_NAME,
      Math.min(bytes.length, MEMORY_MAX_BYTES),
    )
    if (base === null) return null
    const view = memView()
    const at = base + HEADER_SIZE + 4
    if (at + bytes.length > view.length) return null
    view.set(bytes, at)
    return BigInt(base)
  }

  /**
   * Allocate a heap ARRAY block holding the jsval bits `elems` through
   * `__new_array`, filling only the elements (mirrors `host.rs::alloc_array`).
   */
  function allocArray(elems: bigint[]): bigint | null {
    const base = guestNew(
      NEW_ARRAY_EXPORT_NAME,
      Math.min(elems.length, MEMORY_MAX_BYTES / 8),
    )
    const mem = memory()
    if (base === null || !mem) return null
    const at = base + HEADER_SIZE + 4
    if (at + 8 * elems.length > mem.buffer.byteLength) return null
    const dv = new DataView(mem.buffer)
    elems.forEach((bits, i) => {
      dv.setBigUint64(at + 8 * i, bits, true)
    })
    return BigInt(base)
  }

  /** `array.join(sep)` — allocates the joined heap string (mirrors `host.rs::array_join`). */
  function arrayJoin(
    mem: Uint8Array,
    ptr: number,
    sepBits: bigint,
  ): bigint | null {
    const base = allocString(arrayJoinText(mem, ptr, sepBits))
    return base === null ? null : HEAP | base
  }

  /** `array.slice(a, b)` — allocates a NEW heap array (mirrors `host.rs::array_slice`). */
  function arraySlice(
    mem: Uint8Array,
    ptr: number,
    a: bigint,
    b: bigint,
  ): bigint | null {
    const len = blockLen(mem, ptr)
    const start = sliceIndex(a, mem, 0, len)
    const end = Math.max(sliceIndex(b, mem, len, len), start)
    const elems: bigint[] = []
    for (let i = start; i < end; i++) {
      const elem = arrayElemBits(mem, ptr, i)
      if (elem === null) return null
      elems.push(elem)
    }
    const base = allocArray(elems)
    return base === null ? null : HEAP | base
  }

  /** `string.slice(a, b)` — allocates a NEW substring by BYTE index (mirrors `string_slice`). */
  function stringSlice(
    mem: Uint8Array,
    ptr: number,
    a: bigint,
    b: bigint,
  ): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    const len = bytes.length
    const start = sliceIndex(a, mem, 0, len)
    const end = Math.max(sliceIndex(b, mem, len, len), start)
    const base = allocString(utf8Decode(bytes.subarray(start, end)))
    return base === null ? null : HEAP | base
  }

  /** `string.trim`/`trimStart`/`trimEnd` — NEW string minus ASCII whitespace (mirrors `string_trim`). */
  function stringTrim(mem: Uint8Array, ptr: number, op: number): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    const trimStart = op !== METHOD_OP_TRIM_END
    const trimEnd = op !== METHOD_OP_TRIM_START
    const base = allocString(utf8Decode(trimBytes(bytes, trimStart, trimEnd)))
    return base === null ? null : HEAP | base
  }

  /** `string.repeat(n)` — NEW string of `n` copies (mirrors `string_repeat`). */
  function stringRepeat(
    mem: Uint8Array,
    ptr: number,
    arg0: bigint,
  ): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    // A negative/NaN (=> 0) count yields "" (JS throws a RangeError — a
    // documented limitation).
    const n = Math.max(truncToInt(valueToNumber(decode(arg0, mem))), 0)
    const out = new Uint8Array(bytes.length * n)
    for (let i = 0; i < n; i++) out.set(bytes, i * bytes.length)
    const base = allocString(utf8Decode(out))
    return base === null ? null : HEAP | base
  }

  /** `string.padStart`/`padEnd(len, pad)` — NEW padded string (mirrors `string_pad`). */
  function stringPad(
    mem: Uint8Array,
    ptr: number,
    arg0: bigint,
    arg1: bigint,
    op: number,
  ): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    const target = truncToInt(valueToNumber(decode(arg0, mem)))
    // The pad string defaults to a single space when omitted.
    const padValue = decode(arg1, mem)
    const pad =
      padValue.kind === 'undefined'
        ? utf8Encode.encode(' ')
        : utf8Encode.encode(formatValue(padValue))
    const base = allocString(
      padBytes(bytes, target, pad, op === METHOD_OP_PAD_START),
    )
    return base === null ? null : HEAP | base
  }

  /** `string.substring(a, b)` — NEW `[a, b)` BYTE slice (mirrors `string_substring`). */
  function stringSubstring(
    mem: Uint8Array,
    ptr: number,
    arg0: bigint,
    arg1: bigint,
  ): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    const len = bytes.length
    const a = substringIndex(arg0, mem, 0, len)
    const b = substringIndex(arg1, mem, len, len)
    const lo = Math.min(a, b)
    const hi = Math.max(a, b)
    const base = allocString(utf8Decode(bytes.subarray(lo, hi)))
    return base === null ? null : HEAP | base
  }

  /** `string.split(sep)` — NEW array of substrings (mirrors `string_split`). */
  function stringSplit(
    mem: Uint8Array,
    ptr: number,
    sepBits: bigint,
  ): bigint | null {
    const bytes = stringBytes(mem, ptr)
    if (bytes === null) return null
    const sepValue = decode(sepBits, mem)
    let pieces: Uint8Array[]
    if (sepValue.kind === 'undefined') {
      // An omitted separator yields a single-element array of the whole string.
      pieces = [bytes]
    } else {
      const sep = utf8Encode.encode(formatValue(sepValue))
      pieces =
        sep.length === 0
          ? // An empty separator splits into one element per byte.
            Array.from(bytes, (byte) => Uint8Array.of(byte))
          : splitOn(bytes, sep)
    }
    // Allocate each piece as a heap string first (the pieces are independent
    // views/copies), then build the backing array from their bits.
    const elems: bigint[] = []
    for (const piece of pieces) {
      const base = allocString(utf8Decode(piece))
      if (base === null) return null
      elems.push(HEAP | base)
    }
    const base = allocArray(elems)
    return base === null ? null : HEAP | base
  }

  /**
   * Dispatch a `__method` call (mirrors `host.rs::method_dispatch`): snapshot
   * memory, read the receiver's TYPE BYTE, and perform the array/string method,
   * returning the result jsval bits (or `null` to yield `undefined`).
   */
  function methodDispatch(
    recv: bigint,
    op: number,
    arg0: bigint,
    arg1: bigint,
  ): bigint | null {
    // Snapshot memory (a copy) so reads below are stable across a later
    // guest allocation that may grow/detach the buffer — mirrors the Rust snapshot.
    const mem = memView().slice()
    const ptr = heapPtr(recv)
    const typeByte = ptr === null ? undefined : mem[ptr]
    switch (op) {
      case METHOD_OP_INDEX_OF:
        if (ptr !== null && typeByte === TYPE_ARRAY)
          return arrayIndexOf(mem, ptr, arg0)
        if (ptr !== null && typeByte === TYPE_STRING)
          return stringIndexOf(mem, ptr, arg0)
        return null
      case METHOD_OP_INCLUDES:
        if (ptr !== null && typeByte === TYPE_ARRAY) {
          return arrayIndexOf(mem, ptr, arg0) === f64ToBits(-1) ? FALSE : TRUE
        }
        // `str.includes(sub)` — substring containment (an empty `sub` => true).
        if (ptr !== null && typeByte === TYPE_STRING) {
          const hay = stringBytes(mem, ptr)
          if (hay === null) return null
          return byteContains(hay, argBytes(arg0, mem)) ? TRUE : FALSE
        }
        return null
      case METHOD_OP_JOIN:
        if (ptr !== null && typeByte === TYPE_ARRAY)
          return arrayJoin(mem, ptr, arg0)
        return null
      case METHOD_OP_SLICE:
        if (ptr !== null && typeByte === TYPE_ARRAY)
          return arraySlice(mem, ptr, arg0, arg1)
        if (ptr !== null && typeByte === TYPE_STRING)
          return stringSlice(mem, ptr, arg0, arg1)
        return null
      case METHOD_OP_TO_UPPER:
      case METHOD_OP_TO_LOWER: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const bytes = stringBytes(mem, ptr)
        if (bytes === null) return null
        const mapped = bytes.map((byte) =>
          op === METHOD_OP_TO_UPPER ? asciiUpper(byte) : asciiLower(byte),
        )
        const base = allocString(utf8Decode(mapped))
        return base === null ? null : HEAP | base
      }
      case METHOD_OP_CHAR_AT: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const bytes = stringBytes(mem, ptr)
        if (bytes === null) return null
        const i = truncToInt(valueToNumber(decode(arg0, mem)))
        // Out-of-range (a negative index) => "".
        const text =
          i >= 0 && i < bytes.length ? utf8Decode(bytes.subarray(i, i + 1)) : ''
        const base = allocString(text)
        return base === null ? null : HEAP | base
      }
      case METHOD_OP_TRIM:
      case METHOD_OP_TRIM_START:
      case METHOD_OP_TRIM_END:
        if (ptr === null || typeByte !== TYPE_STRING) return null
        return stringTrim(mem, ptr, op)
      case METHOD_OP_REPEAT:
        if (ptr === null || typeByte !== TYPE_STRING) return null
        return stringRepeat(mem, ptr, arg0)
      case METHOD_OP_STARTS_WITH:
      case METHOD_OP_ENDS_WITH: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const hay = stringBytes(mem, ptr)
        if (hay === null) return null
        const needle = argBytes(arg0, mem)
        const hit =
          op === METHOD_OP_STARTS_WITH
            ? startsWithBytes(hay, needle)
            : endsWithBytes(hay, needle)
        return hit ? TRUE : FALSE
      }
      case METHOD_OP_PAD_START:
      case METHOD_OP_PAD_END:
        if (ptr === null || typeByte !== TYPE_STRING) return null
        return stringPad(mem, ptr, arg0, arg1, op)
      case METHOD_OP_SUBSTRING:
        if (ptr === null || typeByte !== TYPE_STRING) return null
        return stringSubstring(mem, ptr, arg0, arg1)
      case METHOD_OP_SPLIT:
        if (ptr === null || typeByte !== TYPE_STRING) return null
        return stringSplit(mem, ptr, arg0)
      default:
        return null
    }
  }

  // ---- promise engine (ported from promise.rs) ----------------------------

  /** The FIFO microtask queue. */
  const queue: Job[] = []
  /** Reactions of each still-pending promise, keyed by block base offset. */
  const reactions = new Map<number, Reaction[]>()
  /** Every rejected promise, by block base (ascending base = creation order). */
  const rejected = new Set<number>()
  /** Every promise that had a reaction registered (then/catch/finally, await, adoption). */
  const handled = new Set<number>()

  /** An exported `(i64, i64) -> i64` trampoline by name, or `undefined` when absent. */
  function exportTrampoline(
    name: string,
  ): ((a: bigint, b: bigint) => bigint) | undefined {
    return instance?.exports[name] as
      | ((a: bigint, b: bigint) => bigint)
      | undefined
  }

  /**
   * Allocate a fresh promise block through `__new_promise`, settle it to
   * `state`/`value` unless pending, and return its heap jsval, or `null` on a
   * failed allocation (mirrors `promise.rs::alloc_promise`).
   */
  function allocPromise(state: number, value: bigint): bigint | null {
    const base = guestNew(NEW_PROMISE_EXPORT_NAME)
    if (base === null) return null
    if (state !== PROMISE_PENDING) writeSettlement(base, state, value)
    if (state === PROMISE_REJECTED) rejected.add(base)
    return HEAP | BigInt(base)
  }

  /** Write a promise block's settlement into guest memory (mirrors `write_settlement`). */
  function writeSettlement(base: number, state: number, value: bigint): void {
    const mem = memory()
    if (!mem || base + PROMISE_BLOCK_SIZE > mem.buffer.byteLength) return
    const dv = new DataView(mem.buffer)
    dv.setInt32(base + PROMISE_STATE_OFFSET, state, true)
    dv.setBigUint64(base + PROMISE_VALUE_OFFSET, value, true)
  }

  /**
   * Run the guest trampoline `name(a, b)` re-entrantly (mirrors
   * `promise.rs::invoke1` / `async_step`): returns its result bits and whether
   * it threw — detected by `__exc_pending`, which (with `__exc_value`) is
   * cleared so a surfaced exception does not leak into the next microtask. A
   * missing export or a trap yields `[undefined, false]`.
   */
  function callGuest(name: string, a: bigint, b: bigint): [bigint, boolean] {
    const func = exportTrampoline(name)
    const pending = exportGlobal(EXC_PENDING_EXPORT_NAME)
    const excValue = exportGlobal(EXC_VALUE_EXPORT_NAME)
    if (!func || !pending || !excValue) return [UNDEFINED, false]
    let result: bigint
    try {
      result = asU64(func(a, b))
    } catch {
      return [UNDEFINED, false]
    }
    if (Number(pending.value) !== 0) {
      const value = asU64(excValue.value as bigint)
      pending.value = 0
      excValue.value = 0n
      return [value, true]
    }
    return [result, false]
  }

  /**
   * Settle promise `base` once; fulfilling with a promise ADOPTS it. On a real
   * settlement its reactions move into the queue (mirrors `promise.rs::settle`).
   */
  function settle(base: number, state: number, value: bigint): void {
    if (readState(memView(), base) !== PROMISE_PENDING) return
    if (state === PROMISE_FULFILLED && asPromise(memView(), value) !== null) {
      registerSource(value, UNDEFINED, UNDEFINED, base, false, null)
      return
    }
    writeSettlement(base, state, value)
    if (state === PROMISE_REJECTED) rejected.add(base)
    const pendingReactions = reactions.get(base) ?? []
    reactions.delete(base)
    for (const r of pendingReactions) {
      queue.push({
        callback: state === PROMISE_FULFILLED ? r.onFulfilled : r.onRejected,
        state,
        value,
        result: r.result,
        finally: r.finally,
        asyncGen: r.asyncGen,
      })
    }
  }

  /**
   * Register a reaction on `source`: stored on a pending promise, queued now
   * for a settled one, queued as fulfilled for a non-promise value (mirrors
   * `promise.rs::register_source`).
   */
  function registerSource(
    source: bigint,
    onFulfilled: bigint,
    onRejected: bigint,
    result: number,
    isFinally: boolean,
    asyncGen: bigint | null,
  ): void {
    const mem = memView()
    const base = asPromise(mem, source)
    if (base === null) {
      queue.push({
        callback: onFulfilled,
        state: PROMISE_FULFILLED,
        value: source,
        result,
        finally: isFinally,
        asyncGen,
      })
      return
    }
    const state = readState(mem, base)
    handled.add(base)
    if (state === PROMISE_PENDING) {
      const list = reactions.get(base) ?? []
      list.push({
        onFulfilled,
        onRejected,
        result,
        finally: isFinally,
        asyncGen,
      })
      reactions.set(base, list)
      return
    }
    queue.push({
      callback: state === PROMISE_FULFILLED ? onFulfilled : onRejected,
      state,
      value: readValue(mem, base),
      result,
      finally: isFinally,
      asyncGen,
    })
  }

  /**
   * Advance an `async function` body one step and route the outcome (mirrors
   * `promise.rs::drive_async`): reject on throw, fulfill on `done`, otherwise
   * register a continuation on the awaited value.
   */
  function driveAsync(gen: bigint, resultBase: number, sent: bigint): void {
    const [value, threw] = callGuest(ASYNC_STEP_EXPORT_NAME, gen, sent)
    if (threw) {
      settle(resultBase, PROMISE_REJECTED, value)
      return
    }
    if (readGenDone(memView(), gen)) {
      settle(resultBase, PROMISE_FULFILLED, value)
      return
    }
    registerSource(value, UNDEFINED, UNDEFINED, resultBase, false, gen)
  }

  /** Run one microtask (mirrors `promise.rs::run_job`). */
  function runJob(job: Job): void {
    if (job.asyncGen !== null) {
      if (job.state === PROMISE_FULFILLED) {
        driveAsync(job.asyncGen, job.result, job.value)
      } else {
        settle(job.result, PROMISE_REJECTED, job.value)
      }
      return
    }
    if (!asFunction(memView(), job.callback)) {
      settle(job.result, job.state, job.value)
      return
    }
    const [outcome, threw] = callGuest(
      INVOKE_EXPORT_NAME,
      job.callback,
      job.value,
    )
    if (threw) settle(job.result, PROMISE_REJECTED, outcome)
    else if (job.finally) settle(job.result, job.state, job.value)
    else settle(job.result, PROMISE_FULFILLED, outcome)
  }

  /** Drive the microtask queue to empty, FIFO (mirrors `promise.rs::drain`). */
  function drain(): void {
    for (let job = queue.shift(); job !== undefined; job = queue.shift()) {
      runJob(job)
    }
  }

  /**
   * Top-level `await p`: drain the queue, then return `p`'s fulfillment value,
   * or surface a rejection through the exception globals (mirrors
   * `promise.rs::await_promise`). A non-promise is returned unchanged.
   */
  function awaitPromise(p: bigint): bigint {
    const base = asPromise(memView(), p)
    if (base === null) return p
    handled.add(base)
    drain()
    const mem = memView()
    const state = readState(mem, base)
    if (state === PROMISE_FULFILLED) return readValue(mem, base)
    if (state === PROMISE_REJECTED) {
      const pending = exportGlobal(EXC_PENDING_EXPORT_NAME)
      const excValue = exportGlobal(EXC_VALUE_EXPORT_NAME)
      if (excValue) excValue.value = BigInt.asIntN(64, readValue(mem, base))
      if (pending) pending.value = 1
    }
    return UNDEFINED
  }

  /** `__promise(op, a, b)` (mirrors `promise.rs::dispatch`). */
  function promiseDispatch(op: number, a: bigint, b: bigint): bigint {
    switch (op) {
      case PROMISE_OP_RESOLVE:
        return asPromise(memView(), a) !== null
          ? a
          : (allocPromise(PROMISE_FULFILLED, a) ?? UNDEFINED)
      case PROMISE_OP_REJECT:
        return allocPromise(PROMISE_REJECTED, a) ?? UNDEFINED
      case PROMISE_OP_FINALLY: {
        const result = allocPromise(PROMISE_PENDING, UNDEFINED)
        if (result === null) return UNDEFINED
        registerSource(a, b, b, baseOf(result), true, null)
        return result
      }
      case PROMISE_OP_AWAIT:
        return awaitPromise(a)
      case PROMISE_OP_NEW:
        return allocPromise(PROMISE_PENDING, UNDEFINED) ?? UNDEFINED
      case PROMISE_OP_SETTLE_FULFILL:
      case PROMISE_OP_SETTLE_REJECT: {
        const base = asPromise(memView(), a)
        if (base !== null) {
          settle(
            base,
            op === PROMISE_OP_SETTLE_FULFILL
              ? PROMISE_FULFILLED
              : PROMISE_REJECTED,
            b,
          )
        }
        return UNDEFINED
      }
      case PROMISE_OP_ASYNC_SPAWN: {
        const result = allocPromise(PROMISE_PENDING, UNDEFINED)
        if (result === null) return UNDEFINED
        driveAsync(a, baseOf(result), UNDEFINED)
        return result
      }
      default:
        return UNDEFINED
    }
  }

  /** `__then(p, onF, onR)` (mirrors `promise.rs::then`). */
  function promiseThen(p: bigint, onF: bigint, onR: bigint): bigint {
    const result = allocPromise(PROMISE_PENDING, UNDEFINED)
    if (result === null) return UNDEFINED
    registerSource(p, onF, onR, baseOf(result), false, null)
    return result
  }

  const print = (bits: bigint, flag: number): void => {
    let piece: string
    if (flag === PRINT_FLAG_SEP || flag === PRINT_FLAG_END) {
      const text = formatValue(decode(asU64(bits), memView()))
      piece = flag === PRINT_FLAG_SEP ? `${text} ` : `${text}\n`
    } else {
      // Newline-only (zero-argument `console.log()`).
      piece = '\n'
    }
    out += piece
  }

  const convert = (bits: bigint, op: number): bigint => {
    const u = asU64(bits)
    const value = decode(u, memView())
    switch (op) {
      case CONVERT_OP_TO_NUMBER:
        return f64ToBits(valueToNumber(value))
      case CONVERT_OP_PARSE_FLOAT:
        return f64ToBits(parseFloatStr(formatValue(value)))
      case CONVERT_OP_TO_STRING: {
        // A string argument is returned unchanged (no allocation).
        if (value.kind === 'string') return u
        const base = allocString(formatValue(value))
        return base === null ? UNDEFINED : HEAP | base
      }
      default:
        return UNDEFINED
    }
  }

  const method = (
    recv: bigint,
    op: number,
    arg0: bigint,
    arg1: bigint,
  ): bigint =>
    oomGuard(
      methodDispatch(asU64(recv), op, asU64(arg0), asU64(arg1)) ?? UNDEFINED,
    )

  const imports: WebAssembly.Imports = {
    env: {
      print,
      __convert: (bits: bigint, op: number): bigint =>
        oomGuard(convert(bits, op)),
      __method: method,
      __promise: (op: number, a: bigint, b: bigint): bigint =>
        oomGuard(promiseDispatch(op, asU64(a), asU64(b))),
      __then: (p: bigint, onF: bigint, onR: bigint): bigint =>
        oomGuard(promiseThen(asU64(p), asU64(onF), asU64(onR))),
      // The host wall clock backing `Date.now()` / `new Date()` (ms since epoch).
      __now: (): number => Date.now(),
      // `parseInt(s, radix)` (an omitted radix arrives as `undefined`).
      __parse_int: (s: bigint, radix: bigint): bigint =>
        f64ToBits(
          parseIntStr(
            formatValue(decode(asU64(s), memView())),
            valueToNumber(decode(asU64(radix), memView())),
          ),
        ),
    },
  }

  return {
    imports,
    stdout: () => out,
    bindInstance: (bound) => {
      instance = bound
    },
    drainAfterMain: () => {
      drain()
      if (heapExhausted()) throw new Error(OUT_OF_MEMORY)
      const unhandled = [...rejected]
        .filter((base) => !handled.has(base))
        .sort((a, b) => a - b)[0]
      if (unhandled === undefined) return
      const mem = memView()
      throw new Error(
        `Uncaught (in promise) ${formatValue(decode(readValue(mem, unhandled), mem))}`,
      )
    },
    heapExhausted,
  }
}

// ---- pure helpers ported from host.rs (no allocation) ---------------------

/** The linear-memory byte offset of a heap jsval, or `null` for a non-heap value. */
function heapPtr(bits: bigint): number | null {
  if ((bits & QNAN) === QNAN && ((bits >> 48n) & 0b111n) === 0b101n) {
    return Number(bits & U32_MASK)
  }
  return null
}

/** The base offset (low 32 bits) of a heap jsval. */
function baseOf(bits: bigint): number {
  return Number(bits & U32_MASK)
}

/** The block base of a heap jsval whose TYPE BYTE is {@link TYPE_PROMISE}, else `null`. */
function asPromise(mem: Uint8Array, bits: bigint): number | null {
  const base = heapPtr(bits)
  return base !== null && mem[base] === TYPE_PROMISE ? base : null
}

/** Whether `bits` is a heap function value (TYPE BYTE {@link TYPE_FUNCTION}). */
function asFunction(mem: Uint8Array, bits: bigint): boolean {
  const base = heapPtr(bits)
  return base !== null && mem[base] === TYPE_FUNCTION
}

/** A promise block's `i32` state; a truncated block reads as pending. */
function readState(mem: Uint8Array, base: number): number {
  const at = base + PROMISE_STATE_OFFSET
  if (at + 4 > mem.length) return PROMISE_PENDING
  return new DataView(mem.buffer, mem.byteOffset).getInt32(at, true)
}

/** A promise block's settled value bits; a truncated block reads as `undefined`. */
function readValue(mem: Uint8Array, base: number): bigint {
  const at = base + PROMISE_VALUE_OFFSET
  if (at + 8 > mem.length) return UNDEFINED
  return new DataView(mem.buffer, mem.byteOffset).getBigUint64(at, true)
}

/** An async-body generator handle's `done` flag (mirrors `promise.rs::read_gen_done`). */
function readGenDone(mem: Uint8Array, gen: bigint): boolean {
  const base = heapPtr(gen)
  if (base === null) return false
  const at = base + GEN_DONE_OFFSET
  if (at + 4 > mem.length) return false
  return new DataView(mem.buffer, mem.byteOffset).getInt32(at, true) !== 0
}

/** The `u32` `len` header of an array/string block at `ptr + HEADER_SIZE`. */
function blockLen(mem: Uint8Array, ptr: number): number {
  const at = ptr + HEADER_SIZE
  if (at + 4 > mem.length) return 0
  return (
    (mem[at]! |
      (mem[at + 1]! << 8) |
      (mem[at + 2]! << 16) |
      (mem[at + 3]! << 24)) >>>
    0
  )
}

/** The raw jsval bits of array element `i`, or `null` when out of range. */
function arrayElemBits(mem: Uint8Array, ptr: number, i: number): bigint | null {
  const start = ptr + HEADER_SIZE + 4 + 8 * i
  if (start + 8 > mem.length) return null
  let value = 0n
  for (let k = 7; k >= 0; k--) value = (value << 8n) | BigInt(mem[start + k]!)
  return value
}

/** The UTF-8 payload bytes of a heap STRING block, or `null` when truncated. */
function stringBytes(mem: Uint8Array, ptr: number): Uint8Array | null {
  const len = blockLen(mem, ptr)
  const start = ptr + HEADER_SIZE + 4
  if (start + len > mem.length) return null
  return mem.subarray(start, start + len)
}

/** Truncate a JS number toward zero to an integer (matches `host.rs::trunc_to_int`). */
function truncToInt(n: number): number {
  return Math.trunc(n)
}

/** Number jsval bits for a non-negative sequence index (lossless `u32 -> f64`). */
function indexBits(i: number): bigint {
  return f64ToBits(i >>> 0)
}

function asciiUpper(b: number): number {
  return b >= 0x61 && b <= 0x7a ? b - 0x20 : b
}

function asciiLower(b: number): number {
  return b >= 0x41 && b <= 0x5a ? b + 0x20 : b
}

/**
 * JS strict equality (`===`) between two raw jsval bits (mirrors
 * `host.rs::strict_eq_bits`): numbers by `f64` value, strings by content, and
 * everything else by identical raw bits.
 */
function strictEqBits(a: bigint, b: bigint, mem: Uint8Array): boolean {
  const va = decode(a, mem)
  const vb = decode(b, mem)
  if (va.kind === 'number' && vb.kind === 'number') return va.value === vb.value
  if (va.kind === 'string' && vb.kind === 'string') return va.value === vb.value
  return a === b
}

/** `array.indexOf(x)`: first index whose element `=== x`, or `-1`. */
function arrayIndexOf(mem: Uint8Array, ptr: number, needle: bigint): bigint {
  const len = blockLen(mem, ptr)
  for (let i = 0; i < len; i++) {
    const elem = arrayElemBits(mem, ptr, i)
    if (elem !== null && strictEqBits(elem, needle, mem)) return indexBits(i)
  }
  return f64ToBits(-1)
}

/** `string.indexOf(sub)`: first BYTE index where `sub` occurs (empty => 0), or `-1`. */
function stringIndexOf(mem: Uint8Array, ptr: number, sub: bigint): bigint {
  const hay = stringBytes(mem, ptr)
  if (hay === null) return f64ToBits(-1)
  const subValue = decode(sub, mem)
  if (subValue.kind !== 'string') return f64ToBits(-1)
  const needle = utf8Encode.encode(subValue.value)
  if (needle.length === 0) return indexBits(0)
  for (let i = 0; i + needle.length <= hay.length; i++) {
    let match = true
    for (let k = 0; k < needle.length; k++) {
      if (hay[i + k] !== needle[k]) {
        match = false
        break
      }
    }
    if (match) return indexBits(i)
  }
  return f64ToBits(-1)
}

/**
 * The joined text of `array.join(sep)` (`","` when `sep` is `undefined`);
 * `null`/`undefined` elements stringify to `""`. Mirrors the pure part of
 * `host.rs::array_join` (allocation is done by the caller).
 */
function arrayJoinText(mem: Uint8Array, ptr: number, sepBits: bigint): string {
  const sepValue = decode(sepBits, mem)
  const sep = sepValue.kind === 'undefined' ? ',' : formatValue(sepValue)
  const len = blockLen(mem, ptr)
  const pieces: string[] = []
  for (let i = 0; i < len; i++) {
    const elem = arrayElemBits(mem, ptr, i)
    const v = elem === null ? null : decode(elem, mem)
    pieces.push(
      v === null || v.kind === 'null' || v.kind === 'undefined'
        ? ''
        : formatValue(v),
    )
  }
  return pieces.join(sep)
}

/**
 * Resolve one JS slice endpoint against a sequence `len` (mirrors
 * `host.rs::slice_index`): `undefined` uses `fallback`, otherwise the argument is
 * truncated, offset from the end when negative, and clamped to `0..=len`.
 */
function sliceIndex(
  bits: bigint,
  mem: Uint8Array,
  fallback: number,
  len: number,
): number {
  const decoded = decode(bits, mem)
  if (decoded.kind === 'undefined') return fallback
  const n = truncToInt(valueToNumber(decoded))
  const idx = n < 0 ? Math.max(len + n, 0) : Math.min(n, len)
  return Math.max(idx, 0)
}

/** Whether `b` is ASCII whitespace (matches Rust `u8::is_ascii_whitespace`). */
function isAsciiWhitespace(b: number): boolean {
  return b === 0x20 || b === 0x09 || b === 0x0a || b === 0x0c || b === 0x0d
}

/**
 * Coerce a method argument's raw bits to its JS string form as bytes (mirrors
 * `host.rs::arg_bytes`): a string yields its payload, everything else its
 * `formatValue` rendering.
 */
function argBytes(bits: bigint, mem: Uint8Array): Uint8Array {
  return utf8Encode.encode(formatValue(decode(bits, mem)))
}

/** Whether `hay` contains the contiguous `needle` (empty => true; mirrors `byte_contains`). */
function byteContains(hay: Uint8Array, needle: Uint8Array): boolean {
  if (needle.length === 0) return true
  for (let i = 0; i + needle.length <= hay.length; i++) {
    let match = true
    for (let k = 0; k < needle.length; k++) {
      if (hay[i + k] !== needle[k]) {
        match = false
        break
      }
    }
    if (match) return true
  }
  return false
}

/** Whether `hay` begins with `needle`. */
function startsWithBytes(hay: Uint8Array, needle: Uint8Array): boolean {
  if (needle.length > hay.length) return false
  for (let i = 0; i < needle.length; i++) if (hay[i] !== needle[i]) return false
  return true
}

/** Whether `hay` ends with `needle`. */
function endsWithBytes(hay: Uint8Array, needle: Uint8Array): boolean {
  if (needle.length > hay.length) return false
  const off = hay.length - needle.length
  for (let i = 0; i < needle.length; i++)
    if (hay[off + i] !== needle[i]) return false
  return true
}

/** Slice `bytes` minus leading/trailing ASCII whitespace (mirrors `host.rs::trim_bytes`). */
function trimBytes(
  bytes: Uint8Array,
  trimStart: boolean,
  trimEnd: boolean,
): Uint8Array {
  let start = 0
  let end = bytes.length
  if (trimStart) while (start < end && isAsciiWhitespace(bytes[start]!)) start++
  if (trimEnd) while (end > start && isAsciiWhitespace(bytes[end - 1]!)) end--
  return bytes.subarray(start, end)
}

/**
 * Pad `bytes` to BYTE length `target` with `pad` repeated (mirrors
 * `host.rs::pad_bytes`); the receiver is returned unchanged when already long
 * enough or `pad` is empty.
 */
function padBytes(
  bytes: Uint8Array,
  target: number,
  pad: Uint8Array,
  atStart: boolean,
): string {
  const need = Math.max(Math.max(target, 0) - bytes.length, 0)
  if (need === 0 || pad.length === 0) return utf8Decode(bytes)
  const filler = new Uint8Array(need)
  for (let i = 0; i < need; i++) filler[i] = pad[i % pad.length]!
  const out = new Uint8Array(bytes.length + need)
  if (atStart) {
    out.set(filler, 0)
    out.set(bytes, need)
  } else {
    out.set(bytes, 0)
    out.set(filler, bytes.length)
  }
  return utf8Decode(out)
}

/**
 * Resolve one `substring` endpoint against a BYTE `len` (mirrors
 * `host.rs::substring_index`): `undefined` uses `fallback`, otherwise the
 * argument is truncated and clamped to `0..=len` (no negative-from-end offset).
 */
function substringIndex(
  bits: bigint,
  mem: Uint8Array,
  fallback: number,
  len: number,
): number {
  const decoded = decode(bits, mem)
  if (decoded.kind === 'undefined') return fallback
  const n = truncToInt(valueToNumber(decoded))
  return Math.min(Math.max(n, 0), len)
}

/**
 * Split `hay` on each non-overlapping occurrence of the non-empty `sep`, keeping
 * empty pieces (mirrors `host.rs::split_on`).
 */
function splitOn(hay: Uint8Array, sep: Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = []
  let start = 0
  let i = 0
  while (i + sep.length <= hay.length) {
    let match = true
    for (let k = 0; k < sep.length; k++) {
      if (hay[i + k] !== sep[k]) {
        match = false
        break
      }
    }
    if (match) {
      out.push(hay.subarray(start, i))
      i += sep.length
      start = i
    } else {
      i++
    }
  }
  out.push(hay.subarray(start))
  return out
}
