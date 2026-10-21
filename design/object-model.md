# jsc object model (phase 2c design)

Status: proposal for review. Describes `main @ b1dc6b9` and the target model
that phase-2c L2s implement. Every current-state claim cites `path:line` at
`b1dc6b9`. Abbreviations: `core/` = `crates/jsc-core/src/`, `pkg/` =
`packages/jsc/src/`, `t262/` = `crates/jsc-test262/src/`.

Product stage: no backward compatibility. Every superseded fixed-shape path,
`@@`-reserved key, syntactic claim, and shim is deleted in the stage that
replaces it; nothing is kept behind a flag.

Concurrent workstreams (not on this branch; line numbers in their scope may
shift, the layouts described here are not expected to change):

- **mr9syrjv** runtime-semantics (`core/emit/**`, `core/run/**`):
  primitive-receiver property-write guard (store dispatch
  `core/emit/object.rs:1533-1591`), string property reads (read dispatch
  `core/emit/object.rs:1329-1447`), `+` coercion (`core/emit/numeric.rs:540-560`),
  `**` hang, unhandled rejections (`core/run/promise.rs`), `parseInt` radix
  (`core/run/harness.rs:172`), nullish guards on `then/catch/finally`/`next`.
- **iqxrfhn5** codegen-language (`core/codegen/**`): generator closures,
  nested-function hoisting, member destructuring, `try` completion values,
  capture gaps, `yield*`.

Phase 2c starts after both are merged to `main`; every stage below rebases
onto that `main`.

---

## 1. Current state

### 1.1 Value model

- A jsval is one NaN-boxed `i64` (`core/abi.rs:12-31`). Boxed iff
  `bits & QNAN == QNAN` (`core/abi.rs:41-43`); tags: booleans
  (`core/abi.rs:45-49`), `null` (`core/abi.rs:51-52`), `undefined`
  (`core/abi.rs:54-57`), and a SINGLE heap tag `HEAP = 0x7FFD…`, value
  `HEAP | ptr` (`core/abi.rs:73-76`). Tags `0b100`, `0b110`, `0b111` are unused
  (`core/abi.rs:24-27`).
- Every f64 rebox canonicalizes NaN through `__box_f64`
  (`core/emit/value.rs:4-29`), so the heap-pointer test
  (`QNAN` mask + tag `0b101`, `core/emit/value.rs:103-119`) is exact.
- The TDZ hole is `UNDEFINED | 1`, an internal non-value
  (`core/emit/mod.rs:216-219`).
- Heap kind = the TYPE BYTE at block offset 0 (`core/abi.rs:33-39`), read with
  `i32.load8_u` behind the pointer test (`core/emit/value.rs:125-133`,
  `:141-217`).
- Header: `[type:u8][pad:u8×3]`, `HEADER_SIZE = 4` (`core/abi.rs:78-80`). Bytes
  1..3 are written zero by host allocators (`core/run/host.rs:63-64`) and the
  data segment (`core/emit/sections.rs:382-383`) and never read.
- Blocks are not aligned: static strings are packed from `HEAP_BASE = 8`
  (`core/emit/sections.rs:430-438`, `core/emit/mod.rs:231-235`), `__alloc`
  does not round (`core/emit/runtime.rs:16-79`).

### 1.2 Heap layouts today

| kind (byte) | layout | cite |
|---|---|---|
| `TYPE_STRING` 0 | `[0][pad×3][len:u32 @4][utf8 @8]` | `core/abi.rs:93-95` |
| `TYPE_ARRAY` 1 | `[1][pad×3][len:u32 @4][elem:i64 × len @8]`, no capacity | `core/abi.rs:97-99` |
| `TYPE_OBJECT` 2 | `[2][pad×3][n:u32 @4][(key:i64, val:i64) × n @8]` | `core/abi.rs:101-104` |
| `TYPE_FUNCTION` 3 | `[3][pad×3][func_idx:u32 @4][env:i64 @8]`, 16 bytes | `core/abi.rs:106-108`, `core/emit/mod.rs:273-275` |
| `TYPE_ENV` 4 | `[4][pad×3][count:u32][cell:i64 × count]` | `core/abi.rs:110-112` |
| `TYPE_CELL` 5 | `[5][pad×3][value:i64 @4]` | `core/abi.rs:114-117` |
| `TYPE_MAP`/`TYPE_SET` 6/7 | `[k][pad×3][size:u32 @4][entries:i64 @8]` → flattened pair array | `core/abi.rs:119-133` |
| `TYPE_PROMISE` 8 | `[8][pad×3][state:i32 @4][value:i64 @8]`, host-allocated only | `core/abi.rs:135-155`, `core/run/promise.rs:643-657` |
| `TYPE_GENERATOR` 9 | `[9][pad×3][sm_idx @4][state @8][done @12][env:i64 @16]` | `core/abi.rs:157-182` |
| `TYPE_DATE` 10 | `[10][pad×3][pad×4][ms:f64 @8]` | `core/abi.rs:184-195` |
| `TYPE_ACCESSOR` 11 | `[11][pad×3][pad×4][getter @8][setter @16]` | `core/abi.rs:197-218` |

### 1.3 Fixed-shape objects

- An object is an `IrNode::Object` of `(String, IrNode)` entries
  (`core/ir.rs:542-548`) built by `lower_object`
  (`core/codegen/expr.rs:1516-1571`); keys must be identifier/string
  (`core/codegen/expr.rs:1930-1941`); spread and literal getters/setters are
  rejected (`core/codegen/expr.rs:1521-1527`, `:1556-1562`). Literal keys are
  interned static strings written as `HEAP | offset`
  (`core/emit/object.rs:56-66`).
- `__obj_get` linear-scans entries comparing keys by byte content with
  `__str_eq` (`core/emit/runtime.rs:494-654`); on an own miss, if the program
  interned `@@prototype` (`core/emit/mod.rs:1073-1077`) it rescans for
  `@@prototype` and recurses (`core/emit/runtime.rs:590-648`).
- `__obj_set` only overwrites an existing key and returns `0` on a miss
  (`core/emit/runtime.rs:656-739`); the caller throws
  `Cannot add property '<k>': dynamic properties are not supported yet` via
  `__prop_throw` (`core/emit/object.rs:1596-1615`, `core/emit/mod.rs:450-458`).
- `delete` compacts entries with `memory.copy` (`core/emit/runtime.rs:1436-1449`);
  `in` scans own keys only (`core/emit/runtime.rs:1382-1392`).
- Compile-time field scan: the instance shape of a class is the union of field
  declarations and constructor-body `this.<name> =` targets
  (`core/codegen/stmt.rs:1643-1672`), not descending into nested functions
  (`core/codegen/stmt.rs:1640-1642`), recorded by class NAME
  (`core/codegen/stmt.rs:331-336`); `new` pre-slots one `undefined` entry per
  field after `@@prototype` (`core/codegen/expr.rs:1845-1860`).
- Static-shape side table: `let`/`const` bindings initialized with an
  all-static-key literal and never assigned (`core/codegen/capture.rs:49-56`,
  `:189-198`), used ONLY for the unknown-method diagnostic
  (`core/codegen/expr.rs:1101-1110`, `:1140-1145`).
- **No property access is slot-resolved today**: `o.k`/`o[e]` lower to
  `IrNode::Index` (`core/codegen/expr.rs:97-106`) and `emit_index_node`
  dispatches on the type byte at run time (`core/emit/object.rs:1329-1447`).

### 1.4 Syntactic builtin dispatch

