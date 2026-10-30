# jsc Symbol, iteration protocol, and dual strings (phase 3 design)

## 0. Status and summary

Status: **approved 2026-10-30** (with the recommended defaults of §11);
**refreshed against `main @ be9114d`** (phase 3a merged) on 2026-10-30. The
defaults that changed in the refresh are listed in the next section; the
wave-3c implementation L2s follow this document literally.

Abbreviations: `core/` = `crates/jsc-core/src/`, `pkg/` =
`packages/jsc/src/`, `t262/` = `crates/jsc-test262/src/`. A citation is
`file` + function (`core/emit/props.rs` `emit_to_key`); line numbers are
avoided. Guest helpers are named `__x` as in DESIGN §6.2. Behaviour claims
about `be9114d` come from reading its source (the 3c campaign runs no local
tests or probes); the few examples quoted from the original D3 research
probes at `bbc496b` are marked "(bbc496b probe)" and were re-checked against
the code path, which is unchanged where they are quoted.

Scope:

- `Symbol` as a primitive, symbol-keyed properties, the registry, wrappers,
  and the 13 well-known symbols (which are honoured, in which stage).
- The generic iteration protocol (`GetIterator` / `IteratorStep` /
  `IteratorClose`) behind every consumer, with today's specialised paths kept
  as guarded fast paths; iterator prototypes; the `Iterator` global and its
  helpers; `Promise` combinators.
- Async iteration: `for await`, `yield*` in async generators,
  `%AsyncIteratorPrototype%`, `%AsyncFromSyncIteratorPrototype%`,
  `Symbol.asyncIterator`, `Array.fromAsync` (stage Y5, §5.8; moved in from
  phase 5 by the refresh, Q4).
- The Latin-1 / UTF-16 dual string representation: layout, literals,
  interning, every string operation in code units, host/shim boundary.

Out of scope (phase 5 or later, DESIGN §14): RegExp, BigInt, Proxy/Reflect,
typed arrays, private members, static blocks, object spread, `eval` /
`Function` constructor, the iterator proposals beyond ES2025
`iterator-helpers` (Q21).

Principles applied throughout:

- **No backward compatibility.** Every superseded interface is deleted in the
  stage that replaces it: the `IterSource`/`IterLen`/`IterItem`/`IsGenerator`
  IR nodes, `Builtin::IterSource`, `iter_snapshot`, the UTF-8 string layout,
  host `METHOD_OP_*` string ops that move into the guest and their refusal
  sentinels, `GEN_LIMIT_RETURN` / `GEN_LIMIT_THROW` and their `TypeError`s,
  the computed-class-member and `for await` compile errors. No aliases,
  flags, or shims.
- **Throw or be correct, never silently wrong.** Every behaviour that a stage
  makes reachable but does not implement throws a named `TypeError` until the
  stage that implements it (table in §9.4).

Stages (§9): **Y1** Symbol + symbol keys + computed class member names →
**Y2** iteration protocol → **Y4** well-known hooks → **Y3** `Iterator` +
helpers + `Promise` combinators (one L2, L2-Y); **U1** dual string flip →
**U2** Unicode semantics + string iterator → **Y5** async iteration (one L2,
L2-U); cross constraints Y1 → U1, Y2 → U2, Y2 → Y5 (§9.3). Estimated total
≈ **+3270** test262 passes (range 2755-3725) over `eee36ef`'s CI result
19378 / 30014, the eligible set growing by ≈ 2860 to ≈ 32870 (≈ 22650 /
32870 = 68.9 % after 3c, from 64.6 %); without Y5 ≈ +2145.

---

## Changed since approval (2026-10-30)

Defaults that changed in the refresh against `be9114d` (every other §11
default is unchanged; counts and citations were updated in place):

| Q | old default | new default | reason |
|---|---|---|---|
| Q4 | async iteration in phase 5 | **stage Y5 in wave 3c** (`for await`, async `yield*`, `%AsyncFromSyncIteratorPrototype%`, `Symbol.asyncIterator`, `Array.fromAsync`), owned by L2-U after U2; Y2-T1 adds `%AsyncIteratorPrototype%` and reparents `%AsyncGeneratorPrototype%` | the premise ("revisit if L2-L lands a working async generator object") came true in L2-L T5; ≈ 1730 eligible tests wait on async iteration, ≈ +1125 expected (§11 Q4) |
| Q19 | U1 waits for L2-F | **resolved: no wait** (U1 depends only on Y1) | L2-F landed in `24d466f`; its String methods are on `be9114d` and U1 ports them |

New questions added by the refresh: Q20 (Y5 ownership), Q21 (iterator
proposals beyond `iterator-helpers`). Stage plan changes that are not §11
defaults: the 2-L2 layout of §9.3 (L2-Y = Y1 → Y2 → Y4 → Y3, L2-U = U1 → U2 →
Y5); `GenResume` survives as the guarded direct state-machine step instead of
being deleted (§5.3).

---

## 1. Current state (main @ be9114d)

### 1.1 Values and tags

- One NaN-boxed `i64` per value; boxed iff `bits & QNAN == QNAN`, tag = bits
  `[50:48]` (`core/abi.rs` module docs, `QNAN`). Tags in use: `0b000`
  canonical NaN, `0b001` boolean, `0b010` `null`, `0b011` `undefined` (and
  the internal `HOLE = UNDEFINED | 1`, also the `METHOD_UNSUPPORTED`
  sentinel; `UNDEFINED | 2` / `| 3` are `METHOD_RANGE_ERROR` /
  `METHOD_NEEDS_REGEXP`), `0b101` the single heap tag `HEAP`. **Free:
  `0b100`, `0b110`, `0b111`** (`0b100` is "retired", `core/abi.rs` doc of
  `TYPEOF_STRINGS`).
- Decoders read a free tag as a NaN number: `core/run/value.rs` `decode_in`,
  `pkg/value.ts` `decodeIn`.
- `core/emit/value.rs` `emit_is_pointer` = QNAN mask ∧ tag `0b101`.
- `TYPEOF_STRINGS` has 6 words (`number` … `function`); index 6 is free.

### 1.2 Heap

- 4-byte header (`HEADER_SIZE`): byte 0 type, byte 1 `gc` (phase 4), byte 2
  `flags` (`HEADER_FLAGS_OFFSET`), byte 3 `class` (`HEADER_CLASS_OFFSET`);
  blocks 8-aligned (`BLOCK_ALIGN`). Object-family prefix: `props: u32 @4`,
  `proto: i64 @8`, payload `@16` (`OBJ_PREFIX_SIZE`). Object-family flag
  `OBJ_FLAG_NOT_EXTENSIBLE = 1` (L2-O); array flags `4..=32`, function flags
  `2..=128`.
- Type bytes `0..=13` in use (`TYPE_STRING` 0 … `TYPE_ELEMS` 13,
  `core/abi.rs`); **`14..=255` free**. `TYPE_OBJECT` classes `0..=6`
  (`CLASS_ORDINARY` … `CLASS_GLOBAL`); **`7..=255` free**. Wrappers keep
  `[[PrimitiveValue]]` at `WRAPPER_VALUE_OFFSET = 16`, 24-byte block
  (`WRAPPER_BLOCK_SIZE`).
- `TYPE_PROPS` table flags (`tflags @2`): `PROPS_HAS_INDEX_KEYS = 1`,
  `PROPS_HAS_NON_ENUM = 2`, `PROPS_WATCHED = 4`; **`8..=128` free**. Entry
  `attrs`: `ATTR_WRITABLE 1`, `ATTR_ENUMERABLE 2`, `ATTR_CONFIGURABLE 4`,
  `ATTR_ACCESSOR 8` (value = `TYPE_ACCESSOR` block).
- Generator handle (`TYPE_GENERATOR`): `sm_idx @16`, `state @20`, `done @24`,
  `limits @28` (`GEN_LIMIT_RETURN = 1`, `GEN_LIMIT_THROW = 2`,
  `GEN_RUNNING = 4`, `GEN_ASYNC = 8`, `GEN_AWAIT = 16`, `GEN_BUSY = 32`;
  **bits `6..=31` free**), `env @32`, async request queue head/tail
  `@40`/`@44` (`GEN_QUEUE_OFFSET`), 48 bytes (`GEN_BLOCK_SIZE`).

### 1.3 Property keys and descriptors

- Every key is a `u32` pointer to an interned `TYPE_STRING`; key equality is
  pointer equality (`core/emit/props.rs` `emit_prop_find`, `0` =
  tombstone).
- Intern table: open addressing, FNV-1a over UTF-8 bytes (runtime
  `emit_str_hash`, compile-time twin `fnv1a`, which must agree bit for bit),
  `emit_intern` / `emit_intern_slot`, globals 9-11 (DESIGN §5.3).
- `ToPropertyKey` = `core/emit/props.rs` `emit_to_key`: interned string (its
  `STR_FLAG_INTERNED` bit) → itself; integer `0 <= v < 2^32-1` →
  `__index_string`; **anything else → `__to_string`**, then intern. No
  symbol arm.
- Own keys: `emit_own_keys(obj, mode)` with `OWN_KEYS` / `OWN_VALUES` /
  `OWN_ENTRIES` (enumerable string keys) and `OWN_ALL` (every string key)
  (`core/emit/props.rs` constants); the integer-key test reads the key's
  `STR_FLAG_ARRAY_INDEX` bit **without checking the key's type byte**.
  Consumers: `Object.keys` / `values` / `entries` (`core/emit/object.rs`,
  `ObjectBuiltinOp`), `getOwnPropertyNames` / `getOwnPropertyDescriptors` /
  `assign` (`OWN_ALL`) and `defineProperties` (`OWN_KEYS`)
  (`core/emit/builtins/object_static.rs` `emit_get_own_property_names`,
  `emit_get_own_property_descriptors`, `emit_assign`,
  `object_define_properties`), JSON (`core/emit/builtins/json.rs`),
  `emit_for_in_keys`, `emit_rest`, renderers `core/run/value.rs`
  `table_entries` (decodes every key with `decode_string`, which does not
  check the type byte) and `pkg/value.ts` `decodeProps`.
- Descriptor model (L2-O, `core/emit/props/descriptor.rs`): `__to_desc`
  (`emit_to_desc`), `__define_own` (`emit_define_own`), `__own_desc`
  (`emit_own_desc`), `__set_integrity` / `__test_integrity` (walk every live
  entry, `emit_live_entries`), `__own_enum`, `__set_proto`. All take keys
  that already went through `__to_key` and read/write table entries; none
  inspects the key's kind, so they are key-agnostic.
- Realm registry entries have `key: &'static str`
  (`core/emit/realm.rs` `struct Entry`), so no intrinsic can own a
  non-string key.
- No `Symbol` global, no `@@` keys. `Symbol` appears only in comments and
  one message: `core/emit/convert.rs` `emit_to_primitive` doc,
  `core/emit/builtins/object.rs` `emit_to_string` doc,
  `core/emit/builtins/array.rs` `species_ctor` doc and `SPECIES_NOT_CTOR`
  (`"object.constructor[Symbol.species]"`). test262: `Symbol*` is denied in
  `tests/test262/unsupported-features.txt`.

### 1.4 Strings

- `TYPE_STRING`: `header | len: u32 @4 | UTF-8 bytes @8`; `len` and `.length`
  are **UTF-8 byte counts** (DESIGN §5.2, §13). Flags
  `STR_FLAG_INTERNED = 1`, `STR_FLAG_ARRAY_INDEX = 2`. No named offset
  constants: `core/emit/props.rs`, `core/emit/builtins/json.rs`,
  `core/emit/runtime.rs` `emit_str_eq`, `core/run/host.rs` `block_len` /
  `string_bytes`, `core/run/value.rs` `decode_string`, `pkg/shim.ts`
  `allocString`, `pkg/value.ts` `decodeString` each recompute
  `HEADER_SIZE + 4`, so the drift test cannot catch a mismatch.
- Literals: swc's `Wtf8Atom` is converted with `to_string_lossy()` in
  `core/codegen/expr.rs` (`Lit::Str` lowering, `lower_tpl`,
  `member_method_name`, `PropName::Str`) and `core/codegen/stmt.rs`
  (`prop_key_name`, `check_member_name`); `core/codegen/globals.rs`
  `visit_str` skips non-UTF-8. **A lone surrogate becomes U+FFFD at compile
  time** (silently wrong). `IrNode::Str(String)` cannot hold one.
  `core/emit/sections.rs` `collect_strings` / `emit_data_section` write one
  interned block per literal.
- Guest operations: `__concat` (`emit_concat`, always copies), `__str_eq`
  (length then byte loop, no pointer fast path), `__str_cmp` (UTF-8 byte
  order), `__str_index` (fresh 1-**byte** string), `__str_to_array` (per
  byte), `__json_escape`, `emit_gap`, JSON parse rejects every `\u` escape
  (`core/emit/json_parse.rs` `emit_json_string`). String wrappers define one
  own index property **per byte** eagerly (`core/emit/builtins/mod.rs`
  `emit_to_object`).
- `String.prototype` methods (L2-F, `core/emit/builtins/string.rs`) coerce in
  the guest, then call one host `__method` op (`METHOD_OP_INDEX_OF` 0 …
  `METHOD_OP_SPLIT` 16, `METHOD_OP_AT` 18 … `METHOD_OP_FROM_CODE_POINT` 28;
  `StringOp::flag` in `core/emit/realm.rs`; `core/run/host.rs`
  `method_dispatch`, shim twin in `pkg/shim.ts`). `method_dispatch` copies
  the whole linear memory per call. The host search ops already work in
  exact UTF-16 offsets for any text (`indexOf`, `lastIndexOf`, `includes`,
  `startsWith`, `endsWith`, `search`); `trim*` and `str_to_number` /
  `parse_int_str` / `parse_float_str` already use the JS whitespace set
  (`is_js_whitespace`); `toUpperCase` / `toLowerCase` are the full Unicode
  mappings on the host.
- **L2-F's non-ASCII refusals** (the interim guard of the UTF-8 layout): an
  op whose result depends on a byte position or length of non-ASCII text
  returns `METHOD_UNSUPPORTED`; the guest builtin throws `TypeError:
  String.prototype.<name> does not support non-ASCII strings yet` (texts in
  `core/emit/builtins/string.rs` `texts!` / `Texts::non_ascii`, the check
  after the host call in the same file; `may_refuse` decides which inline
  call sites fall back to the throwing builtin). Refusing ops (host
  `core/run/host.rs` `method_dispatch` arms and helpers `index_exact`,
  `endpoint_exact`, `string_slice`, `string_substring`, `string_pad`,
  `string_code_at`, `string_split`, `replace_str`; shim twins): `at`,
  `charAt`, `charCodeAt`, `codePointAt` (index past a non-ASCII byte; `at`
  with a negative index on any non-ASCII string), `slice` / `substring`,
  `padStart` / `padEnd` (non-ASCII receiver or fill), `split("")` /
  `split` with a limit over non-ASCII text, `replace` / `replaceAll` with an
  empty pattern over astral text, `normalize` (any non-ASCII receiver).
  `String.fromCharCode` / `fromCodePoint` throw `TypeError: String.<name>
  cannot build a lone surrogate yet` (`FROM_CHAR_CODE_SURROGATE`,
  `FROM_CODE_POINT_SURROGATE`). `search` / `match` with a string pattern
  that has RegExp syntax return `METHOD_NEEDS_REGEXP` (a `TypeError`). U1
  removes every non-ASCII and lone-surrogate refusal (§7.5); U2 removes the
  `normalize` one.
- Still silently wrong for non-ASCII / astral / lone-surrogate input on
  `be9114d` (U1 fixes them): `.length` (`"é".length` → 2, `"😀".length` →
  4), `s[i]` and the string iteration of for-of / spread / destructuring /
  `Array.from` (per byte), `<` / `>` (`"\uFFFF" < "\u{10000}"` → `true`),
  equality of lone surrogates collapsed to U+FFFD at compile time
  (`"\uD800" === "\uDBFF"` → `true`), `sort` without a comparator (code-point
  order), `String` wrapper index keys and `length` (bytes), JSON
  `emit_gap` (10 bytes), and the search results' offsets disagreeing with
  `.length`. `JSON.parse` of a `\u` escape throws (correct-or-throw).

### 1.5 Iteration paths

Shared IR (`core/ir.rs`): `IterSource { value, live }`, `IterLen`,
`IterItem`, `IsGenerator`, `GenResume`. `IterSource` calls
`Builtin::IterSource` (`core/emit/realm.rs`) whose body is
`core/emit/builtins/iter.rs` `emit_iter_source`: a `String` wrapper is
unwrapped; an array is itself; a string is itself (live) or
`__str_to_array`; a sync generator (`emit_is_generator`, which excludes
`GEN_ASYNC`) is drained by `__gen_to_array`; a Map/Set is itself (live) or a
fresh pair/value array; `arguments` is sliced; anything else throws
`map_set::not_iterable` (`x is not iterable`). An async generator is not a
sync iterable (correct).

