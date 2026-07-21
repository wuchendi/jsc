/**
 * The runtime ABI, ported verbatim from `crates/jsc-core/src/abi.rs` — the
 * single source of truth on the Rust side. This TS copy DUPLICATES those
 * numbers so the browser shim can decode emitted-module values and back the
 * host imports without a Rust runtime. ACCEPTED TEMPORARY DEBT (see README):
 * a drift here silently mis-decodes; keep it in lock-step with `abi.rs`.
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
/** `typeof`-result type-string immediate header (tag `0b100`); low bits index the table. */
export const TYPEOF = 0x7ffc_0000_0000_0000n
/** NaN-boxed heap-pointer header (tag `0b101`): `HEAP | (ptr as u32)`. */
export const HEAP = 0x7ffd_0000_0000_0000n

/** Bytes of every heap block's leading header: the TYPE BYTE plus 3 padding bytes. */
export const HEADER_SIZE = 4

/** TYPE BYTE of a heap string block: `[type=0][pad×3][len:u32][utf8]`. */
export const TYPE_STRING = 0
/** TYPE BYTE of a heap array block: `[type=1][pad×3][len:u32][elem:i64 × len]`. */
export const TYPE_ARRAY = 1
/** TYPE BYTE of a heap object block: `[type=2][pad×3][n:u32][(key:i64, val:i64) × n]`. */
export const TYPE_OBJECT = 2
/** TYPE BYTE of a first-class function value. Renders as `[Function (anonymous)]`. */
export const TYPE_FUNCTION = 3

/** Export name of the module's linear memory. */
export const MEMORY_EXPORT_NAME = 'memory'
/** Export name of the bump-allocator heap-pointer global (host allocation). */
export const HEAP_PTR_EXPORT_NAME = '__heap_ptr'
/** Export name of the pending-exception flag global (`i32`, S15.1). */
export const EXC_PENDING_EXPORT_NAME = '__exc_pending'
/** Export name of the in-flight exception value global (`i64`, S15.1). */
export const EXC_VALUE_EXPORT_NAME = '__exc_value'

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
/** `__convert` op: parse a leading integer (`parseInt(x)`). */
export const CONVERT_OP_PARSE_INT = 2
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

/** The `typeof` result strings, indexed by a boxed type-string immediate. */
export const TYPEOF_STRINGS = [
  'number',
  'boolean',
  'undefined',
  'object',
  'string',
  'function',
] as const

/** 64-bit unsigned mask, for normalizing signed `bigint`s that cross the wasm boundary. */
export const U64_MASK = 0xffff_ffff_ffff_ffffn
/** 32-bit unsigned mask, for extracting the low 32 bits (heap offset / typeof index). */
export const U32_MASK = 0xffff_ffffn