`lower_call` (`core/codegen/expr.rs:516-737`) claims call shapes in a fixed
order: `console.log` (`:542-544`), `Math.*` (`:549-553`,
`core/codegen/op.rs:58-93`), `push/pop` on ANY receiver (`:558-563`), array
HOFs (`:568-573`), host string/array methods (`:580-585`,
`core/codegen/op.rs:100-121`), `Object.keys/values/entries` (`:590-594`,
`core/codegen/op.rs:258-290`), `JSON.*` (`:599-603`), `Promise.resolve/reject`
(`:608-612`), `Date.now` (`:616-631`), `getTime` (`:636-647`), Date methods
with user fallback (`:652-657`, `core/emit/object.rs:624-684`),
`then/catch/finally` (`:663-668`), Map/Set methods with user fallback
(`:674-679`, `core/emit/object.rs:1112-1180`), `next` (`:684-691`), then user
methods (`:697-699`), indirect calls (`:700-716`), direct calls (`:717-724`),
conversions (`:725-730`, `core/codegen/op.rs:127-178`), else
`call to undeclared function` (`:731-735`). Non-call names: `Math.PI/E`
(`core/codegen/expr.rs:83-92`), `.length` on ANY receiver as `StrLen`
(`core/codegen/expr.rs:94-96`, emitted without a type check
`core/emit/expr.rs:1017-1036`), Map/Set `.size` intercept
(`core/emit/object.rs:1358-1380`), `new Map/Set/Promise/Date/Error`
(`core/codegen/expr.rs:1668-1750`), `instanceof` builtins
(`core/codegen/expr.rs:1607-1647`). None of `console`, `Math`, `JSON`,
`Object`, `Array`, `String`, `Error`, `globalThis`, … is a value: an
identifier read falls to `undeclared variable`
(`core/codegen/expr.rs:391-409`).

### 1.5 `@@`-reserved keys, accessors, classes, errors

| key | role | produced | consumed |
|---|---|---|---|
| `@@prototype` | prototype link (class value, instance, derived prototype) | `core/codegen/stmt.rs:440`, `:493-499`, `core/codegen/expr.rs:1856` | `core/emit/runtime.rs:590-648`, `core/emit/object.rs:253-302` |
| `@@constructor` | class constructor function | `core/codegen/stmt.rs:494` | `core/codegen/expr.rs:1851-1854`, `:445` |
| `@@errorkind` | error brand `0/1/2` (`core/abi.rs:253-272`) | `core/codegen/expr.rs:1745-1750`, `core/emit/mod.rs:390-412` | `core/emit/object.rs:303-343`, `core/run/value.rs:420-425` |
| `@@accessor` | compile-time sentinel gating accessor dispatch | `core/emit/sections.rs:680-685` | `core/emit/mod.rs:903-906` |

- Hiding is a first-byte `'@'` test (`core/emit/runtime.rs:1122-1138`) applied
  only in `Object.keys/values/entries` and `JSON.stringify`; `for-in`
  (`core/codegen/stmt.rs:1413`, `core/emit/object.rs:1481-1496`), `in`,
  object rest (`core/emit/runtime.rs:757-911`), computed access and both
  renderers (`core/run/value.rs:211-236`, `pkg/value.ts:231-234`) see them.
- Accessors: a `TYPE_ACCESSOR` value stored under a data key, only on class
  prototypes (`core/emit/object.rs:84-148`, `core/codegen/stmt.rs:384-432`);
  every member read gets a getter postlude when the program has any accessor
  (`core/emit/expr.rs:233-241`, `:601-680`).
- Classes: the class value is a `TYPE_OBJECT`
  `{@@constructor, @@prototype, ...statics}` (`core/codegen/stmt.rs:490-501`),
  so `typeof C` is `"object"`; methods live on the prototype object
  (`core/codegen/stmt.rs:366-383`); statics are not inherited
  (`core/codegen/stmt.rs:448-449`); `extends` must name an earlier class
  declaration (`core/codegen/stmt.rs:178-197`); `super` is resolved by class
  name (`core/codegen/stmt.rs:338-341`); class expressions are rejected
  (`core/codegen/expr.rs:1987`).
- Functions: no property storage (`core/abi.rs:106-108`); `new` accepts only a
  recorded class or a builtin (`core/codegen/expr.rs:1650-1698`); the
  constructor's return value is discarded (`core/emit/expr.rs:830-835`);
  `new.target` is unsupported (`core/codegen/expr.rs:243`).
- Errors: plain objects `{name, message, stack, @@errorkind}` from
  `new Error/TypeError/RangeError` (`core/codegen/expr.rs:1745-1750`) and
  engine raises (`core/emit/object.rs:2056-2131`); `name/message/stack` are
  enumerable (`DESIGN.md:1049-1050`).
- `this` travels in global `__this` (`core/abi.rs:228-235`); the uniform
  function signature is `(a0..a_{K-1}, env, argc) -> i64` with `argc` a number
  jsval (`core/abi.rs:82-91`, `core/codegen/function.rs:155-164`).

### 1.6 Host boundary today

- Six `env` imports and exports `memory`, `__heap_ptr`, `__exc_pending`,
  `__exc_value`, `__invoke1`, `__async_step` (`core/emit/mod.rs:920-963`,
  `:1002-1026`).
- Readers of `TYPE_OBJECT` internals are only the renderers: `decode_object`
  (`core/run/value.rs:211-236`), `is_error_object` (`core/run/value.rs:420-425`),
  `decodeObject` (`pkg/value.ts:185-199`) and everything that formats values
  (`print`, `__convert` ToString, `join`, `strict_eq_bits`,
  `core/run/host.rs:304-312`, `:376-384`). No host import writes an object or
  looks up a key.
- Fixed-offset dependencies: string/array `len @4`, payload `@8`
  (`core/run/host.rs:258-278`, `pkg/shim.ts:829-858`); promise `state @4`,
  `value @8` (`core/run/promise.rs:659-673`, `pkg/shim.ts:483-489`); generator
  `done @12` (`core/run/promise.rs:434-444`, `pkg/shim.ts:821-827`); Map/Set
  `size @4` (`core/run/value.rs:150-158`); Date `ms @8`
  (`core/run/value.rs:117-127`).
- Host allocation bumps the shared `__heap_ptr` directly
  (`core/run/host.rs:80-128`, `core/run/promise.rs:675-693`,
  `pkg/shim.ts:143-170`); promise reactions are keyed by block base
  (`core/run/promise.rs:89-95`, `pkg/shim.ts:450-452`).
- `pkg/abi.ts` duplicates ABI numbers by hand (`pkg/abi.ts:1-12`).
- `method_dispatch` copies the whole memory per call
  (`core/run/host.rs:152-160`).

### 1.7 Known defects the migration removes

| # | defect | evidence | removed in |
|---|---|---|---|
| DF1 | `@@` keys leak to `for-in`, `in`, object rest, computed access, renderers; `Object.keys` hides any user key starting with `@` | `core/emit/object.rs:1481-1496`, `core/emit/runtime.rs:1122-1138`, `:757-911` | S1 |
| DF2 | `.length` claimed for every receiver (object entry count, wild read on numbers) | `core/codegen/expr.rs:94-96`, `core/emit/expr.rs:1017-1036` | S1 |
| DF3 | member read loads the type byte without a pointer test | `core/emit/object.rs:1362`, `:1384` | S1 (mr9syrjv may fix first) |
| DF4 | `CallIndirect` has no callee check; `let x = {}; x()` calls funcref 0 | `core/emit/expr.rs:1284-1311` | S1 |
| DF5 | `in` ignores the prototype chain; `Object.keys` order is not integer-first | `core/emit/runtime.rs:1382-1392`, `core/emit/object.rs:54-76` | S1 |
| DF6 | renderer recursion has no cycle guard (`o.self = o` overflows the native stack) | `core/run/value.rs:185-236`, `pkg/value.ts:172-199` | S0 |
| DF7 | shim parity: `TYPEOF` tag still exported/decoded; `hostAlloc` lacks the OOM sentinel | `pkg/abi.ts:24-25`, `pkg/value.ts:102-105`, `pkg/shim.ts:158-163` | S0 |
| DF8 | `push` returns a new base: aliases keep the old block; O(n²) pushes; captured / member receivers rejected | `core/emit/runtime.rs:913-987`, `core/emit/array.rs:170-216`, `core/codegen/expr.rs:888-909` | S2 |
| DF9 | `a.foo` / `a["1"]` read `a[0]` (no ToPropertyKey) | `core/emit/object.rs:1271-1292` | S2 |
| DF10 | optional call `o?.m()` drops `this`; class identity by name for `new`/`instanceof`/`super`; ctor return ignored | `core/codegen/expr.rs:363-379`, `:1661`, `:1621`, `core/emit/expr.rs:830-835` | S3 |
| DF11 | `new Error` ignores shadowing | `core/codegen/expr.rs:1693` | S4 |
| DF12 | "any receiver" names (`push`, `map`, `indexOf`, `then`, `next`, `getTime`, …) hijack user methods | `core/codegen/expr.rs:558-691` | S5 |

