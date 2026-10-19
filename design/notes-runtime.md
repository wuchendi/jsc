# Object-model notes: arrays, strings/primitives, GC inputs

Fact notes feeding `design/object-model.md` (phase 2c). Every current-state
claim cites `path:line` at commit `b1dc6b9`. Paths are relative to the repo
root; `core/` abbreviates `crates/jsc-core/src/`.

> **May change.** Sibling workstreams edit code concurrently and are not
> reflected here:
> - `mr9syrjv` (runtime semantics, `core/emit/**`, `core/run/**`): the
>   primitive-receiver property-write guard and string properties touch §3.2
>   and the unchecked type-byte loads in §1.4 / §1.5; `+` coercion touches §3.1.
> - `iqxrfhn5` (codegen, `core/codegen/**`): capture gaps, generator closures
>   and member destructuring touch §1.7 (captured-array `push`) and §4.1 (what
>   lives in cells/envs).

---

## 1. Arrays today

### 1.1 Layout

- One block kind `TYPE_ARRAY = 1`: `[1][pad×3][len:u32][elem:i64 × len]`
  (`core/abi.rs:97-99`). There is **no capacity field**: the block is exactly
  `HEADER_SIZE + 4 + 8*len` bytes (`core/emit/array.rs:38`).
- `HEADER_SIZE = 4` (`core/abi.rs:80`); elements start at `+8`
  (`core/emit/array.rs:36`). Elements are full NaN-boxed jsvals, so arrays nest
  (`DESIGN.md:349-351`).
- An array value is the single heap tag `HEAP | ptr` (`core/abi.rs:73-76`); the
  kind is read from the type byte (`core/emit/value.rs:125-133`).
- A literal `[e0, …]` allocates once, writes type byte and `len`, then each
  element left to right; `dst` stays on the wasm operand stack while element
  expressions (which may call and allocate) run (`core/emit/array.rs:40-67`).
- Elements are stored with `align: 0` (`core/emit/array.rs:62-66`) because
  `__alloc` does not align blocks (§4.4).

### 1.2 Method implementations and complexity

| operation | implementation | cost |
|---|---|---|
| `a.push(v)` | `__array_push(base, v)` allocates a fresh `len+1` block, `memory.copy`s the whole old block, appends, returns the new base (`core/emit/runtime.rs:913-987`) | **O(n) per push, O(n²) for n pushes**; old block abandoned |
| push rebinding | `emit_array_push_node` stores the new base back into the receiver **wasm local** (`core/emit/array.rs:171-212`) | — |
| `a.pop()` | decrements `len` in place, returns the old last slot (`core/emit/array.rs:214-261`) | O(1) |
| `[...xs, y]` | starts from an empty block and appends via `__array_push` / `__array_concat` (`core/emit/array.rs:75-169`; concat at `core/emit/runtime.rs:989-1073`) | each part reallocates: O(total²) |
| `map` | preallocates `len` result slots (`core/emit/array.rs:389-408`) | O(n) |
| `filter` | grows the result with `__array_push` per kept element (`core/emit/array.rs:409-424`, `:467-480`) | O(k²) |
| `reduce` / `forEach` | counted loop, no allocation (`core/emit/array.rs:425-435`, `:481-495`) | O(n) |
| `indexOf`/`includes`/`join`/`slice` | host `__method` import; snapshots the **whole linear memory** into a `Vec` per call (`core/run/host.rs:152-161`) then dispatches on the type byte (`core/run/host.rs:164-188`) | O(memory size) per call |
| `Map`/`Set` growth | `__map_set` appends key and value with two `__array_push` calls to the backing array (`core/emit/runtime.rs:2369-2376`) | O(n) per insert |

The host import list is fixed (`core/abi.rs:412-461`); `push` takes exactly one
argument and `reduce` requires an init (`core/codegen/expr.rs:911-917`,
`:952-960`; `DESIGN.md:1065`).

### 1.3 `push`/`pop` receivers

- `push`/`pop` are matched **syntactically by method name** on any member call
  (`core/codegen/expr.rs:554-563`), before user-method dispatch.
- The receiver must be a bare identifier (`core/codegen/expr.rs:888-894`)
  resolving to `AccessKind::Plain` — a plain wasm local
  (`core/codegen/expr.rs:903-909`; `core/codegen/mod.rs:382-388`). A boxed-cell
  binding (captured, or a module cell) is rejected at compile time with
  `array.push on a captured array is unsupported`.
