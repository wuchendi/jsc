/**
 * The runtime ABI, ported verbatim from `crates/jsc-core/src/abi.rs` — the
 * single source of truth on the Rust side. This TS copy DUPLICATES those
 * numbers so the browser shim can decode emitted-module values and back the
 * host imports without a Rust runtime (see README, "Host parity"): a drift
 * here silently mis-decodes; keep it in lock-step with `abi.rs`.
 *
 * A jsval is a 64-bit carrier (`bigint` on the JS side). Any value without the
 * full {@link QNAN} mask set is an ordinary IEEE-754 `f64`; with the mask set,
 * the 3-bit tag in payload bits `[50:48]` discriminates the boxed immediates
 * and the single heap pointer.
 */

/** Quiet-NaN mask: a value is a boxed immediate (or number NaN) iff `bits & QNAN === QNAN`. */
export const QNAN = 0x7ff8_0000_0000_0000n
/** NaN-boxed `false` (tag `0b001`, value bit clear). */
export const FALSE = 0x7ff9_0000_0000_0000n
/** NaN-boxed `true` (tag `0b001`, value bit set). */
export const TRUE = 0x7ff9_0000_0000_0001n
/** NaN-boxed `null` (tag `0b010`). */
export const NULL = 0x7ffa_0000_0000_0000n
/** NaN-boxed `undefined` (tag `0b011`). */
export const UNDEFINED = 0x7ffb_0000_0000_0000n
/** NaN-boxed heap-pointer header (tag `0b101`): `HEAP | (ptr as u32)`. */
export const HEAP = 0x7ffd_0000_0000_0000n

/** Bytes of every heap block's leading header: the TYPE BYTE plus the zero `gc`/`flags`/`class` bytes. */
export const HEADER_SIZE = 4

/** TYPE BYTE of a heap string block: `[type=0][pad×3][len:u32][utf8]`. */
export const TYPE_STRING = 0
/** TYPE BYTE of a heap array block: `[type=1][pad×3][len:u32][elem:i64 × len]`. */
export const TYPE_ARRAY = 1
/** TYPE BYTE of a heap object block: `[type=2][pad×3][n:u32][(key:i64, val:i64) × n]`. */
export const TYPE_OBJECT = 2
/** TYPE BYTE of a first-class function value. Renders as `[Function (anonymous)]`. */
export const TYPE_FUNCTION = 3
/** TYPE BYTE of a promise block: `[type=8][pad×3][state:i32][value:i64]`. */
export const TYPE_PROMISE = 8
/** TYPE BYTE of a `Date` block: `[type=10][pad×3][pad:u32][ms:f64]`. */
export const TYPE_DATE = 10

/** {@link TYPE_PROMISE} `state`: the promise is unsettled. */
export const PROMISE_PENDING = 0
/** {@link TYPE_PROMISE} `state`: the promise fulfilled with its `value`. */
export const PROMISE_FULFILLED = 1
/** {@link TYPE_PROMISE} `state`: the promise rejected with its `value` as the reason. */
export const PROMISE_REJECTED = 2

/** Byte offset (from a generator block base) of the `done` flag (`i32`). */
export const GEN_DONE_OFFSET = HEADER_SIZE + 8
/** Byte offset (from a {@link TYPE_DATE} block base) of the `ms` timestamp (`f64`). */
export const DATE_MS_OFFSET = HEADER_SIZE + 4

/** Export name of the module's linear memory. */
export const MEMORY_EXPORT_NAME = 'memory'
/** Export name of the bump-allocator heap-pointer global (read for the OOM sentinel). */
export const HEAP_PTR_EXPORT_NAME = '__heap_ptr'
/** The linear-memory cap in 64 KiB wasm pages (512 MiB). */
export const MEMORY_MAX_PAGES = 8192
/** The sticky out-of-memory sentinel the guest stores in `__heap_ptr` (`i32`). */
export const HEAP_PTR_OOM = -1
/** Export name of the `__new_string(len) -> base` guest string constructor. */
export const NEW_STRING_EXPORT_NAME = '__new_string'
/** Export name of the `__new_array(len) -> base` guest array constructor. */
export const NEW_ARRAY_EXPORT_NAME = '__new_array'
/** Export name of the `__new_promise() -> base` guest pending-promise constructor. */
export const NEW_PROMISE_EXPORT_NAME = '__new_promise'
/** Export name of the pending-exception flag global (`i32`). */
export const EXC_PENDING_EXPORT_NAME = '__exc_pending'
/** Export name of the in-flight exception value global (`i64`). */
export const EXC_VALUE_EXPORT_NAME = '__exc_value'
/** Export name of the `__invoke1(callback, arg)` callback trampoline (promise reactions). */
export const INVOKE_EXPORT_NAME = '__invoke1'
/** Export name of the `__async_step(gen, sent)` async-body step trampoline. */
export const ASYNC_STEP_EXPORT_NAME = '__async_step'

