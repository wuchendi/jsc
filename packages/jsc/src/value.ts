/**
 * A TypeScript port of `crates/jsc-core/src/run/value.rs` (NaN-box decoder and
 * the Node-style value formatter) plus the numeric coercions from
 * `run/host.rs`. It duplicates Rust runtime semantics (see README, "Host
 * parity") so the browser shim can render emitted-module values.
 */

import {
  ACCESSOR_GET_OFFSET,
  ACCESSOR_SET_OFFSET,
  ARRAY_ELEMS_OFFSET,
  ARRAY_FLAG_SPARSE,
  ARRAY_LEN_OFFSET,
  ATTR_ACCESSOR,
  ATTR_ENUMERABLE,
  CLASS_ARGUMENTS,
  CLASS_BOOLEAN,
  CLASS_ERROR,
  CLASS_STRING,
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
  HEADER_SIZE,
  HEAP,
  HOLE,
  MAP_SIZE_OFFSET,
  OBJ_PROPS_OFFSET,
  OBJ_PROTO_OFFSET,
  PROMISE_FULFILLED,
  PROMISE_REJECTED,
  PROMISE_STATE_OFFSET,
  PROMISE_VALUE_OFFSET,
  PROP_ATTRS_OFFSET,
  PROP_ENTRY_SIZE,
  PROP_KEY_OFFSET,
  PROP_VALUE_OFFSET,
  PROPS_COUNT_OFFSET,
  PROPS_HAS_INDEX_KEYS,
  PROPS_HEADER_SIZE,
  PROPS_TFLAGS_OFFSET,
  QNAN,
  STR_FLAG_ARRAY_INDEX,
  TYPE_ARRAY,
  TYPE_DATE,
  TYPE_FUNCTION,
  TYPE_GENERATOR,
  TYPE_MAP,
  TYPE_OBJECT,
  TYPE_PROMISE,
  TYPE_SET,
  TYPE_STRING,
  U32_MASK,
  U64_MASK,
  UNDEFINED,
  WRAPPER_VALUE_OFFSET,
} from './abi.js'

/** A decoded jsval — the in-scope runtime value types (mirrors `value.rs::Value`). */
export type Value =
  | { kind: 'number'; value: number }
  | { kind: 'bool'; value: boolean }
  | { kind: 'null' }
  | { kind: 'undefined' }
  | { kind: 'string'; value: string }
  | { kind: 'array'; items: Value[]; props: [string, Value][] }
  | { kind: 'holes'; count: number }
  | { kind: 'object'; entries: [string, Value][] }
  | { kind: 'instance'; className: string; entries: [string, Value][] }
  | { kind: 'arguments'; entries: [string, Value][] }
  | { kind: 'wrapper'; primitive: Value; entries: [string, Value][] }
  | { kind: 'error'; name: string; message: string }
  | { kind: 'accessor'; getter: boolean; setter: boolean }
  | {
      kind: 'function'
      name: string
      isClass: boolean
      extends: string | undefined
      props: [string, Value][]
    }
  | { kind: 'map'; size: number }
  | { kind: 'set'; size: number }
  | { kind: 'promise'; state: number; value: Value }
  | { kind: 'generator' }
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
const utf8Encode = new TextEncoder()

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
    case 0b101n:
      return decodeHeap(u, mem, guard)
    // Tags 0b100/0b110/0b111 are retired and 0b000 is the canonical number NaN.
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
  if (
    ty === TYPE_ARRAY ||
    ty === TYPE_OBJECT ||
    ty === TYPE_PROMISE ||
    ty === TYPE_FUNCTION
  ) {
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
          : ty === TYPE_FUNCTION
            ? decodeFunction(ptr, mem, guard)
            : decodePromise(ptr, mem, guard)
    guard.path.pop()
    const id = guard.refs.indexOf(ptr) + 1
    return id === 0 ? value : { kind: 'ref', id, value }
  }
  switch (ty) {
    case TYPE_STRING:
      return decodeString(ptr, mem)
    case TYPE_MAP:
      return { kind: 'map', size: decodeSize(ptr, mem) }
    case TYPE_SET:
      return { kind: 'set', size: decodeSize(ptr, mem) }
    case TYPE_GENERATOR:
      return { kind: 'generator' }
    case TYPE_DATE:
      return decodeDate(ptr, mem)
    default:
      return { kind: 'undefined' }
  }
}

/**
 * Resolve a heap promise block at `ptr` (mirrors `value.rs::decode_promise`):
 * the `i32` state at `ptr + PROMISE_STATE_OFFSET` and the settled `i64` value at
 * `ptr + PROMISE_VALUE_OFFSET`. A truncated block reads as pending with an
 * `undefined` value.
 */