---

## 2. Target model

### 2.1 Common header (every block, all phases)

The header stays 4 bytes so string offsets do not move; bytes 1..3 get
meanings whose all-zero value is the default.

| byte | field | values |
|---|---|---|
| 0 | `kind` | type byte (table 2.2) |
| 1 | `gc` | phase 4: bit0 mark; 0 = unmarked. Reserved now, always written 0 |
| 2 | `flags` | per kind (below); 0 = default |
| 3 | `class` | `TYPE_OBJECT` sub-class (2.4); 0 elsewhere |

Every block is 8-byte aligned from S0 on (`__alloc` rounds sizes up to 8;
static blobs are padded to 8). `i64` slots at offsets ≡ 0 mod 8 use `align: 3`.

### 2.2 Kinds and layouts after phase 2c

All OBJECT-FAMILY kinds (1, 2, 3, 6, 7, 8, 9, 10) share a 16-byte prefix:

| offset | size | field |
|---|---|---|
| 0 | 4 | header (`kind`, `gc`, `flags`, `class`) |
| 4 | 4 | `props: u32` → `TYPE_PROPS` block, `0` = no own named properties |
| 8 | 8 | `proto: i64` jsval (heap pointer or `null`) |
| 16 | … | kind payload |

| kind | byte | payload (from +16) | size | stage |
|---|---|---|---|---|
| `TYPE_STRING` (primitive, no prefix) | 0 | unchanged: `len:u32 @4`, bytes `@8` | `8 + len`, rounded | — |
| `TYPE_ARRAY` | 1 | `len:u32 @16`, `elems:u32 @20` → `TYPE_ELEMS` (0 when cap 0) | 24 | S2 |
| `TYPE_OBJECT` | 2 | none; wrappers (`class` 3..5) add `value:i64 @16` | 16 / 24 | S1 |
| `TYPE_FUNCTION` | 3 | `func_idx:u32 @16`, reserved `u32 @20`, `env:i64 @24` | 32 | S1 |
| `TYPE_ENV` 4, `TYPE_CELL` 5 | 4, 5 | unchanged (internal, no prefix) | — | — |
| `TYPE_MAP`/`TYPE_SET` | 6/7 | `size:u32 @16`, pad, `entries:i64 @24` | 32 | S1 |
| `TYPE_PROMISE` | 8 | `state:i32 @16`, pad, `value:i64 @24` | 32 | S1 |
| `TYPE_GENERATOR` | 9 | `sm_idx @16`, `state @20`, `done @24`, pad, `env:i64 @32` | 40 | S1 |
| `TYPE_DATE` | 10 | `ms:f64 @16` | 24 | S1 |
| `TYPE_ACCESSOR` (internal) | 11 | unchanged: `getter @8`, `setter @16` | 24 | — |
| `TYPE_PROPS` (internal, new) | 12 | table 2.3 | `16 + 16·cap` | S1 |
| `TYPE_ELEMS` (internal, new) | 13 | `cap:u32 @4`, `elem:i64 × cap @8` | `8 + 8·cap` | S2 |
| `TYPE_SYMBOL` (reserved) | 14 | phase 3 | — | — |

Object-family `flags` (byte 2): bit0 `NOT_EXTENSIBLE`, bit1 `WATCHED`
(intrinsic whose mutation sets a dirty bit, 3.3). Per kind, bits 2..7:
function — bit2 `LAZY` (name/length/prototype not materialized), bit3
`CONSTRUCTOR`, bit4 `CLASS_CTOR` (throws when called without `new`), bit5
`SELF_ALLOC` (derived class or builtin: allocates its own `this`); array —
bit2 `SPARSE`, bit3 `LENGTH_READONLY` (phase 3). String `flags`: bit0
`INTERNED`, bit1 `ARRAY_INDEX` (canonical array-index spelling), bit2
reserved for the phase-3 encoding (6).

### 2.3 Property table (`TYPE_PROPS`)

| offset | size | field |
|---|---|---|
| 0 | 1 | kind `12` |
| 2 | 1 | `tflags`: bit0 `HAS_INDEX_KEYS`, bit1 `HAS_NON_ENUM` |
| 4 | 4 | `count: u32` (slots used, including tombstones) |
| 8 | 4 | `cap: u32` |
| 12 | 4 | `live: u32` |
| 16 + 16·k | 4 | `key: u32` → interned string (phase 3: or `TYPE_SYMBOL`); `0` = tombstone |
| 20 + 16·k | 4 | `attrs: u32` |
| 24 + 16·k | 8 | `value: i64` (data value, or a `TYPE_ACCESSOR` pointer when `ACCESSOR`) |

`attrs`: bit0 `WRITABLE`, bit1 `ENUMERABLE`, bit2 `CONFIGURABLE`, bit3
`ACCESSOR`. Data properties created by assignment or literals are `0b0111`.
For an accessor property `WRITABLE` is ignored and the `TYPE_ACCESSOR` pair
holds getter/setter (`undefined` when absent). This is exactly the state
space of a spec property descriptor, so phase-3 `Object.defineProperty` /
`getOwnPropertyDescriptor` / `freeze` need no relayout.

- **Keys** are interned: two keys are equal iff their `u32` pointers are
  equal. Lookup is one `i32.load` + `i32.eq` per entry (today: a `__str_eq`
  call per entry).
- **Insertion order** is slot order. **Delete** writes `key = 0`, sets the
  value to `undefined`, decrements `live`; later slots keep their index (so a
  predicted slot index stays valid until the table is rebuilt).
- **Growth**: when `count == cap`, allocate `cap' = max(4, 2·live)` (or the
  same `cap` if tombstones free half), copy live entries in order, rewrite the
  owner's `props`. The owner handle never moves. Literals and class instances
  allocate `cap` = static key count / field-scan count up front; `{}` and
  function objects start with `props = 0`.
- **Lookup structure**: linear scan only in 2c (open question 7).

### 2.4 `TYPE_OBJECT` classes (byte 3)

`0` ordinary, `1` ERROR (has [[ErrorData]]; kind is the prototype),
`2` ARGUMENTS (S3), `3/4/5` Boolean/Number/String wrapper with
`[[PrimitiveValue]] @16` (S5), `6` GLOBAL (S4). `@@errorkind`,
`@@prototype`, `@@constructor`, `@@accessor` and the `'@'` filter are deleted
in S1.

### 2.5 Key interning

- `__intern(str: i32) -> i32` returns the canonical block for the string's
  content and sets its `INTERNED` flag; `__intern_find(str) -> i32` returns `0`
  on a miss without inserting.
- Table: open addressing over `u32` slots, FNV-1a of the bytes, load ≤ 1/2.
  The compiler emits the initial table into the data segment containing every
  static string (all literals are already deduplicated by content,
  `core/emit/sections.rs:392-439`, and get `INTERNED` at compile time); growth
  copies it to the heap.
- Literal-key sites pass the constant `u32` key: zero runtime interning.
  Computed string keys test `INTERNED` (1 load + 1 `and`) and only then call
  `__intern_find` (get/has/delete; a miss means "absent everywhere") or
  `__intern` (set/define).
- ToPropertyKey of a number: integers `0 ≤ i < 2^32-1` are formatted in-guest
  into digits; other numbers through the host ToString (`__convert`) until the
  phase-4 self-contained module. On arrays and strings an integer number key
  never becomes a string (element path).
- `ARRAY_INDEX` is set at intern time when the content is a canonical array
  index; the table's `HAS_INDEX_KEYS` is set when such a key is added.

### 2.6 Algorithms

Helpers live in a new `core/emit/props.rs` and replace `__obj_get`,
`__obj_set`, `__obj_has`, `__obj_delete`, `__obj_keys/values/entries`,
`__obj_rest` (`core/emit/runtime.rs:494-1449`) with no compatibility wrappers.
Receivers are jsvals; keys are `u32` interned refs.

**`__get(recv, key) -> i64`** (OrdinaryGet with exotic own hooks):

1. If `recv` is not a heap pointer or is a string: `undefined`/`null` →
   TypeError (existing nullish guard, `core/emit/object.rs:1953-1959`);
   string → own `length` / array-index keys, else `O =
   %String.prototype%`; number → `%Number.prototype%`; boolean →
   `%Boolean.prototype%` (S4; before S4 → `undefined`).
