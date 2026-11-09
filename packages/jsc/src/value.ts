/**
 * A TypeScript port of the parts of `crates/jsc-core/src/run/value.rs` (NaN-box
 * decoder, `ToString` text) and `run/host.rs` (numeric coercions) the `env`
 * shim still needs (see README, "Host parity").
 */

import {
  ARRAY_ELEMS_OFFSET,
  ARRAY_FLAG_SPARSE,
  ARRAY_LEN_OFFSET,
  ATTR_ACCESSOR,
  ATTR_ENUMERABLE,
  CLASS_ARGUMENTS,
  CLASS_BOOLEAN,
  CLASS_ERROR,
  CLASS_STRING,
  CLASS_SYMBOL,
  DATE_MS_OFFSET,
  ELEMS_CAP_OFFSET,
  ELEMS_DATA_OFFSET,
  FN_INFO_ADDR,
  FN_INFO_ENTRY_SIZE,
  FN_INFO_NAME_OFFSET,
  FUNC_FLAG_BOUND,
  FUNC_FLAG_CLASS_CTOR,
  FUNC_FLAG_CONSTRUCTOR,
  FUNC_INDEX_OFFSET,
  HEADER_CLASS_OFFSET,
  HEADER_FLAGS_OFFSET,
  HEAP,
  HOLE,
  OBJ_PROPS_OFFSET,
  OBJ_PROTO_OFFSET,
  PROP_ATTRS_OFFSET,
  PROP_ENTRY_SIZE,
  PROP_KEY_OFFSET,
  PROP_VALUE_OFFSET,
  PROPS_COUNT_OFFSET,
  PROPS_HAS_INDEX_KEYS,
  PROPS_HEADER_SIZE,
  PROPS_TFLAGS_OFFSET,
  QNAN,
  STR_DATA_OFFSET,
  STR_FLAG_ARRAY_INDEX,
  STR_FLAG_TWO_BYTE,
  STR_LEN_OFFSET,
  SYMBOL_DESCRIPTION_OFFSET,
  TYPE_ARRAY,
  TYPE_DATE,
  TYPE_FUNCTION,
  TYPE_GENERATOR,
  TYPE_MAP,
  TYPE_OBJECT,
  TYPE_PROMISE,
  TYPE_SET,
  TYPE_STRING,
  TYPE_SYMBOL,
  U32_MASK,
  U64_MASK,
  WRAPPER_VALUE_OFFSET,
} from './abi.js'

/** A decoded jsval, carrying what `ToString` and `ToNumber` read. */
type Value =
  | { kind: 'number'; value: number }
  | { kind: 'bool'; value: boolean }
  | { kind: 'null' }
  | { kind: 'undefined' }
  | { kind: 'string'; value: string }
  | { kind: 'symbol'; description: string | undefined }
  | { kind: 'array'; items: Value[] }
  | { kind: 'holes'; count: number }
  | { kind: 'circular' }
  | { kind: 'object'; tag: string }
  | { kind: 'wrapper'; primitive: Value }
  | { kind: 'error'; name: string; message: string }
  | {
      kind: 'function'
      name: string
      isClass: boolean
      extends: string | undefined
    }
  | { kind: 'date'; ms: number }

// A single scratch buffer for f64 <-> u64 bit reinterpretation, reused so we do
// not allocate a DataView per decode.
const scratch = new DataView(new ArrayBuffer(8))

/** Reinterpret raw `u64` bits as an IEEE-754 `f64` (mirrors `f64::from_bits`). */
function bitsToF64(bits: bigint): number {
  scratch.setBigUint64(0, bits & U64_MASK, true)
  return scratch.getFloat64(0, true)
}

/** Reinterpret an `f64` as raw `u64` bits (mirrors `f64::to_bits`). */
export function f64ToBits(value: number): bigint {
  scratch.setFloat64(0, value, true)
  return scratch.getBigUint64(0, true)
}

/** Normalize a `bigint` that crossed the wasm `i64` boundary to unsigned 64-bit. */
export function asU64(bits: bigint): bigint {
  return bits & U64_MASK
}

