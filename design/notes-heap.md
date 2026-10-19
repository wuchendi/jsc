# Notes: heap layouts, runtime helpers, and the host boundary

Fact notes for the phase-2c object-model design (`design/object-model.md`).
Scope: value encoding, every heap block layout, `@@`-reserved keys, the
allocator, and every host-side reader/writer of guest heap memory. Not covered
here (sibling notes): codegen dispatch/specialization (`notes-codegen.md`),
array/string/primitive semantics and GC strategy (`notes-runtime.md`),
test262 clusters (`notes-test262.md`).

All citations are `path:line` at commit `b1dc6b9`. Paths are relative to the
repo root; `core/` abbreviates `crates/jsc-core/src/`, `pkg/` abbreviates
`packages/jsc/src/`.

**May change (concurrent workstreams, not on this branch):**

- `mr9syrjv` (runtime-semantics) edits `core/emit/**` and `core/run/**`: the
  primitive-receiver property-write guard (touches the store dispatch in
  `core/emit/object.rs:1533-1591`), string properties (the read dispatch in
  `core/emit/object.rs:1329-1447`), `+` coercion (`__to_number`,
  `core/emit/value.rs:50-74`, and `__convert`), unhandled rejections
  (`core/run/promise.rs`), and the `parseInt` radix (`__convert` op 2,
  `core/run/harness.rs:172`). Line numbers in those files may shift; layouts
  are not expected to change.
- `iqxrfhn5` (codegen-language) edits `core/codegen/**`: generator closures
  (generator frame / env construction inputs), hoisting (module cells), member
  destructuring, capture gaps (env/cell usage). Producers cited under
  `core/codegen/` may move.

---

## 1. Value model (NaN-boxing)

A jsval is one 64-bit word carried as wasm `i64` (`core/abi.rs:12-31`).

| constant | bits | meaning | cite |
|---|---|---|---|
| `QNAN` | `0x7FF8_0000_0000_0000` | boxed iff `bits & QNAN == QNAN`; also the canonical number NaN | `core/abi.rs:41-43` |
| tag `0b000` | `QNAN` exactly | number NaN (canonical) | `core/abi.rs:20`, `core/abi.rs:29-31` |
| `FALSE` / `TRUE` | `0x7FF9_0000_0000_0000` / `…_0001` | tag `0b001`, value in bit 0 | `core/abi.rs:45-49` |
| `NULL` | `0x7FFA_0000_0000_0000` | tag `0b010` | `core/abi.rs:51-52` |
| `UNDEFINED` | `0x7FFB_0000_0000_0000` | tag `0b011` | `core/abi.rs:54-57` |
| tag `0b100` | — | unused (retired `typeof` immediate) | `core/abi.rs:24`, `core/abi.rs:59-61` |
| `HEAP` | `0x7FFD_0000_0000_0000` | tag `0b101`, the SINGLE heap tag; `HEAP \| (ptr as u32)` | `core/abi.rs:73-76` |
| tags `0b110`, `0b111` | — | unused | `core/abi.rs:26-27` |
| TDZ hole | `UNDEFINED \| 1` | internal: module cell not yet initialized; never a JS value | `core/emit/mod.rs:216-219` |

- Pointer encoding: the low 32 bits are the byte offset of the block BASE in
  linear memory; bits 32..47 are zero (`core/abi.rs:73-76`,
  `core/emit/mod.rs:237-241`). No pointer tagging in low bits.
- Heap-pointer recognition (guest): `(bits & QNAN) == QNAN` AND tag ==
  `0b101` (`core/emit/value.rs:109-119`); the QNAN check is required because a
  plain f64 may carry tag bits `0b101` (`core/emit/value.rs:103-108`).
  Number test is a signed compare `bits <= QNAN | 0xFFFF_FFFF_FFFF`
  (`core/emit/value.rs:82-86`).
- Heap-pointer recognition (host): `core/run/host.rs:250-256`,
  `core/run/promise.rs:717` (`heap_ptr`), `pkg/shim.ts:782-788`; decoders
  `core/run/value.rs:76-94`, `pkg/value.ts:89-112`.
- Heap kind = TYPE BYTE at block offset 0, not the tag (`core/abi.rs:33-39`);
  read with `i32.load8_u` at offset 0 (`core/emit/value.rs:125-133`), always
  gated behind the pointer test (`core/emit/value.rs:141-217`).
- NaN canonicalization: every f64→jsval rebox goes through `__box_f64`
  (`core/emit/value.rs:4-29`); host twin `number_bits`
  (`core/run/harness.rs:31-35`).
- Parity drift: `pkg/abi.ts:24-25` still exports `TYPEOF = 0x7FFC…` and
  `pkg/value.ts:102-105` still decodes tag `0b100` as a `typestr`; Rust treats
  it as a NaN number (`core/run/value.rs:89-92`).

---

## 2. Heap block layouts