/** `print` flag: format the jsval and follow it with a single space. */
export const PRINT_FLAG_SEP = 0
/** `print` flag: format the jsval and follow it with a newline. */
export const PRINT_FLAG_END = 1
/** `print` flag: emit only a newline (zero-argument `console.log()`). */
export const PRINT_FLAG_NEWLINE = 2

/** `__convert` op: stringify the argument (`String(x)`). */
export const CONVERT_OP_TO_STRING = 0
/** `__convert` op: coerce the argument to a number (`Number(x)`). */
export const CONVERT_OP_TO_NUMBER = 1
/** `__convert` op: parse a leading float (`parseFloat(x)`). */
export const CONVERT_OP_PARSE_FLOAT = 3

/** `__method` op: `indexOf` (overloaded on array/string). */
export const METHOD_OP_INDEX_OF = 0
/** `__method` op: `includes`. */
export const METHOD_OP_INCLUDES = 1
/** `__method` op: `join`. */
export const METHOD_OP_JOIN = 2
/** `__method` op: `slice` (overloaded on array/string). */
export const METHOD_OP_SLICE = 3
/** `__method` op: `toUpperCase`. */
export const METHOD_OP_TO_UPPER = 4
/** `__method` op: `toLowerCase`. */
export const METHOD_OP_TO_LOWER = 5
/** `__method` op: `charAt`. */
export const METHOD_OP_CHAR_AT = 6
/** `__method` op: `trim` (string). */
export const METHOD_OP_TRIM = 7
/** `__method` op: `trimStart` (string). */
export const METHOD_OP_TRIM_START = 8
/** `__method` op: `trimEnd` (string). */
export const METHOD_OP_TRIM_END = 9
/** `__method` op: `repeat` (string). */
export const METHOD_OP_REPEAT = 10
/** `__method` op: `startsWith` (string). */
export const METHOD_OP_STARTS_WITH = 11
/** `__method` op: `endsWith` (string). */
export const METHOD_OP_ENDS_WITH = 12
/** `__method` op: `padStart` (string). */
export const METHOD_OP_PAD_START = 13
/** `__method` op: `padEnd` (string). */
export const METHOD_OP_PAD_END = 14
/** `__method` op: `substring` (string). */
export const METHOD_OP_SUBSTRING = 15
/** `__method` op: `split` (string, yields an array). */
export const METHOD_OP_SPLIT = 16

/** `__promise` op: `Promise.resolve(a)`. */
export const PROMISE_OP_RESOLVE = 0
/** `__promise` op: `Promise.reject(a)`. */
export const PROMISE_OP_REJECT = 1
/** `__promise` op: top-level `await a`. */
export const PROMISE_OP_AWAIT = 2
/** `__promise` op: `a.finally(b)`. */
export const PROMISE_OP_FINALLY = 3
/** `__promise` op: allocate a pending promise (`new Promise`). */
export const PROMISE_OP_NEW = 4
/** `__promise` op: fulfill promise `a` with `b` (a `resolve` settler call). */
export const PROMISE_OP_SETTLE_FULFILL = 5
/** `__promise` op: reject promise `a` with `b` (a `reject` settler call). */
export const PROMISE_OP_SETTLE_REJECT = 6
/** `__promise` op: spawn an `async function` body from generator handle `a`. */
export const PROMISE_OP_ASYNC_SPAWN = 7

/** 64-bit unsigned mask, for normalizing signed `bigint`s that cross the wasm boundary. */
export const U64_MASK = 0xffff_ffff_ffff_ffffn
/** 32-bit unsigned mask, for extracting the low 32 bits (heap offset). */
export const U32_MASK = 0xffff_ffffn
