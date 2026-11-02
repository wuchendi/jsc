# Notes: memory management and weak collections (phase 4, R-T2)

Intermediate notes for sections 5 (memory management) and 6 (weak
collections) of `design/runtime-memory.md`. R-T3 folds them into that file
and deletes this one. Every claim about the current code cites the code at
`c34cd36`; "today" means that commit. Proposed names (globals, constants,
helpers) are recommendations that R-T3 may rename, but the mechanisms are
meant to be implemented literally.

Contents:

1. Collector and roots
2. Per-kind tracing table
3. Allocation, sweeping, collection triggers, out-of-memory
4. Cost model
5. Weak collections
6. Staging hints for R-T3
7. Corrections to DESIGN.md that the fold must carry

---

## 1. Collector and roots

### 1.1 Algorithm

Precise, non-moving, stop-the-world mark-sweep, run synchronously inside
`__alloc` (the only collection point; §3.4). Non-moving is required, not
merely convenient:

- emitted code and helpers keep raw `i32` block bases in wasm locals and on
  the wasm operand stack (e.g. the array literal keeps its store base on the
  operand stack while element expressions run, `emit/array.rs:119-121`,
  `emit/array.rs::emit_array_node`);
- object-literal property tables are co-allocated *inside* the object's
  allocation (§3.1, `emit/object.rs:38-45`), so a table address is an interior
  address of another block;
- the intern table, props keys and symbol keys compare by `u32` address
  (DESIGN §5.3).

Consequence: a raw `i32` base never needs to be "re-derived" after a
safepoint (DESIGN §5.7 says it does; drop that sentence, §7). It only needs
its block to be *rooted* somewhere while the base is live.

No write barriers (stop-the-world, non-incremental, non-generational).

### 1.2 Mark bit: an epoch byte

Header byte 1 (`HEADER_GC_OFFSET`, `abi.rs:94-97`) is free today: every block
is created with it `0` (`emit/runtime.rs:90-104` zeroes the 4 header bytes;
static blobs write `0`, `emit/sections.rs:596`). Use the whole byte as an
**epoch mark**:

- new global `$__gc_epoch: i32`, cycling `1, 2, …, 255, 1, …` (never `0`);
- a block is marked in the current cycle iff `gc == $__gc_epoch`;
- marking writes the epoch; nothing ever clears marks.

Why not a single bit plus a clearing pass: co-allocated interior blocks (the
inline `TYPE_PROPS` table of an object literal) and the static region are
never visited by the sweep, so a single bit set on them would never be
cleared and the next cycle would wrongly treat them as already traced. With
an epoch, a stale mark only aliases after 255 cycles, and only on a block no
longer referenced by anyone (an abandoned inline table), so the alias is
harmless. A freshly allocated block (`gc = 0`) is never marked.

### 1.3 Shadow stack

Root discovery without stack scanning: every jsval (or raw block base) that is
live across a safepoint is also stored in a slot of the current function's
frame on a **shadow stack** in linear memory. The collector scans the whole
live range of the shadow stack as a flat array of jsvals; frames carry no
header and no descriptor.

**Region and SP.**