/** Read a little-endian `u32` at `off`, or `undefined` when out of range. */
function readU32(mem: Uint8Array, off: number): number | undefined {
  if (off < 0 || off + 4 > mem.length) return undefined
  return (
    (mem[off]! |
      (mem[off + 1]! << 8) |
      (mem[off + 2]! << 16) |
      (mem[off + 3]! << 24)) >>>
    0
  )
}

/** Read a little-endian `u64` (as unsigned `bigint`) at `off`, or `undefined`. */
function readU64(mem: Uint8Array, off: number): bigint | undefined {
  if (off < 0 || off + 8 > mem.length) return undefined
  let value = 0n
  for (let i = 7; i >= 0; i--) value = (value << 8n) | BigInt(mem[off + i]!)
  return value
}

/**
 * Decode raw jsval `bits` into a {@link Value} per the NaN-box layout.
 *
 * `mem` is the module's linear-memory view; it is consulted only for heap
 * pointer jsvals (tag `0b101`). Pass an empty array for immediate values.
 * Mirrors `value.rs::decode`.
 */
export function decode(bits: bigint, mem: Uint8Array): Value {
  return decodeIn(bits, mem, [])
}

/** {@link decode} with `path`, the arrays on the current recursion path. */
function decodeIn(bits: bigint, mem: Uint8Array, path: number[]): Value {
  const u = asU64(bits)
  // Any value without the full QNAN mask set is an ordinary number (this also
  // covers infinities, whose exponent is all ones but quiet bit is clear).
  if ((u & QNAN) !== QNAN) return { kind: 'number', value: bitsToF64(u) }
  // QNAN is set: the 3-bit tag in payload bits [50:48] discriminates.
  switch ((u >> 48n) & 0b111n) {
    case 0b001n:
      return { kind: 'bool', value: (u & 1n) !== 0n }
    case 0b010n:
      return { kind: 'null' }
    case 0b011n:
      return { kind: 'undefined' }
    case 0b101n:
      return decodeHeap(u, mem, path)
    // Tags 0b100/0b110/0b111 are retired and 0b000 is the canonical number NaN.
    default:
      return { kind: 'number', value: bitsToF64(u) }
  }
}

/** Resolve a heap pointer jsval by reading the block's TYPE BYTE. */
function decodeHeap(bits: bigint, mem: Uint8Array, path: number[]): Value {
  const ptr = Number(bits & U32_MASK)
  switch (mem[ptr]) {
    case TYPE_ARRAY: {
      if (path.includes(ptr)) return { kind: 'circular' }
      path.push(ptr)
      const value = decodeArray(ptr, mem, path)
      path.pop()
      return value
    }
    case TYPE_OBJECT:
      return decodeObject(ptr, mem)
    case TYPE_FUNCTION:
      return decodeFunction(ptr, mem)
    case TYPE_STRING:
      return decodeString(ptr, mem)
    case TYPE_SYMBOL:
      return decodeSymbol(ptr, mem)
    case TYPE_MAP:
      return { kind: 'object', tag: 'Map' }
    case TYPE_SET:
      return { kind: 'object', tag: 'Set' }
    case TYPE_PROMISE:
      return { kind: 'object', tag: 'Promise' }
    case TYPE_GENERATOR:
      return { kind: 'object', tag: 'Generator' }
    case TYPE_DATE:
      return decodeDate(ptr, mem)
    default:
      return { kind: 'undefined' }
  }
}

/**
 * Resolve a heap symbol block at `ptr` by its description jsval (mirrors
 * `value.rs::decode_symbol`): a string is the description, anything else is
 * `Symbol()`.
 */
function decodeSymbol(ptr: number, mem: Uint8Array): Value {
  const bits = readU64(mem, ptr + SYMBOL_DESCRIPTION_OFFSET)
  const desc = bits === undefined ? undefined : decode(bits, mem)
  return {
    kind: 'symbol',
    description: desc?.kind === 'string' ? desc.value : undefined,
  }
}

/**
 * Resolve a heap `Date` block at `ptr` by reading its `f64` ms payload (mirrors
 * `value.rs::decode_date`); a truncated block reads as `NaN` (`Invalid Date`).
 */