function decodePromise(ptr: number, mem: Uint8Array, guard: Guard): Value {
  const state = readU32(mem, ptr + PROMISE_STATE_OFFSET)
  const bits = readU64(mem, ptr + PROMISE_VALUE_OFFSET)
  return {
    kind: 'promise',
    state: state === undefined ? 0 : state | 0,
    value:
      bits === undefined ? { kind: 'undefined' } : decodeIn(bits, mem, guard),
  }
}

/**
 * The `u32` logical entry count of a Map/Set handle block at `ptr` (mirrors
 * `value.rs::decode_size`); a truncated block reads as `0`.
 */
function decodeSize(ptr: number, mem: Uint8Array): number {
  return readU32(mem, ptr + MAP_SIZE_OFFSET) ?? 0
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

/**
 * The length of the array handle at `ptr` and the address of its element 0
 * (element `k` is at `data + 8 * k` of its element store), or `undefined`
 * when the handle is truncated (mirrors `value.rs::array_layout`).
 */
export function arrayLayout(
  mem: Uint8Array,
  ptr: number,
): [len: number, data: number] | undefined {
  const len = readU32(mem, ptr + ARRAY_LEN_OFFSET)
  const elems = readU32(mem, ptr + ARRAY_ELEMS_OFFSET)
  if (len === undefined || elems === undefined) return undefined
  return [len, elems + ELEMS_DATA_OFFSET]
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
  const layout = arrayLayout(mem, ptr)
  if (layout === undefined) return undefined
  const [len, data] = layout
  const sparse =
    ((mem[ptr + HEADER_FLAGS_OFFSET] ?? 0) & ARRAY_FLAG_SPARSE) !== 0
  let stored = len
  if (sparse) {
    const store = readU32(mem, ptr + ARRAY_ELEMS_OFFSET) ?? 0
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
    const keyed: [number, bigint][] = []
    for (const [index, , , bits] of tableEntries(ptr, mem)) {
      if (index !== undefined) keyed.push([index, bits])
    }
    keyed.sort((a, b) => a[0] - b[0])
    present.push(...keyed)
  }
  return [len, present]
}

/**
 * Resolve a heap array handle at `ptr`: its present elements (each decoded
 * recursively) with every run of holes up to `len` as one `holes`, then its
 * own named properties (mirrors `value.rs::decode_array`).
 */
function decodeArray(ptr: number, mem: Uint8Array, guard: Guard): Value {
  const elements = arrayElements(mem, ptr)
  if (elements === undefined) return { kind: 'array', items: [], props: [] }
  const [len, present] = elements
  const items: Value[] = []
  let next = 0
  for (const [index, bits] of present) {
    if (index > next) items.push({ kind: 'holes', count: index - next })
    items.push(decodeIn(bits, mem, guard))
    next = index + 1
  }
  if (len > next) items.push({ kind: 'holes', count: len - next })
  return { kind: 'array', items, props: decodeProps(ptr, mem, guard) }
}

/**
 * Decode the own enumerable properties of the object-family block at `ptr` by
 * walking its property table (mirrors `value.rs::decode_props`): tombstones
 * and non-enumerable entries are skipped, integer keys come first in
 * ascending order when the table has `HAS_INDEX_KEYS`, then the other keys in
 * slot order. An accessor property decodes as `accessor`.
 */
function decodeProps(
  ptr: number,
  mem: Uint8Array,
  guard: Guard,
): [string, Value][] {
  // An array's integer keys are its (sparse) elements, decoded by `decodeArray`.
  const array = mem[ptr] === TYPE_ARRAY
  const keyed = tableEntries(ptr, mem).filter(
    ([index]) => !(array && index !== undefined),
  )
  // A stable sort keeps the non-index keys in slot order after the indices.
  keyed.sort((a, b) =>
    a[0] === undefined
      ? b[0] === undefined
        ? 0
        : 1
      : b[0] === undefined
        ? -1
        : a[0] - b[0],
  )
  const entries: [string, Value][] = []
  for (const [, key, attrs, bits] of keyed) {
    if ((attrs & ATTR_ACCESSOR) !== 0) {
      const pair = Number(bits & U32_MASK)
      const half = (offset: number) => {
        const b = readU64(mem, pair + offset)
        return b !== undefined && b !== UNDEFINED
      }
      entries.push([
        key,
        {
          kind: 'accessor',
          getter: half(ACCESSOR_GET_OFFSET),
          setter: half(ACCESSOR_SET_OFFSET),
        },
      ])
    } else {
      entries.push([key, decodeIn(bits, mem, guard)])
    }
  }
  return entries
}

/**
 * The live enumerable entries of the property table of the object-family
 * block at `ptr`, in slot order, as `[index, key, attrs, bits]` — `index` is
 * the value of an integer key (mirrors `value.rs::table_entries`).
 */
function tableEntries(
  ptr: number,
  mem: Uint8Array,
): [number | undefined, string, number, bigint][] {
  const props = readU32(mem, ptr + OBJ_PROPS_OFFSET) ?? 0
  if (props === 0) return []
  const count = readU32(mem, props + PROPS_COUNT_OFFSET) ?? 0
  const tflags = mem[props + PROPS_TFLAGS_OFFSET] ?? 0
  const keyed: [number | undefined, string, number, bigint][] = []
  for (let k = 0; k < count; k++) {
    const entry = props + PROPS_HEADER_SIZE + PROP_ENTRY_SIZE * k
    const key = readU32(mem, entry + PROP_KEY_OFFSET)
    const attrs = readU32(mem, entry + PROP_ATTRS_OFFSET)
    const bits = readU64(mem, entry + PROP_VALUE_OFFSET)
    if (key === undefined || attrs === undefined || bits === undefined) break
    if (key === 0) continue
    const text = stringAt(key, mem)
    if ((attrs & ATTR_ENUMERABLE) === 0) continue
    const flags = mem[key + HEADER_FLAGS_OFFSET] ?? 0
    const index =
      (tflags & PROPS_HAS_INDEX_KEYS) !== 0 &&
      (flags & STR_FLAG_ARRAY_INDEX) !== 0
        ? Number(text)
        : undefined
    keyed.push([index, text, attrs, bits])
  }
  return keyed
}

/**
 * Resolve a heap object block at `ptr` from its own enumerable properties
 * (mirrors `value.rs::decode_object`): an object of class `ERROR` decodes as
 * `error` with its (possibly inherited) `name` and `message`; an object whose
 * prototype chain (from its prototype, so a prototype object's own
 * `constructor` back-link does not name it) holds a `constructor` naming a
 * function other than `Object` decodes as `instance`.
 */
function decodeObject(ptr: number, mem: Uint8Array, guard: Guard): Value {
  if (mem[ptr + HEADER_CLASS_OFFSET] === CLASS_ERROR) {
    const field = (name: string) => {
      const bits = lookupData(ptr, name, mem)
      const value = bits === undefined ? undefined : decodeIn(bits, mem, guard)
      return value?.kind === 'string' ? value.value : undefined
    }
    return {
      kind: 'error',
      name: field('name') ?? 'Error',
      message: field('message') ?? '',
    }
  }
  let entries = decodeProps(ptr, mem, guard)
  const classByte = mem[ptr + HEADER_CLASS_OFFSET] ?? 0
  if (classByte === CLASS_ARGUMENTS) return { kind: 'arguments', entries }
  if (classByte >= CLASS_BOOLEAN && classByte <= CLASS_STRING) {
    const bits = readU64(mem, ptr + WRAPPER_VALUE_OFFSET)
    const primitive: Value =
      bits === undefined ? { kind: 'undefined' } : decodeIn(bits, mem, guard)
    if (primitive.kind === 'string') {
      // A String wrapper's own indices are its characters, not properties.
      const len = utf8Encode.encode(primitive.value).length
      entries = entries.filter(
        ([key]) => !(/^\d+$/.test(key) && Number(key) < len),
      )
    }
    return { kind: 'wrapper', primitive, entries }
  }
  const proto = readU64(mem, ptr + OBJ_PROTO_OFFSET)
  const protoPtr = proto === undefined ? undefined : heapObject(proto, mem)
  if (protoPtr !== undefined) {
    const ctor = lookupData(protoPtr, 'constructor', mem)
    const className = ctor === undefined ? undefined : functionName(ctor, mem)
    if (className !== undefined && className !== '' && className !== 'Object')
      return { kind: 'instance', className, entries }
  }
  return { kind: 'object', entries }
}

/** Resolve a function block at `ptr` (mirrors `value.rs::decode_function`). */
function decodeFunction(ptr: number, mem: Uint8Array, guard: Guard): Value {
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
  return {
    kind: 'function',
    name: name ?? '',
    isClass,
    extends: parent,
    props: decodeProps(ptr, mem, guard),
  }
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
  return ty === undefined || ty === TYPE_STRING || ty === TYPE_ARRAY
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
    // A string renders as its raw text, with no surrounding quotes.
    case 'string':
      return value.value
    case 'array':
      return formatArray(value.items, value.props)
    // A hole outside an array (never produced) reads as `undefined`.
    case 'holes':
      return 'undefined'
    case 'object':
      return formatEntries(value.entries)
    // A class instance is prefixed with its constructor's name.
    case 'instance':
      return `${value.className} ${formatEntries(value.entries)}`
    // A wrapper names its kind and quotes a wrapped string, as Node does.
    case 'wrapper': {
      const p = value.primitive
      const head =
        p.kind === 'string'
          ? `[String: '${p.value}']`
          : p.kind === 'number'
            ? `[Number: ${Object.is(p.value, -0) ? '-0' : formatNumber(p.value)}]`
            : `[Boolean: ${formatValue(p)}]`
      return value.entries.length === 0
        ? head
        : `${head} ${formatEntries(value.entries)}`
    }
    // An `arguments` object quotes its (index) keys, as Node does.
    case 'arguments':
      return `[Arguments] ${formatEntries(
        value.entries.map(([k, v]): [string, Value] => [quoteKey(k), v]),
      )}`
    // A branded Error renders as its single-line stack.
    case 'error':
      return value.message === ''
        ? value.name
        : `${value.name}: ${value.message}`
    case 'accessor':
      return value.getter && value.setter
        ? '[Getter/Setter]'
        : value.setter
          ? '[Setter]'
          : '[Getter]'
    case 'function': {
      const head = value.isClass
        ? `[class ${value.name === '' ? '(anonymous)' : value.name}${value.extends === undefined ? '' : ` extends ${value.extends}`}]`
        : value.name === ''
          ? '[Function (anonymous)]'
          : `[Function: ${value.name}]`
      return value.props.length === 0
        ? head
        : `${head} ${formatEntries(value.props)}`
    }
    case 'map':
      return `Map(${value.size})`
    case 'set':
      return `Set(${value.size})`
    case 'generator':
      return 'Object [Generator]'
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
 * The JS `ToString` text of a decoded {@link Value} (mirrors
 * `value.rs::to_js_string`): arrays join their elements with `,` (nullish
 * elements and cycle back-references are empty), plain objects and instances
 * are `[object Object]`, errors use `Error.prototype.toString`, Map/Set/
 * promise/generator their tags; functions and Dates keep the console text.
 */
export function toJsString(value: Value): string {
  switch (value.kind) {
    case 'array':
      return joinElements(value.items, ',')
    case 'object':
    case 'instance':
      return '[object Object]'
    case 'arguments':
      return '[object Arguments]'
    case 'wrapper':
      return toJsString(value.primitive)
    case 'error':
      if (value.message === '') return value.name
      if (value.name === '') return value.message
      return `${value.name}: ${value.message}`
    case 'map':
      return '[object Map]'
    case 'set':
      return '[object Set]'
    case 'promise':
      return '[object Promise]'
    case 'generator':
      return '[object Generator]'
    case 'accessor':
    case 'circular':
    case 'holes':
      return ''
    case 'ref':
      return toJsString(value.value)
    default:
      return formatValue(value)
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
 * Render an array Node-console style: `[ 1, 2 ]`, `[]` when empty, a run of
 * holes as `<N empty items>`, named properties last (mirrors the
 * `value.rs::format_value` array arm).
 */
function formatArray(items: Value[], props: [string, Value][]): string {
  const parts: string[] = []
  let holes = 0
  const flush = () => {
    if (holes > 0) parts.push(`<${holes} empty item${holes === 1 ? '' : 's'}>`)
    holes = 0
  }
  for (const item of items) {
    if (item.kind === 'holes') {
      holes += item.count
    } else {
      flush()
      parts.push(formatValue(item))
    }
  }
  flush()
  for (const [k, v] of props) parts.push(`${k}: ${formatValue(v)}`)
  return parts.length === 0 ? '[]' : `[ ${parts.join(', ')} ]`
}

/** A property key as Node's console prints it: identifier-like bare, else single-quoted. */
function quoteKey(key: string): string {
  return /^[\p{L}\p{N}_$]+$/u.test(key) && !/^[0-9]/.test(key)
    ? key
    : `'${key}'`
}

/** Render own properties Node-console style: `{ a: 1 }`, `{}` when empty. */
function formatEntries(entries: [string, Value][]): string {
  return entries.length === 0
    ? '{}'
    : `{ ${entries.map(([k, v]) => `${k}: ${formatValue(v)}`).join(', ')} }`
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