- Because `push` rebinds only the receiver's local, **aliases keep the old
  block**: after `let b = a; a.push(2)`, `b` still points at the pre-push block
  and sees the old length. The same is true of an array stored in an object
  field or passed as an argument: `o.xs.push(v)` is rejected (non-identifier
  receiver), and a callee that pushes onto its parameter does not change the
  caller's array.
- Internal desugarings rely on this plain-local form: array-pattern rest
  (`core/codegen/stmt.rs:764-786`) and rest parameters
  (`core/codegen/function.rs:296-329`).

### 1.4 Index reads

- `x[i]` and `x.k` both lower to `IrNode::Index` (`core/codegen/expr.rs:93-106`)
  and dispatch at run time on the type byte (`core/emit/object.rs:1309-1447`):
  Map/Set → `size` (`:1358-1380`), object → `__obj_get` (`:1384-1396`), array →
  bounds check + load (`:1397-1422`), string → one-byte string (`:1427-1438`),
  anything else → unchecked `__str_index` (`:1440-1441`).
- The receiver's type byte is loaded **without** an `is_pointer` gate
  (`core/emit/object.rs:1362`, `:1384`); the comment assumes the receiver is a
  heap pointer (`core/emit/object.rs:1318-1319`). A number/boolean receiver
  reads memory at its low 32 bits.
- The array bounds test reinterprets the key as `f64` and truncates it
  (`core/emit/object.rs:1271-1292`). A non-number key (a heap-string pointer, a
  NaN box) truncates to `0`, so `a.foo` / `a["1"]` on a non-empty array read
  `a[0]` instead of `undefined` / `a[1]`; there is no ToPropertyKey / canonical
  numeric string step. Out-of-range or negative reads return `undefined`
  (`core/emit/object.rs:1420-1422`).

### 1.5 Index writes, growth, `length` writes

- `obj[i] = v` dispatches object → `__obj_set` else the array arm
  (`core/emit/object.rs:1565-1587`).
- Array arm (`core/emit/object.rs:1617-1692`): only a number key that is a
  non-negative integer and `< len` stores (`:1636-1683`); an integer key
  `>= len` throws `Cannot grow array by index assignment yet`
  (`:1684-1685`; message `core/emit/mod.rs:456-458`); any other key or receiver
  throws `Cannot add property '<k>': dynamic properties are not supported yet`
  (`:1688-1689`; `core/emit/mod.rs:450-455`).
- `.length` reads lower to `IrNode::StrLen` for every receiver
  (`core/codegen/expr.rs:94-96`, `:348-350`), which loads the `u32` at
  `+HEADER_SIZE` with no type check (`core/emit/expr.rs:1017-1036`). On an
  object that word is the entry count; on Map/Set it is `size`; on a number it
  is a wild read.
- `.length` is **not assignable**: assignment and `++`/`--` to `.length` are
  compile errors (`core/codegen/expr.rs:1288-1305`, `:1426-1438`).

### 1.6 Holes

- Array holes (elisions) are a compile error in both the plain and spread
  literal paths (`core/codegen/expr.rs:1473-1480`, `:1491-1498`). Since growth
  by index throws (§1.5), no runtime path can create a hole today; the array is
  always dense with exactly `len` initialized slots.

### 1.7 Closures, spread, `for-of`

- Captured bindings live in `TYPE_CELL` blocks (`core/abi.rs:114-117`;
  `core/ir.rs:420-436`) reached through the closure env
  (`core/ir.rs:437-442`) or a module-cell global (`core/ir.rs:443-448`). Any of
  these makes the binding non-`Plain`, so `push` on it is rejected (§1.3).
- Spread consumption: a string source goes through `__str_to_array`, a
  generator through `__gen_to_array`, anything else is assumed to be an array
  (`core/emit/array.rs:130-161`). `__gen_to_array` itself grows by
  `__array_push` (`core/emit/generator.rs:474-500`).
- `for-of` over a non-generator caches `StrLen(iter)` once and indexes
  `0..len` (`core/codegen/stmt.rs:1227-1259`); a runtime `IsGenerator` test
  selects the generator branch (`core/codegen/stmt.rs:1208-1216`).
  A spread call fills at most `K` positional slots (`DESIGN.md:305-309`;
  `core/emit/expr.rs:1347-1359`).