function decodeDate(ptr: number, mem: Uint8Array): Value {
  const bits = readU64(mem, ptr + DATE_MS_OFFSET)
  return { kind: 'date', ms: bits === undefined ? Number.NaN : bitsToF64(bits) }
}

/** Resolve a heap string block at `ptr` (see {@link stringText}); a truncated block reads as `''`. */
function decodeString(ptr: number, mem: Uint8Array): Value {
  return { kind: 'string', value: stringText(mem, ptr) ?? '' }
}

/** Code units per `String.fromCharCode` call (keeps the argument list small). */
const CHAR_CODE_CHUNK = 0x2000

/**
 * The text of the heap STRING block at `ptr`, or `null` when truncated: `len`
 * (at `ptr + STR_LEN_OFFSET`) Latin-1 bytes, or with `STR_FLAG_TWO_BYTE` `len`
 * UTF-16LE units, at `ptr + STR_DATA_OFFSET`. Lossless (a lone surrogate stays
 * a unit); `TextDecoder` is avoided on purpose — its `latin1` label is
 * windows-1252 and `utf-16le` replaces lone surrogates (mirrors
 * `value.rs::string_units`).
 */
export function stringText(mem: Uint8Array, ptr: number): string | null {
  const len = readU32(mem, ptr + STR_LEN_OFFSET)
  if (len === undefined) return null
  const twoByte =
    ((mem[ptr + HEADER_FLAGS_OFFSET] ?? 0) & STR_FLAG_TWO_BYTE) !== 0
  const start = ptr + STR_DATA_OFFSET
  const size = twoByte ? len * 2 : len
  if (start + size > mem.length) return null
  let units: Uint8Array | Uint16Array = mem.subarray(start, start + len)
  if (twoByte) {
    // A copy when the view is not 2-aligned (a `Uint16Array` needs it).
    const bytes =
      (mem.byteOffset + start) % 2 === 0
        ? mem.subarray(start, start + size)
        : mem.slice(start, start + size)
    units = new Uint16Array(bytes.buffer, bytes.byteOffset, len)
  }
  let text = ''
  for (let i = 0; i < len; i += CHAR_CODE_CHUNK) {
    text += String.fromCharCode(...units.subarray(i, i + CHAR_CODE_CHUNK))
  }
  return text
}

/**
 * The `length` of the array handle at `ptr` and its present elements as
 * `[index, bits]` in ascending index order (holes are absent): the store
 * slots, then — for a `SPARSE` array, whose store holds only the elements
 * below its `cap` — the array-index keys of its own table (mirrors
 * `value.rs::array_elements`).
 */
export function arrayElements(
  mem: Uint8Array,
  ptr: number,
): [len: number, present: [number, bigint][]] | undefined {
  const len = readU32(mem, ptr + ARRAY_LEN_OFFSET)
  const store = readU32(mem, ptr + ARRAY_ELEMS_OFFSET)
  if (len === undefined || store === undefined) return undefined
  const data = store + ELEMS_DATA_OFFSET
  const sparse =
    ((mem[ptr + HEADER_FLAGS_OFFSET] ?? 0) & ARRAY_FLAG_SPARSE) !== 0
  let stored = len
  if (sparse) {
    const cap = store === 0 ? 0 : (readU32(mem, store + ELEMS_CAP_OFFSET) ?? 0)
    stored = Math.min(len, cap)
  }
  const present: [number, bigint][] = []
  for (let k = 0; k < stored; k++) {
    const bits = readU64(mem, data + 8 * k)
    if (bits === undefined) break
    if (bits !== HOLE) present.push([k, bits])
  }
  if (sparse) {
    const keyed = indexEntries(ptr, mem)
    keyed.sort((a, b) => a[0] - b[0])
    present.push(...keyed)
  }
  return [len, present]
}

/**
 * Resolve a heap array handle at `ptr`: its present elements (each decoded
 * recursively) with every run of holes up to `len` as one `holes`.
 */
