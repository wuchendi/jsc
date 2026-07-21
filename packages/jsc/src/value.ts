/**
 * A TypeScript port of `crates/jsc-core/src/run/value.rs` (NaN-box decoder and
 * the Node-style value formatter) plus the numeric coercions from
 * `run/host.rs`. ACCEPTED TEMPORARY DEBT (see README): this duplicates Rust
 * runtime semantics so the browser shim can render emitted-module values.
 */

import {
  HEADER_SIZE,
  QNAN,
  TYPE_ARRAY,
  TYPE_FUNCTION,
  TYPE_OBJECT,
  TYPE_STRING,
  TYPEOF_STRINGS,
  U32_MASK,
  U64_MASK,
} from './abi.js'

/** A decoded jsval — the in-scope runtime value types (mirrors `value.rs::Value`). */
export type Value =
  | { kind: 'number'; value: number }
  | { kind: 'bool'; value: boolean }
  | { kind: 'null' }
  | { kind: 'undefined' }
  | { kind: 'typestr'; value: string }
  | { kind: 'string'; value: string }
  | { kind: 'array'; items: Value[] }
  | { kind: 'object'; entries: [string, Value][] }
  | { kind: 'function' }

// A single scratch buffer for f64 <-> u64 bit reinterpretation, reused so we do
// not allocate a DataView per decode.
const scratch = new DataView(new ArrayBuffer(8))

/** Reinterpret raw `u64` bits as an IEEE-754 `f64` (mirrors `f64::from_bits`). */
export function bitsToF64(bits: bigint): number {
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

const utf8 = new TextDecoder()

/**
 * Decode raw jsval `bits` into a {@link Value} per the NaN-box layout.
 *
 * `mem` is the module's linear-memory view; it is consulted only for heap
 * pointer jsvals (tag `0b101`). Pass an empty array for immediate values.
 * Mirrors `value.rs::decode`.
 */
export function decode(bits: bigint, mem: Uint8Array): Value {
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
    case 0b100n: {
      const s = TYPEOF_STRINGS[Number(u & U32_MASK)]
      return { kind: 'typestr', value: s ?? 'undefined' }
    }
    case 0b101n:
      return decodeHeap(u, mem)
    // Tags 0b110/0b111 are retired and 0b000 is the canonical number NaN.
    default:
      return { kind: 'number', value: bitsToF64(u) }
  }
}

/** Resolve a heap pointer jsval by reading the block's TYPE BYTE. */
function decodeHeap(bits: bigint, mem: Uint8Array): Value {
  const ptr = Number(bits & U32_MASK)
  switch (mem[ptr]) {
    case TYPE_STRING:
      return decodeString(ptr, mem)
    case TYPE_ARRAY:
      return decodeArray(ptr, mem)
    case TYPE_OBJECT:
      return decodeObject(ptr, mem)
    case TYPE_FUNCTION:
      return { kind: 'function' }
    default:
      return { kind: 'undefined' }
  }
}

/** Resolve a heap string block at `ptr`: `[len:u32][utf8]` at `ptr + HEADER_SIZE`. */
function decodeString(ptr: number, mem: Uint8Array): Value {
  const len = readU32(mem, ptr + HEADER_SIZE)
  if (len === undefined) return { kind: 'string', value: '' }
  const start = ptr + HEADER_SIZE + 4
  if (start + len > mem.length) return { kind: 'string', value: '' }
  return {
    kind: 'string',
    value: utf8.decode(mem.subarray(start, start + len)),
  }
}

/** Resolve a heap array block at `ptr`, decoding each `i64` element recursively. */
function decodeArray(ptr: number, mem: Uint8Array): Value {
  const len = readU32(mem, ptr + HEADER_SIZE)
  if (len === undefined) return { kind: 'array', items: [] }
  const items: Value[] = []
  for (let k = 0; k < len; k++) {
    const elem = readU64(mem, ptr + HEADER_SIZE + 4 + 8 * k)
    if (elem === undefined) break
    items.push(decode(elem, mem))
  }
  return { kind: 'array', items }
}

/** Resolve a heap object block at `ptr`: `n` × `(key_ptr:i64, val:i64)` entries. */
function decodeObject(ptr: number, mem: Uint8Array): Value {
  const n = readU32(mem, ptr + HEADER_SIZE)
  if (n === undefined) return { kind: 'object', entries: [] }
  const entries: [string, Value][] = []
  for (let k = 0; k < n; k++) {
    const keyStart = ptr + HEADER_SIZE + 4 + 16 * k
    const keyBits = readU64(mem, keyStart)
    const valBits = readU64(mem, keyStart + 8)
    if (keyBits === undefined || valBits === undefined) break
    const decodedKey = decode(keyBits, mem)
    const key = decodedKey.kind === 'string' ? decodedKey.value : ''
    entries.push([key, decode(valBits, mem)])
  }
  return { kind: 'object', entries }
}