### 1.8 Observed behaviour (`jsc -e`, debug build of `b1dc6b9`)

| source | result |
|---|---|
| `let a=[1]; let b=a; a.push(2); console.log(a.length, b.length)` | `2 1` (alias keeps the old block) |
| `let a=[1]; a[5]=2` | `Uncaught TypeError: Cannot grow array by index assignment yet`, exit 1 |
| `let a=[1,2]; a.length=0` | compile error `unsupported assignment target`, exit 2 |
| `let a=[7,8]; console.log(a.foo, a["1"])` | `7 7` (non-number keys read slot 0) |
| `console.log("abc".foo, "é".length)` | `a 2` |
| `console.log({a:1,b:2}.length)` | `2` (entry count) |
| `let a=[1]; function f(){ a.push(2) } f()` | compile error `array.push on a captured array is unsupported` |
| `let o={xs:[1]}; o.xs.push(2)` | compile error `array.push requires a bound array variable receiver` |
| `let o={indexOf(x){return 42}}; console.log(o.indexOf(1))` | `undefined` (built-in name shadows the user method) |
| `let a=[1,,3]` | compile error `array holes (elisions) are unsupported` |
| `function f(x){ return x.length } console.log(f(5))` | `0` (wild header read on a number) |

---

## 2. Array design inputs

### 2.1 Requirements the layout must satisfy

1. **Stable identity.** `push` must mutate the array in place so every alias
   (locals, cells, object fields, arguments, host-held values) observes the
   change. This removes the plain-local restriction in §1.3 and the rebinding in
   `core/emit/array.rs:200-205`. Since a block cannot grow in place under a
   bump allocator, the array handle must stay put while its **element storage**
   moves.
2. **Amortized O(1) append.** Fixes the O(n²) `__array_push`
   (`core/emit/runtime.rs:913-987`), and with it `filter`, spread, `Map.set`.
3. **Index-write growth** (`a[len] = v`, `a[len+k] = v`) and **`length`
   writes** (truncate / extend).
4. **Arrays are objects.** `a.foo = 1`, `a["1"]` and inherited methods must go
   through the generic property table (sibling `notes-heap.md`) while integer
   keys hit the element fast path.

### 2.2 Layout options

| option | handle layout | pros | cons |
|---|---|---|---|
| A. inline elements + realloc (today) | `[hdr][len][elems…]` | 1 indirection | identity breaks on growth; needs forwarding or rebinding |
| **B. handle + separate element store** | `[hdr][len:u32][cap:u32][elems:i32 ptr][props:i32 ptr]`, store = `[hdr][cap][elem:i64 × cap]` | identity stable; doubling realloc only touches the store; same shape as today's Map/Set handle (`core/abi.rs:119-126`) | 1 extra load per element access |
| C. inline elements with spare capacity | `[hdr][len][cap][elems × cap]` | 1 indirection while it fits | still moves on overflow, so needs B's indirection anyway |

**Recommendation: B.** Map/Set already use the "fixed handle, growable backing
rewritten in place" pattern (`DESIGN.md:358-362`), so the emitter and host
decoders already handle it. The handle must keep the common object-header
prefix (type byte, flags, size; §4.6) and carry a `props` pointer (null when
the array has no named properties) so arrays share the generic property path.
`len` and `cap` stay `u32` (JS max length is `2^32 - 1`; the 512 MiB cap,
`core/abi.rs:368-371`, bounds `cap` far below that).

### 2.3 Growth policy

- `push` / `a[len] = v`: if `len == cap`, allocate a store with
  `cap' = max(4, 2*cap)` (sticking to the requested size when an explicit
  `length` or `new Array(n)` asks for more), `memory.copy` the `len` slots,
  repoint `elems`, free the old store (phase 4 allocator; abandoned until then).
- `a[i] = v` with `len < i`: allowed when the result stays dense enough —
  proposed rule `i < max(2*cap, len + 1024)`. Slots `len..i` are filled with
  the hole sentinel, then `len = i + 1`. Larger gaps fall back to the sparse
  path (§2.4).