| item | value |
|---|---|
| region | `[0, SS_SIZE)`, `SS_SIZE = 1 MiB` (16 wasm pages), new `abi::SHADOW_STACK_SIZE` |
| direction | grows **down** from `SS_SIZE` toward `0` |
| SP | new global `$__sp: i32`, initial `SS_SIZE`; the live range is `[$__sp, SS_SIZE)` |
| static region | moves up: `HEAP_BASE = SS_SIZE + 8`; `FN_INFO_ADDR` moves to `SS_SIZE + 4` (today `abi.rs:310`, inside the `0..8` null guard) |
| overflow | `fp = $__sp - 8·N` wraps below `0` to an address above the 512 MiB memory maximum, so the first slot store traps out of bounds. No explicit check (same failure class as today's native wasm stack overflow). See open question Q1 for a catchable variant. |

**Frame layout.** A function with `N > 0` root slots reserves `8·N` bytes:

```text
fp = $__sp - 8·N
[fp + 0]       slot 0   (i64 jsval)
[fp + 8]       slot 1
…
[fp + 8·(N-1)] slot N-1
```

Every slot always holds a valid jsval: `0` (the number `+0`, never a pointer)
after the prologue clears it, a written-through jsval, or `HEAP | base` for a
rooted raw base. Slot order is the emitter's allocation order; nothing reads
it except the collector, which treats all slots alike.

**Prologue** (emitted once at function entry, only when `N > 0`):

```wasm
global.get $__sp  local.tee $old  i32.const 8N  i32.sub  local.tee $fp  global.set $__sp
;; clear: N <= 4 -> N × (local.get $fp  i64.const 0  i64.store offset=8k)
;;        N >  4 -> local.get $fp  i32.const 0  i32.const 8N  memory.fill
```

Clearing is mandatory: a stale slot from a popped frame may name a block that
a later cycle freed, and the scan would follow it.

**Epilogue** (`local.get $old  global.set $__sp`, 2 ops) before **every**
exit:

- the function's final `end` (fall-through result);
- every `return`. In user code all returns funnel through three emitters:
  `emit/function.rs::emit_throw_return` (the no-handler unwind,
  `emit/function.rs:173`), `emit/function.rs::emit_return_transfer` (the direct
  `return`, `emit/function.rs:244`), and the generator state machine's
  `Return`s (`emit/generator.rs:499,644,657`). Hand-written helpers and
  builtins contain about 240 further `Return` instructions (`grep -w Return`
  over `emit/`).

To make this mechanical rather than a 240-site audit, M1 introduces a
function builder that wraps `wasm_encoder::Function` (see §6, M1): it buffers
instructions, intercepts every `Instruction::Return` and the outermost `End`
to insert the epilogue, and emits the prologue with the final `N` once the
body is complete (frame size is known only after emission because temporary
slots are allocated on the fly).

**Exceptions.** jsc unwinds manually (DESIGN §6.6): a throw either `br`s to a
handler in the same function (SP unchanged, correct) or returns
(`emit_throw_return`), which the builder's `Return` interception covers. No
other unwind path exists. A trap kills the instance; nothing to restore.

**Re-entry.** Until the runtime-independence stages delete the host promise
engine, the host re-enters the guest (`__invoke1`, `__async_step`, the
constructors) while guest frames are live; the new frames simply push below
the current SP, so nesting stays consistent. After R-phase only `main` (and
the guest job loop it runs) is entered from outside.

### 1.4 Safepoints

A safepoint is any instruction during which a collection can run:

1. every `call $__alloc` (§3.4: the only place a collection starts);
2. every `call` / `call_indirect` whose callee may transitively reach
   `__alloc`. That is every user function, every builtin body, every
   `call_indirect`, and every runtime helper except a fixed **non-allocating
   leaf set**, computed by the emitter from a static call graph of the
   helpers (each helper emitter declares the helpers it calls). Expected leaf
   set (by reading the emitters): `__str_eq`, `__str_cmp`, `__str_hash`,
   `__intern_find`, `__index_value`, `__prop_find`, `__same_value_zero`,
   `__map_find`, `__box_f64`, `__protos_indexless`, `__private_find`,
   `__json_ws`, `__str_exotic_index`. Everything else allocates or may call
   user code (a getter, `toString`, a callback).
3. host imports: after R-phase only the WASI imports remain and none calls
   back into the guest, so they are not safepoints.

### 1.5 What codegen spills

All IR locals are `i64` jsvals (the IR comments still say "f64",
`ir.rs:6,2351,2473`, but `emit/function.rs:45-56,128-139` declares them
`ValType::I64`); there are no `f64`- or `i32`-typed IR locals today. The
spill set per function:

| class | where | rooted how |
|---|---|---|
| IR locals (`let`/`const`/`var`, temps, hidden locals such as `%penv`, iterator records) | `0..locals` of `IrFunction` / `IrProgram` (`ir.rs:2347-2365,2470-2478`) | **write-through**: every `local.set`/`local.tee` of a rooted local also stores to its slot (3 extra ops). Reads stay register reads. |
| parameters `a0..a_{K-1}`, `env` | locals `0..K+1` (DESIGN §4.2) | stored to their slot in the prologue if the parameter is read after a safepoint; unread padding slots get no slot. `argc` is a number (never rooted). |
| `this` snapshot local | DESIGN §4.3 (prologue copy of `$__this`) | write-through like an IR local |
| emitter scratch `i64` | `scratch+0..+2, +4..+6, +13..+20, +22, +23` (`emit/function.rs:15-44`) | write-through. **Exception:** the `Date` scratch `+24..+29` holds raw integer arithmetic, never jsvals; declared `raw`, never rooted. |
| emitter scratch `i32` holding a block base or a key | `+3` (`dst` while a block is filled, e.g. object literal `emit/object.rs:31`), `+7` (indirect callee base / array store), `+8`, `+11`, `+12` (HOF receiver, callback, result bases), `+21` (generator frame base, `emit/generator.rs:43`) | write-through of `HEAP | i64(base)` (6 extra ops). `+9`, `+10` (length, index) are raw integers. |
| builtin `Frame` locals | `emit/builtins/mod.rs:127-151` (`i64` jsvals, `i32` bases/indices, `f64` numbers) | `i64`: write-through; `i32`: only those the builtin declares as bases (`frame.base(k)` instead of `frame.i32(k)`); `f64`: never |
| hand-written helper locals | each helper's `Function::new` locals | the helper declares which `i64` locals are jsvals and which `i32` locals are bases/keys; undeclared locals are never rooted |
| operand-stack temporaries | a value pushed, then a sibling expression containing a safepoint evaluated before the value is consumed: call arguments (`emit/expr.rs:808`, `emit_call_args`), binary operands, the `dst` base duplicated beneath an element/field expression (`emit/array.rs:162-167`, `emit/object.rs:76-86`) | **temp slot**: `local.set $t; local.get $fp; local.get $t; i64.store off; local.get $t` (4 extra ops; 7 for an `i32` base). Temp slots are a depth-indexed stack inside the frame, released when the consumer runs, so `N` = rooted locals + max temp depth. |

Keys deserve a separate note: `__to_key(v, 1)` may return the canonical block
of an *already interned* string, which is reachable only from the (weak, §1.7)
intern table. Any `i32` key held across a safepoint (`__define` table growth,
`__set` calling a setter) is therefore rooted as a base, like any block base.

### 1.6 Spill elision

Two provable elisions, both local to a function:

1. **No-safepoint functions get no frame.** If a body contains no safepoint
   (§1.4), no collection can run while it is active, so none of its values
   need roots (its callers rooted their own). `N = 0` and no prologue. The
   proof is the definition of a safepoint plus single-threaded execution.
   Applies to the leaf helpers above and to small user functions such as
   `(x) => x * 2` in `array_hofs`.
2. **NoRoot locals.** A local whose every definition is a **NoRoot**
   expression is never rooted. NoRoot values cannot keep a collectable block
   alive: numbers, booleans, `null`, `undefined`, `HOLE`, and pointers to
   static blocks (string literals, `typeof` words, static function blocks,
   intrinsics). NoRoot expressions: those literals; every numeric operator
   result (`-`, `*`, `/`, `%`, `**`, bitwise, shifts, unary `+`/`-`/`~`, `++`/`--`,
   since BigInt is unsupported they always yield numbers even when they call
   `valueOf`); comparisons and `!`, `in`, `instanceof`, `delete` (booleans);
   `typeof` (a static word); `a + b` when both operands are NoRoot and neither
   can be a string (a NoRoot operand that is a static string literal makes the
   result a fresh concatenation, so `+` with a possibly-string operand is not
   NoRoot); a read of another NoRoot local. Fixpoint over the function's
   locals; a local that is captured (boxed in a cell) is the cell pointer and
   is never NoRoot. This covers loop counters (`for (let i = 0; i < n; i++)`)
   and numeric accumulators.

   The same predicate elides operand-stack temp slots: a NoRoot operand needs
   no temp slot (e.g. the `n - 1` argument in `fib`).

Shrink-wrapping (helpers only): a hand-written helper whose fast path has no
safepoint and whose slow path does (`__array_push`: in-place append vs
`__array_reserve` growth) pushes its frame only on the slow path. The helper
emitter does this explicitly; the builder enforces that a `Return` after a
frame push restores SP and one before it does not.

### 1.7 Root set

| root | where today | treatment |
|---|---|---|
| shadow stack | new | scan `[$__sp, SS_SIZE)` as jsvals |
| `$exc_value` (global 2) | `emit/sections.rs:496` | jsval root |
| `$cf_value` (global 4) | `emit/sections.rs:503` | jsval root (a routed return value, or the loop-target code `encode_loop_target`, a small integer whose bits are a denormal number, never a pointer) |
| `$__this` (5), `$__new_target` (12), `$__spread_args` (13) | `emit/sections.rs:507-513,529-547` | jsval roots |
| module cells (15+) incl. tagged-template sites | `emit/sections.rs:551-557`; cells allocated in `main`'s prologue `emit/function.rs:60-77`; template cells `codegen/mod.rs:268`, `codegen/expr.rs:1085-1095` | jsval roots (each a `TYPE_CELL`; template objects live in their cell forever, as `GetTemplateObject` requires) |
| `$__json_data/_pos/_len` (6-8) | `emit/sections.rs:515-520` | **not** roots: `$__json_data` is an interior data address valid only during `__json_parse`, whose parameter `s` (the string) is rooted in its own frame (`emit/json_parse.rs:785-803`) |
| intern table (`$__intern_base/_cap/_count`, 9-11) | `emit/props.rs:505-540` | **weak** (§1.8); the heap table block itself is marked via `$__intern_base - 8` |
| `$__intrinsics_dirty` (14), `$__heap_ptr`, `$exc_pending`, `$cf_kind` | | integers, not roots |
| static region | DESIGN §4.2 table | never swept. Its object-family blocks are mutable (user code adds properties to `%Object.prototype%`, materializes a static function's `length`/`name` into a heap table), so the compiler emits a **static root list** `[count: u32][addr: u32 × count]` of every static object-family block (realm intrinsics, the static builtin-method `TYPE_FUNCTION` blocks, the static user top-level function blocks); each cycle traces those blocks' fields without marking them. Static strings, static symbols (description = static string) and static accessor blocks (getter/setter = static functions; a redefinition allocates a new heap accessor) are immutable and skipped. |
| `Symbol.for` registry | static intrinsic `SymbolRegistry` (`emit/realm.rs:66-69`) | strong: it is in the static root list; its (possibly heap-rebuilt) table keeps every registered symbol and its key string alive, as the spec requires |
| `%penv` arrays | hidden locals / env cells (`codegen/stmt.rs:454-475`) | ordinary arrays, reached through locals, env cells and closures; nothing special |
| interned property keys | `u32` keys in `TYPE_PROPS` entries | traced as keys (§2); static keys are static |
| guest promise state (after R-phase) | new, R-T1 | the guest microtask queue head/tail globals and any unhandled-rejection list global are jsval roots; promise reaction lists hang off the promise blocks and are traced with them (§2) |
| weak-collection globals (M3) | new | `$__kept_objects` (strong array, §5.3), `$__fin_jobs` (strong list, §5.4); the per-cycle weak lists are collector-internal and rebuilt every cycle |
| host-held handles | `run/promise.rs:36-98` today | **none after R-phase.** Today the native host holds jsvals and bases in `PromiseEngine` (`queue: VecDeque<Job>` with `callback`, `value`, `result`, `async_gen`; `reactions: HashMap<u32, Vec<Reaction>>` with `on_fulfilled`, `on_rejected`, `result`, `async_gen`; `rejected`, `handled` sets of bases), and both hosts hold freshly allocated `__new_string`/`__new_array` bases while filling them. None is visible to a guest collector, so M2 must not land before the host promise engine and host-allocating ops are gone (§6). `rejected` also relies on "ascending base order is creation order" (`run/promise.rs:92-94`), which free-list reuse breaks; the guest replacement must record order explicitly. |

### 1.8 Interned strings (weak)

The intern table is weak: after marking, every slot holding an unmarked heap
string is overwritten with the tombstone `1` (never a block base: bases are
8-aligned). Required changes to `emit/props.rs`:

- `__intern_slot` / `__intern_find` treat `1` as occupied-but-not-matching and
  keep probing (today `0` ends a probe and any nonzero value is compared);
- insertion does not reuse tombstones (keeps `__intern_slot` single-pass);
  `$__intern_count` keeps counting them, so the load-factor test still
  triggers growth;
- growth (`emit/props.rs:516-540`) re-inserts only slots `> 1`, dropping the
  tombstones;
- static strings and keys reachable from any table stay marked, so a live key
  is never tombstoned. Re-interning the content of a dead string later simply
  inserts the new block.

---

## 2. Per-kind tracing table

Marking a block: if it lies in the static region, do not mark it (static
blocks are traced only from the static root list); if `gc == epoch`, stop;
else write the epoch and push it on the mark stack. Popping traces by kind.
A **jsval slot** is followed iff `bits & 0xFFFF_0000_0000_0000 ==
0x7FFD_0000_0000_0000` (the exact `QNAN` + heap tag, DESIGN §5.7; `HOLE`,
`0`, booleans etc. are not pointers) and the low 32 bits lie in the heap
(`>= heap_start`) or the static region; a **raw ref** is a `u32` base, `0` =
none.

Every `TYPE_*` constant of `abi.rs:198-608` has a row; proposed new kinds are
marked *new*.

| type | kind | pointer fields (offsets from base) | variable part | leaf |
|---|---|---|---|---|
| `0` `TYPE_STRING` | string, Latin-1 or UTF-16 | none | data `8 + (len << two_byte)` bytes | **leaf** |
| `1` `TYPE_ARRAY` | array handle | prefix: raw `props @4`, jsval `proto @8`; raw `elems @20` (`0` = no store) | none | no |
| `2` `TYPE_OBJECT` | object, by `class` byte 3: | prefix `props @4`, `proto @8`, plus: | | no |
| | `0` ordinary, `1` error, `2` arguments, `6` global | prefix only (an object literal's inline table at `+16` is reached through `props`, never by class) | | |
| | `3`/`4`/`5`/`7` Boolean/Number/String/Symbol wrapper | jsval `@16` (`WRAPPER_VALUE_OFFSET`) | | |
| | `8` iterator, kinds `0..8` | jsval `@16` (`[[IteratedObject]]`); `@24` u32 index and `@28` u8 kind are integers | | |
| | `8` iterator, kind `16` (`Iterator.from` wrapper) | jsvals `@16`, `@32` (iterator), `@40` (`next`) | | |
| | `8` iterator, kinds `17..21` (helpers) | jsvals `@16`, `@32`, `@40`, `@56` (callback), `@64` (inner iterator), `@72` (inner `next`); `@48` is an `f64` counter (a number, skipped or harmless) | | |
| `3` `TYPE_FUNCTION` | function, closure, bound function, settler | prefix; jsval `env @24` (a `TYPE_ENV`, a bound function's `[target, boundThis, ...args]` array, a settler's promise, or `undefined`) | none | no |
| `4` `TYPE_ENV` | closure env, generator frame, async-generator request record | jsval slots `@8 + 8k`, `k < count @4` | `count` slots | no (see fix F2: the request record's `next` must become a jsval) |
| `5` `TYPE_CELL` | boxed binding, module cell | jsval `@4` (unaligned i64; may be `HOLE` while in TDZ) | none | no |
| `6` `TYPE_MAP` / `7` `TYPE_SET` | Map / Set handle | prefix; jsval `entries @24` → backing `TYPE_ARRAY` (traced as an array: tombstones are `HOLE` keys and `undefined` values) | none | no |
| `8` `TYPE_PROMISE` | promise | prefix; jsval `value @24`; plus the reaction-list field(s) R-T1 adds when promises move to the guest (each a jsval or raw ref to a list of reaction records, which must be headered blocks with jsval fields) | none | no |
| `9` `TYPE_GENERATOR` | generator / async generator | prefix; jsval `env @32` (frame); raw `queue @40`, raw `queue_tail @44` (request records) | none | no |
| `10` `TYPE_DATE` | Date | prefix; `ms @16` is an `f64` | none | no (prefix only) |
| `11` `TYPE_ACCESSOR` | accessor pair | jsvals `getter @8`, `setter @16` | none | no |
| `12` `TYPE_PROPS` | own-property table | per entry `k < count @4` at `16 + 16k`: raw `key @+0` (`0` = tombstone; else a `TYPE_STRING` or `TYPE_SYMBOL` base), jsval `value @+8` (a data value or a boxed `TYPE_ACCESSOR`) | `cap` entries | no |
| `13` `TYPE_ELEMS` | element store | jsval slots `@8 + 8i` for **all** `i < cap @4` | `cap` slots | no |
| `14` `TYPE_SYMBOL` | symbol (incl. private names, well-known statics) | jsval `description @8` | none | no |
| `15` `TYPE_WEAKMAP` *new* | WeakMap handle | prefix; raw `entries @20` → `TYPE_ELEMS` of pairs, **ephemeron** (§5.2); raw `weak_next @16` is collector scratch, not traced | none | no |
| `16` `TYPE_WEAKSET` *new* | WeakSet handle | as `TYPE_WEAKMAP`; values unused (keys only, weak) | none | no |
| `17` `TYPE_WEAKREF` *new* | WeakRef | prefix; jsval `target @24` **weak**; `@16` weak-list link | none | no |
| `18` `TYPE_FINREG` *new* | FinalizationRegistry | prefix; jsval `cleanup @24` strong; raw `cells @20` → `TYPE_ELEMS` of triples `[target (weak), held (strong), token (weak)]`; `@16` weak-list link; `@32` u32 flags (`1` = cleanup job queued); raw `job_next @36` (traced as a registry while queued) | none | no |
| `19` `TYPE_RAW` *new* | internal raw buffer | none | `len @4` bytes from `@8` | **leaf** |
| `0xFF` `TYPE_FREE` *new* | free block (sweeper) | — (`next_free @4` raw, allocator-only) | | never traced |

Notes:

- **Why `TYPE_ELEMS` traces `0..cap`, not `0..len`.** Slots at and past the
  owner's `length` are "unused" (`abi.rs:581-586`), but `pop` does not clear
  the vacated slot (`emit/array.rs::emit_array_pop_node`, `emit/array.rs:344-377`). Tracing a slot one cycle and not
  the next would let a later cycle follow a pointer freed in between; tracing
  all of `cap` every cycle can only retain (a bounded leak), never dangle.
  Recommended companion change: `pop`/`shift`/`length` truncation write `HOLE`
  into vacated slots (truncation already does, DESIGN §5.7).
- **`TYPE_PROPS` traces `0..count`.** Entries past `count` were never written
  (zeroed at allocation, §3.2); tombstones have key `0` and value `undefined`.
- **Inline (co-allocated) tables.** `emit/object.rs::emit_object_node`
  allocates `16 + 16 + 16·cap` bytes and points `props` at `base + 16`
  (`emit/object.rs:38-45,58-75`; error objects are built the same way,
  `ir.rs:2498-2510`). The table is marked and traced through `props` like any
  table; its allocation is freed only with the object's. After growth the
  inline table is abandoned inside the live object (dead space, not traced).
  The sweep only ever examines allocation starts, so an interior table is never
  freed on its own.
- **Headerless allocations today** (fix F1). Two `__alloc` calls create blocks
  without a valid header, which the tracer and sweep cannot tolerate:
  the intern table growth copy (`emit/props.rs:525-526`, `cap·8` bytes of `u32`
  slots, slot 0 overlapping the header) and the `Math.sumPrecise` accumulator
  (`emit/builtins/number.rs:446-447`, `SUM_SLOTS · 8 = 576` bytes of `f64`s,
  held in an `i32` local across iterator steps that run user code). Both become
  `TYPE_RAW` blocks (header + `len`, data at `+8`); `$__intern_base` then
  points at `block + 8`, and the accumulator base local is rooted.
- **Request record link** (fix F2). The async-generator request record is a
  `TYPE_ENV` of 4 slots `[value, promise, kind, next]`, and `next` is written
  as a zero-extended raw `u32` (`emit/builtins/generator.rs:505-519`,
  `I64ExtendI32U` at `:517`), which is not a heap jsval, so generic env
  tracing would miss every record after the head. Write `HEAP | record`
  instead (`I64Or` with `HEAP_BITS`); the readers already `I32WrapI64`
  (`:395`). It is the only raw-`u32`-into-`i64` store in `emit/`
  (`grep 'I64ExtendI32U, I64Store'`). `kind` is a small integer (a denormal
  number, never a pointer).
- **Partially initialized blocks are reachable.** A block may be traced
  before its constructor finishes: the generator frame env is allocated, its
  `count` written, then each cell allocated and stored
  (`emit/generator.rs:51-83`); `__new_array` writes `len` before allocating the
  store (`emit/runtime.rs:343-360`); array literals allocate the store before
  evaluating elements. This is safe only because every unwritten field is
  `0` (number `+0`, or raw ref "none") at allocation time, which §3.2
  guarantees for reused memory too.
- **Mark stack.** An explicit stack of `u32` bases in a large-object span
  (§3.1), doubled on overflow; if doubling cannot get pages, set an overflow
  flag and finish with a linear rescan of all pages tracing every block whose
  `gc == epoch` until no new block is marked.

---

## 3. Allocation, sweeping, collection triggers, out-of-memory

### 3.1 Heap layout

```text
[0, SS_SIZE)                 shadow stack (grows down)                       §1.3
[SS_SIZE, static_end)        null guard word, FN_INFO pointer, static region
                             (DESIGN §4.2 order) + page table + static root list
[static_end, heap_start)     padding to a 64 KiB boundary
[heap_start, frontier)       64 KiB GC pages
```

- **Page table**: one byte per wasm page (8192 pages max → 8 KiB, static):
  `0` not heap, `1..=19` small-object page of size class `c`, `0xFE` large-span
  head, `0xFD` large-span tail, `0xFC` free page.
- **Small-object page**: a 16-byte page header (`class: u32`, `bump: u32`
  offset of the first never-allocated slot, `live: u32`, reserved), then
  slots of `size(c)` bytes from `page + 16` (8-aligned). No per-block size
  word: the size is the page's class (as DESIGN §5.7 planned).
- **Large span**: `k` contiguous pages for a request `> 2048` bytes; the head
  page holds a 16-byte pre-header (`pages: u32`, `next_large: u32`, reserved)
  and the block at `page + 16`. Large blocks are headered like any block, so
  the mark is the block's own byte 1.
- **Size classes** (19): `16, 32, …, 256` by 16 (16 classes), then `512`,
  `1024`, `2048`. Index: `size <= 256 → (size + 15) >> 4` (1-based), else
  `17 + ceil_log2(size) - 9` with `ceil_log2(size) = 32 - clz(size - 1)`
  (`257..=512` → 17, `513..=1024` → 18, `1025..=2048` → 19); the emitter
  precomputes the class for every constant-size call site (most sites,
  §3.6).

### 3.2 Free blocks and zeroing

- A free slot is `[type = 0xFF][gc = 0][0][0][next_free: u32 @4]`, threaded
  per class from the global free-list heads (a static `u32[19]` array).
- **Allocation zeroes the whole block** when it comes from a free list
  (`memory.fill(base, 0, size(c))`) and writes nothing extra when it comes
  from a page's never-used bump area or a fresh `memory.grow` page (already
  zero). This keeps today's implicit contract: callers write only the fields
  they set (`emit/value.rs::emit_object_prefix` writes header, `props = 0` and
  `proto` only, `emit/value.rs:10-25`), and §2 relies on zero being
  tracer-safe. Pages returned to the free-page pool are zeroed when reused
  (`memory.fill` of the page).

### 3.3 Sweep: eager

After marking and weak processing (§5), walk every small page from `page+16`
to its `bump`: a slot with `type != 0xFF` and `gc != epoch` becomes a free
slot pushed on its class list; count live slots; a page with `live == 0` goes
to the free-page pool (page table `0xFC`) and its slots leave the lists
(rebuild the lists page by page, so dropping a page is just not threading
it). Walk the large list: an unmarked span returns its pages to the pool.

Eager, not lazy: lazy sweeping needs "allocated since the last mark" state per
page and complicates the weak-processing order; eager sweep cost is linear in
heap size, which the budget rule (§3.4) amortizes against allocation. Lazy
sweep is a phase-6 option.

The bump allocator does not survive as the general path: `$__heap_ptr`
becomes the page **frontier** (end of the highest page ever obtained) and
`memory.grow` happens only when the frontier needs new pages. A per-page bump
cursor is the fast path while a class's current page has never-used slots.

### 3.4 When to collect

New globals: `$__gc_debt: i32` (bytes left before the next collection),
`$__gc_live: i32` (bytes live after the last collection), `$__gc_epoch`.

| step in `__alloc(size)` | rule |
|---|---|
| 1. budget | `debt -= size(c)` (or the span size); if `debt < 0` and not already collecting → **collect**, then `debt = max(GC_MIN_BUDGET, live)` (heap ≈ 2 × live; `GC_MIN_BUDGET = 4 MiB`) |
| 2. take | class free list → current page bump → free-page pool → grow |
| 3. grow | `memory.grow` of the needed pages (doubling as today, `emit/runtime.rs:55-87`) |
| 4. collect-before-OOM | if grow fails and no collection ran for this request → collect, retry 2 |
| 5. OOM | still no memory → the out-of-memory path (§3.5) |

Collect-before-grow is the budget rule itself: memory grows only while the
debt since the last collection is not spent, i.e. while the heap is below
about twice the live size. `GC_MIN_BUDGET = 4 MiB` keeps every opcount corpus
program (§4) collection-free; test coverage comes from the `gc_stress` compile
option (§6) that sets the budget to `0` (collect at every allocation).

A collection never runs user code (weak cleanup only *enqueues* jobs, §5.4),
never allocates through `__alloc` (the mark stack takes pages directly), and
is not re-entrant (a flag).

### 3.5 Out of memory: catchable `RangeError`

Today OOM stores the sticky `HEAP_PTR_OOM` sentinel and traps
(`emit/runtime.rs:82-86`, `abi.rs:820-824`), and the run stage reports
`Uncaught RangeError: out of memory` (DESIGN §5.6); it is not catchable.
With a collector, OOM means the live heap really is near 512 MiB, and it
becomes a thrown `RangeError`:

- **Error object.** A static, preallocated `CLASS_ERROR` block in the realm
  image, `%OutOfMemoryError%`: `proto = %RangeError.prototype%`, own
  non-enumerable `message = "out of memory"`, `stack = "RangeError: out of
  memory"`. Throwing cannot allocate, so every OOM throws this same object
  (identity is shared; acceptable, OOM is implementation-defined).
- **Emergency reserve.** `GC_RESERVE = 256 KiB` of pages held back from
  normal allocation. On OOM `__alloc` sets `$exc_value = %OutOfMemoryError%`,
  `$exc_pending = 1`, and:
  - for a request `<= 2048` bytes returns a zeroed block carved from the
    reserve, so the caller's unconditional field writes stay in bounds and its
    next existing exception check unwinds;
  - for a larger (variable-size) request returns `0`. The variable-size
    allocators are few and shared: `emit/runtime.rs::emit_alloc_str` (strings),
    `emit_new_elems` (stores), the props growth (`emit/props.rs:812-825`),
    the intern growth, `TYPE_ENV` of a runtime count, `TYPE_RAW`; each checks
    `0` and returns a valid dummy (the empty string / `0` store / no growth)
    with the exception left pending.
- **Propagation.** Allocation becomes a throwing operation: in user code
  every call of an allocating helper is followed by
  `emit/function.rs::emit_propagate_if_pending` (most already are, because
  those helpers can run user code; the pure allocators `__concat`,
  `__new_array`, `__str_index`, `__index_string`, `__array_reserve`,
  `__array_push` gain the check). Builtin and helper bodies propagate at their
  existing checks; a dummy value never reaches an observable side effect
  before a check because every helper that writes into user-visible state
  after an allocation already checks `$exc_pending` after its own nested
  calls (the M2 audit item; §6).
- **Reserve exhausted while OOM is pending** (a helper allocating in a loop
  without checking) → store `HEAP_PTR_OOM` in a sticky global and `unreachable`;
  the run stage keeps reporting `Uncaught RangeError: out of memory` for that
  case only. The next successful collection re-reserves `GC_RESERVE`.
- `HEAP_PTR_OOM`, the `__heap_ptr` export and `oom_guard` (DESIGN §5.6) exist
  for the hosts; the R-phase deletes them with the hosts (no compatibility).

### 3.6 Fast path

Constant-size sites (every fixed-layout block: objects, functions, cells,
accessors, symbols, generators, Map/Set/Date/promise handles, array handles,
literal-sized stores and envs) call a per-class entry `__alloc_c(k)` (or pass
the precomputed class): pop the class list head (≈ 6 ops), zero it with
`memory.fill` (4 ops), decrement the debt and test it (5 ops); otherwise bump
in the current page. Today's fast path executes about 25 instructions
(`emit/runtime.rs:30-104`); the new fast path is estimated at 20-30.

---

## 4. Cost model

Costs per executed event (instructions = wasmtime fuel units; estimates from
the instruction sequences above):

| event | ops |
|---|---|
| frame push + pop (`N <= 4` slots, cleared by stores) | 6 + 3N + 2 |
| frame push + pop (`N > 4`, `memory.fill`) | 12 |
| write-through of a rooted `i64` local | 3 |
| write-through of a rooted `i32` base | 6 |
| operand-stack temp root (`i64` / `i32`) | 4 / 7 |
| allocation fast path vs today | −5 … +5 (+4 for `memory.fill` on free-list reuse) |
| propagate check after a pure allocator in user code | 2 (no exception) |
| a collection | ≈ 8 per live jsval slot + ≈ 6 per page slot swept |

No corpus row reaches `GC_MIN_BUDGET` (the largest, `push_1e5`, peaks near
2 MiB: a `2^17`-slot store = 1 MiB plus the abandoned smaller stores), so the
rows below carry no collection cost. Rows from
`crates/jsc-core/tests/opcount/snapshot.tsv` (fuel at `c34cd36`); executed
events counted by reading each corpus program (`tests/opcount/*.js`),
**unmeasured**:

| row | fuel today | dominant new costs | est. Δ fuel | est. Δ % |
|---|---|---|---|---|
| `fib` | 3 601 020 | 21 891 calls × (frame 2 slots: `n`, the first call's result temp) ≈ 21 ops | +460 k | +13 % |
| `push_1e5` | 20 749 986 | `i` NoRoot (elided); `a` written once; `__array_push` shrink-wrapped (frame only on the 17 growths); 1e5 × propagate check 2 | +0.2 M | +1 % |
| `array_hofs` | 22 907 | 4 closures (frame-free leaves), 120 callback calls via inline HOF loops (rooted HOF bases: receiver, callback, result in `main`, ≈ 6 ops per loop entry), 30 pushes | +1.5 k | +7 % |
| `class_methods` | 58 552 | ≈ 45 calls (ctors, `super`, methods, getter, setter) × ≈ 20; object allocs ≈ 25 | +1.5 k | +3 % |
| `console_log` | 1 503 | 11 `print` calls, no user functions | +50 | +3 % |
| `for_in` | 19 683 | 20 key iterations, `keys = keys + k` (rooted write-through 3, concat check 2) | +0.3 k | +2 % |
| `for_of` | 157 813 | 100 generator steps (SM frames) + 120 pushes + 20 `Map.set` + 26 chars | +5 k | +3 % |
| `generators` | 163 734 | 211 `next()` calls (SM frame ≈ 15) + 211 result objects | +5 k | +3 % |
| `json` | 30 256 | parser recursion (≈ 20 `__json_value` frames) + stringify | +1 k | +3 % |
| `map_set` | 45 517 | 40 `set`/`add`, 14 `has`/`get`, 27 string concats | +1.5 k | +3 % |
| `math` | 54 375 | `acc`, `i` NoRoot; no calls | ≈ 0 | 0 % |
| `object_reads` | 18 607 | `acc`, `i` NoRoot | ≈ 0 | 0 % |
| `spread` | 112 920 | 100 generator steps + 300 element appends + 1 spread call | +4 k | +4 % |
| `string_methods` | 45 060 | 10 iterations × 5 allocating string calls, rooted `out` | +1 k | +2 % |

**Code size (`user_ops`).** Per function with a frame: prologue 6 + clear
(≤ 12) + 2 per return site; +3 per write of a rooted local; +4 per temp root.
Estimated `user_ops` growth 5-15 % per row (`fib` 346 → ≈ 380: one frame, two
returns, one temp). Module-wide, the ≈ 200 runtime helpers and builtin bodies
(`Builtin::ALL`, 142 entries, plus the helpers of DESIGN §6.2) add an
estimated 6-10 k instructions (frames, write-throughs, epilogues at the ≈ 240
`Return`s), about +4-8 % of a typical module's code section.

**Is a no-GC fast path provably safe?** A *program-wide* "never collect, never
spill" mode is **rejected**. Its proof obligation is a static bound
`B` on total bytes allocated over the whole run with
`static_end + SS + B <= GC_MIN_BUDGET`, which requires: no allocation site
inside a loop, a recursive call cycle (including any `call_indirect`, whose
callee set is unknown), a callback reachable from a builtin, or a microtask;
and a constant size at every allocation site (builtins allocate
data-dependently: `repeat`, `join`, `JSON.parse`, `Array(n)`, number
formatting). Only programs too trivial to matter satisfy it, and the analysis
would be a second source of truth for allocation behaviour. What *is* provably
safe, and adopted, is the per-function elision of §1.6: a function with no
safepoint needs no frame (no collection can start while it runs), NoRoot
locals need no slot, and helpers shrink-wrap their frames. Those capture the
loop-heavy numeric rows (`math`, `object_reads`, `push_1e5` ≈ 0 %).

---

## 5. Weak collections

### 5.1 Surface (ECMA-262 §24.3, §24.4, §26.1, §26.2)

New realm intrinsics (`emit/realm.rs`, appended to the intrinsic registry): `%WeakMap%`, `%WeakMap.prototype%` (`delete`, `get`, `has`,
`set`, `@@toStringTag "WeakMap"`), `%WeakSet%`, `%WeakSet.prototype%` (`add`,
`delete`, `has`, `@@toStringTag`), `%WeakRef%`, `%WeakRef.prototype%`
(`deref`, `@@toStringTag`), `%FinalizationRegistry%`,
`%FinalizationRegistry.prototype%` (`register`, `unregister`,
`@@toStringTag`). Constructors are `CONSTRUCTOR | SELF_ALLOC` (allocate from
`new.target`, like `Map`, DESIGN §5.5), throw `TypeError` when called without
`new`; `WeakMap`/`WeakSet` take an iterable through the same adder protocol as
`Map`/`Set` (`emit/builtins/map_set.rs::emit_ctor`, `call_adder`). Global
object entries for the four constructors.

**`CanBeHeldWeakly(v)`**: `v` is an object-family block (any of `TYPE_ARRAY`,
`TYPE_OBJECT`, `TYPE_FUNCTION`, `TYPE_MAP`, `TYPE_SET`, `TYPE_PROMISE`,
`TYPE_GENERATOR`, `TYPE_DATE`, and the four new kinds), or a `TYPE_SYMBOL`
without `SYMBOL_FLAG_REGISTERED` (`abi.rs:623`). Well-known symbols (static)
qualify and are simply never collected; private names never reach user code.
Otherwise `TypeError` ("Invalid value used as weak map key" / "... in weak
set" / "WeakRef: target must be an object" / "FinalizationRegistry.register:
invalid target"). This also implements the `symbols-as-weakmap-keys` feature.

### 5.2 WeakMap / WeakSet: ephemerons

Layout: object-family prefix, `weak_next: u32 @16` (collector scratch),
`entries: u32 @20` (raw ref to a `TYPE_ELEMS` of `(key, value)` pairs, `0` =
none), `live: u32 @24`, pad; 32 bytes. Lookup is a linear scan by identity
(keys are objects or symbols, so `SameValue` is bit equality and no
`__same_value_zero` is needed); `delete` tombstones the pair (`HOLE` key,
`undefined` value); `set` of a new key reuses the first tombstone or appends
(growth like `__array_reserve`). WeakMaps are not iterable, so the collector
may compact freely.

Marking:

1. Tracing a `TYPE_WEAKMAP`/`TYPE_WEAKSET` handle marks its prefix fields and
   the `entries` store block itself (so it is not swept) but **not** the
   pairs, and links the handle into the cycle's `weak_maps` list via
   `weak_next`.
2. After the mark stack drains, the **ephemeron fixpoint**: repeat { for each
   handle in `weak_maps`, for each live pair whose key is marked (or static)
   and whose value is an unmarked heap pointer: mark the value; drain the mark
   stack } until a full pass marks nothing. Worst case `O(pairs × rounds)`;
   acceptable for phase 4.
3. Clearing (after the fixpoint, before WeakRef/registry processing and
   before the sweep): every pair whose key is an unmarked heap block is
   removed; the store is compacted in place (live pairs slide down, the tail
   is rewritten `HOLE`/`undefined`), `live` updated.

### 5.3 WeakRef and KeepDuringJob

Layout: prefix, `weak_next: u32 @16`, pad, `target: i64 @24`; 32 bytes.

- Tracing a `TYPE_WEAKREF` marks its prefix but not `target`, and links it
  into `weak_refs`. After the ephemeron fixpoint, every `weak_refs` entry with
  an unmarked heap target gets `target = undefined`.
- `deref()` returns `target`; when it is not `undefined` it performs
  `AddToKeptObjects(target)`.
- **`[[KeptAlive]]`**: a strong guest list rooted in a new global
  `$__kept_objects` (an internal `TYPE_ARRAY`). `new WeakRef(t)` and a
  successful `deref()` push `t`.
- **`ClearKeptObjects`** runs at each microtask **checkpoint**: when the guest
  microtask queue (R-T1) becomes empty after a drain (the post-`main` drain and
  each top-level-`await` drain), set the list length to `0` writing `HOLE`
  into the vacated slots. This matches HTML's "perform a microtask checkpoint"
  (clear kept objects after running all microtasks); clearing after every
  single job would also conform but makes `deref()` results within one
  checkpoint unstable for no benefit.

### 5.4 FinalizationRegistry

Layout: prefix, `weak_next: u32 @16`, `cells: u32 @20` (raw ref to a
`TYPE_ELEMS` of triples), `cleanup: i64 @24` (the callback, strong), `flags:
u32 @32` (`1` = a cleanup job is queued), `job_next: u32 @36` (link in
`$__fin_jobs` while queued); 40 bytes.

- `register(target, held, token)`: `CanBeHeldWeakly(target)` else `TypeError`;
  `SameValue(target, held)` → `TypeError`; `token` must be `undefined` or
  `CanBeHeldWeakly`; append `[target, held, token]` (token `undefined` when
  absent).
- `unregister(token)`: `CanBeHeldWeakly(token)` else `TypeError`; remove every
  cell whose token is `token` (bit equality); return whether any was removed.
- Tracing a `TYPE_FINREG` marks `cleanup`, the `cells` store block, and every
  cell's `held` (strong, per spec), but not `target` or `token`; links into
  `weak_regs`.
- After the ephemeron fixpoint and WeakRef clearing: for each registry, each
  cell with an unmarked heap `target` sets `target = HOLE` ("empty", pending
  cleanup); each cell with an unmarked heap `token` sets `token = undefined`
  (it can no longer be passed to `unregister`). If any cell became empty and
  `flags & 1 == 0`, set the flag and append the registry to the strong guest
  list `$__fin_jobs` (`HostEnqueueFinalizationRegistryCleanupJob`). The list is
  intrusive through `job_next` (head in the `$__fin_jobs: i32` global, a raw
  root traced as a registry), so enqueueing never allocates during a
  collection.
- **Job scheduling (host-agnostic).** The guest job loop, after the microtask
  queue empties (and after `ClearKeptObjects`), pops registries from
  `$__fin_jobs`, clears their flag, and runs `CleanupFinalizationRegistry`:
  for each empty cell, remove it and call `cleanup(held)`; a throw is reported
  like an uncaught exception in a job. Then it drains microtasks again; repeat
  until both queues are empty. No host involvement, no timers.
- `cleanupSome` is not standard (a separate proposal) and is not added.

Order inside a collection: mark roots → drain → ephemeron fixpoint → clear
WeakMaps/WeakSets → clear WeakRefs → process registries → tombstone the
intern table → sweep. The sweep frees exactly the blocks still unmarked, so
no cleared slot can be followed later.

### 5.5 test262 impact

Read-only counts over `/srv/work/jsc/.tmp/test262/test` against
`tests/test262/baseline.txt` (28037 passing paths at `c34cd36`) and
`tests/test262/unsupported-features.txt` (fixtures excluded):

| directory | files | passing today | eligible today (all failing: no constructor) | skipped only by weak flags | skipped by other flags (`Reflect.construct`, `Reflect`, `cross-realm`) |
|---|---|---|---|---|---|
| `built-ins/WeakMap` | 141 | 0 | 82 | 51 | 8 |
| `built-ins/WeakSet` | 85 | 0 | 65 | 15 | 5 |
| `built-ins/WeakRef` | 29 | 0 | 0 | 22 | 7 |
| `built-ins/FinalizationRegistry` | 47 | 0 | 0 | 40 | 7 |

Removing the flags `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry`
and `symbols-as-weakmap-keys` (plus the comment line above the latter) from
`unsupported-features.txt` makes 163 tests eligible: the 128 above plus
`built-ins/Map` 13, `built-ins/Set` 7, `language/statements` 7,
`built-ins/Object` 4, `language/expressions` 3, `staging/sm` 1. Feature tag
counts over the whole suite: `WeakMap` 79, `WeakSet` 34, `WeakRef` 37,
`FinalizationRegistry` 49, `symbols-as-weakmap-keys` 29; none passes. A
further 51 files outside the four directories mention a weak global without
the tag; none passes today.

`host-gc-required` (15 tests, none in the four directories) **stays**: those
tests call `$262.gc()`/`asyncGC`, a host hook jsc's harness assembly does not
provide (open question Q8).

Upper bound for M3: 82 + 65 + 163 = 310 tests (≈ +0.9 % of the 32889
eligible, plus whatever the 51 untagged files gain), all depending on the
API surface, not on observing collection: test262 cannot force a collection
without `host-gc-required`.

---

## 6. Staging hints for R-T3

Dependencies: M1 has none (the shadow stack is inert without a collector and
can land first). M2 requires the runtime-independence stage that moves
promises/microtasks and every host-allocating op (`__convert`, `__method`,
`print` formatting) into the guest, because host-held handles (§1.7) are
invisible roots. M3 requires M2 and the guest microtask queue.

### M1 — shadow stack (inert)

- **Files**: `emit/mod.rs` (the function builder: wraps
  `wasm_encoder::Function`, buffers instructions, owns root-slot and temp-slot
  allocation, intercepts `Return`/outermost `End`, emits the prologue last;
  `Helpers` gains the leaf set), `emit/function.rs` (prologue/epilogue in
  `emit_main`/`emit_function`; `emit_throw_return` and `emit_return_transfer`
  go through the builder), `emit/expr.rs`, `emit/object.rs`, `emit/array.rs`,
  `emit/generator.rs`, `emit/stmt.rs` (temp roots at n-ary sites),
  `emit/builtins/mod.rs` (`Frame` declares bases vs integers), every
  `emit/builtins/*.rs` and `emit/runtime.rs`, `emit/props.rs`,
  `emit/props/descriptor.rs`, `emit/json_parse.rs`, `emit/convert.rs`,
  `emit/numeric.rs` (declare jsval/base locals; shrink-wrap `__array_push`),
  `emit/sections.rs` (globals `$__sp`, `$__gc_epoch`; `MODULE_CELL_GLOBAL_BASE`
  at `emit/mod.rs:241` moves past the new globals; the static region starts at
  `SS_SIZE + 8`; initial memory pages + 16), `abi.rs` (`SHADOW_STACK_SIZE`,
  `HEAP_BASE`, `FN_INFO_ADDR`), a new `emit/roots.rs` (NoRoot analysis over the
  IR and the helper call graph), and fixes F1 (`TYPE_RAW`, abi `19`) and F2.
- **Hot files shared with runtime independence**: `emit/mod.rs`,
  `emit/sections.rs` (imports, exports, globals), `emit/runtime.rs`,
  `emit/builtins/promise.rs`, `emit/realm.rs`, `abi.rs`, `DESIGN.md`. Land M1
  before or after an R stage, never interleaved within the same files.
- **Tests** (`emit/tests.rs` unless noted): `shadow_sp_restored_after_return`,
  `shadow_sp_restored_after_uncaught_throw` (throw through 5 frames, read the
  `$__sp` global from the instance in the test; no export needed if the test
  inspects memory/globals through the test-only harness, else a
  `#[cfg(test)]` export), `shadow_sp_restored_after_caught_throw`,
  `leaf_function_has_no_frame` (parse the module with `wasmparser`, assert no
  `global.set $__sp` in a frame-free body), `noroot_loop_counter_not_spilled`,
  `frame_slots_cleared_on_entry`, `async_gen_request_next_is_heap_jsval` (F2),
  `intern_growth_block_is_type_raw` (F1); a run test for deep recursion
  (trap message). Every module the existing tests compile is validated by
  `wasmparser` (the memory note on invalid wasm slipping through).
- **Interim state**: always on; opcount snapshot updated with per-row reasons
  (§4); test262 unchanged (no semantic change).
- **Rough size**: 1500-2200 LOC changed (builder ≈ 400, analysis ≈ 300, the
  declaration sweep over helpers/builtins is the bulk).
- **Risks**: a missed root is silent until M2. Mitigation: M2's `gc_stress`
  mode; in M1 a `#[cfg(test)]` assertion in the builder that every `i64` local
  is either declared jsval or raw, and every `i32` local is declared base or
  integer (no default). Moving the shadow stack to `[0, SS_SIZE)` also means a
  stray read of address `0..8` (today the zero null guard) now reads stack
  data; every raw ref is already tested against `0` before use, but the M1
  review should grep the `props == 0` / `elems == 0` guards.

### M2 — collector

- **Files**: `emit/runtime.rs` (`__alloc` rewrite, per-class entries, the
  collector functions `__gc_collect`, `__gc_mark`, `__gc_trace`,
  `__gc_sweep`, emitted in wasm), a new `emit/gc.rs` for the tracer/sweeper
  (the per-kind table of §2 as one `match` over type bytes), `emit/props.rs`
  (intern tombstones §1.8), `emit/realm.rs` (static root list,
  `%OutOfMemoryError%`), `emit/sections.rs` (page table, free-list heads,
  reserve), `emit/array.rs` (`HOLE` on `pop`/`shift`), the OOM checks of §3.5
  in the variable-size allocators and pure-allocator call sites, `abi.rs`
  (`TYPE_FREE`, size classes, `GC_MIN_BUDGET`, `GC_RESERVE`), a compile option
  `gc_stress` (budget `0`; test-only), DESIGN §5.6/§5.7/§13.
- **Tests**: `gc_stress_run_suite` (every existing run test recompiled with
  `gc_stress`, same output), `gc_reclaims_churn` (allocate ~2 GiB total in a
  loop with a small live set; completes under the 512 MiB cap),
  `gc_oom_is_catchable_range_error` (`try { const a = []; for (;;)
  a.push('x'.repeat(1 << 20)) } catch (e) { e instanceof RangeError }`),
  `gc_keeps_operand_stack_temps` (`f(g(), h())` with `h` allocating heavily),
  `gc_keeps_partially_built_literals` (object/array literals whose element
  expressions allocate), `gc_generator_frame_survives`,
  `gc_async_gen_queue_survives`, `gc_static_proto_heap_props_survive`
  (`Object.prototype.x = {}` then churn), `gc_intern_tombstones_probe`
  (intern, drop, churn, re-intern same content), `gc_map_iterator_survives`,
  `gc_free_list_reuse_zeroes`; opcount `push_fuel_is_linear`
  (`tests/opcount.rs:228`) must still hold.
- **Interim state**: lands with collection enabled; test262 should not move
  (any change is a root bug); OOM tests in the suite (if any) may change from
  fatal to caught.
- **Rough size**: 1800-2600 LOC.
- **Risks**: missed roots in hand-written helpers (mitigated by `gc_stress`
  over the whole run suite and test262 sharded under `gc_stress` in CI once,
  not locally); collection pauses on large heaps (eager sweep, acceptable);
  the OOM-propagation audit (a dummy value written into user state before a
  check).

### M3 — weak collections

- **Files**: new `emit/builtins/weak.rs` (the four constructors and
  prototypes), `emit/realm.rs` (intrinsics, global entries, `Builtin::ALL`
  growth), `emit/gc.rs` (ephemeron fixpoint, weak clearing, registry
  processing), the guest job loop (R-T1's microtask drain: checkpoint hook for
  `ClearKeptObjects` and the finalization job queue), `abi.rs` (`TYPE_WEAKMAP`
  … `TYPE_FINREG`, layouts), the guest renderer (`WeakMap { <items unknown> }`,
  `WeakSet { <items unknown> }`, `WeakRef { … }`, `FinalizationRegistry {}`),
  `tests/test262/unsupported-features.txt` (remove the five flags),
  DESIGN §5.2/§6.4/§7.9/§13.
- **Tests**: `weakmap_basic_api`, `weakmap_symbol_keys`
  (registered symbol rejected), `weakmap_entry_dropped_after_key_unreachable`
  (under `gc_stress`: observable through a `FinalizationRegistry` on the key,
  or through the internal `live` count read from memory in the test),
  `ephemeron_chain_fixpoint` (`wm.set(a, b); wm.set(b, c)`, only `a` rooted →
  all three survive), `weakref_cleared_after_checkpoint`,
  `weakref_kept_during_job`, `finreg_cleanup_runs_after_drain`,
  `finreg_unregister_token`, `finreg_target_equals_held_throws`; test262 dirs
  `built-ins/WeakMap`, `built-ins/WeakSet`, `built-ins/WeakRef`,
  `built-ins/FinalizationRegistry`.
- **Interim state**: one merge; baseline rises by up to ≈ 310.
- **Rough size**: 1300-1800 LOC.
- **Risks**: cleanup jobs observable ordering vs microtasks; collection inside
  the registry's own `register` (the cells store growth allocates: root the
  triple's values across it).

### Open questions (recommended default first)

| # | question | default |
|---|---|---|
| Q1 | Shadow-stack overflow: trap by address wrap, or an explicit check throwing a catchable `RangeError: Maximum call stack size exceeded`? | Wrap trap (0 ops; today's deep recursion already traps on the native wasm stack). The check costs ≈ 4 ops per frame push and needs a static error object; revisit in phase 5/6. |
| Q2 | Mark representation: epoch byte or bit + clearing pass? | Epoch byte (§1.2). |
| Q3 | `GC_MIN_BUDGET` and growth factor | 4 MiB, next budget = `max(4 MiB, live)`. |
| Q4 | Eager or lazy sweep | Eager in phase 4; lazy is phase-6 performance work. |
| Q5 | OOM error identity: one static object or a fresh object when memory allows? | One static `%OutOfMemoryError%`. |
| Q6 | WeakMap storage: linear pairs or an address-hashed table? | Linear pairs with in-collection compaction (keys are addresses, so hashing is easy later; not needed for test262). |
| Q7 | `ClearKeptObjects` timing | At each microtask checkpoint (queue empty), §5.3. |
| Q8 | Provide `$262.gc()` for `host-gc-required` tests? | No in phase 4; keep the flag. A test-only internal builtin could back it later. |
| Q9 | Compact Map/Set backings during collection (DESIGN §13 tombstone limitation)? | No: a running `forEach` holds a slot index in a wasm local that the collector cannot remap. Keep tombstones; revisit with an iteration counter on the handle. |
| Q10 | `gc_stress` as a compile option | Yes, test-only (`#[cfg(test)]` or an internal option not exposed by the CLI). |
| Q11 | Root `i64` scratch by default or by declaration? | By declaration with no default (the builder rejects an undeclared local), so raw integer `i64` locals such as the `Date` scratch can never be mistaken for jsvals. |

---

## 7. Corrections to DESIGN.md that the fold must carry

- §5.1: header byte 1 is the GC **epoch**, not a mark bit.
- §5.6: replaced by §3 (pages, size classes, free lists, budget, catchable OOM);
  "No free" and the sticky-sentinel-only OOM text are deleted.
- §5.7: delete "raw `i32` bases are re-derived from spilled jsvals after
  safepoints" (non-moving: they only need a rooted block); "every internal
  store a headered block" is false today (F1) until M1 lands; the Map/Set
  compaction sentence moves to the open question Q9; host-held roots: "none
  after the R-phase" plus the dependency in §6.
- §5.2: add the new kinds `15..19` and `TYPE_FREE`; Map/Set note "until a
  collector exists" becomes the Q9 decision.
- §13: drop "No garbage collection"; keep the Map/Set tombstone bullet.
- §14 phase 4: `WeakSet` belongs with `WeakMap`, `WeakRef`,
  `FinalizationRegistry`.
