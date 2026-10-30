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
/** An array hole (`UNDEFINED | 1`): an element slot with no element; reads as `undefined`. */
export const HOLE = 0x7ffb_0000_0000_0001n
/** NaN-boxed heap-pointer header (tag `0b101`): `HEAP | (ptr as u32)`. */
export const HEAP = 0x7ffd_0000_0000_0000n

/** Bytes of every heap block's leading header: the TYPE BYTE plus the `gc`/`flags`/`class` bytes. */
export const HEADER_SIZE = 4

/** TYPE BYTE of a heap string block: `[type=0][pad×3][len:u32][utf8]`. */
export const TYPE_STRING = 0
/** TYPE BYTE of an array handle (prefix, then `len:u32`, `elems:u32` → its element store). */
export const TYPE_ARRAY = 1
/** TYPE BYTE of an ordinary object block: the object-family prefix; properties live in its `props` table. */
export const TYPE_OBJECT = 2
/** TYPE BYTE of a function object (prefix, then `func_idx:u32`, reserved `u32`, `env:i64`). */
export const TYPE_FUNCTION = 3
/** TYPE BYTE of a `Map` handle block (prefix, then `size:u32`, `entries:i64`). */
export const TYPE_MAP = 6
/** TYPE BYTE of a `Set` handle block: the {@link TYPE_MAP} layout. */
export const TYPE_SET = 7
/** TYPE BYTE of a promise block (prefix, then `state:i32`, `value:i64`). */
export const TYPE_PROMISE = 8
/** TYPE BYTE of a generator handle block. Renders as `Object [Generator]`. */
export const TYPE_GENERATOR = 9
/** TYPE BYTE of a `Date` block (prefix, then `ms:f64`). */
export const TYPE_DATE = 10
/** TYPE BYTE of a Symbol primitive (`[type=14][gc][flags][class][reserved:u32][description:i64]`). */
export const TYPE_SYMBOL = 14
/** Byte offset (from a {@link TYPE_SYMBOL} block base) of the `description` jsval (a string or `undefined`). */
export const SYMBOL_DESCRIPTION_OFFSET = 8
/** Byte size of a {@link TYPE_SYMBOL} block. */
export const SYMBOL_BLOCK_SIZE = 16
/** {@link TYPE_SYMBOL} `flags` bit: the symbol is in the `Symbol.for` registry. */
export const SYMBOL_FLAG_REGISTERED = 4

/** Header byte 2: the per-kind `flags` byte (string: `INTERNED`, `ARRAY_INDEX`, `TWO_BYTE`; function: `LAZY`, `CONSTRUCTOR`, `CLASS_CTOR`). */
export const HEADER_FLAGS_OFFSET = 2
/** Header byte 3: the `TYPE_OBJECT` sub-`class` byte. */
export const HEADER_CLASS_OFFSET = 3
/** `TYPE_OBJECT` `class` byte of an Error instance (its kind is its prototype). */
export const CLASS_ERROR = 1
/** `TYPE_OBJECT` `class` byte of an (unmapped) `arguments` object. */
export const CLASS_ARGUMENTS = 2
/** `TYPE_OBJECT` `class` byte of a Boolean wrapper object. */
export const CLASS_BOOLEAN = 3
/** `TYPE_OBJECT` `class` byte of a Number wrapper object. */
export const CLASS_NUMBER = 4
/** `TYPE_OBJECT` `class` byte of a String wrapper object. */
export const CLASS_STRING = 5
/** `TYPE_OBJECT` `class` byte of a Symbol wrapper object. */
export const CLASS_SYMBOL = 7
/** `TYPE_OBJECT` `class` byte of a builtin iterator object (Array, String, Map or Set iterator). */
export const CLASS_ITERATOR = 8
/** Offset of a wrapper object's `[[PrimitiveValue]]` jsval. */
export const WRAPPER_VALUE_OFFSET = 16
/** String `flags` bit: the content is a canonical array index. */
export const STR_FLAG_ARRAY_INDEX = 2
/** String `flags` bit: the data is UTF-16LE code units (absent: Latin-1 bytes). */
export const STR_FLAG_TWO_BYTE = 4
/** Byte offset of a string block's `len:u32`, in UTF-16 code units (header | len @4 | data @8). */
export const STR_LEN_OFFSET = 4
/** Byte offset of a string block's data. */
export const STR_DATA_OFFSET = 8