| consumer | today (`be9114d`) | spec gap |
|---|---|---|
| for-of | `core/codegen/stmt.rs` `lower_for_of` → `IsGenerator` ? `lower_for_of_generator` (`GenResume` loop) : `lower_for_of_index` (`IterSource` live + `IterLen`/`IterItem`); `for await` is a compile error (`for-await-of is not supported`) | no `@@iterator`, no `IteratorClose` |
| spread (array/call/new, `AggregateError` errors) | `core/emit/array.rs` `emit_array_spread` (string → `__str_to_array`, generator → `__gen_to_array`, else `IterSource`) then `__array_concat`; `AggregateError` spreads its argument (`core/emit/builtins/ctor.rs` `aggregate_error`) | no protocol |
| array destructuring (binding, assignment, params, for-of heads) | `core/codegen/stmt.rs` `lower_binding_pattern`, `lower_destructuring_assign`, `assign_to_target` via `iter_snapshot` (whole source copied first), then indexed reads | eager, no close; infinite generators run out of memory |
| `yield*` | `core/codegen/generator.rs` `lower_yield_delegate`: statement only (expression position is a compile error, `core/codegen/expr.rs` "`yield*` is only supported as a statement"); values forwarded, sent values / result value not modelled; rejected in async generators ("`yield*` in an async generator is unsupported") | not spec |
| Map/Set ctors | `core/emit/builtins/map_set.rs` `emit_ctor` (Map/Set live, array/string/generator snapshot, else `not_iterable`; `arguments` is not accepted) | no close on adder throw |
| `Array.from` (L2-F) | `core/emit/builtins/array.rs` `emit_from`: generator stepped by `GenResume` and closed (`close_on_throw` → `GeneratorReturn`) on an abrupt `mapFn`; array/string/wrapper/Map/Set/`arguments` walked through `IterSource`; any other object is array-like | no user `@@iterator` |
| `Object.fromEntries` (L2-O) | `core/emit/builtins/object_static.rs` `emit_from_entries` over `Builtin::IterSource` | no protocol, no close |
| `it.next()` call site | `core/codegen/op.rs` `"next"` → guarded `GenResume` fast path (`ProtoKind::Generator`, `core/emit/expr.rs` `emit_proto_kind_test`: no own properties, untouched `prototype` chain, clean `%GeneratorPrototype%` bit), else an ordinary method call | correct |
| `Promise.all/…`, `keys/values/entries`, `Iterator` | absent | — |
| `arguments` | `core/codegen/function.rs` `lower_arguments_object`: no own `@@iterator` | — |
| generators | `%GeneratorPrototype%.proto` and `%AsyncGeneratorPrototype%.proto` = `%Object.prototype%` (`core/emit/realm.rs` `Intrinsic::proto`); per-function `prototype` objects exist (L2-L) | spec: `%IteratorPrototype%` / `%AsyncIteratorPrototype%` |

**Silent-wrong iteration bugs still present on `be9114d`** (failing-first
tests land with their fix):

| # | bug | root cause (be9114d) | fixed in |
|---|---|---|---|
| B1 | an own `next` on a generator object is ignored by for-of, spread, destructuring, `yield*`, `Array.from`: `it.next = function(){ n++; return {done:true} }; for (const x of it) {}` leaves `n == 0`; `[...it].length` is the generator's count (spec `n == 1`, length `0`) (bbc496b probe; path unchanged) | the `IsGenerator` branches call the state machine directly (`core/emit/generator.rs` `emit_gen_resume`, `__gen_to_array`) without the guard the `it.next()` call site has | **Y2-T2** (generator fast path guarded like `ProtoKind::Generator`) |
| B2 | Map delete during for-of / `forEach` skips the next entry: `for (const [k] of m) { seen.push(k); if (k===1) m.delete(1) }` → `[1, 3]` (spec `[1, 2, 3]`) (bbc496b probe; path unchanged) | `delete` compacts the backing array (`core/emit/runtime.rs` `emit_map_delete`; DESIGN §5.2, §13) | **Y2-T5** (tombstones, §5.5) |
| B3 | leaving a for-of early (`break`, `return`, a throw) does not close the iterator: `function* g(){ try { yield 1 } finally { log("f") } } for (const x of g()) break` never runs the `finally` | `lower_for_of_generator` has no close path; no `IteratorClose` exists outside `Array.from` | **Y2-T2** (close on abrupt exit, §5.2) |
| B4 | an array pattern drains its source instead of stepping it: `[] = g()`, `let [] = g()`, `for (let [] of [g()])` run the generator body to completion (spec: `GetIterator`, then `IteratorClose` → the body never runs); `[,] = g()` / `[a] = g()` run past the first `yield` (spec: one step, then close); test262 `*-ary-ptrn-empty.js`, `*-ary-ptrn-elision.js`, `*-ary-ptrn-elem-ary-elision-*`, `*-ary-ptrn-elem-ary-empty-*`, `array-elision-*`, `array-empty-*` | `iter_snapshot` (`IterSource { live: false }` → `__gen_to_array`) in `lower_binding_pattern`, `lower_destructuring_assign`, `assign_to_target` | **Y2-T3** (lazy step + close, §5.2) |

### 1.6 Guards available today

`$__intrinsics_dirty` (`DIRTY_GLOBAL = 14`, `core/emit/mod.rs`), one bit per
watched intrinsic, set by `__set`/`__define`/`__delete`
(`core/emit/props.rs` `emit_mark_dirty`); bits `0..=13` used
(`core/emit/realm.rs` `Intrinsic::watch_bit`), **18 free** (L2-O / L2-F /
L2-L / L2-V took none). `core/emit/expr.rs` `emit_proto_kind_test` is the
receiver test (kind byte, `props == 0`, `proto ==` realm prototype or an
untouched per-function generator `prototype`, dirty bit clear unless
sealed). `core/codegen/globals.rs` `sealed` / `PROTOTYPE_REACHING` decide
the compile-time folding (DESIGN §6.4).

---

## 2. Symbol

### 2.1 Representation: a heap kind under the existing heap tag

`TYPE_SYMBOL = 14`, a heap block behind the `HEAP` tag (Q1).

- Property keys are `u32` block pointers, so a symbol must be a block anyway;
  under the heap tag the jsval's low 32 bits *are* the key and
  `__prop_find`/`__define`/`__get`/`__set`/`__has`/`__delete` and the
  descriptor helpers (§1.3) need no change.
- The phase-4 collector (DESIGN §5.7) keeps one pointer test (QNAN + heap
  tag).
- Rejected: a new NaN-box tag (`0b100`/`0b110`). Every "not a pointer" arm
  would treat a symbol as a number/boolean (`__get` returns `undefined`, the
  host decodes a free tag as NaN, `String(sym)` → `"NaN"`): silently wrong,
  and keys would still need a block with a type byte.

Cost: every site that equates "heap, not string" with "object" must exclude
`TYPE_SYMBOL`. The list is bounded (§2.3).

### 2.2 Layout

| offset | size | field |
|---|---|---|
| 0 | 1 | type `TYPE_SYMBOL = 14` |
| 1 | 1 | `gc` (0) |
| 2 | 1 | flags: `4` `SYMBOL_FLAG_REGISTERED`; **bits `1` and `2` always clear** |
| 3 | 1 | `class` (0) |
| 4 | 4 | reserved, always `0` |
| 8 | 8 | `description: i64` — a string jsval or `undefined` |

`SYMBOL_BLOCK_SIZE = 16`, `SYMBOL_DESCRIPTION_OFFSET = 8`.

Invariants:

- Flag bits `1`/`2` stay clear: `emit_own_keys` tests `STR_FLAG_ARRAY_INDEX`
  (`2`) on raw keys and `emit_to_key` tests `STR_FLAG_INTERNED` (`1`), so a
  symbol key never looks like an integer index or an interned string even on
  a path that forgot the kind check.
- `@4 = 0`: a symbol misread as an object-family block has "no own
  properties" (defence in depth for a missed audit site).
- The description is immutable; identity is the block address.
- Well-known symbols are static `TYPE_SYMBOL` blocks in the realm image
  (DESIGN §6.4), description a static string (`"Symbol.iterator"`); their
  addresses are compile-time constants like any intrinsic.

### 2.3 Audit list (heap ∧ not string ⇒ object)

| site | change |
|---|---|
| `core/emit/expr.rs` `emit_is_object` (every caller: `__to_primitive`, `__to_number`, Map entry checks, `instanceof` operands, return override, `Object(v)`, `species_ctor`, descriptor `ToPropertyDescriptor`, `Object.*` argument checks, …) | exclude `TYPE_SYMBOL` — one compare |
| `core/emit/object.rs` `emit_instanceof_node` (inline object check of `Get(C, "prototype")`) and `emit_type_byte_instanceof` | exclude `TYPE_SYMBOL` |
| `core/emit/expr.rs` `emit_typeof_node` | `TYPE_SYMBOL` → `"symbol"` |
| `core/emit/props.rs` `emit_get` | a symbol receiver continues at `%Symbol.prototype%` (like the number/boolean arms) |
| `emit_set`, `emit_has_lookup`, `emit_delete`, `emit_own_keys`, descriptor helpers' receiver checks (`Object.defineProperty(sym, …)` → `TypeError`; `Object.getOwnPropertyDescriptor(sym, k)` → `ToObject`) | primitive arms (`Cannot create property`, `in` TypeError, no own keys) |
| `core/emit/runtime.rs` `emit_loose_eq` | `sym == sym` identity; `sym == 1` / `sym == "x"` → `false` without coercion; `Object(sym) == sym` → `true` via `ToPrimitive` |
| `core/emit/convert.rs` `emit_to_string`, `core/emit/value.rs` `emit_to_number_helper`, `__compare` | throw (§2.4) before reaching the host |
| `core/emit/builtins/object.rs` `emit_to_string` | `[object Symbol]` for a symbol / `CLASS_SYMBOL` receiver |
| `core/run/value.rs` `heap_object` / `decode_heap`, `pkg/value.ts` `decodeHeap` (`ty !== STRING && ty !== ARRAY` ⇒ object) | symbol arm |

Already right: truthiness (heap non-string is truthy), strict equality
(`core/emit/numeric.rs` `emit_strict_eq`), SameValue (`emit_same_value` in
`core/emit/props/descriptor.rs`) and SameValueZero
(`core/emit/runtime.rs` `emit_same_value_zero`) compare non-strings by bits,
so Map/Set keys, `includes` and `Object.is` work unchanged.

### 2.4 Conversions