function decodeArray(ptr: number, mem: Uint8Array, path: number[]): Value {
  const elements = arrayElements(mem, ptr)
  if (elements === undefined) return { kind: 'array', items: [] }
  const [len, present] = elements
  const items: Value[] = []
  let next = 0
  for (const [index, bits] of present) {
    if (index > next) items.push({ kind: 'holes', count: index - next })
    items.push(decodeIn(bits, mem, path))
    next = index + 1
  }
  if (len > next) items.push({ kind: 'holes', count: len - next })
  return { kind: 'array', items }
}

/**
 * The live enumerable integer-keyed entries of the property table of the
 * array at `ptr`, in slot order, as `[index, bits]`.
 */
function indexEntries(ptr: number, mem: Uint8Array): [number, bigint][] {
  const props = readU32(mem, ptr + OBJ_PROPS_OFFSET) ?? 0
  if (props === 0) return []
  if (((mem[props + PROPS_TFLAGS_OFFSET] ?? 0) & PROPS_HAS_INDEX_KEYS) === 0)
    return []
  const count = readU32(mem, props + PROPS_COUNT_OFFSET) ?? 0
  const keyed: [number, bigint][] = []
  for (let k = 0; k < count; k++) {
    const entry = props + PROPS_HEADER_SIZE + PROP_ENTRY_SIZE * k
    const key = readU32(mem, entry + PROP_KEY_OFFSET)
    const attrs = readU32(mem, entry + PROP_ATTRS_OFFSET)
    const bits = readU64(mem, entry + PROP_VALUE_OFFSET)
    if (key === undefined || attrs === undefined || bits === undefined) break
    if (key === 0 || (attrs & ATTR_ENUMERABLE) === 0) continue
    if (mem[key] === TYPE_SYMBOL) continue
    if (((mem[key + HEADER_FLAGS_OFFSET] ?? 0) & STR_FLAG_ARRAY_INDEX) === 0)
      continue
    keyed.push([Number(stringAt(key, mem)), bits])
  }
  return keyed
}

/**
 * Resolve a heap object block at `ptr` by its `class` byte: an Error with its
 * (possibly inherited) `name` and `message`, a wrapper with its primitive, or
 * an `[object Tag]` object.
 */
function decodeObject(ptr: number, mem: Uint8Array): Value {
  const classByte = mem[ptr + HEADER_CLASS_OFFSET] ?? 0
  if (classByte === CLASS_ERROR) {
    const field = (name: string) => {
      const bits = lookupData(ptr, name, mem)
      const value = bits === undefined ? undefined : decode(bits, mem)
      return value?.kind === 'string' ? value.value : undefined
    }
    return {
      kind: 'error',
      name: field('name') ?? 'Error',
      message: field('message') ?? '',
    }
  }
  if (classByte === CLASS_ARGUMENTS) return { kind: 'object', tag: 'Arguments' }
  if (
    (classByte >= CLASS_BOOLEAN && classByte <= CLASS_STRING) ||
    classByte === CLASS_SYMBOL
  ) {
    const bits = readU64(mem, ptr + WRAPPER_VALUE_OFFSET)
    const primitive: Value =
      bits === undefined ? { kind: 'undefined' } : decode(bits, mem)
    return { kind: 'wrapper', primitive }
  }
  return { kind: 'object', tag: 'Object' }
}

/** Resolve a function block at `ptr` (mirrors `value.rs::decode_function`). */
function decodeFunction(ptr: number, mem: Uint8Array): Value {
  const isClass =
    ((mem[ptr + HEADER_FLAGS_OFFSET] ?? 0) & FUNC_FLAG_CLASS_CTOR) !== 0
  let parent: string | undefined
  if (isClass) {
    // The parent is the class's own [[Prototype]] when that is a constructor
    // (a base class inherits from the non-constructor %Function.prototype%).
    const link = readU64(mem, ptr + OBJ_PROTO_OFFSET)
    const linkPtr = link === undefined ? undefined : heapObject(link, mem)
    if (
      linkPtr !== undefined &&
      mem[linkPtr] === TYPE_FUNCTION &&
      ((mem[linkPtr + HEADER_FLAGS_OFFSET] ?? 0) & FUNC_FLAG_CONSTRUCTOR) !== 0
    )
      parent = functionName(HEAP | BigInt(linkPtr), mem)
  }
  // A bound function's name is its own `name` property (`bound f`).
  let name: string | undefined
  if (((mem[ptr + HEADER_FLAGS_OFFSET] ?? 0) & FUNC_FLAG_BOUND) === 0) {
    name = functionName(HEAP | BigInt(ptr), mem)
  } else {
    const bits = ownData(ptr, 'name', mem)
    const own = bits === undefined ? undefined : decode(bits, mem)
    name = own?.kind === 'string' ? own.value : undefined
  }
  return { kind: 'function', name: name ?? '', isClass, extends: parent }
}