/** Byte offset of the object-family prefix's `props:u32` slot (the `TYPE_PROPS` table, `0` = none). */
export const OBJ_PROPS_OFFSET = HEADER_SIZE
/** Byte size of the object-family prefix (header, `props:u32`, `proto:i64`); payloads start here. */
export const OBJ_PREFIX_SIZE = 16

/** Byte offset (from a property table base) of the `tflags` byte. */
export const PROPS_TFLAGS_OFFSET = 2
/** Byte offset (from a property table base) of the `count` of used slots (`u32`, tombstones included). */
export const PROPS_COUNT_OFFSET = 4
/** Byte size of a property table header; entry `k` starts at `PROPS_HEADER_SIZE + PROP_ENTRY_SIZE * k`. */
export const PROPS_HEADER_SIZE = 16
/** Byte size of one property table entry. */
export const PROP_ENTRY_SIZE = 16
/** Byte offset (within an entry) of the interned key string base (`u32`, `0` = tombstone). */
export const PROP_KEY_OFFSET = 0
/** Byte offset (within an entry) of the `attrs` (`u32`). */
export const PROP_ATTRS_OFFSET = 4
/** Byte offset (within an entry) of the value (`i64`; an accessor pair pointer when `ATTR_ACCESSOR`). */
export const PROP_VALUE_OFFSET = 8
/** Property table `tflags` bit: some key is a canonical array index (list integer keys first). */
export const PROPS_HAS_INDEX_KEYS = 1
/** Property table `tflags` bit: some key is a {@link TYPE_SYMBOL} block. */
export const PROPS_HAS_SYMBOL_KEYS = 8
/** Property `attrs` bit: the key is listed by enumeration and rendering. */
export const ATTR_ENUMERABLE = 2
/** Property `attrs` bit: the value is a getter/setter pair. */
export const ATTR_ACCESSOR = 8
/** Byte offset (from an accessor pair block base) of the getter jsval. */
export const ACCESSOR_GET_OFFSET = HEADER_SIZE + 4
/** Byte offset (from an accessor pair block base) of the setter jsval. */
export const ACCESSOR_SET_OFFSET = HEADER_SIZE + 12

/** {@link TYPE_PROMISE} `state`: the promise is unsettled. */
export const PROMISE_PENDING = 0
/** {@link TYPE_PROMISE} `state`: the promise fulfilled with its `value`. */
export const PROMISE_FULFILLED = 1
/** {@link TYPE_PROMISE} `state`: the promise rejected with its `value` as the reason. */
export const PROMISE_REJECTED = 2