Common header (every block): `[type:u8][pad:u8 × 3]`, `HEADER_SIZE = 4`
(`core/abi.rs:78-80`, `core/emit/mod.rs:243-246`). Payload starts at +4.
Little-endian. Bytes 1..3 are written as zero by every allocator path that
builds a block from a byte vector (`core/run/host.rs:63-64`,
`core/emit/sections.rs:382-383`) and are never read by anyone (no reader
masks or tests them — see §4).

Alignment: blocks are NOT 8-aligned in general. Static strings are packed
contiguously from `HEAP_BASE = 8` with no padding (`core/emit/sections.rs:430-438`),
`__alloc` does not round sizes (`core/emit/runtime.rs:16-79`), so the first
heap block and every later one can start at any byte offset. Most loads use
`align: 0` hints (e.g. `core/emit/runtime.rs:537-546`); `align: 3` hints exist
for Date/cells (`core/emit/object.rs:510-516`, `core/emit/function.rs:72-75`)
but are hints only.

### 2.1 TYPE_STRING = 0

`core/abi.rs:93-95`

| offset | size | field |
|---|---|---|
| 0 | 1 | type `0` |
| 4 | 4 | `len: u32` (UTF-8 byte count) |
| 8 | len | UTF-8 bytes |

- Static literals: data segment blobs at compile-time offsets from
  `HEAP_BASE = 8` (offset 0 is a null guard) (`core/emit/mod.rs:231-235`,
  `core/emit/sections.rs:370-439`). Object KEYS from literals are these static
  blobs (`core/emit/object.rs:56-66`).
- Guest allocators: `__concat` (`core/emit/runtime.rs:184`), `__str_index`
  (`core/emit/runtime.rs:444`), `__json_escape` (`core/emit/runtime.rs:1587`),
  `__json_str` (`core/emit/runtime.rs:1756`), `__json_string`
  (`core/emit/json_parse.rs:240`), Date ISO (`core/emit/object.rs:989`).
- Host allocators: `host_alloc_string` (`core/run/host.rs:57-69`),
  `hostAllocString` (`pkg/shim.ts:172-181`).
- Readers: `__str_eq`/`__str_cmp` (`core/emit/runtime.rs:270`, `:347`),
  `.length` (`core/emit/expr.rs:1017-1036`), host `string_bytes`
  (`core/run/host.rs:274-278`), decoders (`core/run/value.rs:165-176`,
  `pkg/value.ts:160-169`).

### 2.2 TYPE_ARRAY = 1

`core/abi.rs:97-99`. Details of array semantics are in `notes-runtime.md`.

| offset | size | field |
|---|---|---|
| 0 | 1 | type `1` |
| 4 | 4 | `len: u32` |
| 8 + 8·i | 8 | element jsval `i` |

- No capacity field: size is exactly `8 + 8·len`. `__array_push` allocates a
  new `len+1` block, copies, and returns a NEW base
  (`core/emit/runtime.rs:913-987`); the caller rebinds its local
  (`core/emit/array.rs:170-216`). `pop` shrinks `len` in place
  (`core/emit/array.rs:218-264`). Aliases of a pushed array keep the old block.
- Allocators: literal (`core/emit/array.rs:14`), `__array_push`,
  `__array_concat` (`core/emit/runtime.rs:999`), `__obj_keys/values/entries`
  (`core/emit/runtime.rs:1148`, `:1219`, `:1291`), `__str_to_array`
  (`core/emit/runtime.rs:2163`), `__gen_to_array`
  (`core/emit/generator.rs:482`), JSON parse arrays
  (`core/emit/json_parse.rs:521`), host `array_slice` / `string_split`
  (`core/run/host.rs:411-431`, `:618-653`), shim twins (`pkg/shim.ts:193-217`,
  `:305-341`).
- Also used as internal backing: Map/Set entries (§2.7), object-rest exclude
  list (`core/emit/runtime.rs:741-753`), JSON-object key/value pair staging
  (`core/emit/json_parse.rs:588-591`).

### 2.3 TYPE_OBJECT = 2 (the block the new design replaces)

`core/abi.rs:101-104`, `core/emit/mod.rs:262-265`

| offset | size | field |
|---|---|---|
| 0 | 1 | type `2` |
| 4 | 4 | `n: u32` entry count |
| 8 + 16·k | 8 | key jsval (heap STRING pointer) of entry `k` |
| 16 + 16·k | 8 | value jsval of entry `k` |

- Size exactly `8 + 16·n`; no capacity, no shape id, no prototype slot, no
  flags (`core/emit/object.rs:34-36`).
- Keys compared by byte content via `__str_eq` (`core/emit/runtime.rs:569-571`);
  insertion order = table order. Lookup is a linear scan.
- Fixed shape: `__obj_set` only overwrites an existing key and returns `0` on
  a miss (`core/emit/runtime.rs:656-739`); the caller throws via `__prop_throw`
  (`core/emit/object.rs:1596-1615`, message `core/emit/mod.rs:450-458`).