- Non-integer / non-canonical keys (`"01"`, `-1`, `1.5`, `"foo"`) go to the
  property table; canonical numeric strings (`"1"`) must be converted to the
  element path (fixes §1.4's `a["1"]` → `a[0]`).

### 2.4 Holes: dense-with-sentinel vs sparse

- **Dense with a hole sentinel (recommended).** Reserve one NaN-box value that
  no JS value produces. The TDZ hole already uses this trick: an
  `undefined`-tagged box with payload `1` (`core/emit/mod.rs:216-219`). Reuse
  that bit pattern (`UNDEFINED | 1`) as the array hole: a load that sees it
  reads `undefined` after a prototype lookup (holes must consult
  `Array.prototype` in full JS, but returning `undefined` is enough until the
  prelude installs prototype elements). `in`, `hasOwnProperty`, `forEach`,
  `Object.keys` skip it. One compare on the read path; zero extra memory.
  The NaN-boxed model makes this free: the sentinel cannot collide with a
  number because canonical NaN is exactly `QNAN` (`core/abi.rs:29-31`).
- **Sparse fallback.** For huge gaps (`a[1e9] = 1`) store the element as a
  string-keyed property in the generic table and set a `SPARSE` flag on the
  handle; all element ops then go through the slow path. Never converting back
  is acceptable for phase 2c.
- Literal elisions `[1,,3]` (rejected today, §1.6) lower to the sentinel.

### 2.5 `length` writes

- `a.length = n` with `n < len`: set `len = n` and overwrite the dropped slots
  with the hole sentinel (or leave them: they are beyond `len` and unreachable;
  overwriting only matters for GC precision, §4.5 — the collector should trace
  `0..len` only, so leaving them is fine).
- `n > len`: if `n <= cap` (or a reasonable dense bound) fill `len..n` with
  holes; otherwise go sparse. `n` must pass ToUint32 equality or throw
  `RangeError` (spec ArraySetLength).
- Lowering: drop the compile-time rejection (`core/codegen/expr.rs:1295`,
  `:1428`) and route `.length` stores through an `__array_set_length` helper.
  `.length` reads should stop being the type-blind `StrLen` header load
  (`core/emit/expr.rs:1017-1036`) and dispatch on kind (string length, array
  `len`, else generic property get).

---

## 3. Strings and primitives

### 3.1 Representation today

- `TYPE_STRING = 0`: `[0][pad×3][len:u32][utf8 bytes × len]`
  (`core/abi.rs:93-95`); `len` is a **byte** count and `.length` reads it
  directly (`core/emit/expr.rs:1017-1036`; `DESIGN.md:1014-1016`).
- Literals are interned at compile time into active data segments packed from
  `HEAP_BASE = 8` (offset 0 is a null guard; `core/emit/mod.rs:231-235`),
  deduplicated by content (`core/emit/sections.rs:392-439`); each blob is
  byte-identical to a runtime string (`core/emit/sections.rs:366-390`). The
  bump allocator starts right after them (`core/emit/mod.rs:891-894`,
  `core/emit/sections.rs:305`, `:327`).
- `typeof` results are interned strings (`core/abi.rs:59-71`).
- Runtime strings are produced by `__concat` (`core/emit/runtime.rs:184`),
  `__str_index` (one byte; `core/emit/runtime.rs:437-460`), `__str_to_array`
  (`core/emit/runtime.rs:2155-2165`) and the host (`host_alloc_string`,
  `core/run/host.rs:48-69`). There is no runtime interning: equality is a byte
  compare (`__str_eq`, `core/emit/runtime.rs:270`), and object keys are matched
  by content (`DESIGN.md:352-354`).
- `+` with a string operand converts both operands with the host `__convert`
  and concatenates (`core/emit/numeric.rs:540-560`).
- All string methods except indexing/`length` run host-side in `__method` and
  operate on bytes (`core/run/host.rs:141-144`, `:189-243`); case mapping is
  ASCII-only (`core/run/host.rs:189-206`). The renderer decodes with
  `from_utf8_lossy` (`core/run/value.rs:165-175`).

### 3.2 Property access on primitives today

- There are **no wrapper objects** and no ToObject: no `TYPE_*` for Number,
  Boolean, String or Symbol wrappers exists (`core/abi.rs:93-208`), and
  `Symbol` is not implemented (`DESIGN.md:1027`).
- `"abc".length` is the header load above. `"abc"[i]` and `"abc".foo` both go
  through the string arm of `Index`; the non-number key truncates to `0`, so
  `"abc".foo` yields `"a"` (`core/emit/object.rs:1427-1438`; `DESIGN.md:1044-1045`).
- String methods are recognized by name at the call site
  (`core/codegen/expr.rs:574-585`, arity at `:1001-1043`) and passed to the host,
  which returns `undefined` for a receiver of the wrong kind
  (`core/run/host.rs:164-245`). A consequence: a **user object** method named
  `indexOf`, `slice`, `join`, `includes`, `trim`, … is never called — the call
  goes to `__method`, which yields `undefined` for an object receiver.
- Any other method on a literal primitive/array receiver is a compile error
  (`core/codegen/expr.rs:1125-1133`; message `core/codegen/mod.rs:207-216`).
  On a non-literal receiver it becomes a user method call that reads the key
  through `Index`, i.e. the unchecked type-byte path of §1.4.
- Number receivers: no `toFixed`/`toString` (`DESIGN.md:1046-1049`); `x.length`
  / `x.k` on a number read memory at the number's low bits (§1.4, §1.5).

### 3.3 What phase 2c needs

1. **Kind dispatch for every receiver.** The generic get path must classify
   the receiver first (`is_pointer` + type byte, `core/emit/value.rs:103-133`)
   instead of the current unchecked load. Primitive classes: number (non-boxed
   or canonical NaN), boolean, string, `undefined`/`null` (TypeError, already
   guarded by `emit_nullish_guard`, `core/emit/object.rs:1953-1959`).
2. **Primitive receivers look up a per-kind prototype without allocating a
   wrapper.** `"abc".foo` → own string properties (`length`, canonical index
   keys) else `String.prototype` lookup; `(5).foo` → `Number.prototype`;
   `true.foo` → `Boolean.prototype`. `this` for a method found this way is the
   primitive itself (strict mode, `DESIGN.md:1005-1006`), so no ToObject box is
   needed for calls. Writes to a primitive are silent no-ops in sloppy mode but
   **TypeError in strict mode** (the only mode jsc supports) — `mr9syrjv` is
   adding that guard.
3. **Prototype roots.** The property-table design must give `String.prototype`,
   `Number.prototype`, `Boolean.prototype`, `Array.prototype` stable addresses
   (module globals like the module cells at `core/emit/mod.rs:211-214`, or
   fixed static blocks in the data segment) so the get path can find them
   without a global lookup. Until the JS prelude (roadmap item 3,
   `DESIGN.md:1109-1110`) these prototypes hold only the built-ins the compiler
   recognizes today; the syntactic recognition (`core/codegen/expr.rs:554-585`)
   must stop shadowing user methods of the same name — a recognized name should
   dispatch to the built-in only when the receiver's kind matches, else fall
   back to the generic call (the pattern Map/Set already use,
   `DESIGN.md:772-774`).
4. **Explicit wrapper objects** (`new String("a")`, `Object("a")`) are rare;
   reserve a `TYPE_PRIMITIVE_WRAPPER` kind with a `[value:i64]` slot in the
   object header family but do not implement it in 2c.
5. **Key normalization.** Property keys are interned-or-content-compared
   strings today (`DESIGN.md:352-354`). The property table should key on
   **interned** string pointers (pointer compare) plus an integer-index fast
   path; runtime-built keys (`o[k]`) are interned through a host or guest
   intern table at lookup time. That interning table becomes a GC root /
   weak table in phase 4 (§4).

### 3.4 Phase 3: Latin-1 / UTF-16 (roadmap item 2, `DESIGN.md:1107-1108`)

- Plan a flags byte in the string header now: `encoding ∈ {latin1, utf16}` and
  an `interned` bit. Latin-1 keeps one byte per code unit (all ASCII literals
  stay as they are); UTF-16 uses two. `len` becomes the **code-unit** count, so
  `.length`, indexing, and `charCodeAt` are O(1).
- The interned-literal data segment and the host decoder
  (`core/run/value.rs:165-175`) must read the encoding bit; the host `__method`
  byte algorithms (`core/run/host.rs:345-659`) are replaced, not adapted.
- Interaction with the property table: keys compare by identity once
  interned, so encoding only matters at intern time (a Latin-1 and a UTF-16
  string with the same code units must intern to the same key — canonicalize
  to Latin-1 when every unit `< 0x100`).
- Optional later: rope/concat nodes for repeated `+=`; reserve a
  `TYPE_STRING_ROPE` kind number but do not design it now.

---

## 4. GC inputs

### 4.1 Where values live today

| location | holds | citation |
|---|---|---|
| wasm `i64` locals | every binding not captured; three+ i64 scratch slots per function | `core/emit/function.rs:15-57` |
| wasm `i32` locals | **raw untagged block bases** (array/HOF/callee scratch: `dst`, `arr`, `cb`, `res`) | `core/emit/function.rs:19-27`; `core/emit/array.rs:359-364` |
| wasm operand stack | heap pointers live across nested evaluation and calls (array literal `dst`, `__array_push` base, `+`'s converted lhs across the second `__convert`) | `core/emit/array.rs:40-67`, `:122-129`; `core/emit/numeric.rs:550-558` |
| globals | `$exc_value`, `$cf_value`, `$__this` (jsvals); module-cell pointers from global 9 | `DESIGN.md:281-290`; `core/emit/mod.rs:211-214` |
| heap cells / envs | captured bindings (`TYPE_CELL`), closure env (`TYPE_ENV` of cell pointers) referenced from function values | `core/abi.rs:106-117`; `core/ir.rs:402-448` |
| generator handle | `env` frame (a `TYPE_ENV` of cells holding every generator local) | `core/abi.rs:157-182`; `core/ir.rs:462-470` |
| Map/Set handle | `entries` backing array pointer | `core/abi.rs:119-133` |
| host (Rust) | promise reactions keyed by block base and the microtask queue, each holding callback/value jsvals and `async_gen` handles | `core/run/promise.rs:57-95` |
| host (TS shim) | same engine duplicated | `packages/jsc/src/shim.ts:449-452` |
| static data | interned literals below the heap start | `core/emit/sections.rs:392-439` |

There is **no shadow stack** and no frame in linear memory: functions use only
wasm locals and the operand stack (`core/emit/function.rs:46-57`), and the
uniform signature passes all arguments as wasm params (`core/abi.rs:82-91`).
Nothing is ever freed (`DESIGN.md:400-402`).

### 4.2 What a precise mark-sweep needs: roots

wasm locals and the operand stack are invisible to a collector running inside
the module, so every live heap pointer must be in linear memory or a global at
each **safepoint** (any call that may allocate: `__alloc`, every runtime
helper that allocates, every host import, every user call).

- **Shadow stack.** A linear-memory stack (`$__sp` global, grows down from a
  reserved region below the heap). Each function reserves `n` i64 slots in its
  prologue, where `n` is the number of jsval locals that can be live across a
  safepoint; it clears them to `undefined` (so a stale pointer is never
  traced) and pops them on every exit, including the exception-return paths
  (`emit_propagate_if_pending`, `core/emit/array.rs:326-329`).
- **What codegen must spill.** Simplest correct rule for phase 4: keep every
  jsval binding that holds a heap value in its shadow slot (store on write,
  load on read) — i.e. treat locals as memory. Optimization later: liveness-
  based spill only around safepoints. Raw `i32` base locals must either be
  re-derived from a spilled jsval after each safepoint or be eliminated;
  the operand-stack idiom of §4.1 (keeping `dst` under a sub-expression that
  allocates) must be rewritten to spill to a slot first. Non-moving collection
  keeps raw pointers valid, but they still must not be the only reference.
- **Globals** `$exc_value`, `$cf_value`, `$__this`, module cells, and the
  prototype roots (§3.3) are scanned directly.
- **Host roots.** Every jsval the host stores across a guest call (promise
  reactions, queued jobs, `async_gen` handles — `core/run/promise.rs:62-95`)
  must be registered as a root: either the host keeps them in a guest-memory
  root array, or the guest collector calls a host import to enumerate them.
  This disappears with the self-contained module (roadmap item 4,
  `DESIGN.md:1111-1113`) if the promise engine moves into wasm; the collector
  should be designed for that end state.
- **Static data** (addresses `< heap_start`, `core/emit/sections.rs:398-399`)
  is never collected; marking must skip it (no mark bits there) — so the
  header mark bit must not be required to exist on data-segment strings, or
  the data segment must contain pre-marked "permanent" headers.

### 4.3 Pointer identification and tracing

- A word is a heap reference iff `(bits & QNAN) == QNAN` and tag `== 0b101`
  (`core/emit/value.rs:103-119`); this is exact (canonical NaN invariant,
  `core/abi.rs:29-31`), so tracing through jsval slots is precise. The array
  hole / TDZ sentinel (`UNDEFINED | 1`) is not a pointer.
- Per-kind trace (current kinds `core/abi.rs:93-208`):
  string, Date — no children; array — `len` element slots (and, with §2.2 B,
  the `elems` store and `props`); object — `n` key/value pairs; function —
  `env`; env — `count` cell pointers; cell — `value`; Map/Set — `entries`;
  promise — `value`; generator — `env`; accessor — getter, setter.
- The tracer needs the block **size** for every kind to sweep without a side
  table; today size is implicit in kind-specific counts and is not stored for
  any block (`core/abi.rs:93-117`).

### 4.4 Allocation API

- Today: `__alloc(size: i32) -> i32` bumps `$__heap_ptr` with no alignment
  (`core/emit/runtime.rs:4-79`); string blobs of odd length leave following
  blocks unaligned (`core/emit/sections.rs:433-436`), which is why element
  loads use `align: 0`. The host bumps the same global
  (`core/run/host.rs:71-128`).
- Phase 4 needs: 8-byte aligned blocks (sizes rounded up), size classes with
  segregated free lists for small blocks (e.g. 16…256 bytes by 16, then powers
  of two) and a large-object list; a `__free`-less sweep that rebuilds the free
  lists; `__alloc` triggering GC when a budget is exhausted before growing
  memory (keep the 512 MiB cap and OOM sentinel, `core/abi.rs:368-377`).
- The host must stop bumping `__heap_ptr` directly and call an exported
  `__alloc` instead (one allocator, one policy).
- Non-moving: the host promise engine keys reactions by block base
  (`core/run/promise.rs:94`), and raw `i32` bases are pervasive; a copying
  collector would need all of these rewritten. Mark-sweep avoids that.

### 4.5 Write barriers

A non-incremental, non-generational mark-sweep needs no barriers. If phase 4
goes generational/incremental later, every jsval store site would need one;
the property-table and array-store helpers (§2) should be the only places that
store into heap slots, so a barrier can be added in one place. Codegen sites
that store jsvals into heap blocks directly today: array literal / HOF / index
set (`core/emit/array.rs:58-67`, `:449-465`; `core/emit/object.rs:1668-1683`),
cell set (`core/emit/expr.rs:1063-1080`).

### 4.6 What the object-model layout must reserve now

So phase 4 does not need a second relayout, every heap block header should be
8 bytes (the current header is 4, `core/abi.rs:78-80`):

| byte(s) | field | used by |
|---|---|---|
| 0 | `kind` (type byte; keep current numbering, `core/abi.rs:93-208`) | dispatch (all phases) |
| 1 | `gc` bits: mark bit, `permanent` (data segment / static prototypes), 6 spare | phase 4 |
| 2 | `flags`: per-kind (string encoding + interned; array `sparse`/`has_props`; object `extensible`/`frozen`) | 2c, 3 |
| 3 | reserved | — |
| 4–7 | `size:u32` — total block size in bytes (or size-class index) | sweep, free lists |

- Payloads then start 8-aligned, so i64 slots can use `align: 3`.
- Data-segment string blobs must be written with the same 8-byte header and
  `permanent` set (`core/emit/sections.rs:378-387` would change accordingly).
- Kinds that hold raw `i32` pointers (the proposed array `elems`/`props`) are
  traced by kind, so they need no tagging; but the element store must itself be
  a block with a header (kind `ARRAY_STORE`, `cap` in its payload) so the
  sweep can walk the heap linearly.
- Internal-only kinds (env, cell, accessor, array store, property table) keep
  the same header, so one sweep loop handles every block.

---

## Open questions

1. Dense-growth bound for `a[i] = v` (§2.3): the proposed
   `i < max(2*cap, len + 1024)` is a heuristic; V8 uses a similar gap limit.
   Needs a decision in `object-model.md`.
2. Whether `length` on objects/Map should keep any fast path once `StrLen`
   becomes kind-dispatched (§2.5); Map/Set `.size` is currently intercepted in
   `Index` (`core/emit/object.rs:1358-1380`).
3. Whether the host promise engine moves into wasm before GC (§4.2); this
   decides whether a host-root registration API is needed at all.