/** The text of the string block at `ptr`. */
function stringAt(ptr: number, mem: Uint8Array): string {
  const decoded = decodeString(ptr, mem)
  return decoded.kind === 'string' ? decoded.value : ''
}

/**
 * The block base of `bits` when it is a heap pointer to an object-family
 * block with a `props` slot (mirrors `value.rs::heap_object`).
 */
function heapObject(bits: bigint, mem: Uint8Array): number | undefined {
  if ((bits & QNAN) !== QNAN || ((bits >> 48n) & 0b111n) !== 0b101n)
    return undefined
  const ptr = Number(bits & U32_MASK)
  const ty = mem[ptr]
  return ty === undefined ||
    ty === TYPE_STRING ||
    ty === TYPE_ARRAY ||
    ty === TYPE_SYMBOL
    ? undefined
    : ptr
}

/** The value bits of the own DATA property `key` at `ptr` (mirrors `value.rs::own_data`). */
function ownData(
  ptr: number,
  key: string,
  mem: Uint8Array,
): bigint | undefined {
  const props = readU32(mem, ptr + OBJ_PROPS_OFFSET) ?? 0
  if (props === 0) return undefined
  const count = readU32(mem, props + PROPS_COUNT_OFFSET) ?? 0
  for (let k = 0; k < count; k++) {
    const entry = props + PROPS_HEADER_SIZE + PROP_ENTRY_SIZE * k
    const keyPtr = readU32(mem, entry + PROP_KEY_OFFSET)
    const attrs = readU32(mem, entry + PROP_ATTRS_OFFSET)
    if (keyPtr === undefined || attrs === undefined) return undefined
    if (
      keyPtr !== 0 &&
      (attrs & ATTR_ACCESSOR) === 0 &&
      stringAt(keyPtr, mem) === key
    )
      return readU64(mem, entry + PROP_VALUE_OFFSET)
  }
  return undefined
}

/**
 * The value bits of the data property `key` on `ptr` or the nearest object on
 * its `proto` chain (mirrors `value.rs::lookup_data`; bounded walk).
 */
function lookupData(
  ptr: number,
  key: string,
  mem: Uint8Array,
): bigint | undefined {
  let cur = ptr
  for (let i = 0; i < 1024; i++) {
    const own = ownData(cur, key, mem)
    if (own !== undefined) return own
    const proto = readU64(mem, cur + OBJ_PROTO_OFFSET)
    const next = proto === undefined ? undefined : heapObject(proto, mem)
    if (next === undefined) return undefined
    cur = next
  }
  return undefined
}

/** The `__fn_info` name of the function `bits` (mirrors `value.rs::function_name`). */
function functionName(bits: bigint, mem: Uint8Array): string | undefined {
  const ptr = heapObject(bits, mem)
  if (ptr === undefined || mem[ptr] !== TYPE_FUNCTION) return undefined
  const info = readU32(mem, FN_INFO_ADDR)
  const index = readU32(mem, ptr + FUNC_INDEX_OFFSET)
  if (info === undefined || index === undefined) return undefined
  const key = readU32(
    mem,
    info + FN_INFO_ENTRY_SIZE * index + FN_INFO_NAME_OFFSET,
  )
  return key === undefined ? undefined : stringAt(key, mem)
}