/** Byte offset (from a {@link TYPE_MAP}/{@link TYPE_SET} block base) of the `size` entry count (`u32`). */
export const MAP_SIZE_OFFSET = OBJ_PREFIX_SIZE
/** Byte offset (from a {@link TYPE_PROMISE} block base) of the `state` (`i32`). */
export const PROMISE_STATE_OFFSET = OBJ_PREFIX_SIZE
/** Byte offset (from a {@link TYPE_PROMISE} block base) of the settled `value` (`i64`). */
export const PROMISE_VALUE_OFFSET = OBJ_PREFIX_SIZE + 8
/** Byte size of a {@link TYPE_PROMISE} block. */
export const PROMISE_BLOCK_SIZE = OBJ_PREFIX_SIZE + 16
/** Byte offset (from a {@link TYPE_GENERATOR} block base) of the `done` flag (`i32`). */
export const GEN_DONE_OFFSET = OBJ_PREFIX_SIZE + 8
/** Byte offset (from a {@link TYPE_DATE} block base) of the `ms` timestamp (`f64`). */
export const DATE_MS_OFFSET = OBJ_PREFIX_SIZE
/** Byte offset of the object-family prefix's `proto:i64` jsval slot. */
export const OBJ_PROTO_OFFSET = 8
/** Byte offset (from an array handle base) of the `length` (`u32`). */
export const ARRAY_LEN_OFFSET = OBJ_PREFIX_SIZE
/** Byte offset (from an array handle base) of its element store base (`u32`, `0` when the capacity is 0). */
export const ARRAY_ELEMS_OFFSET = OBJ_PREFIX_SIZE + 4
/** Array `flags` bit: sparse — elements at or past the store's `cap` are array-index keys in `props`. */
export const ARRAY_FLAG_SPARSE = 4
/** Byte offset (from an element store base) of its capacity (`u32`). */
export const ELEMS_CAP_OFFSET = 4
/** Byte offset (from an element store base) of element 0; element `i` is at `+ 8 * i`. */
export const ELEMS_DATA_OFFSET = 8
/** Byte offset (from a {@link TYPE_FUNCTION} block base) of the funcref-table index (`u32`). */
export const FUNC_INDEX_OFFSET = OBJ_PREFIX_SIZE
/** {@link TYPE_FUNCTION} `flags` bit: the function is a constructor (`new` works). */
export const FUNC_FLAG_CONSTRUCTOR = 8
/** {@link TYPE_FUNCTION} `flags` bit: the function is a class constructor. */
export const FUNC_FLAG_CLASS_CTOR = 16
/** {@link TYPE_FUNCTION} `flags` bit: the constructor allocates its own `this` (a derived class). */
export const FUNC_FLAG_SELF_ALLOC = 32
/** {@link TYPE_FUNCTION} `flags` bit: a bound function (own `length`/`name`, `env` = `[target, boundThis, ...args]`). */
export const FUNC_FLAG_BOUND = 64
/** Address of the `u32` base of the `__fn_info` table (`[name key:u32][length:u32]` per function). */
export const FN_INFO_ADDR = 4
/** Byte size of one `__fn_info` entry. */
export const FN_INFO_ENTRY_SIZE = 8
/** Byte offset (within a `__fn_info` entry) of the interned `name` key (`u32`). */
export const FN_INFO_NAME_OFFSET = 0

/** Export name of the module's linear memory. */
export const MEMORY_EXPORT_NAME = 'memory'
/** Export name of the bump-allocator heap-pointer global (read for the OOM sentinel). */
export const HEAP_PTR_EXPORT_NAME = '__heap_ptr'
/** The linear-memory cap in 64 KiB wasm pages (512 MiB). */
export const MEMORY_MAX_PAGES = 8192
/** The sticky out-of-memory sentinel the guest stores in `__heap_ptr` (`i32`). */
export const HEAP_PTR_OOM = -1
/**
 * Export name of the `__new_string(len, twoByte) -> base` guest string constructor:
 * `len` code units, Latin-1 (`twoByte` 0) or UTF-16LE (`twoByte` 1, sets
 * `STR_FLAG_TWO_BYTE`); the caller fills the data at `base + STR_DATA_OFFSET`.
 */
export const NEW_STRING_EXPORT_NAME = '__new_string'
/** Export name of the `__new_array(len) -> base` guest array constructor. */
export const NEW_ARRAY_EXPORT_NAME = '__new_array'
/** Export name of the `__new_promise() -> base` guest pending-promise constructor. */
export const NEW_PROMISE_EXPORT_NAME = '__new_promise'
/**
 * Export name of the `__promise_self_error() -> jsval` guest constructor of the
 * `TypeError` a promise resolved with itself is rejected with.
 */
export const PROMISE_SELF_ERROR_EXPORT_NAME = '__promise_self_error'
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