2. Else `O = recv`. Loop: exotic own step for `O.kind` (array: index keys and
   `length`; function with `LAZY`: materialize first); scan `O.props`; on hit,
   data → value, accessor → call getter with `this = recv` (or `undefined`
   when no getter); on miss `O = O.proto`; `null` → `undefined`.

**`__set(recv, key, v)`** (OrdinarySet, strict): primitive receiver →
TypeError (mr9syrjv's guard). Own hit: writable data → store; non-writable →
TypeError; accessor → setter or TypeError. Own miss: walk `proto`: the first
hit decides (accessor → setter with `this = recv`; non-writable data →
TypeError; writable data → fall through). Then add an own data property
(`0b0111`) if extensible, else TypeError. A write to a `WATCHED` object sets
its dirty bit (3.3).

**`__has(obj, key)`**: own (exotic + table) then `proto` walk.
**`__delete(obj, key)`**: own only; non-configurable → TypeError (strict),
else tombstone; returns `true`. **`__define(obj, key, v, attrs)`**: internal
CreateDataProperty / accessor install used by literals, classes, builtins and
(phase 3) `defineProperty`.

**Own-key order** (`__own_keys(obj, mode)`, used by `Object.keys/values/entries`,
`for-in`, `JSON.stringify`, object rest and spread, renderers):
integer keys ascending (array elements `0..len` skipping holes, then table
keys with `ARRAY_INDEX` sorted numerically, only when `HAS_INDEX_KEYS`), then
string keys in slot order, then symbols (phase 3). `mode` filters
enumerable/string-only. `for-in` walks the `proto` chain, emits enumerable
string keys not already seen (shadowing), snapshotted at loop entry.

### 2.7 Prototype slot

`proto` is an ordinary jsval slot (`null` or object). Class instances get
`C.prototype`, literals `%Object.prototype%` (S4; `null` until the realm
exists), functions `%Function.prototype%`, arrays `%Array.prototype%`, etc.
Phase 3 `Object.setPrototypeOf` / `__proto__` write this slot after a cycle
check.

---

## 3. Fast paths

### 3.1 What is statically resolved today (the must-not-regress set)

From `notes-codegen.md` section 4.1 (no property access is slot-resolved,
1.3): (1) direct calls to top-level declarations
(`core/codegen/expr.rs:717-724`, `core/emit/expr.rs:57-74`); (2) `Math.*`,
`Math.PI/E`, `**`; (3) `console.log`; (4) conversions, `JSON.*`,
`Object.keys/values/entries`, `Promise.*`, `Date.now`,
`new Map/Set/Promise/Date/Error`; (5) array/string method families resolved
by name to a fixed helper; (6) `.length` as one `i32.load`; (7) class
instance allocation with field-scan pre-sizing; (8) `instanceof` with a
builtin right-hand side.

### 3.2 Mapping each specialization onto the generic path

Two program-level facts computed once by codegen (whole-program syntactic scan):

- **sealed(N)** for a global name `N`: `N` resolves to the global object (not
  shadowed), is never assigned, and the program contains no `globalThis`,
  top-level `this`, or `delete` of a global. Then `N` compiles to the
  intrinsic's constant address (`i64.const`), and `N.m` for a namespace
  (`Math`, `JSON`, `console`, `Object`, `Promise`, `Date`) is the builtin
  identity if additionally `N` never escapes (only `N.member` reads, no member
  writes on `N`).
- **intrinsics-sealed**: the program contains none of `prototype` (as a
  member name), `constructor` (member read), `__proto__`, `getPrototypeOf`,
  `setPrototypeOf`, `defineProperty`, `defineProperties`, `assign`,
  `Reflect`. Then no builtin prototype can be mutated and every dirty-bit
  test below is folded to "clean" at compile time.

| specialization | fast path (no runtime guard) | runtime guard otherwise | generic fallback |
|---|---|---|---|
| direct `f(..)` | unchanged: declarations are not assignable (`core/codegen/expr.rs:1256-1262`) | none | — |
| `Math.*`, `JSON.*`, `Object.*`, `Promise.*`, `Date.now`, conversions, `console.log` | `sealed(N)`: emit exactly today's inline code | load `N.m`, compare with the builtin function constant, inline on equal | `__get` + `__call` |
| `push/pop`, HOFs, string/array host methods | receiver kind byte `ARRAY`/`STRING` (already tested inside today's helpers) **and** intrinsics-sealed | kind byte + `props == 0` (no own shadowing) + `Array.prototype` dirty bit clear | `__get` method from receiver, `IsCallable`, call |
| `then/catch/finally`, `next`, `getTime`, Date/Map/Set methods | kind byte `PROMISE`/`GENERATOR`/`DATE`/`MAP`/`SET` + intrinsics-sealed | + `props == 0` + dirty bit | generic method call (also the user-method fallback, `core/emit/object.rs:1112-1180`) |
| `.length` | kind `STRING` → `len @4`; kind `ARRAY` → `len @16` (own exotic, no guard needed) | — | `__get(recv, "length")` |
| Map/Set `.size` | kind `MAP`/`SET` + intrinsics-sealed | + `props == 0` + dirty bit | getter on `%Map.prototype%` (S5) |
| `o.k` on a literal or class instance | — | **predicted-slot check** (optional, not required in 2c): `props.entries[i].key == K` → value; `i` from the literal order / field scan | `__get` |
| `new C()` pre-sizing | field-scan count becomes the initial `props.cap` | — | `__set` adds outside the scan instead of throwing |
| `o.m()` on a class instance | — | none beyond `__get` | `__get` + `IsCallable` |
| `x instanceof C` (class) | `C.prototype` is non-writable for classes, so the chain walk against the loaded `C.prototype` stays exact | — | `OrdinaryHasInstance` (S3) |
| `x instanceof Error/TypeError/…` | sealed(name): walk `proto` against the static prototype address | — | `OrdinaryHasInstance` |

### 3.3 Dirty bits

One `i32` global `$__intrinsics_dirty`, one bit per watched intrinsic
prototype/namespace (`Object.prototype`, `Array.prototype`,
`String.prototype`, `Number.prototype`, `Boolean.prototype`,
`Function.prototype`, `Promise.prototype`, `Map.prototype`, `Set.prototype`,
`Date.prototype`, generator prototype, `Math`, `JSON`, `console`). The bit
index lives in the watched object's static info; `__set`, `__define`,
`__delete` and phase-3 `setPrototypeOf` set it when the target has `WATCHED`.
Bits are never cleared.

### 3.4 The no-regression rule and its guard

**Rule**: a site in the 3.1 set must not get more instructions (static) nor
execute more (dynamic) than at `b1dc6b9`, unless the stage's PR records the
increase with a reason and the reviewer accepts it (open question 4).

**Guard** (lands in S0, before any layout change):

- Test `crates/jsc-core/tests/opcount.rs`, corpus
  `crates/jsc-core/tests/opcount/*.js` (one program per 3.1 row: `fib` via
  direct calls, `Math.*`, `console.log`, array HOFs + `push`, string methods,
  Map/Set, class with methods/accessors/`extends`, object-literal reads,
  `for-in`, `JSON.stringify/parse`), snapshot
  `crates/jsc-core/tests/opcount/snapshot.tsv` with columns
  `program user_ops call_indirect prop_calls host_calls fuel reason`.
- Static columns: compile in-process (`core/frontend.rs:33`,
  `core/codegen/mod.rs:52`, `core/emit/mod.rs:888`) and walk the code section
  with `wasmparser` (dev-dependency, `crates/jsc-core/Cargo.toml:27-28`;
  operator walking as in `core/emit/tests.rs:385-386`). `user_ops` counts
  operators in `main` and user functions only (helpers are shared and tracked
  by their own row).
- Dynamic column: `fuel` = executed wasm instructions from wasmtime fuel
  metering (`Config::consume_fuel`), deterministic for a pinned wasmtime
  (`crates/jsc-core/Cargo.toml:25`); the run harness builds its engine with
  `Engine::default()` (`core/run/harness.rs:119`), so S0 adds a test-only
  engine constructor with fuel enabled.
- Flow mirrors the test262 gate (`t262/baseline.rs:28-45`, CLI flags
  `t262/main.rs:18`): the test fails on any increase and on any decrease
  (keeps the snapshot tight); `JSC_OPCOUNT_UPDATE=1 cargo nextest run -p
  jsc-core opcount` rewrites it. It runs inside the existing `cargo nextest run
  --workspace` CI step (`DESIGN.md:927-930`).
- Structural complement: codegen unit tests assert the fast-path IR nodes
  survive (`Call`, `MathCall`, `ConsoleLog`, `ArrayHof`, …) as in
  `core/codegen/tests.rs:575-583`.
- Not a wall-clock benchmark: CI timing is not stable; fuel is.

---

## 4. Builtins as real objects

### 4.1 Realm image

All intrinsics are **static blocks in the data segment**, laid out by the
compiler like interned strings (below `heap_ptr_init`,
`core/emit/sections.rs:428-439`): object handles, their `TYPE_PROPS` tables
and builtin `TYPE_FUNCTION` objects (`env = undefined`, `func_idx` of a
uniform-ABI wasm builtin). Consequences: zero startup cost, every intrinsic
address is a compile-time constant (no globals, no lookup), user mutation
works (a static table that overflows is copied to the heap like any other).
The image is generated from one registry `core/emit/realm.rs`:
`(intrinsic, key, attrs, value: Method(builtin fn) | Value | Accessor(get, set))`,
the same registry the roadmap-3 JS prelude will feed later
(`DESIGN.md:1109-1110`). Builtin bodies live in `core/emit/builtins/<family>.rs`,
wrapping today's helpers (`__array_push`, `__map_*`, Date methods, host
`__method` ops) with the uniform ABI (`this` from `__this`, args from slots).

### 4.2 Intrinsics and wiring

| intrinsic | proto | contents in 2c | stage |
|---|---|---|---|
| `%Object.prototype%` | `null` | `hasOwnProperty`, `toString`, `valueOf`, `constructor` | S4 |
| `%Function.prototype%` | `%Object.prototype%` | `call`, `apply` (S4), `bind` (phase 3), `constructor` | S4 |
| `%Array.prototype%` | `%Object.prototype%` | today's array methods (S4: `map` generic over array-likes; rest S5) | S4/S5 |
| `%String.prototype%`, `%Number.prototype%`, `%Boolean.prototype%` | `%Object.prototype%` | today's string methods; `toString`, `valueOf` | S5 |
| `%Error.prototype%` | `%Object.prototype%` | `name: "Error"`, `message: ""`, `toString` | S1 (data), S4 (ctor) |
| `%TypeError/RangeError/ReferenceError/SyntaxError/EvalError/URIError.prototype%` | `%Error.prototype%` | `name`, `message` | S1 (first two), S4 |
| `%Map/Set/Promise/Date.prototype%`, generator prototype | `%Object.prototype%` | today's methods; `size` getter | S5 |
| `Object, Function, Array, String, Number, Boolean, Error…, Map, Set, Promise, Date` | `%Function.prototype%` | constructor functions; `prototype` (non-writable), statics (`Object.keys/values/entries`, `Array.isArray`, `Promise.resolve/reject`, `Date.now`) | S4 |
| `Math`, `JSON`, `console` | `%Object.prototype%` | today's members | S4 |
| global object (`class GLOBAL`) | `%Object.prototype%` | every global above, `globalThis`, `undefined`/`NaN`/`Infinity` (non-writable) | S4 |

### 4.3 Global bindings

Resolution: lexical scopes → top-level declarations (module cells,
`core/codegen/mod.rs:166-179`) → global object. An unresolved identifier
compiles to the constant intrinsic when `sealed(N)`, else to
`__get_global(key)` = HasProperty + Get on the global object with a
**runtime** `ReferenceError` on miss; `typeof N` never throws; assignment to
an unresolvable name throws `ReferenceError` (strict). Top-level `this` is the
global object in scripts. Top-level `let/const/function` remain module cells,
not global-object properties (open question 6).

### 4.4 Functions as objects

- Every function object carries the 16-byte prefix (`props`, `proto =
  %Function.prototype%`). Closures are created with `LAZY` set and `props = 0`;
  the first property operation of any kind calls `__fn_materialize`, which
  defines `length`, `name` (from a static per-function info table indexed by
  `func_idx`: `[name: u32 key][length: u32]`, exported as `__fn_info` for the
  renderer) and, for `CONSTRUCTOR` functions, `prototype` = a fresh object
  `{constructor: F}` (non-enumerable) whose `proto` is `%Object.prototype%`.
  Arrows and methods are not `CONSTRUCTOR` and get no `prototype`.
- `CONSTRUCTOR` is set for `function` declarations/expressions and classes;
  `CLASS_CTOR` for classes; `SELF_ALLOC` for derived classes and builtins.

### 4.5 Construct, `F.prototype`, `new.target`

`new X(args)` lowers to `__construct(X, args…)`:

1. `IsConstructor(X)` (function with `CONSTRUCTOR`) else TypeError.
2. Unless `SELF_ALLOC`: `this = ` new object with `proto = Get(X, "prototype")`
   if that is an object, else `%Object.prototype%`.
3. Set `__this = this`, global `$__new_target = X`, call `X` with the
   construct marker: `argc` is passed as `-(n + 1)` (functions that read
   `argc` or `new.target` decode it; nobody else pays, so plain and direct calls
   are unchanged).
4. Result: the return value if it is an object, else `this` (fixes DF10).

Callees: a `CLASS_CTOR` called without the marker throws TypeError;
`new.target` reads `$__new_target` when the marker is present, else
`undefined`. A derived class constructor (`SELF_ALLOC`) starts with `this` in
TDZ; `super(args)` = `__construct_with(Parent, args, new.target)` and binds
`this`; field initializers run right after `super` returns (spec order).
Classes become ordinary function objects: `C.prototype` (non-writable),
`C.prototype.constructor = C`, statics as own properties of `C`,
`C.[[Prototype]] = Parent` (statics inherited), class identity by binding,
class expressions lowered like declarations (S3). Constructor field
initializers use `__define` (CreateDataProperty) rather than `__set`
(open question 8).

### 4.6 `instanceof`

`OrdinaryHasInstance(C, x)`: `IsCallable(C)` else TypeError; walk `x.proto`
against `Get(C, "prototype")`. Any expression is a valid right-hand side
(fixes the RHS restriction, `core/codegen/expr.rs:1607-1647`).
`Symbol.hasInstance` is phase 3.

### 4.7 Errors and `extends Error`

Error objects: `class = ERROR`, `proto` = the kind's prototype, own
`message` (only if provided) and `stack` (both non-enumerable). `name` is
inherited. Engine-raised errors (`core/emit/object.rs:2056-2131`,
`core/emit/mod.rs:390-412`) build the same shape against the static
prototypes. `Error(m)` without `new` constructs too. `Error` is a
`SELF_ALLOC` builtin that allocates with `proto = new.target.prototype`, so
`class E extends Error` works through 4.5 (S4). Other builtin parents
(Map, Set, Promise, Date, Array) use the same mechanism once their
constructors honour `new.target` (S4; Iterator helpers are phase 3).

---

## 5. Arrays

| block | offset | field |
|---|---|---|
| handle `TYPE_ARRAY` (24 B) | 0..16 | object prefix (`props`, `proto = %Array.prototype%`) |
| | 16 | `len: u32` |
| | 20 | `elems: u32` → `TYPE_ELEMS`, `0` when `cap = 0` |
| store `TYPE_ELEMS` | 4 | `cap: u32` |
| | 8 + 8·i | element jsval `i`, or `HOLE` |

- **Identity**: the handle never moves; only the store is reallocated (the
  Map/Set pattern, `core/abi.rs:119-126`). `push` mutates in place, so the
  receiver may be any expression (captured bindings, `o.xs.push(v)`,
  parameters); the rebinding in `core/emit/array.rs:170-216` and the
  `AccessKind::Plain` restriction (`core/codegen/expr.rs:888-909`) are deleted.
- **Amortized push**: if `len == cap`, new store with `cap' = max(4, 2·cap)`
  (or the requested size for `new Array(n)` / a `length` write), `memory.copy`
  `len` slots, repoint `elems`. Literals allocate `cap = len`. `filter`,
  spread, `__gen_to_array`, Map/Set backing arrays inherit the fix.
- **Growth by index**: `a[i] = v`, `i ≥ len`: dense when
  `i < max(2·cap, len + 1024)`: fill `len..i` with `HOLE`, `len = i + 1`.
  Larger gaps: set `SPARSE`, store the element as an `ARRAY_INDEX` key in
  `props`, `len = i + 1`; a `SPARSE` array takes the slow path for every
  element op and never converts back (open question 1).
- **Holes**: `HOLE = UNDEFINED | 1`, the TDZ bit pattern
  (`core/emit/mod.rs:216-219`), which no JS value produces. A read that loads
  `HOLE` continues on `proto` (fast path: `undefined` when the
  `Array.prototype`/`Object.prototype` dirty bits are clear); `in`,
  `hasOwnProperty`, `forEach`, `Object.keys` skip it. Literal elisions
  (rejected today, `core/codegen/expr.rs:1473-1480`) lower to `HOLE`.
- **`length`**: `length` is an own exotic, non-enumerable property. Writes
  route through `__array_set_length`: ToUint32 mismatch → RangeError;
  `n < len` overwrites `n..len` with `HOLE` (keeps regrowth and GC tracing
  correct) and sets `len`; `n > len` within the dense bound fills holes, else
  `SPARSE`. The compile-time rejections (`core/codegen/expr.rs:1288-1305`,
  `:1426-1438`) are deleted.
- **Keys**: number keys go to the element path; canonical index strings
  (`ARRAY_INDEX`) too (fixes DF9); everything else to `props`. `a.foo = 1` is
  an ordinary property add.
- **Host**: host-built arrays (`array_slice`, `string_split`,
  `core/run/host.rs:411-431`, `:618-653`) are built through the exported
  `__new_array` (7).

---

## 6. Strings and primitives

| concern | 2c (now) | phase 3 |
|---|---|---|
| property get on a primitive | kind dispatch in `__get` (2.6): string own `length`/index, else `%String.prototype%`; number/boolean → their prototypes; no wrapper allocated | Symbol receivers |
| `this` in a builtin/user method called on a primitive | the primitive itself (strict) | — |
| property set on a primitive | TypeError (strict; mr9syrjv lands the guard) | — |
| wrappers `new String/Number/Boolean`, `Object(x)` | `TYPE_OBJECT` classes 3..5 with `[[PrimitiveValue]] @16`; String wrapper exposes index/`length` as exotic own props (S5) | Symbol wrapper |
| `"abc".foo` | `undefined` via `String.prototype` (mr9syrjv fixes first) | — |
| encoding | UTF-8 bytes, `len` = bytes (`core/abi.rs:93-95`, `DESIGN.md:1014-1016`) | Latin-1/UTF-16 (`DESIGN.md:1107-1108`): `flags` bit2 = encoding, `len` = code units |

Interaction with the property model: keys compare by interned identity, so
the encoding matters only at intern time; phase 3 must canonicalize to
Latin-1 when every unit is `< 0x100` so equal code-unit sequences intern to
one key. `ARRAY_INDEX` detection and the in-guest integer formatter (2.5)
are encoding-agnostic for ASCII digits. Ropes for `+=` would take a reserved
kind number; they are not designed here.

---

## 7. Host boundary until phase 4

Principle: **the host reads layouts but never builds object-family blocks by
hand**. S0 exports guest constructors (`__alloc` rounded to 8,
`__new_string(len)`, `__new_array(len)`, `__new_promise()`), and the Rust host
and shim call them instead of bumping `__heap_ptr` (deleting `heap_reserve`,
`host_alloc`, `heap_alloc`, `hostAlloc`; `core/run/host.rs:80-128`,
`core/run/promise.rs:675-693`, `pkg/shim.ts:143-170`). wasmtime and JS both
allow re-entering an export from an import. Layout knowledge then lives in
one place (the emitter) plus read-only decoders.

| host entry | change | stage |
|---|---|---|
| `decode_object` / `decodeObject` (`core/run/value.rs:211-236`, `pkg/value.ts:185-199`) | walk `props` (skip tombstones, non-enumerable, accessors render as `[Getter/Setter]`), order per 2.6; prefix with the constructor name found via `proto.constructor.name` (`A { x: 1 }`) | S1 |
| `is_error_object` / `render_error` (`core/run/value.rs:420-443`), shim `formatValue` (`pkg/value.ts:231-234`) | `class == ERROR`; `name`/`message` via a read-only data-property chain lookup (bytes compare, no interning needed host-side) | S1 |
| function rendering | name from `__fn_info` | S1 |
| promise (`core/run/promise.rs:643-741`, `pkg/shim.ts:472-489`, `:807-818`) | allocate via `__new_promise`; `state @16`, `value @24` | S0 (alloc), S1 (offsets) |
| generator `done` (`core/run/promise.rs:434-444`, `pkg/shim.ts:821-827`) | `@24` | S1 |
| Map/Set `decode_size`, Date `decode_date` (`core/run/value.rs:117-158`) | `size @16`, `ms @16`; shim learns Map/Set (today renders `undefined`, `pkg/value.ts:115-133`) | S1 |
| `__method` array reads (`core/run/host.rs:258-278`, `pkg/shim.ts:829-858`) | `len @16`, elements via `elems` store; allocate via `__new_array` | S0 (alloc), S2 (layout) |
| renderer recursion (`core/run/value.rs:185-236`, `pkg/value.ts:172-199`) | visited set, prints `[Circular *1]` | S0 |
| `pkg/abi.ts` drift | a Rust test parses `pkg/abi.ts` and asserts every exported constant equals `core/abi.rs` (removes `TYPEOF`, `pkg/abi.ts:24-25`) | S0 |
| `__convert` / `strict_eq_bits` / `array_join` | go through the new decoders; no direct layout reads | S1 |
| promise identity (`core/run/promise.rs:89-95`) | unchanged: handles never move | — |

Stable across 2c: the single heap tag, type byte @0, string `len @4` /
bytes `@8`, the export set plus the new constructors. Host-held jsvals
(reaction lists, job queue) stay valid because no block moves.

---

## 8. GC (design only; implementation phase 4)

- **Algorithm**: precise, non-moving, stop-the-world mark-sweep. Non-moving
  because handles are identity (promise reactions keyed by base,
  `core/run/promise.rs:89-95`) and emitted code holds raw `i32` bases.
  No write barriers (non-incremental, non-generational); if that changes,
  `__set`/`__define`/element stores/cell stores are the only heap-store sites.
- **Allocation**: segregated size classes (16..256 by 16, then powers of two
  to 2 KiB) in 64 KiB pages with a per-page class header (size derivable from
  the page, so no per-block size word), large objects in a list with a 16-byte
  pre-header. `__alloc` collects when the allocation budget since the last
  collection is spent, then grows memory; the 512 MiB cap and OOM sentinel
  stay (`core/abi.rs:368-377`). The host allocates only through exports (S0).
- **Roots**: a shadow stack in linear memory (`$__sp`, below the heap): each
  function reserves slots for jsvals live across a safepoint (any call that
  may allocate), clears them on entry, pops on every exit including the
  exception paths; raw `i32` base locals are re-derived from spilled jsvals
  after safepoints, and operand-stack-held pointers across allocating calls
  (`core/emit/array.rs:40-67`, `core/emit/numeric.rs:550-558`) are spilled
  first. Globals: `$exc_value`, `$cf_value`, `__this`, `$__new_target`, module
  cells (`core/emit/mod.rs:211-214`). The static region (realm image and
  data-segment strings) is a root region: every static object-family block is
  traced, never swept (address `< heap_start`).
- **Host roots**: none if the promise engine moves into wasm first (roadmap
  4 before 5, `DESIGN.md:1111-1115`); otherwise a `__gc_roots` host import
  (open question 3).
- **Tracing (NaN-boxing)**: a jsval slot is a pointer iff the exact QNAN+tag
  test holds (`core/emit/value.rs:103-119`); `HOLE`/TDZ are not pointers. Raw
  `u32` references (`props`, `elems`, property keys) are traced by kind:

| kind | children |
|---|---|
| string, date-payload | none (date traces the prefix only) |
| object-family prefix | `props`, `proto` |
| array | prefix + `elems` store |
| `TYPE_ELEMS` | slots `0..len` of the owning array (sweep uses `cap`) |
| `TYPE_PROPS` | per live entry: `key`, `value` |
| function | prefix + `env` |
| env / cell / accessor | cells / value / getter, setter |
| Map/Set, promise, generator | prefix + `entries` / `value` / `env` |

- **Intern table**: weak; sweep clears entries whose string is unmarked.
- **What 2c reserves now**: header byte 1 (`gc`, written 0), 8-byte alignment
  (S0), every internal store is a headered block (`TYPE_PROPS`, `TYPE_ELEMS`,
  `TYPE_ACCESSOR`, env, cell), every reference at a kind-known offset, host
  allocation only through exports (S0), and truncation writing `HOLE` (5).

---

## 9. M1: official `sta.js` + `assert.js` compile unchanged

Runner today: jsc's own shim harness (`t262/main.rs:19`, `:120-134`), two
lexical rewrites (`t262/assemble.rs:7-23`), `Test262Error` as a class
(`tests/test262/harness/sta.js:2-14`). Harness line numbers below refer to
the official files in `.tmp/test262/harness/` (test262 @ `2e0a5676`,
`scripts/test262-fetch.sh:5`); probe results are from `notes-test262.md`
4.2-4.4.

| construct (official harness) | blocker today | delivered by |
|---|---|---|
| `function Test262Error(message) { … this.message = … }` (`.tmp/test262/harness/sta.js:13-16`) | compiles | — |
| `this instanceof Test262Error` (`sta.js:14`) | RHS must be a class/builtin | S3 (4.6) |
| `new Test262Error(message)` (`sta.js:14`, `:23`, `assert.js:61`) | `unsupported constructor` (`core/codegen/expr.rs:1693-1698`) | S3 (4.5) |
| `Test262Error.prototype.toString = function…` (`sta.js:18-20`) | `Cannot add property` | S1 (props on functions) + S3 (`F.prototype`) |
| `Test262Error.thrower = …`, `assert._isSameValue = …`, every `assert.X = function` (`assert.js:64`, `:74`, `:95`, `:111`, `:147`, `:178-184`) | `Cannot add property` | S1 |
| `assert._toString(…)`, `var format = compareArray.format; format(x)` (`assert.js:59`, `:162-163`) | function property reads return garbage | S1 |
| `typeof JSON !== "undefined" ? JSON.stringify(v) : …` (`assert.js:27`) | `undeclared variable: JSON` | S4 (4.3) |
| `Object.prototype.toString.call(value)` (`assert.js:47`) | `undeclared variable: Object` | S4 (`Object.prototype.toString`, `Function.prototype.call`) |
| `thrown.constructor !== expectedErrorConstructor` (`assert.js:130`) | `.constructor` not the class; builtin errors not values | S3 (`constructor`) + S4 (error constructors) |
| `expectedErrorConstructor.name`, `thrown.constructor.name` (`assert.js:131-132`, `:143`) | `.name` undefined/garbage | S1 (lazy `name`) |
| `Array.prototype.map.call(arrayLike, String)` (`assert.js:179`) | `undeclared variable: String` / `Array` | S4 (`String` value, generic `map` over array-likes) |
| `String(new Test262Error(..))`, `"" + err`, `message += …` conversions of harness objects | ToString ignores user toString/valueOf | S4 (OrdinaryToPrimitive) |
| `$DONOTEVALUATE`, `typeof` checks, `switch`, `try/catch`, `message += ' '` | compile today | — |

**M1 lands at the end of S4**, which also switches the runner:
`HARNESS_DIR` → the official `.tmp/test262/harness`, delete
`tests/test262/harness/`, both rewrites, `THROWS_SHIMMED` and their tests
(`t262/assemble.rs:7-23`, `:43-117`, `:119-203`), make the `shimmed` set
(`t262/main.rs:142`) "includes that compile", update the outcome matcher
comment (`t262/outcome.rs:9-17`) now that uncaught `Test262Error` instances
render with their constructor name, and regenerate the baseline
(`--update-baseline`): tests can flip both ways.
`doneprintHandle.js` (needs a host `print`, async runner) and
`propertyHelper.js` (needs phase-3 reflection) stay out of M1.

---

## 10. Staged migration plan (phase 2c)

**Baseline** (`notes-test262.md` section 1, run at `b1dc6b9`): 5197 pass /
22992 eligible (22.6 %), 47665 total; 16714 compile-error, 1081 runtime fail,
0 timeouts. Checked-in baseline: 5195 (`tests/test262/history.tsv:6`).

**Estimate method**: per cluster, `count × yield` from the
`notes-test262.md` section 3 table (yields assume earlier stages are in),
assigned to the stage that removes the cluster's first blocker. Splits:
A1 (2595 × 35 % ≈ 900) 40 % to S4 (constructors/namespaces/prototypes as
values) and 60 % to S5 (method bodies); F1 (763) split by its 533
`Array.prototype.X.call(arrayLike)` tests (≈ 265 at S5) vs 230 others
(× 50 % ≈ 115 at S1); E3 literal 160 × 50 % = 80 (S1) vs static 30 × 50 % = 15
(S3); A4 ≈ 40 split 10 (S4) / 30 (S5); G9 ≈ 75 split 25 (S1) / 50 (S5).
Added beyond the notes' attribution: G9 array holes 133 × 50 % ≈ 65 and
captured `push` 60 × 80 % ≈ 50 (S2). B1/B2 (≈ 1030) are phase 3
(`defineProperty` family) and excluded.

| stage | goal | write scope (one L2) | depends on | acceptance | est. test262 |
|---|---|---|---|---|---|
| **S0** groundwork & gates | opcount/fuel snapshot gate; 8-byte aligned `__alloc`; header bytes 1..3 defined (all zero); exported guest constructors and host/shim switched to them; renderer cycle guard; `abi.ts` drift test; shim OOM parity | `core/emit/{runtime,mod,sections}.rs`, `core/run/**`, `pkg/{abi,shim,value}.ts`, `crates/jsc-core/tests/opcount*`, `core/abi.rs` | — | repo gate + web gate green; snapshot committed; `o.self = o` prints `[Circular *1]`; test262 `--check` 0 regressions | ≈ 0 |
| **S1** property tables & proto slot | 2.2 prefix for objects/functions/Map/Set/Promise/Generator/Date; `TYPE_PROPS`; interning; `__get/__set/__has/__delete/__define/__own_keys`; dynamic add/delete; computed and numeric literal keys; literal getters/setters; class value = constructor function with `prototype`, statics; errors via static prototypes + `class ERROR`; `.length` kind dispatch; `IsCallable` on `CallIndirect`; delete all `@@` keys, `object_shapes`, G1b diagnostic (`core/codegen/expr.rs:1140-1145`); host decoders | `core/abi.rs`, `core/emit/{props(new),object,runtime,value,expr,mod,sections,generator,json_parse}.rs`, `core/codegen/{expr,stmt,capture,op}.rs`, `core/run/{value,promise}.rs`, `pkg/{abi,value,shim}.ts`, tests | S0 | defects DF1-DF5 have regression tests; opcount `--check` green or justified; `unsupported.rs:138-164` rewritten as runtime behaviour | R1 90 + F1 115 + E3 80 + G9 25 ≈ **310** |
| **S2** arrays | handle + `TYPE_ELEMS`; amortized `push` on any receiver; growth by index; `HOLE`; `length` writes; elisions; named props on arrays; ToPropertyKey on arrays | `core/emit/{array,object,runtime,generator}.rs`, array helpers in `props.rs`, `core/codegen/expr.rs` (push/length/elision lowering), `core/run/host.rs`, `pkg/shim.ts` array readers, tests | S1 | DF8, DF9 tests; `let b = a; a.push(2)` → both see length 2; 10^5 pushes in O(n) fuel | R2 30 + holes 65 + push 50 ≈ **145** |
| **S3** constructors & classes as values | `__construct`, construct marker, `new.target`, return override, `F.prototype` materialization, `CLASS_CTOR`/`SELF_ALLOC`, spec derived `this`/`super`, class identity by binding, class expressions, static accessors, `OrdinaryHasInstance`, optional-call `this`, `arguments` object (`class ARGUMENTS`) | `core/codegen/{expr,stmt,function,scope}.rs`, `core/emit/{expr,object}.rs`, `props.rs` (materialize), tests | S1 | DF10 tests; the official `sta.js` alone compiles and runs unmodified | D1 160 + D2 30 + R7 80 + E3 15 + E1 650 + H1 175 ≈ **1110** |
| **S4** realm, global object, M1 | realm image + registry; intrinsic constructors/namespaces/prototypes as values; global object, `globalThis`, global `this`, runtime `ReferenceError`; Error family as `SELF_ALLOC` builtins, `extends Error` and other builtin parents; `Function.prototype.call/apply`, `Object.prototype.toString/hasOwnProperty`, generic `Array.prototype.map`; dirty bits; `sealed`/intrinsics-sealed analyses; G1c deletion (`core/codegen/op.rs:69-73`, `:267-273`, `:357-359`, `:231-237`); runner on the official harness | `core/emit/{realm(new),builtins/*(new),mod,sections}.rs`, `core/codegen/{expr,op,scope,mod}.rs`, `t262/**`, `tests/test262/**` (delete harness, new baseline), tests | S1, S3 | official `sta.js` + `assert.js` compile and run unmodified; DF11 test; baseline regenerated, net gain reported | A2 300 + A1 360 + C2 140 + C4 40 + A4 10 + R6 10 + H2 80 + E2 75 ≈ **1015** |
| **S5** builtin methods on prototypes & wrappers | move every syntactic family (1.4) onto prototypes as guarded fast paths (3.2) and delete the claims; `console`/`Math`/`JSON` dispatch via `sealed`; `String/Number/Boolean` prototypes, wrappers and `Object(x)`; Map/Set `size` getter; generic array methods over array-likes; delete G1a (`core/codegen/expr.rs:1131-1133`) | `core/emit/builtins/*`, `core/codegen/{expr,op}.rs`, `core/emit/{array,object,expr}.rs`, `core/run/host.rs` (`__method` callers), tests; may split into two L2s by family (Array/Object/Function vs String/Number/Boolean/Map/Set/Promise/Date) | S4 (S2 for array families) | DF12 tests (`o.push`, `o.map`, `o.indexOf` call user methods); opcount `--check` green or justified for every 3.1 row | A1 540 + R3 85 + R5 20 + F1 265 + G9 50 + C1 320 + A4 30 ≈ **1310** |

Dependencies: S0 → S1 → {S2, S3} → S4 → S5. S2 and S3 touch disjoint
code (arrays vs functions/classes) and may run in parallel after S1; merge
order S2 → S3 if both are ready.

Total ≈ **3890** → ≈ 9.1k / 22992 (≈ 39.5 %) after 2c, before eligibility
growth. The E1 share (≈ 650) is codegen-gated (many class-expression tests
also hit private members/generators, G1/G2). Phase 3 (descriptors and
reflection: B1 ≈ 1000, B2 ≈ 30) additionally makes `propertyHelper.js`
compile, adding 3074 tests to the eligible set (≈ 1000 passes at 35 %).

Every stage: repo gate (`DESIGN.md:913-937`: fmt, typos, clippy, nextest
incl. opcount, doc tests, shear, deny), test262 `--check` with zero
regressions (baseline updated in the same commit), web gate when `pkg/`
changes, `DESIGN.md` sections 4-7 and 13 updated for what the stage changed.

---

## 11. Risks, open questions, rejected alternatives

### 11.1 Risks

1. **S1 width**: layout, emitter, codegen, both host decoders change
   together. Mitigation: S0 moves allocation behind exports and lands the gate
   first; all offsets become named constants in `core/abi.rs` checked against
   `pkg/abi.ts`.
2. **Per-access cost**: one extra load (`props`) per property access and per
   array element (`elems`). Offset by pointer-compare keys and amortized push;
   any net increase is visible in the fuel column and must be justified.
3. **Sibling merges**: mr9syrjv rewrites the same dispatch S1 replaces
   (`core/emit/object.rs:1329-1591`); 2c must start after R and C merge.
4. **Baseline churn at S4**: the official harness flips tests both ways
   (shimmed `assert_throws_*` accept `instanceof` + `.name`, the real one
   compares constructors).
5. **Code size / compile time**: the realm image and builtin bodies are
   emitted into every module (open question 5); test262 compiles one module
   per test.
6. **Uniform `K` ABI**: variadic builtins called generically receive at most
   `K` arguments (`DESIGN.md:1063-1064`); fast paths keep handling the static
   variadic forms.
7. **Unbounded intern table** until the phase-4 weak sweep.
8. **E1 estimate** depends on codegen work outside the object model.

### 11.2 Open questions (each with a recommended default)

1. Dense-growth bound for `a[i] = v`: (a) `i < max(2·cap, len + 1024)`
   **[default]**; (b) `i < len + 1024`; (c) always dense up to the memory cap.
2. Fast paths for `.length` / `.size`: (a) keep kind-dispatched `.length`
   inline and a guarded `.size` **[default]**; (b) always the generic get.
3. Promise engine into wasm before GC: (a) yes, roadmap order (self-contained
   module before GC), no host-root API **[default]**; (b) GC first with a
   `__gc_roots` host import.
4. Opcount/fuel gate strictness: (a) any increase fails; raising the snapshot
   requires a `reason` entry reviewed in the PR **[default]**; (b) 2 %
   tolerance per program.
5. Builtins in every module: (a) emit every implemented intrinsic
   **[default]**; (b) prune intrinsics whose names never appear in the source
   (spec deviation for computed keys).
6. Top-level `var`/`function` declarations as global-object properties:
   (a) no, they stay module cells in 2c **[default]**; (b) yes.
7. Lookup structure for large property tables: (a) linear scan only in 2c
   **[default]**; (b) add a hash side index above 32 entries.
8. Class field initializers: (a) CreateDataProperty (`__define`, spec) from
   S3 **[default]**; (b) keep today's `this.f = v` assignment semantics.
9. Builtin bodies in 2c: (a) Rust-emitted wasm wrapping today's helpers; the
   JS prelude stays roadmap item 3 **[default]**; (b) start the JS prelude in
   S5.
10. Error `stack`: (a) own non-enumerable `"<name>: <message>"` as today
    **[default]**; (b) drop `stack`.
11. Rendering of class/function instances: (a) Node style `A { x: 1 }`,
    uncaught `Uncaught A { … }`, errors as `Uncaught Name: message`
    **[default]**; (b) keep plain `{ … }`.
12. S5 staffing: (a) two parallel L2s split by intrinsic family
    **[default]**; (b) one L2.
13. Reflection (`defineProperty` family, `propertyHelper.js`): (a) phase 3 as
    planned **[default]**; (b) add a stage S6 to 2c (≈ +1000 passes, +3074
    eligible).

### 11.3 Rejected alternatives

- **WasmGC**: jsvals are NaN-boxed `i64` (`core/abi.rs:12-31`); GC references
  cannot live in `i64`, so the value model, every helper, the host decoders
  and the snapshot renderer would be rewritten; dynamic properties would still
  need dictionary arrays inside GC structs. A rewrite, not a migration;
  revisit only after phase 4.
- **Hidden classes / shapes**: pay off only with inline caches (per-site
  feedback, shape transition trees, shape GC); jsc has no IC infrastructure
  and no measurement asking for it. **Dictionary-only**: what we pick, with
  interned keys. **Hybrid** (shapes until `delete`, then dictionary): two
  representations behind every access; the optional predicted-slot check
  (3.2) gets most of the static benefit with one representation.
- **Fixed shapes plus a side dictionary**: two lookup paths per access, keeps
  the `@@` hacks and slot-compacting delete, enumeration order must merge two
  sources; it is the compatibility shim the product stage forbids.
- **Per-object hash table**: 2-4× memory for typical small objects, needs a
  separate insertion-order array, and hashing still happens at intern time;
  a 16-byte linear entry with a `u32` compare wins for the sizes test262 and
  user code use. A hash side index stays possible (open question 7).
- **8-byte header with an allocation-size word**: moves every payload offset
  including strings; size classes make the size recoverable without it.
- **Inline property slots in the handle**: needs both an inline and an
  overflow path at every access.
- **Host building blocks by hand**: duplicates layouts in Rust and TS
  (today's drift, DF7); replaced by exported guest constructors.