/**
 * The JS `ToString` text of a decoded {@link Value} (mirrors
 * `value.rs::to_js_string`): arrays join their elements with `,` (nullish
 * elements and cycle back-references are empty), objects are `[object Tag]`,
 * errors use `Error.prototype.toString`.
 */
export function toJsString(value: Value): string {
  switch (value.kind) {
    case 'number':
      return String(value.value)
    case 'bool':
      return value.value ? 'true' : 'false'
    case 'null':
      return 'null'
    case 'undefined':
      return 'undefined'
    case 'string':
      return value.value
    case 'symbol':
      return `Symbol(${value.description ?? ''})`
    case 'array':
      return joinElements(value.items, ',')
    case 'object':
      return `[object ${value.tag}]`
    case 'wrapper':
      return toJsString(value.primitive)
    case 'error':
      if (value.message === '') return value.name
      if (value.name === '') return value.message
      return `${value.name}: ${value.message}`
    case 'function':
      if (value.isClass)
        return `[class ${value.name === '' ? '(anonymous)' : value.name}${value.extends === undefined ? '' : ` extends ${value.extends}`}]`
      return value.name === ''
        ? '[Function (anonymous)]'
        : `[Function: ${value.name}]`
    case 'date':
      return formatDateIso(value.ms)
    case 'circular':
    case 'holes':
      return ''
  }
}

/**
 * `Array.prototype.join` over decoded array elements: each element's
 * `ToString` (nullish elements, holes, and cycle back-references are empty)
 * separated by `sep`; a run of `N` holes contributes `N` empty pieces (mirrors
 * `value.rs::join_elements`).
 */
function joinElements(items: Value[], sep: string): string {
  const pieces: string[] = []
  for (const v of items) {
    if (v.kind === 'holes') {
      for (let i = 0; i < v.count; i++) pieces.push('')
    } else {
      pieces.push(
        v.kind === 'null' || v.kind === 'undefined' ? '' : toJsString(v),
      )
    }
  }
  return pieces.join(sep)
}

/**
 * Render a millisecond epoch timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ` (mirrors
 * `value.rs::format_date_iso`, Howard Hinnant's `civil_from_days`); a
 * non-finite timestamp renders as `Invalid Date`. Ported rather than delegated
 * to `Date.prototype.toISOString`, which throws for an invalid time value.
 */
function formatDateIso(ms: number): string {
  if (!Number.isFinite(ms)) return 'Invalid Date'
  const totalMs = Math.floor(ms)
  const days = Math.floor(totalMs / 86_400_000)
  let tod = totalMs - days * 86_400_000
  const millis = tod % 1000
  tod = Math.trunc(tod / 1000)
  const secs = tod % 60
  tod = Math.trunc(tod / 60)
  const mins = tod % 60
  const hours = Math.trunc(tod / 60)

  const z = days + 719_468
  const era = Math.trunc((z >= 0 ? z : z - 146_096) / 146_097)
  const doe = z - era * 146_097
  const yoe = Math.trunc(
    (doe -
      Math.trunc(doe / 1460) +
      Math.trunc(doe / 36_524) -
      Math.trunc(doe / 146_096)) /
      365,
  )
  const doy = doe - (365 * yoe + Math.trunc(yoe / 4) - Math.trunc(yoe / 100))
  const mp = Math.trunc((5 * doy + 2) / 153)
  const day = doy - Math.trunc((153 * mp + 2) / 5) + 1
  const month = mp < 10 ? mp + 3 : mp - 9
  const year = yoe + era * 400 + (month <= 2 ? 1 : 0)

  return `${isoYear(year)}-${pad2(month)}-${pad2(day)}T${pad2(hours)}:${pad2(mins)}:${pad2(secs)}.${String(millis).padStart(3, '0')}Z`
}

/** Zero-pad to two digits (Rust `{:02}` for a non-negative integer). */
function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

/** Four digits, or an expanded `±YYYYYY` year outside `0..=9999`. */
function isoYear(year: number): string {
  if (year >= 0 && year <= 9999) return String(year).padStart(4, '0')
  return (year < 0 ? '-' : '+') + String(Math.abs(year)).padStart(6, '0')
}