/** `__method` op: array `indexOf`. */
export const METHOD_OP_INDEX_OF = 0
/** `__method` op: array `includes`. */
export const METHOD_OP_INCLUDES = 1
/** `__method` op: `toUpperCase`. */
export const METHOD_OP_TO_UPPER = 4
/** `__method` op: `toLowerCase`. */
export const METHOD_OP_TO_LOWER = 5
/** `__method` op: `Number.prototype.toString(radix)` (number receiver). */
export const METHOD_OP_NUMBER_TO_STRING = 17
/** `__method` op: `String.prototype.localeCompare` (string, code-unit order). */
export const METHOD_OP_LOCALE_COMPARE = 22
/** `__method` op: `String.prototype.normalize` (string). */
export const METHOD_OP_NORMALIZE = 23
/** `__method` result: an out-of-range argument (unknown `normalize` form). */
export const METHOD_RANGE_ERROR = 0x7ffb_0000_0000_0002n
/** `__method` op: `Number.prototype.toFixed(f)` (finite number receiver). */
export const METHOD_OP_NUMBER_TO_FIXED = 40
/** `__method` op: `Number.prototype.toExponential(f)` (finite number receiver). */
export const METHOD_OP_NUMBER_TO_EXPONENTIAL = 41
/** `__method` op: `Number.prototype.toPrecision(p)` (finite number receiver). */
export const METHOD_OP_NUMBER_TO_PRECISION = 42
/** `__method` op: a host `Math` function (`arg1` the raw `MATH_FN_*` code). */
export const METHOD_OP_MATH = 43

/** `METHOD_OP_MATH` function code: `Math.cbrt`. */
export const MATH_FN_CBRT = 0
/** `METHOD_OP_MATH` function code: `Math.log`. */
export const MATH_FN_LOG = 1
/** `METHOD_OP_MATH` function code: `Math.log2`. */
export const MATH_FN_LOG2 = 2
/** `METHOD_OP_MATH` function code: `Math.log10`. */
export const MATH_FN_LOG10 = 3
/** `METHOD_OP_MATH` function code: `Math.log1p`. */
export const MATH_FN_LOG1P = 4
/** `METHOD_OP_MATH` function code: `Math.exp`. */
export const MATH_FN_EXP = 5
/** `METHOD_OP_MATH` function code: `Math.expm1`. */
export const MATH_FN_EXPM1 = 6
/** `METHOD_OP_MATH` function code: `Math.sin`. */
export const MATH_FN_SIN = 7
/** `METHOD_OP_MATH` function code: `Math.cos`. */
export const MATH_FN_COS = 8
/** `METHOD_OP_MATH` function code: `Math.tan`. */
export const MATH_FN_TAN = 9
/** `METHOD_OP_MATH` function code: `Math.asin`. */
export const MATH_FN_ASIN = 10
/** `METHOD_OP_MATH` function code: `Math.acos`. */
export const MATH_FN_ACOS = 11
/** `METHOD_OP_MATH` function code: `Math.atan`. */
export const MATH_FN_ATAN = 12
/** `METHOD_OP_MATH` function code: `Math.atan2`. */
export const MATH_FN_ATAN2 = 13
/** `METHOD_OP_MATH` function code: `Math.sinh`. */
export const MATH_FN_SINH = 14
/** `METHOD_OP_MATH` function code: `Math.cosh`. */
export const MATH_FN_COSH = 15
/** `METHOD_OP_MATH` function code: `Math.tanh`. */
export const MATH_FN_TANH = 16
/** `METHOD_OP_MATH` function code: `Math.asinh`. */
export const MATH_FN_ASINH = 17
/** `METHOD_OP_MATH` function code: `Math.acosh`. */
export const MATH_FN_ACOSH = 18
/** `METHOD_OP_MATH` function code: `Math.atanh`. */
export const MATH_FN_ATANH = 19
/** `METHOD_OP_MATH` function code: `Math.random`. */
export const MATH_FN_RANDOM = 20

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
