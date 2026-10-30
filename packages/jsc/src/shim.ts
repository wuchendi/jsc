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
  HEAP,
  HEAP_PTR_EXPORT_NAME,
  HEAP_PTR_OOM,
  INVOKE_EXPORT_NAME,
  MATH_FN_ACOS,
  MATH_FN_ACOSH,
  MATH_FN_ASIN,
  MATH_FN_ASINH,
  MATH_FN_ATAN,
  MATH_FN_ATAN2,
  MATH_FN_ATANH,
  MATH_FN_CBRT,
  MATH_FN_COS,
  MATH_FN_COSH,
  MATH_FN_EXP,
  MATH_FN_EXPM1,
  MATH_FN_LOG,
  MATH_FN_LOG1P,
  MATH_FN_LOG2,
  MATH_FN_LOG10,
  MATH_FN_RANDOM,
  MATH_FN_SIN,
  MATH_FN_SINH,
  MATH_FN_TAN,
  MATH_FN_TANH,
  MEMORY_EXPORT_NAME,
  MEMORY_MAX_PAGES,
  METHOD_OP_INCLUDES,
  METHOD_OP_INDEX_OF,
  METHOD_OP_LOCALE_COMPARE,
  METHOD_OP_MATH,
  METHOD_OP_NORMALIZE,
  METHOD_OP_NUMBER_TO_EXPONENTIAL,
  METHOD_OP_NUMBER_TO_FIXED,
  METHOD_OP_NUMBER_TO_PRECISION,
  METHOD_OP_NUMBER_TO_STRING,
  METHOD_OP_TO_LOWER,
  METHOD_OP_TO_UPPER,
  METHOD_RANGE_ERROR,
  METHOD_UNSUPPORTED,
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
  PROMISE_SELF_ERROR_EXPORT_NAME,
  PROMISE_STATE_OFFSET,
  PROMISE_VALUE_OFFSET,
  QNAN,
  STR_DATA_OFFSET,
  TRUE,
  TYPE_ARRAY,
  TYPE_FUNCTION,
  TYPE_PROMISE,
  TYPE_STRING,
  U32_MASK,
  UNDEFINED,
} from './abi.js'
import {
  arrayElements,
  asU64,
  decode,
  f64ToBits,
  formatValue,
  parseFloatStr,
  parseIntStr,
  stringText,
  toJsString,
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
   * Allocate a heap STRING block holding the code units of `text` in canonical
   * form (one-byte Latin-1 when every unit is `<= 0xFF`, else UTF-16LE)
   * through `__new_string(len, twoByte)`, filling only the payload (mirrors
   * `host.rs::alloc_units`).
   */
  function allocString(text: string): bigint | null {
    const len = Math.min(text.length, MEMORY_MAX_BYTES)
    let twoByte = false
    for (let i = 0; i < len && !twoByte; i++)
      twoByte = text.charCodeAt(i) > 0xff
    const base = guestNew(NEW_STRING_EXPORT_NAME, len, twoByte ? 1 : 0)
    if (base === null) return null
    const view = memView()
    const at = base + STR_DATA_OFFSET
    if (at + (twoByte ? len * 2 : len) > view.length) return null
    // `at` is 8-aligned, so a `Uint16Array` (little-endian, like wasm) fits.
    const units = twoByte
      ? new Uint16Array(view.buffer, view.byteOffset + at, len)
      : view.subarray(at, at + len)
    for (let i = 0; i < len; i++) units[i] = text.charCodeAt(i)
    return BigInt(base)
  }

  /**
   * Dispatch a `__method` call (mirrors `host.rs::method_dispatch`): snapshot
   * memory, read the receiver's TYPE BYTE, and perform the array/string method,
   * returning the result jsval bits (or `null` to yield `undefined`).
   */
  /** The host `Math` function `code` (a `MATH_FN_*`) of `x` (and `y` for `atan2`). */
  function mathFn(code: number, x: number, y: number): number {
    switch (code) {
      case MATH_FN_CBRT:
        return Math.cbrt(x)
      case MATH_FN_LOG:
        return Math.log(x)
      case MATH_FN_LOG2:
        return Math.log2(x)
      case MATH_FN_LOG10:
        return Math.log10(x)
      case MATH_FN_LOG1P:
        return Math.log1p(x)
      case MATH_FN_EXP:
        return Math.exp(x)
      case MATH_FN_EXPM1:
        return Math.expm1(x)
      case MATH_FN_SIN:
        return Math.sin(x)
      case MATH_FN_COS:
        return Math.cos(x)
      case MATH_FN_TAN:
        return Math.tan(x)
      case MATH_FN_ASIN:
        return Math.asin(x)
      case MATH_FN_ACOS:
        return Math.acos(x)
      case MATH_FN_ATAN:
        return Math.atan(x)
      case MATH_FN_ATAN2:
        return Math.atan2(x, y)
      case MATH_FN_SINH:
        return Math.sinh(x)
      case MATH_FN_COSH:
        return Math.cosh(x)
      case MATH_FN_TANH:
        return Math.tanh(x)
      case MATH_FN_ASINH:
        return Math.asinh(x)
      case MATH_FN_ACOSH:
        return Math.acosh(x)
      case MATH_FN_ATANH:
        return Math.atanh(x)
      case MATH_FN_RANDOM:
        return Math.random()
      default:
        return Number.NaN
    }
  }

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
        return null
      case METHOD_OP_INCLUDES:
        if (ptr !== null && typeByte === TYPE_ARRAY)
          return arrayIncludes(mem, ptr, arg0) ? TRUE : FALSE
        return null
      case METHOD_OP_TO_UPPER:
      case METHOD_OP_TO_LOWER: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const text = stringText(mem, ptr)
        if (text === null) return null
        // The full Unicode default case mappings (`ß` -> `SS`, final sigma).
        const base = allocString(
          op === METHOD_OP_TO_UPPER ? text.toUpperCase() : text.toLowerCase(),
        )
        return base === null ? null : HEAP | base
      }
      case METHOD_OP_LOCALE_COMPARE: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const self = stringText(mem, ptr)
        if (self === null) return null
        const that = toJsString(decode(arg0, mem))
        // Code-unit order.
        return f64ToBits(self < that ? -1 : self > that ? 1 : 0)
      }
      case METHOD_OP_NORMALIZE: {
        if (ptr === null || typeByte !== TYPE_STRING) return null
        const text = stringText(mem, ptr)
        if (text === null) return null
        const formValue = decode(arg0, mem)
        const form =
          formValue.kind === 'undefined' ? 'NFC' : toJsString(formValue)
        if (!['NFC', 'NFD', 'NFKC', 'NFKD'].includes(form))
          return METHOD_RANGE_ERROR
        // Every form maps an ASCII string to itself; Unicode normalization is
        // not available to the native host yet.
        return isAscii(text) ? recv : METHOD_UNSUPPORTED
      }
      case METHOD_OP_NUMBER_TO_STRING: {
        const value = decode(recv, mem)
        if (value.kind !== 'number') return null
        const radix = Math.trunc(valueToNumber(decode(arg0, mem)))
        const base = allocString(value.value.toString(radix))
        return base === null ? null : HEAP | base
      }
      case METHOD_OP_NUMBER_TO_FIXED:
      case METHOD_OP_NUMBER_TO_EXPONENTIAL:
      case METHOD_OP_NUMBER_TO_PRECISION: {
        const value = decode(recv, mem)
        if (value.kind !== 'number') return null
        const digits = decode(arg0, mem)
        const d =
          digits.kind === 'number' ? Math.trunc(digits.value) : undefined
        if (d === undefined && op !== METHOD_OP_NUMBER_TO_EXPONENTIAL)
          return null
        const text =
          op === METHOD_OP_NUMBER_TO_FIXED
            ? value.value.toFixed(d)
            : op === METHOD_OP_NUMBER_TO_PRECISION
              ? value.value.toPrecision(d)
              : value.value.toExponential(d)
        const base = allocString(text)
        return base === null ? null : HEAP | base
      }
      case METHOD_OP_MATH: {
        const number = (bits: bigint): number => {
          const value = decode(bits, mem)
          return value.kind === 'number' ? value.value : Number.NaN
        }
        const n = mathFn(Number(arg1), number(recv), number(arg0))
        return Number.isNaN(n) ? QNAN : f64ToBits(n)
      }
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
   * Settle promise `base` once; fulfilling with another promise ADOPTS it, and
   * fulfilling with itself rejects with the guest's `__promise_self_error`
   * `TypeError`. On a real settlement its reactions move into the queue
   * (mirrors `promise.rs::settle`).
   */
  function settle(base: number, state: number, value: bigint): void {
    if (readState(memView(), base) !== PROMISE_PENDING) return
    if (state === PROMISE_FULFILLED) {
      const inner = asPromise(memView(), value)
      if (inner === base) {
        const selfError = instance?.exports[PROMISE_SELF_ERROR_EXPORT_NAME] as
          | (() => bigint)
          | undefined
        if (!selfError) return
        try {
          value = asU64(selfError())
        } catch {
          return // an out-of-memory trap: the run stage reports it.
        }
        state = PROMISE_REJECTED
      } else if (inner !== null) {
        registerSource(value, UNDEFINED, UNDEFINED, base, false, null)
        return
      }
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
      let sent = job.value
      if (job.state !== PROMISE_FULFILLED) {
        // Resume with the reason pending so the `await` throws into the body.
        const pending = exportGlobal(EXC_PENDING_EXPORT_NAME)
        const excValue = exportGlobal(EXC_VALUE_EXPORT_NAME)
        if (excValue) excValue.value = BigInt.asIntN(64, job.value)
        if (pending) pending.value = 1
        sent = UNDEFINED
      }
      driveAsync(job.asyncGen, job.result, sent)
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

  /**
   * `__convert` on primitives: the guest runs `ToPrimitive` first, so the
   * only objects it sees are a Date (`Date.prototype.toString`) and a
   * function without a user `toString` (mirrors `harness.rs`).
   */
  const convert = (bits: bigint, op: number): bigint => {
    const u = asU64(bits)
    const value = decode(u, memView())
    switch (op) {
      case CONVERT_OP_TO_NUMBER:
        return f64ToBits(valueToNumber(value))
      case CONVERT_OP_PARSE_FLOAT:
        return f64ToBits(parseFloatStr(toJsString(value)))
      case CONVERT_OP_TO_STRING: {
        // A string argument is returned unchanged (no allocation).
        if (value.kind === 'string') return u
        const base = allocString(toJsString(value))
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

/** Number jsval bits for a non-negative sequence index (lossless `u32 -> f64`). */
function indexBits(i: number): bigint {
  return f64ToBits(i >>> 0)
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

/**
 * `array.includes(x)` (mirrors `host.rs::array_includes`): some element is
 * `SameValueZero` to `x` — `===`, except that `NaN` matches `NaN`; a hole
 * reads as `undefined`.
 */
function arrayIncludes(mem: Uint8Array, ptr: number, needle: bigint): boolean {
  const [len, present] = arrayElements(mem, ptr) ?? [0, []]
  if (needle === UNDEFINED && present.length < len) return true
  const n = decode(needle, mem)
  for (const [, elem] of present) {
    if (strictEqBits(elem, needle, mem)) return true
    const e = decode(elem, mem)
    if (
      e.kind === 'number' &&
      n.kind === 'number' &&
      Number.isNaN(e.value) &&
      Number.isNaN(n.value)
    )
      return true
  }
  return false
}

/** `array.indexOf(x)`: first index whose element `=== x`, or `-1` (a hole never matches). */
function arrayIndexOf(mem: Uint8Array, ptr: number, needle: bigint): bigint {
  const [, present] = arrayElements(mem, ptr) ?? [0, []]
  for (const [i, elem] of present) {
    if (strictEqBits(elem, needle, mem)) return indexBits(i)
  }
  return f64ToBits(-1)
}

/** Whether every code unit of `text` is ASCII. */
function isAscii(text: string): boolean {
  for (let i = 0; i < text.length; i++)
    if (text.charCodeAt(i) > 0x7f) return false
  return true
}
