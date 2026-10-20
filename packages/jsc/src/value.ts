/**
 * A TypeScript port of `crates/jsc-core/src/run/value.rs` (NaN-box decoder and
 * the Node-style value formatter) plus the numeric coercions from
 * `run/host.rs`. It duplicates Rust runtime semantics (see README, "Host
 * parity") so the browser shim can render emitted-module values.
 */

import {
  DATE_MS_OFFSET,
  HEADER_SIZE,
  PROMISE_FULFILLED,
  PROMISE_REJECTED,
  QNAN,
  TYPE_ARRAY,
  TYPE_DATE,
  TYPE_FUNCTION,
  TYPE_OBJECT,
  TYPE_PROMISE,
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
  | { kind: 'promise'; state: number; value: Value }
  | { kind: 'date'; ms: number }
  | { kind: 'circular'; id: number }
  | { kind: 'ref'; id: number; value: Value }

/**
 * Cycle-guard state for one {@link decode} call (mirrors `value.rs::Guard`):
 * the heap blocks on the current recursion path and the cycle targets numbered
 * so far, in discovery order (Node's `*1`, `*2`, ...).
 */
interface Guard {
  path: number[]
  refs: number[]
}

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
  return decodeIn(bits, mem, { path: [], refs: [] })
}

/** {@link decode} under the cycle guard `guard` (mirrors `value.rs::decode_in`). */
function decodeIn(bits: bigint, mem: Uint8Array, guard: Guard): Value {
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
      return decodeHeap(u, mem, guard)
    // Tags 0b110/0b111 are retired and 0b000 is the canonical number NaN.
    default:
      return { kind: 'number', value: bitsToF64(u) }
  }
}

/**
 * Resolve a heap pointer jsval by reading the block's TYPE BYTE. Containers
 * (array / object / promise) are cycle-guarded: a block already on the
 * recursion path decodes as `circular`, and the block it points back to is
 * wrapped in `ref` once decoded.
 */
function decodeHeap(bits: bigint, mem: Uint8Array, guard: Guard): Value {
  const ptr = Number(bits & U32_MASK)
  const ty = mem[ptr]
  if (ty === TYPE_ARRAY || ty === TYPE_OBJECT || ty === TYPE_PROMISE) {
    if (guard.path.includes(ptr)) {
      let id = guard.refs.indexOf(ptr) + 1
      if (id === 0) id = guard.refs.push(ptr)
      return { kind: 'circular', id }
    }
    guard.path.push(ptr)
    const value =
      ty === TYPE_ARRAY
        ? decodeArray(ptr, mem, guard)
        : ty === TYPE_OBJECT
          ? decodeObject(ptr, mem, guard)
          : decodePromise(ptr, mem, guard)
    guard.path.pop()
    const id = guard.refs.indexOf(ptr) + 1
    return id === 0 ? value : { kind: 'ref', id, value }
  }
  switch (ty) {
    case TYPE_STRING:
      return decodeString(ptr, mem)
    case TYPE_FUNCTION:
      return { kind: 'function' }
    case TYPE_DATE:
      return decodeDate(ptr, mem)
    default:
      return { kind: 'undefined' }
  }
}

/**
 * Resolve a heap promise block at `ptr` (mirrors `value.rs::decode_promise`):
 * the `i32` state at `ptr + HEADER_SIZE` and the settled `i64` value after it. A
 * truncated block reads as pending with an `undefined` value.
 */