- Allocators: object literal / every `IrNode::Object` (`core/emit/object.rs:12-82`),
  `__obj_rest` (`core/emit/runtime.rs:757`, type byte at `:802-804`),
  `__array_to_object` for `JSON.parse` (`core/emit/json_parse.rs:698-785`),
  generator `{value, done}` result (`core/emit/generator.rs:234-318`), branded
  error objects (§3.3). Class values, prototypes and instances are all plain
  `TYPE_OBJECT` blocks built from `IrNode::Object` (§2.12).
- Guest readers/writers: `__obj_get` (`core/emit/runtime.rs:520-654`),
  `__obj_set` (`:670-739`), `__obj_has` (`:1392`), `__obj_delete` compacts
  entries with `memory.copy` and decrements `n` (`:1449`, doc `core/emit/runtime.rs:1436-1448`),
  `__obj_keys/values/entries` (`:1148`, `:1219`, `:1291`), `__obj_rest`
  (`:757`), `__json_str` object arm (`:1862`, `:1887`), `ObjectKey` direct key
  load for `for-in` (`core/emit/object.rs:1461-1503`), `.length`/`StrLen`
  reading `n` as the `for-in` count (`core/codegen/stmt.rs:1413`,
  `core/emit/expr.rs:1017-1036`), member read dispatch
  (`core/emit/object.rs:1388-1396`), member store dispatch
  (`core/emit/object.rs:1574-1588`).
- Host readers: `decode_object` (`core/run/value.rs:211-236`),
  `decodeObject` (`pkg/value.ts:185-199`) — see §5.

### 2.4 TYPE_FUNCTION = 3