| operation | result | site |
|---|---|---|
| `typeof sym` | `"symbol"` | `core/abi.rs` `TYPEOF_STRINGS` gains `"symbol"` (index 6); `emit_typeof_node` |
| `ToString(sym)` (`""+s`, templates, `__add`, string builtins' coercions) | `TypeError: Cannot convert a Symbol value to a string` | `__to_string` symbol arm (`__add` inherits it) |
| `String(sym)` | `"Symbol(desc)"` (`SymbolDescriptiveString`) | `ConvertCall` String path and the String constructor builtin: symbol arm before `__convert` |
| `new String(sym)` | `TypeError` (via `ToString`) | — |
| `ToNumber(sym)` (arithmetic, unary `+`, relational, `Number(sym)`, `Math.*` coercions) | `TypeError: Cannot convert a Symbol value to a number` | `emit_to_number_helper`, `__compare` |
| `ToPropertyKey(sym)` | the symbol, no interning | `emit_to_key`: `TYPE_SYMBOL` → `v as u32` before the number / `__to_string` arms |
| `ToPropertyKey(Object(sym))` | `ToPrimitive(hint string)` → the symbol | `__to_key` re-checks the primitive result for a symbol |
| `ToObject(sym)` | `CLASS_SYMBOL` wrapper | `__to_object` (`core/emit/builtins/mod.rs` `emit_to_object`) |
| `ToBoolean(sym)` | `true` | unchanged |
| `JSON.stringify(sym)` / symbol-valued properties | `undefined` / omitted (array slot `null`) | `core/emit/builtins/json.rs` `emit_json_rep` (treat like a function value) |

Messages follow V8 (Q10); test262 checks only the constructor.

### 2.5 `Symbol()`, `Symbol.for`, `Symbol.keyFor`

- `Symbol(desc)`: a fresh block; `desc === undefined` keeps `undefined`, else
  `ToString(desc)` (throws for a symbol argument). With the construct marker
  it throws `TypeError: Symbol is not a constructor`; the intrinsic still has
  `CONSTRUCTOR` (spec `IsConstructor(Symbol)` is true; test262
  `isConstructor.js` checks it), so the body tests the `argc` construct
  marker like a class constructor. `length = 0`, `name = "Symbol"`.
- **Registry** (Q2): a hidden static object-family block in the realm image
  (not reachable from `GLOBALS`) whose `TYPE_PROPS` table maps interned key
  strings to registered symbols. `Symbol.for(k)`: `key = __intern(ToString(k))`;
  `__prop_find(registry, key)` hit → stored symbol; miss → new symbol with
  `REGISTERED`, `description = key`, `__define(registry, key, sym, 0)`.
  `Symbol.keyFor(s)`: `TypeError` unless a symbol; then
  `REGISTERED ? description : undefined` (O(1), no reverse map). The phase-4
  GC keeps registered symbols alive through the block, as the spec requires.

### 2.6 Wrappers and `%Symbol.prototype%`

- `CLASS_SYMBOL = 7` `TYPE_OBJECT` wrapper, 24 bytes, `[[SymbolData]]` at
  `WRAPPER_VALUE_OFFSET`; created only by `ToObject`. Method calls on a
  primitive symbol do not allocate (`__get` resolves at the realm prototype,
  DESIGN §6.3).
- `%Symbol.prototype%`: `BuiltinCtor::Symbol` joins `core/emit/realm.rs`
  `PLAIN_PROTOTYPES` as a watched entry (one dirty bit). Own: `constructor`,
  `toString` (`SymbolDescriptiveString(thisSymbolValue)`), `valueOf`,
  `description` getter (an accessor entry like the Map/Set `size` getters),
  `[@@toPrimitive]` (configurable only, returns `thisSymbolValue`),
  `[@@toStringTag] = "Symbol"` (configurable only). `thisSymbolValue`
  accepts a symbol or a `CLASS_SYMBOL` wrapper, else `TypeError`.
- `Symbol` statics: the 13 well-known symbols (non-writable, non-enumerable,
  non-configurable), `for`, `keyFor`. `Symbol` joins the sealed-global set
  (`core/codegen/globals.rs` `sealed`): `Symbol.iterator` through a sealed
  `Symbol` compiles to the static block address; the properties are
  non-writable/non-configurable, so no member guard is needed.

### 2.7 Rendering (both hosts)

Node style: `Symbol(a)`, `Symbol()`; in containers `[ Symbol(a) ]`; symbol
keys `{ [Symbol(a)]: 1 }` after the string keys; wrapper
`[Symbol: Symbol(a)]`; uncaught `Uncaught Symbol(a)`. Details in §7.

---

## 3. Symbol-keyed properties

### 3.1 Tables

- A symbol key is the symbol block's address in the `u32` key slot. No
  interning and no hashing: the intern table and `__str_hash` never see a
  symbol.
- New `TYPE_PROPS` flag **`PROPS_HAS_SYMBOL_KEYS = 8`**, set by `__define`
  (and `__define_own`) when the key's type byte is `TYPE_SYMBOL`, kept
  across rebuilds like `PROPS_WATCHED` (Q9). It lets string-key walkers skip
  the per-entry kind check, lets renderers skip symbol handling, and lets the
  well-known lookups of §4 short-circuit on tables that never held a symbol
  key.
- Realm registry: `Entry.key` becomes `Key::Str(&'static str) |
  Key::Symbol(WellKnown)` so intrinsics own `[@@iterator]`, `[@@toStringTag]`,
  `[@@toPrimitive]`, `[@@hasInstance]`, `get [@@species]`,
  `[@@asyncIterator]`. Builtin `name`s: `"[Symbol.iterator]"`,
  `"get [Symbol.species]"`.

### 3.2 `OrdinaryOwnPropertyKeys` ordering

Spec order: integer indices ascending, string keys in creation order, symbol
keys in creation order. Today `emit_own_keys` produces the first two; symbols
are a third pass over the slots (skipped when `HAS_SYMBOL_KEYS` is clear).

| mode | keys | consumers |
|---|---|---|
| `OWN_KEYS` / `OWN_VALUES` / `OWN_ENTRIES` / `OWN_ALL` (existing) | strings only (symbol keys skipped) | `Object.keys/values/entries`, `getOwnPropertyNames`, `for-in`, JSON, renderers |
| `OWN_SYMBOLS` (new) | symbols only | `Object.getOwnPropertySymbols` |
| `OWN_ALL_KEYS` (new) | strings, then symbols | `CopyDataProperties` (`__rest`), `Object.assign`, `defineProperties` (and `Object.create`'s `Properties`), `getOwnPropertyDescriptors` |

- `__rest` (`emit_rest`) copies enumerable symbol keys too.
- `for-in` / `Object.keys` / JSON skip symbols; the `emit_for_in_keys`
  shadowing set compares key pointers, and a symbol never equals a string
  pointer, so it needs no change once `OWN_KEYS` filters symbols.
- Exotic own keys (arrays, strings, `arguments`) have no symbol exotics;
  their tables may hold symbol keys like any object.

### 3.3 Interaction with the landed descriptor model (L2-O)

L2-O is on `be9114d` (`core/emit/props/descriptor.rs`,
`core/emit/builtins/object_static.rs`, §1.3).

- Every `Object` static and `Object.prototype` method takes keys through
  `__to_key`, so once Y1 adds the symbol arm, `defineProperty`,
  `getOwnPropertyDescriptor`, `hasOwn`, `hasOwnProperty`,
  `propertyIsEnumerable`, `__defineGetter__` / `__lookupGetter__` work on
  symbol keys with no change.
- Own-key enumeration goes through `__own_keys` modes except the integrity
  helpers, which walk every live entry (`emit_live_entries`) and are
  key-agnostic (sealing or freezing a symbol-keyed property is correct as
  is). Y1-T4 switches `emit_get_own_property_descriptors`, `emit_assign` and
  `object_define_properties` to `OWN_ALL_KEYS` and adds
  `Object.getOwnPropertySymbols` as a new `ReflectOp`
  (`GetOwnPropertySymbols`, `OWN_SYMBOLS`) beside
  `emit_get_own_property_names`.
- Property attributes (`core/abi.rs` attrs) are key-agnostic.
- `Object.prototype.toString` (`core/emit/builtins/object.rs`
  `emit_to_string`) still tags Map/Set/Promise/Generator by kind byte (L2-O
  did not change it); Y4 replaces the kind shortcuts with `@@toStringTag`.
- **String wrapper exotic `[[GetOwnProperty]]`**: `emit_to_object` defines
  every index eagerly (per byte) and the descriptor helpers read those table
  entries. U1 deletes the eager definition and adds one shared guest helper
  `__str_exotic_index(o, key) -> i32` (the code-unit index when `o` is a
  `CLASS_STRING` wrapper and `key` is a canonical array index `<` its length,
  else `-1`), consulted first by `__get`, `__has`, `__own_keys` (the
  indices `0..len` before the table's keys), `__own_desc`
  (`{value: s[i], writable: false, enumerable: true, configurable: false}`),
  `__define_own` (validates against that virtual non-configurable entry),
  `__own_enum`, `__set` (non-writable: `TypeError` in strict code),
  `__delete` (`TypeError`), and `__test_integrity` (indices count as frozen)
  (Q16). `length` stays a real non-writable own property (O(1)).

### 3.4 Computed keys

- Object literals already lower computed keys
  (`core/codegen/expr.rs` `lower_object`, `PropKey::Computed` → `__to_key`).
  Needed in Y1: run-time function `name` from a symbol key (`"[desc]"`,
  `"get [desc]"`, `""` for an undefined description); today
  `static_key_name` yields `""` for a computed key, which is silently wrong
  for symbol and string computed keys alike.
- Classes reject computed member names (`core/codegen/stmt.rs`
  `reject_out_of_scope_method` — "computed class method/accessor names are
  not supported" — and `reject_out_of_scope_field` — "computed class field
  names are not supported"); L2-L / L2-V did not touch them. On
  `be9114d`, **371** eligible failing tests (private-member tests excluded)
  contain a class body with a computed member name, plus **53** tests that
  Y1 makes eligible (§9.1); without them a class cannot be iterable
  (`class A { *[Symbol.iterator]() {} }`). DESIGN §14 lists them in phase 5;
  Q3 pulls them into Y1-T5.

### 3.5 Fast paths

- `ProtoGuard` fast paths require `props == 0` (`emit_proto_kind_test`); an
  own symbol key already disables them. No change.
- intrinsics-sealed (`PROTOTYPE_REACHING`): the new intrinsics are reachable
  only through `prototype` / `getPrototypeOf` / `__proto__` /
  `setPrototypeOf`, already listed, so the analysis stays sound.

---

## 4. Well-known symbols

All 13 exist as `Symbol.*` from Y1 (test262 checks their presence and
attributes). "Guard until" is the named `TypeError` a consult site throws in
the stages before it honours a user value (§9.4).

| symbol | spec consult sites | jsc site to change | honoured in | fast-path guard |
|---|---|---|---|---|
| `@@iterator` | `GetIterator`: for-of, spread, array destructuring, `yield*`, `Array.from`, Map/Set ctors, `Object.fromEntries`, `AggregateError`, `Promise.*` combinators, `Iterator.from`, `CreateAsyncFromSyncIterator` | `core/emit/builtins/iter.rs` `emit_iter_source`, `core/emit/array.rs` `emit_array_spread`, `core/codegen/stmt.rs` `lower_for_of` / `iter_snapshot`, `core/codegen/generator.rs` `lower_yield_delegate`, `core/emit/builtins/map_set.rs` `emit_ctor`, `core/emit/builtins/array.rs` `emit_from`, `core/emit/builtins/object_static.rs` `emit_from_entries` | **Y2** (guard Y1) | §5.3 |
| `@@asyncIterator` | `GetIterator(async)`: `for await`, async `yield*`, `Array.fromAsync`; `%AsyncIteratorPrototype%[@@asyncIterator]` | `lower_for_of` (`for await` rejected), `lower_yield_delegate` (async rejected) | **Y5** (§5.8); defined in Y1; `%AsyncIteratorPrototype%` in Y2-T1 | — (generic protocol only) |
| `@@hasInstance` | `InstanceofOperator` before `OrdinaryHasInstance`; `%Function.prototype%[@@hasInstance]` | `core/emit/object.rs` `emit_instanceof_node`, `core/codegen/expr.rs` `instanceof_kind` | **Y4** (guard Y1) | sealed builtin RHS type-byte arms unchanged (overriding needs `defineProperty` on the ctor or a class `static [Symbol.hasInstance]`, which shadows the name so `sealed` is false); generic arm: `HAS_SYMBOL_KEYS` test on the RHS's own table, then compare with the known builtin value |
| `@@toPrimitive` | `ToPrimitive`; `Date.prototype[@@toPrimitive]`, `Symbol.prototype[@@toPrimitive]` | `core/emit/convert.rs` `emit_to_primitive` (the Date hint special case becomes the real `Date.prototype[@@toPrimitive]`) | **Y4** (guard Y1) | walk the proto chain testing `HAS_SYMBOL_KEYS` per table; do the real `__get` only at the first table that has it |
| `@@toStringTag` | `Object.prototype.toString`; builtin tags (Map/Set/Promise/Generator/AsyncGenerator/Symbol/Math/JSON/iterator prototypes) | `core/emit/builtins/object.rs` `emit_to_string` (kind table) | **Y4** (guard Y1) | same chain test; fallback is the builtin-tag table; builtin tags installed as real properties and the kind shortcuts for Map/Set/Promise/Generator deleted (an async generator then tags `AsyncGenerator`, fixing the DESIGN §13 item) |
| `@@species` | `ArraySpeciesCreate` (`map/filter/slice/splice/concat/flat/flatMap`), `SpeciesConstructor` (`Promise.prototype.then/finally`) | `core/emit/builtins/array.rs` `species_ctor` (today walks `C`'s chain for a builtin ctor as a stand-in), `core/emit/builtins/promise.rs` (no `SpeciesConstructor` today) | **Y4** for Array and Promise (guard Y1) | array `ProtoGuard` path proves `proto == %Array.prototype%` with clean bit, hence `constructor === %Array%`; `%Array%[@@species]` / `%Promise%[@@species]` need new ctor dirty bits |
| `@@isConcatSpreadable` | `Array.prototype.concat` | `core/emit/builtins/array.rs` `Builtin::ArrayConcat` body (spreads by `IsArray` only) | **Y4** (guard Y1) | array receiver with `HAS_SYMBOL_KEYS` clear and clean `%Array.prototype%` bit → `IsArray` |
| `@@unscopables` | `with` only | `with` rejected (DESIGN §8) | documented only; Y4 installs the `Array.prototype[@@unscopables]` null-proto data object (test262 checks it) | — |
| `@@match` | `IsRegExp` in `startsWith`/`endsWith`/`includes`, `String.prototype.match` | `core/emit/builtins/string.rs` (`emit_method`, the `Match` / `Search` arms) | **Y4** for object arguments (RegExp itself out of scope) (guard Y1) | primitive arguments skip the lookup |
| `@@matchAll`, `@@replace`, `@@search`, `@@split` | `String.prototype.matchAll/replace/replaceAll/search/split` with an object argument | `core/emit/builtins/string.rs` | **Y4** (guard Y1) | primitive arguments skip the lookup |

---

## 5. Iteration protocol

### 5.1 Operations

- **`GetIterator(obj, sync)`** → record `{iterator, next, done}`:
  `method = GetMethod(obj, @@iterator)`; `undefined` ⇒ `TypeError: x is not
  iterable` (today's texts, `map_set::not_iterable`);
  `iterator = Call(method, obj)`; not an object ⇒ `TypeError: Result of the
  Symbol.iterator method is not an object`; `next = Get(iterator, "next")`,
  cached (later changes to `next` are unobservable).
- **`IteratorStep`**: `result = Call(next, iterator)`; not an object ⇒
  `TypeError: Iterator result x is not an object`; `done =
  ToBoolean(Get(result, "done"))`; `IteratorValue = Get(result, "value")`
  only when not done. Any throw sets `done = true` (no close afterwards).
- **`IteratorClose(rec, completion)`**: `ret = GetMethod(iterator,
  "return")`; `undefined` ⇒ completion; else call it; a throw completion is
  rethrown ignoring the call's own throw/result; otherwise a throwing call
  propagates and a non-object result throws `TypeError`.
- **`IterableToList`** for spread, `Promise.*`, `AggregateError`.
- Codegen representation: the record is two locals / frame cells (iterator,
  next) plus a `done` flag where close must be decided; it never escapes, so
  no heap record. The record carries an `async` bit used by Y5 (§5.8).

### 5.2 Consumers and abrupt completion

| consumer | protocol | close on |
|---|---|---|
| for-of | step per iteration | `break`, `return`, outer labelled `break`/`continue`, a throw from the body or the head's destructuring (not from `next`) |
| array destructuring (binding, assignment, params, for-of heads) | lazy step per element; elisions step; rest drains; an empty pattern steps nothing | after the pattern if not done (an empty pattern closes immediately, B4); on a throw from a default initializer, nested pattern, or member target |
| spread (array, call, new, `AggregateError`) | drain | never (a `next` throw marks done) |
| `yield*` | forward `next(v)` / `throw(e)` / `return(v)` with the received value; value of `yield*` = delegate's return value | `throw` on a delegate without `throw`: close, then `TypeError: The iterator does not provide a 'throw' method`; `return` without `return` completes normally |
| Map/Set ctors (`AddEntriesFromIterable`), `Object.fromEntries` | step; `Get(entry, "0"/"1")`; adder / `CreateDataProperty` | adder throw, non-object entry (`TypeError`), entry `Get` throw |
| `Array.from` | `GetMethod(items, @@iterator)`, `undefined` ⇒ array-like | `mapFn` throw, `CreateDataProperty` throw (today: generators only) |
| `Promise.all/allSettled/any/race` | step, `promiseResolve`, `then` | `IfAbruptRejectPromise` after close |
| `Iterator.from`, helpers | `GetIteratorDirect` / `GetIteratorFlattenable` | helper `return()` closes the underlying iterator |
| `for await`, async `yield*`, `Array.fromAsync` | §5.8 | `AsyncIteratorClose` (awaits the `return()` result) |

Lowering close for for-of: the body sits in an implicit `try/finally` that
acts only on abrupt exits; the existing `$cf_kind`/`$cf_value` routing
(DESIGN §6.6) already separates `break`/`continue`/`return` from a pending
exception, so the close call runs in that finalizer and a pending exception
suppresses the `return()` result. Destructuring uses the same shape around
the pattern.

**Generators (blocking for Y2).** A for-of / destructuring over a user
iterator inside a generator suspended at a `yield` in the loop must close
the inner iterator when the generator is `return()`ed. On `be9114d`
`throw()` already resumes the state machine at the suspended `yield` (an
enclosing `catch` sees it) except in a body with a `yield*`; `return()`
completes the generator without running anything unless the body has a
`finally` or `yield*`, where it throws (`resume_limits` in
`core/codegen/generator.rs` sets `GEN_LIMIT_RETURN` / `GEN_LIMIT_THROW`;
`core/emit/builtins/generator.rs` `RETURN_UNSUPPORTED`, `THROW_UNSUPPORTED`,
and for async generators `ASYNC_RETURN_UNSUPPORTED`). With close, the
"complete without running anything" path becomes silently wrong. Y2
implements `return()` as a return completion injected at the suspended
`yield` and routed through the state machine's finally blocks (sync and
async generators), forwards `throw()` / `return()` into a `yield*`
delegate, and deletes `GEN_LIMIT_RETURN` / `GEN_LIMIT_THROW`, the three
messages and `resume_limits` (Q6).

### 5.3 Guarded fast paths

Decided **once at `GetIterator`**: the spec caches `next`, so mutating `next`
or `Array.prototype[@@iterator]` mid-loop is unobservable. A failed guard
runs the generic protocol on the same value. Same pattern as the builtin
fast paths (DESIGN §6.4).

| source | fast path (today's code) | guard (all must hold) |
|---|---|---|
| array | index walk re-reading `length` (today `IterLen`/`IterItem`, `core/emit/array.rs`) | `TYPE_ARRAY`; `proto == %Array.prototype%`; own table has no `@@iterator` (`props == 0` or `HAS_SYMBOL_KEYS` clear); `%Array.prototype%` and `%ArrayIteratorPrototype%` bits clear (folded when intrinsics-sealed). Hole reads consult `%Array.prototype%`, covered by its bit |
| string | per-code-point walk (`__str_iter_next`, §5.4) | primitive string; `%String.prototype%` and `%StringIteratorPrototype%` bits clear |
| `String` wrapper | unwrap | `CLASS_STRING`, `HAS_SYMBOL_KEYS` clear, proto `%String.prototype%`, same bits |
| Map / Set | live entry walk with tombstones (§5.5) | `TYPE_MAP`/`TYPE_SET`; realm proto; no own `@@iterator`; Map/Set prototype and Map/Set iterator prototype bits clear |
| generator | direct state-machine call (`GenResume`) | exactly the `ProtoKind::Generator` receiver test of the `it.next()` call site (`emit_proto_kind_test`: sync `TYPE_GENERATOR`, `props == 0` — fixes B1 —, proto `%GeneratorPrototype%` or an object without own properties inheriting it, clean `%GeneratorPrototype%` bit) plus a clean `%IteratorPrototype%` bit. The iterator is the generator, so `return`/`throw` resolve on it |
| `arguments` | array-like index walk on `length` | own `@@iterator` value `== %Array.prototype.values%` and `%ArrayIteratorPrototype%` bit clear (requires `lower_arguments_object` to define the own `@@iterator`) |

- Spread and destructuring of a guarded array keep `__array_concat` / an
  index walk (unobservable for a clean array). For other sources the
  snapshot (`iter_snapshot`) is deleted: destructuring steps lazily, spread
  drains through the protocol. `__gen_to_array` survives only as the guarded
  generator drain for spread.
- `IsGenerator`, `IterSource`, `IterLen`, `IterItem` (`core/ir.rs`),
  `Builtin::IterSource` and `emit_iter_source` are deleted, replaced by
  record-based nodes (`GetIterator { value, kind_hint }`, `IteratorStep`,
  `IteratorValue`, `IteratorClose`). `GenResume` stays as the guarded
  direct state-machine step (used by the `it.next()` call-site fast path in
  `core/codegen/op.rs` and by the generator fast path of `IteratorStep`).

### 5.4 Intrinsics and iterator objects

| intrinsic | needed | stage |
|---|---|---|
| `%IteratorPrototype%` (`Iterator.prototype`) | proto `%Object.prototype%`; `[@@iterator]() { return this }`; helpers (§5.7); `constructor` and `@@toStringTag` are accessors (`SetterThatIgnoresPrototypeProperties`) | Y2-T1 (helpers Y3) |
| `%ArrayIteratorPrototype%` | proto `%IteratorPrototype%`; `next`; tag `"Array Iterator"`; from `Array.prototype.keys/values/entries/@@iterator` (`@@iterator === values`) | Y2-T1 |
| `%StringIteratorPrototype%` | `next` (code points); tag `"String Iterator"`; `String.prototype[@@iterator]` | Y2-T1 (U2-T1 closes out) |
| `%MapIteratorPrototype%` / `%SetIteratorPrototype%` | `next`; tags `"Map Iterator"` / `"Set Iterator"`; `Map.prototype.entries/keys/values` (`@@iterator === entries`); `Set.prototype.values` (`keys === values`, `@@iterator === values`), `entries` | Y2-T1 |
| `%GeneratorPrototype%` | proto becomes `%IteratorPrototype%`; tag `"Generator"` as a real property (Y4) | Y2-T1 |
| `%AsyncIteratorPrototype%` | proto `%Object.prototype%`; `[@@asyncIterator]() { return this }` | Y2-T1 (Q4) |
| `%AsyncGeneratorPrototype%` | proto becomes `%AsyncIteratorPrototype%`; tag `"AsyncGenerator"` (Y4) | Y2-T1 |
| `%GeneratorFunction%`, `%GeneratorFunction.prototype%`, async twins | low priority (`Object.getPrototypeOf(function*(){})` tests); not modeled today (DESIGN §7.8) | Y2-T6 if time permits |
| `%IteratorHelperPrototype%`, `%WrapForValidIteratorPrototype%` | §5.7 | Y3 |
| `%AsyncFromSyncIteratorPrototype%` | §5.8 | Y5 |

Iterator objects (Q5): `TYPE_OBJECT` with class **`CLASS_ITERATOR = 8`**, so
every object test stays unchanged. `next` checks class and kind for its
receiver (`TypeError: next method called on incompatible receiver`, like
`core/emit/builtins/generator.rs` `NOT_GENERATOR`).

| offset | size | field |
|---|---|---|
| 0 | 16 | object prefix (header class `8`, `props`, `proto`) |
| 16 | 8 | `[[IteratedObject]]: i64` (`undefined` once exhausted) |
| 24 | 4 | `[[NextIndex]]: u32` (Map/Set: entry index in the backing array; string: code-unit index) |
| 28 | 1 | `kind: u8` — `0` array keys, `1` array values, `2` array entries, `3` string, `4` map keys, `5` map values, `6` map entries, `7` set values, `8` set entries, `16..` helper kinds (Y3), `32` async-from-sync (Y5) |
| 29 | 3 | pad |

Block 32 bytes. Helper objects (Y3) extend the same class to 80 bytes:
`underlying iterator: i64 @32`, `underlying next: i64 @40`,
`counter/limit: f64 @48`, `callback: i64 @56`, `inner iterator: i64 @64`
(`flatMap`), `inner next: i64 @72`; `@24` holds the helper state
(`suspended-start` / `suspended-yield` / `running` / `completed`). The
async-from-sync object (Y5) uses `@32` / `@40` for the sync iterator and its
`next`. Y3-T1 / Y5-T1 may adjust their tails; the 32-byte head is fixed in
Y2.

Dirty bits: one each for `%Symbol.prototype%` (Y1), `%IteratorPrototype%`,
`%ArrayIteratorPrototype%`, `%StringIteratorPrototype%`,
`%MapIteratorPrototype%`, `%SetIteratorPrototype%` (Y2), and the `Array` /
`Promise` constructors (Y4): **8 of the 18 free bits**; Y5 has no fast path
and takes none. Bit numbers are assigned at implementation time, in merge
order.

`__str_iter_next(s, i)` returns the next code-point string and the new
index. Before U1 (UTF-8 layout) it steps one UTF-8 sequence, which is one
code point and correct because no lone surrogate exists before U1; on code
units with surrogate pairing after U1 (Q15). Whichever of Y2-T1 / U1-T2
merges second writes or ports it (§9.3).

### 5.5 Map/Set deletion and live iterators

The spec iterator survives deletion and sees entries appended later,
including after `clear`. Design (Q7):

- `delete` writes a tombstone pair (`HOLE` key) in place; `size` counts live
  entries; lookups and walks skip tombstones (`core/emit/runtime.rs`
  `emit_map_find`, `emit_map_set`, `emit_map_delete`; `forEach` in
  `core/emit/builtins/map_set.rs` `emit_for_each`).
- `clear` tombstones every entry in place (it does not shrink the backing
  array), so a running iterator continues at its index and sees entries
  added after the `clear`, as the spec requires.
- **No compaction in phase 3.** Compacting while an iterator is live moves
  entry indices under it (silently wrong); a correct compaction needs a
  forward chain from the old backing array to the new one that iterators
  follow (V8's `OrderedHashTable` transition). Without a collector the
  abandoned backing array leaks anyway, so compaction saves little before
  phase 4; it is designed together with the GC. Cost: lookups scan
  tombstones (Risk R6).
- Also fixes the DESIGN §13 `forEach` skip (B2). Host decoders read only
  `size @16` (DESIGN §6.8); Map/Set rendering must skip tombstones in both
  hosts.

### 5.6 Generator state machine and `yield*`

- Spec `yield*` needs the outer resumption **mode** (next/throw/return) and
  value at the delegate loop. New field: `GEN_RESUME_MODE` in bits `8..=9`
  of `limits @28` (`0` next, `1` throw, `2` return; mask
  `GEN_RESUME_MODE_MASK = 0x300`, shift `GEN_RESUME_MODE_SHIFT = 8`; bits
  `0..=5` are taken, §1.2), written by `next`/`throw`/`return` (sync and
  async request serving) before the resume. The delegate loop calls
  `inner.next(v)` / `inner.throw(e)` / `inner.return(v)` through
  `GetMethod`, with a guarded fast path for a clean generator delegate
  (direct state-machine call). `yield*` becomes an expression (its value is
  the delegate's return value); `core/codegen/expr.rs` drops the "`yield*` is
  only supported as a statement" rejection.
- `return()` / `throw()` at any `yield` route through the state machine's
  finally blocks (§5.2); `GEN_LIMIT_RETURN`, `GEN_LIMIT_THROW`,
  `resume_limits` and their three `TypeError`s (`generator return() at a
  yield inside try/finally or yield* is not supported`, `generator throw() at
  a yield* is not supported`, `async generator return() at a yield inside
  try/finally is not supported`, `core/emit/builtins/generator.rs`) are
  deleted.
- L2-L delivered generator / async function expressions and methods,
  per-function `prototype` objects and async generators (Q8); Y2 builds on
  them. `yield*` in an async generator is Y5 (§5.8).

### 5.7 `Iterator` global and helpers (Y3)

- `Iterator`: abstract — `TypeError` when `new.target` is `undefined` or
  `Iterator` itself; subclassable. `Iterator.prototype` =
  `%IteratorPrototype%` (non-writable).
- `Iterator.from(O)`: `GetIteratorFlattenable(O, iterate-strings)`; result
  inheriting `%IteratorPrototype%` is returned, else wrapped in
  `%WrapForValidIteratorPrototype%` (`next`, `return` forwarding).
- Lazy `map`, `filter`, `take`, `drop`, `flatMap` → helper objects over
  `%IteratorHelperPrototype%` (`next`, `return`, tag `"Iterator Helper"`).
  Eager `reduce`, `toArray`, `forEach`, `some`, `every`, `find` consume via
  `GetIteratorDirect` and close on callback throw / early result. Argument
  validation order is test262-visible (`next` read once, callable checks
  before stepping, `RangeError` for `take`/`drop` after `ToNumber`).
- Helper `next` while running → `TypeError` (running state); `return()`
  before start closes the underlying iterator.
- Not in 3c (Q21): `Iterator.concat` (`iterator-sequencing`),
  `Iterator.zip` / `zipKeyed` (`joint-iteration`), `chunks` / `windows`
  (`iterator-chunking`), `includes` (`iterator-includes`),
  `Iterator.prototype.join`, `Promise.allKeyed` / `allSettledKeyed`
  (`await-dictionary`). None of these features is denied today, so their
  ≈ 309 tests stay eligible and failing.
- `Promise.all/allSettled/any/race` on the protocol in the guest (`then`
  per element through the existing `__then`); `any` uses L2-F's
  `AggregateError`. `Math.sumPrecise` (an iterable consumer, DESIGN §13)
  joins them in Y3-T4.

### 5.8 Async iteration (Y5)

Builds on Y2's records (the `async` bit), `%AsyncIteratorPrototype%`
(Y2-T1), async generators (L2-L T5, DESIGN §7.8) and the async driver
(DESIGN §6.7).

- **`GetIterator(obj, async)`**: `method = GetMethod(obj, @@asyncIterator)`;
  `undefined` ⇒ `CreateAsyncFromSyncIterator(GetIterator(obj, sync))`; the
  record's `async` bit is set.
- **`%AsyncFromSyncIteratorPrototype%`** (`CLASS_ITERATOR` kind `32`,
  §5.4): `next` / `return` / `throw` call the sync method, then
  `AsyncFromSyncIteratorContinuation`: `PromiseResolve(%Promise%, value)`
  and a promise for `{ value, done }`; a rejected value closes the sync
  iterator when not done (ES2025). Missing `return` → `{ value, done: true
  }`; missing `throw` → close, then reject with a `TypeError`.
- **`for await (x of y)`** (in async functions, async generators and the
  top level of `main`, where `await` is already legal): `lower_for_of`
  accepts `is_await`; each step is `Await(Call(next, iterator))` through the
  existing await suspend (`await_suspend` in `core/codegen/generator.rs`),
  then the result-object checks of §5.1; abrupt exits call
  `AsyncIteratorClose` (`return()` result awaited, then checked for an
  object). Destructuring heads reuse Y2-T3's lazy patterns. No fast path:
  every step is a microtask turn by specification.
- **`yield*` in async generators**: the delegate loop of §5.6 with
  `GetIterator(async)`, every inner result awaited, and the received mode
  forwarded; `core/codegen/generator.rs` `lower_yield_delegate` drops the
  "`yield*` in an async generator is unsupported" rejection.
- **`Array.fromAsync(items, mapFn, thisArg)`**: an async builtin (it returns
  a promise and awaits per element); `GetMethod(@@asyncIterator)`, then
  `@@iterator` through async-from-sync, else array-like with awaited
  elements.
- test262: `Symbol.asyncIterator` leaves `unsupported-features.txt`
  (added there explicitly by Y1, §9.2).

---

## 6. Strings: Latin-1 / UTF-16 dual representation

### 6.1 Representation

Options considered: UTF-16 only (2× memory for ASCII), UTF-8 plus index
cache (lone surrogates need WTF-8, every unit op translates, no engine
precedent), **Latin-1 / UTF-16 with a header flag** (chosen; V8,
SpiderMonkey, JavaScriptCore all do this).

| offset | size | field |
|---|---|---|
| 0 | 1 | type `TYPE_STRING = 0` |
| 1 | 1 | `gc` |
| 2 | 1 | flags: `1` `STR_FLAG_INTERNED`, `2` `STR_FLAG_ARRAY_INDEX`, **`4` `STR_FLAG_TWO_BYTE`** (new) |
| 3 | 1 | `class` = 0 |
| 4 | 4 | `len: u32` — length in **UTF-16 code units** (= spec `.length`) |
| 8 | `len` or `2·len` | Latin-1 bytes, or UTF-16LE code units (8-aligned base ⇒ 2-aligned units) |

Block size `8 + (len << two_byte)` rounded to 8. New `core/abi.rs`
constants: `STR_LEN_OFFSET = 4`, `STR_DATA_OFFSET = 8`,
`STR_FLAG_TWO_BYTE = 4`. `MAX_STRING_LENGTH` (`core/emit/builtins/string.rs`,
`536_870_888` = `2^29 − 24`, today in bytes) counts code units (Q13). Every
site of §1.4 switches to them. No cached hash word (Q12): interned strings
compare by pointer and hashing happens only in `__intern` /
`__intern_find`.

### 6.2 Invariants

- **Canonical form: a string is one-byte iff every code unit is ≤ 0xFF**
  (Q11). Producers that can narrow (slice, substring, JSON.parse, host
  results from a two-byte source) narrow while copying.
- Hence: (a) strings with different `TWO_BYTE` flags are never equal;
  (b) a canonical array index is always one-byte, so `__index_value` returns
  `-1` immediately for `TWO_BYTE`; (c) the intern table never holds two forms
  of one content.
- Strings stay primitives without an object prefix. A symbol key never
  enters string paths (§3.1).

### 6.3 Compile-time literals

- Lower `Str.value` / template `cooked` (`Wtf8Atom`) to UTF-16 code units
  **without** `to_string_lossy` at every site of §1.4 plus
  `core/codegen/globals.rs` `visit_str`. WTF-8 → UTF-16 is lossless (a lone
  surrogate is its 3-byte generalized UTF-8 form). `String.raw` / tagged
  template `raw` strings (L2-F `emit_raw`) take the same path.
- `IrNode::Str` carries a code-unit string type (e.g. `JsStr(Box<[u16]>)`)
  with `From<&str>` for the compiler-built ASCII construction sites.
  `collect_strings`'s map and `static_key` key on it.
- `emit_data_section` emits each literal canonically; `is_array_index` runs
  on code units. Number literals used as keys stay ASCII.

### 6.4 Hashing and interning (shared key space with symbols)

- FNV-1a **over code units**: `h = (h ^ unit) * 0x0100_0193`, one round per
  unit (zero-extended). One-byte strings feed bytes, two-byte strings feed
  u16s. For ASCII the result equals today's byte FNV-1a, so the static intern
  table is unchanged for ASCII programs. Runtime `emit_str_hash` gets a
  two-loop body; compile-time `fnv1a` takes `impl Iterator<Item = u16>`.
- `emit_intern_slot` / `emit_intern` unchanged apart from `__str_eq` and
  `__index_value`.
- Property key slots hold either an interned canonical string or a symbol
  block (§3.1); both are canonical blocks, so key equality stays pointer
  equality. Symbols bypass the hash.

### 6.5 Concatenation and slicing

- `__concat(a, b)`: result two-byte iff either operand is two-byte (canonical
  automatically). Same width → two `memory.copy`; mixed → copy the two-byte
  side, widen the one-byte side. **Empty operand → return the other operand**
  (strings are immutable).
- Ropes: no (Q12). `s = s + x` loops stay O(n²) as today; revisit with a
  builder after phase 4.
- Slices **copy** (no views): the phase-4 collector is non-moving with no
  interior pointers (DESIGN §5.7); views would need a traced kind, keep
  parents alive, and complicate host decoders. Copying a two-byte range
  narrows when possible.
- Single-unit results (`s[i]`, `charAt`, iteration) allocate a 16-byte block
  as today; a cache for `0..=0xFF` is Q14.

### 6.6 Length, indexing, surrogates

| operation | unit | on `be9114d` | stage |
|---|---|---|---|
| `.length`, `s[i]` | code unit | bytes (silently wrong) | U1-T2 |
| `charAt`, `charCodeAt`, `at`, `codePointAt`, `slice`, `substring`, `padStart`/`padEnd`, `split("")` / `split` with a limit, `replace`/`replaceAll` with an empty pattern | code unit | host, byte offsets, refuse non-ASCII (§1.4) | U1-T4 (guest, code units; refusals deleted) |
| `indexOf`, `lastIndexOf`, `includes`, `startsWith`, `endsWith`, `search` (string pattern), `concat`, `repeat`, `split` (separator), `replace`/`replaceAll` | code unit | host, exact UTF-16 offsets / content-only | U1-T4 (guest) |
| `trim*`, ToNumber / `parseFloat` / `parseInt` whitespace | WhiteSpace ∪ LineTerminator (U+0009–000D, 0020, 00A0, 1680, 2000–200A, 2028, 2029, 202F, 205F, 3000, FEFF) | host `is_js_whitespace` (correct) | U1-T4 moves `trim*` into the guest with the table; host parsers keep `is_js_whitespace` |
| `<` `>` `<=` `>=`, `sort` default order | code-unit order | UTF-8 byte order | U1-T2 |
| `String.fromCharCode`, `String.fromCodePoint` | `ToUint16` / code point | host, refuse lone surrogates | U1-T4 (guest) |
| `isWellFormed`, `toWellFormed` | code unit | absent | U1-T4 |
| string iterator (for-of, spread, `Array.from`, destructuring) | code point | per byte | Y2-T1 introduces it (UTF-8 sequences), U1-T2 ports to units, U2-T1 closes out (Q15) |
| `toUpperCase` / `toLowerCase` / `toLocale*Case` | code point, full mapping incl. SpecialCasing and `Final_Sigma` | host (Rust `to_uppercase`, correct for valid UTF-8) | U2-T2 (guest Latin-1 fast path, host for the rest; Q5) |
| `normalize`, `localeCompare` | code point / code-unit order | `normalize` refuses non-ASCII; `localeCompare` code-unit order | U2-T3 (host normalizer; Q5) |
| `String.raw` | — | exists (L2-F `emit_raw`) | U1-T1 (lossless raw strings) |

Surrogate rule implemented once (`CodePointAt(s, i)`): a lead `D800–DBFF`
followed by a trail `DC00–DFFF` is a pair; otherwise the unit itself. Only
two-byte strings can hold surrogates; one-byte strings skip the check.

### 6.7 Comparison and equality

- `__str_eq(a, b)`: `a == b` → true (new pointer fast path); `TWO_BYTE` flags
  differ → false; `len` differs → false; else compare `len << two_byte`
  bytes, 8 at a time with `i64.load`, then the tail.
- `__str_cmp(a, b)`: code-unit order. Same width: unsigned byte compare
  (one-byte) or `i32.load16_u` units; mixed: a unit loop reading each side at
  its width. Fixes `"\uFFFF" < "\u{10000}"`. The `sort` default comparator
  uses it.

### 6.8 JSON

- `__json_escape`: a lone surrogate → `\udxxx` (lowercase hex);
  well-formed pairs verbatim.
- `__json_string` (`core/emit/json_parse.rs` `emit_json_string` /
  `emit_escape_byte`): `\uXXXX` escapes including lone/paired surrogates;
  output narrows when all units ≤ 0xFF. Deletes the "any `\u` escape
  throws" cut.
- `emit_gap`: first 10 **code units** of a string `space`.

### 6.9 Number ↔ string

- Number → string (host `__convert` op 0, `Number.prototype.toString(radix)`
  `METHOD_OP_NUMBER_TO_STRING`, `toFixed` / `toExponential` /
  `toPrecision` ops 40-42, guest `__index_string`), `typeof` words, Date
  strings, error texts, JSON tokens: always ASCII; host allocation picks the
  width from content, so they stay one-byte with no special case.
- String → number: `core/run/host.rs` `str_to_number`, `parse_float_str`,
  `parse_int_str` (`__parse_int` import) already use the §6.6 whitespace set;
  they read the string lossily (safe: a lone surrogate is never whitespace or
  a digit).

### 6.10 String wrappers

`emit_to_object` stops defining per-index own properties; the string exotic
`[[GetOwnProperty]]` / `[[OwnPropertyKeys]]` / `[[DefineOwnProperty]]` is the
shared `__str_exotic_index` arm in the property and descriptor helpers listed
in §3.3 (Q16). Removes O(len) interned keys per `new String(s)` and per
primitive-receiver `ToObject`. `pkg/value.ts` / `core/run/value.rs` render a
wrapper from its `[[PrimitiveValue]]` instead of filtering index keys.

---

## 7. Host / shim boundary

### 7.1 What crosses imports

- The host receives primitives only after guest `ToPrimitive` (DESIGN §6.1):
  `__convert` and `__method` never see a symbol (the guest throws first,
  §2.4). `print` renders arbitrary jsvals; `__promise`/`__then` store jsvals
  opaquely, so a symbol resolution value or reason needs no change. Uncaught
  exceptions render from `__exc_value`.
- **No new imports or exports for symbols or async iteration.** Strings
  change one export signature: `__new_string(len, two_byte) -> i32` (len in
  code units; the guest writes the flag).

### 7.2 Symbol rendering

- `core/run/value.rs` `decode_heap` and `pkg/value.ts` `decodeHeap` gain a
  `TYPE_SYMBOL` arm → `Value::Symbol(Option<String>)`; `format_value` renders
  `Symbol(desc)` / `Symbol()`.
- `table_entries` / `decodeProps` check each key's type byte (today a symbol
  key would be decoded as a garbage string) and render enumerable symbol keys
  after the string keys as `[Symbol(desc)]: v`.
- `CLASS_SYMBOL` wrappers render `[Symbol: Symbol(desc)]` (beside the
  existing wrapper arm in `core/run/value.rs`); `CLASS_ITERATOR` objects
  render Node style (`Object [Array Iterator] {}`).

### 7.3 Strings: decoders and allocators

- Rust `decode_string`, `string_bytes`, `string_text`, `block_len`: read
  `flags & TWO_BYTE`; one-byte → `char::from(b)` per byte (Latin-1 = first
  256 code points); two-byte → `u16::from_le_bytes`. `Value::Str` keeps a
  lossless form (`Vec<u16>`) where a host op reads content; lossy only at
  output.
- Shim `decodeString`: **neither `TextDecoder('latin1')`** (WHATWG maps it to
  windows-1252) **nor `TextDecoder('utf-16le')`** (replaces lone
  surrogates). Use `String.fromCharCode` over chunked `Uint8Array` /
  `Uint16Array` views. The wrapper index filter in `pkg/value.ts` (today
  `utf8Encode.encode(..).length`) is deleted with the eager index keys
  (§6.10).
- Allocation: Rust `alloc_string(&[u16])` (or `&str` via `encode_utf16`)
  narrows when all units ≤ 0xFF; shim scans `charCodeAt` and writes a
  `Uint8Array` or `Uint16Array` view at `base + STR_DATA_OFFSET`.

### 7.4 Console output

UTF-16 → UTF-8: pairs combine, **lone surrogates → U+FFFD** (what Node
writes). Rust: `char::decode_utf16(..).map(|r| r.unwrap_or('\u{FFFD}'))`;
shim: JS strings are already UTF-16.

### 7.5 `__method` string ops

All code-unit string ops move into the guest in U1-T4; their `METHOD_OP_*`
constants, host `method_dispatch` arms / helpers and shim arms are deleted:

- moved: `METHOD_OP_SLICE` 3, `CHAR_AT` 6, `TRIM` / `TRIM_START` /
  `TRIM_END` 7-9, `REPEAT` 10, `STARTS_WITH` / `ENDS_WITH` 11-12,
  `PAD_START` / `PAD_END` 13-14, `SUBSTRING` 15, `SPLIT` 16, `AT` 18,
  `CHAR_CODE_AT` 19, `CODE_POINT_AT` 20, `LAST_INDEX_OF` 21, `SEARCH` 24,
  `REPLACE` / `REPLACE_ALL` 25-26, `FROM_CHAR_CODE` 27,
  `FROM_CODE_POINT` 28, and the **string arms** of `INDEX_OF` 0 /
  `INCLUDES` 1 (their array arms stay: `core/emit/numeric.rs` `MethodOp` and
  `core/emit/builtins/array.rs` use them for array element search; the dead
  array arm of `SLICE` goes with the op);
- deleted with them: the refusal texts (`Texts::non_ascii`,
  `FROM_CHAR_CODE_SURROGATE`, `FROM_CODE_POINT_SURROGATE`), `may_refuse`'s
  non-ASCII cases, `METHOD_NEEDS_REGEXP` (the RegExp-syntax check of
  `search` / `match` moves into the guest), the host helpers of §1.4
  (`index_exact`, `endpoint_exact`, `ascii_prefix`, …);
- host ops that remain: `TO_UPPER` / `TO_LOWER` 4-5 and `LOCALE_COMPARE` 22
  (U2), `NORMALIZE` 23 (U2; `METHOD_UNSUPPORTED` is deleted when U2-T3 makes
  it total), `NUMBER_TO_STRING` 17, `NUMBER_TO_FIXED` /
  `EXPONENTIAL` / `PRECISION` 40-42, `MATH` 43, and `METHOD_RANGE_ERROR`
  (`normalize`'s form check) (Q5, Q17).

Reasons: each host op exists twice (Rust + TS), phase 4 removes both hosts
(DESIGN §14), and it removes the per-call whole-memory copy in
`method_dispatch` for every string method.

### 7.6 ABI changes (`core/abi.rs` single source, mirrored in `pkg/abi.ts`, listed in `core/abi_drift_tests.rs` `expected()`)

| stage | added | deleted |
|---|---|---|
| Y1 | `TYPE_SYMBOL = 14`, `SYMBOL_DESCRIPTION_OFFSET = 8`, `SYMBOL_BLOCK_SIZE = 16`, `SYMBOL_FLAG_REGISTERED = 4`, `CLASS_SYMBOL = 7`, `PROPS_HAS_SYMBOL_KEYS = 8`, `TYPEOF_STRINGS` + `"symbol"` | — |
| Y2 | `CLASS_ITERATOR = 8` (+ payload offsets only if a host renders them), `GEN_RESUME_MODE_MASK = 0x300`, `GEN_RESUME_MODE_SHIFT = 8`, `GEN_DELEGATES = 64`, `GEN_DELEGATED_RESULT = 128` (guest-only, Rust only like the other `GEN_*` limits bits; the last two Y2-T6) | `GEN_LIMIT_RETURN`, `GEN_LIMIT_THROW` (Rust only; `pkg/abi.ts` never mirrored them) |
| U1 | `STR_LEN_OFFSET = 4`, `STR_DATA_OFFSET = 8`, `STR_FLAG_TWO_BYTE = 4`, `__new_string(len, two_byte)` doc | the moved `METHOD_OP_*` of §7.5, `METHOD_NEEDS_REGEXP` |
| U2 | — | `METHOD_UNSUPPORTED` |
| Y5 | — (iterator kind `32` is a payload value, not an ABI constant) | — |

Constant values were re-checked free on `be9114d`; if a sibling takes a
class byte or table flag first, the implementing stage picks the next free
value and updates this table.

---

## 8. Performance and memory

### 8.1 Fast paths

- One-byte strings keep today's byte loops (copy, compare, hash, index, JSON
  escape) plus one flag test per helper call. New wins: `__str_eq` pointer
  and width early-outs, `__concat` empty-operand shortcut, no memory copy per
  string method call (§7.5).
- `.length` stays one `i32.load` at offset 4; `s[i]` keeps its bounds check
  and gains one width branch in `__str_index`.
- Iteration over clean arrays / strings / Map / Set / generators keeps the
  index or direct-call walk; the guard runs once per loop.
- Symbol lookups for well-known hooks cost one `HAS_SYMBOL_KEYS` test per
  prototype level on objects that never received a symbol key.
- Async iteration (Y5) has no fast path: each step is a microtask turn by
  specification.

### 8.2 Memory

| content | today (UTF-8) | dual | change |
|---|---|---|---|
| ASCII | 1 B/char | 1 B/char | same |
| Latin-1 non-ASCII (é, ñ, ü) | 2 B | 1 B | −50 % |
| BMP non-Latin-1 (CJK, Cyrillic, —, €) | 2–3 B | 2 B | ≤ |
| astral (emoji) | 4 B | 4 B | same |
| long ASCII + one non-Latin-1 char | ~1 B/char | 2 B/char | up to 2× (the only regression) |

Symbols: 16 bytes each; wrappers 24; iterator objects 32 (helpers 80). The
realm image grows by `%Symbol%`, `%Symbol.prototype%`, 13 well-known blocks,
the registry, 8 iterator prototypes (incl. `%AsyncIteratorPrototype%`,
`%AsyncFromSyncIteratorPrototype%`) and `%Iterator%` (data segment only, no
startup cost). Map/Set backing arrays keep tombstones until phase 4 (§5.5).
String wrappers lose their per-index tables (§6.10).

### 8.3 Opcount expectations (`crates/jsc-core/tests/opcount/snapshot.tsv`)

`user_ops` counts main + user functions; helper bodies affect `fuel`. Every
delta needs a reason cell. Rows on `be9114d`: `array_hofs`, `class_methods`,
`console_log`, `fib`, `for_in`, `generators`, `json`, `map_set`, `math`,
`object_reads`, `push_1e5`, `string_methods`. No local runs: the numbers are
regenerated with `JSC_OPCOUNT_UPDATE=1` from the final CI pass (§9.2), and
each stage closeout drafts the reason cells from this table.

| row | stage | expectation |
|---|---|---|
| `fib`, `math`, `push_1e5` | all | **must not change** |
| `console_log` | Y1, U1 | unchanged (print is host); renderer cost is host time |
| `class_methods`, `object_reads`, `array_hofs` | Y1 | `emit_is_object` gains one compare: static ops up slightly, fuel up only on object/primitive classification paths; `ProtoGuard` and sealed paths unchanged |
| `class_methods`, `array_hofs`, `string_methods` | Y4 | `ToPrimitive` gains one `HAS_SYMBOL_KEYS` test per prototype level per object conversion (a few fuel each); sealed type-byte `instanceof` arms unchanged; species fast path keeps `array_hofs` fuel |
| `for_in`, `object_reads` | Y1 | `__to_key` kind test before the number arm: a few fuel per computed key |
| `generators` | Y2 | the `it.next()` call-site fast path is unchanged; `return(7)` at a suspended `yield` now resumes the state machine (completion routing) instead of completing directly: small fuel rise, accepted with reason |
| `map_set` | Y2 | tombstone skip in walks; `size` maintained separately; small fuel change |
| new `for_of`, `spread` programs | Y1-T1 adds, Y2 delta | per-loop constant for the guard; **per-iteration fuel of array, string, Map and generator paths must not increase** |
| `string_methods` | U1 | `host_calls` drop (ops move into the guest); fuel **rises** (guest loops are metered, host time is not) — accepted with reason; `user_ops` ≈ unchanged |
| `json` | U1 | small fuel rise (escape width branch, parse narrowing) |
| `for_in` | U1 | small change (`keys + k` concat width branch; `__str_eq` pointer fast path may lower it) |

Must not regress: direct calls, sealed fast paths, the `.length` inline
path, static-key property access (a constant interned pointer), `ProtoGuard`
builtin fast paths, the `it.next()` generator fast path.

Y1 reason drafts (one line per affected row, for the final CI pass):

- `for_of`, `spread`: "Y1-T1: new program"; Y1-T4 adds one `__symbol_get`
  chain test per loop / spread setup (for-of over a non-generator runs it
  twice); per-iteration fuel unchanged.
- `class_methods`, `object_reads`, `array_hofs`, `string_methods`:
  `emit_is_object` gains one type-byte compare (Y1-T2); every object
  `ToPrimitive` gains one `__symbol_get` chain test (Y1-T4); `__get` gains a
  type-byte compare on the pointer path (Y1-T2); `array_hofs` also pays the
  `species_ctor` / `concat` chain tests (Y1-T4).
- `for_in`, `object_reads`: `__to_key` symbol and object kind tests before the
  number arm (Y1-T4).
- concat-heavy rows: the `__add` string arm calls
  `__to_string` for non-string primitives (Y1-T2).
- `json`: `JSON.parse` text goes through `__to_string` (Y1-T4).
- rows with accessor-half class statics or object-literal accessors: defined
  through `__define_own` (Y1-T5).
- rows with `typeof` sites: +4 static ops per site; `"symbol"` interned word
  (Y1-T1).
- `fib`, `math`, `push_1e5`, `console_log`: must not change.

U1 reason drafts (one line per affected row, for the final CI pass):

- `string_methods`: `slice`, `charAt`, `indexOf`, `includes`, `split`,
  `trim`, `startsWith` run in the guest on code units (U1-T4): `host_calls`
  drop (only `toUpperCase` / `toLowerCase` stay host ops), fuel rises
  (guest loops are metered, host time is not); `user_ops` changes only where
  a former inline host op site now calls the builtin; `out + ...` pays the
  `__concat` width branch (U1-T2).
- `json`: `__json_escape` gains the per-unit width branch and lone-surrogate
  test; `__json_string` parses `\u` escapes and narrows its result;
  `JSON.stringify(again)` pays the same escape path (U1-T2): small fuel rise.
- `for_in`: `keys + k` pays the `__concat` width branch (an empty `keys`
  returns `k` without copying); `__str_eq` pointer fast path and 8-byte
  compare on key lookups (U1-T2): small change either way.
- `map_set`, `for_of`, `spread`, `class_methods`: `'k' + i` /
  `name + ':'` concat width branch and `__str_eq` on Map key compares
  (U1-T2); string for-of / spread walk `__str_to_array` on code units, a
  one-byte string skipping the surrogate test (U1-T2): small change, the
  array / Map / generator per-iteration fuel unchanged.
- `array_hofs`: `indexOf` / `includes` keep their host array arms; no
  `Array.prototype.slice` or String method: unchanged apart from shared
  helper bodies.
- every row: ASCII intern hashes are unchanged (FNV-1a over units, U1-T1b),
  so the static intern table and literal layout are identical.
- `fib`, `math`, `push_1e5`, `console_log`, `object_reads`: must not
  change; `generators` changes only by Y2 (see the Y2 drafts below).

U2 reason drafts (one line per affected row, for the final CI pass):

- `string_methods`: `toUpperCase` / `toLowerCase` on the one-byte `s` now
  go through the builtin and map per unit in the guest (U2-T2) instead of
  the inline host call: `host_calls` drop by the 20 case calls, fuel rises
  for the metered guest loop, `user_ops` changes at the two former inline
  sites; no `normalize` / `localeCompare` call, so their unchanged host path
  does not show.
- every row: the two interned static strings of the deleted
  `NORMALIZE_UNSUPPORTED` message are gone (U2-T3), so the static intern
  table shrinks and intern-table probe fuel may shift by a few.
- `fib`, `math`, `push_1e5`, `console_log`, `object_reads`, `json`,
  `for_in`, `map_set`, `for_of`, `spread`, `class_methods`, `array_hofs`,
  `generators`: no case / `normalize` / `localeCompare` call; must not
  change beyond the intern-table line above.

Y2 reason drafts (one line per affected row, for the final CI pass):

- `for_of`: per loop a `GetIterator` classify plus the try/catch/finally
  landing of the close; per iteration the array path is at most the old
  index walk, a string similar (`__str_iter_next`), a `Map` at most the old
  walk (slot walk, `[k, v]` built via `ArraySpread`), and a generator drops
  sharply (direct state-machine resume, no result object) (Y2-T2); array
  destructuring now steps a record (Y2-T3); the generator classify also
  tests the `GEN_DELEGATES` bit (one load, Y2-T6).
- `spread`: a per-source classify guard; a `Map` spread now runs the
  protocol (rises; spec-observable) (Y2-T2); an `arguments` spread is the
  `ToList` index walk (Y2-T5).
- `generators`: `return(7)` at a suspended `yield` resumes the state machine
  (completion routing; small fuel rise); `it.next()` is unchanged apart from
  one `GEN_DELEGATED_RESULT` bit test per resume (Y2-T4, Y2-T6).
- `map_set`: a `HOLE` compare per scanned slot in `__map_find`; `delete`
  without `memory.copy`; a tombstone test per entry in `forEach` and the
  iterators; the constructor goes through the iterator record (Y2-T5).
- rows with spread, destructuring, unsealed for-of or `arguments`: dirty
  tracking is now on (a few fuel per watched-table write) (Y2-T2);
  `arguments` creation defines an own `@@iterator` (Y2-T5).
- rows with an object `ToPrimitive` on arrays or strings: the Array, String,
  Map and Set prototypes carry `HAS_SYMBOL_KEYS`, so the well-known-symbol
  consults do a real lookup (Y2-T1).
- `fib`, `math`, `push_1e5`, `console_log`: must not change.

---

## 9. Staged migration

### 9.1 Baseline and estimate method

Base: `eee36ef` CI, **19378 pass / 30014 eligible** (64.6 %,
`tests/test262/history.tsv`, `tests/test262/baseline.txt`), corpus at the
pinned SHA `2e0a567`. Method (no runs): a frontmatter scan of all 47665 test
files reproducing `t262/filter.rs` `classify` and `t262/walk.rs` `collect`
(the harness-include probe is approximated by the includes that baseline
tests use, giving 29924 eligible instead of 30014; pools below carry that
±0.3 % error), then:

1. **Newly eligible tests** when a stage removes a denylist entry, minus
   tests in directories that fail regardless (RegExp, DataView, ArrayBuffer,
   WeakMap/WeakSet, BigInt: 315) and private-member tests (140), assigned to
   the first stage that provides everything they use (by `features:` and
   path).
2. **Currently eligible failing tests** (≈ 10546) attributed to a stage by
   path / feature / source pattern (computed class members, `for await`,
   async `yield*`, the B4 pattern files, iterator builtins, `Symbol` used
   without the feature tag, the D3 rows of DESIGN §13).
3. A pass fraction from comparable directories on `eee36ef`
   (`language/statements/for-of` 80 %, `for-of/dstr` 81 %,
   `language/expressions/class/async-gen-method` 82 %,
   `language/statements/async-generator` 78 %, `built-ins/Array/prototype`
   97 %, `built-ins/String/prototype` 80 %, `built-ins/Promise` 27 % —
   thenables are not assimilated, DESIGN §13 —, `class/elements` 35 %).

Denylist changes (`tests/test262/unsupported-features.txt`):

- **Y1**: replace `Symbol*` with the single line `Symbol.asyncIterator`
  (kept `symbols-as-weakmap-keys`). Newly eligible: **2322** = 315 dead
  directories + 140 private + 175 that need async iteration (→ Y5) + 343
  Y1 + 842 Y2 + 350 Y4 + 145 Y3 + 12 U2.
- **Y5**: delete `Symbol.asyncIterator`. Newly eligible: **537** (176
  private, 361 others).
- No other stage changes the denylist (Q21 keeps the iterator proposals
  eligible).

Per-stage arithmetic (pool × fraction):

| stage | pools | expected |
|---|---|---|
| Y1 | 343 newly eligible Symbol-only × 70 % = 240; 70 eligible failing tests that use `Symbol` untagged × 50 % = 35; computed class members (Q3, Y1-T5) 371 eligible failing × 60 % = 223 (the 53 newly eligible ones are inside the Y1/Y4 pools) | **+498 ≈ +500** (420–570; +275 without T5) |
| Y2 | 842 newly eligible × 75 % = 632; 247 eligible failing B4-pattern destructuring tests × 80 % = 198; 78 eligible failing iterator-builtin tests (`keys/values/entries`, iterator prototypes, generator / async generator `return` in `try/finally`) × 75 % = 58 | **+888 ≈ +890** (760–980) |
| Y4 | 350 newly eligible (tagged with a well-known symbol other than `@@iterator`; 106 in `built-ins/String/prototype`, 90 in `built-ins/Array/prototype`, 20 in `built-ins/Date/prototype`) × 60 % | **+210** (175–245) |
| Y3 | 378 eligible failing `iterator-helpers` tests × 75 % = 284; Promise combinators 244 eligible failing + 136 newly eligible = 380 × 45 % = 171; 9 newly eligible others × 60 % = 5 | **+460** (380–530) |
| U1 | ≈ 80 encoding failures (≈ 55 in `built-ins/String` outside RegExp and case mapping — non-ASCII positions, `isWellFormed` / `toWellFormed`, lone-surrogate `fromCharCode`, raw strings — and ≈ 25 in language / JSON tests: relational operators, string and template literals, string for-of) × 75 % | **+60** (50–70) |
| U2 | 12 newly eligible string-iterator tests + 12 case-mapping + 3 `normalize` + 1 `localeCompare` = 28 × 85 % | **+24 ≈ +25** (20–30) |
| Y5 | 1194 eligible failing (1025 `for-await-of`, 31 `for await` elsewhere, 154 async-generator `yield*`, 82 `Array.fromAsync`, `AsyncFromSyncIteratorPrototype`; overlaps removed) + 175 newly eligible at Y1 + 361 newly eligible at Y5 = 1730 × 65 % (the `for-await-of` files are the for-of templates, 81 % there; discounted for the async runner and thenables) | **+1125** (950–1300) |

Every currently passing encoding-sensitive test must stay passing for real
after U1: **311** baseline tests contain non-ASCII text or `\u` escapes
above U+007F (Risk 4).

### 9.2 Stages

| stage | owner | goal | files (be9114d) | depends on | exit criteria | est. test262 |
|---|---|---|---|---|---|---|
| **Y1** Symbol + symbol keys + computed class members | L2-Y | §2, §3, §4 guards, Q3; denylist `Symbol*` → `Symbol.asyncIterator` | `core/abi.rs`, `pkg/abi.ts`, `core/abi_drift_tests.rs`; `core/emit/{expr,value,convert,props,object,runtime,realm}.rs`; `core/emit/props/descriptor.rs`; new `core/emit/builtins/symbol.rs`; `core/emit/builtins/{json,object,object_static,mod}.rs`; `core/codegen/{expr,globals,stmt}.rs`; `core/run/value.rs`, `pkg/value.ts` | — (L2-O landed) | §2.3 sites and every §9.4 Y1 guard have tests; computed-class compile errors and their tests deleted | **+500** (420–570) |
| **Y2** iteration protocol | L2-Y | §5.1–§5.6: iterator prototypes/objects incl. `%AsyncIteratorPrototype%`, `keys/values/entries/@@iterator` on Array/Map/Set/String, record IR replacing `IterSource`/`IterLen`/`IterItem`/`IsGenerator`, lazy destructuring + close, for-of close, spread / `Array.from` / `Object.fromEntries` / `AggregateError` / Map/Set ctors via protocol (incl. `arguments`, wrappers, user iterables), `arguments[@@iterator]`, tombstones, generator `return()`/`throw()` routing (sync and async), spec `yield*` expression | `core/ir.rs`, `core/codegen/{stmt,generator,function,expr,op}.rs`, `core/emit/{array,generator,realm,sections,expr,runtime}.rs`, `core/emit/builtins/{iter,map_set,generator,array,object_static,ctor}.rs`, `core/run/value.rs`, `pkg/value.ts` | Y1 | B1–B4 regression tests; deleted IR nodes and limits gone; opcount per-iteration rule (§8.3) | **+890** (760–980) |
| **Y4** well-known hooks | L2-Y | §4 "honoured" rows: `@@toPrimitive`, `@@toStringTag`, `@@hasInstance`, `@@species` (Array, Promise), `@@isConcatSpreadable`, `@@unscopables` data, string-method dispatch for `@@match/matchAll/replace/search/split` on object arguments; replaces the Y1 guards | `core/emit/convert.rs`, `core/emit/builtins/{object,array,promise,string}.rs`, `core/emit/object.rs`, `core/codegen/expr.rs`, `core/emit/realm.rs` (ctor dirty bits) | Y1, Y2 (tags of the iterator prototypes) | each Y1 guard replaced by the call and tested; species fast path proves `constructor === %Array%` | **+210** (175–245) |
| **Y3** `Iterator` + helpers + `Promise` combinators | L2-Y | §5.7 | `core/emit/realm.rs`, new `core/emit/builtins/iterator.rs`, `core/emit/builtins/promise.rs`, `core/emit/numeric.rs` / `realm.rs` `MathOp` (`Math.sumPrecise`) | Y2 | no existing opcount row changes; `K` does not widen for programs that do not reach the new builtins | **+460** (380–530) |
| **U1** dual string flip | L2-U | §6.1–§6.10, §7.3–§7.6: layout, lossless literals, code-unit hashing, all core helpers, both hosts, String methods in the guest on code units, JSON escapes, wrapper exotic | T1: `core/abi.rs`, `core/ir.rs`, `core/codegen/{expr,stmt,globals}.rs`, `core/emit/sections.rs`, `core/emit/props.rs`, `pkg/abi.ts`, `core/abi_drift_tests.rs`; T2: `core/emit/{runtime,props,json_parse,object}.rs`, `core/emit/props/descriptor.rs`, `core/emit/builtins/{json,mod,iter}.rs`; T3: `core/run/{host,value}.rs`, `pkg/{shim,value}.ts`; T4: `core/emit/builtins/string.rs`, `core/emit/realm.rs`, `core/run/host.rs`, `pkg/shim.ts` | Y1 (T1's zero-behaviour pre-step may start earlier, §9.3) | §1.4 silent-wrong list has tests and passes; every L2-F refusal deleted; the 311 encoding-sensitive passing tests still pass genuinely | **+60** (50–70) |
| **U2** Unicode semantics + string iterator | L2-U | code-point and Unicode-data operations | `core/emit/builtins/{string,iter}.rs`, `core/run/host.rs`, `pkg/shim.ts` | U1; Y2 (`@@iterator`, protocol, `%StringIteratorPrototype%`) | `METHOD_UNSUPPORTED` gone | **+25** (20–30) |
| **Y5** async iteration | L2-U | §5.8: `GetIterator(async)`, `%AsyncFromSyncIteratorPrototype%`, `for await`, async `yield*`, `Array.fromAsync`; denylist `Symbol.asyncIterator` removed | `core/codegen/{stmt,generator}.rs`, `core/emit/builtins/{generator,array}.rs`, new `core/emit/builtins/async_iter.rs`, `core/emit/realm.rs`, `tests/test262/unsupported-features.txt` | Y2 (records, `%AsyncIteratorPrototype%`, lazy destructuring, `yield*` modes) | `for await` / async `yield*` compile errors and their tests deleted | **+1125** (950–1300) |

Cumulative in the expected L1 merge order (§9.3), eligible counts on the CI
basis:

| after | expected pass | eligible | rate |
|---|---|---|---|
| `eee36ef` | 19378 | 30014 | 64.6 % |
| Y1 | 19878 | 32336 | 61.5 % (denominator grows first) |
| U1 | 19938 | 32336 | 61.7 % |
| Y2 | 20828 | 32336 | 64.4 % |
| U2 | 20853 | 32336 | 64.5 % |
| Y4 | 21063 | 32336 | 65.1 % |
| Y5 | 22188 | 32873 | 67.5 % |
| Y3 | 22648 | 32873 | 68.9 % |

Total ≈ **+3270** (2755–3725); without Y5 ≈ +2145 (21523 / 32336 =
66.6 %). Possible double count: none known (string-iterator tests are only in
U2's pool; class-computed tests only once).

Every L3 gate (no local test runs, campaign rule of 2026-10-30):
`cargo fmt --all --check && cargo check --workspace --all-targets && cargo
clippy --all-targets --workspace && typos`, plus `bun run lint` when `pkg/`
changes. Tests named in §9.5 are written failing-first but run only in the
final GitHub Actions pass (repo gate: nextest incl. opcount, doc tests,
shear, deny; web gate; test262 `--check` with zero regressions). The final
pass also regenerates `tests/test262/baseline.txt` / `history.tsv` and the
opcount snapshot (`JSC_OPCOUNT_UPDATE=1`), whose reason cells come from the
stage closeouts (§8.3). Each stage closeout updates DESIGN §4–§7, §13, §14
for what the stage changed; this document is folded into DESIGN.md and
deleted when 3c completes.

### 9.3 Dependency graph, two L2s, merge order

At most two implementation L2s run concurrently:

- **L2-Y** owns Y1 → Y2 → Y4 → Y3.
- **L2-U** owns U1 → U2 → Y5.

Stage DAG (an arrow means "merged before the dependent stage's flip L3s
start"):

```
Y1 ──► Y2 ──► Y4 ──► Y3          (L2-Y, serial)
 │      │
 │      ├──────────────┐
 ▼      ▼              ▼
U1 ──► U2 ──────────► Y5          (L2-U, serial)
```

Constraints: **Y1 → U1** (both edit `__to_key` / `__own_keys` / the
descriptor helpers, `core/abi.rs` and the decoders; U1 flips on top of
symbol keys), **Y2 → U2** (U2 closes out the string iterator Y2 creates),
**Y2 → Y5** (Y5 reuses Y2's records, lazy destructuring, `yield*` modes and
`%AsyncIteratorPrototype%`). Adjustments to the requested layout, with
reasons: Y5 is added to L2-U because L2-U's load (U1 + U2) is far smaller
than L2-Y's and Y5's files are disjoint from U2's and mostly from Y4/Y3's;
Y4 runs before Y3 because Y4 replaces the Y1 guards that Y3's helpers would
otherwise hit.

Timeline:

1. L2-Y runs Y1. L2-U runs **U1-T1a** only (the zero-behaviour pre-step:
   named `STR_*` constants, raw string accesses behind small emit / host
   helpers, drift entries; no layout change), then waits.
2. Y1 merged → L2-U rebases U1-T1a and runs U1-T1b..T5; L2-Y runs Y2.
3. U1 merged (expected first; fewer L3s) → L2-U waits for Y2 or prepares
   U2-T2/T3 (case mapping, `normalize` do not depend on Y2) on top of U1.
4. Y2 merged → L2-U runs U2 (T1 last), then Y5; L2-Y runs Y4, then Y3.

**Expected L1 merge order into main: Y1, U1, Y2, U2, Y4, Y5, Y3.** Rule: L1
merges each stage branch as soon as its stage is complete (green L3 gates,
stage closeout done); when two stages are ready together the earlier one in
this order merges first and the other L2 rebases. If L1 batches 3c into one
PR, the same order applies to the integration branch.

Hot files between the two L2s and how they are sequenced:

| file | L2-Y stage | L2-U stage | sequencing |
|---|---|---|---|
| `core/abi.rs`, `pkg/abi.ts`, `core/abi_drift_tests.rs` | Y1, Y2 | U1, U2 | additions grouped per stage; U1 after Y1 (constraint); Y2 vs U1: append-only, rebase is mechanical |
| `core/emit/props.rs`, `core/emit/props/descriptor.rs` | Y1 (`__to_key`, own-key modes, `HAS_SYMBOL_KEYS`) | U1-T1/T2 (hashing, string arms, wrapper exotic) | U1 after Y1 (constraint) |
| `core/run/value.rs`, `pkg/value.ts` | Y1 (symbol decoding), Y2-T5 (tombstone-aware Map/Set) | U1-T3 (string decoding, wrapper rendering) | U1-T3 after Y1; Y2-T5 rebases on U1 if U1 merged first (different functions) |
| `core/emit/builtins/iter.rs` | Y2-T1/T2 (`__str_iter_next`; `emit_iter_source` deleted) | U1-T2 (port of `__str_iter_next`), U2-T1 | whichever of Y2 / U1 merges second writes `__str_iter_next` on its final layout; with the expected order (U1 first) Y2-T1 writes it on code units directly and U1-T2 only ports `__str_to_array` |
| `core/emit/runtime.rs` | Y2-T5 (Map helpers) | U1-T2 (string helpers) | disjoint functions; rebase mechanical |
| `core/emit/sections.rs`, `core/ir.rs` | Y2-T2 (record IR nodes) | U1-T1b (`IrNode::Str` → `JsStr`, `collect_strings`) | U1-T1b before Y2-T2 when possible; else Y2-T2 rebases (rename only) |
| `core/emit/realm.rs` | every Y stage (registrations) | U1-T4 (String methods), U2, Y5-T1 | registrations grouped by family; one family per L3 |
| `core/emit/builtins/string.rs` | Y4-T4 (`@@match` … dispatch) | U1-T4, U2-T2/T3 | Y4-T4 is the last Y4 L3 and runs after U2-T3 merged |
| `core/emit/builtins/array.rs` | Y2-T2 (`emit_from`), Y4-T3 (species, concat) | Y5-T4 (`Array.fromAsync`) | Y5-T4 after Y4-T3 merged |
| `core/codegen/stmt.rs`, `core/codegen/generator.rs`, `core/emit/builtins/generator.rs` | Y1-T5 (class members), Y2-T2..T4 | U1-T1b (literal sites), Y5-T2/T3 | Y5 starts after Y2 merged (constraint); U1-T1b's literal edits are one-line per site |
| `tests/test262/unsupported-features.txt` | Y1 | Y5 | Y5 after Y1 (implied) |

### 9.4 Throw or be correct: interim guards

| behaviour reachable from | until | interim result |
|---|---|---|
| user `[Symbol.iterator]` on an object, or a modified `@@iterator` on a builtin prototype (Y1 makes the symbol writable as a key) | Y2 | every `IterSource` / spread / `GenResume` path (for-of, spread, destructuring, `yield*`, `Array.from`, `Object.fromEntries`, Map/Set ctors) does the real `GetMethod(v, @@iterator)` when the guard bits are not clean; a non-builtin value → `TypeError: Symbol.iterator protocol is not supported yet` |
| user `[Symbol.toPrimitive]` | Y4 | `ToPrimitive` chain test finds one → `TypeError: Symbol.toPrimitive is not supported yet` |
| user `[Symbol.toStringTag]` differing from the builtin tag table | Y4 | `TypeError: Symbol.toStringTag is not supported yet` |
| user `[Symbol.hasInstance]` on an `instanceof` RHS | Y4 | `TypeError: Symbol.hasInstance is not supported yet` |
| `C[Symbol.species]` reached by `ArraySpeciesCreate` with a `C` whose own table has `HAS_SYMBOL_KEYS` (a user `@@species`) | Y4 | `TypeError: Symbol.species is not supported yet` (today's stand-in of `species_ctor` stays for every other `C`) |
| `[Symbol.isConcatSpreadable]` on a `concat` argument/receiver | Y4 | `TypeError: Symbol.isConcatSpreadable is not supported yet` |
| object argument with `@@match/matchAll/replace/search/split` to a String method | Y4 | `TypeError: Symbol.<name> is not supported yet` |
| user `[Symbol.asyncIterator]` | Y5 | no consult site exists (`for await`, async `yield*` are compile errors; `Array.fromAsync` is absent); defining the key is harmless |
| `for await`, `yield*` in an async generator | Y5 | today's compile errors stay until deleted in Y5 |
| `Iterator`, `Iterator.from`, helpers | Y3 | absent: `Iterator is not defined` (`ReferenceError`, unchanged) |
| `return()` at a `yield` inside `try/finally` / `yield*`; `throw()` in a body with `yield*`; async generator `return()` in `try/finally` | Y2 | today's named `TypeError`s stay until deleted in Y2 |
| `yield*` in expression position | Y2 | today's compile error stays |
| B1, B2, B3, B4 (silently wrong today) | Y2 | no interim guard (they predate this design; Y2 is the fix) — Q18 |
| byte `.length` / indexing / iteration / comparison of non-ASCII strings, compile-time U+FFFD for lone surrogates (silently wrong today, DESIGN §13) | U1 | no interim guard (pre-existing; documented) — Q18; L2-F's refusals (§1.4) stay as the guard of the position-based methods until U1-T4 deletes them |
| `normalize` of non-ASCII text | U2 | today's `TypeError: String.prototype.normalize does not support non-ASCII strings yet` |

Each guard is the same lookup the honouring stage performs, so it moves no
opcount row beyond what that stage would; the honouring stage replaces the
throw with the call and deletes the guard's message (and its tests in
`crates/jsc-core/tests/unsupported.rs`).

### 9.5 L3 breakdown per stage

Test locations: `crates/jsc-core/tests/*.rs` (in-process, `mod common`),
`crates/jsc/tests/*.rs` (CLI), `packages/jsc/tests/run.test.ts` (shim
parity), `crates/jsc-core/src/abi_drift_tests.rs` (`expected()`),
`crates/jsc-core/src/{codegen,emit,run}/tests.rs` (unit),
`crates/jsc-core/tests/opcount/*.js` + `snapshot.tsv`. "Asserts" lists the
behaviours each test file must check; every test is written to fail on
`be9114d` (or on the stage's starting point) and pass after the L3. Every L3
gate is the §9.2 gate; tests run only in the final CI pass.

#### Y1 (L2-Y)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| Y1-T1 ABI, `typeof`, decoders, opcount programs | `core/abi.rs` (§7.6 Y1 row), `pkg/abi.ts`, `core/abi_drift_tests.rs` `expected()`; `core/emit/expr.rs` `emit_typeof_node`; `core/run/value.rs` `decode_heap` / `heap_object` / `table_entries` / `format_value`; `pkg/value.ts` `decodeHeap` / `decodeProps`; new `crates/jsc-core/tests/opcount/{for_of,spread}.js` (arrays, strings, Map, generator; rows filled by the final CI pass, reason "Y1-T1: new program") | — | `abi_drift_tests.rs`: the new constants mirrored; new `crates/jsc-core/tests/symbols.rs`: `typeof Symbol()` is `"symbol"`, rendering `Symbol(a)`, `Symbol()`, `[ Symbol(a) ]`, `{ a: 1, [Symbol(b)]: 2 }` (symbol keys last), uncaught `Uncaught Symbol(a)`; `run.test.ts`: the same four renderings through the shim |
| Y1-T2 conversions and the §2.3 audit | every §2.3 site; `__to_string`, `__to_number`, `__compare`, `emit_loose_eq`, `emit_to_object`, `emit_json_rep` | T1 | `symbols.rs`: `""+s`, `` `${s}` ``, `s + 1`, `+s`, `s < 1`, `Math.abs(s)` throw `TypeError`; `String(s)` = `"Symbol(x)"`; `new String(s)` throws; `s == s`, `Object(s) == s` true, `s == "Symbol(x)"` false; `!!s`; `JSON.stringify({a: s, b: 1})` = `{"b":1}`, `JSON.stringify([s])` = `[null]`, `JSON.stringify(s)` undefined; `typeof Object(s)` `"object"`, `Object(s) instanceof Symbol`, `Object(s).valueOf() === s`; `new Map([[s, 1]]).get(s)`, `[s].includes(s)`, `Object.is(s, s)`; `Object.defineProperty(s, "x", {})` throws; `Object.prototype.toString.call(s)` = `[object Symbol]` |
| Y1-T3 `Symbol` builtins, registry, wrappers, well-known blocks | new `core/emit/builtins/symbol.rs`; `core/emit/realm.rs` (`BuiltinCtor::Symbol`, `PLAIN_PROTOTYPES`, `Entry.key` → `Key`, 13 static symbol blocks, hidden registry block, `description` accessor); `core/codegen/globals.rs` `sealed` | T1 | `symbols.rs`: `Symbol("a") !== Symbol("a")`; `.description` (`undefined` for `Symbol()`); `Symbol.for("k") === Symbol.for("k")`, `Symbol.keyFor(Symbol.for("k"))` = `"k"`, `Symbol.keyFor(Symbol())` undefined, `Symbol.keyFor("k")` throws; `new Symbol()` throws `TypeError`; `Symbol.length` 0, `Symbol.name`; descriptor of `Symbol.iterator` (all false) and of the 13 well-known names; `Symbol.prototype.toString.call(1)` throws; `Symbol.prototype[Symbol.toPrimitive]` returns the primitive; `Symbol.iterator.toString()` = `"Symbol(Symbol.iterator)"`; `crates/jsc-core/tests/intrinsics.rs`: `%Symbol.prototype%` is watched (mutation disables the folded path) |
| Y1-T4 symbol keys and the §9.4 Y1 guards | `emit_to_key` symbol arm; `__define` / `__define_own` set `PROPS_HAS_SYMBOL_KEYS`; `emit_own_keys` `OWN_SYMBOLS` / `OWN_ALL_KEYS`; `emit_rest`; `emit_get_own_property_descriptors`, `emit_assign`, `object_define_properties` → `OWN_ALL_KEYS`; new `ReflectOp::GetOwnPropertySymbols`; run-time `SetFunctionName` for computed keys (`core/codegen/expr.rs` `static_key_name`, `lower_object`); the §9.4 Y1 guards in `emit_iter_source`, `emit_array_spread`, `emit_gen_resume` callers, `emit_to_primitive`, `emit_to_string` (object), `emit_instanceof_node`, `species_ctor`, `ArrayConcat`, string-method argument paths | T2, T3 | new `crates/jsc-core/tests/symbol_keys.rs`: `o[s] = 1; o[s]`, `s in o`, `delete o[s]`; `Object.keys` / `for-in` / `JSON.stringify` skip symbols; `Object.getOwnPropertySymbols` lists symbols in creation order, and `Object.getOwnPropertyDescriptors` lists string keys before symbol keys; `Object.assign` and `let {...r} = o` copy enumerable symbol keys; `Object.defineProperty(o, s, {value: 1})` + `getOwnPropertyDescriptor`; `Object.freeze(o)` then `Object.isFrozen(o)` with a symbol key; `({[s]() {}})[s].name` = `"[d]"`, getter `"get [d]"`, `Symbol()` key → `""`, string computed key → its string; `crates/jsc-core/tests/unsupported.rs`: each Y1 guard message of §9.4 (iterator, toPrimitive, toStringTag, hasInstance, species, isConcatSpreadable, `Symbol.match` argument) |
| Y1-T5 computed class member names (Q3) | `core/codegen/stmt.rs` `lower_class` / `lower_class_method` (key evaluated once, in order, at class definition; methods, accessors, generator / async methods, static and instance fields); delete `reject_out_of_scope_method` / `reject_out_of_scope_field`'s computed arms and their messages; `check_member_name` keeps static-`"prototype"` / field-`"constructor"` early errors for literal keys and adds the run-time `TypeError` for a computed static `"prototype"` | T4 | `crates/jsc/tests/classes.rs`: `class A { [k]() {} }`, `static [k]`, `get [k]()` / `set [k](v)`, `*[Symbol.iterator]()`, `async [k]()`, fields `[k] = v` / `static [k] = v`; key evaluation order with side effects (keys before field initializers; each `ToPropertyKey` once); method `name` from the key; computed `"constructor"` is an ordinary method; computed static `"prototype"` throws `TypeError`; `crates/jsc-core/tests/unsupported.rs`: delete the three computed-member compile-error tests |
| Y1-T6 closeout | `tests/test262/unsupported-features.txt` (`Symbol*` → `Symbol.asyncIterator`); DESIGN §4–§7, §13 ("no `Symbol`", computed class names), §14 (computed class member names leave phase 5); opcount reason drafts (§8.3 Y1 rows) | T1–T5 | — (gate only) |

#### Y2 (L2-Y)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| Y2-T1 iterator intrinsics and objects | `core/emit/realm.rs`: `%IteratorPrototype%`, `%ArrayIteratorPrototype%`, `%StringIteratorPrototype%`, `%MapIteratorPrototype%`, `%SetIteratorPrototype%`, `%AsyncIteratorPrototype%`; reparent `%GeneratorPrototype%` / `%AsyncGeneratorPrototype%` (`Intrinsic::proto`); five dirty bits; `CLASS_ITERATOR`; `Array.prototype.keys/values/entries/[@@iterator]`, `Map/Set.prototype.entries/keys/values/[@@iterator]`, `String.prototype[@@iterator]`; `next` bodies (in `core/emit/builtins/iter.rs`, which loses `emit_iter_source` in T2); `__str_iter_next` (§5.4) | Y1 | new `crates/jsc-core/tests/iterator_protocol.rs`: `[1,2].values().next()` = `{value: 1, done: false}`, `keys` / `entries` shapes; `Array.prototype[Symbol.iterator] === Array.prototype.values`; `Set.prototype.keys === Set.prototype.values`; `Map.prototype[Symbol.iterator] === Map.prototype.entries`; one shared `%IteratorPrototype%` above array, Map, Set, string and generator iterators, whose `[Symbol.iterator]()` returns `this`; `next` on a foreign receiver throws `TypeError`; a Map iterator sees entries appended during iteration; `"ab"[Symbol.iterator]().next().value` = `"a"`; `crates/jsc-core/tests/async_generators.rs`: `Object.getPrototypeOf(Object.getPrototypeOf(async function*(){}.prototype))` has `[Symbol.asyncIterator]` returning `this` |
| Y2-T2 record IR, for-of, spread, other consumers, close (B1, B3) | `core/ir.rs` (delete `IterSource`, `IterLen`, `IterItem`, `IsGenerator`; add `GetIterator`, `IteratorStep`, `IteratorValue`, `IteratorClose`); `core/codegen/stmt.rs` `lower_for_of` (one path; delete `lower_for_of_index`, `lower_for_of_generator`); `core/emit/array.rs` `emit_array_spread`; `core/emit/builtins/iter.rs` (delete `emit_iter_source`); `core/emit/realm.rs` (delete `Builtin::IterSource`); `core/emit/builtins/array.rs` `emit_from`, `object_static.rs` `emit_from_entries`, `ctor.rs` `aggregate_error`; guarded fast paths of §5.3; `core/emit/sections.rs`, `core/emit/expr.rs`; remove the Y1 iterator guard | T1 | `iterator_protocol.rs`: B1 (an own `next` on a generator is called by for-of, spread, `Array.from`; `[...it].length` 0); B3 (`break`, `return` and a body throw run a generator's `finally`; a user iterator's `return` is called once with no arguments; its non-object result → `TypeError` only on non-throw exits); user iterables in for-of / spread / call spread / `Array.from` / `Object.fromEntries` / `new AggregateError(it)`; `next` read once (replacing `next` mid-loop is unobservable); a non-object step result → `TypeError`; `Symbol.iterator` returning a primitive → `TypeError`; modifying `Array.prototype[Symbol.iterator]` is honoured by for-of and spread; `crates/jsc-core/tests/iteration.rs`: update the `is not iterable` texts that moved; opcount `for_of` / `spread` rows (final CI) |
| Y2-T3 lazy destructuring and close (B4) | `core/codegen/stmt.rs` `lower_binding_pattern`, `lower_destructuring_assign`, `assign_to_target`, `assign_array_pat`, `lower_array_rest`, `collect_array_rest`; delete `iter_snapshot` | T2 | `crates/jsc/tests/destructuring.rs`: B4 (`let [] = g()` and `[] = g()` never run the body; `[,] = g()` runs to the first `yield` only; `[a] = g()` calls `return` once); `[a] = infinite()` terminates; a default initializer that throws closes the iterator; rest drains and does not close; `next` / default evaluation order (`[a = f(), b = h()]` interleaves steps and defaults); for-of heads `for (const [a] of [g()])` close per element; parameters `function f([a]) {}` close |
| Y2-T4 generators: `return` / `throw` routing, `yield*` expression | `core/codegen/generator.rs` (`lower_yield_delegate` rewrite; delete `resume_limits`); `core/codegen/expr.rs` (`yield*` in expressions); `core/emit/builtins/generator.rs` (`emit_return` / `emit_throw` resume with `GEN_RESUME_MODE`; async request serving; delete `RETURN_UNSUPPORTED`, `THROW_UNSUPPORTED`, `ASYNC_RETURN_UNSUPPORTED`); `core/emit/generator.rs` state machine finally routing for an injected return; `core/abi.rs` (delete `GEN_LIMIT_*`, add `GEN_RESUME_MODE_*`) | T2 | `crates/jsc-core/tests/generator_methods.rs`: `return(v)` at a `yield` in `try/finally` runs the `finally` and yields `{value: v, done: true}`; a `finally` that `yield`s suspends again; a `finally` that returns overrides; `throw(e)` at a `yield*` reaches the delegate's `catch`; `crates/jsc-core/tests/generators.rs`: `const r = yield* inner()` gets the delegate's return value; sent values reach the delegate; a delegate without `throw` → close then `TypeError`; `return()` forwarded to the delegate; a generator suspended inside a for-of over a user iterator closes it on `return()`; `async_generators.rs`: `return()` inside `try/finally` settles after the `finally`; `unsupported.rs`: delete the three limit-message tests and the `yield*`-as-expression compile-error test |
| Y2-T5 Map/Set via protocol, tombstones, `arguments` (B2) | `core/emit/builtins/map_set.rs` `emit_ctor`, `emit_for_each`; `core/emit/runtime.rs` `emit_map_find` / `emit_map_set` / `emit_map_delete` (tombstones, live `size`); `core/emit/object.rs` `emit_map_op` (`clear`); `core/codegen/function.rs` `lower_arguments_object` (own `@@iterator` = `%Array.prototype.values%`); `core/run/value.rs` / `pkg/value.ts` Map/Set rendering skips tombstones | T2 | `crates/jsc-core/tests/map_set_date.rs`: B2 (delete during for-of and during `forEach` visits `[1, 2, 3]`); `clear()` during iteration then `set` → the new entry is visited; `size` after deletes; re-adding a deleted key appends; `new Set(arguments)`; a throwing adder closes a user iterator; a non-object Map entry → `TypeError` after close; `crates/jsc/tests/collections.rs`: `console.log` of a Map after deletes; `run.test.ts`: the same rendering through the shim |
| Y2-T6 closeout | DESIGN §5.2 (Map/Set layout), §6.4, §7.3, §7.8 (`yield*`, limits), §13 (iteration items, `forEach` skip); opcount reason drafts (`generators`, `map_set`, `for_of`, `spread`); `%GeneratorFunction%` intrinsics if time permits | T1–T5 | — (gate only; `%GeneratorFunction%` tests in `generator_functions.rs` if done) |

#### Y4 (L2-Y)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| Y4-T1 `@@toPrimitive`, `@@toStringTag` | `core/emit/convert.rs` `emit_to_primitive` (+ `Date.prototype[@@toPrimitive]`, real property); `core/emit/builtins/object.rs` `emit_to_string` (tag lookup; delete the Map/Set/Promise/Generator kind shortcuts; install tags on Map, Set, Promise, Generator, AsyncGenerator, Symbol, Math, JSON and the iterator prototypes) | Y2 | new `crates/jsc-core/tests/well_known.rs`: `[Symbol.toPrimitive]` receives `"number"` / `"string"` / `"default"`; non-callable → `TypeError`; object result → `TypeError`; `undefined` / `null` method falls back to `OrdinaryToPrimitive`; `new Date(0) + ""` uses hint `"default"` → string; `Object.prototype.toString.call(Math)` = `[object Math]`, `JSON` → `[object JSON]`, async generator object → `[object AsyncGenerator]`, `[].values()` → `[object Array Iterator]`; a user tag; a non-string tag ignored; `unsupported.rs`: delete the two Y1 guard tests |
| Y4-T2 `@@hasInstance` | `core/emit/object.rs` `emit_instanceof_node`; `core/codegen/expr.rs` `instanceof_kind`; `%Function.prototype%[@@hasInstance]` | T1 | `well_known.rs`: `x instanceof {[Symbol.hasInstance]: () => true}`; `static [Symbol.hasInstance]` on a class; result coerced with `ToBoolean`; non-callable → `TypeError`; `Function.prototype[Symbol.hasInstance]` descriptor (non-writable, non-configurable) and `call` on a bound function; builtin `instanceof` arms unchanged (`[] instanceof Array`) |
| Y4-T3 `@@species`, `@@isConcatSpreadable`, `@@unscopables` | `core/emit/builtins/array.rs` `species_ctor` (`Get(C, @@species)`; delete the chain-walk stand-in), `get [Symbol.species]` on `%Array%`, `%Map%`, `%Set%`, `%Promise%`; `core/emit/builtins/promise.rs` `SpeciesConstructor` in `then` / `finally`; `ArrayConcat` spreadable test; `Array.prototype[@@unscopables]`; ctor dirty bits for `Array` / `Promise` | T1 | `well_known.rs`: subclass with `static get [Symbol.species]() { return Array }` gets plain arrays from `map` / `filter` / `slice` / `splice` / `concat` / `flat` / `flatMap`; `@@species` `null` / `undefined` → `ArrayCreate`; non-constructor species → `TypeError`; `Promise` subclass `then` uses species; `concat` of `{length: 2, 0: 1, 1: 2, [Symbol.isConcatSpreadable]: true}` spreads, an array with it `false` does not; `Array.prototype[Symbol.unscopables]` is a null-prototype object with `flat: true`; `unsupported.rs`: delete the Y1 species / isConcatSpreadable guard tests |
| Y4-T4 String-method dispatch | `core/emit/builtins/string.rs`: `match`, `matchAll`, `replace`, `replaceAll`, `search`, `split` call `GetMethod(arg, @@x)` for object arguments; `IsRegExp` (`@@match`) in `startsWith` / `endsWith` / `includes` | T1; after U2-T3 merged (§9.3) | `well_known.rs`: `"abc".replace({[Symbol.replace]: (s, r) => s + r}, "x")`; same for `split`, `search`, `match`, `matchAll`, `replaceAll`; `"a".startsWith({[Symbol.match]: true})` throws `TypeError`; `{[Symbol.match]: false}` is coerced to a string; `unsupported.rs`: delete the Y1 `Symbol.match` guard test |
| Y4-T5 closeout | DESIGN §6.5, §7.6, §7.9, §13 (`ArraySpeciesCreate`, toStringTag items); opcount reason drafts | T1–T4 | — |

#### Y3 (L2-Y)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| Y3-T1 `Iterator`, `Iterator.from`, wrap prototype | new `core/emit/builtins/iterator.rs`; `core/emit/realm.rs` (`BuiltinCtor::Iterator`, `%WrapForValidIteratorPrototype%`, `constructor` / `@@toStringTag` accessors on `%IteratorPrototype%`) | Y2, Y4 | new `crates/jsc-core/tests/iterator_helpers.rs`: `new Iterator()` throws, `class I extends Iterator {}` constructs; `Iterator.prototype === Object.getPrototypeOf(Object.getPrototypeOf([].values()))`; `Iterator.from([1])` is iterable; `Iterator.from({next() {…}})` wraps and forwards `return`; strings are iterated, other primitives throw |
| Y3-T2 lazy helpers | `map`, `filter`, `take`, `drop`, `flatMap`; `%IteratorHelperPrototype%` (`next`, `return`, tag `"Iterator Helper"`) | T1 | `iterator_helpers.rs`: laziness (no `next` call before the first helper `next`); `take(2)` closes the source; `drop(-1)` / `take(NaN)` → `RangeError` after `ToNumber`; non-callable mapper → `TypeError` before any step; `flatMap` rejects a string result; helper `next` while running → `TypeError`; `return()` before start closes the source |
| Y3-T3 eager helpers | `reduce`, `toArray`, `forEach`, `some`, `every`, `find` | T1 | `iterator_helpers.rs`: results; `some` / `every` / `find` close on an early result; a throwing callback closes; `reduce` without an initial value on an empty iterator → `TypeError` |
| Y3-T4 `Promise` combinators, `Math.sumPrecise` | `core/emit/builtins/promise.rs` (`all`, `allSettled`, `any`, `race` over the protocol, `__then` per element, `AggregateError` for `any`); `Math.sumPrecise` | T1 | new `crates/jsc-core/tests/promise_combinators.rs`: resolution order and values of each combinator over arrays, Sets and generators; `any` of all-rejected → `AggregateError` with `errors`; a throwing `next` rejects; a throwing `resolve` lookup closes the iterator; `Promise.all` uses `this` as constructor; `Math.sumPrecise([0.1, 0.2, 0.3])` exact sum, non-number element → `TypeError` |
| Y3-T5 closeout | DESIGN §7.9, §13 (no `Promise.all`, `Math.sumPrecise`), §14 | T1–T4 | — |

#### U1 (L2-U)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| U1-T1a zero-behaviour pre-step | named `STR_LEN_OFFSET` / `STR_DATA_OFFSET` in `core/abi.rs`, `pkg/abi.ts`, `core/abi_drift_tests.rs`; every raw string access (§1.4 list) behind small emit helpers (`emit_str_len`, `emit_str_data`) and host / shim helpers | — (may start before Y1) | `abi_drift_tests.rs`: the two offsets mirrored (no behaviour change; every existing test is the regression test) |
| U1-T1b layout flag, lossless literals, code-unit hashing | `STR_FLAG_TWO_BYTE`; `core/ir.rs` `IrNode::Str` → code-unit string; `core/codegen/{expr,stmt,globals}.rs` literal sites (§1.4, incl. `String.raw` raw strings); `core/emit/sections.rs` `collect_strings` / `emit_data_section`; `core/emit/props.rs` `fnv1a` / `emit_str_hash` / `is_array_index` on units | Y1 merged, T1a | `core/codegen/tests.rs`: a lone-surrogate literal lowers to the unit `0xD800`; `"\u00e9"` lowers to one unit; `core/emit/tests.rs`: `fnv1a` over ASCII units equals the byte hash of today (intern table unchanged); one-byte vs two-byte data blocks are canonical |
| U1-T2 guest helpers | `core/emit/runtime.rs` (`emit_concat`, `emit_str_eq`, `emit_str_cmp`, `emit_str_index`, `emit_str_to_array`, `emit_json_escape`), `core/emit/props.rs` (`emit_index_value`, `emit_index_string`, intern), `core/emit/json_parse.rs` (`\u` escapes), `core/emit/builtins/json.rs` `emit_gap`, `core/emit/builtins/mod.rs` `emit_to_object` + `__str_exotic_index` in `core/emit/props.rs` / `core/emit/props/descriptor.rs` (§3.3), `core/emit/builtins/iter.rs` (`__str_iter_next` port if Y2 merged first) | T1b | new `crates/jsc-core/tests/string_units.rs`: `"é".length` 1, `"😀".length` 2, `"😀"[0]` = `"\uD83D"`; `"\uD800" !== "\uDBFF"`; `"\uFFFF" < "\u{10000}"` false; `["\u{10000}", "\uFFFF"].sort()` order; `"é" + "😀"` length 3; `JSON.parse('"\\u0041\\uD800"')` length 2; `JSON.stringify("\uD800")` = `"\"\\ud800\""`, a pair verbatim; `JSON.stringify({a: 1}, null, "ééééééééééé")` uses 10 units; `Object.getOwnPropertyDescriptor(new String("é"), 0)` = `{value: "é", writable: false, enumerable: true, configurable: false}`; `Object.keys(new String("aé"))` = `["0", "1"]`; `new String("a")[0] = "b"` throws; `Object.isFrozen(Object.freeze(new String("ab")))`; `crates/jsc-core/tests/wrappers.rs`: update the per-byte index expectations |
| U1-T3 hosts | `core/run/host.rs` (`block_len`, `string_bytes`, `string_text`, `alloc_string`), `core/run/value.rs` `decode_string` and wrapper rendering, `pkg/shim.ts` `allocString`, `pkg/value.ts` `decodeString`; console output U+FFFD for lone surrogates | T1b (∥ T2) | `core/run/tests.rs`: decode of one-byte and two-byte blocks; `crates/jsc/tests/strings.rs`: CLI prints `é`, `😀`, a lone surrogate as U+FFFD, `[String: 'é']`; `run.test.ts`: the same through the shim; Latin-1 bytes `0x80..=0xFF` decode to U+0080..U+00FF (not windows-1252) |
| U1-T4 String methods in the guest | `core/emit/builtins/string.rs` (every moved op of §7.5 on code units; `isWellFormed`, `toWellFormed`; `trim*` table; RegExp-syntax check), `core/emit/realm.rs` (`StringOp::flag` shrinks), `core/run/host.rs` / `pkg/shim.ts` (delete the moved arms and helpers), `core/abi.rs` / `pkg/abi.ts` / drift (delete the constants) | T2, T3 | `crates/jsc-core/tests/strings.rs`: every former refusal returns the spec result — `"aé".charAt(1)` = `"é"`, `"😀".at(-1)` = `"\uDE00"`, `"aé😀".slice(1, 3)`, `"é".padStart(3, "é")`, `"aé".split("")`, `"😀".split("")` (two units), `"😀".replace("", "-")`, `String.fromCharCode(0xD800).length` 1, `String.fromCodePoint(0xDC00)`, `"\uD800".isWellFormed()` false, `toWellFormed()` → U+FFFD; `"\u3000a\u3000".trim()` = `"a"`; delete the refusal-message tests; `run.test.ts`: the same matrix through the shim |
| U1-T5 closeout | DESIGN §4–§6, §6.9, §13 (strings item rewritten), `string_methods` / `json` / `for_in` opcount reason drafts | T1–T4 | — |

#### U2 (L2-U)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| U2-T1 string iterator closeout | `core/emit/builtins/iter.rs` (`__str_iter_next` on code points with surrogate pairs; `%StringIteratorPrototype%.next` brand check) | U1, Y2 | new `crates/jsc-core/tests/unicode_strings.rs`: `[..."a😀b"]` has 3 elements; `[..."\uD800\uD800"]` 2 lone units; `for (const c of "😀")` once; `Array.from("😀").length` 1; `next.call({})` throws |
| U2-T2 case mapping | guest Latin-1 table (`ÿ` → U+0178, `µ` → U+039C, `ß` → `SS` leave Latin-1) + host for the rest (`TO_UPPER` / `TO_LOWER` on lossless units; lone surrogates unchanged) | U1 | `unicode_strings.rs`: `"ß".toUpperCase()` = `"SS"`, `"ÿ".toUpperCase()` = `"\u0178"`, final sigma `"ΑΣ".toLowerCase()` = `"ας"`, `"\uD800".toUpperCase()` unchanged; `run.test.ts` parity |
| U2-T3 `normalize`, `localeCompare`, code-point audit | host normalizer (`icu_normalizer`, Q5) and shim `String.prototype.normalize`; delete `METHOD_UNSUPPORTED`; `localeCompare` code-unit order on lossless units; audit `codePointAt` / `fromCodePoint` | U1 | `unicode_strings.rs`: `"\u0041\u030A".normalize("NFC")` = `"\u00C5"`, NFD / NFKC / NFKD, invalid form → `RangeError`; `"a".localeCompare("b")`; delete the `normalize` refusal test |
| U2-T4 closeout | DESIGN §13, §14 (strings done), `Cargo.toml` / `deny.toml` for the normalizer dependency | T1–T3 | — |

#### Y5 (L2-U)

| L3 | scope | depends on | tests (file: asserts) |
|---|---|---|---|
| Y5-T1 `GetIterator(async)`, `%AsyncFromSyncIteratorPrototype%`, `AsyncIteratorClose` | new `core/emit/builtins/async_iter.rs`; `core/emit/realm.rs`; record `async` bit in `core/ir.rs` nodes | Y2 | new `crates/jsc-core/tests/async_iteration.rs`: the async-from-sync wrapper's `next` resolves `{value, done}` with awaited values; a rejected value closes the sync iterator; missing `throw` closes then rejects with `TypeError`; missing `return` resolves `{done: true}` |
| Y5-T2 `for await` | `core/codegen/stmt.rs` `lower_for_of` (`is_await`; delete "for-await-of is not supported"); `core/codegen/generator.rs` await suspends per step; destructuring heads | T1 | `async_iteration.rs`: `for await` over an async generator, over an array of promises, over a user `[Symbol.asyncIterator]`; `break` / a body throw await `return()`; a non-object step result rejects; `for await` at the top level of `main`; `unsupported.rs`: delete the `for await` compile-error test |
| Y5-T3 `yield*` in async generators | `core/codegen/generator.rs` `lower_yield_delegate` async arm (delete its rejection); `core/emit/builtins/generator.rs` mode forwarding with awaits | T1 | `async_generators.rs`: `yield*` over an async generator and over a sync iterable; sent values, `throw` and `return` forwarded; the `yield*` value is the delegate's return; `unsupported.rs`: delete the async-`yield*` compile-error test |
| Y5-T4 `Array.fromAsync` | `core/emit/builtins/array.rs` (async builtin through the driver) | T1; after Y4-T3 merged | `async_iteration.rs`: `Array.fromAsync` over an async iterable, a sync iterable of promises, an array-like with promise elements; `mapFn` awaited; a non-callable `mapFn` rejects |
| Y5-T5 closeout | `tests/test262/unsupported-features.txt` (delete `Symbol.asyncIterator`); DESIGN §7.8, §13, §14 (`for await` and async `yield*` leave phase 5) | T1–T4 | — |

---

## 10. Risks

1. **Missed "heap, not string ⇒ object" site** (Y1): a symbol read as an
   object. Mitigation: the §2.3 list, `@4 = 0`, flag bits `1`/`2` clear,
   a test per listed site.
2. **Generator close and `return()` routing** (Y2): CFG lowering of the
   implicit finally inside generators; `yield*` needs the resume-mode
   channel; destructuring defaults now interleave with `next` calls (order
   is test262-visible); async generator request serving shares the routing.
3. **Big-bang layout flip** (U1): once `len` means code units, every reader
   (guest helpers, both hosts) must agree. Mitigation: T1a's zero-behaviour
   pre-step; the flip is gated on the L2 branch only after T2 and T3 merge.
4. **Accidental passes lost** (U1): ~311 encoding-sensitive tests pass today,
   some only because U+FFFD collapsing makes both sides of an assertion
   equal; they must become genuinely correct, not newly failing.
5. **No local test runs**: every failing-first test and opcount delta is
   first executed in the final CI pass, so regressions surface late and in
   bulk. Mitigation: narrow L3 scopes, the stage closeouts' reason drafts,
   and fixing L3s that L1 dispatches from the CI report.
6. **Map/Set tombstones without compaction** (Y2): lookups scan dead entries;
   add/delete churn grows the backing array until phase 4.
7. **Species** (Y4): every array-producing builtin reads `constructor`;
   the guarded fast path must prove `constructor === %Array%` or array HOF
   fuel rises.
8. **Host normalizer dependency** (U2): a direct `icu_normalizer` dependency
   (already in `Cargo.lock` transitively) must pass `cargo deny`/`shear`;
   phase 4 moves the tables into the guest anyway.
9. **Realm image size**: every module carries the new intrinsics; test262
   compiles one module per test (compile-time cost, not runtime).
10. **Estimate uncertainty**: Y3's Promise share is capped by thenable
    assimilation (DESIGN §13); Y5's share depends on the async runner and on
    Y2's destructuring; the pools come from an offline scan (±0.3 %
    eligibility error, pattern-based attribution).
11. **Two L2s on shared files** (§9.3): mitigated by the Y1 → U1 / Y2 → U2 /
    Y2 → Y5 constraints, family-grouped realm registrations, and the hot-file
    table.

---

## 11. Open questions (recommended default in bold; approved 2026-10-30, refreshed against `be9114d`)

1. **Symbol representation** — **`TYPE_SYMBOL = 14` under the heap tag** vs a
   new NaN-box tag (§2.1).
2. **Registry storage** — **hidden static `TYPE_PROPS`-backed realm block
   keyed by interned strings** vs a guest Map (linear SameValueZero scan).
3. **Computed class member names** (no phase-3 L2 implemented them;
   `be9114d` still rejects them in `reject_out_of_scope_method` /
   `reject_out_of_scope_field`; 371 eligible failing tests contain them plus
   53 newly eligible at Y1, ≈ +223 expected) — **implement in Y1-T5 and move
   the item out of phase 5**; required for iterable classes.
4. **Async iteration timing** — *changed*: **stage Y5 in wave 3c** (owned by
   L2-U after U2; §5.8) vs phase 5. Async generators landed (L2-L T5), so
   whatever the timing Y2-T1 adds `%AsyncIteratorPrototype%` (proto
   `%Object.prototype%`, `[@@asyncIterator]() { return this }`) and
   reparents `%AsyncGeneratorPrototype%` under it, and Y4 gives async
   generator objects the `"AsyncGenerator"` tag. Options: (a) phase 5 —
   +0 in 3c, the 1194 currently eligible async-iteration tests stay failing
   and 537 `Symbol.asyncIterator` tests stay denied; (b) only the prototype
   parenting (in Y2) — ≈ +5; (c) **Y5: `for await`, async `yield*`,
   `%AsyncFromSyncIteratorPrototype%`, `Symbol.asyncIterator`,
   `Array.fromAsync`** — pool 1730, ≈ **+1125** (950–1300), the largest
   single stage, with no new ABI and no host change.
5. **Where case mapping / `normalize` live** — **host in phase 3** (Rust
   `char::to_uppercase` — already used by L2-F — / `icu_normalizer`, shim
   native JS), guest Latin-1 fast path; tables move into the guest in phase
   4.
6. **Generator `return()`/`throw()` limits** — **lift in Y2** (needed for
   `IteratorClose` inside generators and spec `yield*`) vs extend
   `GEN_LIMIT_RETURN` to bodies with for-of and keep the `TypeError`s. On
   `be9114d` `throw()` already resumes at a plain `yield` (L2-L); the
   remaining limits are `return()` in bodies with `finally` / `yield*` and
   `throw()` in bodies with `yield*`, for sync and async generators (§5.2).
7. **Map/Set deletion** — **tombstones, no compaction in phase 3** (§5.5)
   vs compaction on growth with a forward-chain transition for live
   iterators.
8. **Per-generator-function `prototype`** — **delivered by L2-L** (on
   `be9114d`, DESIGN §7.8); Y2 only reparents `%GeneratorPrototype%` (and
   `%AsyncGeneratorPrototype%`) under the iterator prototypes.
9. **`HAS_SYMBOL_KEYS` table flag** — **adopt** vs a per-lookup `__get` of
   the symbol key on every hook site.
10. **Error message texts** — **V8 wording** (`Cannot convert a Symbol value
    to a string`, `x is not iterable`, `Result of the Symbol.iterator method
    is not an object`); test262 checks only the constructor.
11. **Canonical one-byte form** — **yes** (§6.2).
12. **Cached hash word / ropes / slice views** — **no** to all three; revisit
    after phase 4 if profiles show `__intern` or concatenation.
13. **Maximum string length** — **keep `MAX_STRING_LENGTH = 2^29 − 24`
    (`536_870_888`), now in code units** (V8's limit); a two-byte string
    that large exceeds the 512 MiB cap and fails as OOM `RangeError`.
14. **Interned single-unit cache `0..=0xFF`** — **not in U1**; measure after
    U1 (lazily filled 256-slot table, no data-segment growth).
15. **Where the code-point string iterator lands** — **Y2 introduces
    `%StringIteratorPrototype%` and `String.prototype[@@iterator]`; its
    `__str_iter_next` steps UTF-8 sequences if Y2 merges before U1, code
    units otherwise; the second of Y2-T1 / U1-T2 ports it; U2-T1 closes
    out** vs leaving strings on a specialised per-byte path until U2.
16. **String wrapper exotic and the landed L2-O descriptors** — **U1
    replaces the eager per-index definition with the shared
    `__str_exotic_index` arm in `__get`, `__has`, `__own_keys`, `__own_desc`,
    `__define_own`, `__own_enum`, `__set`, `__delete`, `__test_integrity`**
    (§3.3) vs keeping eager definition per code unit (O(len) keys per
    wrapper and per primitive `ToObject`).
17. **Host `__method` ops for code-unit methods** — **delete; move into the
    guest in U1-T4** — now the 21 string ops of §7.5 (L2-F added `AT` …
    `FROM_CODE_POINT`), with case mapping, `normalize`, `localeCompare`,
    number formatting and `Math` staying on the host.
18. **Interim guards for pre-existing silent-wrong paths** (B1–B4, byte
    semantics of non-ASCII strings) — **none beyond L2-F's refusals; fix in
    Y2 / U1** vs adding a `TypeError` before those stages land (e.g. throwing
    on any non-ASCII `.length`), which would regress currently-passing
    tests. L2-F's refusals already guard the position-based String methods
    and are deleted by U1-T4.
19. **Does U1 wait for L2-F?** — *resolved*: **no wait** — L2-F landed in
    `24d466f`; U1 depends only on Y1 (§9.3).
20. **Who owns Y5** (new) — **L2-U after U2** vs L2-Y after Y2 (delays Y4 /
    Y3 and idles L2-U) vs a third L2 (exceeds the two-L2 limit).
21. **Iterator proposals beyond ES2025 `iterator-helpers`** (new):
    `iterator-sequencing`, `joint-iteration`, `iterator-chunking`,
    `iterator-includes`, `Iterator.prototype.join`, `await-dictionary`
    (≈ 309 eligible failing tests) — **not in 3c and not added to the
    denylist** (the eligible denominator stays comparable across phase 3)
    vs implementing `Iterator.concat` in Y3 (31 tests) vs denying them.