function decodePromise(ptr: number, mem: Uint8Array, guard: Guard): Value {
  const state = readU32(mem, ptr + HEADER_SIZE)
  const bits = readU64(mem, ptr + HEADER_SIZE + 4)
  return {
    kind: 'promise',
    state: state === undefined ? 0 : state | 0,
    value:
      bits === undefined ? { kind: 'undefined' } : decodeIn(bits, mem, guard),
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
function decodeArray(ptr: number, mem: Uint8Array, guard: Guard): Value {
  const len = readU32(mem, ptr + HEADER_SIZE)
  if (len === undefined) return { kind: 'array', items: [] }
  const items: Value[] = []
  for (let k = 0; k < len; k++) {
    const elem = readU64(mem, ptr + HEADER_SIZE + 4 + 8 * k)
    if (elem === undefined) break
    items.push(decodeIn(elem, mem, guard))
  }
  return { kind: 'array', items }
}

/** Resolve a heap object block at `ptr`: `n` × `(key_ptr:i64, val:i64)` entries. */
function decodeObject(ptr: number, mem: Uint8Array, guard: Guard): Value {
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
    entries.push([key, decodeIn(valBits, mem, guard)])
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
    case 'promise':
      if (value.state === PROMISE_FULFILLED)
        return `Promise { ${formatValue(value.value)} }`
      if (value.state === PROMISE_REJECTED)
        return `Promise { <rejected> ${formatValue(value.value)} }`
      return 'Promise { <pending> }'
    case 'date':
      return formatDateIso(value.ms)
    // Cycle markers render Node-console style.
    case 'circular':
      return `[Circular *${value.id}]`
    case 'ref':
      return `<ref *${value.id}> ${formatValue(value.value)}`
  }
}

/**
 * Render a millisecond epoch timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ` (mirrors
 * `value.rs::format_date_iso`, Howard Hinnant's `civil_from_days`); a
 * non-finite timestamp renders as `Invalid Date`. Ported rather than delegated
 * to `Date.prototype.toISOString`, which uses extended `±YYYYYY` years and
 * throws out of range.
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

  return `${padSigned(year, 4)}-${pad2(month)}-${pad2(day)}T${pad2(hours)}:${pad2(mins)}:${pad2(secs)}.${String(millis).padStart(3, '0')}Z`
}

/** Zero-pad to two digits (Rust `{:02}` for a non-negative integer). */
function pad2(n: number): string {
  return String(n).padStart(2, '0')
}

/** Zero-pad to `width` chars counting the sign (Rust `{:0width}` for a signed integer). */
function padSigned(n: number, width: number): string {
  const sign = n < 0 ? '-' : ''
  return sign + String(Math.abs(n)).padStart(width - sign.length, '0')
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

/**
 * `parseInt(s, radix)` on the already-stringified `s` and number-coerced `radix`
 * (mirrors `host.rs::parse_int_str`): skip leading JS whitespace, an optional
 * sign, then parse the longest radix-`ToInt32(radix)` digit prefix (`0` means 10,
 * or 16 after a `0x` prefix; `16` also strips `0x`; outside `2..=36` is NaN).
 */
export function parseIntStr(str: string, radix: number): number {
  let s = str.replace(/^\s+/, '')
  let sign = 1
  if (s.startsWith('-')) {
    sign = -1
    s = s.slice(1)
  } else if (s.startsWith('+')) {
    s = s.slice(1)
  }
  const r = radix | 0
  let base = r === 0 ? 10 : r
  if (base < 2 || base > 36) return Number.NaN
  if ((r === 0 || r === 16) && /^0x/i.test(s)) {
    s = s.slice(2)
    base = 16
  }
  let end = 0
  while (end < s.length && digitValue(s.charCodeAt(end)) < base) end++
  const digits = s.slice(0, end)
  if (digits.length === 0) return Number.NaN
  // Base 10 goes through the correctly rounded decimal parser; other radices
  // accumulate digit by digit.
  let value = 0
  if (base === 10) value = Number(digits)
  else
    for (let i = 0; i < end; i++)
      value = value * base + digitValue(s.charCodeAt(i))
  return sign * value
}

/** The value of an ASCII digit or (case-insensitive) letter, or 36 for any other code unit. */
function digitValue(code: number): number {
  if (code >= 48 && code <= 57) return code - 48
  const lower = code | 0x20
  if (lower >= 97 && lower <= 122) return lower - 87
  return 36
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