`core/abi.rs:106-108`, size `FUNC_BLOCK_SIZE = 16` (`core/emit/mod.rs:273-275`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `3` |
| 4 | 4 | `func_idx: u32` (funcref table slot) |
| 8 | 8 | `env` jsval (`TYPE_ENV` pointer, `undefined`, or — for promise settlers — the bound promise) |

- No property storage: functions cannot carry own properties
  (`fn.prop`/`fn.prototype` have no slot). Class statics live on a separate
  `TYPE_OBJECT` class value (§2.12).
- Allocators: `emit_func_value` (`core/emit/expr.rs:850-922`), settler values
  with `env = promise` (`core/emit/expr.rs:1222-1269`).
- Readers: `call_indirect` sites read `env` at +8 and `func_idx` at +4
  (`core/emit/expr.rs:1294-1311`, `core/emit/runtime.rs:120-139`,
  method calls `core/emit/expr.rs:577-590`); settlers read the env param
  (`core/emit/runtime.rs:156-173`); host checks only the type byte
  (`core/run/promise.rs:710-713`, `pkg/shim.ts:800-804`).

### 2.5 TYPE_ENV = 4 (internal)

`core/abi.rs:110-112`

| offset | size | field |
|---|---|---|
| 0 | 1 | type `4` |
| 4 | 4 | `count: u32` |
| 8 + 8·i | 8 | cell pointer jsval `i` |

- Allocators: `emit_env_block` (`core/emit/expr.rs:924-976`), generator frame
  (`core/emit/generator.rs:41-72`).
- Readers: `EnvLoad` (`core/emit/expr.rs:1049-1061`); generator SM through
  the ordinary env path (`core/abi.rs:157-168`). Never decoded by the host
  (`core/run/value.rs:113` catch-all → `undefined`).

### 2.6 TYPE_CELL = 5 (internal) — boxed bindings and module cells

`core/abi.rs:114-117`, size `CELL_BLOCK_SIZE = 12` (`core/emit/mod.rs:297-299`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `5` |
| 4 | 8 | `value` jsval |

- Allocator: `emit_cell_new` (`core/emit/expr.rs:978-1015`); module cells are
  cells whose pointers live in globals `9 + n` (`core/emit/mod.rs:211-214`),
  allocated in `main`'s prologue and pre-filled with the TDZ hole
  (`core/emit/function.rs:62-75`).
- Readers/writers: `emit_cell_load` / `emit_cell_set`
  (`core/emit/expr.rs:1038-1089`).

### 2.7 TYPE_MAP = 6 / TYPE_SET = 7

`core/abi.rs:119-133`, size `MAP_BLOCK_SIZE = 16` (`core/emit/mod.rs:311-314`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `6` / `7` |
| 4 | 4 | `size: u32` logical entry count |
| 8 | 8 | `entries` jsval → `TYPE_ARRAY` of flattened `[k0, v0, k1, v1, …]` (Set: `v = undefined`) |

- Handle never moves; growth reallocs the backing array via `__array_push`
  and rewrites `entries` in place (`core/emit/runtime.rs:2328-2335`).
  `delete` compacts the pair out of the backing array in place, lowering its
  `len` by 2 and `size` by 1 (`core/emit/runtime.rs:2436-2446`), so the
  backing block's allocation size exceeds its `len` afterwards.
- Allocator: `emit_map_new_node` (`core/emit/object.rs:440-503`).
- Readers: `__map_find/set/get/has/delete` (`core/emit/runtime.rs:2251-2523`),
  `.size` intercept in member reads (`core/emit/object.rs:1358-1381`), host
  `decode_size` (`core/run/value.rs:150-158`). Shim does NOT decode Map/Set
  (`pkg/value.ts:115-133` has no case; renders `undefined`).

### 2.8 TYPE_PROMISE = 8

`core/abi.rs:135-155`, size `PROMISE_BLOCK_SIZE = 16` (`core/run/promise.rs:34-35`,
`pkg/shim.ts:114-115`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `8` |
| 4 | 4 | `state: i32` (0 pending / 1 fulfilled / 2 rejected) |
| 8 | 8 | settled `value` jsval (`undefined` while pending) |

- Allocated ONLY host-side: `alloc_promise` (`core/run/promise.rs:643-657`),
  `allocPromise` (`pkg/shim.ts:472-480`). The guest obtains promises only as
  `__promise`/`__then` results (`core/emit/expr.rs:1100-1131`,
  `core/emit/expr.rs:1142-1148`).
- Written host-side: `write_settlement` (`core/run/promise.rs:659-673`),
  `writeSettlement` (`pkg/shim.ts:483-489`).
- Reactions/microtask queue live host-side, keyed by block base
  (`core/run/promise.rs:89-95`, `pkg/shim.ts:450-452`).
- The guest never reads the promise block fields (no emit reference to
  `PROMISE_*` state outside imports); the renderer does
  (`core/run/value.rs:133-148`, `pkg/value.ts:140-148`).

### 2.9 TYPE_GENERATOR = 9

`core/abi.rs:157-182`, size `GEN_BLOCK_SIZE = 24` (`core/emit/mod.rs:277-284`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `9` |
| 4 | 4 | `sm_idx: u32` (funcref slot of the state machine) |
| 8 | 4 | `state: i32` resume point |
| 12 | 4 | `done: i32` |
| 16 | 8 | `env` jsval → `TYPE_ENV` frame of cells |

- Allocator: `emit_make_generator` (`core/emit/generator.rs:19-126`).
- Readers/writers: `emit_gen_resume` (`core/emit/generator.rs:128-213`), the
  SM body (`core/emit/generator.rs:320-416`, `store_gen_i32` `:642`),
  `__gen_to_array` (`:482`), `__async_step` (`:587-640`); host reads `done`
  at +12 (`core/run/promise.rs:434-444`, `pkg/shim.ts:821-827`).
- Iterator result objects are plain `TYPE_OBJECT` `{value, done}` built with
  interned `"value"`/`"done"` keys (`core/emit/generator.rs:234-318`).
- No separate iterator block kind exists; `for-of` over generators drains via
  `__gen_to_array` (`core/emit/generator.rs:482`).

### 2.10 TYPE_DATE = 10

`core/abi.rs:184-195`, size `DATE_BLOCK_SIZE = 16` (`core/emit/mod.rs:316-324`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `10` |
| 4 | 4 | padding |
| 8 | 8 | `ms: f64` |

- Allocator: `emit_date_new_node` (`core/emit/object.rs:518-565`).
- Readers: Date methods (`core/emit/object.rs:576-1110`), decoders
  (`core/run/value.rs:117-127`, `pkg/value.ts:154-157`).

### 2.11 TYPE_ACCESSOR = 11 (internal)

`core/abi.rs:197-218`, size `ACCESSOR_BLOCK_SIZE = 24` (`core/emit/mod.rs:326-334`)

| offset | size | field |
|---|---|---|
| 0 | 1 | type `11` |
| 4 | 4 | padding |
| 8 | 8 | getter function jsval (or `undefined`) |
| 16 | 8 | setter function jsval (or `undefined`) |

- Allocator: `emit_make_accessor_node` (`core/emit/object.rs:84-148`), only as
  a value inside a class prototype `IrNode::Object`
  (`core/codegen/stmt.rs:384-432`).
- Readers: getter postlude on member reads (`core/emit/expr.rs:601-680`),
  setter path on member stores (`core/emit/object.rs:1703-1797`); detection
  `emit_is_accessor` (`core/emit/value.rs:201-217`). Host decoders map it to
  `undefined` (catch-all, `core/run/value.rs:113`).
- The accessor is a VALUE stored under a data key; there is no per-property
  attribute word. Accessor dispatch is compiled in only when `@@accessor` is
  interned (`core/emit/mod.rs:903-906`).

### 2.12 Classes, prototypes, instances (no dedicated block kind)

All three are `TYPE_OBJECT` blocks; the link is the hidden `@@prototype` entry.

- Class value: `{ "@@constructor": ctor_fn, "@@prototype": proto_obj, ...statics }`
  (`core/codegen/stmt.rs:490-499`).
- Prototype object: one entry per method name + accessor boxes, plus
  `@@prototype → parent prototype` under `extends`
  (`core/codegen/stmt.rs:367-441`).
- Instance (`new C`): `{ "@@prototype": C["@@prototype"], field_1: undefined, … }`,
  every field pre-slotted so `this.f = …` hits an existing key
  (`core/codegen/expr.rs:1845-1860`).
- Prototype-chain read: on own miss, `__obj_get` scans again for its own
  `@@prototype` and recurses (`core/emit/runtime.rs:590-648`). Fallback is
  compiled in only when `@@prototype` is interned (`core/emit/mod.rs:1073-1077`).
- `instanceof` for classes walks `@@prototype` links by pointer identity
  (`core/emit/object.rs:253-302`).

### 2.13 Branded error objects (no dedicated block kind)

Plain `TYPE_OBJECT` with entries `name`, `message`, `stack`, `@@errorkind`
(in that order). Producers:

- `new Error/TypeError/RangeError(...)` (`core/codegen/expr.rs:1745-1750`).
- TDZ `ReferenceError` (branded as base `Error`) (`core/codegen/scope.rs:276-287`).
- Runtime not-a-function guard (`core/emit/mod.rs:390-412`).
- `__nullish_throw` / `__prop_throw` via `emit_raise_type_error`
  (`core/emit/object.rs:2056-2093`, `:2102-2131`).
- `JSON.parse` syntax error (branded `Error`, name `SyntaxError`)
  (`core/emit/mod.rs:499-513`).

Their key strings must be interned at every guarded site
(`core/emit/mod.rs:376-388`, `:430-448`, `:460-473`, `:486-497`).

---

## 3. `@@`-reserved keys

The `@@` prefix cannot start a JS identifier, so source `obj.name` never
collides (`core/abi.rs:237-244`).

| key | meaning | produced | consumed |
|---|---|---|---|
| `@@prototype` | class prototype link (class value → prototype; instance → prototype; derived prototype → parent prototype) | `core/codegen/stmt.rs:440`, `core/codegen/stmt.rs:493-499`, `core/codegen/expr.rs:1856` | `__obj_get` fallback (`core/emit/runtime.rs:590-648`, gate `core/emit/mod.rs:1076`); `instanceof` (`core/emit/object.rs:262-301`, `core/codegen/expr.rs:1623-1629`); `super.m` (`core/codegen/expr.rs:482`); `new` (`core/codegen/expr.rs:1847-1850`) |
| `@@constructor` | class constructor function value | `core/codegen/stmt.rs:494` | `new` (`core/codegen/expr.rs:1851-1854`); `super(...)` and implicit derived ctor (`core/codegen/expr.rs:445`, `core/codegen/stmt.rs:361`) |
| `@@errorkind` | error identity marker; number `0` Error / `1` TypeError / `2` RangeError (`core/abi.rs:253-272`) | §2.13 producers | `instanceof Error/TypeError/RangeError` (`core/emit/object.rs:303-343`, interned at `core/emit/sections.rs:700-707`); renderer `is_error_object` (`core/run/value.rs:420-425`, `core/run/value.rs:344`) |
| `@@accessor` | compile-time sentinel only: present in the string table iff the program has an accessor; gates accessor dispatch; never stored in an object | `core/emit/sections.rs:680-685` | `core/emit/mod.rs:903-906` (`abi.rs:220-226`) |
| `@@ys_it_<pos>` / `@@ys_r_<pos>` | NOT heap keys: synthetic local binding names for `yield*` | `core/codegen/generator.rs:148-153` | codegen scope only |

Hiding mechanism: a first-payload-byte `'@'` test, `emit_key_not_reserved`
(`core/emit/runtime.rs:1122-1138`). Applied ONLY in `__obj_keys` (`:1186`),
`__obj_values` (`:1258`), `__obj_entries` (`:1331`) and `__json_str` (`:1887`).
NOT applied in:

- `for-in`: count = raw `n` (`core/codegen/stmt.rs:1413`), key = raw entry key
  (`core/emit/object.rs:1481-1496`) — `@@` keys are enumerated (DESIGN.md
  section 5.1 claims otherwise; DESIGN.md:354-357).
- `__obj_has` (`in`): own scan, matches `@@` keys if spelled (not reachable
  from source identifiers, but reachable via a string `"@@prototype" in o`)
  (`core/emit/runtime.rs:1382-1392`).
- `__obj_rest` (`{...r} = o`): copies `@@prototype`/`@@errorkind` into the
  rest object (no reserved check in `core/emit/runtime.rs:757-911`).
- Renderers: `decode_object` keeps every entry and `format_value` prints them
  (`core/run/value.rs:211-236`, `:346-357`), so a class instance prints its
  `@@prototype` entry (and nested prototype); errors are special-cased
  (`core/run/value.rs:344`). Same in the shim, which also lacks the error
  special case (`pkg/value.ts:231-234`).
- Computed access `o["@@prototype"]` reads/writes the hidden slot directly
  (member dispatch passes any string key to `__obj_get`/`__obj_set`,
  `core/emit/object.rs:1388-1396`, `:1574-1588`).

---

## 4. Allocator

`__alloc(size: i32) -> i32` (`core/emit/runtime.rs:4-79`):

1. `old = $__heap_ptr` (global 0, `core/emit/mod.rs:160-161`), `end =
   u64(old) + u64(size)` (`core/emit/runtime.rs:23-30`).
2. Fast path when `end <= memory.size << 16` (`core/emit/runtime.rs:31-37`).
3. Grow by `max(need, memory.size)` pages, fallback exactly `need`
   (`core/emit/runtime.rs:38-63`).
4. OOM: store `HEAP_PTR_OOM = -1` and `unreachable` (`core/emit/runtime.rs:65-68`,
   `core/abi.rs:373-377`); max 8192 pages = 512 MiB (`core/abi.rs:368-371`,
   memory section `core/emit/mod.rs:986-996`).

- Start: `heap_ptr_init` = first byte after the static strings
  (`core/emit/sections.rs:428-439`, global init `core/emit/sections.rs:305-333`).
- Host allocation shares the same global: `heap_reserve` / `host_alloc`
  (`core/run/host.rs:80-128`), promise engine `heap_alloc`
  (`core/run/promise.rs:675-693`), shim `hostAlloc` (`pkg/shim.ts:143-170`).
  Every Rust host import result passes `oom_guard` (`core/run/harness.rs:51-64`);
  the run fails with `Uncaught RangeError: out of memory` after `main` / the
  drain (`core/run/harness.rs:257-280`).
- Shim parity gap: `hostAlloc` never stores the OOM sentinel and returns
  `null` on grow failure (`pkg/shim.ts:158-163`).
- No free, no size/capacity metadata, no free list, no mark bits. A block's
  size is NOT recoverable from the block alone for variable kinds without the
  kind-specific formula (`8 + len` string, `8 + 8·len` array/env, `8 + 16·n`
  object); and even that is wrong after in-place shrink (`pop` lowers `len`,
  `core/emit/array.rs:218-264`; `__obj_delete` lowers `n`,
  `core/emit/runtime.rs:1436-1448`; `__obj_rest` writes `m <= n` into a block
  sized for `n`, `core/emit/runtime.rs:741-753`). Abandoned blocks are not
  marked in any way.
- Free header space today: bytes 1, 2, 3 of every block (24 bits) are
  always-zero padding and unread (`core/abi.rs:35-39`). The Date and
  accessor blocks additionally carry 4 bytes of padding at +4
  (`core/abi.rs:184`, `:197-198`).
- Static strings live in the data segment below `heap_ptr_init`
  (`core/emit/sections.rs:370-390`); they share the block format and are
  reachable from object keys and code constants, but were never allocated
  by `__alloc`.

---

## 5. Host boundary

Imports (all module `env`, function indices 0–5): `core/emit/mod.rs:920-963`,
names `core/abi.rs:274-303`. Exports read by the host: `memory`,
`__heap_ptr`, `__exc_pending`, `__exc_value`, `__invoke1`, `__async_step`
(`core/emit/mod.rs:1002-1026`).

Classification key: **(a)** must change if objects get property tables;
**(b)** stable if the named header/field stays where it is;
**(c)** independent of heap object layout.

### 5.1 Rust host (`core/run/`)

| entry point | heap reads/writes (offsets, tags) | class |
|---|---|---|
| `decode` / `decode_heap` (`core/run/value.rs:76-115`) | type byte @0 for kinds 0,1,2,3,6,7,8,9,10; unknown → `undefined` | (a) for kind 2, (b) others |
| `decode_object` (`core/run/value.rs:211-236`) | `n` @+4, entries @+8+16k (key @+0, value @+8) | **(a)** |
| `is_error_object` / `render_error` (`core/run/value.rs:420-443`) | searches decoded entries for `@@errorkind`, `name`, `message` | **(a)** (depends on entries being a flat own list incl. hidden keys) |
| `decode_string` / `decode_array` (`core/run/value.rs:160-201`) | `len` @+4, payload @+8 | (b) string/array header |
| `decode_size` (`core/run/value.rs:150-158`) | Map/Set `size` @+4 | (b) |
| `decode_promise` (`core/run/value.rs:129-148`) | `state` @+4, `value` @+8 | (b) |
| `decode_date` (`core/run/value.rs:117-127`) | `ms` @+8 | (b) |
| `print` (`core/run/harness.rs:123-149`) | full `decode` + `format_value` of the argument (recursive) | **(a)** via `decode_object` |
| `__convert` (`core/run/harness.rs:156-194`) | `decode` of the operand; ToString formats objects via `format_value`; allocates a string block (`core/run/host.rs:57-69`) | **(a)** for ToString/parseInt/parseFloat of objects (`core/run/harness.rs:172-180`); (b) for string alloc |
| `__method` (`core/run/host.rs:145-246`) | type byte @0 (array/string only); `len` @+4, elems @+8+8i (`core/run/host.rs:258-278`); allocates arrays/strings (`:411-431`, `:618-653`) | (b) array/string; **(a)** indirectly: `strict_eq_bits` decodes both operands fully (`core/run/host.rs:304-312`) and `array_join` formats object elements (`core/run/host.rs:376-384`) |
| `__promise` / `__then` (`core/run/promise.rs:165-234`) | promise block @0/@+4/@+8 (`:699-741`, `:643-673`); function type byte check (`:710-713`); generator `done` @+12 (`:434-444`) | (b) promise/function/generator headers |
| `__invoke1` / `__async_step` calls (`core/run/promise.rs:602-641`) | pass jsvals opaquely; read/clear exception globals | (c) |
| `__now` (`core/run/harness.rs:247-251`) | none | (c) |
| uncaught render (`core/run/harness.rs:292-304`) | `decode` + `format_value` of `__exc_value` | **(a)** (errors are objects) |
| OOM check (`core/run/harness.rs:51-64`, `:257-280`) | `__heap_ptr` global only | (c) |

### 5.2 TS shim (`pkg/shim.ts`, `pkg/value.ts`, `pkg/abi.ts`)

The shim duplicates the ABI numbers by hand (`pkg/abi.ts:1-12`); it only
imports `TYPE_STRING/ARRAY/OBJECT/FUNCTION/PROMISE/DATE`, `GEN_DONE_OFFSET`
and `DATE_MS_OFFSET` (`pkg/abi.ts:33-55`).

| entry point | heap reads/writes | class |
|---|---|---|
| `decode` / `decodeHeap` (`pkg/value.ts:89-133`) | type byte; kinds 0,1,2,3,8,10 only (Map/Set/Generator → `undefined`) | (a) kind 2, (b) others |
| `decodeObject` (`pkg/value.ts:185-199`) | `n` @+4, entries @+8+16k | **(a)** |
| `formatValue` object arm (`pkg/value.ts:231-234`) | prints every entry; no `@@errorkind` special case | **(a)** |
| `print` (`pkg/shim.ts:714-724`) | `formatValue(decode(...))` | **(a)** |
| `convert` (`pkg/shim.ts:726-744`) | `decode` + `formatValue`; allocates a string | **(a)** for objects |
| `methodDispatch` (`pkg/shim.ts:351-447`) | type byte, `len` @+4, elems @+8+8i (`pkg/shim.ts:829-858`); `strictEqBits` / `arrayJoinText` decode elements fully (`pkg/shim.ts:882-946`); allocs (`pkg/shim.ts:193-217`, `:305-341`) | (b), **(a)** indirectly |
| `argBytes` (`pkg/shim.ts:971-973`) | `formatValue(decode(...))` of a method argument | **(a)** indirectly |
| promise engine (`pkg/shim.ts:450-712`) | `allocPromise` / `writeSettlement` @+4/@+8 (`:472-489`); `asPromise` / `asFunction` type byte (`:795-804`); `readState`/`readValue` (`:807-818`); `readGenDone` @+12 (`:821-827`); reactions keyed by base (`:452`) | (b) |
| `hostAlloc` (`pkg/shim.ts:149-170`) | `__heap_ptr` bump | (c) (but no OOM sentinel) |

### 5.3 Summary of what the host depends on

- Only the RENDERER path (`decode_object`, `is_error_object`, and everything
  that formats a value: `print`, `__convert` ToString/parse, `join`,
  `indexOf/includes` through `strict_eq_bits`, uncaught-exception rendering)
  reads `TYPE_OBJECT` internals. No host import ever WRITES an object block
  or looks up a property by key (promises do not probe `then` on objects:
  `core/run/promise.rs:250-263` checks only the promise type byte).
- Everything else depends on: the type byte at offset 0; string/array `len`
  @+4 and payload @+8; promise `state` @+4 / `value` @+8; generator `done`
  @+12; function type byte; Map/Set `size` @+4; Date `ms` @+8; the shared
  `__heap_ptr` bump global.
- Host-held guest references: reaction lists and the microtask queue store
  raw jsvals (callbacks, settled values, generator handles) and promise base
  offsets outside linear memory (`core/run/promise.rs:57-95`,
  `pkg/shim.ts:87-112`, `pkg/shim.ts:450-452`). The Rust `method_dispatch`
  copies the whole memory per call (`core/run/host.rs:152-160`).
- `decode` recursion has no cycle guard (`core/run/value.rs:185-236`,
  `pkg/value.ts:172-199`), so a cyclic object graph rendered by `print`
  recurses without bound.

---

## 6. Design implications

### 6.1 Constraints to keep `host.rs` / `promise.rs` / `shim.ts` working until phase 4

1. Keep the single `HEAP` tag and the type byte at offset 0 of every block
   (`core/abi.rs:73-80`); all host dispatch is `memory[ptr]`
   (`core/run/value.rs:101-115`, `pkg/value.ts:115-133`). A new object kind
   should take a NEW type byte (12+) or keep `TYPE_OBJECT = 2`; either way the
   two decoders must learn it in the same change (the shim has no automatic
   sync: `pkg/abi.ts:1-6`).
2. Do not move: string/array `len` @+4 / payload @+8; promise
   `state` @+4 / `value` @+8; generator `done` @+12; Map/Set `size` @+4; Date
   `ms` @+8; function `func_idx` @+4 / `env` @+8. These are the (b)-class
   dependencies in §5.
3. Host-allocated blocks (promises, strings, arrays) are built from plain byte
   vectors with zeroed header bytes 1..3 (`core/run/host.rs:62-67`,
   `core/run/promise.rs:651-655`, `pkg/shim.ts:172-181`, `:472-477`). Any
   header word the new layout adds (mark bit, size class, shape id) must have
   a valid "all-zero" meaning, or every host allocator must be updated to
   write it.
4. Renderer contract for objects: the host needs, per object, an ordered list
   of `(string key, value)` own data properties plus (a) a way to tell hidden
   internal slots from user keys, (b) the error kind. Today both come from
   flat entries (`core/run/value.rs:211-236`, `:420-443`). The new layout must
   either keep a decodable own-entry table reachable from the object header
   (e.g. header → property-table pointer → `[n][(key, value, attrs)…]`) or
   ship host decoders in the same change (Rust and TS). Keeping `n` @+4 and
   entries at a fixed stride is the cheapest bridge but forbids inline growth.
5. Identity stability: promise reactions are keyed by block base
   (`core/run/promise.rs:89-95`, `pkg/shim.ts:452`) and Map/Set rely on a
   non-moving handle (`core/abi.rs:119-126`); objects referenced from host
   queues as callback/values must not move. Growth must therefore relocate
   only an out-of-line property table (handle stays put), as Map/Set already
   do (`core/emit/runtime.rs:2328-2335`) — unlike arrays today, whose `push`
   returns a new base (`core/emit/array.rs:170-216`).
6. Hidden internal state (prototype link, error kind, accessor marking)
   should move out of the string-keyed table into header slots or property
   attributes; otherwise every enumerator must keep the `'@'` byte test, and
   the current leaks (§3: `for-in`, `in`, object rest, renderers, computed
   access) persist. If the renderer still needs the error kind, give it a
   fixed slot or an object-kind byte the host can read without key lookup.
7. Functions need property storage too (statics, `fn.prototype`): today
   `TYPE_FUNCTION` has no slot for it (`core/abi.rs:106-108`) and the class
   value is a separate object (`core/codegen/stmt.rs:490-499`). The host only
   checks the function type byte (`core/run/promise.rs:710-713`), so adding
   a property-table pointer after `env` (+16) is host-safe.

### 6.2 Header bits/words to reserve now for precise mark-sweep GC

Facts the GC must cope with:

- Interior layouts are mixed: jsval slots (object entries, array elems, env
  cells, cell value, Map entries, promise value, generator env, accessor
  pair, function env) vs raw scalars (string bytes, Date f64, generator
  `sm_idx/state/done`, function `func_idx`, promise `state`). A tracer needs
  a per-kind trace rule; kinds 0 and 10 are leaf blocks.
- A raw f64 can look like a pointer only if it is a boxed NaN; numbers are
  canonicalized (`core/abi.rs:29-31`) so a jsval slot is a pointer iff
  `QNAN` mask + tag `0b101` (`core/emit/value.rs:109-119`). The TDZ hole
  (`UNDEFINED | 1`, `core/emit/mod.rs:219`) is a non-pointer. Function `env`
  can hold a promise pointer instead of an env (`core/emit/expr.rs:1222-1269`)
  — the tracer must treat it as a generic jsval.
- Roots outside linear memory: module-cell globals `9+n`
  (`core/emit/mod.rs:211-214`), `$exc_value`, `$cf_value`, `$__this`
  (`core/emit/mod.rs:168-196`), wasm locals/operand stack (no shadow stack
  exists), host reaction/job lists (`core/run/promise.rs:57-95`,
  `pkg/shim.ts:87-112`). Static strings below `heap_ptr_init` are permanent
  and must be skipped by sweep.
- Block size is not recoverable today (§4: in-place shrink by `pop`,
  `delete`, object rest), so a linear sweep needs an explicit size.

Recommended reservations (for `object-model.md` to decide):

- Header byte 1: GC flags (bit 0 mark; bit 1 "static/pinned" for data-segment
  and host-pinned blocks; remaining bits free). Zero = unmarked, non-static,
  so host allocators remain correct unchanged (constraint 3).
- Header bytes 2..3: reserved (e.g. size class or object-kind/shape bits);
  must stay zero-valid.
- An explicit allocation-size word (u32) for sweep — either a hidden word
  before the block base (keeps every current offset in §6.1.2 unchanged and
  needs only `__alloc` + the three host allocators to write it) or a new
  header word (moves every payload offset and breaks every (b)-class host
  dependency). The pre-base word is the host-compatible choice.
- Keep the type byte as the trace-rule selector; the new object layout should
  keep its jsval slots contiguous (header-adjacent proto/table pointers +
  table of jsvals) so its trace rule is "scan N jsval words".
- 8-byte block alignment (round in `__alloc`, pad static strings) would free
  low pointer bits and let size be expressed in words; today nothing
  guarantees it (§2 alignment note).