/**
 * Render an `f64` the way JavaScript prints a number. `value.rs::format_number`
 * reimplements ECMAScript `Number::toString` by hand; JS `String(n)` already IS
 * that algorithm (NaN/Infinity/-0/shortest-round-trip/exponential thresholds all
 * match), so this delegates to it.
 */
export function formatNumber(value: number): string {
  return String(value)
}

/** Render a decoded {@link Value} as user-facing text (mirrors `value.rs::format_value`). */
export function formatValue(value: Value): string {
  switch (value.kind) {
    case 'number':
      return formatNumber(value.value)
    case 'bool':
      return value.value ? 'true' : 'false'
    case 'null':
      return 'null'
    case 'undefined':
      return 'undefined'
    case 'typestr':
      return value.value
    // A string renders as its raw text, with no surrounding quotes.
    case 'string':
      return value.value
    case 'array':
      return value.items.length === 0
        ? '[]'
        : `[ ${value.items.map(formatValue).join(', ')} ]`
    case 'object':
      return value.entries.length === 0
        ? '{}'
        : `{ ${value.entries.map(([k, v]) => `${k}: ${formatValue(v)}`).join(', ')} }`
    case 'function':
      return '[Function (anonymous)]'
  }
}

/**
 * Coerce a decoded {@link Value} to a JS number (the `Number(x)` subset from
 * `host.rs::value_to_number`): number is identity, `true`/`false` are `1`/`0`,
 * `null` is `0`, `undefined` is `NaN`, a string is full-string parsed, and the
 * heap kinds (array/object/function) yield `NaN`.
 */
export function valueToNumber(value: Value): number {
  switch (value.kind) {
    case 'number':
      return value.value
    case 'bool':
      return value.value ? 1 : 0
    case 'null':
      return 0
    case 'string':
    case 'typestr':
      return strToNumber(value.value)
    default:
      return Number.NaN
  }
}

/** Parse a whole string as JS `Number(str)` (mirrors `host.rs::str_to_number`). */
function strToNumber(s: string): number {
  const trimmed = s.replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, '')
  if (trimmed.length === 0) return 0
  // `Number("bad")` is already `NaN`, matching Rust's `parse::<f64>().unwrap_or(NaN)`.
  return Number(trimmed)
}

/** Parse a leading base-10 integer (the `parseInt` subset from `host.rs::parse_int_str`). */
export function parseIntStr(s: string): number {
  let i = 0
  while (i < s.length && isAsciiWhitespace(s[i]!)) i++
  let sign = 1
  if (i < s.length && (s[i] === '+' || s[i] === '-')) {
    if (s[i] === '-') sign = -1
    i++
  }
  const start = i
  let value = 0
  while (i < s.length && isAsciiDigit(s[i]!)) {
    value = value * 10 + (s.charCodeAt(i) - 48)
    i++
  }
  if (i === start) return Number.NaN
  return sign * value
}

/** Parse a leading decimal float (the `parseFloat` subset from `host.rs::parse_float_str`). */
export function parseFloatStr(s: string): number {
  let i = 0
  while (i < s.length && isAsciiWhitespace(s[i]!)) i++
  const from = i
  if (i < s.length && (s[i] === '+' || s[i] === '-')) i++
  let seenDigit = false
  while (i < s.length && isAsciiDigit(s[i]!)) {
    i++
    seenDigit = true
  }
  if (i < s.length && s[i] === '.') {
    i++
    while (i < s.length && isAsciiDigit(s[i]!)) {
      i++
      seenDigit = true
    }
  }
  if (!seenDigit) return Number.NaN
  // Optional exponent: only consumed when at least one exponent digit follows.
  if (i < s.length && (s[i] === 'e' || s[i] === 'E')) {
    let j = i + 1
    if (j < s.length && (s[j] === '+' || s[j] === '-')) j++
    const expStart = j
    while (j < s.length && isAsciiDigit(s[j]!)) j++
    if (j > expStart) i = j
  }
  // `Number("bad")` is already `NaN`, matching Rust's `parse::<f64>().unwrap_or(NaN)`.
  return Number(s.slice(from, i))
}

function isAsciiWhitespace(c: string): boolean {
  return (
    c === ' ' ||
    c === '\t' ||
    c === '\n' ||
    c === '\r' ||
    c === '\f' ||
    c === '\v'
  )
}

function isAsciiDigit(c: string): boolean {
  return c >= '0' && c <= '9'
}
