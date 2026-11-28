# jsc — Design

> An ahead-of-time **JavaScript → WebAssembly** compiler written in Rust, with
> conformance tracked by test262. jsc parses source with swc, lowers it to an
> in-house IR, emits a self-contained WASI preview-1 command module with
> `wasm-encoder`, and runs that module on wasmtime (natively) or on a JS WASI
> polyfill (in the browser and Node).

This is the authoritative technical spec. It describes the compiler as it is
today: the pipeline, the value and memory model, the module ABI, the object
model, the runtime and the WASI host boundary, the supported language surface,
diagnostics, tooling, and the known limitations.

## Contents

- [1. Overview](#1-overview)
- [2. Pipeline](#2-pipeline)
- [3. Crate & package layout](#3-crate--package-layout)
- [4. Value model & module ABI](#4-value-model--module-abi)
- [5. Heap & object model](#5-heap--object-model)
- [6. Runtime](#6-runtime)
- [7. Language surface](#7-language-surface)
- [8. Diagnostics](#8-diagnostics)
- [9. CLI](#9-cli)
- [10. Conformance (test262)](#10-conformance-test262)
- [11. CI gates](#11-ci-gates)
- [12. Building a static release](#12-building-a-static-release)
- [13. Current limitations](#13-current-limitations)
- [14. Roadmap](#14-roadmap)

---

## 1. Overview

jsc compiles a JavaScript program **ahead of time** into a WebAssembly module.
There is no interpreter and no JIT: every program becomes a real, validatable
wasm module whose exported `_start` function runs the top-level code.

- **Output.** A wasm module (`jsc build FILE -o OUT.wasm`), or an immediate run of
  that module on an embedded wasmtime (`jsc FILE`, `jsc -e`, `jsc -p`).
- **Embeddable core.** `jsc-core` is a library: `frontend::parse` →
  `codegen::lower` → `emit::emit` produce module bytes; the optional `run` stage
  (feature `native-run`) executes them on wasmtime.
- **In the browser.** `crates/jsc-wasm` compiles `jsc-core` (without the `run`
  stage) to wasm via `wasm-bindgen`; the `@cdlab/jsc` package (`packages/jsc`)
  wraps it with a TypeScript WASI preview-1 polyfill, and `apps/jsc-web` is a browser
  playground on top of that package.
- **Input language.** JavaScript, or TypeScript reduced to JavaScript by type
  stripping (no type checking). `frontend::parse(src, dialect)` takes a
  `Dialect`: the CLI selects `Dialect::Ts` for `.ts` / `.mts` / `.cts` files,
  rejects `.tsx`, and uses `Dialect::Js` for every other file and for `-e` /
  `-p`; `jsc-wasm` and the test262 runner always use `Dialect::Js`.
- **Semantics target.** Full JavaScript semantics is the direction; the compiled
  subset follows strict-mode rules (§7, §13). Conformance is measured by the
  test262 runner on the official, unmodified harness (§10).

Three structural decisions shape the implementation:

- **The IR is the seam.** A Rust `enum IrNode` / `IrStmt` (`crates/jsc-core/src/ir.rs`)
  sits between the swc AST and the wasm backend. swc AST shapes never reach
  `emit`; wasm concerns never reach `codegen`. Language features are added by
  extending the IR and the two walks over it.
- **Emit wasm directly.** The backend builds the module straight from the IR with
  `wasm-encoder` — no C, text, or LLVM intermediate. Builtins are wasm
  functions emitted into every module, installed on static realm objects (§6.4).
- **One value word.** Every JS value is a single NaN-boxed `i64`; all heap data
  lives in one linear memory with a page allocator (§4, §5).

---

## 2. Pipeline

```
JS source ──▶ swc AST ──▶ IR ──────────▶ wasm bytes ──▶ result
            frontend     codegen         emit           run (wasmtime)
            swc_ecma_    IrProgram /     wasm-encoder   or any WASI host
            parser       IrNode/IrStmt                  (@cdlab/jsc, node:wasi)
```

1. **frontend** (`frontend.rs`) — hands the source to `swc_ecma_parser` with
   `Syntax::Es(EsSyntax::default())` (`Dialect::Js`) or
   `Syntax::Typescript(TsSyntax::default())` (`Dialect::Ts`) and returns a
   `swc_ecma_ast::Program`. A fatal parse error, or else the first recoverable
   error swc collected (e.g. a strict-mode early error), becomes a spanned
   `CompileError` (§8). `Dialect::Ts` programs then run through swc's
   `resolver` and TypeScript `strip` pass, so codegen only sees plain
   ECMAScript.
2. **codegen** (`codegen/`) — runs a whole-program scan for the global facts
   (§6.4), then walks the AST and builds an `IrProgram`: the `main` entry
   (top-level statements), the lifted user functions with their `name` /
   `length` / kind, and per-program facts (uniform arity `K`, module-cell
   count). This stage owns scoping, capture analysis, desugaring, the
   generator/async state machines, and every compile-time rejection.
3. **emit** (`emit/`) — walks the IR and produces the module: types, imports,
   function bodies, the runtime helpers and builtin bodies, the funcref table,
   memory, globals, exports, and the data segments holding the static region
   (interned strings, function data, the realm image, the intern table, the Unicode tables,
   the collector's static root list).
4. **run** (`run/`, feature `native-run`) — instantiates the module on wasmtime
   with a `Linker` that supplies the four WASI imports (§6.1), calls
   `_start`, and maps `proc_exit` to an exit code. The module itself drains the
   microtask queue, reports an uncaught exception or unhandled rejection, and
   writes its output (§4.2, §6.9).

`run::run` takes finished module bytes; the CLI and `jsc-wasm` call the stages
individually.

---

## 3. Crate & package layout

A Cargo workspace (`resolver = "3"`) with four members, plus a Bun workspace for
the JS side.

```
jsc/
├── Cargo.toml                 # workspace, shared lints, [profile.release], [profile.dist]
├── rust-toolchain.toml        # channel 1.99.1 (+ rustfmt, clippy)
├── crates/
│   ├── jsc-core/              # library: the compiler pipeline
│   │   ├── src/
│   │   │   ├── lib.rs             # stage modules; `run` behind feature `native-run`
│   │   │   ├── abi.rs             # single source of the runtime ABI constants
│   │   │   ├── abi_drift_tests.rs # pins abi.rs layouts to this document
│   │   │   ├── error.rs           # CompileError (message + optional span) + caret renderer
│   │   │   ├── frontend.rs        # swc parse wrapper
│   │   │   ├── ir.rs              # IrProgram / IrFunction / IrNode / IrStmt
│   │   │   ├── codegen/           # AST -> IR
│   │   │   │   ├── mod.rs             # driver, shared lowering state, module cells
│   │   │   │   ├── globals.rs         # whole-program global facts (sealed, intrinsics-sealed)
│   │   │   │   ├── expr.rs  stmt.rs   # expression / statement lowering
│   │   │   │   ├── op.rs              # operator, Math and builtin-name mapping
│   │   │   │   ├── function.rs        # function / arrow / method / class lifting
│   │   │   │   ├── capture.rs         # capture analysis
│   │   │   │   ├── arity.rs           # uniform-ABI K scan
│   │   │   │   ├── scope.rs           # scope stack, binding resolution, TDZ guard
│   │   │   │   └── generator.rs       # generator / async state-machine CFG
│   │   │   ├── emit/              # IR -> wasm
│   │   │   │   ├── mod.rs             # module builder, globals, Helpers index table
│   │   │   │   ├── registry.rs        # the helper roster: groups, signatures and bodies in function-index order
│   │   │   │   ├── sections.rs        # type / function sections, globals, data segment
│   │   │   │   ├── props.rs           # property tables, interning, __get/__set/...
│   │   │   │   ├── realm.rs           # realm image: intrinsics registry, Builtin roster
│   │   │   │   ├── builtins/          # builtin bodies by family (array, string, json, ...)
│   │   │   │   ├── convert.rs         # ToPrimitive, ToString, `+`, relational, join
│   │   │   │   ├── expr.rs  stmt.rs   # node / statement emission
│   │   │   │   ├── libm.rs            # Math transcendentals (musl/msun port)
│   │   │   │   ├── numeric.rs         # arithmetic, comparisons, Math, conversions
│   │   │   │   ├── value.rs           # jsval helpers (__box_f64, __to_number, type tests)
│   │   │   │   ├── array.rs  object.rs function.rs generator.rs
│   │   │   │   ├── json_parse.rs      # in-wasm JSON.parse
│   │   │   │   ├── render.rs          # guest console renderer, output writer, _start helpers
│   │   │   │   └── runtime.rs         # __alloc, strings, Map/Set, equality, trampolines
│   │   │   └── run/               # wasmtime runner: the four WASI imports
│   │   │       └── harness.rs         # Linker (preview 1), _start call, proc_exit -> exit code
│   │   └── tests/             # integration tests by theme (arrays, properties, intrinsics,
│   │                          #   intrinsic_guards, to_primitive, wrappers, ...) and the
│   │                          #   opcount snapshot gate (opcount.rs + opcount/)
│   ├── jsc/                   # binary: clap CLI (src/main.rs) + CLI integration tests
│   ├── jsc-test262/           # test262 conformance runner (§10)
│   └── jsc-wasm/              # wasm-bindgen boundary: `compile(src, options?) -> Uint8Array`
├── packages/jsc/              # @cdlab/jsc: compile() + run() in JS, WASI preview-1 polyfill
│   └── src/  index.ts  wasi.ts
├── apps/jsc-web/              # browser playground (Vite + React + CodeMirror)
├── scripts/                   # build-wasm.sh, ensure-wasm.sh, test262-fetch.sh
└── tests/test262/             # unsupported-features denylist, baseline, history
```

**`jsc-core`.** The library. `#![forbid(unsafe_code)]`. The default feature
`native-run` pulls in `wasmtime` (`cranelift`, `runtime`, `std`) and
the runner hand-writes the four WASI imports; building with
`default-features = false` leaves frontend + codegen + emit only. Dependencies:
`swc_ecma_parser 46`, `swc_ecma_ast 29`, `swc_ecma_visit 29`, `swc_common 26`,
`swc_ecma_transforms_base 50`, `swc_ecma_transforms_typescript 56`,
`wasm-encoder 0.262`, `anyhow`, optional `wasmtime 50`;
`wasmparser 0.262` is
a dev-dependency.

**`jsc`.** A thin `clap` shell (§9). `#![forbid(unsafe_code)]`.

**`jsc-wasm`.** `crate-type = ["cdylib", "rlib"]`, depends on `jsc-core` with
`default-features = false`. Exposes one `#[wasm_bindgen]` function,
`compile(src, options?) -> Result<Vec<u8>, JsError>` (`options.printCompletion`
sets `print_completion`, §4.2); a spanned error becomes the caret block labelled
`<source>`. Its lint table mirrors the workspace's but relaxes
`unsafe_code` to `deny` because the `wasm-bindgen` glue expands to generated
`unsafe` shims. `wasm-pack` builds it with `wasm-opt -Oz`.

**`@cdlab/jsc`** (`packages/jsc`). `compile(src)` returns module bytes from the
wasm-bindgen artifact; `run(src, options?)` compiles, instantiates the module
against an in-memory `wasi_snapshot_preview1` polyfill (`wasi.ts`, the same
four functions the module imports and nothing else), calls `_start`, and
returns `{ stdout, stderr, exitCode, completion? }`. An uncaught exception or
unhandled rejection is the `Uncaught …` text on `stderr` with `exitCode` 1;
`run` rejects on a compile error or a guest trap. The package knows no heap
layout. The wasm artifact is gitignored;
`scripts/ensure-wasm.sh` rebuilds it (via `scripts/build-wasm.sh` →
`wasm-pack build crates/jsc-wasm --target web`) when missing or stale.

**`apps/jsc-web`.** A browser playground that imports `compile`/`run` from
`@cdlab/jsc`.

### Workspace constraints

- **Toolchain.** Rust edition 2024, MSRV 1.99, pinned to `1.99.1` by
  `rust-toolchain.toml`.
- **Safety.** `unsafe_code = "forbid"` in `[workspace.lints.rust]` (relaxed to
  `deny` only in `jsc-wasm`).
- **Lints.** `warnings = deny` is inherited from `[workspace.lints]`, so
  `cargo clippy` needs no `-D`. `clippy::pedantic` warns; `unwrap_used`,
  `expect_used`, `panic`, and `dbg_macro` are denied — runtime paths never panic.

---

## 4. Value model & module ABI

### 4.1 The NaN-boxed `jsval`

Every JS value is one 64-bit word carried as a wasm **`i64`**. `QNAN =
0x7FF8_0000_0000_0000`. A word whose bits do not contain the full `QNAN` mask is an
ordinary IEEE-754 `f64` (a number). With the mask set, the 3-bit tag in bits
`[50:48]` discriminates:

| tag | meaning | encoding |
|---|---|---|
| `0b000` | the number NaN | exactly `0x7FF8_0000_0000_0000` (canonical) |
| `0b001` | boolean | `false = 0x7FF9_0000_0000_0000`, `true = 0x7FF9_0000_0000_0001` |
| `0b010` | `null` | `0x7FFA_0000_0000_0000` |
| `0b011` | `undefined` | `0x7FFB_0000_0000_0000` |
| `0b100` | unused | — |
| `0b101` | heap pointer | `0x7FFD_0000_0000_0000 \| ptr` (32-bit byte offset in the low bits) |
| `0b110` | unused | — |
| `0b111` | unused | — |

There is exactly **one** heap tag. The concrete heap kind (string, array,
object, function, …) is the **type byte** at offset 0 of the heap block (§5),
not the NaN-box tag.

**`HOLE`.** `abi::HOLE = UNDEFINED | 1` is an internal non-value that no JS
expression produces. It marks an array hole (§5.4) and a module cell still in
its TDZ (§7.1); every read path maps it to `undefined` or a `ReferenceError`
before it can escape.

**Why `i64`, not `f64`.** The wasm spec allows NaN payloads to be canonicalized
by `f64` arithmetic and at the host boundary, which would destroy tags. `i64`
moves are bit-preserving. Numeric operations reinterpret operands to `f64`
(`f64.reinterpret_i64`), compute with `f64.*`, and rebox the result.

**NaN canonicalization.** The number NaN is *always* exactly `QNAN`. Every site
that turns an `f64` back into a jsval — arithmetic, `Math.*`, a `Date` payload,
the WASI clock — calls the `__box_f64(x: f64) -> i64` helper
(`x == x ? reinterpret(x) : QNAN`), because wasm may produce a sign-bit or
payload-carrying NaN (`0/0` on x86, or a NaN operand such as `undefined`
propagating its tag bits). Integer-to-float conversions cannot produce NaN and
reinterpret directly. Consequently NaN tests compare bits directly, and a
computed NaN can never alias a boolean, `null`, `undefined`, or pointer.

**`typeof`.** `typeof` yields an interned heap string: the type index
(`number`, `boolean`, `undefined`, `object`, `string`, `function`, `symbol`,
`bigint` — `abi::TYPEOF_STRINGS`) selects the data-segment offset of the matching word,
so `typeof x === "number"`, concatenation, and `.length` all go through
ordinary string machinery. `TYPE_FUNCTION` blocks report `"function"`, strings
`"string"`, `TYPE_SYMBOL` blocks `"symbol"`, `TYPE_BIGINT` blocks `"bigint"`
(§6.10), every other heap kind `"object"`. `typeof` of an unresolvable
global name is `"undefined"` and never throws.

**Truthiness.** Falsy is exactly `false`, `null`, `undefined`, `±0`, `NaN`,
`0n`, and the empty string; every other value — including `[]`, `{}`, wrapper objects, and
every function — is truthy.

### 4.2 Module layout

**Types.** `_start` has type `() -> ()` and the internal `main` has type
`() -> i64` (it returns the completion value). Every user function (declarations,
expressions, arrows, methods, constructors, generator state machines), every
builtin body, and the two `new Promise` settlers share **one** uniform type:

```text
(a0: i64, …, a_{K-1}: i64, env: i64, argc: i64) -> i64
```

`K` is the program-wide positional arity, computed by `codegen/arity.rs` from
every function's parameter count and every call site's argument count
(including the extra `(element, index, array)` arguments the array
higher-order builtins pass their callbacks, and setters, which are called with
one argument by assignment); when builtins are reachable as values `K` also
covers the widest call (§6.4). `env` is the closure environment (`undefined`
for a non-capturing callee); `argc` is the actual argument count as a number
jsval, or the **construct marker** `-(n + 1)` when the callee runs under `new`
(§7.6). A single signature lets `call_indirect` serve every function.

**Function index space.**

| index | function |
|---|---|
| `0`–`3` | WASI imports `fd_write`, `clock_time_get`, `random_get`, `proc_exit` (§6.1) |
| `4` | `main` (not exported) |
| `5 … 4+n` | the `n` user functions |
| then | runtime helpers in a fixed order (§6.2): `__alloc` … `__to_number` |
| then | `__invoke1`, `__async_step`, the two `new Promise` settlers, `new_string`, `__new_array`, `__has_own` |
| then | the builtin bodies, one per `Builtin::ALL` entry (`emit/realm.rs`, 366 entries) |
| then | `__to_primitive`, `__to_string`, `__add`, `__compare`, `__array_join`, `__to_object`, `__json_rep`, `__json_indent`, `__json_revive`, and the rest of §6.2 up to the private-name helpers |
| then | the renderer helpers (§6.2), then `_start` |
| then | the Unicode helpers (`emit/unicode.rs`), the `Math` helpers (`emit/libm.rs`), the promise engine (`emit/promise_engine.rs`, §6.7), then the allocator helpers (`emit/gc.rs`, §5.6) |
| last | `__stack_overflow` (§6.6), `__map_hash`, `__map_reindex` (§5.2) |

The helper roster is `emit/registry.rs`: groups in this order, each helper one
`(member, signature, body)` row or a family module's `TYPES` and
`emit_helpers`; a helper's index is its group's base plus its position.

**Runtime shaking** (`emit/shake.rs`). Every module declares every runtime
function, so indices, types and the table never depend on the program; the
body of a function the program cannot reach is an `unreachable` stub. While a
body is emitted, the emitters record the static strings it looks up, the
builtins and intrinsics it names by address, table slot or call, and each
conversion of a run-time value to a property key. Kept, to a fixpoint: `main`,
the user functions, the settlers and `_start`; every function a kept body
calls; every builtin or callable intrinsic a kept body names; every builtin
installed in the realm (§6.4) under a string key a kept body looks up, under
a symbol key, as an accessor half, or under no key at all (none of which
counts for a binary-data builtin or entry in a program that cannot reach
binary data, §5.8.5); and the prototype chain of every kept intrinsic. The module is left whole when user code
converts a value that is not a literal or a plain numeric expression to a
property key (`o[k]`, a computed key, `k in o`, `delete o[k]`), or when a
kept runtime body returns or redefines a property its caller picks by a
run-time key (`shake::note_runtime_key`: `Object.getOwnPropertyDescriptor(s)`,
`__lookupGetter__` / `__lookupSetter__`, `Object.defineProperty`,
`defineProperties` and `create`, and `JSON.stringify` for its property
list): either can reach a builtin by a name no body mentions. `console.log('hi')` is 432 KB shaken and 890 KB whole. The data
segments are not shaken. `CompileOptions::full_runtime` turns the shake off.

**Table.** A funcref table of exactly `n + 2 + 366` entries (min = max), filled
by one active element segment: slots `0 … n-1` are the user functions, slots
`n` and `n + 1` the two settlers, then the builtin bodies in `Builtin::ALL`
order. A function value's `func_idx` is a slot in this table. Every module has
the table, including one without user functions.

**Memory.** One linear memory, `min` = the pages below the heap start,
`max = 8192` pages (512 MiB, `abi::MEMORY_MAX_PAGES`), exported as
`"memory"`.

**Globals.**

None is exported.

| index | name | type | role |
|---|---|---|---|
| 0 | `$__heap_ptr` | `i32` | page frontier: the end of the highest heap page obtained (§5.6) |
| 1 | `$exc_pending` | `i32` | `1` while an exception propagates |
| 2 | `$exc_value` | `i64` | the in-flight thrown jsval |
| 3 | `$cf_kind` | `i32` | pending `return`/`break`/`continue` routed through `finally` |
| 4 | `$cf_value` | `i64` | routed return value / labeled-loop target |
| 5 | `$__this` | `i64` | `this` carrier, initialized to `undefined` (`abi::THIS_GLOBAL_INDEX`) |
| 6–8 | `$__json_data`, `$__json_pos`, `$__json_len` | `i32` | `JSON.parse` cursor |
| 9–11 | `$__intern_base`, `$__intern_cap`, `$__intern_count` | `i32` | intern table location, slot count, occupancy (§5.3) |
| 12 | `$__new_target` | `i64` | `new.target` carrier, initialized to `undefined` |
| 13 | `$__spread_args` | `i64` | argument array of a spread call / construct, initialized to `undefined` |
| 14 | `$__intrinsics_dirty` | `i32` | one dirty bit per watched intrinsic (§6.4) |
| 15–17 | `$__out_buf`, `$__out_len`, `$__out_cap` | `i32` | output buffer: base, UTF-8 length, capacity (§6.9) |
| 18–19 | `$__path_buf`, `$__path_cap` | `i32` | renderer recursion path |
| 20–22 | `$__refs_buf`, `$__refs_cap`, `$__refs_len` | `i32` | renderer ref table |
| 23 | `$__sp` | `i32` | shadow-stack pointer (§5.7), initialized to `SS_SIZE - 8` |
| 24–25 | `$__rng0`, `$__rng1` | `i64` | `Math.random` xorshift128+ state, seeded on first use (§6.2) |
| 26–27 | `$__job_head`, `$__job_tail` | `i32` | promise job queue, raw refs (§6.7) |
| 28 | `$__promise_seq` | `i32` | promise creation counter (§6.7) |
| 29 | `$__rejected_head` | `i32` | rejected-promise list head, raw ref (§6.7) |
| 30 | `$__gc_debt` | `i32` | allocation budget left before a collection, initialized to `GC_MIN_BUDGET` (§5.6) |
| 31 | `$__gc_live` | `i32` | bytes surviving the last sweep (§5.6) |
| 32 | `$__gc_epoch` | `i32` | mark epoch, `1..=255`, initialized to `1` (§5.1) |
| 33 | `$__gc_busy` | `i32` | `1` while a collection runs |
| 34 | `$__gc_free_pages` | `i32` | pages in the free-page pool (§5.6) |
| 35 | `$__gc_large` | `i32` | head of the large-span list, raw ref (§5.6) |
| 36–38 | `$__gc_stack`, `$__gc_stack_top`, `$__gc_stack_end` | `i32` | the mark stack of the running collection: base, first free entry, end (§5.7) |
| 39 | `$__gc_overflow` | `i32` | set when a mark-stack push found no room (§5.7) |
| 40 | `$__gc_reserve` | `i32` | the out-of-memory reserve's first page, `0` while spent (§5.6) |
| 41 | `$__gc_weak` | `i32` | the running collection's `WeakMap` / `WeakSet` list, linked through `weak_next`, sentinel `1` = empty (§5.7) |
| 42 | `$__gc_weak_refs` | `i32` | the running collection's `WeakRef` list, same shape (§5.7) |
| 43 | `$__gc_weak_regs` | `i32` | the running collection's `FinalizationRegistry` list, same shape (§5.7) |
| 44 | `$__kept_objects` | `i32` | the `KeepDuringJob` set: raw ref to an internal array handle, `0` until the first push (§6.7) |
| 45 | `$__fin_jobs` | `i32` | pending finalization cleanup jobs: raw ref to a registry, linked through `job_next`, `0` = none (§6.7) |
| 46 | `$__call_depth` | `i32` | active user-function calls, counted by the call-stack guard (§6.6) |
| 47+ | module cells | `i64` | one cell pointer per module binding (`47 + slot`, §7.1), then one per tagged-template site (§7.4) |
| after the cells | `$__gc_stress_count` | `i32` | test-only `gc_stress` interval above `1`: collections run so far, saturating at `GC_STRESS_PREFIX` (§5.6); absent otherwise |

**Exports.** Exactly `_start: () -> ()` and `memory` (§6.8).

**`_start`.** The command entry point calls `main` and keeps its result. Unless
`main` left an exception pending, it drains the microtask queue (the guest
`__drain()`, §6.7). A pending exception is then reported as one
`Uncaught <value>` line on fd 2 and `proc_exit(1)`; otherwise the first
unhandled rejection (`__first_unhandled()`, the oldest by creation `seq`)
is reported as `Uncaught (in promise) <value>` the same way. On a clean finish, a module compiled with `CompileOptions::print_completion`
renders the completion value as one line on fd 1 (`jsc -p`, §9); the default
emit discards it. Finally `_start` flushes a line left unfinished on fd 1 by a
throwing `console.log` argument, and returns (exit code `0`).

**Shadow stack.** `[0, abi::SHADOW_STACK_SIZE)` (1 MiB, 16 pages) holds the
shadow stack, growing down from its end (§5.7). Its body is `[8, SS_SIZE)`:
`$__sp` starts at `SS_SIZE - 8`, the frame pointer `$fp = $__sp - 8N` is biased
by 8 (slot `k` at `$fp + 8 + 8k`, the first frame's top slot ending at
`SS_SIZE`), and addresses `0..8` never hold stack data, so a raw ref of `0`
always reads zero. A big frame (`N > 4`) is cleared by `memory.fill` from `$fp`
for `8N + 8` bytes, never from `$fp + 8`, which wraps to `0` at `$fp = -8`. The
collector (M2) scans `[$__sp + 8, SS_SIZE)`. The call-stack guard (§6.6)
keeps user recursion away from the floor; past it, overflow wraps below `0`
and the first slot store traps: at `$fp = -8` the static-offset address does
not wrap.

**Static region (data segments).** Everything from `SS_SIZE` to the heap
start is laid out by the compiler, in this order:

| range (from `SS_SIZE`) | contents |
|---|---|
| `0 … 7` | null guard word; the `u32` at `abi::FN_INFO_ADDR = SS_SIZE + 4` holds the base of the function info table |
| `8 … 47` | WASI scratch area (`abi::WASI_SCRATCH`): one iovec, `nwritten`, the clock result and the random seed buffer |
| `48 … 95` | the iovec (`abi::OOM_IOVEC`) and bytes of the static out-of-memory line (§5.6) |
| `96 … 103` | 8 zero bytes read as an empty one-byte string (`abi::OOM_EMPTY_STR`), the dummy result of a failed string allocation (§5.6) |
| `104 …` `abi::GC_PAGE_TABLE` | the number-conversion scratch and tables: `abi::DEC_SCRATCH`, the decimal parser tables, `abi::NUM_SCRATCH_ADDR`, `abi::RYU_TABLES_ADDR`, then 4 padding bytes: the allocator area's end is rounded up to `abi::BLOCK_ALIGN` with the pad placed before it, so `abi::GC_MARK_STACK`, `abi::STATIC_AREA_END` and every block after it are 8-aligned (const-asserted in `abi.rs`) |
| `abi::GC_PAGE_TABLE …` `abi::STATIC_AREA_END` | the allocator's static state (§5.6): the page table (one byte per page, 8 KiB), the per-class free-list heads and current pages (`u32[19]` each, zero), the class size table (`u32[19]`), the collector's static mark stack (`abi::GC_MARK_STACK`, 4 KiB, §5.7) |
| `abi::STATIC_AREA_END …` `abi::STATIC_AREA_END + libm::STATIC_SIZE` | the `Math` block (`emit/libm.rs`): the constant tables of `rem_pio2_large` and `atan`, then the working arrays of `rem_pio2_large` |
| from `HEAP_BASE = abi::STATIC_AREA_END + libm::STATIC_SIZE` | every distinct string literal and static key, one `TYPE_STRING` block each, 8-byte aligned, `flags = INTERNED` (plus `ARRAY_INDEX` for a canonical index spelling) |
| next | the function info table: one 8-byte entry `[name: u32 interned key][length: u32]` per funcref slot (user functions, the two settlers, the builtins) |
| next | one static 32-byte `TYPE_FUNCTION` block per user function (`LAZY`, no properties, `proto = %Function.prototype%`, `env = undefined`): the single function object of a top-level declaration |
| next | the realm image (§6.4) |
| next | the intern table: a power-of-two array of `u32` slots (§5.3) |
| next | the Unicode tables (case runs, special casing, `Cased` / `Case_Ignorable` / `Alphanumeric` ranges): one raw segment, never traced, generated into `emit/unicode_data.rs` by `tools/unicode-tables` |
| next (4-aligned) | the collector's static root list `[count: u32][addr: u32 × count]`: every static declaration block, then the realm image's intrinsic and builtin-method blocks (§5.7) |

The heap starts at the first 64 KiB page boundary after the static root list
(the initial `$__heap_ptr`). Identical literals share one
blob, so a literal costs no run-time allocation and its address is its
interned property key.

### 4.3 Calling convention

- **Direct calls.** A call to a statically known top-level function declaration
  emits `call` with its arguments, then `undefined` padding up to `K` slots, then
  `env = undefined`, then `argc`.
- **Indirect calls.** A call through a function *value* evaluates the callee
  once, checks that it is callable (a `TYPE_FUNCTION` block; otherwise it throws
  a `TypeError` naming the callee, `x is not a function`), pushes the arguments
  and padding, then reads the callee's `env` (`+FUNC_ENV_OFFSET = 24`) and
  `func_idx` (`+FUNC_INDEX_OFFSET = 16`) and issues `call_indirect`. `env` is
  read last so the callee data is read only after every argument (which may
  itself contain calls) has been evaluated.
- **Arity.** Calls are not arity-checked: missing arguments read as `undefined`,
  extra arguments are ignored, and `argc` carries the real count. A spread call
  (`f(...xs)`) fills the `K` positional slots from the flattened array and
  publishes the whole array in `$__spread_args`, so a callee that gathers its
  arguments (a rest parameter, `arguments`, a variadic builtin, a builtin
  constructor's argument such as the `Promise` executor) reads the elements
  beyond `K` from it. Every builtin body reads an argument past `K` from that
  array (`undefined` past `argc`), never as a silent `undefined`. A nested
  spread call publishes a fresh array, so a builtin that reads an argument
  past `K` after user code may run (a coercion, a getter, a callback,
  `Get(new.target, "prototype")`) first snapshots `$__spread_args` into a
  rooted local and reads that; when every argument it reads fits in `K` it
  emits no snapshot.
- **Builtin calls into user code.** A builtin calling a function value with
  more arguments than `K` (a `forEach` callback's `(value, key, map)`, a
  replacer's `(key, value)`, a `replace` callback's `(match, position,
  string)`, a constructor's executor under `K = 0`) never drops the surplus:
  it fills the `K` slots, passes the real `argc`, and publishes all the
  arguments in a fresh spread argument array, exactly like a spread call. When
  that allocation runs out of memory the callee is not called and the
  `RangeError` propagates. The same holds for a promise reaction or a
  `FinalizationRegistry` cleanup callback under `K = 0` (`__invoke1` passes
  its one argument in a one-element spread array), for a one-argument
  accessor-style call under `K = 0` (a setter, `@@toPrimitive` with its hint,
  `@@hasInstance`: the same one-element array; on OOM the call yields
  `undefined` with the `RangeError` pending), for an inline array
  higher-order method whose callback may gather more than `K` arguments (the
  shared spill array is allocated, and checked, before the first callback),
  and for an IR call node a builtin builds with more literal arguments than
  `K`: it is emitted as the equivalent spread call (a direct call, an indirect
  call, a method call, or a `new` /
  `super(..)`), a compile-time choice that leaves every call fitting in `K` —
  every source call — unchanged.
- **`this`.** `this` rides the `$__this` global rather than a parameter. A method
  call `obj.m(args)` evaluates `obj` and the arguments, then sets
  `$__this = obj` immediately before `call_indirect`; a plain call resets it to
  `undefined`. A function that reads `this` snapshots `$__this` into a local as
  the first statement of its prologue, so nested calls cannot clobber it.
  Optional calls (`o?.m()`) bind `this` like plain method calls.
- **Construct.** `new X(args)` sets `$__this` to the new object and
  `$__new_target = X`, and passes the construct marker `argc = -(n + 1)`
  (§7.6). Only functions that read `argc`, `arguments`, or `new.target` decode
  it.
- **Exceptions.** After every call into user code the caller checks
  `$exc_pending` and branches to its nearest handler or returns early (§6.6).

---

## 5. Heap & object model

### 5.1 Header, alignment, object-family prefix

Every heap block starts with a 4-byte header (`HEADER_SIZE = 4`):

| byte | field | values |
|---|---|---|
| 0 | `kind` | the type byte (§5.2) |
| 1 | `gc` | mark epoch (§5.7): the block is marked iff `gc == $__gc_epoch`; `0` on a fresh block and on every static blob |
| 2 | `flags` | per kind, `0` = default (below) |
| 3 | `class` | `TYPE_OBJECT` sub-class (§5.2); `0` for every other kind |

Every block base is 8-byte aligned (`BLOCK_ALIGN = 8`): heap blocks sit on
16-byte slot or span boundaries (§5.6) and static blobs are padded, so `i64`
slots at block offsets ≡ 0 (mod 8) are naturally aligned. Multi-byte fields are little-endian.

The **object family** — `TYPE_ARRAY`, `TYPE_OBJECT`, `TYPE_FUNCTION`,
`TYPE_MAP`, `TYPE_SET`, `TYPE_PROMISE`, `TYPE_GENERATOR`, `TYPE_DATE`,
`TYPE_WEAKMAP`, `TYPE_WEAKSET`, `TYPE_WEAKREF`, `TYPE_FINREG` — shares
a 16-byte prefix (`OBJ_PREFIX_SIZE = 16`):

| offset | size | field |
|---|---|---|
| 0 | 4 | header |
| 4 | 4 | `props: u32` (`OBJ_PROPS_OFFSET`) → the block's `TYPE_PROPS` table, `0` = no own named properties |
| 8 | 8 | `proto: i64` (`OBJ_PROTO_OFFSET`) — a heap pointer or `null` |
| 16 | … | kind payload |

**Flags.** Strings: bit `1` `INTERNED` (the canonical block for its content),
bit `2` `ARRAY_INDEX` (content is a canonical array index: `"0"` or a nonzero
digit then digits, value `< 2^32 - 1`), bit `4` `TWO_BYTE` (the data is
UTF-16LE code units; same bit value as `SYMBOL_FLAG_REGISTERED`, read only on
`TYPE_STRING` blocks). Object family: bit `1`
`NOT_EXTENSIBLE` (set by `preventExtensions` / `seal` / `freeze`, never
cleared; only on a `DICTIONARY` array). Functions additionally: `2` `ASYNC`,
`4` `LAZY`, `8` `CONSTRUCTOR`, `16` `CLASS_CTOR`, `32` `SELF_ALLOC`, `64`
`BOUND`, `128` `GENERATOR` (§5.5).
Arrays additionally: `4` `SPARSE`, `8` `JOINING` (set while `join` runs, so a
cyclic element joins as `""`), `16` `LENGTH_READONLY` (`length` is
non-writable; only on a `DICTIONARY` array), `32` `DICTIONARY` (§5.4). None of
the array bits is ever cleared (except `JOINING`).
Proxies (`TYPE_PROXY`, phase 5b, §6.11) keep the prefix shape but
are not in the object family; their flags reuse `CONSTRUCTOR` (`8`) and
`SELF_ALLOC` (`32`), and header byte 3 holds `1` `PROXY_CLASS_CALLABLE`.

### 5.2 Block layouts

| type byte | kind | layout (offsets from the block base) | size | `typeof` |
|---|---|---|---|---|
| `0` `TYPE_STRING` | string | header, `len: u32 @4` (`STR_LEN_OFFSET`, UTF-16 code units), data `@8` (`STR_DATA_OFFSET`): Latin-1 bytes, or UTF-16LE units with `TWO_BYTE` | `8 + (len << two_byte)`, rounded | `string` |
| `1` `TYPE_ARRAY` | array handle | prefix, `len: u32 @16`, `elems: u32 @20` → `TYPE_ELEMS` (`0` while capacity is 0) | 24 | `object` |
| `2` `TYPE_OBJECT` | object | prefix; wrapper classes add `[[PrimitiveValue]]: i64 @16` (`WRAPPER_VALUE_OFFSET`) | 16 / 24 | `object` |
| `3` `TYPE_FUNCTION` | function | prefix, `func_idx: u32 @16`, reserved `u32 @20`, `env: i64 @24` | 32 | `function` |
| `4` `TYPE_ENV` | closure environment (internal) | header, `count: u32 @4`, `cell: i64 × count @8` | `8 + 8·count` | — |
| `5` `TYPE_CELL` | boxed binding (internal) | header, `value: i64 @4` | 12 | — |
| `6` `TYPE_MAP` | `Map` | prefix, `size: u32 @16`, `index: u32 @20` → hash index (`0` = none), `entries: i64 @24` → backing array | 32 | `object` |
| `7` `TYPE_SET` | `Set` | same as `TYPE_MAP` | 32 | `object` |
| `8` `TYPE_PROMISE` | promise | prefix, `state: i32 @16`, `reactions: u32 @20`, `value: i64 @24`, `seq: u32 @32`, `rejected_next: u32 @36` | 40 | `object` |
| `9` `TYPE_GENERATOR` | generator / async generator | prefix, `sm_idx: u32 @16`, `state: i32 @20`, `done: i32 @24`, `limits: u32 @28`, `env: i64 @32`, `queue: u32 @40`, `queue_tail: u32 @44` | 48 | `object` |
| `10` `TYPE_DATE` | `Date` | prefix, `ms: f64 @16` (the `TimeClip`ped time value; local time is UTC, `LocalTZA = 0`) | 24 | `object` |
| `11` `TYPE_ACCESSOR` | accessor pair (internal) | header, pad, `getter: i64 @8`, `setter: i64 @16` | 24 | — |
| `12` `TYPE_PROPS` | own-property table (internal) | §5.3 | `16 + 16·cap` | — |
| `13` `TYPE_ELEMS` | array element store (internal) | header (flags: `1` `ELEMS_FLAG_WEAK`, the entry store of a weak collection or registry), `cap: u32 @4`, `elem: i64 × cap @8` | `8 + 8·cap` | — |
| `15` `TYPE_WEAKMAP` | `WeakMap` | prefix, `weak_next: u32 @16` (collector scratch, `0` outside a collection), `entries: u32 @20` → weak `TYPE_ELEMS` of `(key, value)` pairs (`0` until the first entry), `live: u32 @24`, pad | 32 | `object` |
| `16` `TYPE_WEAKSET` | `WeakSet` | same as `TYPE_WEAKMAP`; the store holds keys alone | 32 | `object` |
| `17` `TYPE_WEAKREF` | `WeakRef` | prefix, `weak_next: u32 @16`, pad, `target: i64 @24` (weak; `undefined` once collected) | 32 | `object` |
| `18` `TYPE_FINREG` | `FinalizationRegistry` | prefix, `weak_next: u32 @16`, `cells: u32 @20` → weak `TYPE_ELEMS` of `(target, held, token)` triples, `cleanup: i64 @24`, `flags: u32 @32` (`1` `FINREG_FLAG_JOB_QUEUED`), `job_next: u32 @36` (the `$__fin_jobs` link) | 40 | `object` |
| `19` `TYPE_RAW` | internal raw buffer | header, `len: u32 @4`, `len` bytes from `@8`, no references (the grown intern table, `Math.sumPrecise` accumulator) | 8 + `len` | — |
| `20` `TYPE_PROXY` | Proxy exotic object (phase 5b, §6.11) | header (flags `8` `CONSTRUCTOR`, `32` `SELF_ALLOC`; byte 3 `1` `PROXY_CLASS_CALLABLE`), `props: u32 @4` (private entries only), `proto: i64 @8` (always `null`), `func_idx: u32 @16` (the `ProxyCall` slot when callable), reserved `u32 @20`, `env: i64 @24` (itself when callable), `target: i64 @32`, `handler: i64 @40` (both `null` once revoked) | 48 | `function` when callable, else `object` |
| `14` `TYPE_SYMBOL` | Symbol primitive | header (flags: `4` `SYMBOL_FLAG_REGISTERED`, `8` `SYMBOL_FLAG_PRIVATE` (a private name, §7.6, phase 5a); bits `1`/`2` always clear), reserved `u32 @4` = 0, `description: i64 @8` (string or `undefined`) | 16 | `symbol` |
| `21` `TYPE_BIGINT` | BigInt primitive (§6.10) | header (flags: `1` `BIGINT_FLAG_NEG`), `len: u32 @4` (limb count), `u32` limbs × `len` @8, little-endian, canonical | `8 + 4·len`, rounded | `bigint` |

**`TYPE_OBJECT` classes** (header byte 3): `0` `CLASS_ORDINARY`, `1`
`CLASS_ERROR` (an Error instance: its kind is its prototype, §7.7), `2`
`CLASS_ARGUMENTS` (an unmapped `arguments` object: own `"0".."n-1"`, a
non-enumerable `length` and a non-enumerable `@@iterator` =
`%Array.prototype.values%`), `3` / `4` / `5` `CLASS_BOOLEAN` / `CLASS_NUMBER` /
`CLASS_STRING` wrappers (24 bytes, `[[PrimitiveValue]]` at
`WRAPPER_VALUE_OFFSET = 16`; a String wrapper owns its `length` as a
read-only property and its indices virtually, §5.3), `6` `CLASS_GLOBAL` (the global object, a static realm
block), `7` `CLASS_SYMBOL` (a Symbol wrapper, `[[SymbolData]]` at
`WRAPPER_VALUE_OFFSET`), `8` `CLASS_ITERATOR` (an Array / String / Map / Set
iterator, 32 bytes: `[[IteratedObject]]` at `16` (`undefined` once
exhausted), `[[NextIndex]]: u32` at `24` (an array index, a string
code-unit index, a Map/Set backing slot index), `kind: u8` at `28`: `0`..`2` array
keys / values / entries, `3` string, `4`..`6` map keys / values / entries,
`7` / `8` set values / entries; `16` an `Iterator.from` wrapper (48 bytes:
the wrapped iterator at `32`, its `next` at `40`); `17`..`21` a `map` /
`filter` / `take` / `drop` / `flatMap` helper (80 bytes: `[[NextIndex]]`
holds the generator state `0` suspended-start, `1` suspended-yield, `2`
running, `3` completed; the underlying iterator at `32`, its `next` at
`40`, the counter or remaining limit (`f64`) at `48`, the callback at `56`,
a `flatMap`'s inner iterator at `64` (`undefined` while none is open) and
its `next` at `72`); no host reads the payload), `9` `CLASS_BIGINT` (a BigInt
wrapper, created only by `ToObject`, `[[BigIntData]]` at
`WRAPPER_VALUE_OFFSET`).

Notes per kind:

- **Strings** are primitives with no prefix and no own table; `len` and
  `.length` count UTF-16 code units. The form is canonical: a string is
  one-byte (Latin-1) iff every unit is `<= 0xFF`, so every producer narrows
  (literals, slices, `JSON.parse`, builtin results), strings of different width
  are never equal, a two-byte string is never an array index, and the intern
  table never holds two forms of one content. Strings are immutable; slices
  copy (no views, no ropes).
- **Map / Set** handles point (`entries`) at an ordinary array holding
  `(key, value)` pairs flattened into even/odd slots (a Set's value slot is
  `undefined`). The backing grows in place like any array, so the handle never
  moves; `delete` tombstones the pair in place (`HOLE` key, `undefined`
  value) and `clear` tombstones every pair, so live iterators and `forEach`
  keep their slot index and see entries appended later (including after a
  `clear`); a re-added key appends. The backing is never compacted or
  shrunk, not even by the collector (a running `forEach` holds a slot index
  in a wasm local; §13). `size` is
  the live entry
  count. A backing shorter than 16 slots is scanned with
  `__same_value_zero`, skipping tombstones. From 16 slots on, the handle's
  `index` word points at a hash index: a `TYPE_RAW` block
  `[used: u32 @8][cap: u32 @12][slot: u32 x cap @16]`, open addressing with
  linear probing over `__map_hash(key)` (a string's content hash, else the
  value bits with `-0` as `+0`), each slot a backing key slot index plus one
  (`0` = empty). `__map_set` enters an appended key while the index stays at
  most half full; otherwise `__map_reindex` builds a fresh one of at least
  four slots per live entry from the backing. A deleted entry keeps its index
  slot (its `HOLE` key matches nothing) until the next rebuild. The renderer
  skips tombstones too.
- **WeakMap / WeakSet** handles (`emit/builtins/weak.rs`) point (`entries`)
  at a `TYPE_ELEMS` store flagged `ELEMS_FLAG_WEAK` holding `(key, value)`
  pairs (a `WeakSet`: keys alone), allocated with 4 entries by the first
  `set` / `add`. Every slot past the live entries holds a `HOLE` key (and an
  `undefined` value). Lookup is a linear identity scan of the whole store
  (keys are objects or non-registered symbols, so identity is SameValue);
  `delete` tombstones the entry in place, `set` / `add` reuse the first
  `HOLE` or double the store. `live` counts the non-`HOLE` entries. The
  collector compacts a store in place (§5.7); nothing iterates one, so no
  slot index can go stale.
- **WeakRef** blocks hold the target in one weak slot; the collector writes
  `undefined` there once the target dies (§5.7).
- **FinalizationRegistry** blocks keep `(target, held, token)` cells (24
  bytes) in a weak `TYPE_ELEMS` store of the same shape: an unused cell has an
  `undefined` target, an emptied cell (target collected, cleanup pending) a
  `HOLE` target. `register` reuses the first unused cell or doubles the store,
  `unregister(token)` turns every cell of `token` unused. `cleanup` is the
  callback; `flags` / `job_next` queue the registry's cleanup job (§6.7).
- **Promise** blocks hold the settlement (`state` ∈ pending `0` / fulfilled
  `1` / rejected `2`, plus the settled `value`), a raw ref to the newest
  reaction record (`reactions`, `0` = none), the creation number `seq` and
  the rejected-list link `rejected_next`. Header flags (byte 2): `2`
  `HANDLED` (a reaction was ever registered), `4` `IN_REJECTED_LIST` (§6.7).
- **Generator** handles carry the state-machine table slot, the resume state,
  the `done` flag, the guest-only `limits` bits (`4` the state machine is
  running, so re-entering it throws; `8` an async generator; `16` its last
  suspend was an `await`; `32` it is executing or awaiting, so queued requests
  wait; `64` the body has a `yield*`, set at construction; `128` the last
  suspend was a `yield*` step returning the delegate's result object, which
  the resumer returns as-is and clears; bits `8..=9` the resumption mode —
  `1` throw, `2` return — of a resume entered with the value pending, written
  by `throw()` / `return()`), the frame (a `TYPE_ENV` of cells holding every generator local), and
  an async generator's request queue (§7.8): a linked list of internal
  `TYPE_ENV` records `[value, promise, kind, next]`.
- **Accessor** blocks are the value of an `ACCESSOR` property-table entry; they
  are consumed by `__get`/`__set` and never observed as values (the renderer
  prints `[Getter]`, `[Setter]`, `[Getter/Setter]`).

**Initial prototypes.** Ordinary objects are created with
`%Object.prototype%`, functions with `%Function.prototype%`, arrays, Map, Set,
promises and dates with their realm prototype, generator and async generator
objects with their function's `prototype` (`%GeneratorPrototype%` /
`%AsyncGeneratorPrototype%` when that is not an object), class instances with
`C.prototype`, wrappers with `%Boolean/Number/String.prototype%`.

### 5.3 Property tables and interning

An object-family block's own named properties live in a `TYPE_PROPS` table:

| offset | size | field |
|---|---|---|
| 0 | 1 | kind `12` |
| 2 | 1 | `tflags`: `1` `HAS_INDEX_KEYS`, `2` `HAS_NON_ENUM`, `4` `WATCHED`, `8` `HAS_SYMBOL_KEYS` (some key is a `TYPE_SYMBOL` block) |
| 3 | 1 | dirty-bit index of a watched intrinsic's table |
| 4 | 4 | `count: u32` — slots used, tombstones included |
| 8 | 4 | `cap: u32` |
| 12 | 4 | `live: u32` — non-tombstone entries |
| `16 + 16·k` | 4 | `key: u32` → interned string block or `TYPE_SYMBOL` block; `0` = tombstone |
| `20 + 16·k` | 4 | `attrs: u32` |
| `24 + 16·k` | 8 | `value: i64` — a data value, or a `TYPE_ACCESSOR` pointer when `ACCESSOR` |

`attrs`: `1` `WRITABLE`, `2` `ENUMERABLE`, `4` `CONFIGURABLE`, `8` `ACCESSOR`
(for which `WRITABLE` is ignored). Properties created by assignment or
literals are `0b0111` (`ATTRS_DEFAULT`). This is the state space of a spec
property descriptor: descriptor reflection (§6.3) reads and rewrites `attrs`
and `value` in place.

- **Keys** are interned: two keys are equal iff their `u32` pointers are
  equal, so a lookup is one load and compare per entry (linear scan).
- **Insertion order** is slot order. **Delete** writes `key = 0` and value
  `undefined` and decrements `live`; later slots keep their index.
- **Growth** (`__define`): a missing table starts at capacity 4; a full one is
  rebuilt at `max(4, 2·live)` (the same capacity when tombstones free half,
  unless it is `0`: an empty static realm table), copying live entries in order and keeping the watch flag and bit, and the
  owner's `props` slot is rewritten. The owner handle never moves. Literals and
  class instances reserve their static key count up front. A static realm
  table that overflows is rebuilt on the heap the same way.

**Interning.** The intern table is open-addressed over `u32` slots (FNV-1a
over the code units — one round per unit, zero-extended, so an ASCII key
hashes as its bytes — linear probing, load ≤ 1/2), located by globals 9–11. The compiler
emits the initial table containing every static string. `__intern(s)` returns
the canonical block for `s`'s content, inserting `s` (and flagging it) on a
miss and doubling the table into a heap copy once more than half full;
`__intern_find(s)` returns `0` on a miss, which means "absent everywhere".
The table is **weak** (§5.7): after marking, the collector turns the slot of
every unmarked heap string into the tombstone `1`; the probe passes over a
tombstone, an insertion never reuses one, `$__intern_count` keeps counting
them until the next growth, and growth moves only the strings (slots `> 1`)
and restarts the count from them. A key nobody references is therefore
freed, and interning the same content later yields a new canonical block.
Literal-key sites pass the constant key address; computed keys go through
`__to_key(v, insert)` (`ToPropertyKey`: an interned string is its own key, a
symbol is its own key — its block address, never interned —, an object is
converted with `ToPrimitive` (hint string; a `Symbol` wrapper yields its
symbol) first, an integer `0 ≤ v < 2^32 - 1` is formatted in the guest,
anything else goes through `__to_string`). `__define` sets the table's
`HAS_SYMBOL_KEYS` when it appends a symbol key. Lookups use `insert = 0` but still intern a canonical
array index, so an element is found by an index key no property used.

**String exotic indices.** A `CLASS_STRING` wrapper has no per-index table
entries: `__str_exotic_index(o, key)` returns the code-unit index when `o` is
a String wrapper and `key` a canonical array index below its
`[[PrimitiveValue]]`'s length, else `-1`. `__get`, `__has`, `__set`,
`__delete`, `__own_desc`, `__own_enum`, `__define_own` and `__own_keys`
consult it before the table, treating each index as a virtual own data
property `{value: s[i], writable: false, enumerable: true, configurable:
false}`; `__own_keys` lists `0..len` before the table's keys.

### 5.4 Arrays

An array is a 24-byte handle plus a separately allocated `TYPE_ELEMS` store.
The handle never moves: growth replaces the store and rewrites `elems`, so
aliases, captured bindings, and member receivers all observe `push`.

- **Elements.** Element `i < len` is the jsval at
  `elems + ELEMS_DATA_OFFSET + 8·i` (`ELEMS_DATA_OFFSET = 8`); slots past
  `len` are unused.
- **Holes.** A hole slot holds `HOLE`. Literal elisions, `delete a[i]`, gaps
  left by growth, and truncation produce holes; `pop` writes `HOLE` into the
  slot it vacates, so a store never keeps a removed element alive. A hole is not an own
  property: `hasOwnProperty` and own-key enumeration skip it and the
  renderer prints `<N empty items>`, while `[[Get]]` and `HasProperty` (and
  so `in`, member reads, spread, destructuring, `for-of`, `JSON.stringify`,
  and every `Array.prototype` method) continue along the prototype chain
  exactly as for an index at or past `length`. Every element read that meets
  a hole calls `__proto_elem(o, i)`: `HOLE` at once when
  `__protos_indexless(o)` (no prototype is an array outside dictionary mode
  or has a table flagged `HAS_INDEX_KEYS`; a dictionary-mode array such as
  `%Array.prototype%` keeps every element in its table, so the flag covers
  it), else `HasProperty` then `Get` of the index key with
  `this = o`. A dense read pays nothing; a hole read pays one call.
- **Element writes.** A store to an index the array does not own (a hole, or
  at or past `length`) is `OrdinarySet` through the prototype chain:
  `__proto_set(o, i, v)` answers at once under `__protos_indexless(o)`, else
  the first prototype owning `i` (an array's present store element, or a
  table entry) decides — an accessor calls its setter with `this = o` and
  creates nothing (a getter-only one throws), a read-only data property
  throws `TypeError` (all code is strict) — and a writable data property or
  no owner leaves the store to create the own element (below). The inline
  `a[i] = v` stores in place only into an in-bounds slot that is not a hole;
  every other element store is `__array_set` (and a `SPARSE` store-slot hole
  `__sparse_set`), which runs that check, so every `Array.prototype` method
  writing by `Set` (`push`, `unshift`, `splice`, `fill`, `copyWithin`,
  `reverse`, the `sort` write-back) sees inherited setters and read-only
  indices. The in-place appends of `push` (its guarded fast path and the
  builtin's array branch) run only while no prototype can hold an index.
  `CreateDataPropertyOrThrow` never consults the chain: the result arrays of
  `map`, `filter`, `concat`, `slice`, `splice`, `flat`, `flatMap`,
  `Array.of` / `from` and the change-by-copy methods, and `defineProperty` of
  a default-attribute element, store through `__array_define` /
  `__sparse_define` (the same element paths without `__proto_set`).
- **Growth.** `__array_reserve(base, need)` (behind `push` and every bulk
  append) grows the store to `max(need, 4, 2·cap)` slots and copies the `len`
  live ones. A store
  `a[i] = v` with `i ≥ len` stays dense while `i < len + 1024` (`DENSE_GAP`;
  store capacity `max(4, 2·cap, i + 1)`, gap filled with holes,
  `len = i + 1`). The bound is a gap, not a multiple of the capacity: each
  store adds at most 1023 hole slots, so stores that keep doubling the index
  (`x[2^k - 2]`) go sparse instead of doubling the store until memory runs
  out.
- **Sparse arrays.** A larger gap (a store or a `length` growth to
  `i ≥ len + 1024`) sets `SPARSE` (never cleared): the store is
  kept for indices `< cap`, and every element at or past `cap` is an
  array-index key in the handle's own property table. `length` may then exceed
  the store; every element operation on a sparse array goes through
  `__sparse_get` / `__sparse_set`.
- **Dictionary mode.** Before an element takes non-default attributes
  (`defineProperty` with `writable`/`enumerable`/`configurable` false or an
  accessor), before the array becomes non-extensible (`preventExtensions` /
  `seal` / `freeze`), and before `length` becomes read-only, the whole array
  turns `SPARSE | DICTIONARY`: every present store element moves into the own
  table under its index key with the default attributes and the store is
  dropped (`elems = 0`). Every element is then a table entry carrying its own
  attributes, reached through the sparse paths: `__sparse_get` calls an
  accessor's getter, `__sparse_set` is `__set` of the index key (read-only,
  setter, and non-extensible rules) and throws `Cannot add property i, object
  is not extensible` when it would grow a read-only `length`; `__delete`
  honours `configurable`. The builtin fast paths (the `ProtoGuard` receiver
  test, the `Array.prototype` bodies' array branches, the slice copy) skip a
  dictionary array, which takes the generic per-index path; `indexOf` /
  `includes` instead scan a `SPARSE` or dictionary array's own index keys
  when its table holds no accessor element and its prototypes are indexless
  (an accessor array keeps the per-index path, so getters run in index
  order). The mode is never left.
- **`length`** is an own, non-enumerable, non-configurable exotic property,
  writable unless `LENGTH_READONLY`. Writes go through `__array_set_length`
  (`ArraySetLength`): a read-only `length` throws `Cannot assign to read only
  property 'length'`; `ToUint32(v)` must equal `ToNumber(v)`, else
  `RangeError: Invalid array length`; shrinking turns `n..len` into holes
  (keeping the store) and drops a sparse array's index keys `≥ n`, stopping at
  the highest non-configurable element `k ≥ n` (then `length = k + 1` and
  `Cannot delete property 'k'` is thrown); growing fills holes densely within
  the dense bound, else makes the array sparse without allocating the gap.
  `defineProperty(a, "length", d)` first converts a `value` (`ToUint32` and
  `ToNumber`, two conversions; an abrupt one propagates and a mismatch is the
  `RangeError`, even on a read-only `length`), then rejects
  `configurable`/`enumerable` `true` and accessor fields; on a read-only
  `length` it rejects `writable: true` and
  any value but the current one; otherwise a value is written as above and
  `writable: false` then sets `LENGTH_READONLY` (even when the shrink stopped
  early).
- **Keys.** Number keys and canonical index strings take the element path;
  every other key (`a.foo`, `a["-1"]`) is an ordinary named property in the
  handle's table.

### 5.5 Functions

A function value is a `TYPE_FUNCTION` block whose `func_idx` is its funcref
slot and whose `env` is its closure environment. Closures are allocated at
their definition site; a top-level function declaration is its static block
in the data segment (§4.2), so it has one identity for the whole run.

- **Lazy properties.** Every function is created `LAZY` with `props = 0`. The
  first property operation of any kind calls `__fn_materialize`, which clears
  the flag and defines `length` and `name` (configurable only) from the
  function's info entry (the `u32` at `FN_INFO_ADDR` locates the table; entry
  `func_idx` is `[name key][length]`) and, for a plain `CONSTRUCTOR` (not a
  class), `prototype` (writable only) = a fresh object whose non-enumerable
  `constructor` is the function; for a `GENERATOR`, `prototype` (writable
  only) = a fresh object inheriting `%GeneratorPrototype%`
  (`%AsyncGeneratorPrototype%` when also `ASYNC`), with no `constructor`.
- **Kinds.** `CONSTRUCTOR` is set for `function` declarations and expressions
  and classes; arrows, methods, accessors, generator and async functions are
  not constructors. `GENERATOR` marks a generator function or method, with
  `ASYNC` an async generator one (they get the `prototype` above); the others
  get no `prototype`. A generator function's own `[[Prototype]]` is
  `%Function.prototype%`: `%GeneratorFunction.prototype%` and
  `%AsyncGeneratorFunction.prototype%` are not modeled. `CLASS_CTOR` marks a class constructor
  (callable only with the construct marker; its `prototype` is installed by
  the class definition). `SELF_ALLOC` marks a derived class and the builtin
  constructors that allocate their own object from `new.target` (Array, the
  Error family, Map, Set, Promise, Date, the weak collections, the binary-data
  constructors, and the `String` / `Number` / `Boolean` wrappers).
- **No own `caller` / `arguments`.** Every function is strict; both names
  resolve to the `%ThrowTypeError%` accessors on `%Function.prototype%`.
- **Bound functions.** `Function.prototype.bind` allocates a `BOUND`
  `TYPE_FUNCTION` block (never `LAZY`) whose `func_idx` is the shared
  `BoundCall` builtin trampoline and whose `env` is an internal array
  `[target, boundThis, ...boundArgs]`; its `proto` is the target's. It is
  `CONSTRUCTOR | SELF_ALLOC` iff the target is a constructor, and owns only
  `length` (`max(0, ToIntegerOrInfinity(target.length) - boundArgs)` when the
  target has an own number `length`, else `0`) and `name` (`"bound " +
  target.name`, `"bound "` for a non-string), both configurable only. The
  trampoline prepends the bound arguments to the call's (into one fresh array,
  called like a spread call); a call runs the target with `this = boundThis`,
  a construct constructs the target with `new.target` = the target when it was
  the bound function itself (recognised by its `env`), else the incoming
  `new.target`.

### 5.6 Allocator, pages and memory growth

The heap (`emit/gc.rs`, `emit/runtime.rs::emit_alloc`) is a run of 64 KiB
pages from the first page boundary after the static region up to the
**frontier** `$__heap_ptr`. Every block comes back zeroed, header included, so
callers write only the fields they set and a fresh block's `gc` byte is `0`.

- **Page table** (`abi::GC_PAGE_TABLE`, one byte per wasm page, 8 KiB,
  indexed by `address >> 16`): `0` not heap (the shadow stack, the static
  region, pages above the frontier), `1..=19` a small page of that class,
  `0xFE` a large-span head, `0xFD` a large-span tail (and any page handed out
  by `__gc_take_pages` before its caller re-codes it), `0xFC` a page in the
  free-page pool.
- **Size classes** (19): `16..=256` by 16 (classes `1..=16`), then `512`,
  `1024`, `2048` (classes `17..=19`); a size maps to `(size + 15) >> 4` when
  `<= 256`, else `17 + ceil_log2(size) - 9` (`abi::gc_size_class`). The slot
  sizes are a static `u32[19]` (`abi::GC_CLASS_SIZES`).
- **Small page**: a 16-byte header (`class`, `bump` = the first slot never
  allocated, `live` = slots that survived the last sweep, reserved), then
  slots of the class size from `page + 16`. Each class has one **current
  page** (`abi::GC_CUR_PAGES`, `u32[19]`) it bump-allocates from.
- **Free slot**: `[0xFF TYPE_FREE][0][0][0][next_free: u32 @4]`, on a
  per-class list headed in the static `u32[19]` at `abi::GC_FREE_HEADS`.
- **Large span**: a request above 2048 bytes takes `k = ceil((size + 16) /
  64 KiB)` contiguous pages with a 16-byte pre-header (`pages`,
  `next_large`), the block at `page + 16`; spans are linked from
  `$__gc_large`.

**`__alloc_small(class)`**, the per-class entry: constant-size sites call it
with the class computed at compile time (`gc::emit_alloc_const`). In order:

1. Budget (`gc::emit_gc_budget`): `$__gc_debt -= size(class)`; when it goes
   negative outside a collection (`$__gc_busy`), `__gc_collect` runs (§5.7),
   which renews the budget to `max(GC_MIN_BUDGET, $__gc_live)`
   (`GC_MIN_BUDGET = 4 MiB`, so the heap stays near twice the live size).
2. Pop the class free list and `memory.fill` the slot with zeros.
3. Bump the class's current page while the slot fits.
4. Take one page from `__gc_take_pages(1)`, code it with the class, and make
   it the current page.
5. Collect before running out: when no page could be taken and this request
   has not collected yet, `__gc_collect` runs once and the request retries
   from step 2.
6. Out of memory (below): from the reserve, else the fatal exit.

**`__alloc(size)`**, for variable sizes, computes the class and calls
`__alloc_small`, or for a large size runs the budget step (`k` pages) and
takes a span from `__gc_take_pages(k)`, with the same single
collect-and-retry when the take fails; then it returns `0` (below).
**`__alloc_fixed(size)`** is `__alloc` for callers that write the whole block
unconditionally — the constant-size sites above 2048 bytes
(`gc::emit_alloc_const`), the render buffers (§6.9), and `__new_array`'s
element stores of up to 32 765 slots — and never returns `0`: a span that
fits the reserve comes from it like a small request.

**`gc_stress`** (`CompileOptions::gc_stress: u32`, test only; the CLI does
not expose it) is a collection interval in allocations: under any nonzero
value the budget step subtracts `1` per allocation instead of the bytes, and
the budget starts at `0`. At `1` every renewal sets it to `0`, so every
allocation collects. At `N > 1` the first `GC_STRESS_PREFIX` (30 000)
collections, counted in `$__gc_stress_count` (an i32 global after the module
cells, emitted only then), renew it to `0`, so a run collects at every
allocation until then and at every `N`th allocation afterwards. At `0` (every
non-test build) none of this is emitted: the budget counts bytes and the
module has no extra global.

**`__gc_take_pages(k) -> base | 0`** never calls `__alloc`: with at least `k`
pages in the pool (`$__gc_free_pages`) it takes the first run of `k` free
pages in the page table and zeroes them; otherwise it advances the frontier,
growing memory by `max(need, memory.size)` pages (at least doubling) and, if
that fails, by exactly `need`. Pages above the frontier were never written,
so they are already zero. It returns `0` when memory cannot supply the
pages. `__gc_release_pages(base, k)` returns pages to the pool.

**`__gc_sweep()`** (the last step of a collection, §5.7) clears
the class lists, walks every small page from its first slot to `bump`, keeps
the slots whose `gc` byte is `$__gc_epoch`, rewrites every other one as a
free slot on a page-local list, then returns a page with no survivor to the
pool (dropping it as its class's current page) or splices its list onto the
class list and stores its `live`; unmarked large spans are unlinked and
released; `$__gc_live` becomes the surviving bytes. Blocks in the static
region are on no heap page and are never swept.

**Out of memory** (design/runtime-memory.md §5.8) is a catchable
`RangeError`: every one throws the same static `%OutOfMemoryError%` (a realm
intrinsic of class `CLASS_ERROR`, `proto` `%RangeError.prototype%`, own
non-enumerable `message` `"out of memory"` and `stack`
`"RangeError: out of memory"`, unreachable from the global object), by setting
`$exc_value` to it and `$exc_pending`.

- **Reserve.** `abi::GC_RESERVE_PAGES` (4 pages, 256 KiB) are held out of the
  pool in `$__gc_reserve`, taken by the first collection and, once spent,
  taken back by the next collection that finds them.
- **A request that must not fail** (a small one, `__alloc_fixed`) releases
  the reserve to the pool on out of memory, throws, and retries: the caller
  gets a valid zeroed block, writes its fields, and its next exception check
  unwinds. With the reserve spent (or a fixed span larger than it) it writes
  the static line `abi::OOM_LINE` (`Uncaught RangeError: out of memory`) to
  fd 2 through its static iovec — allocating nothing — and calls
  `proc_exit(1)`.
- **A large variable-size request** (`__alloc`; a span beyond the cap
  included) throws and returns `0`. Every such caller checks and leaves a
  valid dummy with the exception pending: a string builder returns the static
  empty string `abi::OOM_EMPTY_STR` (a builtin returns `undefined`),
  `__new_string` returns `0` to its one large caller (`__json_indent`),
  `__new_array` an empty array, `__array_reserve` / `__define` / `__intern`
  leave the store, table or intern table unchanged (`__array_push`,
  `__array_concat`, `__array_set` / `__array_define`, `__array_set_length` and the
  `__new_array` callers with a run-time length then stop before writing).
- **Propagation.** User code checks `$exc_pending` after the pure allocators
  (`__concat` through `+` and `String`, `__array_push`, `__array_concat`,
  `__new_array` with a run-time length, `SetFunctionName`'s `__define`, a
  global's `__define`) and before and after `console.log`, so no dummy
  reaches a binding, a property or the output. An engine-built error, and a
  `throw`, whose construction ran out of memory throw the pending
  `%OutOfMemoryError%` instead (`expr.rs::emit_set_exc_value`), so no error
  with a dummy message escapes.

### 5.7 Garbage collection

`emit/gc.rs`: a precise, non-moving, stop-the-world mark-sweep, run only
inside the allocator (§5.6 steps 1 and 5). Non-moving because handles are
identities, emitted code keeps raw `i32` bases in locals and on the operand
stack, object literals co-allocate their property table inside the object,
and interned keys compare by address: a raw base never needs re-deriving
after a safepoint, its block only needs to be rooted while the base is live.
No write barriers. A collection never calls `__alloc`, never runs user code
and never nests (`$__gc_busy`).

- **Shadow stack.** Each function reserves a slot for every jsval or base
  live across a safepoint (any call that may allocate), clears its slots on
  entry, and pops them on every exit including the exception paths; a raw
  `i32` base is rooted as `HEAP | base`. Each helper declares every parameter
  and local as a jsval, a base or key, or raw, and the builder rejects an
  undeclared one, a rooted local written before or after a shrink-wrapped
  frame region, and a safepoint outside it; a raw local holds an integer, an
  interior address, or a base that another root keeps reachable. Helpers with
  a cold allocating path (`__array_push`, `__array_reserve`, `__intern`
  growth, `__prop_find` materialization) push the frame only on that path.
- **Marks.** Header byte `gc` holds an epoch: `$__gc_epoch` cycles `1..=255`
  (never `0`) and a block is marked iff `gc == $__gc_epoch`; nothing clears
  marks. Fresh blocks (`gc = 0`) are never marked and static blobs are never
  marked at all.
- **Collection** (`__gc_collect`): set `$__gc_busy`, advance the epoch, reset
  the three weak lists to the sentinel `1`, mark (below), run the weak steps
  (below: ephemeron fixpoint, clear `WeakMap` / `WeakSet` stores, clear
  `WeakRef`s, process registries), return a paged mark stack to the pool,
  tombstone the intern table (§5.3), `__gc_sweep` (§5.6), take back the
  out-of-memory reserve when it was spent (§5.6), renew the budget, clear
  `$__gc_busy`.
- **Roots**, visited by every marking pass:

  | root | treatment |
  |---|---|
  | shadow stack `[$__sp + 8, SS_SIZE)` | jsvals |
  | `$exc_value`, `$cf_value` (loop-target codes are numbers), `$__this`, `$__new_target`, `$__spread_args`, the module cells (tagged-template sites included) | jsvals |
  | `$__job_head`, `$__job_tail`, `$__rejected_head`, `$__kept_objects` (§6.7); `$__out_buf`, `$__path_buf`, `$__refs_buf` (§6.9) | raw refs |
  | every registry on `$__fin_jobs` (linked through `job_next`) | raw refs: a queued cleanup job keeps its registry, its `cleanup` and its held values alive |
  | the intern table | weak; only its own `TYPE_RAW` block (at `$__intern_base - 8`, once grown onto the heap) is marked |
  | the static root list (§4.2) | each block's fields are traced every pass; the block itself is never marked. Static strings, symbols and accessors are immutable and not listed. The `Symbol.for` registry is an intrinsic, so registered symbols stay alive, as the spec requires |
  | `$__json_data/_pos/_len`, `$__intrinsics_dirty`, `$exc_pending`, `$cf_kind`, `$__heap_ptr`, the collector's own globals (`$__gc_reserve` included: its pages are out of the heap's page codes) | integers, not roots |

- **Visiting** a reference (`__gc_visit`): a jsval is followed iff its top 16
  bits are `0x7FFD` (`HEAP`; `HOLE` and every number fail the test); a raw
  `u32` ref of `0` is none. A heap address that the page table shows to be a
  small slot start or a large span's block is marked and pushed unless already
  marked. An interior heap address holding a `TYPE_PROPS` header (an object
  literal's or error's inline table) and a static `TYPE_PROPS` / `TYPE_ELEMS`
  (a static block's table, which user code may write heap values into) are
  pushed without marking: each is reached only through its owner, so it is
  traced on every trace of the owner and no stale mark can hide it. Anything
  else is ignored.
- **Tracing** (`__gc_trace`, by type byte):

  | type | references traced (offsets from the base) |
  |---|---|
  | `0` `TYPE_STRING`, `19` `TYPE_RAW`, `21` `TYPE_BIGINT`, `0xFF` `TYPE_FREE` | none (leaves) |
  | object family (`1`–`3`, `6`–`10`, `15`–`18`) | prefix: raw `props @4`, jsval `proto @8`; then per kind below |
  | `1` `TYPE_ARRAY` | raw `elems @20` |
  | `2` `TYPE_OBJECT` classes `0` ordinary, `1` error, `2` arguments, `6` global | prefix only (an inline table is reached through `props`) |
  | `2` classes `3` / `4` / `5` / `7` / `9` wrappers | jsval `@16` |
  | `2` class `8` iterators | jsval `@16`; kinds `>= 16` (the `Iterator.from` wrapper `16`, the lazy helpers `17..=21`, the async-from-sync iterator `32`) also jsvals `@32`, `@40`; helpers also `@56`, `@64`, `@72` (`@24`, `@28` and a helper's `f64 @48` are not references) |
  | `3` `TYPE_FUNCTION` | jsval `env @24` (a bound function's `[target, boundThis, ...args]` array, a settler's resolving record) |
  | `4` `TYPE_ENV` | jsval slots `@8 + 8k`, `k < count @4`: closure envs, generator frames, async-generator request records, promise reaction / job / thenable-job records, resolving-function records, `Array.fromAsync` state |
  | `5` `TYPE_CELL` | jsval `@4` (unaligned) |
  | `6` `TYPE_MAP`, `7` `TYPE_SET` | raw ref `index @20`, jsval `entries @24` |
  | `8` `TYPE_PROMISE` | raw `reactions @20`, jsval `value @24`, raw `rejected_next @36` |
  | `9` `TYPE_GENERATOR` | jsval `env @32`, raw `queue @40`, `queue_tail @44` |
  | `10` `TYPE_DATE` | prefix only |
  | `11` `TYPE_ACCESSOR` | jsvals `getter @8`, `setter @16` |
  | `12` `TYPE_PROPS` | entries `k < count @4` at `16 + 16k`: raw key `@+0` (`0` = tombstone), jsval value `@+8` |
  | `13` `TYPE_ELEMS` | jsval slots `@8 + 8i` for every `i < cap @4` (not `len`, so a stale slot can only retain); a store flagged `ELEMS_FLAG_WEAK` is marked but none of its slots is traced |
  | `14` `TYPE_SYMBOL` | jsval `description @8` |
  | `20` `TYPE_PROXY` (phase 5b, §6.11) | raw `props @4`, jsvals `proto @8`, `env @24`, `target @32`, `handler @40` |
  | `15` `TYPE_WEAKMAP`, `16` `TYPE_WEAKSET` | raw `entries @20` (the weak store: marked, not traced); the handle is linked into `$__gc_weak` through `weak_next` unless already linked this cycle |
  | `17` `TYPE_WEAKREF` | nothing past the prefix (`target @24` is weak); linked into `$__gc_weak_refs` |
  | `18` `TYPE_FINREG` | jsval `cleanup @24`, raw `cells @20` (the weak store: marked, not traced) and each cell's `held` value (slot `3i + 1`; targets and tokens are weak); linked into `$__gc_weak_regs` |

  Partially initialized blocks are safe to trace: every unwritten field is
  `0`.
- **Mark stack.** `u32` bases, starting on the static 4 KiB stack at
  `abi::GC_MARK_STACK`, so marking always makes progress; a full stack moves
  to `max(1, 2·size / 64 KiB)` fresh pages from `__gc_take_pages` (copied,
  paged storage released), never `__alloc`. When no page is left the pushed
  block stays unmarked and `$__gc_overflow` is set. Marking is a sequence of
  passes: visit the roots, trace the static root list, drain the stack after
  each group; a pass that overflowed is followed by one that also re-traces
  every marked heap block (`__gc_drain` after each), until a pass completes
  without overflow.
- **Weak steps**, after marking, in this order. None allocates or runs user
  code; each walks only the handles traced this cycle, so an unreachable
  `WeakMap`, `WeakRef` or registry is simply swept (an unreachable registry
  is not processed and its cleanup never runs). A key or target is *live*
  when it is a static block or a heap block carrying the epoch.
  1. *Ephemeron fixpoint*: for every pair of every `$__gc_weak` `WeakMap`
     whose key is live, visit the value; when that marked anything or
     overflowed, drain (rescanning until no overflow is left) and repeat,
     until a pass marks nothing. Values of dead keys stay unmarked.
  2. *Clear `WeakMap` / `WeakSet`*: compact each store in place, keeping the
     entries whose key is neither `HOLE` nor dead, refill the tail with empty
     entries, store the new `live` count, clear `weak_next`.
  3. *Clear `WeakRef`s*: a dead target becomes `undefined`.
  4. *Process registries*: a cell whose target died becomes empty (target
     `HOLE`; its held value was marked by the trace and waits for the
     cleanup), a dead token becomes `undefined`; a registry that emptied a
     cell and has no job queued sets `FINREG_FLAG_JOB_QUEUED` and is pushed on
     `$__fin_jobs` (`HostEnqueueFinalizationRegistryCleanupJob`). The cleanup
     itself runs at the next `__drain()` checkpoint (§6.7).

  Every `weak_next` link is `0` again after the weak steps.

### 5.8 Binary data: `ArrayBuffer`, typed arrays, `DataView` (target design)

**Status: stages T2 (`ArrayBuffer`), T3a (typed-array core), T4
(`DataView`), T3b (typed-array methods) and T5a (`Float16Array`,
`Math.f16round`, base64 / hex) implemented, the rest is design.** This
section is the target of the phase-5b binary-data workstream (§14, stages
T2–T6 in §5.8.10). In the tree now: the three type constants and the three
layouts (`abi::TYPE_*`, `AB_*`, `TA_*`, `DV_*`, `BYTE_LENGTH_MAX`), the
§5.8.2–§5.8.4 byte blocks from `__ab_alloc`, `resize`, `transfer` /
`transferToFixedLength` and detaching, the buffer witnesses `__ta_length`
/ `__ta_oob` / `__dv_length` and `__ta_validate`, the collector arms
(§5.8.3), every §5.8.5 hook with `__ta_canonical`, the element helpers and
the inline `ta[k]` arms, the number kinds of §5.8.6 through the shared
element accessors `__raw_load(addr, kind, swap)` / `__raw_store(addr, kind,
n, swap)` (both typed arrays, with `swap = 0`, and `DataView`, with `swap
= !littleEndian`, call them; the byte swap is implemented), the
`%ArrayBuffer%` / `%ArrayBuffer.prototype%` part of §5.8.7
(`emit/builtins/array_buffer.rs`), its `%TypedArray%` part — the ten number-kind constructors `Int8Array` .. `Float64Array` (each
`Builtin::Ctor(BuiltinCtor::Typed(kind))`), `from`, `of`, `get
[@@species]`, the prototype getters, `get [@@toStringTag]`, `keys` /
`values` / `entries` / `[@@iterator]` and the Array Iterator `next` arm
(`emit/builtins/typed_array.rs`, with `TypedArrayCreateFromConstructor`,
`TypedArrayCreateSameType` and `TypedArraySpeciesCreate`), and every other
`%TypedArray.prototype%` method of §5.8.7 (`emit/builtins/typed_array_methods.rs`,
`Builtin::TypedArray(op)` for `op` in `TypedArrayOp::METHODS`; `toString` is
the `Builtin::ArrayToString` function object; `sort` / `toSorted` run the
merge sort of `Array.prototype.sort`, `array::merge_pairs`, over a private
list of `(value, value)` pairs for both orders, and `toLocaleString` shares
its element step, `array::locale_element`; the bulk copies are inline
`memory.copy`, so `emit/binary.rs` gained no helper; no new `__prop_throw`
mode: a re-validation after user code raises mode `45` with the method's
name) — and its
`%DataView%` part except the BigInt pairs
(`emit/builtins/data_view.rs`; the get / set bodies are
`Builtin::DataView(DataViewOp::Get(kind) | DataViewOp::Set(kind))` over
`ir::ElementKind`), the rendering of all three (§5.8.8, `emit/binary.rs`,
the `Group::Binary` helpers), and the test262 `$262` prelude. A byte
block's `len` (its capacity) is exactly the size requested from
`__ab_alloc`, not rounded up to the size class. The `has_binary` gate of
§5.8.5 exists (`Helpers::has_binary`, `sections.rs::program_has_binary`,
naming `ArrayBuffer`, `DataView`, the ten typed-array constructors and the
global object so far; `Math.f16round` is not a binary name: its body calls
the `Group::Binary` leaves `__f16_encode` / `__f16_decode` directly, which
need no gated arm, so runtime shaking keeps just those two) and keeps every binary arm out of a program that
cannot reach binary data; the same names are `program_has_accessors`' too.
The typed-array `__prop_throw` modes are `44` (incompatible receiver), `45`
(out of bounds or detached), `46` (`[[PreventExtensions]]` of a
variable-length view; `__set_integrity` itself returns status `0` without
throwing, `Object.preventExtensions` / `seal` / `freeze` raise mode `46`),
`47` / `48` (`seal` / `freeze` with elements, left pending by
`__set_integrity`). Stage T5a added `Float16Array` (kind `7`, through the
generic constructor and method bodies; `__raw_load` / `__raw_store` call
`__f16_encode` / `__f16_decode`, §5.8.6), `DataView.prototype.getFloat16` /
`setFloat16` (`DataViewOp::Get` / `Set(ElementKind::Float16)`),
`Math.f16round` (`MathOp::F16round`, lowered like `fround` in
`emit/numeric.rs`) and the base64 / hex builtins (`Builtin::Base64(op)`,
`emit/builtins/base64.rs`, part of `Builtin::is_binary`; no new
`__prop_throw` mode). The BigInt kinds do not exist yet
(the content-type checks of `TypedArraySpeciesCreate` and `set` compare
the kinds against the first BigInt kind `10`, so they hold until T5b):
every stage implements its part against this text and,
as it lands, moves the landed facts into the sections that describe the
current tree (§5.1 family list, §5.2 table, §5.7 tracing table, §6.2 helpers,
§6.3, §6.4 intrinsics, §6.9, §7.9) and deletes them from here, so this
section shrinks to nothing by T6. A stage whose code disproves a decision
here reports the conflict instead of picking a different design.

#### 5.8.1 Type tags

| type byte | kind |
|---|---|
| `20`..`23` | `20` is `TYPE_PROXY` (§6.11), `21` is `TYPE_BIGINT` (§6.10); `22` / `23` spare |
| `24` `TYPE_ARRAYBUFFER` | an `ArrayBuffer` object |
| `25` `TYPE_TYPEDARRAY` | an instance of any of the concrete typed-array constructors |
| `26` `TYPE_DATAVIEW` | a `DataView` object |

All three join the **object family** (§5.1): the 16-byte prefix (`props`,
`proto`), so they carry own named and symbol properties, a prototype, the
`NOT_EXTENSIBLE` flag (bit `1`), and `typeof` is `"object"` (they are neither
strings nor symbols, so `emit_is_object` already answers `1`). The header
`class` byte stays `0`; the binary-specific header flags use bits `2` and up
(below). The byte storage is not a new kind: it is a `TYPE_RAW` block (§5.8.2).

#### 5.8.2 Byte storage

A buffer's bytes live in a **`TYPE_RAW` data block** (`19`, §5.2): header,
`len: u32 @4` = the block's byte **capacity**, the bytes from `@8`. Reusing
`TYPE_RAW` instead of a dedicated leaf kind costs nothing and needs no new
collector or renderer arm: `TYPE_RAW` is already a traced-as-leaf kind, never a
jsval, and only reachable through a raw `u32` field of its owner.

- **Alignment.** A block base is 8-byte aligned (§5.1), so the data at
  `block + 8` is 8-byte aligned. A typed array's `byteOffset` is a multiple
  of its element size (the constructor throws otherwise), so every typed-array
  element access is naturally aligned. `DataView` accesses take any byte
  offset. Wasm defines unaligned loads and stores and `MemArg.align` is only
  a hint, so the shared accessors (§5.8.6) emit `align = 0` for both and are
  correct on every host.
- **Zero-fill.** Every allocator block comes back zeroed (§5.6), so a fresh
  buffer needs no `memory.fill`. Bytes that leave the live range (a shrinking
  `resize`, a shrinking `transfer`) are zeroed at once with `memory.fill`, so
  a later growth inside the same capacity reads zeros, as the spec requires.
- **Maximum length.** `abi::BYTE_LENGTH_MAX = 2^29` (512 MiB, the whole
  linear-memory cap `MEMORY_MAX_PAGES`, §5.6). Every byte length and offset
  therefore fits a `u32` with room for `offset + length` arithmetic, and every
  valid typed-array index is below `2^29` (so it is always a canonical array
  index, §5.8.5). A requested `byteLength` or `maxByteLength` above it throws
  `RangeError: Array buffer allocation failed` at the spec's
  `CreateByteDataBlock` step (after `GetPrototypeFromConstructor`), without
  allocating.
- **Allocation.** `__ab_alloc(size) -> block | 0` allocates `TYPE_RAW` of
  `8 + size` bytes through the variable-size `__alloc` (§5.6) and writes the
  header and `len = size`; a `size` of `0` allocates nothing and returns `0`.
  Sizes up to 2040 bytes take a small size class, larger ones a large span.
  When `__alloc` returns `0` the out-of-memory path has already left the
  `%OutOfMemoryError%` (a `RangeError`) pending; the caller returns at once
  (a builtin returns `undefined` with the exception pending), which satisfies
  the spec's "throw a `RangeError`" for an impossible data block.
- **No reservation.** A resizable buffer is *not* allocated at
  `maxByteLength`: its block holds the current length rounded by the size
  class, and growth reallocates (§5.8.4). Views never cache a data address
  (they always load `buffer.data`), so a moved block is invisible to them.

#### 5.8.3 Block layouts

Offsets from the block base; all three are 32-byte blocks (size class 2).

**`TYPE_ARRAYBUFFER` (`24`)** — `abi::AB_*`:

| offset | size | field |
|---|---|---|
| 0 | 4 | header; flags (byte 2): `1` `NOT_EXTENSIBLE`, `2` `AB_FLAG_RESIZABLE`, `4` `AB_FLAG_DETACHED`; bit `8` is reserved for an immutable buffer (not implemented, §5.8.9) |
| 4 | 4 | `props` |
| 8 | 8 | `proto` |
| 16 | 4 | `data: u32` (`AB_DATA_OFFSET`) → the `TYPE_RAW` block; `0` when detached or when no byte was ever needed (`byte_length` and capacity `0`) |
| 20 | 4 | `byte_length: u32` (`AB_BYTE_LENGTH_OFFSET`), `0` when detached |
| 24 | 4 | `max_byte_length: u32` (`AB_MAX_BYTE_LENGTH_OFFSET`); equals `byte_length` for a fixed-length buffer, `0` when detached |
| 28 | 4 | reserved, `0` |

**`TYPE_TYPEDARRAY` (`25`)** — `abi::TA_*`:

| offset | size | field |
|---|---|---|
| 0 | 4 | header; flags: `1` `NOT_EXTENSIBLE`, `2` `TA_FLAG_LENGTH_TRACKING` (`[[ArrayLength]]` is `auto`), `4` `TA_FLAG_RESIZABLE_BUFFER` (the buffer was resizable at construction, so `IsTypedArrayFixedLength` is false; never cleared, a buffer's resizability never changes) |
| 4 | 4 | `props` |
| 8 | 8 | `proto` |
| 16 | 4 | `buffer: u32` (`TA_BUFFER_OFFSET`) → the `TYPE_ARRAYBUFFER` block, never `0` |
| 20 | 1 | `kind: u8` (`TA_KIND_OFFSET`), the element kind (§5.8.6) |
| 21 | 1 | `shift: u8` (`TA_SHIFT_OFFSET`), `log2` of the element size (cached from `kind`) |
| 22 | 2 | reserved, `0` |
| 24 | 4 | `byte_offset: u32` (`TA_BYTE_OFFSET_OFFSET`) |
| 28 | 4 | `array_length: u32` (`TA_LENGTH_OFFSET`); ignored (kept `0`) when length-tracking |

`[[ByteLength]]` is not stored: it is `array_length << shift` (or derived from
the buffer when length-tracking). `[[ContentType]]` is `BigInt` exactly for
kinds `10` / `11`.

**`TYPE_DATAVIEW` (`26`)** — `abi::DV_*`:

| offset | size | field |
|---|---|---|
| 0 | 4 | header; flags: `1` `NOT_EXTENSIBLE`, `2` `DV_FLAG_LENGTH_TRACKING`, `4` `DV_FLAG_RESIZABLE_BUFFER` |
| 4 | 4 | `props` |
| 8 | 8 | `proto` |
| 16 | 4 | `buffer: u32` (`DV_BUFFER_OFFSET`) → the `TYPE_ARRAYBUFFER` block |
| 20 | 4 | `byte_offset: u32` (`DV_BYTE_OFFSET_OFFSET`) |
| 24 | 4 | `byte_length: u32` (`DV_BYTE_LENGTH_OFFSET`); ignored when length-tracking |
| 28 | 4 | reserved, `0` |

**Garbage collection** (§5.7 `__gc_trace`): each is traced like the object
family (raw `props @4`, jsval `proto @8`), then:

| type | references traced |
|---|---|
| `24` `TYPE_ARRAYBUFFER` | raw `data @16` (a `TYPE_RAW` leaf: marked, nothing inside traced) |
| `25` `TYPE_TYPEDARRAY` | raw `buffer @16` |
| `26` `TYPE_DATAVIEW` | raw `buffer @16` |

The arms sit in `emit/gc.rs::emit_trace` before the existing "everything past
`TYPE_DATE` is a leaf" exit, so that exit keeps returning for `19` and the
reserved `20..=23`. A view keeps its buffer alive; a buffer keeps its block
alive; nothing points back. There are no weak edges and no finalization.

**Initial prototypes.** `%ArrayBuffer.prototype%`, `%DataView.prototype%`, and
the concrete `%Int8Array.prototype%` .. `%BigUint64Array.prototype%` (each
inheriting `%TypedArray.prototype%`), or `GetPrototypeFromConstructor`'s
result for a subclass or any other `new.target` (the constructors are
`SELF_ALLOC`, §5.5).

#### 5.8.4 Detach, resize, and out-of-bounds views

Every view reads its buffer's state on every access; nothing is cached across
a user-observable step.

- **`ArrayBuffer.prototype.resize(n)`** (resizable only, else `TypeError`):
  `ToIndex(n)`, then a detached buffer throws `TypeError`, then
  `n > max_byte_length` throws `RangeError: ArrayBuffer.prototype.resize:
  Invalid length parameter`. Shrinking zeroes `[n, byte_length)` and keeps the
  block. Growing within the block's capacity just raises `byte_length` (the
  tail is already zero); beyond it allocates `min(max_byte_length, max(n,
  2·capacity))` bytes, copies `byte_length` bytes, and rewrites `data` (the
  old block becomes garbage). An allocation failure leaves the buffer
  unchanged with the `RangeError` pending.
- **Detach** (`DetachArrayBuffer`) is reachable only through `transfer` /
  `transferToFixedLength` (`ArrayBufferCopyAndDetach`): `newLength` is the
  current length when `undefined`, else `ToIndex`; a detached receiver throws
  `TypeError`; `transfer` keeps resizability (`newMax = max_byte_length`, and
  `newLength > newMax` is a `RangeError`), `transferToFixedLength` makes a
  fixed buffer. The new buffer (from `%ArrayBuffer%`, never a species) takes
  over the old `data` block **without copying** when `newLength <=`
  capacity (zeroing `[newLength, byte_length)` when it shrinks), else gets a
  fresh block and a copy. The old buffer then gets `data = 0`,
  `byte_length = max_byte_length = 0` and `AB_FLAG_DETACHED`. `slice`
  never detaches.
- **The test262 host hook.** `harness/detachArrayBuffer.js` calls
  `$262.detachArrayBuffer(buffer)`, a host-defined function; jsc has no host
  detach API. The test262 runner (`crates/jsc-test262/src/assemble.rs`)
  prepends a runner-owned prelude, like `PRINT_PRELUDE`, to the program of
  every test whose `includes` list `detachArrayBuffer.js` (only those, so no
  other test changes):

  ```js
  var $262 = (function (transfer, detached) {
    return { detachArrayBuffer: function (buffer) {
      if (!detached.call(buffer)) transfer.call(buffer);
      return null;
    } };
  })(ArrayBuffer.prototype.transfer,
     Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, "detached").get);
  ```

  Capturing `transfer` and the `detached` getter first keeps the hook working
  when a test replaces them; detaching an already detached buffer is a no-op,
  as for the spec's `DetachArrayBuffer`. The other `$262` members
  (`createRealm`, `evalScript`, `gc`, `agent`) stay absent; their tests are
  denied by feature (`cross-realm`, `host-gc-required`, `Atomics`).
- **Buffer witness.** `__ta_length(o) -> i32` is
  `TypedArrayLength(MakeTypedArrayWithBufferWitnessRecord(o, seq-cst))` with
  out-of-bounds folded to `0`: let `b = buffer`, `L = b.byte_length`; a
  detached `b` is out of bounds; else with `off = byte_offset`, a
  length-tracking view is out of bounds iff `off > L` and has length
  `(L - off) >> shift`; a fixed view is out of bounds iff `off + (array_length
  << shift) > L` and has length `array_length`. `__ta_oob(o) -> i32` is
  `IsTypedArrayOutOfBounds` (detached included). `__dv_length(o) -> i32`
  is `GetViewByteLength` with `-1` for `IsViewOutOfBounds` (the same rule over
  bytes). All three are non-allocating leaves.
- **Validation.** `__ta_validate(o, name) -> i32` is `ValidateTypedArray`: a
  receiver that is not `TYPE_TYPEDARRAY` throws `TypeError: Method <name>
  called on incompatible receiver`, an out-of-bounds or detached one throws
  `TypeError: Cannot perform <name> on a detached or out-of-bounds TypedArray`
  (`name` is the builtin's static name string, spliced in by `__prop_throw`
  modes `44` / `45`), else it returns the length.
- **Re-validation rule.** Every builtin follows its spec step order exactly.
  After any step that can run user code — `ToNumber` / `ToIntegerOrInfinity`
  / `ToIndex` of an argument (`valueOf`, `@@toPrimitive`), a callback, a
  comparator, `SpeciesConstructor`, an iterator step — it re-reads whatever
  the spec re-reads: the spec's explicit re-validations (`If
  IsTypedArrayOutOfBounds(taRecord) is true, throw a TypeError`, a re-taken
  `TypedArrayLength`) are emitted as written, and every element read or write
  in between goes through `__ta_get` / `__ta_set`, which check
  `IsValidIntegerIndex` on each access (a read of an invalid index is
  `undefined`, a write is ignored). So a buffer detached or shrunk by a
  callback can never make a builtin read or write outside the live bytes.
  Bulk copies (`set`, `slice`, `copyWithin`, `fill`, the constructor's
  typed-array overload) re-derive the byte range from the witness taken after
  the last user-code step and `memory.copy` / `memory.fill` only within it.

#### 5.8.5 Integer-indexed exotic object hooks (§6.3)

A typed array is an integer-indexed exotic object (ECMA-262 §10.4.5). Its
hooks sit in the existing §6.3 helpers, as one extra arm next to the
string / array / String-wrapper exotic arms, keyed on the type byte `25` of
the object being visited (the receiver, or a chain object):

- **Numeric keys.** A key is interned (§5.3). A key flagged `ARRAY_INDEX`
  (`"0"` .. `"4294967294"`) is an integer index `i`. Any other key is passed to
  `__ta_canonical(key) -> i32` (`1` iff `CanonicalNumericIndexString(key)`
  is not `undefined`): a symbol key or a key whose first code unit is not a
  digit, `-`, `I` or `N` is `0` at once; `"-0"`, `"NaN"`, `"Infinity"` and
  `"-Infinity"` are `1`; otherwise `n = __string_to_number(key)` and the
  answer is `ToString(n) === key` (`__num_to_str`, `__str_eq`; so `"1.0"`,
  `"01"`, `"+1"`, `"1e3"` are `0` and stay ordinary keys, while `"1.5"`,
  `"-1"`, `"4294967295"`, `"1e+21"` are `1`). Because a valid index is below
  `2^29` (§5.8.2), every canonical numeric key that is not an
  `ARRAY_INDEX` key — `-0` included — is an **invalid** integer index. It
  allocates (`__num_to_str`), so it is not a leaf; it runs only for a
  typed-array object and a non-index key starting with one of those units.
- **`IsValidIntegerIndex(o, i)`** is `i < __ta_length(o)` (unsigned; an
  out-of-bounds view has length `0`). A computed key that was never interned
  reaches the helpers as `0` (§5.3), which every object treats as absent;
  that is also the right answer for an invalid numeric key.
- **`[[GetOwnProperty]]`** (`__own_desc`, `__own_enum`, `__has_own`): a valid
  index is `{value: element, writable: true, enumerable: true, configurable:
  true}`; an invalid numeric key is absent and **never reaches the table**.
- **`[[HasProperty]]`** (`__has`): a numeric key answers
  `IsValidIntegerIndex` and stops; the chain is not consulted.
- **`[[Get]]`** (`__get`, `__super_get`, `__proto_elem`): a numeric key
  returns `__ta_get(o, i)` (`undefined` when invalid) and stops — on the
  receiver or on any chain object (a typed array used as a prototype answers
  its own indices). Other keys continue at the table and the chain.
- **`[[Set]]`** (`__set`, `__super_set`, `__array_set`'s chain consult
  `__proto_set`), the ES2024 rule: at the first chain object `o` that is a
  typed array, a numeric key decides. If `o` is the receiver,
  `TypedArraySetElement`: `ToNumber(v)` (`ToBigInt` for the BigInt kinds)
  **first** — even for an invalid index — then `__ta_set(o, i, n)` stores
  when the index is (still) valid; the result is success either way. If `o`
  is not the receiver and the index is invalid, the set is a silent success
  (nothing is created). If it is valid, `o` behaves as an own writable data
  property found on the chain: the existing receiver path of `__set`
  (`OrdinarySetWithOwnDescriptor`) defines or creates the property on the
  receiver (a primitive or non-extensible receiver throws as today).
  `__protos_indexless(o)` (§5.4) answers `0` when a prototype is a typed
  array, so an array's hole or out-of-range store consults it through
  `__proto_set` (and an invalid index there creates nothing, per the rule
  above).
- **`[[DefineOwnProperty]]`** (`__define_own`, `__define`): a numeric key
  rejects when invalid, or when the descriptor has `configurable: false`,
  `enumerable: false`, an accessor field, or `writable: false`; otherwise a
  present `value` is written by `TypedArraySetElement` and the result is ok.
  `Object.defineProperty` turns a rejection into its usual `TypeError`
  (`Cannot redefine property: k`).
- **`[[Delete]]`** (`__delete`): a numeric key succeeds iff the index is
  invalid (nothing to remove, the table is not touched); deleting a valid
  index throws the strict-mode `TypeError` of a non-configurable property.
- **`[[OwnPropertyKeys]]`** (`__own_keys`, `__for_in_keys`, `__copy_data`):
  the indices `0 .. __ta_length(o) - 1` ascending (index keys from
  `__index_string`), then the table's string keys in insertion order, then
  its symbol keys. Values and entries modes read elements through `__ta_get`
  (the per-key re-check of §6.3 applies unchanged: a getter that detaches the
  buffer makes later indices absent).
- **Extensibility and integrity** (`__set_integrity`, `__test_integrity`):
  `[[PreventExtensions]]` fails (`TypeError: Cannot prevent extensions on a
  variable-length TypedArray`) when the view is length-tracking or flagged
  `TA_FLAG_RESIZABLE_BUFFER`; `seal` / `freeze` of a typed array with length
  `> 0` throw `TypeError: Cannot seal array buffer views with elements` /
  `Cannot freeze array buffer views with elements` (an element cannot become
  non-configurable or non-writable); `isSealed` / `isFrozen` are `false` while
  the length is `> 0`. Typed arrays never enter dictionary mode; the array
  helpers (`__dictionize`, `__sparse_*`, `__array_set_length`) never see one.
- **Everything else is ordinary.** `length`, `byteLength`, `byteOffset`,
  `buffer` are prototype getters (§5.8.7), not own properties, so a static
  `ta.length` is `__get` → table miss → `%TypedArray.prototype%`'s getter. No
  key other than a canonical numeric string is exotic. `ArrayBuffer` and
  `DataView` objects are ordinary objects with internal slots; they have no
  exotic hooks.

**Element helpers.** `__ta_get(o, i) -> i64` and `__ta_set(o, i, n: i64)`
(`n` an already converted number jsval, or a BigInt for kinds `10` / `11`)
check `IsValidIntegerIndex`, compute `addr = buffer.data + byte_offset + (i <<
shift)`, and call `__raw_load(addr, kind, swap = 0)` /
`__raw_store(addr, kind, n, swap = 0)` (§5.8.6). `__ta_set_value(o, i, v)` is
`TypedArraySetElement` (the conversion, then `__ta_set`); it is the only one
that can run user code.

**Fast path and gating.** A program that never touches binary data must
emit and execute exactly what it does today (every `tests/opcount` row
unchanged). The mechanism is the one `has_accessors` uses (§6.4,
`emit/sections.rs::program_has_accessors`): a whole-program scan sets
`Helpers::has_binary` (`emit/mod.rs`), and every arm above, the inline fast
path below, and the binary branches of the renderer and the iterators are
emitted only when it is `true`. `program_has_binary` is `true` iff the IR
names a binary global — `IrNode::Intrinsic(GlobalIntrinsic::Ctor(c))` /
`GlobalGet` / `GlobalSet` of `ArrayBuffer`, `DataView` or a typed-array
constructor, or an `IrNode::Str` equal to one of those names (a static
`globalThis.Uint8Array` or `o["DataView"]`) — or the program uses the global
object as a value
(`IrNode::Intrinsic(GlobalIntrinsic::Global)`: a `globalThis` / top-level
`this` that could reach a constructor by a computed name). A binary program
also needs the accessor dispatch of `__get` (the `length` / `buffer` /
`byteLength` / `byteOffset` getters), so the same names are added to
`program_has_accessors`. Runtime shaking (§4.2) applies the same gate to
the realm: with `has_binary` false no binary-data builtin (the
`ArrayBuffer`, `DataView` and `TypedArray` builtin families, the
binary-data constructors and their `get [@@species]`, the `Uint8Array`
base64 / hex builtins, `Builtin::is_binary`)
and no realm entry owned by or naming a binary-data constructor or
prototype (`Entry::is_binary`) is a shaking root — not as an internal
builtin, a symbol-keyed entry, an accessor half, or under a string key a
kept body looks up (a global name, a prototype's `constructor`). Since no
shared helper calls a `Group::Binary` helper without the gate, the binary
builtin bodies and the `Group::Binary` helpers are stubbed; only the realm
image's binary intrinsic blocks and the static strings remain, as the data
segments are not shaken. With the gate open shaking is unchanged, and a
program indexing with computed keys keeps the whole runtime as today.

Inline `ta[k]` (gated): `emit/object.rs::emit_index_dynamic` gains a second
arm after the `TYPE_ARRAY` arm — type byte `25` and a number key — that
calls `__ta_index_get(o, k: i64) -> i64`; `emit_index_set_node` gains the same
arm calling `__ta_index_set(o, k, v)`. A number key on a typed array never
reaches the chain: `ToPropertyKey` of a number is always a canonical numeric
string, so an integral `0 <= k < 2^32 - 1` (`-0` is `0`) is index `k`
(element or `undefined`), and any other number (`1.5`, `-1`, `NaN`,
`Infinity`, `2^32`) is an invalid index (`undefined`; a store still runs
`ToNumber(v)` and is then ignored). Every other key or receiver falls
through to the existing `__get` / `__set` / `__array_set` calls. There is no
`.length` fast path for typed arrays in this workstream (phase 6).

#### 5.8.6 Element kinds and value conversion

Typed-array storage is little-endian (the wasm memory order; the spec's
"platform endianness" is chosen little), and so is a `DataView` access with a
truthy `littleEndian`; a big-endian `DataView` access byte-swaps the integer
bits after the load / before the store.

| kind | constructor | size | conversion on write | load (bits) | read as number | store |
|---|---|---|---|---|---|---|
| `0` | `Int8Array` | 1 | `ToInt8` | `i32.load8_s` | `f64.convert_i32_s` | `i32.store8` |
| `1` | `Uint8Array` | 1 | `ToUint8` | `i32.load8_u` | `f64.convert_i32_u` | `i32.store8` |
| `2` | `Uint8ClampedArray` | 1 | `ToUint8Clamp` | `i32.load8_u` | `f64.convert_i32_u` | `i32.store8` |
| `3` | `Int16Array` | 2 | `ToInt16` | `i32.load16_s` | `f64.convert_i32_s` | `i32.store16` |
| `4` | `Uint16Array` | 2 | `ToUint16` | `i32.load16_u` | `f64.convert_i32_u` | `i32.store16` |
| `5` | `Int32Array` | 4 | `ToInt32` | `i32.load` | `f64.convert_i32_s` | `i32.store` |
| `6` | `Uint32Array` | 4 | `ToUint32` | `i32.load` | `f64.convert_i32_u` | `i32.store` |
| `7` | `Float16Array` | 2 | `__f16_encode` | `i32.load16_u` | `__f16_decode` | `i32.store16` |
| `8` | `Float32Array` | 4 | `f32.demote_f64` | `i32.load` | `f64.promote_f32(f32.reinterpret_i32)` → `__box_f64` | `i32.store` of `i32.reinterpret_f32` |
| `9` | `Float64Array` | 8 | none | `i64.load` | `__box_f64(f64.reinterpret_i64)` | `i64.store` |
| `10` | `BigInt64Array` | 8 | `ToBigInt64` (T5b) | `i64.load` | a BigInt of the signed value (T5b) | `i64.store` |
| `11` | `BigUint64Array` | 8 | `ToBigUint64` (T5b) | `i64.load` | a BigInt of the unsigned value (T5b) | `i64.store` |

`shift` is `0, 0, 0, 1, 1, 2, 2, 1, 2, 3, 3, 3`. The kind order is the
spec's Table 71 order, so `kind` indexes the static per-kind tables (name,
size, prototype intrinsic).

- **Shared accessors.** `__raw_load(addr, kind, swap) -> i64` loads the
  kind's width as integer bits (one `br_table` over `kind`), byte-swaps them
  when `swap`, and converts them by the "read as number" column;
  `__raw_store(addr, kind, n, swap)` converts the number `n` to the kind's
  bits, swaps when `swap`, and stores the width. Typed arrays pass `swap = 0`
  and an aligned `addr`; `DataView` passes `swap = !littleEndian` and any
  `addr`, so the integer loads use `align = 0` there (two helpers are not
  needed: the align field is a hint, and the helpers use `align = 0`
  throughout, which is as fast on aligned addresses). Both are non-allocating
  leaves for the number kinds; the BigInt kinds (T5b) allocate on read.
  Swaps: 16-bit `((x & 0xFF) << 8) | (x >> 8 & 0xFF)` (then `i32.extend16_s`
  for `Int16`), 32-bit and 64-bit by shift-and-mask (`i32.rotl` / `i64.rotl`
  pairs).
- **Integer conversions.** `ToInt8` .. `ToUint32` are the `ToInt32` bit
  pattern of the bitwise operators (`emit/numeric.rs::emit_to_int32`,
  §7.2) stored at the kind's width: a narrowing store keeps the low bits, which
  is exactly the spec's modulo-`2^8` / `2^16` reduction for both
  signednesses.
- **`ToUint8Clamp`** is `i32.trunc_sat_f64_u(f64.nearest(f64.min(f64.max(n,
  0), 255)))`: wasm's `f64.nearest` rounds half to even, as the spec
  requires (`2.5 -> 2`, `3.5 -> 4`), and a `NaN` survives `min` / `max` and
  saturates to `0`.
- **NaN canonicalization.** A stored float may carry any NaN payload (a
  `DataView` write of raw bits, a `Float32` demotion, bytes written through a
  `Uint8Array` alias). Reads go through `__box_f64` (§4.1), so a NaN read
  from a buffer is always exactly `QNAN` and can never alias a boolean,
  `null`, `undefined` or a pointer. Writes store the number's own bits (a jsval
  NaN is already `QNAN`), which the spec allows (the NaN bit pattern is
  implementation-defined).
- **`Float32`** writes use `f32.demote_f64` (IEEE round-to-nearest-even;
  overflow to `±Infinity`, `-0` kept).
- **`Float16`** (T5a). `__f16_encode(x: f64) -> i32` rounds **directly from
  the `f64` bits** (never through `f32`, which would double-round): sign
  `s`; `NaN` → `0x7E00`; `|x| >= 65520` (the midpoint between the largest
  finite half `65504` and `2^16`) → `±Infinity` (`0x7C00 | s`); a normal
  result (`|x| >= 2^-14`) keeps 10 mantissa bits rounded half-to-even from the
  52 (a carry into the exponent is the correct next binade); below `2^-14`
  the result is `s | trunc(f64.nearest(|x| · 2^24))` — the scaling is exact
  and `f64.nearest` rounds half to even, so `|x| <= 2^-25` gives `±0` and a
  value rounding up to `1024` is the smallest normal's encoding.
  `__f16_decode(h: i32) -> i64` returns the number jsval, exact in `f64`:
  exponent `0` → `±m · 2^-24`; `31` → `±Infinity` or `QNAN`; else
  `±(1024 + m) · 2^(e - 25)`. Both are non-allocating `Group::Binary`
  leaves. `Math.f16round(x)` is `__f16_decode(__f16_encode(ToNumber(x)))`.
- **BigInt kinds** (T5b). Storage, element size and loads/stores are fixed
  now (table above); the value conversion (`ToBigInt64` / `ToBigUint64`, the
  BigInt read) and the content-type checks (a `Number` / `BigInt` mix in
  `set`, the constructor's typed-array overload, `TypedArraySpeciesCreate`
  results, `DataView.prototype.getBigInt64` / `setBigInt64` /
  `getBigUint64` / `setBigUint64`) land with the BigInt workstream's value
  representation; until then `BigInt64Array` / `BigUint64Array` are not
  installed (§5.8.9).

#### 5.8.7 Built-in objects

New intrinsics are appended to the realm image (§6.4) after the existing ones,
in this order: `%ArrayBuffer%`, `%ArrayBuffer.prototype%`, `%TypedArray%`,
`%TypedArray.prototype%`, the concrete constructors and their prototypes in
kind order, `%DataView%`, `%DataView.prototype%`. They are
`Intrinsic::Ctor` / `Intrinsic::Prototype` of new `BuiltinCtor` variants
(`ArrayBuffer`, `TypedArray`, `Typed(ElementKind)`, `DataView`; `ElementKind`
is a new `ir.rs` enum in kind order with its name, size and shift). Every
constructor is a `TYPE_FUNCTION` with `CONSTRUCTOR | SELF_ALLOC` (§5.5); each
throws `TypeError: Constructor <Name> requires 'new'` when called (the
`emit/builtins/ctor.rs` fragment). No new dirty bit is added: no binary
builtin has a guarded fast path that depends on a prototype staying intact.
Methods are writable, configurable, not enumerable; getters are accessor
entries with no setter, configurable, not enumerable.

- **`%ArrayBuffer%`** (`length` 1): `ArrayBuffer(length, options)` —
  `ToIndex(length)`, then `options.maxByteLength` (`Get` only when `options`
  is an object; `undefined` means fixed) through `ToIndex`, `length >
  maxByteLength` is a `RangeError: Invalid array buffer max length`, then
  `GetPrototypeFromConstructor`, then the allocation (§5.8.2);
  `ArrayBuffer.isView(v)` (type byte `25` or `26`), `get [@@species]` (the
  shared builtin returning `this`, as for `%Array%`).
- **`%ArrayBuffer.prototype%`**: getters `byteLength` (`0` when detached),
  `maxByteLength` (`byte_length` for a fixed buffer), `resizable`,
  `detached`; `resize(newLength)`, `slice(start, end)` (relative indices,
  `SpeciesConstructor(O, %ArrayBuffer%)`, the result must be a different,
  non-detached, large-enough `ArrayBuffer`, and the source is re-checked for
  detachment after the constructor runs; the copy is clamped to the source's
  current length), `transfer(newLength)`, `transferToFixedLength(newLength)`;
  `@@toStringTag` `"ArrayBuffer"` (configurable only). A receiver that is not
  `TYPE_ARRAYBUFFER` throws `TypeError: Method ArrayBuffer.prototype.<m>
  called on incompatible receiver` (a getter too); a detached receiver of
  `resize` / `slice` / `transfer*` throws `TypeError: Cannot perform
  ArrayBuffer.prototype.<m> on a detached ArrayBuffer`; `resize` on a fixed
  buffer is the incompatible-receiver `TypeError`.
- **`%TypedArray%`** (`length` 0, `name` `"TypedArray"`, not a global):
  calling or constructing it throws `TypeError: Abstract class TypedArray not
  directly constructable`; statics `from(source, mapFn, thisArg)` (`this`
  must be a constructor; an `@@iterator` source is drained to a list first,
  then `TypedArrayCreateFromConstructor(this, [len])`, then each mapped
  value is `Set`; else an array-like), `of(...items)`, `get [@@species]`.
  Each concrete constructor's `[[Prototype]]` is `%TypedArray%`.
- **Concrete constructors** `%Int8Array%` .. `%BigUint64Array%` (`length` 3,
  own `BYTES_PER_ELEMENT` non-writable, non-enumerable, non-configurable):
  one shared builtin body per kind (`Builtin::Ctor(BuiltinCtor::Typed(kind))`) implementing
  every overload of `TypedArray(...args)`: no argument or a non-object →
  `ToIndex(length)` and a fresh zeroed buffer from `%ArrayBuffer%`
  (`RangeError: Invalid typed array length` past `BYTE_LENGTH_MAX`); a typed
  array → a copy of its current elements (`TypeError` when out of bounds,
  same-kind `memory.copy`, else per-element conversion); an `ArrayBuffer` →
  a view: `ToIndex(byteOffset)` (`RangeError: Start offset of <Name> should
  be a multiple of <size>`), `ToIndex(length)` unless `undefined`, then a
  detached buffer is a `TypeError`, then a length-tracking view (resizable
  buffer, `length` `undefined`) or a fixed one with the spec's
  `RangeError`s (`Invalid typed array length`, `Byte length of <Name> should
  be a multiple of <size>`, `Start offset is outside the bounds of the
  buffer`); any other object → `@@iterator` (`GetMethod`) drained to a list
  when present, else an array-like (`length`, then `Get` of each index, the
  conversions running in index order after the allocation).
  `GetPrototypeFromConstructor` runs at the spec's point of each overload:
  first for the object overloads, after `ToIndex(length)` for the length
  overload.
- **`%TypedArray.prototype%`**: getters `buffer`, `byteLength`,
  `byteOffset`, `length` (the three numeric ones are `0` when out of bounds
  or detached; a non-typed-array receiver throws the incompatible-receiver
  `TypeError`), `get [@@toStringTag]` (the kind's name for a typed array,
  `undefined` for any other value, never throwing); `constructor`; the
  methods `at`, `copyWithin`, `entries`, `every`, `fill`, `filter`, `find`,
  `findIndex`, `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`,
  `join`, `keys`, `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`,
  `set` (array-like and typed-array sources, overlapping same-buffer copies
  through a temporary when the kinds differ, `memmove` otherwise), `slice`,
  `some`, `sort` (numeric order with `-0 < +0` and `NaN` last when no
  comparator; a comparator sorts a jsval copy with the same stable merge sort
  as `Array.prototype.sort`, then writes back through `__ta_set`), `subarray`
  (never validates: an out-of-bounds source counts as length `0`; a
  length-tracking source with `end` `undefined` yields a length-tracking
  result), `toLocaleString`, `toReversed`, `toSorted`, `values` (also
  `[@@iterator]`, the same function object), `with`; `toString` is the same
  function object as `Array.prototype.toString`. `map` / `filter` / `slice`
  / `subarray` create their result by `TypedArraySpeciesCreate`
  (`SpeciesConstructor(O, default)`, then `TypedArrayCreateFromConstructor`:
  the result is validated, must have at least the requested length —
  `TypeError` otherwise — and the same content type); `toReversed` /
  `toSorted` / `with` always create the default kind. Every method starts
  with `__ta_validate` (§5.8.4) and follows the re-validation rule.
- **Iteration.** `keys` / `values` / `entries` return `CLASS_ITERATOR`
  objects of kinds `0`..`2` over `%ArrayIteratorPrototype%` (§5.2), like
  arrays. `%ArrayIteratorPrototype%.next` gains a gated arm for a typed-array
  `[[IteratedObject]]`: an out-of-bounds or detached view throws `TypeError:
  Cannot perform %ArrayIteratorPrototype%.next on a detached or out-of-bounds
  TypedArray`, else the length comes from `__ta_length` and the element from
  `__ta_get`. The `GetIterator` fast paths (§6.4) do not cover typed arrays:
  `for-of` / spread over one take the protocol.
- **Concrete prototypes**: `constructor`, `BYTES_PER_ELEMENT` (constant
  attributes); `[[Prototype]]` `%TypedArray.prototype%`.
- **`%DataView%`** (`length` 1): `DataView(buffer, byteOffset, byteLength)` —
  a non-`ArrayBuffer` throws `TypeError: First argument to DataView
  constructor must be an ArrayBuffer`, `ToIndex(byteOffset)`, a detached
  buffer is a `TypeError`, an offset past the buffer is `RangeError: Start
  offset is outside the bounds of the buffer`, a `byteLength` of
  `undefined` takes the rest of the buffer (a length-tracking view when the
  buffer is resizable), else `ToIndex(byteLength)` and `RangeError: Invalid
  DataView length` past the buffer's end; then
  `GetPrototypeFromConstructor`, then the buffer is re-checked (detached:
  `TypeError`; shrunk below the view: `RangeError`), as the spec orders.
- **`%DataView.prototype%`**: getters `buffer`, `byteLength`, `byteOffset`
  (the latter two throw `TypeError` when the view is out of bounds or
  detached); `getInt8` / `setInt8`, `getUint8` / `setUint8`, `getInt16`,
  `getUint16`, `getInt32`, `getUint32`, `getFloat32`, `getFloat64` and their
  setters (T4), `getFloat16` / `setFloat16` (T5a), the BigInt pairs (T5b) —
  one builtin body per `(kind, get|set)` (`Builtin::DataView(kind, set)`):
  `ToIndex(requestIndex)`, a setter's `ToNumber(value)`, `ToBoolean
  (littleEndian)`, then out of bounds or detached → `TypeError: Cannot
  perform DataView.prototype.<m> on a detached or out-of-bounds DataView`,
  `index + size > viewSize` → `RangeError: Offset is outside the bounds of
  the DataView`, then `__raw_load` / `__raw_store` at `data + byte_offset +
  index` with `swap = !littleEndian`; `@@toStringTag` `"DataView"`.
- **Base64 / hex** (T5a): `Uint8Array.fromBase64(string, options)`,
  `Uint8Array.fromHex(string)`, `Uint8Array.prototype.toBase64(options)`,
  `toHex()`, `setFromBase64(string, options)`, `setFromHex(string)` (the last
  two return `{ read, written }`), in the guest over code units: a
  non-string argument is a `TypeError`, an `alphabet` other than `"base64"` /
  `"base64url"`, a `lastChunkHandling` other than `"loose"` / `"strict"` /
  `"stop-before-partial"` is a `TypeError`, malformed input is a
  `SyntaxError` (thrown after writing the bytes decoded before the error, as
  the spec's `setFrom*` require), `omitPadding` is `ToBoolean`. The
  options object is read through `Get` in spec order (`alphabet`, then
  `lastChunkHandling` or `omitPadding`) after the receiver and string checks
  and before the receiver's out-of-bounds / detached `TypeError`. The
  decoder never runs user code: `setFrom*` write each decoded byte straight
  into the target, and the statics decode twice (counting, then into a
  fresh `Uint8Array` over `%Uint8Array.prototype%`). Error messages: `The
  input must be a string`, `The options argument must be an object`, `The
  receiver must be a Uint8Array`, `Invalid base64 string` / `Invalid hex
  string` (`SyntaxError`), `Invalid string length` (`RangeError`, an
  encoding past the longest string).
- **`Math.f16round`** (T5a), §5.8.6.
- **`Object.prototype.toString`** needs no new builtin tag: typed arrays are
  tagged through `get [@@toStringTag]`, the other two through their
  prototypes' string `@@toStringTag` (all read by `__symbol_get`, §6.2).

#### 5.8.8 Rendering (§6.9)

The renderer gains gated arms (one helper, `__render_binary(p, depth)`), in
Node's console style; like arrays, no element list is truncated:

- typed arrays `Uint8Array(3) [ 1, 2, 3 ]`, `Float64Array(0) []` (numbers by
  the number rule, so `-0` prints `0` and `NaN` prints `NaN`); an
  out-of-bounds or detached view renders with length `0`; own named
  properties follow the elements as for arrays (`Int8Array(1) [ 0, foo: 1
  ]`); a subclass instance renders with its kind's name;
- `ArrayBuffer { [Uint8Contents]: <01 02 ff>, byteLength: 3 }` (lowercase
  two-digit hex bytes separated by spaces, `<>` when empty), `ArrayBuffer {
  (detached), byteLength: 0 }` when detached;
- `DataView { byteLength: 2, byteOffset: 0, buffer: ArrayBuffer {
  [Uint8Contents]: <00 00>, byteLength: 2 } }` (out of bounds: `byteLength:
  0, byteOffset: 0`).

The arms read the blocks directly (no getter runs), allocate only through the
output buffer, and never throw.

#### 5.8.9 What stays unsupported

- `SharedArrayBuffer` and `Atomics` (no shared memory or agents; features
  `SharedArrayBuffer`, `Atomics*` stay denied); the `IsSharedArrayBuffer`
  branches of the spec are dead code and are not emitted.
- Immutable buffers (`transferToImmutable`, `sliceToImmutable`,
  `immutable`; feature `immutable-arraybuffer` stays denied). The harness
  feature-detects `transferToImmutable`, so its absence is not observable to
  the other tests. Header bit `8` of `TYPE_ARRAYBUFFER` is kept free for it.
- `BigInt64Array` / `BigUint64Array` and the `DataView` BigInt methods until
  T5b lands after the BigInt workstream (the globals are absent until then).
- No `$262.detachArrayBuffer` outside the test262 runner: a program detaches
  only with `transfer` / `transferToFixedLength`.

#### 5.8.10 Implementation plan

Each stage is one merge; each lists its new (+) and changed (~) files, the
DESIGN sections it moves facts into, and the denylist lines
(`tests/test262/unsupported-features.txt`) it deletes. Every stage writes its
integration tests (with a `JSC_GC_STRESS` variant, as
`crates/jsc-core/tests/weak_collections.rs`) and keeps every `tests/opcount`
row unchanged; the realm image grows by the new intrinsics, so a row that
moves only through the intern-table layout or the static root list (a run
that collects) must say so in its `reason` cell.

| stage | scope | files | denylist lines deleted |
|---|---|---|---|
| **T2** ArrayBuffer | `TYPE_ARRAYBUFFER`, byte block, constructor, prototype getters, `resize`, `slice`, `transfer`, `transferToFixedLength`, `isView` (tests the type bytes `25` / `26`, defined now), `@@species`, rendering, the test262 `$262` prelude | ~`abi.rs` (`TYPE_ARRAYBUFFER`, `TYPE_TYPEDARRAY`, `TYPE_DATAVIEW` constants with the `20..=23` reservation note, `AB_*`, `BYTE_LENGTH_MAX`); ~`ir.rs` (`BuiltinCtor::ArrayBuffer`, `ALL`, `GlobalIntrinsic::from_name`); ~`emit/realm.rs` (intrinsics, `ArrayBufferOp` builtins appended to `Builtin::ALL`, constructor / species / `@@toStringTag` / global entries); +`emit/builtins/array_buffer.rs`; ~`emit/builtins/mod.rs` (module, dispatch); +`emit/binary.rs` (`__ab_alloc`, `__render_binary`), ~`emit/registry.rs` (a new `Group::Binary` appended after `Extra`, so no existing function index moves), ~`emit/mod.rs` (`Helpers` accessors for the group); ~`emit/roots.rs` (helper-graph leaf declarations); ~`emit/gc.rs` (trace arm `24`); ~`emit/render.rs` (dispatch to `__render_binary`, render strings); ~`crates/jsc-test262/src/assemble.rs` (+ its caller in `lib.rs`: the `detachArrayBuffer.js` prelude, unit-tested in `assemble.rs`); +`crates/jsc-core/tests/array_buffer.rs` | `ArrayBuffer`, `arraybuffer-transfer`, `resizable-arraybuffer` |
| **T3a** TypedArray core | `TYPE_TYPEDARRAY`; `%TypedArray%`, the 11 number-kind constructors except `Float16Array` with every overload, `from`, `of`, `@@species`, `TypedArraySpeciesCreate`, `BYTES_PER_ELEMENT`; prototype getters, `@@toStringTag`, `keys` / `values` / `entries` / `@@iterator`; the §5.8.5 hooks, the `has_binary` gate and the inline fast path; element helpers; iterator `next` arm; rendering | ~`abi.rs` (`TA_*`); ~`ir.rs` (`BuiltinCtor::TypedArray`, `Typed(ElementKind)`, `ElementKind`); ~`emit/realm.rs`; +`emit/builtins/typed_array.rs`; ~`emit/builtins/mod.rs`; ~`emit/binary.rs` (`__ta_length`, `__ta_oob`, `__ta_validate`, `__ta_canonical`, `__ta_get`, `__ta_set`, `__ta_set_value`, `__ta_index_get`, `__ta_index_set`, `__raw_load`, `__raw_store`); ~`emit/registry.rs`, ~`emit/roots.rs`; ~`emit/props.rs` (`__get`, `__super_get`, `__set`, `__super_set`, `__has`, `__has_own`, `__delete`, `__own_keys`, `__own_enum`, `__for_in_keys`, `__copy_data`, `__protos_indexless`, `__proto_elem`, `__proto_set`); ~`emit/props/descriptor.rs` (`__define_own`, `__own_desc`, `__set_integrity`, `__test_integrity`); ~`emit/object.rs` (`emit_index_dynamic`, `emit_index_set_node`); ~`emit/sections.rs` (`program_has_binary`, `program_has_accessors` names), ~`emit/mod.rs` (`Helpers::has_binary`); ~`emit/numeric.rs` (`emit_to_int32` made `pub(super)`); ~`emit/builtins/iter.rs` (`step_array` typed-array arm); ~`emit/gc.rs` (arm `25`); ~`emit/render.rs`; +`crates/jsc-core/tests/typed_arrays.rs` (incl. the §5.8.5 key cases and a resize / detach matrix) | `TypedArray*`, `Int8Array`, `Int16Array`, `Int32Array`, `Uint8Array`, `Uint8ClampedArray`, `Uint16Array`, `Uint32Array`, `Float32Array`, `Float64Array` |
| **T4** DataView | `TYPE_DATAVIEW`, constructor, getters, the number get / set pairs except Float16, rendering | ~`abi.rs` (`DV_*`); ~`ir.rs` (`BuiltinCtor::DataView`); ~`emit/realm.rs`; +`emit/builtins/data_view.rs`; ~`emit/builtins/mod.rs`; ~`emit/binary.rs` (`__dv_length`, swap paths of `__raw_load` / `__raw_store`, render arm); ~`emit/gc.rs` (arm `26`); ~`emit/render.rs`; +`crates/jsc-core/tests/data_view.rs` | `DataView*` |
| **T3b** TypedArray methods | every `%TypedArray.prototype%` method of §5.8.7 not in T3a | ~`emit/realm.rs` (`TypedArrayOp` methods); +`emit/builtins/typed_array_methods.rs`; ~`emit/builtins/array.rs` (expose the merge sort and `toLocaleString` / `join` element steps for reuse, no behaviour change); ~`emit/builtins/mod.rs`; ~`emit/binary.rs` (bulk copy helpers if needed); +`crates/jsc-core/tests/typed_array_methods.rs` | `stable-typedarray-sort`, `align-detached-buffer-semantics-with-web-reality` |
| **T5a** Float16 + base64 / hex | kind `7` (`Float16Array`), `__f16_encode` / `__f16_decode`, `Math.f16round`, `DataView` `getFloat16` / `setFloat16`, `Uint8Array.fromBase64` / `fromHex` / `toBase64` / `toHex` / `setFromBase64` / `setFromHex` | ~`ir.rs`, ~`emit/realm.rs` (Float16 intrinsics, the `Math` entry, the base64 / hex entries); ~`emit/binary.rs`; ~`emit/builtins/typed_array.rs`, ~`emit/builtins/data_view.rs`; ~`emit/sections.rs` (`Float16Array` in `BINARY_NAMES`; `f16round` stays out of `program_has_binary`); ~the `Math` builtin dispatch (`emit/builtins/namespace.rs` / `emit/numeric.rs` as `MathOp` dictates); +`emit/builtins/base64.rs`; +`crates/jsc-core/tests/float16.rs`, +`crates/jsc-core/tests/base64_hex.rs` | `Float16Array`, `uint8array-base64` |
| **T5b** BigInt kinds | kinds `10` / `11`, content-type checks, `DataView` BigInt pairs; after the BigInt workstream lands | ~`ir.rs`, ~`emit/realm.rs`, ~`emit/binary.rs` (BigInt arms of `__raw_load` / `__raw_store`, `ToBigInt64` / `ToBigUint64`), ~`emit/builtins/typed_array.rs`, `typed_array_methods.rs`, `data_view.rs`; +`crates/jsc-core/tests/bigint_typed_arrays.rs` | none of its own (`BigInt` belongs to the BigInt workstream; its typed-array tests run once that line is gone) |
| **T6** closeout | move every remaining fact out of this section into §5.1 / §5.2 / §5.7 / §6.2 / §6.3 / §6.4 / §6.9 / §7.9, delete this section, update §13 (delete the "typed arrays, `ArrayBuffer`" words of the not-available line, keep §5.8.9 as limitations), recount §10 and the §13 failure table from CI | `DESIGN.md`, `tests/test262/` baseline files (from CI) | none |

---

## 6. Runtime

### 6.1 Host imports

Every module imports exactly four functions, all from
`wasi_snapshot_preview1` (`abi::WASI_MODULE`), declared in this order:

| # | import | signature | backs |
|---|---|---|---|
| 0 | `fd_write` | `(fd: i32, iovs: i32, iovs_len: i32, nwritten: i32) -> i32` | `console.log`, the `Uncaught …` reports, the completion line, the out-of-memory line |
| 1 | `clock_time_get` | `(id: i32, precision: i64, time: i32) -> i32` | `Date.now()`, `new Date()`, `Date()` (the guest `__now`), and the `Math.random` seed when `random_get` fails |
| 2 | `random_get` | `(buf: i32, buf_len: i32) -> i32` | the `Math.random` seed |
| 3 | `proc_exit` | `(code: i32) -> ()` | exit code `1` after a report or a failed write |

The module is self-contained: it runs under any WASI preview-1 host. The
native host is `run/harness.rs` (four hand-written `Linker::func_wrap`
imports, no WASI crate); the browser and Node host of `@cdlab/jsc` is
`packages/jsc/src/wasi.ts`; `wasmtime run` and `node:wasi` run a `jsc build`
artifact directly.

### 6.2 Guest runtime helpers

Everything else runs in wasm, emitted per module by `emit/`. The helpers, in
function-index order:

| group | helpers |
|---|---|
| array search | `__array_search(recv, needle, includes)`: the guest scan behind the `indexOf` / `includes` fast path; `SameValueZero` over the `TYPE_ELEMS` store and a sparse array's index keys, without allocating |
| allocation, strings | `__alloc`, `__concat`, `__str_eq`, `__str_cmp`, `__str_index` (all on code units, below) |
| interning (§5.3) | `__str_hash`, `__intern_slot`, `__intern_find`, `__intern`, `__index_value`, `__index_string`, `__str_exotic_index(o, key)`, `__to_key` |
| property tables (§6.3) | `__prop_find(o, key)`, `__define(o, key, v, attrs)`, `__fn_materialize(o)`, `__get(recv, key)`, `__set(recv, key, v)`, `__has(key, obj)`, `__delete(obj, key)`, `__own_keys(obj, mode)`, `__for_in_keys(obj)`, `__copy_data(dst, src, excl)` |
| arrays (§5.4) | `__array_reserve(base, need)`, `__array_push(base, v)`, `__array_concat(dst, src)`, `__array_set(recv, key, v)`, `__array_set_length(arr, v)`, `__sparse_get(arr, i)`, `__sparse_set(arr, i, v)`, `__protos_indexless(o)`, `__proto_elem(o, i)`, `__proto_set(o, i, v)`, `__array_define(recv, key, v)`, `__sparse_define(arr, i, v)` |
| JSON (stringify) | `__json_escape(str)`, `__json_str(v)` — the primitive serializer: finite numbers via `__prim_str`, `NaN`/`±Infinity`/`undefined` as `null`, keywords, escaped strings, any heap kind `null` |
| Map/Set, equality | `__same_value_zero`, `__map_hash`, `__map_reindex`, `__map_find`, `__map_set`, `__map_get`, `__map_has`, `__map_delete`, `__loose_eq`, `__str_to_array` |
| JSON (parse) | `__json_ws`, `__json_throw`, `__json_string`, `__json_number`, `__json_value`, `__json_array`, `__json_object`, `__json_parse` |
| number to string (§6.5) | `__umul128`, `__ryu_pow5`, `__ryu_mul_shift`, `__ryu_mp5`, `__ryu_d2d(bits)` (Ryu shortest digits), `__scratch_str(ptr, len)`, `__num_to_str(bits)` (`Number::toString`), `__prim_str(v)` (`ToString` of a primitive), `__msg_str(v)` (the text of any value in an error message), `__dec_expand(bits)` (the exact decimal expansion), `__num_format(bits, mode, f)` (`toFixed` / `toExponential` / `toPrecision`), `__num_radix(bits, radix)` |
| string to number (§6.5) | `__num_unit`, `__str_ws`, `__dec_parse(data, start, end)`, `__dec_slow`, `__dec_round`, `__dec_left`, `__dec_right`, `__digit_span`, `__radix_pow2`, `__radix_fold`, `__string_to_number(s)`, `__to_number_prim(v)`, `__parse_float(v)`, `__parse_int(v, radix)` |
| clock, random | `__now()`, `__random()` (§6.5) |
| BigInt (§6.10) | `__bigint_new(len, neg)`, `__bigint_trim(p)`, `__bigint_eq(a, b)`, `__bigint_cmp_mag(a, b)`, `__bigint_cmp(a, b)`, `__bigint_cmp_num(a, x)`, `__bigint_hash(p)`, `__bigint_to_string(v, radix)`, `__string_to_bigint(s)`, `__bigint_to_f64(p)`, `__bigint_from_f64(x)`, `__to_numeric(v)`, `__to_bigint(v)`, `__bigint_loose_eq(a, b)`, `__number_value(v)`, `__bigint_as_n(bits, v, signed)`, `__bigint_to_i64(p)`, `__bigint_from_i64(v, signed)`, `__bigint_add(a, b)`, `__bigint_sub(a, b)`, `__bigint_mul(a, b)`, `__bigint_divmod(a, b, want_rem)`, `__bigint_pow(a, b)`, `__bigint_neg(a)`, `__bigint_mag_step(p, inc, neg)`, `__bigint_add_small(v, d)`, `__bigint_not(a)`, `__bigint_bitop(a, b, op)`, `__bigint_shift(a, n, right)`, `__numeric_op(a, b, op)`, `__numeric_unary(v, op)` (the last group, after the extra helpers) |
| misc | `__gen_to_array(gen)`, `__nullish_throw(recv, key, set)`, `__prop_throw(recv, key, mode)`, `__box_f64(x)`, `__to_number(v)` |
| trampolines | `__invoke1(fn, arg)` (the promise jobs' callback trampoline), `__async_step(gen, sent)` (the `async` driver's step trampoline), the two `new Promise` settlers |
| constructors | `new_string(len, two_byte)`, `__new_array(len)` |
| realm | `__has_own(key, obj)`, then the builtin bodies (§6.4) |
| conversions (§6.5) | `__to_primitive(v, hint)`, `__to_string(v)`, `__add(a, b)`, `__compare(a, b, op)`, `__array_join(recv, sep)`, `__to_object(v)` |
| JSON (§7.9) | `__json_rep(holder, key, value, replacer, list, stack)` — `SerializeJSONProperty` (`toJSON`, replacer function with `this = holder`, property list, wrapper unwrapping, cycle `TypeError` via the `stack` array); `__json_indent(s, gap)` — re-indent compact text for `space`; `__json_revive(holder, key, reviver)` — `InternalizeJSONProperty` |
| descriptors (§6.3) | `__define_own(o, key, desc, value, get, set)`, `__to_desc(attrs, list)`, `__own_desc(obj, key)`, `__set_integrity(obj, level)`, `__test_integrity(obj, level)` |
| promises (§6.7) | the self-resolution `TypeError` constructor `() -> i64` that `__promise_settle` rejects with |
| well-known symbols | `__symbol_get(v, sym)` — `Get(v, sym)`, looking only in the tables on `v`'s chain that have `HAS_SYMBOL_KEYS` (`undefined` when none does; the well-known symbol consult sites, §6.4); an accessor hit always calls its getter, even in a program compiled without the accessor dispatch of `__get`, since builtins alone read the realm's `get [@@species]` and `%Iterator.prototype%`'s `get [@@toStringTag]` |
| `super` references (§6.3) | `__super_get(home, key, recv)`, `__super_set(home, key, v, recv)` |
| proxies (§6.11, phase 5b), appended after the collector's | `__proxy_trap(p, name)`, `__proxy_get_proto`, `__proxy_set_proto`, `__proxy_is_extensible`, `__proxy_prevent_ext`, `__proxy_own_prop`, `__proxy_define_own`, `__proxy_has`, `__proxy_get`, `__proxy_set`, `__proxy_delete`, `__proxy_own_keys`; the generic `__get_proto(v)`, `__own_prop(obj, key, list)`, `__desc_compatible(ext, d, cur)`, `__is_array(v)`, `__set_bool(target, key, v, recv)` |
| private names (§7.6) | `__private_find(o, key)`, `__private_add(o, key, v, attrs, name)`, `__private_get(o, key, name)`, `__private_set(o, key, v, name)`, `__private_brand(o, v, brand, name, mode)`, `__private_in(o, brand, name)` |
| output (§6.9) | `__out_reserve(need)`, `__path_reserve(need)`, `__refs_reserve(need)`, `__out_byte(b)`, `__out_cp(cp)`, `__out_str(s)`, `__out_uint(n)`, `__out_flush(fd)`, `__report(fd, prefix, v)` |
| renderer (§6.9) | `__render_top(v)`, `__render(v, depth)`, `__render_heap(p, depth)`, `__render_array(p, depth)`, `__render_object(p, depth)`, `__render_function(p, depth)`, `__render_props(..)`, `__render_entry(e, quote, first, open, depth)`, `__render_key(key, quote)`, `__render_lookup(o, key)`, `__heap_object(v)`, `__str_of(v)`, `__fn_name(v)`, `__date_text(v)` |
| entry | `_start` (§4.2) |
| promise engine (§6.7), after the Unicode and libm helpers | `__promise_new(proto)`, `__promise_enqueue(rec)`, `__promise_react(p, on_f, on_r, result, kind)`, `__promise_settle(p, state, v)`, `__promise_then(p, on_f, on_r)`, `__promise_resolve(v)`, `__promise_reject(v)`, `__promise_finally(p, on_finally)`, `__run_job(rec)`, `__drain()`, `__first_unhandled()`, `__promise_await(v)`, `__promise_resolving(p)`, `__async_spawn(gen)`, `__async_drive(gen, result, sent)` |
| allocator and collector (§5.6, §5.7), after the promise engine | `__alloc_small(class)`, `__gc_take_pages(k)`, `__gc_release_pages(base, k)`, `__gc_sweep()`, `__gc_collect()`, `__gc_visit(ref)`, `__gc_trace(block)`, `__gc_drain()`, `__alloc_fixed(size)` |

The string helpers work on code units at either width: `__concat` makes a
two-byte result iff either operand is two-byte (widening the one-byte side)
and returns the other operand when one is empty; `__str_eq` is pointer
equality first, then different widths or lengths are unequal, then an 8-byte
compare; `__str_cmp` is code-unit order (mixed widths read each side at its
width); `__str_index` is the fresh one-unit string at an index;
`__str_to_array` splits by code point (surrogate pairs kept together);
`__json_escape` writes a lone surrogate as `\udxxx` (lowercase hex). Every
helper that builds a string narrows a two-byte result whose units are all
`<= 0xFF` (§5.2).

A helper is a non-allocating leaf, and so not a safepoint for its callers,
only when `emit/roots.rs::helper_graph` declares it and all its callees are
leaves; a test checks each declaration against the emitted calls. `__prop_find`
and `__private_find` are not leaves, because a `LAZY` function materializes on
lookup (§5.5).

`__to_number(v)` runs `ToPrimitive` (hint number), then `__to_number_prim` on
the primitive; numeric call sites skip it when the operand already
is a number; a symbol throws `TypeError`. `__to_object(v)` wraps a string,
symbol, number, or boolean in a fresh wrapper object (§5.2) and returns any
object unchanged.

### 6.3 Property algorithms

`__get`, `__set`, `__has`, `__delete`, `__define`, `__own_keys`,
`__for_in_keys`, `__copy_data`, and the descriptor helpers (`emit/props.rs`,
`emit/props/descriptor.rs`) are the only code that reads or writes property
tables. Keys are interned `u32`s. Each of them has a proxy arm
(§6.11, phase 5b): a proxy as the object or anywhere on a prototype chain
hands the operation to its traps.

- **`__get(recv, key)`** — `OrdinaryGet`. Each object on the chain (`recv`,
  then each `proto`) answers from its exotic own keys first when it is a
  string, a String wrapper or an array (`length`, a present in-range index —
  a string's and a wrapper's by code unit, §5.3; a hole or an index past
  `length` is not own and reads on through the chain), then from its own
  table (a sparse element past the store is a table entry); an accessor
  calls its getter with `this = recv` (`undefined` without a getter). A string, number, or boolean
  continues at its realm prototype without allocating a wrapper (a symbol at
  `%Symbol.prototype%`);
  `null`/`undefined` read `undefined` (member-access sites throw the nullish
  `TypeError` before calling it).
- **`__set(recv, key, v)`** — strict `OrdinarySet`. The first own-or-inherited
  hit decides: an accessor calls its setter with `this = recv` (a getter-only
  accessor throws `Cannot set property k of object which has only a getter`;
  with `K = 0` the value is passed in a one-element spread argument array, so
  a builtin setter such as `__proto__`'s still receives it),
  a non-writable data property throws, an own writable one is overwritten;
  otherwise a `0b0111` own data property is added (a non-extensible object
  throws). An array element the array does not own consults the chain
  through `__proto_set` first (§5.4). Member stores on a primitive (a symbol included) throw (`Cannot create
  property 'k' on string 'abc'`); on `null`/`undefined` they throw `Cannot set properties of
  undefined (setting 'k')`.
- **`__super_get(home, key, recv)` / `__super_set(home, key, v, recv)`** —
  `home.[[Get]](key, recv)` / `home.[[Set]](key, v, recv)` for a `super`
  reference (§7.6): the walk of `__get` / `__set` starts at the super base
  `home` (the home object's prototype, read at the reference), and a getter or setter found on the way is called with
  `this = recv`. A setter-less accessor or a non-writable data property on
  the chain throws; a writable data property or a miss writes the receiver
  (`OrdinarySetWithOwnDescriptor`): a primitive `recv` throws, an own
  accessor or read-only own property of `recv` (a `String` wrapper's index,
  a read-only array `length`) throws, an own writable table property takes
  the value in place, an exotic own key (an array element or `length`) is
  updated by `__define_own` of `{value}`, and a missing key is created
  (`CreateDataProperty`; a non-extensible `recv` throws).
- **`__has(key, obj)`** — `HasProperty` (backs `in`): at each object of the
  chain its exotic own keys (a hole is not own), then its own table. A primitive right-hand side
  throws a `TypeError`. `__has_own` is the same without the chain
  (`hasOwnProperty`).
- **`__delete(obj, key)`** — tombstones an own entry; a non-configurable
  property throws. On an array an in-range index becomes a hole and `length`
  throws.
- **`__define(o, key, v, attrs)`** — `CreateDataProperty` /
  `DefinePropertyOrThrow` of a complete descriptor: overwrite in place or
  append. Redefining a non-configurable property throws `Cannot redefine
  property: k`; adding to a non-extensible object throws `Cannot define
  property k, object is not extensible`. Used by literals, class definitions,
  field initializers (a class field propagates the throw), and builtins.
- **Descriptors.** A descriptor record is four jsvals `[desc, value, get,
  set]`: `desc` packs the `W`/`E`/`C` values in their `attrs` bits plus
  presence bits (`0x100` writable, `0x200` enumerable, `0x400` configurable,
  `0x800` value, `0x1000` get, `0x2000` set); an absent `value`/`get`/`set` is
  `undefined`. `__to_desc(attrs, list)` is `ToPropertyDescriptor` (a
  non-object throws; fields are read in spec order through `__has` + `__get`,
  so inherited fields and getters count; `get`/`set` must be callable or
  `undefined`; mixing accessor and data fields throws) and appends the record
  to the array `list`. `__define_own(o, key, desc, value, get, set)` is
  `ValidateAndApplyPropertyDescriptor`: it returns ok, rejected
  (non-configurable rules), or not extensible, and the caller raises the
  `TypeError`; only an array's `length` (`ArraySetLength`, §5.4) may leave an
  exception pending instead. On a non-`SPARSE` array an index whose resulting
  attributes are the default ones is an element store; any other index puts
  the array in dictionary mode (§5.4) and is an ordinary table property (a new
  one at or past `length` rejects on a read-only `length` and raises `length`
  otherwise). A kind change on a configurable property
  keeps `E`/`C` and resets the rest; an accessor update allocates a fresh
  `TYPE_ACCESSOR` block. `__own_desc(obj, key)` is `FromPropertyDescriptor`
  of `[[GetOwnProperty]]` (a fresh object with `value, writable` or `get,
  set`, then `enumerable, configurable`; an array's `length` is
  non-enumerable, non-configurable, and writable unless `LENGTH_READONLY`;
  store elements have the default attributes, table elements their own).
- **Integrity levels.** `__set_integrity(obj, level)` (`0` not extensible,
  `1` sealed, `2` frozen) clears `C` on every own property (and `W` on data
  properties when frozen), then sets `NOT_EXTENSIBLE`; a `LAZY` function is
  materialized first, and an array is put in dictionary mode first (so the
  level reaches its elements; freezing also sets `LENGTH_READONLY`).
  `__test_integrity(obj, level)` is `IsExtensible` (level `0`) or
  `TestIntegrityLevel` (a frozen array also needs a read-only `length`).
  Both ignore private-name entries (§7.6, phase 5a).
- **Prototypes.** `__set_proto(o, proto)` is `OrdinarySetPrototypeOf`: the
  same `proto` succeeds; `%Object.prototype%` (an immutable prototype exotic
  object), a non-extensible object, and a `proto` whose chain reaches `o`
  are rejected (it returns the `__prop_throw` mode; the caller throws); a
  watched intrinsic sets its dirty bit. `__own_enum(obj, key)` tests for an
  own enumerable property (an array's `length` is not enumerable; a
  dense array's present elements are, a dictionary-mode element has its
  own attributes), backing `Object.assign` and
  `propertyIsEnumerable`. `Object.prototype.__proto__` is an accessor pair
  over the same helpers: the getter reads the `proto` of `ToObject(this)`;
  the setter (`RequireObjectCoercible(this)`; a primitive `this` or a value
  that is neither an object nor `null` is ignored) calls `__set_proto` and
  throws on a rejection.
- **Own-key order** (`__own_keys(obj, mode)`, behind `Object.keys/values/entries`,
  `JSON.stringify`, `for-in`, object rest, the `Object` statics, and the
  renderer): integer keys ascending (array elements skipping holes, then
  table keys flagged `ARRAY_INDEX` sorted numerically when the table has
  `HAS_INDEX_KEYS`), then the other string keys in insertion order, then — in
  `OWN_ALL_KEYS` only — the symbol keys in insertion order. `mode` selects
  enumerable string keys, values, entries, all string keys (`OWN_ALL`,
  `getOwnPropertyNames`), all symbol keys (`OWN_SYMBOLS`,
  `getOwnPropertySymbols`), or every key (`OWN_ALL_KEYS`: object rest,
  `Object.assign`, `defineProperties` / `create`,
  `getOwnPropertyDescriptors`, which check enumerability per key); every
  string-key mode skips symbol keys, and the symbol walks are skipped when the
  table lacks `HAS_SYMBOL_KEYS` (and, from phase 5a, skip private names,
  §7.6). Values and entries follow EnumerableOwnProperties: the key list is
  taken once, then each key is re-checked as own and enumerable (`__own_enum`)
  when visited and skipped if a getter deleted it or made it non-enumerable;
  a key a getter adds is not visited. Only a getter can run between the
  snapshot and a visit, so the re-check (and the `[[Get]]` of every key) runs
  only when the own table holds an accessor entry; otherwise array elements
  load from the store directly.
- **`for-in`** (`__for_in_keys`): a snapshot taken at loop entry — own
  enumerable string keys, then each prototype's enumerable keys not already
  seen (a key seen at a nearer level, enumerable or not, shadows it).
- **`CopyDataProperties`** (`__copy_data`, object spread and object rest):
  defines (`CreateDataProperty`, never a setter) on the target every own
  enumerable property of the source (string keys in own-key order, then
  symbol keys; an object source re-checks each key as own and enumerable
  when visited) whose key is not in the exclusion list, the value read with
  `[[Get]]` (a getter runs). A nullish or non-string primitive source copies
  nothing; a string copies its indices.
- **Integer-indexed exotic objects** (typed arrays): every helper above has
  one arm for a `TYPE_TYPEDARRAY` object, emitted only in a program that can
  reach binary data, which answers canonical numeric string keys from the
  buffer and never lets them reach the table or the chain (§5.8.5).

### 6.4 Realm, global object, and fast paths

**Realm image.** Every intrinsic is a static block in the data segment, laid
out by `emit/realm.rs` from one registry of `(intrinsic, key, attrs, value)`
entries (`key` is an interned string or a well-known symbol; `value` is a
builtin method, a string, a number, `undefined`, another intrinsic, a
well-known symbol, or an accessor pair). Each intrinsic is an object-family
block (a `TYPE_FUNCTION` block when callable; `%Array.prototype%` is a
`TYPE_ARRAY` handle in dictionary mode, `length` 0 and no store, so it is an
Array exotic object whose element writes land in its watched table) followed
by a full `TYPE_PROPS` table (flagged `PROPS_HAS_SYMBOL_KEYS` when it holds a symbol key); then one
static `LAZY` `TYPE_FUNCTION` block per builtin method (`env = undefined`,
`func_idx` = its builtin table slot); then one static `TYPE_ACCESSOR` block
per accessor entry; then the 13 well-known symbols as static `TYPE_SYMBOL`
blocks (description `"Symbol.iterator"`, ..). Every intrinsic address is a
compile-time constant and the realm costs nothing at startup; user mutation
works like on any object.

Intrinsics, in image order: `%Object.prototype%` (`proto` `null`),
`%Function.prototype%` (itself callable, returns `undefined`), the seven error
prototypes (`Error`, `TypeError`, `RangeError`, `ReferenceError`,
`SyntaxError`, `EvalError`, `URIError`), the global object
(`CLASS_GLOBAL`), `Math`, `JSON`, `console`, the constructors (`Object`,
`Function`, `Array`, `String`, `Number`, `Boolean`, the seven error
constructors, `Map`, `Set`, `Promise`, `Date`, `Symbol`, `Iterator`; native
error constructors inherit from `Error`, the rest from `%Function.prototype%`),
the prototypes of `Array`, `String`, `Number`, `Boolean`, `Map`, `Set`,
`Promise`, `Date`, `Symbol`, `%IteratorPrototype%` (`Iterator.prototype`:
`[@@iterator]() { return this }`, and `constructor` / `@@toStringTag`
accessors),
`%ArrayIteratorPrototype%`, `%StringIteratorPrototype%`,
`%MapIteratorPrototype%`, `%SetIteratorPrototype%` (each a `next`,
inheriting `%IteratorPrototype%`), `%AsyncIteratorPrototype%`
(`[@@asyncIterator]() { return this }`), `%GeneratorPrototype%` (inheriting
`%IteratorPrototype%`), `%AsyncGeneratorPrototype%` (inheriting
`%AsyncIteratorPrototype%`), and the
hidden `Symbol.for` registry (`proto` `null`, unreachable from the global
object; its table maps interned keys to the registered symbols),
`Array.prototype[@@unscopables]` (`proto` `null`),
`%WrapForValidIteratorPrototype%` and `%IteratorHelperPrototype%` (each a
`next` / `return`, inheriting `%IteratorPrototype%`; the latter's
`@@toStringTag` is `"Iterator Helper"`), and the hidden `%OutOfMemoryError%`
(§5.6). Each
constructor owns `length`, `name`, a
non-writable `prototype`, and its statics (`%Array%`, `%Map%`, `%Set%` and
`%Promise%` also `get [@@species]`); each prototype owns a
`constructor` back-link. Builtin methods are writable, configurable, and not
enumerable.

**Global object.** Own properties: `globalThis`, `Infinity`, `NaN`,
`undefined` (non-writable, non-enumerable, non-configurable), `Math`, `JSON`,
`console`, every constructor above, `isNaN`, `isFinite`. Top-level `this` is
the global object.

**Global declarations.** Top-level `var`s and function declarations are
global-object properties. A program that never observes the global object —
no `globalThis` in any form and no top-level `this` — cannot tell, so they
stay `main` bindings and module cells (§7.1) at no cost. Otherwise
(also when a top-level `var` and function share a name) `main`'s prologue
runs GlobalDeclarationInstantiation before any statement
(`IrNode::GlobalDeclare`): each function declaration defines
`{value: f, writable, enumerable, non-configurable}` (CreateGlobalFunctionBinding;
a non-configurable existing property such as `NaN` throws
`TypeError: Cannot redefine property: NaN`), then each `var` not already an
own property defines `{undefined, writable, enumerable, non-configurable}`
(CreateGlobalVarBinding). The property is then the only storage: a `var`
is not a binding at all, so every reference — from `main` or any function
— resolves to the global object like an unresolved name (`GlobalGet` /
`GlobalSet`), and `var q = 1; globalThis.q = 2; q` is `2`. These names count
as assigned for `sealed` (so `var parseInt = ..` takes the generic global
path). A function declaration keeps its static identity (direct `call`,
static function object) while its name is `sealed`; once the global object
is open or the name is assigned (`f = ..`), reads and calls go through the
property. Top-level `let` / `const` / `class` are never global properties.

**Global name resolution.** Lexical scopes → top-level declarations (module
cells) → the global object. A read of a `sealed` global compiles to the
intrinsic's constant address; any other unresolved read is `HasProperty` +
`Get` on the global object at run time, throwing `ReferenceError: x is not
defined` on a miss (`typeof x` yields `"undefined"` instead). Assigning to an
unresolvable name throws `ReferenceError` (strict).

**Global facts** (`codegen/globals.rs`, one whole-program syntactic scan):

- **`sealed(N)`** — `N` names an unshadowed global, is never assigned, and the
  program contains no escaping `globalThis` (anything but a static
  `globalThis.k` read or call — a member write escapes), top-level `this`, or
  `delete` of an identifier. A namespace (`Math`, `JSON`, `console`, `Object`, `Promise`,
  `Date`) is additionally **inline** when it never escapes (only `N.k` reads
  and `N.m(..)` calls); then its members compile to inline code.
- **intrinsics-sealed** — none of `prototype`, `constructor`, `__proto__`,
  `getPrototypeOf`, `setPrototypeOf`, `defineProperty`, `defineProperties`,
  `assign`, `Reflect` occurs as an identifier, member or property name, or
  string literal (a class's own `constructor` excepted). Then no builtin
  prototype can be reached, and the dirty-bit tests below fold away.
- **`reaches_builtins`** — some builtin namespace member or constructor can be
  called through its value; then `K` covers the widest call (§4.2).

**Dirty bits.** `$__intrinsics_dirty` has one bit per watched intrinsic, set by
`__set` / `__define` / `__delete` / `__define_own` / `__set_integrity` /
`__set_proto` on a
table flagged `WATCHED` (the table's
byte 3 holds the bit index) when the program tests any bit; bits are never
cleared. Bits: `0` `%Object.prototype%`, `1` `%Array.prototype%`, `2`
`%String.prototype%`, `3` `%Number.prototype%`, `4` `%Boolean.prototype%`,
`5` `%Function.prototype%`, `6` `%Promise.prototype%`, `7` `%Map.prototype%`,
`8` `%Set.prototype%`, `9` `%Date.prototype%`, `10` `Math`, `11` `JSON`,
`12` `console`, `13` `%GeneratorPrototype%`, `14` `%Symbol.prototype%`,
`15` `%IteratorPrototype%`, `16` `%ArrayIteratorPrototype%`, `17`
`%StringIteratorPrototype%`, `18` `%MapIteratorPrototype%`, `19`
`%SetIteratorPrototype%`, `20` `%Array%`, `21` `%Promise%` (their
`@@species`).

**Well-known symbols.** The 13 `Symbol.*` statics are the static realm
blocks above; `Symbol` is an ordinary global, so `Symbol.iterator` is a
`__get` on `%Symbol%` and a user value is matched by identity. Builtins and
codegen read a hook with `__symbol_get` (§6.2), so an object chain that never
held a symbol key costs one `HAS_SYMBOL_KEYS` test per level:

| symbol | consulted by |
|---|---|
| `@@iterator` | `GetIterator` (§7.8): `for-of`, spread, array destructuring, `yield*`, `Array.from`, the Map/Set constructors, `Object.fromEntries`, `AggregateError`'s errors, the `Promise` combinators, `Iterator.from` and `flatMap`, `Math.sumPrecise`, the async-from-sync fallback |
| `@@asyncIterator` | the async `GetIterator` (§7.8): `for await`, `yield*` in an async generator, `Array.fromAsync` |
| `@@hasInstance` | `instanceof` before `OrdinaryHasInstance` (§7.6) |
| `@@toPrimitive` | `ToPrimitive` (§6.5) |
| `@@toStringTag` | `Object.prototype.toString` (§7.9) |
| `@@species` | `ArraySpeciesCreate` (§7.9), `SpeciesConstructor` in `Promise.prototype.then` / `finally` (§6.7) |
| `@@isConcatSpreadable` | `Array.prototype.concat` (§7.9) |
| `@@match`, `@@matchAll`, `@@replace`, `@@search`, `@@split` | the String methods given an object argument, and `IsRegExp` (§7.9) |
| `@@unscopables` | nothing (`with` is rejected); `Array.prototype[@@unscopables]` exists |

**Guarded fast paths.** Specializations are optimizations over the generic
path, never semantics:

| site | fast path | taken when | otherwise |
|---|---|---|---|
| `f(..)` on a top-level declaration | direct `call` | declarations are `main` bindings (not assignable), or `sealed(f)` | an ordinary call of the global property |
| `String/Number/Boolean/parseInt/parseFloat(..)`, `Error(..)` / `new Error(..)` (each native error) | inline conversion / Error instance (a spread `parseInt(...a)` / `parseFloat(...a)` converts the array's first elements) | `sealed(N)` (compile time, no run-time cost) | an ordinary call / construct of the global property (a `ReferenceError` once it is deleted) |
| `N.m(..)` / `N.k` on an inline namespace | inline code (`MathCall`, `ConsoleLog`, JSON, `Object.keys`, ...) | always | — |
| `N.m(..)` on a non-inline namespace (`NamespaceGuard`) | the same inline code | watched namespace: `N === %N%` and its dirty bit clear; constructor namespace: `N.m` is the builtin method | the generic method call |
| `recv.m(..)` with a builtin prototype method name (`ProtoGuard`) | inline per receiver family (Array, Map, Set, Map-or-Set, Promise, Date, Generator) | the kind byte matches, `props == 0` (an array also not `DICTIONARY`), `proto` is the family's realm prototype, and (unless intrinsics-sealed) its dirty bit is clear (an array also needs `%Object.prototype%`'s, since its fast paths read a hole as absent and append without consulting the chain); `push(x)` also needs `length < 2^32 - 1` (the builtin throws the `RangeError`); `map` / `filter` (a plain array) and `then` / `catch` / `finally` (a `%Promise%`) also need `%Array%`'s / `%Promise%`'s bit clear, even when intrinsics-sealed (the global constructor is always reachable), so such a site turns dirty tracking on | a string receiver whose key names a `String.prototype` builtin calls it directly (its dirty bit clear); else one call of the shared out-of-line `MethodCall` builtin |
| `.length` | string `len @4`, array `len @16` | the kind byte | `__get(recv, "length")` |
| `GetIterator` (for-of, spread, destructuring, `Array.from`, `Object.fromEntries`, the Map/Set constructors) | a clean array / `arguments` walked by index, a string by code point, a `Map` / `Set` by backing entry, a generator by direct state-machine resumes; decided once per loop | the receiver test of `ProtoGuard` for its family (a generator: the `next` call site's, and its body has no `yield*`), plus clear bits for its iterator prototype and `%IteratorPrototype%` (a string: `%String.prototype%`'s); `arguments`: its own `@@iterator` is still the data property `%Array.prototype.values%`. Unless intrinsics-sealed, only with dirty tracking (a builtin body or a spread tests the bits only when the program tracks them; a spread source or an unsealed `GetIterator` turns tracking on) | the protocol: `GetMethod(@@iterator)`, its call, `next` read once (the internal `Protocol` builtins) |
| `x instanceof C` for an unshadowed, sealed `Array` / `Function` / `Date` / `Object` / Error-family `C` | a `proto` walk for the constructor's static prototype (`%Array.prototype%`, `%Function.prototype%`, `%Date.prototype%`, `%Object.prototype%`, an error prototype); the operand's direct `proto` is the first link, so a replaced `proto` is honoured | a run-time intact test: `C` and each constructor on its static chain up to `%Function.prototype%` (a native error: `%Error%`) has its realm `proto` and no own `@@hasInstance` (a table without `HAS_SYMBOL_KEYS` passes inline, else a `__prop_find` miss decides, so `%Array%`'s own `get [Symbol.species]` keeps the arm), so `GetMethod(C, @@hasInstance)` is the immutable `%Function.prototype%[@@hasInstance]` | `InstanceofOperator` (§7.6) on the realm constructor |
| `x instanceof C` otherwise | `InstanceofOperator` (§7.6) | — | — |
| `ArraySpeciesCreate` / `SpeciesConstructor` (`%Promise%`) in a builtin body | `ArrayCreate` / the promise engine helpers (§6.7), no `@@species` read | `constructor` is `%Array%` / `%Promise%` and its bit is clear, with dirty tracking | `Get(C, @@species)` |

The shared **`MethodCall`** builtin is the generic call for every guarded site:
`__get(recv, key)`, `IsCallable` (else `TypeError: recv.m is not a function`),
then a call with `this = recv`; it also throws for a nullish receiver. Its
`env` packs the interned key and the error text, so each site costs one
`call_indirect`. Array fast paths keep the `props == 0` and `proto` checks
even when the program is intrinsics-sealed: the sealed analysis does not cover
an own property shadowing a builtin (`a.push = f` is honoured).
§6.11 (phase 5b) lists which guards stay correct when a proxy is a
receiver or sits on a prototype chain, and the two (`__protos_indexless`,
`__symbol_get`) plus the `instanceof` walk that need a proxy test.

**Opcount gate.** `crates/jsc-core/tests/opcount.rs` compiles every program in
`tests/opcount/*.js` and compares, per program, the static operator count of
`main` and the user functions, the `call_indirect` / property-helper / WASI
import (`host_calls`) call sites, and the executed instruction count (wasmtime fuel) against
`tests/opcount/snapshot.tsv`. Any increase or decrease fails;
`JSC_OPCOUNT_UPDATE=1` rewrites the numbers and keeps each row's `reason`
cell, which records why a change was accepted.

### 6.5 Conversions

- **`ToPrimitive`** (`__to_primitive(v, hint)`) first looks up the object's
  `@@toPrimitive` (`GetMethod` through `__symbol_get`, one `HAS_SYMBOL_KEYS`
  test per chain level): it is called with the hint string (`"default"`,
  `"number"`, `"string"`); a non-callable method or an object result throws
  `TypeError`. `%Symbol.prototype%[@@toPrimitive]` and
  `%Date.prototype%[@@toPrimitive]` (hint default is string; generic over
  any object, any other hint throws) are recognised without a call. Without
  one (`undefined` / `null`) it is `OrdinaryToPrimitive`: it
  calls `valueOf` then `toString` (`toString` first for hint string) through
  `__get`, takes the first primitive result, and throws `TypeError` when
  neither yields one.
- **`ToString`** (`__to_string`): a string is itself; an object is converted
  with hint string first; a symbol throws `TypeError` (only a called
  `String(sym)` yields `"Symbol(desc)"`); the remaining primitives are
  formatted in the guest (`__prim_str`; numbers by `__num_to_str`, ECMAScript
  `Number::toString` digits from Ryu's shortest round-trip mantissa over its
  size-optimized tables). `Number.prototype.toFixed` / `toExponential` /
  `toPrecision` round the exact decimal expansion (`__dec_expand`, ties away
  from zero) and `toString(radix)` uses V8's `DoubleToRadixCString` digits;
  every formatter composes in the static `NUM_SCRATCH` buffer and copies into
  one Latin-1 string block.
- **StringToNumber, `parseFloat`, `parseInt`** (`__string_to_number`,
  `__parse_float`, `__parse_int`): guest parsers over code units. A decimal
  takes Clinger's exact fast path when it can, otherwise Simple Decimal
  Conversion over the static 768-digit `DEC_SCRATCH` buffer (a port of
  `dec2flt`'s `parse_long_mantissa`), so results are correctly rounded;
  radix input folds digit by digit.
- **Clock and random** (`__now`, `__random`): `__now()` calls
  `clock_time_get(0, 1_000_000, WASI_CLOCK)` and returns `f64(ns) / 1e6`, or
  `0` on errno; its callers apply TimeClip. `__random()` is `xorshift128+`
  over the i64 globals `$__rng0/$__rng1`, seeded on the first call from
  `random_get(WASI_SEED, 16)`, or from the clock nanoseconds through
  `splitmix64` when that fails; an all-zero seed is replaced by a fixed
  non-zero constant. The result is `f64::from_bits((state0 >> 12) | 0x3FF0…) - 1.0`,
  uniform in `[0, 1)`. Neither allocates: both use the static WASI scratch area.
- **`ToNumber`** (`__to_number`): §6.2.
- **`+`** (`__add`): once an operand is a heap value, both operands go through
  `ToPrimitive` (hint default), then concatenation if either is a string,
  else numeric addition (a symbol throws in either conversion).
- **Relational** (`__compare`): two numbers compare inline; otherwise
  `ToPrimitive` (hint number), then two strings compare in code-unit order
  (`__str_cmp`) and anything
  else by `ToNumber` (a symbol throws).
- **`Array.prototype.join` / `toString`** (`__array_join`): `ToString` of each
  element (`null`, `undefined`, and holes are empty); a cyclic element joins
  as `""` (the `JOINING` flag).

### 6.6 Exceptions (manual unwinding)

jsc does not use the wasm exception-handling proposal. Two exported globals
carry the in-flight exception: `$exc_pending` and `$exc_value`.

- `throw e` stores `e` in `$exc_value`, sets `$exc_pending = 1`, then branches to
  the innermost enclosing `try` handler label in the same function, or returns a
  dummy value from the function.
- After every call that can throw (direct, `call_indirect`, helpers that run user
  code), the caller checks `$exc_pending` and propagates the same way. Helpers
  that call user code (`__get` getters, `__to_primitive`, ...) leave the
  exception pending and return a harmless value.
- `try { B } catch (e) { H }` wraps `B` in a block whose label is pushed on the
  codegen handler stack; after the block, if `$exc_pending` is set it is cleared
  and `$exc_value` is bound to `e` in a fresh scope for `H`.
- `finally` runs on every exit edge — normal fall-through, throw, and any
  `return`/`break`/`continue` leaving the protected region. An abrupt exit is
  recorded in `$cf_kind`/`$cf_value` before the finalizer runs and resumed after
  it; a finalizer that itself completes abruptly overrides the pending action
  (`try { return 1 } finally { return 2 }` returns `2`). A pending exception is
  re-propagated after a normally completing `finally`.
- **Call-stack guard.** Every user function (and generator / async state
  machine) counts itself into `$__call_depth` on entry and out before each
  `return`. On entry, more than `abi::CALL_DEPTH_LIMIT` (10 000) active calls,
  or a shadow stack that cannot hold the function's locals plus
  `abi::SHADOW_STACK_MARGIN` (64 KiB), calls `__stack_overflow()`, which
  leaves a fresh `RangeError: Maximum call stack size exceeded` pending, and
  the function unwinds like any throw. The `jsc` runner gives wasmtime a
  64 MiB wasm stack so the guard fires before the host stack runs out.
- At the boundary `_start` reads `$exc_pending`; if set, it renders
  `$exc_value` as `Uncaught <value>` on fd 2 and calls `proc_exit(1)` (§4.2).

### 6.7 Promises, microtasks, and the async driver

The promise engine is guest code (`emit/promise_engine.rs`): the settlement,
the reaction lists, the microtask queue and the rejection tracking all live
in the guest heap and globals, and so does the `async` function driver; no
promise operation calls the host.

- **Records.** A reaction is one `TYPE_ENV` block of 6 jsval slots:
  `on_fulfilled`, `on_rejected` (`undefined` = passthrough), `result` (the
  derived promise or `undefined`), `kind` (bit 0 `THENABLE`, bit 1
  `ASYNC_RESUME`, bits 2-3 the source state once queued), the settlement
  value (written when queued) and
  `next` (`HEAP | record` or `0`). The same block is first a reaction on a
  pending promise (a newest-first list from the promise's `reactions` ref),
  then a queued job, so settling never allocates. A `THENABLE` record is a
  `PromiseResolveThenableJob` (`then`, the thenable, the promise to resolve),
  queued directly. An `ASYNC_RESUME` record is an `async` body's `await`
  continuation (the generator handle in slot 0, the result promise in slot
  2).
- **Queue.** `$__job_head` / `$__job_tail` are raw refs to an intrusive FIFO
  of records. `__promise_settle(p, state, v)` resolves or rejects `p`
  (below); settling reverses the reaction list in place into registration
  order, fills each record's state and value, and splices the chain onto the
  queue. `__promise_react(p, on_f, on_r, result, kind)` sets `HANDLED` and
  prepends a record to a pending promise, or queues it at once for a settled
  promise (a non-promise `p` acts as already fulfilled). `__promise_then`,
  `__promise_finally`, `__promise_resolve` and `__promise_reject` build on
  these; promises come from `__promise_new(proto)`, which stamps `seq` from
  `$__promise_seq`.
- **Jobs.** `__run_job(rec)` runs a `THENABLE` job as
  `then.call(thenable, resolve, reject)` with a fresh resolving-function pair
  for its promise (a throw calls that `reject`), and an `ASYNC_RESUME` job by
  resuming its body through `__async_drive` (below). A reaction job selects the
  callback by the source state; a non-callable one passes the settlement
  through. Otherwise it calls the internal `__invoke1(fn, arg)` trampoline
  (the uniform-ABI `call_indirect`). A throw leaves `$exc_pending` set; the
  job clears it and rejects the derived promise; a return resolves the
  derived promise with the callback's result.
- **Resolution** follows the spec's Promise Resolve Functions, for every
  resolution (an executor's `resolve`, a reaction's result, an `async` body's
  return, `Promise.resolve`): resolving a promise with itself rejects it with
  a fresh `TypeError` ("Chaining cycle detected for promise #<Promise>")
  built in the guest; a non-object fulfills; otherwise `Get(v, "then")` runs
  (a throw rejects with the thrown value), a callable `then` queues a
  `THENABLE` job — native promises included, so adopting a promise takes two
  extra ticks — and anything else fulfills. A resolving-function pair
  (`CreateResolvingFunctions`) is the two settler functions (slots `n`,
  `n + 1`) over one shared `env` from `__promise_resolving(p)`: a
  `TYPE_ENV` block `[p, alreadyResolved]`, so only the first call of either
  function does anything. `PromiseResolve(%Promise%, v)` is
  `__promise_resolve(v)`: a promise whose `constructor` is `%Promise%` is
  `v` itself (a throw of that read propagates), anything else a fresh promise
  resolved with `v`; every `await` goes through it.
- **Builtins.** `then` on a receiver that is not a promise throws a
  `TypeError`. `then` and `finally` take their result promise's constructor
  from `SpeciesConstructor(this, %Promise%)`: for `%Promise%` the engine
  helpers above; for any other `C` (a subclass, a user `@@species`) `then`
  builds `NewPromiseCapability(C)` (constructing `C` with an internal
  executor closure) and registers two internal reaction closures through
  `__promise_then` that settle the capability, and `finally` (also on any
  non-promise object) calls `this.then(thenFinally, catchFinally)`. Both
  paths use the spec's closures (`PromiseResolve(C, onFinally())` then a
  value / thrower thunk); for `%Promise%`, `__promise_finally` registers them
  through `__promise_then`. `Promise.resolve` / `Promise.reject` construct
  from `this`: `PromiseResolve(C, x)` (a promise whose `constructor` is `C`
  is returned as is) / `NewPromiseCapability(C)`; `C === %Promise%` takes
  `__promise_resolve` / `__promise_reject`, and so does the inline
  `Promise.resolve(x)` of a never-written `Promise` global. The combinators
  `Promise.all` / `allSettled` / `any` /
  `race` (`emit/builtins/promise.rs`) build `NewPromiseCapability(this)`,
  read `this.resolve` once, open the iterable's iterator record (§7.8) and
  per value call `resolve` with `this`, then `Invoke(next, "then", «onF,
  onR»)` with internal element closures (`Builtin::Species(SpeciesOp::Element)`,
  `env = [state, index, alreadyCalled]` over a shared `[values,
  remainingElements, settle]`). A throw closes the iterator unless it came
  from the iterator itself, then rejects the result promise. `this ===
  %Promise%` with its builtin `resolve`, its and `%Promise.prototype%`'s
  dirty bits clear and a `%Promise%` instance without own properties skips
  the `resolve` call and registers the reactions through `__promise_then`.
- **`new Promise(executor)`** allocates a pending promise `P`
  (`__promise_new`) and its resolving `env` (`__promise_resolving`), then
  passes the executor the two settler function values over that `env`. A
  throwing executor calls `reject` with the thrown value (a no-op once
  `alreadyResolved`).
- **Draining.** There are no timers or I/O, so the microtask queue is the
  only asynchronous source. `__drain()` is a loop (no recursion, so a long
  reaction chain uses no wasm stack): pop and run jobs until the queue is
  empty, then the checkpoint, and loop again if the checkpoint queued jobs.
  The checkpoint (1) prunes the promises a late reaction marked `HANDLED`
  from the rejected list; (2) `ClearKeptObjects`: empties the
  `KeepDuringJob` array `$__kept_objects` (length `0`, vacated slots
  `HOLE`), to which `new WeakRef(t)` and every successful `deref()` push
  their target; (3) runs at most one finalization cleanup job: it pops the
  head of `$__fin_jobs`, clears its `FINREG_FLAG_JOB_QUEUED`, and
  (`CleanupFinalizationRegistry`) turns each emptied cell unused and calls
  `cleanup(held)` through `__invoke1`, re-reading the cell store for every
  cell because a callback may `register` (growing the store) or
  `unregister`. A throw out of a cleanup callback is reported like an
  uncaught exception (`Uncaught …` on fd 2, exit 1). `__drain()` returns only
  when both the microtask queue and `$__fin_jobs` are empty. The checkpoints
  are host-agnostic (no timers, no host hook): a cleanup job runs only after
  a collection queued it and some later `__drain()` reaches a checkpoint.
  Top-level `await v` takes `PromiseResolve(%Promise%, v)`, marks that
  promise handled, drains, and yields the fulfillment value (a rejection
  surfaces as an uncaught throw). After `main` returns normally
  `_start` drains whatever remains, so unawaited `then` chains run. If `main`
  ends with a pending exception, the queue is not drained.
- **Unhandled rejections.** A rejection of a promise with no reaction and no
  `HANDLED` flag links it into the rejected list (`$__rejected_head`,
  `rejected_next`, flag `IN_REJECTED_LIST`). After the final drain,
  `__first_unhandled()` returns the unhandled promise with the smallest
  `seq` (creation order), and `_start` fails the run with
  `Uncaught (in promise) <value>` (exit 1); a late `catch` still counts as a
  handler.
- **`async` functions** (§7.8) are spawned by `__async_spawn(gen)`
  (`AsyncFunctionStart`): it allocates the result promise and calls
  `__async_drive(gen, result, undefined)`, which advances the body's state
  machine one suspend through the internal `__async_step(gen, sent)`
  trampoline. A throw out of the body (the pending exception, cleared)
  rejects the result promise; completion resolves it with the returned
  value; otherwise the body suspended on `await v`, and
  `PromiseResolve(%Promise%, v)` gets an `ASYNC_RESUME` reaction (no derived
  promise). When it settles, `__run_job` calls `__async_drive` again with
  the value (fulfilled) or with the reason pending as an exception
  (rejected), which throws at the `await`. An abrupt `PromiseResolve` (a
  throwing `constructor` getter) is thrown into the body at once by the same
  loop. No wasm frame is held across an `await`: every resumption starts
  from the drain loop.
- **Async generators** (§7.8): their request queue and driver are guest
  builtins that allocate (`__promise_new`) and settle (`__promise_settle`)
  each request's promise; every `await` (of the body, of a `yield` or
  `return` operand, of `return(v)`) takes `PromiseResolve(%Promise%, v)` and
  registers its continuation pair with `__promise_react` (no derived
  promise). An abrupt `PromiseResolve` throws into the body at the `await`
  (for `return(v)` on a completed generator it rejects the request).

### 6.8 Host boundary

The boundary is the four WASI imports (§6.1) and the two exports `_start` and
`memory` (§4.2), nothing else. The host calls `_start` once; the guest calls
the host only through the imports, which read or write plain byte buffers
(iovecs, the clock result, the random seed) in the static WASI scratch area
and never call back into the guest, so there is no re-entry. No host reads a
heap layout, a global, or a jsval: console output, the completion line and
the `Uncaught …` reports are UTF-8 the guest writes through `fd_write`
(§6.9); exit codes are `proc_exit` (§9). Promises, the microtask queue and
`async` continuations live entirely in the guest (§6.7), so the host holds no
jsvals. Layout knowledge lives in the emitter and `abi.rs` alone.

### 6.9 Value rendering

The guest renderer (`emit/render.rs`) appends the Node-console form of a value
to the output buffer (`$__out_buf`) as UTF-8. It runs no user code and never
throws. `console.log` evaluates every argument before printing anything (a
throwing argument prints nothing; each value renders in its state after the
last argument ran): with every argument past the first a literal, static block
or plain local read, they stream straight into the buffer; otherwise they are
first collected into an array literal and printed like the spread form. It
joins its arguments with one space and ends the line with
one `fd_write` to fd 1; `__report` writes one
`prefix + value + "\n"` line to its fd (the `Uncaught …` reports and the `-p`
completion, §4.2). A failed or stalled write ends the program with
`proc_exit(1)`. The buffer, recursion path and ref table are `TYPE_RAW`
blocks from `__alloc_fixed`: an out-of-memory while they grow hands a block
from the reserve and leaves the `RangeError` pending for the `console.log`
call site's check (§5.6). The rules:

- numbers by the ECMAScript Number::toString rules, through `__num_to_str`
  (shortest round-trip digits, exponential form at `>= 1e21` and `< 1e-6`,
  `-0` prints `0`); strings unquoted, also inside containers, with a lone
  surrogate written as U+FFFD (what Node writes); symbols `Symbol(a)` / `Symbol()`;
  BigInts as their decimal digits and `n` (`-12n`, §6.10.2);
- arrays `[ 1, <1 empty item>, 3, foo: 1 ]` (holes as runs, then own named
  properties); objects `{ a: 1, b: 2 }` from their own enumerable properties
  in own-key order (enumerable symbol keys last, as `[Symbol(a)]: 1`),
  accessors as `[Getter]` / `[Setter]` / `[Getter/Setter]`;
  an object whose prototype chain holds a `constructor` naming a function other
  than `Object` renders as `A { x: 1 }`;
- functions `[Function: f]` / `[Function (anonymous)]`, classes `[class A]` /
  `[class B extends A]`, names from the function info table;
- `CLASS_ERROR` objects as `Name: message` (name and message looked up along the
  prototype chain without running getters);
- wrappers as `[Number: 1]` / `[String: 'ab']` / `[Boolean: true]` /
  `[Symbol: Symbol(a)]` / `[BigInt: 1n]`, from their `[[PrimitiveValue]]` (a String wrapper's
  virtual indices are not listed);
- `Map(N)` / `Set(N)`, `Promise { 42 }` / `Promise { <pending> }` /
  `Promise { <rejected> r }`, dates as ISO 8601 UTC (`Invalid Date` for a
  `NaN` time value);
- `WeakMap { <items unknown> }` / `WeakSet { <items unknown> }`, `WeakRef {}`,
  `FinalizationRegistry {}` (contents never listed; a subclass instance
  renders the same form);
- cycles as `<ref *1> { self: [Circular *1] }`;
- (phase 5b, §6.11) a proxy as its target, without running a trap;
  a revoked one as `<Revoked Proxy>`.

Private-name entries (§7.6) are never enumerable, so the renderer does not list
them.

### 6.10 BigInt

> **Implemented** (phase 5b): the value, literals, conversions, equality,
> hashing, property keys, JSON, the wrapper and `%BigInt.prototype%`, the
> operators (§6.10.4–6.10.6), the `BigInt` builtins, `Number(1n)` and the
> 64-bit boundary helpers, as described below; the stage table at the end
> records how it was built. ECMA-262 §6.1.6.2 (the BigInt
> type), §7.1 (conversions), §13 (operators), §21.2 (the BigInt object).

#### 6.10.1 Value and block layout

A BigInt is a **heap primitive**: a jsval with the single heap tag (§4.1)
pointing at a `TYPE_BIGINT` block. The type byte is fixed by the phase-5b
allocation: `pub(crate) const TYPE_BIGINT: u8 = 21;` in `abi.rs` (20 is
`TYPE_PROXY`, 24 / 25 / 26 are ArrayBuffer / TypedArray / DataView, 22–23
spare, 27+ RegExp). Code names the constant only, never the number.

| offset | size | field |
|---|---|---|
| 0 | 4 | header: kind `TYPE_BIGINT`; flags bit `1` `BIGINT_FLAG_NEG` (the sign); class `0` |
| 4 | 4 | `len: u32` (`BIGINT_LEN_OFFSET = 4`): the limb count |
| 8 | `4·len` | limbs (`BIGINT_DATA_OFFSET = 8`): the magnitude, little-endian `u32` limbs |

Block size `8 + 4·len`, rounded to the allocator class (§5.6).
`BIGINT_LEN_OFFSET` deliberately equals `STR_LEN_OFFSET`: "`len @4 != 0`"
is ToBoolean for both strings and BigInts (§6.10.2).

- **Canonical form**, an invariant of every block a jsval can reach: the top
  limb is nonzero, zero is `len = 0` with the sign flag clear (there is no
  `-0n`). Every producer finishes with `__bigint_trim(p) -> i64` (lower `len`
  past high zero limbs and return the jsval, `BIGINT_ZERO` for a zero), so
  equality, hashing and comparison can work on the stored words directly.
- **Immutable.** A block is written only between its allocation and the
  moment its jsval is first published (returned from the producing helper);
  afterwards it is shared freely (operands are never reused as results,
  scratch work happens in unpublished blocks).
- **Static blocks.** Literals are static data-segment blobs in the same
  layout: `collect_strings` (`emit/sections.rs`) lays every distinct nonzero
  literal out after the string blobs, deduplicated by value in the
  `bigints` table of `StrOffsets` (keyed on `(neg, limbs)`;
  `StrOffsets::bigint_bits` is the literal's jsval), so a program without a
  BigInt literal keeps its layout. `abi::BIGINT_ZERO` is the static `0n`
  (also every `0n` literal): an 8-byte block at a fixed address in the
  alignment slack between `DEC_POWERS` and `NUM_SCRATCH_ADDR`, its type byte
  written by a one-byte data segment in every module, returned by every zero
  result without allocating and as the dummy result of a helper that throws
  (`RangeError`, out of memory).
- **Maximum size.** `abi::BIGINT_MAX_LIMBS = 1 << 19` (2^24 bits, a 2 MiB
  block, well under the 512 MiB memory cap). `__bigint_new(len, neg) -> i32`
  (the only allocator of BigInt blocks; `__alloc`, zeroed) throws
  `RangeError: Maximum BigInt size exceeded` for `len > BIGINT_MAX_LIMBS`
  and returns `BIGINT_ZERO`'s base; a large-request out of memory leaves the
  pending `%OutOfMemoryError%` (§5.6) and also returns `BIGINT_ZERO`. Every
  caller tests `$exc_pending` after it. Operations whose result size is known
  in advance (`<<`, `**`, `asUintN` of a negative) check the bound *before*
  computing.
- **GC.** No outgoing references: `__gc_trace` already returns early for
  every type byte above `TYPE_DATE` (the leaf rule shared with strings and
  `TYPE_RAW`), so tracing needs no code; `21` is in the leaf row of the
  §5.7 table. The sweeper reclaims by slot class / span (§5.6) and never
  reads `len`, so no size function is needed.
- **No small-value fast path.** The free NaN-box tags (`0b100`, `0b110`,
  `0b111`) could hold an inline small BigInt, but every hot path that today
  tells numbers from non-numbers (`is_number`, `is_pointer`, the strict
  equality identity compare, `typeof`'s immediate arm, truthiness, the root
  classification) would gain a case, moving every Number row of the opcount
  gate. A single heap tag keeps all those paths untouched and confines
  BigInt to the paths that already handle heap values; BigInt arithmetic
  speed is not a phase-5b goal, and literals cost no allocation.

**Wrapper objects.** `Object(1n)` / `ToObject(1n)` (`__to_object`) is a
`TYPE_OBJECT` of class `CLASS_BIGINT = 9` (24 bytes, `[[BigIntData]]` at
`WRAPPER_VALUE_OFFSET`, proto `%BigInt.prototype%`), traced like the other
wrapper classes (jsval `@16`), rendered `[BigInt: 1n]`, unwrapped by
`JSON.stringify`. `%BigInt.prototype%` is `Intrinsic::BigIntPrototype`, an
ordinary object over `%Object.prototype%` holding the builtins of §6.10.7.
No other new
heap kind is needed.

#### 6.10.2 Type tests, `typeof`, ToBoolean, rendering

The `typeof`, ToBoolean, strict-equality and `emit_is_object` tests below
are emitted only when the program can produce a BigInt (§6.10.9).

- **Primitive, not object.** Every site that tells a symbol (a heap
  primitive) from an object also tells a BigInt (`emit_is_bigint(local)` in
  `emit/bigint.rs`, the shape of `emit_is_symbol`): `emit_is_object` tests
  the type byte against `TYPE_BIGINT` too, so `IsObject`, `instanceof`,
  `ToPrimitive` and the store guards treat it as a primitive; it is never
  `CanBeHeldWeakly`. Unlike a symbol (whose reserved word `@4` reads as "no
  own properties"), a BigInt's `len` sits where an object keeps `props`, so
  the helpers that walk any non-string heap value test for it first:
  `__heap_object` (renderer), `__own_keys` (empty), `__has_own` (`false`),
  `__delete` (no-op) and `__for_in_keys` (no keys).
- **Primitive receivers.** `__get` / `__set` / `__has`-style lookups with a
  BigInt receiver start at `%BigInt.prototype%` (the two primitive-receiver
  dispatch blocks of `emit/props.rs` that pick `%Symbol.prototype%` /
  `%Number.prototype%`, and `__symbol_get`); a write to a BigInt receiver is
  the strict-mode `TypeError` that numbers get (`Cannot create property 'x'
  on bigint '1n'`). `__to_object` (`emit/builtins/mod.rs`) wraps a BigInt
  in a `CLASS_BIGINT` wrapper.
- **`typeof`** is `"bigint"`: `abi::TYPEOF_STRINGS[7]`, picked by one more
  layered `select` (`type == TYPE_BIGINT ? 7`) in the heap arm of
  `emit_typeof_node` (`emit/expr.rs`).
- **ToBoolean**: `0n` is falsy, every other BigInt truthy. In `emit_cond`
  (`emit/stmt.rs`) the heap arm's non-string branch is
  `type != TYPE_BIGINT | len @4 != 0`; the number and string paths are
  unchanged. Every ToBoolean site (`!`, `&&` / `||`, conditions, `Boolean(v)`,
  the array callbacks) goes through `emit_cond`.
- **Rendering** (`emit/render.rs`, `__render_heap`): a BigInt prints as its
  decimal digits plus `n` (`1n`, `-12n`), the same at top level, inside
  arrays / objects / Map and Set entries (`[ 1n, 2n ]`, `{ a: 1n }`,
  `Map(1) { 1n => 1 }`); a wrapper as `[BigInt: 1n]`. The digits come from
  `__bigint_to_string(v, 10)` followed by `__out_str`; its allocation may
  run out of memory like the render buffers (§6.9). `__msg_str` (error
  message text) appends the `n` too; `__prim_str` (ToString) has no `n`.
  The `null` / `undefined` member error names a BigInt key by its property
  key (`__nullish_throw` uses `__prim_str` for it: `null[1n]` reads `'1'`).

#### 6.10.3 Conversions

All conversion helpers live in `emit/bigint.rs` (the `Big` group of
`emit/registry.rs`, the last helper group, tree-shaken by `emit/shake.rs`
like every helper). The shaker follows static calls: when the program can
produce a BigInt (the `bigints` gate, §6.10.9) the cold arm of every
numeric operator calls `__numeric_op` / `__numeric_unary` /
`__bigint_add_small` (§6.10.5), so the operator kernels and the conversions
they reach are kept. With the gate closed no operator, `ToNumeric` or
`Number(x)` site references a BigInt kernel; the kernels still reached are
the B2a ones called from always-present runtime helpers (§6.10.9).

| operation | helper / site | rule |
|---|---|---|
| ToPrimitive | `__to_primitive` | a BigInt is a primitive (returned as is); a `CLASS_BIGINT` wrapper unwraps through `%BigInt.prototype%.valueOf` like any object |
| ToNumeric | `__to_numeric(v) -> i64` | `ToPrimitive(v, number)`; a BigInt is returned, anything else goes through `__to_number` |
| ToNumber | `__to_number` | after `ToPrimitive`, a BigInt throws `TypeError: Cannot convert a BigInt value to a number` next to the symbol check (so unary `+`, `Math.*`, `isNaN`, `isFinite`, `Date` arguments, `ToIntegerOrInfinity` all throw); `__to_number_prim` is only reached with a non-BigInt primitive |
| ToString | `__prim_str` | decimal digits, no `n`, sign `-`: `__bigint_to_string(v, 10)`. Covers `String(v)`, template literals, `+` with a string, `join`, the default `sort` comparison, `ToPropertyKey` |
| ToBigInt | `__to_bigint(v) -> i64` | `ToPrimitive(v, number)`, then: BigInt itself; `true` / `false` → `1n` / `0n`; string → `StringToBigInt`, failure `SyntaxError: Cannot convert <s> to a BigInt`; `undefined`, `null`, number, symbol → `TypeError: Cannot convert <x> to a BigInt` (`<x>` the `__msg_str` text) |
| StringToBigInt | `__string_to_bigint(s) -> i64` | below; returns `UNDEFINED_BITS` on failure (callers map it to `SyntaxError` / `false`) |
| NumberToBigInt | `__bigint_from_f64(x: f64) -> i64` | the caller has checked `IsIntegralNumber` (else `RangeError: The number <x> cannot be converted to a BigInt because it is not an integer`, also for `NaN` / `±Infinity`); `|x| < 2^64` via `i64.trunc_f64_u`, else the 53-bit mantissa shifted by `exponent - 52` |
| BigInt → Number | `__bigint_to_f64(p) -> f64` | exact round-half-even: bit length `L`; `L <= 64` → `f64.convert_i64_u` of the low 64 bits (wasm rounds to nearest, ties to even); else the top 64 bits with bit 0 OR-ed with "any lower bit set" (sticky; the rounding position is bit 10, so the result is correctly rounded), converted, times `2^(L-64)` (built from its exponent bits; `L > 1024` → `Infinity`); negate for the sign |
| ToBigInt64 / ToBigUint64 | `__bigint_to_i64(p) -> i64` | the low 64 bits of the two's-complement value (= `asIntN(64)` / `asUintN(64)` read as `i64`/`u64`); with `__bigint_from_i64(v: i64, signed: i32) -> i64` the boundary for L2-T's `BigInt64Array` / `BigUint64Array` / `DataView` (not used by this workstream) |

**StringToBigInt** (`StringIntegerLiteral`): trim JS whitespace and line
terminators at both ends with the existing `__str_ws` scan; an empty
remainder is `0n`. Then either `0x`/`0X`, `0o`/`0O`, `0b`/`0B` followed by at
least one digit of that radix and nothing else (no sign), or an optional `+`
/ `-` followed by at least one decimal digit and nothing else. No numeric
separators, no `n` suffix, no `.`, exponent, `Infinity`; any two-byte unit
fails. Every radix allocates `digits / 8 + 2` limbs (at most 4 bits per
digit) and folds `k`-digit chunks (`r^k <= 2^32 - 1`: 9 decimal, 8 hex, 10
octal, 31 binary digits) with an in-place multiply-add
`acc = acc·r^k + chunk` over the limbs written so far (O(n²) limb steps:
about 1.2·10^5 for a 10 000-digit decimal string), then trims; a separate
O(n) bit packing for the power-of-two radices was not worth its code.

**ToString(radix)** (`__bigint_to_string(v, radix: i32) -> i32`, a string
base; radix `2..=36`, lowercase digits): `0n` is `"0"`. Every radix copies
the magnitude into an unpublished `TYPE_RAW` scratch block and repeatedly
divides it in place by the largest `r^k <= 2^32 - 1` (`10^9` for decimal;
one-limb division with an `i64` dividend, O(n) per step), collecting
`k`-digit chunks in a second `TYPE_RAW` buffer (at most `2·len + 1`: `r^k >
2^26`); then one Latin-1 string of the exact length is filled from the least
significant chunk (padded to `k` digits) up to the most significant one
(unpadded), and the sign. O(n²) overall; out of memory returns the static
empty string with the `RangeError` pending.

#### 6.10.4 Arithmetic kernels

Magnitude kernels in `emit/bigint.rs` work on `u32` limbs with `i64`
intermediates (wasm has no 64×64→128 multiply, and `u32` limbs make every
partial product and carry fit an `i64`). Each allocates its result with
`__bigint_new` sized to the worst case, writes it, then `__bigint_trim`.
Signed operations dispatch on the signs to magnitude add / subtract /
compare.

| helper | algorithm |
|---|---|
| `__bigint_cmp_mag(a, b) -> i32` | length, then limbs from the top; leaf |
| `__bigint_add(a, b)`, `__bigint_sub(a, b)` | sign dispatch to magnitude add (carry, `max(len) + 1` limbs) or subtract-larger-minus-smaller (`__bigint_cmp_mag` picks the larger, which gives the sign; equal magnitudes are `0n`); one body emitted twice (`emit_add_sub(sub)`) |
| `__bigint_mul(a, b)` | schoolbook O(n·m), `len_a + len_b` limbs (every `r + a_i·b_j + carry` fits a `u64`); a zero operand returns `BIGINT_ZERO`; the bound is `__bigint_new`'s |
| `__bigint_divmod(a, b, want_rem)` | `b = 0n` → `RangeError: Division by zero`; `|a| < |b|` → `0n` / `a`; one-limb divisor → short division; otherwise Knuth's Algorithm D (normalize copies of both operands — two unpublished `__bigint_new` blocks — so the divisor's top bit is set, estimate each quotient limb from the top two limbs with the `q̂` correction, multiply-subtract, add back on borrow, denormalize the remainder; `q̂ <= B + 1`, so `q̂·v[n-2]` and `r̂:u[j+n-2]` fit a `u64`). Chosen over shift-subtract, which costs a full-width subtract per *bit* (32× more passes). Quotient truncates toward zero with sign `sa ^ sb`; the remainder takes the dividend's sign |
| `__bigint_pow(a, b)` | `b < 0n` → `RangeError: Exponent must be non-negative`; `b = 0n` → `1n` (also `0n ** 0n`); base `0n` / `1n` → itself, `-1n` → `±1n` by parity; otherwise `b` must fit `u32` and `bitlen(a)·b <= 2^24`, else `RangeError: Maximum BigInt size exceeded`; then square-and-multiply |
| `__bigint_neg(a)` | copy with the sign flipped (`0n` stays `0n`) |
| `__bigint_mag_step(p, inc, neg)` | the value with magnitude `|p| + 1` (`inc`) or `|p| - 1` and sign `neg` (carry / borrow pass) |
| `__bigint_add_small(v: i64, d: i32)` | `v + d` for `d = ±1` (`++` / `--`): `__bigint_mag_step` growing the magnitude when `d` has the value's sign (or the value is `0n`), else shrinking it |
| `__bigint_bitop(a, b, op)` | `&` `|` `^` on infinite two's complement: width `w = max(len) + 1` limbs; a negative operand is converted limb by limb to `~(|x| - 1)` (sign-extended with `0xFFFF_FFFF`), the op runs limb-wise, the result is negative iff its top limb's bit 31 is set (AND: both negative; OR: either; XOR: exactly one) and is converted back with `|r| = ~r + 1` |
| `__bigint_not(a)` | `~a = -a - 1n`: `__bigint_mag_step` with `-(|a| + 1)` for a non-negative `a`, `|a| - 1` for a negative one |
| `__bigint_shift(a, n, right)` | `<<` (`right` 0) and `>>` (`right` 1) in one helper; the count is a BigInt: a negative count shifts the other way (a count of more than one limb is huge either way). `a = 0n` → `0n`. Left: a count above `2^24` → `RangeError: Maximum BigInt size exceeded`; else limb move + bit shift. Right (floor semantics): a count `>= bitlen(a)` gives `0n` (non-negative) or `-1n` (negative); else `q = |a| >> n` and, for a negative `a`, `q + 1` when any shifted-out bit was set (one spare limb for the carry), result `-q` |
| `__bigint_as_n(bits: f64, a, signed: i32)` | `asUintN`: `a mod 2^bits` — non-negative `a` keeps its low `bits` bits; negative `a` with nonzero low part `r` gives `2^bits - r` (size checked: `bits` above `2^24` with such an `a` is the size `RangeError`); `asIntN`: that value minus `2^bits` when `bits > 0` and bit `bits - 1` is set; `bits = 0` → `0n` |
| `__bigint_eq(a, b) -> i32` | both `TYPE_BIGINT` and identical header sign, `len` and limbs (canonical form); `0` when either is not a BigInt; leaf |
| `__bigint_cmp(a, b) -> i32` | signed compare (`-1/0/1`); leaf |
| `__bigint_cmp_num(a, x: f64) -> i32` | exact BigInt-vs-Number compare without allocating: `NaN` → `2` (unordered); `±Infinity` → `∓1` / `±1`; signs; then `t = trunc(x)` as `m·2^s` (53-bit `m`), compared limb by limb against `|a|` (limb `i` of `t` is `(m << s) >> 32i` computed with `i64` shifts); equal integer parts with a fractional `x` make `|x|` the larger; leaf |
| `__bigint_hash(a) -> i32` | FNV-1a over the sign, `len` and limbs (static and heap blocks of one value hash alike); leaf |

#### 6.10.5 Operators

Codegen decides nothing about BigInt statically except literals: every
operator keeps its Number fast path inline and reaches BigInt only on the
path where an operand is already known not to be a number. Everything in
this section applies only when the program can produce a BigInt; with the
`bigints` gate closed the operators emit their Number-only code (§6.10.9).

**New IR** (`ir.rs`, lowered in `codegen/expr.rs`):

- `IrNode::BigInt { neg: bool, limbs: Box<[u32]> }`: a literal, from swc's
  `Lit::BigInt` (`value.to_u32_digits()`; no new dependency). Unary minus
  folds into a literal operand (`-1n`; `-0n` is `0n`).
- `BinOp::Exp`: `a ** b` and `**=` (the binary, and the `**=` forms of
  identifiers, members, private names and `super`). `Math.pow` keeps
  `MathCall Pow` (ToNumber semantics: `Math.pow(2n, 1n)` throws).
  `BinOp::Exp` emits the `emit_pow` body (`emit_pow_body`) after the
  numeric-operand prologue below, so a Number `**` computes exactly what it
  did before.
- `IrNode::Negate(Box<IrNode>)` and `IrNode::BitNot(Box<IrNode>)`: unary `-`
  and `~` (formerly `-0 - x` and `x ^ -1`, which are wrong for BigInt:
  `-0 - 1n` and `1n ^ -1` throw). `emit_numeric_unary` computes a number
  operand inline (`-0 - x`, `ToInt32(x) ^ -1`: the same values) and sends
  any other operand to `__numeric_unary`.
- `ConvertOp::ToNumeric` and `IrNode::NumericStep { arg, inc }` (`arg ± 1`
  of `arg`'s own type; `arg` is already numeric): the update forms that are
  not a plain local — cells, globals, generator slots (`codegen/generator.rs`
  ~1921), members (`IndexOpSet` with `OpSetKind::{Prefix,Postfix}Update`),
  `super` and private members — lower `++x` to
  `store(NumericStep(ToNumeric(read), inc))` instead of `ToNumber(read) +
  delta`, and postfix's old value to `NumericStep(store, !inc)` instead of
  `store - delta` (exact for BigInt; for numbers the same value as today).

**Numeric-operand prologue** (`emit/numeric.rs`). The binary numeric
operators no longer call `emit_coerce_operands` (which stays, with ToNumber
semantics, for `Math.pow` / `Math.atan2` / `Math.imul`); one emitter,
`emit_numeric_binary(op)`, replaces `emit_arith`, `emit_mod` and
`emit_bitwise`:

```text
block (result i64)                         ;; new; 0 fuel
  <evaluate lhs, rhs; stash s0, s1>        ;; unchanged
  if !is_number(s0)                        ;; unchanged test
    s0 s1 OP call __numeric_op ; propagate ; leave frame ; br 1
  end
  if !is_number(s1)                        ;; unchanged test
    s0 s1 OP call __numeric_op ; propagate ; leave frame ; br 1
  end
  <unchanged f64 / i32 computation of op>
end
```

The prologue is `emit_numeric_binary`; the inline Number computation is
`emit_number_op(op, s0, s1, pow_scratch)` (the `emit_pow` body is
`emit_pow_body`, shared with `Math.pow`, whose prologue keeps
`emit_coerce_operands`).

`__numeric_op(a, b, op: i32) -> i64` (`emit/bigint.rs`): `a =
__to_numeric(a)`, then `b = __to_numeric(b)` (left first, each may throw);
both numbers → the Number result (`emit_number_op`, the inline code);
one BigInt and one number → `TypeError: Cannot mix BigInt and
other types, use explicit conversions`; two BigInts → the kernel. Op codes
(`NUMOP_*` in `emit/bigint.rs`): `ADD SUB MUL DIV MOD EXP AND OR XOR SHL SHR
USHR` (`ADD` is `__add`'s, below). `>>>` on two BigInts throws `TypeError: BigInts have no unsigned right
shift, use >> instead`. `__numeric_unary(v, op)` does the same for
`Negate` / `BitNot` (`NUMOP_NEG` / `NUMOP_NOT`; `ToNumeric` once; number →
`-0 - x` / `~ToInt32(x)`, BigInt → `__bigint_neg` / `__bigint_not`); the
inline site computes a number operand itself and calls it only for any other
operand. Unary `+` stays `ToNumber` and so throws for a BigInt.

| operator | two BigInts | BigInt with number | notes |
|---|---|---|---|
| `+` | `__bigint_add` | `TypeError` | in `__add` (`emit/convert.rs`): after both `ToPrimitive`s, a string operand concatenates (BigInt → decimal), else a BigInt operand hands both to `__numeric_op(a, b, NUMOP_ADD)` (the other is a primitive, so its `ToNumeric` is `ToNumber`; a symbol throws), else the `ToNumber`s add as before. The inline `+` already sends any heap operand to `__add`, so `emit_add` is unchanged |
| `-` `*` | `__bigint_sub` / `__bigint_mul` | `TypeError` | |
| `/` `%` | `__bigint_divmod` | `TypeError` | divisor `0n` → `RangeError` |
| `**` | `__bigint_pow` | `TypeError` | negative exponent → `RangeError` |
| `&` `|` `^` | `__bigint_bitop` | `TypeError` | two's complement |
| `<<` `>>` | `__bigint_shift` | `TypeError` | negative count reverses |
| `>>>` | `TypeError` | `TypeError` | |
| unary `-` `~` | `__bigint_neg` / `__bigint_not` | — | via `__numeric_unary` |
| `++` `--` | `__bigint_add_small(v, ±1)` | — | below |

**`++` / `--` on a plain local** (`emit_inc_dec`): the cold arm of the
existing `emit_coerce_number(local)` becomes `local = __to_numeric(local)`
followed, still inside the arm, by a BigInt branch that computes
`__bigint_add_small(local, ±1)`, leaves the prefix / postfix result and
`br`s out of a new enclosing block; the Number path is unchanged. The three
writes of the local are announced as `IncDec { local }` definitions (the
old `note_numeric_def` is gone), so the fixpoint keeps a numeric loop
counter unrooted. `NumericStep` emits `is_number ? f64 add ±1 :
__bigint_add_small(v, ±1)`: its operand is already numeric, so a separate
`__numeric_step` helper is not needed. A member update (`IndexOpSet` with
`OpSetKind::{Prefix,Postfix}Update`, `emit/object.rs`) reads with
`ToNumeric` and stores `NumericStep` of the old value; the cell, global,
generator-slot, private and `super` updates are lowered to the same nodes in
codegen.

**Root classification** (`emit/roots.rs::classify`, and `yields_number` in
`emit/numeric.rs`). An arithmetic result is no longer always a number: it may
be a heap BigInt and then must be rooted. `classify` treats `Binary` `Sub`
`Mul` `Div` `Mod` `Exp` `BitAnd` `BitOr` `BitXor` `Shl` `Shr`, `Negate`,
`BitNot` and `NumericStep` like `Add` today (numeric iff every operand is,
with their dependencies), `IncDec { local }` as numeric iff `local` is
(the fixpoint keeps `for (let i = 0; ..; i++)` unrooted), and only `UShr`,
the relational and equality operators, `MathCall`, `MathSpread` and
`ToNumber` as unconditionally numeric; a `BigInt` literal is
`static_block()`. `yields_number` keeps `Num`, `MathCall`, `MathSpread`,
`ToNumber` and `UShr` only; the `Math.*` argument paths that skip the
coercion of a `yields_number` operand therefore coerce an arithmetic
argument (`Math.floor(a - b)` with BigInts must throw). The shaker's
dynamic-key test (`shake::note_key`) treats the numeric operators other than
`+` as never yielding a string (`roots::never_string`): a BigInt key is a
canonical numeric string, which names no builtin, so `a[i - 1]` still
shakes. With the `bigints` gate closed `classify` and `yields_number` are
the Number-only ones again: every arithmetic operator but `+` and every
`IncDec` is unconditionally numeric (§6.10.9).

#### 6.10.6 Equality, comparison, hashing

- **Strict equality** (`emit_strict_eq`): the then-condition is
  `emit_is_str_or_bigint(s0) & emit_is_str_or_bigint(s1)` (type byte
  `TYPE_STRING` or `TYPE_BIGINT`; a non-pointer takes the same three
  instructions as before), and the then-branch calls `__bigint_eq` when
  either type byte is nonzero (`TYPE_BIGINT`), else `__str_eq` (a string
  and a BigInt are unequal: `__bigint_eq` returns `0`). The number /
  identity `select` is unchanged. `switch` cases and `Array.prototype.indexOf`
  use the same emitter.
- **SameValue / SameValueZero**: `__same_value_zero` (`emit/runtime.rs`)
  calls `__bigint_eq` when its first operand is a BigInt; `emit_same_value`
  (`Object.is`, `defineProperty` value checks), `includes` and the Map / Set
  probes all reach it. The remaining `__str_eq` callers compare against a
  static string, not two values. `Object.is(1n, 1n)` is `true`;
  `[1n].includes(1n)` is `true`.
- **Map / Set keys**: `__same_value_zero` as above; `__map_hash` hashes a
  BigInt with `__bigint_hash`, so equal values share a bucket.
- **Loose equality** (`__loose_eq`, after booleans became numbers and an
  object against a primitive became its `ToPrimitive`; the BigInt arm is
  `__bigint_loose_eq`, before the identity fallback): two
  BigInts → `__bigint_eq`; BigInt and number → `__bigint_cmp_num == 0`
  (`NaN` / infinities unequal, `1n == 1.5` false, no rounding); BigInt and
  string → `StringToBigInt`, failure (or the size `RangeError` of a huge
  string, cleared) → `false`; an object against a BigInt → `ToPrimitive`
  and retry (the BigInt counts as a primitive); BigInt against `null` /
  `undefined` / symbol → `false`.
- **Relational** (`__compare`, reached when the operands are not two strings
  or two numbers): after `ToPrimitive` (hint number) and the two-strings
  test, a BigInt operand selects the BigInt arm: BigInt / BigInt →
  `__bigint_cmp`; BigInt / string → `StringToBigInt` of the string
  (failure, or the size `RangeError` of a huge string, cleared → *undefined*);
  otherwise `ToNumber` of the other operand (a symbol throws) and
  `__bigint_cmp_num`, the order negated when the BigInt is the right
  operand. An *undefined* or unordered (`2`)
  outcome is `false` for all four operators, including `<=` / `>=`.
- **`Array.prototype.sort`** without a comparator compares `ToString`s, so
  BigInts sort by their decimal text through `__prim_str`.

#### 6.10.7 Property keys, JSON, built-ins

- **Property keys.** `{1n: v}`, `class { 1n() {} }`, `{1n: a} = o` key on
  the decimal string, computed at compile time from the literal
  (`value.to_string()`): `lower_prop_key` (`codegen/expr.rs`, object
  literals) and `prop_key_name` (`codegen/stmt.rs`, class members and
  patterns) map `PropName::BigInt` to a static string key. `o[1n]`,
  `1n in o`, `delete o[1n]` and every runtime key go through `__to_key`
  (`emit/props.rs`), whose `__to_string` fallback reaches `__prim_str`, so a
  BigInt key is `__bigint_to_string(v, 10)`, interned (`o[1n]`, `o["1"]` and
  `a[1]` are the same key; an array-index string keeps the index path).
- **JSON.stringify** (`__json_rep`, `emit/builtins/json.rs`): a BigInt
  value consults `toJSON` (`GetV`, i.e. through `%BigInt.prototype%`, so a
  user-installed `BigInt.prototype.toJSON` works) and the replacer as an
  object does; a `CLASS_BIGINT` wrapper is then unwrapped like the other
  wrappers; a BigInt still left throws `TypeError: Do not know how to
  serialize a BigInt`, at any nesting depth. `__json_str` never sees one.
- **`BigInt`** (`BuiltinCtor::BigInt` in `ir.rs`, the last constructor;
  the realm entries in `emit/realm.rs`: the global, `Builtin::Ctor(BigInt)`
  and `Builtin::BigInt(BigIntOp)` appended to `Builtin::ALL`, the statics and
  the tag in `bigint_entries`; bodies in `emit/builtins/bigint.rs`, which
  also re-exports `emit_is_bigint` for the sibling bodies): `BigInt(v)`
  is `ToPrimitive(v, number)`, then a number → NumberToBigInt (`RangeError:
  The number <x> cannot be converted to a BigInt because it is not an
  integer` for `NaN`, `±Infinity` and non-integers, tested as `x - trunc(x)
  != 0`), anything else → `ToBigInt`. It is a constructor for
  `IsConstructor` (flag `CONSTRUCTOR`, like `Symbol`), but `new BigInt(..)`
  throws `TypeError: BigInt is not a constructor` before converting.
  `length` 1, `name` `"BigInt"`. `BigInt.asIntN(bits, bigint)` /
  `BigInt.asUintN(bits, bigint)` (`length` 2): `bits = ToIndex(bits)`
  (`RangeError: Invalid value: not (convertible to) a safe integer` for
  negative or `> 2^53 - 1`), `bigint = ToBigInt(bigint)`, in that order, then
  `__bigint_as_n` (§6.10.4; the `w = ceil(bits / 32)`-limb result is built
  only when `w <= len + 1` or for `asUintN` of a negative value, every other
  wider case returns the operand).
- **`%BigInt.prototype%`** (an ordinary object, not a BigInt): `constructor`,
  `toString(radix)` (`thisBigIntValue`: a BigInt or a `CLASS_BIGINT`
  wrapper, else `TypeError`; `radix` `undefined` → 10, else
  `ToIntegerOrInfinity`, outside `2..=36` → `RangeError` with
  `Number.prototype.toString`'s message), `toLocaleString()` (=
  `toString()`, no locale data), `valueOf()` (all three `length` 0),
  `[@@toStringTag]` `"BigInt"` (configurable only). `Object.prototype.toString`
  then gives `[object BigInt]`.
- **Unchanged builtins that now see BigInts**: `Number(1n)` is `1` (the
  `Number` call and construct — not unary `+` — use `ToNumeric` and convert a
  BigInt with `__bigint_to_f64`; `ConvertOp::ToNumber` stays the unary-`+` /
  ToNumber conversion and `ConvertOp::NumberCall` is `Number(v)`, inlined at
  the call site and in the `Number` constructor body: the number test inline,
  anything else through `__number_value(v) -> i64` (`emit/bigint.rs`:
  `__to_numeric`, then `__bigint_to_f64` of a BigInt); `NumberCall` is
  numeric for the root classification and `yields_number`);
  `Number.isInteger(1n)` / `isSafeInteger` / `isFinite` / `isNaN` are
  `false` (no coercion); `parseInt(1n)` / `parseFloat` read `"1"`; `isNaN`,
  `isFinite`, `Math.*`, `Date` setters throw (ToNumber); `String(1n)` is
  `"1"`; `Object.is`, `Map`, `Set`, `includes` as above.

#### 6.10.8 Opcount, diagnostics, conformance

**Opcount** (`tests/opcount/snapshot.tsv`). The `bigints` gate (§6.10.9)
confines the changes below to programs that can produce a BigInt: a program
with the gate closed emits the pre-B2b operator, `ToNumeric` and
`Number(x)` code and the pre-B2a `typeof`, ToBoolean, strict-equality and
`IsObject` code, so its rows move only by the always-present guest tests of
item 3 and by static-address shifts. In a program with the gate open the
Number fast paths execute the same instructions as before (the new `block`
/ `end` and the cold arms cost no fuel unless taken), but four mechanical
changes move rows, and each moved row gets a reason cell in the stage that
moves it:

1. B2b: the cold arms of every `- * / % ** & | ^ << >> >>>`, unary `-` /
   `~` and `++` / `--` site grow (a `__numeric_op` call and a `br` instead
   of a `__to_number` call): `user_ops` of every row with such a site;
   `fuel` only where a non-number operand is actually converted, except
   that unary `-` / `~` on a number now run their own short inline code
   (one `is_number` test, no constant-operand stash), and `++` / `--` on a
   local open one more `block`.
2. B2b: the root classification (§6.10.5): a local defined from
   arithmetic over a non-numeric operand (a parameter, property read or call
   result) is now rooted, adding shadow-stack stores (`user_ops` and `fuel`);
   `Math.*` arguments that are arithmetic expressions gain their `is_number`
   coercion test.
3. B2a: `typeof` (one more `select`, every `typeof` site), `IsObject` (one
   more type-byte test on heap values), ToBoolean of a non-string heap value
   (the `len` test), strict equality of two strings (the BigInt type test
   before `__str_eq`); in the guest, the BigInt tests of
   `__same_value_zero`, `__map_hash`, `__loose_eq`'s fallback, `__to_number`,
   `__prim_str`, `__msg_str`, `__to_object`, `__json_rep`, the primitive
   receiver dispatch of `__get` and the `__own_keys` / `__has_own` /
   `__delete` / `__for_in_keys` guards, and the realm image growing by
   `%BigInt.prototype%` (later static addresses and intern slots shift).
4. B2b: the non-local update forms (`NumericStep` replaces the `__add`-based
   desugaring of `o.x++`, `cell++`, `global++`).

**Diagnostics** (§8). Nothing BigInt-specific is a compile error (the
literal and literal-key diagnostics are gone; their `tests/pattern_keys.rs`
cases are positive tests); every BigInt error is a run-time `TypeError` /
`RangeError` / `SyntaxError` as listed above.

**Conformance.** `BigInt` is not in `tests/test262/unsupported-features.txt`.
Of the 1 506 tests tagged `BigInt`, 395 are eligible (146
`language/expressions`, 69 `built-ins/BigInt`, 59 `language/literals/bigint`,
57 `DataView` BigInt accessors, 19 `BigInt64Array` / `BigUint64Array` and
`%TypedArray%` tag tests not tagged `TypedArray`, 10 `intl402`, the rest in
`Object`, `JSON`, `String`, `Map`, `Set`, `Promise`, `Array`, `Number`,
`ArrayBuffer`, `Error`, annex B and `harness`; 39 negative, all literal
syntax errors); the other 1 111 also need `TypedArray*` / `Atomics*` /
`SharedArrayBuffer` / `Temporal` / `Reflect*` / `Intl*` or a skipped flag.
Remaining BigInt limitations are in §13.

#### 6.10.9 Compile-time gate (`bigints`)

Most programs can never produce a BigInt, and they must not pay for one:
not in module size (the operator cold arms would keep the kernels alive
through static calls), not in opcount (cold arms, the `++` / `--` block,
the extra rooting of arithmetic results). `Helpers::bigints`
(`emit/mod.rs`, field `bigint_gate: BigIntGate::{Closed, Open}` next to
`binary` and `proxy_gate`), computed once per program by
`sections.rs::program_has_bigint` next to `program_has_binary` (§5.8) and
`program_has_proxies` (§6.11), says whether a BigInt can exist.

**Triggers.** The gate is open iff the program

- has a BigInt literal (`IrNode::BigInt`, including a folded `-1n`);
- names one of `BIGINT_NAMES` (`sections.rs`, next to `BINARY_NAMES`) as a
  global reference (`GlobalGet` / `GlobalSet`), an intrinsic
  (`GlobalIntrinsic::Ctor(BuiltinCtor::BigInt)`) or a string constant (a
  static member key or any string literal, which a computed access could
  use): `BigInt`, `BigInt64Array`, `BigUint64Array`, `getBigInt64`,
  `getBigUint64`, `setBigInt64`, `setBigUint64`;
- uses the global object as a value (`GlobalIntrinsic::Global`:
  `globalThis` in any form, top-level `this`), which reaches `BigInt` by a
  computed name or by reflection — the rule of the binary-data gate (§5.8)
  and of the Proxy gate (§6.11).

These are every source of a BigInt value on this base: a literal; the
`BigInt` function (`BigInt(v)`, and `asIntN` / `asUintN`, reachable only
through it); the `%BigInt.prototype%` methods and the operators, which need
a BigInt operand first; `__to_object`'s wrapper, likewise. `JSON.parse`,
`Number`, `parseInt` and every other builtin produce no BigInt, the realm
reaches `BigInt` only through the global object (a reflective walk such as
`Object.getOwnPropertyDescriptors(globalThis)` names `globalThis`), and
strict mode leaves no other path to the global object. The typed arrays
of §5.8 (T3a) install only the nine Number kinds (`ElementKind` has no
`BigInt64` / `BigUint64`, kinds `10` / `11` stay reserved) and `DataView`
has no `get/setBig(U)Int64`, so binary data produces no BigInt yet. The
typed-array and `DataView` names in `BIGINT_NAMES` are for L2-T's BigInt
kinds. When those land, a `BigInt64Array` / `BigUint64Array` constructor is
reachable only by its name or through the global object (`%TypedArray%`
lists no subclasses), but a `DataView` reached by reflection
(`Object.getOwnPropertyDescriptors(DataView.prototype)`) can call
`getBigInt64` without naming it, so that stage must add `DataView` to
`BIGINT_NAMES`.

**What it switches.** With the gate closed `emit` first rewrites the IR to
its Number-only shapes (`numeric.rs::lower_number_only`, on a copy of the
program): `a ** b` (`BinOp::Exp`) becomes `MathCall Pow`, `Negate(x)` `-0 -
x`, `BitNot(x)` `x ^ -1`, `ConvertOp::ToNumeric` / `NumberCall`
`ToNumber`, and `NumericStep { arg, inc }` `arg + ±1` — or, for a postfix
update's old value (`arg` is the store: `Assign`, `CellSet`, `GlobalSet`),
`store - ±1` —, exactly the IR codegen produced before B2b. The emitters
then branch on the flag:

| site | gate closed | file |
|---|---|---|
| `- * / % & \| ^ << >> >>>` | `emit_number_binary`: stash, `emit_coerce_operands`, `emit_number_op` (the old `emit_arith` / `emit_mod` / `emit_bitwise` code) | `emit/numeric.rs` |
| `++` / `--` of a local | `emit_coerce_number`, two numeric definitions, the inline step (no block, no `__to_numeric` / `__bigint_add_small` arm) | `emit/numeric.rs` |
| member `++` / `--` | `ToNumber` read, `Binary(op, old, ±1)` step | `emit/object.rs` |
| `ToNumeric`, `Number(x)` (also the `Number` constructor body) | the `ToNumber` path of `emit_convert_call` | `emit/numeric.rs` |
| `+` with a heap operand | `__add` without the BigInt arm | `emit/convert.rs` |
| relational | `__compare` without the BigInt arm and its three locals | `emit/convert.rs` |
| `typeof` | no `bigint` select; the word fold stops at `symbol` | `emit/expr.rs` |
| ToBoolean | the non-string heap arm is the constant `1` | `emit/stmt.rs` |
| strict equality | `emit_is_string` tests and `__str_eq` only | `emit/numeric.rs` |
| `IsObject` | no `TYPE_BIGINT` test | `emit/expr.rs` |
| root classification, `yields_number` | every arithmetic operator but `+`, and `IncDec`, unconditionally numeric | `emit/roots.rs`, `emit/numeric.rs` |

`classify` and `emit_is_object` take no `Helpers`, so `emit` also publishes
the flag for the duration of the module through `roots::BigIntScope`
(a thread-local, `true` outside an `emit`, the sound default). The
shaker's `note_key` needs no change: on the Number-only IR the numeric
operators are numeric for `classify_is_numeric`, as before B2b. With the
gate closed no emitted body references `__numeric_op`, `__numeric_unary`,
`__bigint_add_small`, `__to_numeric` or `__number_value`, so they and the
kernels only they reach are shaken out; with it open everything is as
§6.10.3–6.10.8 describe.

**Soundness.** With the gate closed no BigInt can exist at run time: none
of the sources above appears, and every other operation that returns a
BigInt needs a BigInt operand. So the Number-only operator code is exact
(no operand can take the BigInt arm it lacks), the pre-B2b rooting is
sound (an arithmetic result or a stepped local is always a number and
never needs a root), and the missing `typeof`, ToBoolean, equality and
`IsObject` tests could only have been taken by a BigInt. With the gate open
the operators, their rooting and the GC behave exactly as §6.10.5
describes.

**Cost model.** A gate-closed program pays nothing in user code relative to
the pre-B2a emission. The runtime helpers that every program may keep stay
shared and keep their B2a BigInt tests, rather than emitting two variants
of each body: `__same_value_zero`, `__map_hash`, `__loose_eq`,
`__to_number`, `__prim_str`, `__msg_str`, `__to_object`, `__json_rep`, the
renderer, `__get`'s primitive-receiver dispatch, `__prop_throw`'s receiver
kind and the `__own_keys` / `__has_own` / `__delete` / `__for_in_keys`
guards. Each costs one `emit_is_bigint` test (a pointer test, then a
type-byte load: about 15 instructions) plus an `if`, executed only on the
call path that reaches it (a non-string operand of `__same_value_zero`, a
non-string key in `__map_hash`, a heap primitive in the others). They keep
the B2a kernels they call (`__bigint_eq`, `__bigint_hash`,
`__bigint_loose_eq`, `__bigint_to_string`) in modules that keep those
helpers.

#### 6.10.10 Stages

| stage | scope | files | tests (written per the no-local-run rule; CI runs them) |
|---|---|---|---|
| **B2a** value | `abi.rs` (`TYPE_BIGINT`, `BIGINT_*`, `CLASS_BIGINT`, `TYPEOF_STRINGS[7]`, `BIGINT_ZERO`); literals (`IrNode::BigInt`, static blocks, `-` folding); `__bigint_new` / `trim` / `eq` / `cmp` / `hash` / `to_string` / `string_to_bigint` / `to_f64` / `from_f64`; ToBoolean, `typeof`, ToNumber `TypeError`, ToString, ToPrimitive, `__to_numeric`, `__to_bigint`; the `TYPE_SYMBOL`-site audit (primitive vs object), primitive receivers, `CLASS_BIGINT` wrapper + empty `%BigInt.prototype%` skeleton, `__to_object`; renderer, `__msg_str`; strict equality, SameValue(Zero), `__array_search`, Map hash, loose equality; property keys (literal and `__to_key`); JSON `TypeError`; §4.1 / §5.2 / §5.7 / §6.9 / §7.2 / §8 text | `abi.rs`, `ir.rs`, `codegen/{expr,stmt}.rs`, `emit/{bigint,sections,registry,shake,roots,value,expr,stmt,render,dtoa,strtod,convert,runtime,props,realm,gc}.rs`, `emit/props/descriptor.rs`, `emit/builtins/{json,mod,map_set,array}.rs` | new `tests/bigint.rs` (literals, `typeof`, truthiness, rendering incl. nested and wrapper, ToString of multi-limb values in radix 10, equality / SameValueZero / Map keys, loose equality vs number / string incl. `NaN`, `±Infinity`, values above `2^64`, property keys, JSON `TypeError`, ToNumber `TypeError` through `+x` / `Math.abs`); `pattern_keys.rs` rewrites; `abi_drift_tests.rs` `TYPEOF_STRINGS[7]` |
| **B2b** operators | `BinOp::Exp`, `Negate`, `BitNot`, `ToNumeric`, `NumericStep`; `emit_numeric_binary`, `__numeric_op`, `__numeric_unary`; every kernel of §6.10.4 except `as_n`; `__add` BigInt path; relational `__compare`; `emit_inc_dec`; root classification and `yields_number`; opcount reasons | `ir.rs`, `codegen/{expr,generator}.rs`, `emit/{numeric,bigint,convert,expr,roots}.rs`, `tests/opcount/snapshot.tsv` | new `tests/bigint_ops.rs` (every operator and compound form, relational operators vs number / string incl. `NaN`, `±Infinity`, non-integral numbers and unparsable strings, mixed-type `TypeError`s, `>>>` / unary `+` `TypeError`, division / remainder signs and `RangeError`, `**` negative exponent, bitwise and shifts on negatives across limb boundaries, huge shift counts, `++`/`--` on locals, cells, globals, members, generator slots, multi-limb `*` and `/` cross-checked against known products); a `gc_stress` variant module in the same file (`CompileOptions::gc_stress = 1`) over multi-limb loops (factorial of 200, a Fibonacci accumulator, divmod round trips) so every allocation collects while operands are live |
| **B3** built-ins | `BigInt` ctor, `asIntN` / `asUintN` (`__bigint_as_n`), `%BigInt.prototype%` methods and `@@toStringTag`, `toString(radix)`, `Number(1n)` (`ConvertOp::NumberCall`), `__bigint_to_i64` / `__bigint_from_i64` | `ir.rs`, `codegen/*` (the `Number(..)` inline call), `emit/realm.rs`, new `emit/builtins/bigint.rs`, `emit/builtins/{mod,number}.rs`, `emit/bigint.rs` | new `tests/bigint_builtins.rs` (constructor coercions incl. strings in every radix, whitespace, syntax errors, `new BigInt` `TypeError`, `RangeError` for non-integers, `asIntN` / `asUintN` edge widths 0 / 1 / 64 / 65 and negative inputs, `toString` radices 2..36 on multi-limb values, `thisBigIntValue` errors, `Number(2n**1024n)` = `Infinity`, round-half-even at `2^53 + 1`), with a `gc_stress` variant for the radix conversions |
| **B4** closeout | `BigInt` out of `unsupported-features.txt`; the §13 / §14 BigInt lines; `null[1n]` names the key `'1'` | `tests/test262/unsupported-features.txt`, `emit/object.rs`, `DESIGN.md` | `tests/bigint.rs` nullish-key message; a `gc_stress` bitwise / `++` `--` test in `tests/bigint_ops.rs` |
### 6.11 Proxy and Reflect (phase 5b)

**Status: implemented** (stages P2a-P4): the proxy block, `%Proxy%`,
`Proxy.revocable`, the eleven object internal methods with their
invariants, callable / constructor proxies, the integration paths and the
`Reflect` namespace. The test262 runner no longer skips the `Proxy`,
`proxy-missing-checks` and `Reflect*` features. The subsection names the
code each part lives in; "Stages" records where the stages departed from
the plan.

**Scope.** ECMA-262 Proxy exotic objects (§10.5: all thirteen internal
methods with every invariant check, `ProxyCreate`, `Proxy.revocable`) and the
`Reflect` namespace (§28.1: all thirteen functions). None of the three
feature flags is skipped: `proxy-missing-checks` names the invariant checks that
ES2020-ES2022 added (`deleteProperty` on a non-extensible target,
`getOwnPropertyDescriptor` / `defineProperty` reporting a non-writable view of
a writable non-configurable target property; three tests). They are part of
the current spec and are implemented, so the flag is enabled with the
other two.

**Block.** A proxy is a `TYPE_PROXY = 20` block of 48 bytes
(`abi::PROXY_BLOCK_SIZE`, size class 3):

| offset | size | field |
|---|---|---|
| 0 | 4 | header: kind `20`; flags (byte 2) `8` `CONSTRUCTOR` and `32` `SELF_ALLOC`, both set iff `IsConstructor(target)` at creation (the same bit values as `FUNC_FLAG_CONSTRUCTOR` / `FUNC_FLAG_SELF_ALLOC`, `const`-asserted, so `emit_is_constructor` and the `SELF_ALLOC` test of `emit_construct` read them unchanged); byte 3 `1` `PROXY_CLASS_CALLABLE` iff `IsCallable(target)` at creation |
| 4 | 4 | `props: u32` — the proxy's own table, holding **private-name entries only** (below); `0` until the first one |
| 8 | 8 | `proto: i64` — always `null`, never read as a prototype |
| 16 | 4 | `func_idx: u32` (`= FUNC_INDEX_OFFSET`) — the funcref slot of the `ProxyCall` trampoline when callable, else `0` |
| 20 | 4 | reserved, `0` |
| 24 | 8 | `env: i64` (`= FUNC_ENV_OFFSET`) — the proxy itself when callable, else `undefined` |
| 32 | 8 | `target: i64` (`abi::PROXY_TARGET_OFFSET`) — `[[ProxyTarget]]`, `null` once revoked |
| 40 | 8 | `handler: i64` (`abi::PROXY_HANDLER_OFFSET`) — `[[ProxyHandler]]`, `null` once revoked |

A proxy is not in the object family (§5.1): it keeps the 16-byte prefix shape
so an accidental prefix read sees "no table, `null` prototype", but every
property helper tests the kind first and takes the proxy path (below). The
call fields sit where a `TYPE_FUNCTION` keeps them, so every existing
`call_indirect` sequence (`env` @24, `func_idx` @16, §4.3) dispatches a
callable proxy to its trampoline without change. Revocation writes `null` to
`target` and `handler`; the flags, the class byte, `func_idx` and `env` stay,
so a revoked callable proxy is still callable for `typeof` and `IsCallable`
(its call then throws), as the spec requires.

- **`typeof`** (`emit_typeof_node`, `emit/expr.rs`): `"function"` for a
  `TYPE_PROXY` with `PROXY_CLASS_CALLABLE`, else `"object"`.
- **`IsCallable`**: `emit_is_function` (`emit/value.rs`) becomes the
  `IsCallable` test: kind `TYPE_FUNCTION`, or kind `TYPE_PROXY` with
  `PROXY_CLASS_CALLABLE` (it takes `Helpers` and adds the proxy arm only
  under the gate below). Its call sites are `IsCallable` uses followed by a
  call through `func_idx` / `env`, which is valid on a callable proxy. The
  kind test alone is `emit_is_function_block`, used by `BoundCall`'s
  `new.target` identity test (it reads `env`); `Function.prototype.toString`
  and `bind` accept a callable proxy and test the kind themselves under the
  gate (below); `__fn_name` and `__render_function` (§6.9) already test
  the kind byte directly. The `BOUND` test of `instanceof` stays as it is: a
  proxy never carries flag `64`.
- **`IsConstructor`**: `emit_is_constructor` (`emit/expr.rs`) accepts kind
  `TYPE_FUNCTION` or (under the gate) `TYPE_PROXY` with flag `8`.
- **`emit_is_object`**: a proxy is an object (not a string or symbol)
  already.
- **GC** (`__gc_trace`, `emit/gc.rs`): a new arm for kind `20` traces raw
  `props @4` (the private entries' table), jsvals `proto @8` (always
  `null`), `env @24` (itself or `undefined`), `target @32`, `handler @40`.
  A revoked proxy traces two `null`s, so its target and handler can be
  collected. A proxy can be a `WeakMap` key, `WeakRef` target or registry
  target like any object (identity).

**Reachability gate.** A proxy exists only after `new Proxy` or
`Proxy.revocable` ran, so every proxy arm below is emitted only under a new
compile-time fact (`Helpers::proxies()`, the `ProxyGate` field set by
`emit/sections.rs::program_has_proxies` from the IR): the program names
`Proxy` (a global read or write, or the string `"Proxy"`, which covers a
member or property name and a string literal) or holds the global object at
all (`IrNode::Intrinsic(Global)`: `globalThis` or a top-level `this`, which
can find `Proxy` by a run-time key; a superset of `GlobalUse::Open`, kept
simple because no opcount program uses `globalThis`). Without it the emitted
code is byte-for-byte what it is today (no opcount change), and the
`__proxy_*` helpers are never called, so runtime shaking (§4.2) stubs them.
With it `has_accessors` is forced on: a trap runs user code at any property
access, so every member access keeps its post-call exception check.

**Keys reach traps intact.** Today a lookup-only key conversion
(`__to_key(v, 0)`) returns `0` for a string no table ever used, and `__get`,
`__has`, `__delete`, `__own_desc` and `__own_enum` answer "absent
everywhere" for key `0`. A trap must receive the real key, so under
`proxies` `emit_to_key` treats `insert = 0` as `1` (the conversion always
interns; the weak intern table reclaims such keys, §5.3), and every
`__intern_find` whose result can reach a property helper on a user object
interns instead. Keys handed to a trap are the interned string block or the
symbol block, boxed as `HEAP | key`.

**The trap protocol.** New file `emit/props/proxy.rs`; its helpers are rows
appended to the end of `registry::Extra` (no other group moves). Each
`__proxy_<m>(p, ..)` helper:

1. reads `target` (`@32`) into a rooted local **before** anything else (the
   spec reads `[[ProxyTarget]]` before `GetMethod`, and a handler getter may
   revoke the proxy);
2. calls `__proxy_trap(p: i32, name: i32) -> i64`
   (`ValidateNonRevokedProxy` + `GetMethod(handler, name)`): `handler ==
   null` throws `TypeError: Cannot perform '<name>' on a proxy that has been
   revoked`; else `__get(handler, name)` (the handler may itself be a
   proxy); `undefined` / `null` yields `undefined`, a non-callable value
   throws `TypeError: proxy trap '<name>' is not a function`;
3. on `undefined` forwards to the target's internal method (the generic
   helper, which is itself proxy-aware, so proxy chains recurse);
4. otherwise calls the trap with `this = handler` through `call_spilling`
   (`emit/builtins/array.rs`, `push_call`'s core), passing **every**
   argument (up to four:
   `target, key, value, receiver`), so under a small `K` the surplus goes in
   a fresh spread argument array, never dropped (the phase-4 args-past-`K`
   bug class);
5. checks the invariants below, each failure a `TypeError`.

The messages are five keyed `__prop_throw` modes (`emit/mod.rs`
`PROP_KEYED_MSGS`; L1 allotted `28`..`35` to this workstream):
`28` `PROP_THROW_PROXY_REVOKED` (key = the trap name: `Cannot perform
'<trap>' on a proxy that has been revoked`), `29` `PROP_THROW_PROXY_TRAP`
(key = the trap name: `proxy trap '<trap>' is not a function`), `30`
`PROP_THROW_PROXY_FALSISH` (key = the property: `proxy trap returned
falsish for property '<key>'`, a refusing `set` / `defineProperty` /
`deleteProperty` in a throwing context), `31` `PROP_THROW_PROXY_INVARIANT`
(`proxy trap result for '<key>' violates an invariant`; the traps without a
key, `ownKeys` included, pass the trap name), `32`
`PROP_THROW_PROXY_FALSISH_OP` (key = the trap name: `'<trap>' on proxy:
trap returned falsish`, a refusing `setPrototypeOf` / `preventExtensions`;
one fragment pair cannot hold both a trap and a key, hence two falsish
modes). Every helper here
is a safepoint (it runs user code and allocates): its locals follow the
`FnBuilder` root classes (§5.7), and `emit/roots.rs::helper_graph` declares
`__test_integrity` a leaf only without the gate (with it, its callee
`__proxy_is_extensible` is undeclared, so it is a safepoint);
`__protos_indexless` stays a leaf (its proxy arm only returns `0`). A helper
returning `i32` returns `0`, one returning `i64` `undefined`, with an
exception pending; callers test `$exc_pending`.

**New generic helpers** (`emit/props.rs` / `emit/props/descriptor.rs`; each
answers for any object and takes the proxy path on kind `20`):

| helper | meaning | users |
|---|---|---|
| `__get_proto(v: i64) -> i64` | `O.[[GetPrototypeOf]]()`: `proto @8`, or `__proxy_get_proto` | `Object.getPrototypeOf`, `Reflect.getPrototypeOf`, the `__proto__` getter, `isPrototypeOf`, `__lookupGetter__` / `__lookupSetter__`, `bind` (`function.rs`, the target's `proto`), the `instanceof` walks, `__for_in_keys` |
| `__own_prop(obj: i64, key: i32, list: i32) -> i32` | `O.[[GetOwnProperty]](P)` as a complete record `[desc, value, get, set]` (§6.3) appended to the array `list`; `1` present, `0` absent | the proxy invariant checks, `__has_own`'s and `__own_enum`'s proxy arms, the receiver step of `OrdinarySet`, `TestIntegrityLevel` and `SetIntegrityLevel` over a proxy |
| `__desc_compatible(ext: i32, d: i32, cur: i32) -> i32` | `IsCompatiblePropertyDescriptor(Extensible, Desc, Current)` (`ValidateAndApplyPropertyDescriptor` with `O = undefined`); `d` / `cur` are the element addresses of two four-slot records (the caller keeps their array rooted); `cur = 0` is "absent" | `__proxy_own_prop`, `__proxy_define_own` |
| `__is_array(v: i64) -> i32` | `IsArray`: kind `TYPE_ARRAY`; a proxy recurses into its target, a revoked one throws `PROXY_REVOKED` (key `"IsArray"`) | `Array.isArray`, `Object.prototype.toString`, `JSON.stringify` (value and replacer list), the `JSON.parse` reviver, `ArraySpeciesCreate`, `IsConcatSpreadable`, `FlattenIntoArray` |
| `__set_bool(target: i64, key: i32, v: i64, recv: i64) -> i32` | `target.[[Set]](P, V, Receiver)` returning the boolean: a third body of `emit_set_from` (the `__super_set` shape) in a non-throwing mode, where each failure of the receiver step (`emit_receiver_set`: a primitive receiver, an own accessor or read-only property, a rejected define) returns `0` | `Reflect.set`, the trap-less `[[Set]]` of a proxy |

`__own_prop`'s ordinary arm reads the same state as `__own_desc` (one
emitter, `descriptor.rs::emit_own_lookup`, shared by both). `__desc_compatible`
(`descriptor.rs`, an `Extra` row, `__get_proto` / `__own_prop` / `__set_bool`
too) restates `__define_own`'s non-configurable rejection rules over two
records: a record argument is the address `x` whose fields sit at `x +
ELEMS_DATA_OFFSET + 8k` (element store base plus `32 * index`). Factoring
`__define_own`'s validation block, which works on a table entry and
interleaves the array `length` and dictionary-mode paths, was not worth the
churn in a shared helper. Records follow the descriptor encoding of §6.3;
"complete" means all three presence bits plus value or `get` / `set`
(`CompletePropertyDescriptor`).

**Internal methods.** For each, the proxy helper, the existing helpers that
hand off to it, the trap-less forward, and the invariant checks (each a
`TypeError`, `PROXY_INVARIANT` unless noted):

- **[[GetPrototypeOf]]** — `__proxy_get_proto(p) -> i64`; forward
  `__get_proto(target)`. The result must be an object or `null`; when
  `IsExtensible(target)` is false it must be `SameValue` with
  `target.[[GetPrototypeOf]]()`.
- **[[SetPrototypeOf]]** — `__proxy_set_proto(p, v) -> i32`; hook: the entry of
  `__set_proto` (whose result becomes `PROP_THROW_PROXY_FALSISH` for a
  falsish proxy result; `0` stays success), and its cycle walk stops (no
  cycle) at a proxy link (`OrdinarySetPrototypeOf` step 8.c.i); forward
  `__set_proto(target, v) == 0`. `ToBoolean(trap result)`; false returns
  false; when `IsExtensible(target)` is false, `v` must be `SameValue` with
  `target.[[GetPrototypeOf]]()`. Callers of `__set_proto`
  (`Object.setPrototypeOf`, the `__proto__` setter, `Reflect.setPrototypeOf`)
  now check `$exc_pending` after it.
- **[[IsExtensible]]** — `__proxy_is_extensible(p) -> i32`; hook: the entry of
  `__test_integrity` at level `0`; forward `__test_integrity(target, 0)`.
  `ToBoolean(trap result)` must equal `IsExtensible(target)`. Levels `1` /
  `2` on a proxy run the generic `TestIntegrityLevel`: `IsExtensible` (true
  → false), then `__own_keys(p, OWN_ALL_KEYS)` and one `__own_prop` per key.
- **[[PreventExtensions]]** — `__proxy_prevent_ext(p) -> i32`; hook: the entry
  of `__set_integrity`, which now returns `i32` status (`1`, or `0` when a
  proxy refuses); forward `__set_integrity(target, 0)`. A true result with
  `IsExtensible(target)` still true throws. Levels `1` / `2` on a proxy run
  the generic `SetIntegrityLevel`: `PreventExtensions` (false → status `0`),
  `__own_keys(p, OWN_ALL_KEYS)`, then per key `__define_own` of
  `{configurable: false}` (sealed) or, after `__own_prop`, `{configurable:
  false}` for an accessor / `{configurable: false, writable: false}` for data
  (frozen); a rejection throws (`DefinePropertyOrThrow`).
  `Object.preventExtensions` / `seal` / `freeze` throw `PROXY_FALSISH` on
  status `0`; `Reflect.preventExtensions` returns it.
- **[[GetOwnProperty]]** — `__proxy_own_prop(p, key, list) -> i32`; hooks:
  `__own_prop`, `__own_desc` (which builds `FromPropertyDescriptor` from the
  record), `__has_own`, `__own_enum`; forward `__own_prop(target, ..)`. The
  trap result must be an object or `undefined`. `undefined`: absent target
  property → absent; a non-configurable target property, or a present one
  on a non-extensible target, throws. Otherwise `__to_desc` (ToPropertyDescriptor)
  then completed in place; `__desc_compatible(IsExtensible(target), result,
  target)` must hold; a non-configurable result needs a present,
  non-configurable target property, and a non-configurable non-writable
  result needs a non-writable target property (`proxy-missing-checks`).
- **[[DefineOwnProperty]]** — `__proxy_define_own(p, key, desc, value, get,
  set) -> i32` (`DEFINE_OK` / `DEFINE_REJECT`); hooks: the entry of
  `__define_own`, and the entry of `__define` for a non-private key (the
  complete record built from `attrs`, an `ACCESSOR` value split into
  `get` / `set`; `DEFINE_REJECT` throws `PROXY_FALSISH`); forward
  `__define_own(target, ..)`. The trap receives `FromPropertyDescriptor(Desc)`
  with only the present fields (a fresh object). `ToBoolean` false →
  `DEFINE_REJECT`. Then with `targetDesc = __own_prop(target)`: absent and
  target not extensible throws; absent and `configurable: false` requested
  throws; present and not `__desc_compatible(ext, Desc, targetDesc)` throws;
  `configurable: false` requested over a configurable target property throws;
  a non-configurable writable target data property with `writable: false`
  requested throws (`proxy-missing-checks`). `__define_own` may now leave an
  exception pending on any object (until now only an array's `length`), so
  every caller that can receive a user object checks `$exc_pending`:
  `Object.defineProperty` / `defineProperties` / `create`
  (`object_static.rs::emit_apply`), `emit_receiver_set`,
  `json.rs`'s reviver `CreateDataProperty`, and the `CreateDataPropertyOrThrow`
  arm in `array.rs` (the non-array branch at the `__prop_find` probe, which
  takes `__define_own` on a proxy instead of reading a table).
- **[[HasProperty]]** — `__proxy_has(p, key) -> i32`; hook: every level of the
  `__has` walk; forward `__has(key, target)`. On a false result, a present
  target property that is non-configurable, or present on a non-extensible
  target, throws.
- **[[Get]]** — `__proxy_get(p, key, recv) -> i64`; hooks: every level of
  `emit_get_from` (`__get` and `__super_get`) and of `__symbol_get` (a proxy
  level is tested before the `HAS_SYMBOL_KEYS` shortcut); forward
  `__super_get(target, key, recv)` (that is `[[Get]]` with an explicit
  receiver). With a non-configurable target property: a non-writable data
  property needs `SameValue(result, value)`; an accessor without a getter
  needs `undefined`.
- **[[Set]]** — `__proxy_set(p, key, v, recv) -> i32`; hooks: every level of
  `emit_set_from` (`__set`, `__super_set`, `__set_bool`; in the throwing
  forms a `0` throws `PROXY_FALSISH`) and of `__proto_set` (the element-store
  chain walk of §5.4: a proxy owner consumes the store, a `0` throws);
  forward `__set_bool(target, key, v, recv)`. `ToBoolean` false → `0`. With a
  non-configurable target property: a non-writable data property needs
  `SameValue(v, value)`; an accessor without a setter throws.
- **[[Delete]]** — `__proxy_delete(p, key) -> i32`; hook: the entry of
  `__delete`, which gains a `strict: i32` parameter and an `i32` result
  (`strict = 1`: the `delete` operator and `DeletePropertyOrThrow` in the
  `Array.prototype` bodies, throwing as today, a falsish proxy
  `PROXY_FALSISH`; `strict = 0`: `Reflect.deleteProperty` and the
  `JSON.parse` reviver, which return the boolean); forward `__delete(target,
  key, 0)`. On a true result, a non-configurable target property throws, and
  so does a present one on a non-extensible target (`proxy-missing-checks`).
- **[[OwnPropertyKeys]]** — `__proxy_own_keys(p, mode) -> i64` (a fresh dense
  array, already filtered by the `__own_keys` `mode` below); hook: the entry
  of `__own_keys`; forward `__own_keys(target, OWN_ALL_KEYS)`. `CreateListFromArrayLike(trapResult, « String, Symbol »)`: a
  non-object throws; `ToLength(Get(r, "length"))` then `Get(r, i)` per index;
  an element that is neither a string nor a symbol throws; strings are
  interned (`__intern`), so the keys compare by address and the array is a
  key array like every `__own_keys` result; a duplicate throws (a quadratic
  scan; key lists are short). Then `IsExtensible(target)` and the target's
  keys split by `__own_prop` into configurable / non-configurable: an
  extensible target with no non-configurable key returns the list; every
  non-configurable key must be in it; for a non-extensible target every
  target key must be in it and nothing else may be.
  `__proxy_own_keys` then filters by `mode`: `OWN_ALL` the strings, `OWN_SYMBOLS`
  the symbols, `OWN_ALL_KEYS` everything (trap order, not sorted);
  `OWN_KEYS` / `OWN_VALUES` / `OWN_ENTRIES` are `EnumerableOwnProperties`:
  per string key one `__proxy_own_prop` (kept when present and enumerable)
  immediately followed, for values and entries, by `__proxy_get(p, k, p)`.
- **[[Call]] / [[Construct]]** — below.

**Ordinary walks meeting a proxy.** A prototype chain may contain a proxy at
any level (`Object.setPrototypeOf(o, p)`, `Object.create(p)`, a class whose
`prototype`'s `[[Prototype]]` is a proxy, a proxy installed in a realm
prototype's chain). Each loop tests the kind at the top of every iteration,
the first included, and hands the rest of the operation to the proxy with the
original receiver: `__get` / `__super_get` / `__symbol_get` →
`__proxy_get(q, key, recv)`; `__set` / `__super_set` / `__set_bool` /
`__proto_set` → `__proxy_set(q, key, v, recv)`; `__has` →
`__proxy_has(q, key)`; `__for_in_keys` → the proxy level enumerates through
traps (below); `__protos_indexless` → `0` (a proxy may answer any index, so
holes take `__proto_elem`, whose `__has` / `__get` reach the traps). The
receiver step of `OrdinarySet` (`emit_receiver_set`, used by `__super_set`
and `__set_bool`) meets a proxy **receiver** when a trap-less proxy forwards
`[[Set]]` to an ordinary target: it then runs `OrdinarySetWithOwnDescriptor`
steps 2.c-e through the proxy — `__own_prop(recv)` (the
`getOwnPropertyDescriptor` trap), an accessor or non-writable result fails,
a present one is `__define_own(recv, key, {value})`, an absent one
`CreateDataProperty` (`__define_own` with `DESC_DATA_DEFAULT`), both the
`defineProperty` trap. The other single-object helpers (`__delete`,
`__define`, `__define_own`, `__own_desc`, `__own_keys`, `__own_enum`,
`__has_own`, `__set_proto`, `__set_integrity`, `__test_integrity`,
`__copy_data`'s source) test the kind once at entry. `__prop_find` stays a raw
table probe and is never called on a proxy except for a private key
(`__private_find`); its three other non-intrinsic callers get a proxy arm:
`json.rs` (the reviver's configurable probe: on a proxy, `__delete(.., 0)`
with the result ignored), `iterator.rs` (`SetterThatIgnoresPrototypeProperties`:
`__own_prop` on a proxy `this`), and `array.rs` (above).

- **`for-in`** (`__for_in_keys`): at a proxy level, `__proxy_own_keys`, then
  for each string key not yet seen one `__proxy_own_prop` (an enumerable
  present key is added; every key is marked seen), then the next level is
  `__proxy_get_proto(p)`. The snapshot rule is unchanged.
- **`CopyDataProperties`** (`__copy_data`, object spread and rest): a proxy
  source takes the generic order — `__own_keys(src, OWN_ALL_KEYS)`, then per
  key not excluded `__own_prop` (enumerable?) and `__get(src, k)`.
- **`Object.assign`, `defineProperties` / `create`,
  `getOwnPropertyDescriptors`, object rest**: already `__own_keys(..,
  OWN_ALL_KEYS)` followed by per-key `__own_enum` / `__own_desc` / `__get`,
  which become trap calls on a proxy in spec order.

**Callable and constructor proxies.** `new Proxy(target, handler)` with a
callable target stores the funcref slot of the new internal builtin
`Builtin::Proxy(ProxyOp::Call)` (a trampoline like `BoundCall`, `emit/builtins/proxy.rs`)
in `func_idx` and the proxy in `env`. Every call site kind reaches it through
the unchanged `call_indirect` sequence once its callability test accepts the
proxy: direct-value and method calls (`emit_call_indirect`, the method call
paths, the shared `MethodCall` builtin), `new` and `super(..)`
(`emit_construct`: `IsConstructor` reads flag `8`, and flag `32` skips the
`this` allocation, since a proxy's `[[Construct]]` never runs
`OrdinaryCreateFromConstructor` itself), spread calls (`emit_call_spread`,
`$__spread_args`), builtin-to-user calls (`call_spilling` / `push_call`:
array callbacks, `forEach`, replacers, `then` reactions through `__invoke1`,
accessors through `emit_accessor_call`, `@@toPrimitive`, `@@hasInstance`,
iterator methods), bound functions (`BoundCall` calls or constructs its
target), and `Function.prototype.call` / `apply`. The trampoline:

1. decodes `argc` (the construct marker, §7.6) and, before any user code
   runs, copies the `n` arguments (`K` slots, then `$__spread_args` past
   `K`; `store_args` of `emit/builtins/function.rs`) into a fresh array, so
   no `$__spread_args` snapshot is needed later; it snapshots `$__this` and
   `$__new_target` at entry;
2. reads `target`; `__proxy_trap(p, "apply" | "construct")`;
3. **call**, trap absent: `call_with_array(target, this, args)`; trap present:
   `Call(trap, handler, «target, this, argsArray»)` through `push_call`;
4. **construct** (only reachable with flag `8`, so the target is a
   constructor), trap absent: an `IrNode::Construct { callee: target, spread:
   argsArray, new_target: Some(newTarget) }` exactly like `bound_construct`
   (so a non-`SELF_ALLOC` target allocates `this` from `Get(newTarget,
   "prototype")`, which is the `get` trap when `newTarget` is the proxy
   itself); trap present: `Call(trap, handler, «target, argsArray,
   newTarget»)` whose result must be an object (else `TypeError: proxy
   [[Construct]] must return an object`).

`new.target` is whatever the construct site set: the proxy for `new P()`,
the derived class for `super(..)` in `class B extends P`, the explicit one
for `Reflect.construct`. `class B extends P` needs nothing new: the heritage
check is `emit_is_constructor`, `Get(P, "prototype")` is the `get` trap, and
`GetSuperConstructor` reads `B`'s ordinary `proto`.

**Private names** act on the proxy object itself and never trap (§7.6): the
proxy's own `props` table holds its private entries, so `__private_find`,
`__private_get`, `__private_set`, `__private_brand` and `__private_in` work
unchanged; `__private_add` reaches `__define`, whose private-key test runs
before its proxy test (raw append, no not-extensible check). No other walk
reads a proxy's table: they all take the proxy path first, so the entries
stay invisible. A base constructor returning a proxy lets a derived class
install fields: public ones through `__define` (the `defineProperty` trap),
private ones on the proxy's table.

**Revocation.** `Proxy.revocable(target, handler)`
(`Builtin::Proxy(ProxyOp::Revocable)`) runs `ProxyCreate`, makes the revoke
function — a `TYPE_FUNCTION` with the funcref slot of the internal
`Builtin::Proxy(ProxyOp::Revoke)` and `env` = the proxy
(`emit_settler_value`, as the promise combinators make their element
functions), info name `""`, `length` 0, not a constructor — and returns a
fresh ordinary object with own `proxy` then `revoke` (`CreateDataProperty`).
The revoke function writes `null` into the proxy's `target` and `handler` and
returns `undefined`; a second call writes them again (idempotent; the spec
clears `[[RevocableProxy]]`, which only frees the 48-byte block earlier).
Every trap on a revoked proxy throws `PROXY_REVOKED` from `__proxy_trap`,
`IsArray` of one throws, and the renderer prints it as `<Revoked Proxy>`.

**`ProxyCreate`** (`Builtin::Proxy(ProxyOp::Ctor)`, `SELF_ALLOC`): called without `new`
throws `TypeError: Constructor Proxy requires 'new'`; a non-object target or
handler throws `TypeError: Cannot create proxy with a non-object as target or
handler` (a revoked proxy is an acceptable target or handler); then the block
above (`gc::emit_alloc_const`, 48 bytes), callable / constructor bits from
`emit_is_function` / `emit_is_constructor` of the target (which accept a
proxy target); a callable one gets the `ProxyOp::Call` funcref slot in
`func_idx` and itself in `env`. Both arguments are
read before any user code runs (there is none), so no snapshot.

**Integration paths.**

- **`Array.isArray`** (`IrNode::IsArray`, `emit/expr.rs`): the inline
  `TYPE_ARRAY` test, then `__is_array` for a `TYPE_PROXY` operand.
- **`instanceof`**: `GetMethod(C, @@hasInstance)` is `__symbol_get` (the `get`
  trap on a proxy `C`); `IsCallable(C)` accepts a callable proxy;
  `OrdinaryHasInstance` reads `Get(C, "prototype")` (trap) and its chain walk
  (`emit_proto_chain_match`, `emit/object.rs`, used by the generic tail and
  by the sealed fast arm) calls `__get_proto` for every step under the gate
  instead of loading `proto @8` (the `getPrototypeOf` trap on a proxy link,
  exceptions propagated). `isPrototypeOf` walks the same way.
- **`Object.prototype.toString`** (`builtins/object.rs::emit_to_string`): for a
  proxy, the builtin tag comes first (spec order): `__is_array` (may throw) →
  `"Array"`, else `PROXY_CLASS_CALLABLE` → `"Function"`, else `"Object"`;
  then `Get(O, @@toStringTag)` (the trap) overrides it when a string.
- **`JSON.stringify`** (`builtins/json.rs`, `__json_rep`): a callable proxy is
  omitted like a function; `__is_array` decides the array form, whose
  elements of a proxy come from `Get(value, "length")` and `Get(value, i)`
  (no store read); an object proxy's keys are `__own_keys(value, OWN_KEYS)`
  (one `getOwnPropertyDescriptor` trap per key) and its values `__get`; the
  replacer's property list uses `__is_array` too. A proxy is never a
  wrapper (it has no `[[NumberData]]` etc.), so no unwrapping. The
  `JSON.parse` reviver (`__json_revive`) likewise uses `__is_array`, `Get`
  of `length`, `__own_keys(.., OWN_KEYS)`, `__delete(.., 0)` and
  `__define_own`.
- **`Object.keys` / `values` / `entries`, `getOwnPropertyNames` /
  `Symbols`, `Reflect.ownKeys`, object spread / rest, `for-in`**: the
  `__own_keys` / `__copy_data` / `__for_in_keys` arms above.
- **`Array.prototype.concat`**: `IsConcatSpreadable` is `__symbol_get(O,
  @@isConcatSpreadable)` (trap), then `__is_array`; a proxy element always
  takes the generic `LengthOfArrayLike` + per-index `HasProperty` / `Get`
  path, as every `Array.prototype` body does for a proxy receiver (`is_array`
  and `indexless_array` in `builtins/array.rs` stay `TYPE_ARRAY` only, so no
  store fast path ever sees a proxy). `ArraySpeciesCreate` uses `__is_array`
  before `Get(O, "constructor")`.
- **`Function.prototype.toString`** of a callable proxy:
  `function () { [native code] }` (the `NativeFunction` form without a name);
  a non-callable proxy throws the existing `TypeError`.
- **`Function.prototype.bind`** of a callable proxy: `proto` from
  `__get_proto(target)` (the `getPrototypeOf` trap, then the `length` / `name`
  reads, in spec order), `CONSTRUCTOR | SELF_ALLOC` from flag `8`.
- **Renderer** (§6.9, `emit/render.rs`): no trap ever runs. `__render_heap`
  unwraps a `TYPE_PROXY` to its target (repeatedly, for proxy chains) and
  renders the target as Node does without `showProxy`
  (`{ a: 1 }`, `[Function: f]`); a revoked proxy prints `<Revoked Proxy>`.
- **`ToPrimitive`, `ToPropertyKey`, `GetIterator`, promise thenables,
  `Symbol.iterator` / `asyncIterator` / `species` / `match` reads**: all go
  through `__get` / `__symbol_get`, hence through the traps.

**Fast paths and guards** (§6.4). Proxies cannot be watched intrinsics and
are rejected by every receiver test, so most guards are already safe:

- `ProtoGuard`, the `GetIterator` fast paths, `.length`, the
  `ArraySpeciesCreate` / `SpeciesConstructor` identity tests and the
  `NamespaceGuard` match a kind byte or an intrinsic identity a proxy never
  has: a proxy receiver takes the generic path (`MethodCall`, the protocol,
  `__get`). Safe, no change.
- A proxy installed **behind** a realm prototype (`Object.setPrototypeOf(
  Array.prototype, p)`) goes through `__set_proto` on a watched table and sets
  its dirty bit; writes through a trap-less proxy whose target is a realm
  prototype land in `__set` / `__define` / `__define_own` / `__delete` on the
  watched table and set its bit; `%Object.prototype%` cannot take a new
  prototype at all. Safe, no change.
- `__protos_indexless`: **needs the proxy arm** (`0` on a proxy link);
  without it a proxy prototype reads as "no index keys" and an array hole
  would skip its `get` trap. This is the only fast-path guard that needs a
  new test; no new dirty bit is needed.
- `__symbol_get`: **needs the proxy arm** before its `HAS_SYMBOL_KEYS`
  shortcut (a proxy's `props` is `0`).
- The `instanceof` fast arm: **needs** the `__get_proto` step in
  `emit_proto_chain_match` (above); its intact test reads only static realm
  constructors and stays.
- There are no inline caches and no shape assumptions on objects that are
  not freshly allocated literals or class instances (§13), and member access
  always calls `__get` / `__set` except the kind-tested `.length`.
- The global object is a static ordinary block and is never a proxy. Its
  prototype chain may hold one (`Object.setPrototypeOf(globalThis, p)`); an
  unresolved name is then `HasProperty` + `Get` on the global object (§6.4),
  so the `has` and `get` traps run through the ordinary hand-off. Sealed
  names never consult the global object (escaping `globalThis` unseals
  them).
- Runtime shaking: the proxy arms exist only under `proxies` (above);
  `Proxy` and `Reflect` are realm entries kept by their key lookups like
  every global.

**Reflect** (`emit/builtins/reflect.rs`, a new `ReflectFn` enum of thirteen
variants in `emit/realm.rs` — distinct from the existing `ReflectOp`, which
names the `Object` statics). Every function throws `TypeError: Reflect.<f>
called on non-object` for a non-object target, and reads all its arguments
into locals at entry, before `ToPropertyKey` or any other user code runs
(else it would read a clobbered `$__spread_args` past `K`):

| function | `length` | steps |
|---|---|---|
| `apply(f, thisArg, args)` | 3 | `IsCallable(f)` else `TypeError`; `CreateListFromArrayLike(args)` (a non-object, `undefined` included, throws); `call_with_array` |
| `construct(F, args, newTarget = F)` | 2 | `IsConstructor(F)`, `IsConstructor(newTarget)` (when passed, `undefined` included) else `TypeError`; the list; `IrNode::Construct { callee: F, spread: list, new_target: Some(newTarget) }` (`function.rs::bound_construct`) |
| `defineProperty(t, k, attrs)` | 3 | `__to_key`, `__to_desc`, `__define_own == DEFINE_OK` (no throw on rejection) |
| `deleteProperty(t, k)` | 2 | `__delete(t, k, 0)` |
| `get(t, k, receiver = t)` | 2 | `__super_get(t, k, receiver)` (the default only when fewer than three arguments are passed) |
| `getOwnPropertyDescriptor(t, k)` | 2 | `__own_desc` |
| `getPrototypeOf(t)` | 1 | `__get_proto` |
| `has(t, k)` | 2 | `__has` |
| `isExtensible(t)` | 1 | `__test_integrity(t, 0)` |
| `ownKeys(t)` | 1 | `__own_keys(t, OWN_ALL_KEYS)`: integer keys ascending, then strings, then symbols, each in insertion order (a proxy: the checked trap list); a `LAZY` function is materialized first and an array's `length` follows its indices (`object_static.rs::own_key_list`, shared with `Object.getOwnPropertyNames`) |
| `preventExtensions(t)` | 1 | `__set_integrity(t, 0)` status |
| `set(t, k, v, receiver = t)` | 3 | `__set_bool(t, k, v, receiver)` (the key interned; the default only when fewer than four arguments are passed) |
| `setPrototypeOf(t, proto)` | 2 | `proto` an object or `null` else `TypeError`; `__set_proto(t, proto) == 0` |

`CreateListFromArrayLike` for `apply` / `construct` is the list-building part
of `Function.prototype.apply` (`builtins/function.rs::emit_apply`), factored
into `function.rs::create_list` (`apply` itself keeps accepting
`undefined` / `null` as "no arguments"). The other error texts are plain
`TypeError`s (`Reflect.apply target is not a function`, `Reflect.construct
target is not a constructor` / `newTarget is not a constructor`); `setPrototypeOf`'s
prototype check is `Object.setPrototypeOf`'s (`PROP_THROW_PROTO_TYPE`), so
no `__prop_throw` mode is added (`33`..`35` stay free).

**`Reflect.construct` and builtin constructors.** Builtin constructors
already receive `newTarget` through `$__new_target` and compute
`GetPrototypeFromConstructor` from it (`ctor.rs::Body::proto`: `Get(new.target,
"prototype")` when constructing and an object, else the realm prototype of
the constructor — the newTarget's realm is the only realm). So nothing in the
constructor bodies changes: `Reflect.construct` sets `$__new_target` like any
`new` with an explicit `new_target`. Per family: user classes and plain
functions allocate `this` from `Get(newTarget, "prototype")` at the construct
site (`emit_construct`), derived classes pass `newTarget` on through
`super(..)`; the Error family (`AggregateError` included), `Array`,
`Promise`, `Map` / `Set`, `WeakMap` / `WeakSet` / `WeakRef` /
`FinalizationRegistry`, `Date`, and `Boolean` / `Number` / `String` (their
wrapper under `new`) use `Body::proto`; `Object` with a `newTarget` other
than `%Object%` returns the allocated `this` (`ctor.rs::object`; its
prototype falls back to `%Object.prototype%`, `Object`'s own); `Symbol`
throws (`new` is refused); `Function` throws (unsupported); `Iterator` is
abstract unless `newTarget` differs; a bound function constructs its target
with `newTarget` per §5.5; a constructor proxy forwards `newTarget` to its
`construct` trap or target. The bodies that read arguments after `Get(newTarget,
"prototype")` (Error, AggregateError, Array, Date, Promise) already snapshot
`$__spread_args` (`Body::snapshotted`); `Reflect.construct` always passes its
arguments as a spread array, so these snapshots become reachable with a
user-code `prototype` read — the case P3 tests (below).

**Realm entries** (`emit/realm.rs`, appended after `%OutOfMemoryError%` in
image order; no watch bit; not inline namespaces):

- `%Proxy%` — a new `Intrinsic::Proxy`, not a `BuiltinCtor` (it has no
  `prototype`): a static `TYPE_FUNCTION` block, `CONSTRUCTOR | SELF_ALLOC`,
  `func_idx` = the `Builtin::Proxy(ProxyOp::Ctor)` slot, `proto`
  `%Function.prototype%`; own `length` 2, `name` `"Proxy"` (configurable
  only), `revocable` (`Builtin::Proxy(ProxyOp::Revocable)`, `length` 2, a
  method).
- `%Reflect%` — a new `Intrinsic::Reflect`, `CLASS_ORDINARY`, `proto`
  `%Object.prototype%`; the thirteen methods (`Builtin::Reflect(ReflectFn)`)
  and `@@toStringTag` `"Reflect"` (`{writable: false, enumerable: false,
  configurable: true}`).
- Global object: `Proxy` and `Reflect`, writable, configurable, not
  enumerable.
- The proxy builtins are one variant `Builtin::Proxy(ProxyOp)` (`Ctor`,
  `Revocable`, `Revoke`, and `Call`, the call trampoline)
  dispatched by `builtins/proxy.rs::emit`. The revoke function and the
  trampoline are internal (installed nowhere, so shaking keeps them). The
  `ReflectFn` bodies dispatch from `builtins/mod.rs`.

**Stages** (files in scope per stage; each adds, never reorganizes, in the
shared files):

- **P2a — proxy block and object internal methods.** `abi.rs`
  (`TYPE_PROXY = 20`, the offsets, `PROXY_CLASS_CALLABLE`, the `const`
  asserts), `emit/props/proxy.rs` (new: `__proxy_trap` and the eleven
  object `__proxy_*` helpers), `emit/props.rs` and
  `emit/props/descriptor.rs` (the hand-offs, `__get_proto`, `__own_prop`,
  `__desc_compatible`, `__set_bool`, `__delete` / `__set_integrity`
  signatures, `__to_key` interning), `emit/registry.rs` (rows), `emit/mod.rs`
  (`Helpers::proxies`, accessors, `PROP_THROW_PROXY_*`), `emit/roots.rs`,
  `codegen/globals.rs` (the `proxies` fact), `emit/gc.rs` (trace arm),
  `emit/render.rs` (unwrap / `<Revoked Proxy>`), `emit/realm.rs` +
  `emit/builtins/proxy.rs` (new) + `builtins/mod.rs` (`%Proxy%`,
  `ProxyOp::{Ctor, Revocable, Revoke}`; a callable target already
  sets the class byte but calls throw until P2b), the `Object` statics and
  `Object.prototype` methods that switch to the new helpers
  (`builtins/object_static.rs`, `builtins/object.rs`), the three
  `__prop_find` callers, and `emit/expr.rs` for `typeof`.
  **Delivered**, with these departures: the gate is computed in
  `emit/sections.rs` (not `codegen/globals.rs`) and also forces
  `has_accessors`; the builtins are `Builtin::Proxy(ProxyOp)`;
  `__proxy_own_keys` takes the `mode` and filters; `__desc_compatible`
  restates the rejection rules instead of sharing `__define_own`'s block;
  five `__prop_throw` modes (`28`..`32`) instead of four. The `__prop_find`
  callers: `json.rs`'s reviver probe became `__delete(.., 0)` (no probe
  left), `iterator.rs`'s ignoring setter asks `__own_prop` under the gate,
  `array.rs`'s `CreateDataPropertyOrThrow` takes `__define_own` on a proxy.
  Callers of helpers that a trap can now make throw test `$exc_pending`
  under the gate (`for-in`, `Object.keys`, `defineProperties`,
  `getOwnPropertyDescriptors`, `assign`, `JSON.stringify`'s object branch,
  `__copy_data`, whose exclusion test also moves before the
  `[[GetOwnProperty]]` under the gate). An object proxy in
  `JSON.stringify` already serializes through `__own_keys` and `__get`
  (traps); `Object.prototype.toString`, `Array.isArray`, `instanceof` and
  calls still see the raw block until P2b. `src/abi_drift_tests.rs`,
  `tests/proxy.rs`, `tests/shake.rs` and `tests/wasi_shell.rs` carry the
  tests.
- **P2b — callable / constructor proxies and integration paths.**
  the call trampoline (`Builtin::Proxy(ProxyOp::Call)`, `builtins/proxy.rs`), `emit_is_function` /
  `emit_is_function_block` / `emit_is_constructor` (`emit/value.rs`,
  `emit/expr.rs`) and the five internals-reading sites, `__is_array` and its
  users (`emit/expr.rs` `IsArray`, `builtins/array.rs`, `builtins/json.rs`,
  `builtins/object.rs` `toString`), `emit/object.rs` (`instanceof` walk,
  `isPrototypeOf`), `builtins/function.rs` (`toString`, `bind`).
  **Delivered**, with these departures: `emit_is_function` and
  `emit_is_constructor` take `Helpers` and emit their proxy arm only under
  the gate (so without it every call site is byte-identical);
  `emit_is_function_block` replaces it only at `BoundCall`'s `new.target`
  identity test, while `toString` / `bind` keep the widened test and branch
  on the kind under the gate. `__is_array` is an `Extra` row in
  `emit/props/proxy.rs` (after `ProxyOwnKeys`). The trampoline reuses
  `function.rs`'s `store_args`, `call_with_array` and `bound_construct`
  (its locals match the bound trampoline's), and the non-object `construct`
  result is a plain `TypeError` text, so no new `__prop_throw` mode
  (`33`..`35` stay free). `FlattenIntoArray` asks `IsArray` only while depth
  remains (a revoked proxy element of `flat(0)` does not throw).
  `__json_revive` takes the static strings (the `length` key) and walks a
  proxy value like an object or, when `IsArray`, by `length` and index;
  `JSON.stringify`'s replacer list and array form read a proxy by `Get` of
  `length` and each index. `Object.prototype.toString` computes a proxy's
  builtin tag before the `@@toStringTag` read; other objects keep the old
  order (no other builtin tag can throw).
- **P3 — Reflect.** `emit/builtins/reflect.rs` (new), `ReflectFn` and
  `%Reflect%` in `emit/realm.rs`, `builtins/mod.rs`, the
  `CreateListFromArrayLike` factoring in `builtins/function.rs`. No
  constructor body changes (above).
  **Delivered**, with these departures: `String` / `Number` / `Boolean` are
  now `SELF_ALLOC` (`realm.rs::Intrinsic::flags`): their bodies already
  return a wrapper whose prototype comes from `Body::proto`, and without the
  flag the construct site also allocated an unused `this` from
  `Get(newTarget, "prototype")`, reading a user-code `prototype` twice.
  `Reflect.ownKeys` shares `Object.getOwnPropertyNames`'s body
  (`object_static.rs::own_key_list`, parameterized by the `__own_keys`
  mode) for the `LAZY` and array-`length` handling. The `ReflectFn` bodies
  dispatch through `builtins/mod.rs::emit_handle_family` (the main dispatch
  is at its line limit). No registry helper rows. The array-`length`
  insertion is `object_static.rs::emit_with_array_length`, which
  `__proxy_own_keys` also uses: the target's list (trap-less forward and the
  `ownKeys` invariant check) materializes a `LAZY` function target and
  includes an array target's `length`.
- **P4 — closeout.** `Proxy`, `proxy-missing-checks` and `Reflect*` are
  removed from `tests/test262/unsupported-features.txt`; the baseline refresh
  comes from the CI run. **Delivered**, with one fix to P2a: the target's
  `[[OwnPropertyKeys]]` list (the trap-less forward and the `ownKeys`
  invariant check) used `__own_keys(target, OWN_ALL_KEYS)`, which omits an
  array target's `length` and a `LAZY` function target's keys; it now
  materializes the target and inserts `length` (above, P3). The §13 per-family
  failure table keeps its counts until the next CI recount.

A different split would not be materially better: P2a is the largest stage
(every property helper), but its arms share one protocol helper and one test
file, and callable proxies only need the call-site test change plus one
trampoline, so they review well on their own.

**Tests** (written failing-first; `common::assert_output` /
`assert_uncaught` / `assert_same_under_gc_stress`):

- P2a — `crates/jsc-core/tests/proxy.rs` (new): one test per trap (forward
  when absent, trap result, every invariant `TypeError`, the three
  `proxy-missing-checks` cases), traps reached through a prototype chain
  (`Object.create(p)` get / set / has, an array hole over a proxy prototype,
  `super.x` over a proxy, `for-in` over a proxy level), the receiver step
  (`set` through a proxy without a `set` trap over an ordinary
  target firing its `getOwnPropertyDescriptor` and `defineProperty` traps
  on the proxy as receiver), `Object.keys` / `assign` / spread / rest trap order, freeze /
  seal / `isFrozen` over traps, ownKeys result checks (non-array, wrong
  element type, duplicates, missing non-configurable key), never-interned
  computed keys reaching a trap, revocation (each trap throws, `revoke`
  idempotent, `Proxy.revocable` result shape), `Proxy` without `new`,
  non-object arguments, `Proxy.length` / `name` / no `prototype`, private
  fields and methods on a proxy returned by a base constructor (no trap
  fires), and `console.log` of proxies (`crates/jsc-core/tests/console.rs`).
  `gc_stress` variants for the trap-calling paths that allocate (descriptor
  objects, key lists, `push_call` spill arrays, `for-in` snapshots).
  `crates/jsc-core/tests/shake.rs`: a program without `Proxy` keeps no
  `__proxy_*` body.
- P2b — `proxy.rs`: `apply` / `construct` traps and their absence, `typeof`,
  calls from every call-site kind (plain, method, spread with more than `K`
  arguments, `new`, `super(..)` from `class B extends P`, `Array.prototype.map`
  callback, `Function.prototype.call` / `apply`, `bind` of a proxy, accessor
  and `@@toPrimitive` proxies), `new.target` inside the target, a construct
  trap returning a primitive, revoked callable proxies (`typeof` still
  `"function"`, calls throw); `Array.isArray`, `Object.prototype.toString`,
  `JSON.stringify` (arrays, objects, revoked, replacer list), `instanceof`
  with a `getPrototypeOf` trap, `concat` spreading a proxy of an array,
  `Function.prototype.toString`. `gc_stress` for the trampoline's argument
  array and the construct path.
- P3 — `crates/jsc-core/tests/reflect.rs` (new): every function (argument
  checks, return values where the `Object` twin throws), `Reflect.ownKeys`
  order, `Reflect.construct` with a `newTarget` for a user class, a derived
  class, each builtin constructor family, a bound function and a proxy, and
  `Reflect` itself (`@@toStringTag`, not callable, not a constructor). The
  phase-4 leftover lives here as
  `construct_new_target_prototype_getter_keeps_args`: `newTarget` is a
  proxy over a function whose `get` trap, for `"prototype"`, first makes a
  nested spread call with more than `K` arguments, then returns the realm
  prototype; `Reflect.construct(Date, [7 fields], nt)`,
  `Reflect.construct(Error, [msg, {cause}], nt)`, `AggregateError`, `Array`
  with more than `K` elements and `Promise` must see their own arguments,
  not the nested call's (`Body::snapshotted`), as must a user class with a
  rest parameter and `Date` under a bound-function `newTarget` whose
  `prototype` is an accessor; with an `assert_same_under_gc_stress`
  twin.
- P4 — `proxy.rs`: the target's full key list (array, lazy function and
  proxy targets, trap-less and as the `ownKeys` invariant's reference, with a
  `gc_stress` twin); otherwise the test262 run is the gate. No opcount row
  moves (the arms are compiled out without `proxies`).

**test262 effect.** With the flags removed, about 1050 tests become
eligible (1061 strict-compatible tests carry a `Proxy`, `Reflect*` or
`proxy-missing-checks` feature and no other skipped one; 11 of them include
`nativeFunctionMatcher.js`, which does not compile): built-ins/Proxy 260,
Reflect 153, Object 89, Array 77, TypedArrayConstructors 60, Date 49,
TypedArray 40, String 38, Math 36, Promise 35, JSON 24, DataView 24,
Function 22, ... (421 include `isConstructor.js`, which needs
`Reflect.construct`). The failure classes left are other sections' gaps:
`RegExp` literals without a feature tag (§13), the `%TypedArray%.prototype`
methods other than the getters and iterators (§5.8), the `Function`
constructor and `eval` (§14 open decisions), and the generic `Array`
loops' `2^32 - 1` length clamp (§13) observed through a proxy `length`.

---

## 7. Language surface

The compiled language is the strict-mode subset described below. Anything not
listed is rejected at compile time with a spanned diagnostic (§8) or noted in
§13.

### 7.1 Bindings & scoping

- **Declarations.** `let`, `const`, `var`, function declarations (hoisted, also
  nested ones), class declarations. An identifier that resolves to no
  declaration is a global-object lookup (§6.4).
- **`var` hoisting.** A `var` belongs to its function's (or the script's) var
  scope, wherever it appears: the names are collected up front and bound once
  each, to `undefined`, on entry — before any statement, so a read before the
  declaration yields `undefined`. A `var` statement is an assignment to that
  binding (a declarator without an initializer does nothing), and a
  `for (var ..)` / `for-in` / `for-of` `var` head assigns it each iteration
  (closures share one binding, no per-iteration copy). Block-scoped `let` /
  `const` still shadow it. Inside `catch (e)`, `var e = v` assigns the catch
  parameter (Annex B); the function-level `e` exists and stays `undefined`.
  Top-level `var`s are the script's var scope: `main` bindings (module cells
  when a function references them), or global-object properties (§6.4).
- **Block scoping.** Name resolution walks a compile-time scope stack: every
  `{}` block, loop body, `for` head, and `catch` clause pushes a frame. Each
  `let`/`const` gets a fresh wasm local; inner declarations shadow outer ones.
- **`const`.** Rebinding a `const` (by `=`, compound assignment, `++`/`--`) is a
  compile-time error; mutating the referenced object/array is allowed.
- **Closures.** Capture analysis marks every binding referenced from an inner
  function. Captured bindings are promoted to heap **cells** (`TYPE_CELL`);
  uncaptured bindings stay plain wasm locals. A capturing function value
  carries a `TYPE_ENV` of cell pointers, so capture is **by reference** — every
  closure and the defining scope share one cell. Loop bindings captured from a
  `for-of`/`for-in` or a C-style `for (let …)` loop get a fresh cell per
  iteration.
- **Module cells.** A top-level binding that is referenced from inside a
  function is stored in a **module cell**: a cell whose pointer lives in a
  dedicated `i64` global (index `30 + slot`), allocated in `main`'s prologue
  before any user code runs. Top-level functions reach it through the global, so
  they stay hoisted and directly callable from anywhere. The cell holds the TDZ
  `HOLE` until the declaration executes; reading or writing it earlier throws
  `ReferenceError: Cannot access 'n' before initialization`.
- **Class names** are bound before the class body is lowered, so constructors and
  methods can `new` their own class; a named class expression binds its name
  inside its body.

### 7.2 Operators & coercion

- **Arithmetic** `+ - * / % **` (and the compound forms), unary `-` / `+`,
  prefix/postfix `++` / `--`. `- * / % **`, unary `-`/`+`, and `++`/`--` apply
  **ToNumber** (with `ToPrimitive` for objects). Binary `+` follows §6.5.
  `**` and `Math.pow` share one inline implementation.
- **Bitwise and shift** `& | ^ ~ << >> >>>` and their compound forms, with
  ToInt32 / ToUint32 on the operands (ToNumeric first: see BigInt below).
- **BigInt** (§6.10.5–6.10.6): every operator above applies ToNumeric
  instead of ToNumber on its non-number path; two BigInts use the BigInt
  kernels, a BigInt mixed with a number throws `TypeError` (except `+` with
  a string, and the comparisons, which are exact), unary `+` and `>>>` throw
  for a BigInt, and `**` is its own `BinOp::Exp` (`Math.pow` keeps ToNumber).
- **Relational** `< > <= >=` (§6.5).
- **Strict equality** `===` / `!==`: numbers numerically (`NaN !== NaN`,
  `+0 === -0`), strings by byte content, BigInts by value, everything else
  by identity bits.
- **Loose equality** `==` / `!=` (`__loose_eq`): booleans are first normalized
  to numbers; `null` and `undefined` equal each other and nothing else; two
  numbers compare as `f64`; two strings by content; number vs. string converts
  the string; an object against a non-nullish primitive is replaced by its
  `ToPrimitive` (hint default) and compared again (`[] == 0` is `true`); two
  objects compare by identity; a symbol equals only itself (and its wrapper,
  through `ToPrimitive`), never a number or string.
- **SameValueZero** (`__same_value_zero`) backs Map/Set keys and `includes`:
  strings by content, numbers by `f64` equality or both NaN, everything else by
  identity.
- **Logical** `!`, `&&`, `||`, `??` (each yields an operand value, not a coerced
  boolean) and `&&=`, `||=`, `??=` (short circuit).
- **Assignment targets** are an identifier, a member `o.k` / `o[k]`, a
  `super` property `super.k` / `super[k]` (§7.6; any of them parenthesized
  any number of times: `(a) = 1`, `((o.x)) += 1`, `++(a)`) or a
  destructuring pattern (§7.3). A compound, `**=` or logical assignment to a
  member evaluates the base and the key once (a `**=` / logical one converts
  the key once with `ToPropertyKey`, after a `null` / `undefined` base has
  thrown its `TypeError`, so a key's `toString` never runs then); a
  short-circuited `&&=` / `||=` / `??=` skips both the right-hand side and
  the store, so no setter runs. An anonymous function assigned by `=` or a logical assignment to an
  unparenthesized identifier is named after it. The same `NamedEvaluation`
  names an anonymous function, arrow, generator, async function or class
  (also parenthesized, a cover form such as `x = (function () {})`) used as
  a declaration initializer, an object-literal value, or a default in a
  binding pattern, an assignment pattern or a parameter, after the target
  identifier; a member or nested-pattern target names nothing, a named
  definition keeps its own name, and a class with an own static `name`
  member keeps that member. An invalid target (`f() = 1`,
  `1 = 2`, `a + b = c`, `(a, b) = c`, `a?.b = 1`) is an early `SyntaxError`
  (`Invalid left-hand side in assignment`).
- **Conditional** `c ? a : b`, optional chaining `a?.b`, `a?.[k]`, `a?.()`,
  `a?.m()` (a nullish base short-circuits the rest of the chain to
  `undefined`).
- **Comma** `a, b, c` evaluates every operand left to right and yields the last
  (a plain value: `(0, o.m)()` calls without a receiver, while parentheses
  alone keep the reference: `(o.m)()` calls with `this = o`); `yield`/`await`
  operands suspend in order.
- **`typeof`**, **`instanceof`** (§7.6), **`in`** (`__has`), **`delete`**
  (`__delete`; parentheses around the member are transparent; a `null` /
  `undefined` base throws a `TypeError` before the key converts). `delete x`
  of an identifier is an early `SyntaxError` (strict code); `delete super.x`
  / `delete super[k]` reads `this`, evaluates `k`, then throws
  `ReferenceError: Unsupported reference to 'super'`; `delete` of an
  optional chain (`delete o?.x`, `delete o?.a.b`, `delete o?.[k]`) deletes
  the final reference unless the chain short-circuits, and yields `true`
  either way; `delete` of any other non-reference (`delete 1`, `delete f()`,
  `delete o?.f()`) evaluates its operand and yields `true`.
- **`void`** evaluates its operand for its effects (calls, assignments,
  throws, `yield` / `await`) and yields `undefined`; a literal operand folds
  to `undefined`.
- **Template literals** `` `a${x}b` `` concatenate the cooked strings with
  `ToString` of each substitution; an invalid escape is a `SyntaxError`
  (swc). Tagged templates are calls (§7.4).
- **Member access on `null`/`undefined`** throws a `TypeError`
  (`Cannot read properties of null (reading 'x')`), catchable like any throw.

### 7.3 Control flow

`if`/`else`, `while`, `do … while`, C-style `for`, `for-of`, `for-in`, `switch`
(strict `===` matching, fall-through, `default`), `break` / `continue`,
labelled statements, `return`, `throw`, `try`/`catch`/`finally` (with or without a
catch binding).

- **Labels** apply to any statement (`a: { … }`, `a: if (…) …`, `a: try …`,
  `a: switch …`, nested `a: b: while (…)`); a run of nested labels names one
  statement. `break a` exits the labelled statement from anywhere inside it,
  running every intervening `finally`; a labelled non-loop statement is an
  `IrStmt::Labeled` (a wasm `block`, or an exit block in the generator CFG)
  that an unlabeled `break` / `continue` passes through. `continue a` where
  `a` does not label an iteration statement, an undefined label, a duplicate
  nested label (`a: a: ;`, swc) and a labelled function declaration (strict
  code) are early `SyntaxError`s.

- **`catch` parameters** are an identifier or a binding pattern
  (`catch ({message})`, `catch ([a, b = 2, ...r])`), bound like a `let` in a
  fresh scope around the handler from the thrown value. A throw while
  destructuring (`null` into `{a}`, a non-iterable into `[a]`) escapes the
  catch clause as the new exception, still running `finally`; an array
  pattern closes its iterator like any other. A duplicate bound name, or a
  bound name redeclared by a `let`/`const`/`class`/function of the block (or,
  for a pattern, by a `var` of the block), is an early `SyntaxError`.

- **`for-of`** runs the iteration protocol (§7.8): `GetIterator` (`@@iterator`, called once; `next` read once;
  `TypeError: x is not iterable` without a callable `@@iterator`, an async
  generator included), `IteratorStep` / `IteratorValue` per iteration (a
  non-object result throws), and `IteratorClose` when the body exits by
  `break`, `return`, a labelled jump or a throw (also from the head's
  destructuring), never after a throw from the iterator itself; a
  non-object `return()` result throws only on a non-throw exit. A clean
  array (a hole reads `undefined`), string (per code point), `Map`
  (`[key, value]` entries) / `Set` (live walks that visit entries added
  during the loop) or generator without a `yield*` in its body (its state
  machine stepped directly) takes a
  guarded fast path while its builtin `@@iterator` and iterator `next` are
  provably intact (the dirty bits of §6.4); an `arguments` object is walked
  by index while its own `@@iterator` is still `%Array.prototype.values%`
  (and the Array Iterator bits are clean). The loop head declares its
  binding with `const`/`let`/`var` (identifier or pattern) or assigns any
  assignment target (`for (a of xs)`, `for (o.p of xs)`, `for ([a] of
  xs)`, `for ({a} of xs)`) as the first body statement, so a throwing
  assignment closes the iterator.
- **`for await`** (in async functions, async generators and the top level of
  `main`) has `for-of`'s shape and head forms over an async record (§7.8):
  each step awaits `next()`, then checks the result is an object; an abrupt
  body exit calls `return()` (if any) and awaits it, a normal completion
  then requiring an object result while a throw completion discards any
  rejection; a throw from a step does not close. It has no fast path: every
  step is a microtask turn.
- **`for-in`** iterates the key snapshot of §6.3; strings and arrays list their
  index keys first, and any other primitive (`null` / `undefined` included)
  lists none. Its head takes every `for-of` head form (`for (const [a, b]
  in o)`, `for (var {x} in o)`, `for (k in o)`, `for (o.p in o)`); an
  Annex B `var` initializer (`for (var k = 0 in o)`) is a spanned error.

A `for` loop with a `let` head is scoped to the loop; `continue` still runs the
update clause. Labels may only be applied to loops.

The CLI's `-p` mode prints the value of the **last expression statement**
evaluated by `main`; `try` statements contribute their completion values, while
those of `switch` and loops are not tracked.

### 7.4 Functions

- **Forms.** Function declarations and expressions (including generator,
  `async` and async generator ones, §7.8), arrows (expression- or
  block-bodied, optionally `async`), object-literal methods (also generator,
  `async` and async generator methods), getters and setters, class
  constructors/methods/accessors (instance and static), class expressions. All
  are lifted to top-level wasm functions with the uniform ABI (§4.2); a function
  value is a `TYPE_FUNCTION` block (§5.5) and carries properties like any
  object.
- **Parameters.** Default values (evaluated per call in a prologue, may reference
  earlier parameters and captured bindings), trailing rest parameters
  (collected from the positional slots and `$__spread_args`; the rest target
  may itself be an array/object pattern, `f(...[a, b])`), and array/object
  destructuring patterns (composable with defaults). Every parameter name is
  declared up front and the parameters bind left to right. With parameter
  expressions (a default anywhere), a parameter is in its TDZ until it binds:
  a reference from the parameter code itself compiles to a `ReferenceError`
  throw, and a captured parameter's cell holds the hole until then, so a
  closure created by a default checks it (`f(a = b, b)`, `f(a = a)` throw).
  This holds in every function form; a generator, async or async generator
  function binds them when called (§7.8). A duplicate parameter name is an
  early `SyntaxError` (all code is strict).
- **`var` scope.** Every `var` of a function body (in nested blocks, loop
  heads, `try` / `catch` / `finally`, `switch` cases and labelled statements
  too, not in nested functions) is hoisted (§7.1): one binding per name,
  `undefined` on entry before any statement, in every function form (a
  generator or async body binds it in its persistent frame). A `var` naming
  a parameter or a top-level function declaration of the body reuses it:
  without an initializer it keeps its value, with one it assigns. With
  parameter expressions, a body `var` redeclaring a parameter is instead a
  separate binding created on entry from the parameter's value, so closures
  in the parameter list keep the parameter.
- **`arguments`.** A function that references `arguments` builds an unmapped
  `CLASS_ARGUMENTS` object from `argc`, the positional slots, and
  `$__spread_args`, with an own non-enumerable `@@iterator` =
  `%Array.prototype.values%`.
- **Spread.** In calls, `new`, and array literals; a source is
  `IterableToList` over the `for-of` protocol (never closed). A clean array
  is appended directly, a clean string by character (`__str_to_array`) and
  a clean generator without a `yield*` drained by `__gen_to_array`; a
  clean `arguments` object
  is walked by index (`ProtocolOp::ToList`); a derived class's default
  constructor forwards its arguments without the protocol.
- **`this`.** Bound by method calls (`obj.m()`, also `(obj.m)()`), by `new`, by
  `Function.prototype.call` / `apply` / `bind`, and for getters/setters. A plain call
  sees `this === undefined`; top-level `this` is the global object. An arrow
  (`async` ones included) has no `this` of its own: it reads the `this` of the
  nearest enclosing non-arrow function (the global object at top level),
  whatever receiver or `thisArg` it is called with. An ordinary function's
  `this` snapshot is captured by value through each arrow's environment; a
  generator/async body's `this` cell by reference, so it survives suspension.
- **Arity.** `length` is the count of parameters before the first default or
  rest; arity is never checked, at compile time or at run time.
- **Tagged templates** `` tag`a${x}b` `` lower through the ordinary call
  path: `this` is bound for a member tag (`` o.f`x` ``, `` o[k]`x` ``,
  `` (o.f)`x` ``, `` super.m`x` ``), a non-callable tag is the same
  `TypeError` as a call, and the substitutions run left to right after the
  tag. The first argument is the site's template object
  (`GetTemplateObject`): a frozen array of the cooked strings (`undefined`
  for an invalid escape) with a non-writable, non-enumerable,
  non-configurable `raw` holding the frozen array of the raw strings (CR and
  CRLF normalized to LF). Each site owns a module cell (§4.2, after the
  module bindings' cells) holding the hole until the site first runs, which
  builds the object into it; every later evaluation of that site, in a loop
  or another call, yields the same object, and two sites never share one.
  A chain `` t`a``b` `` calls the first result with the second site's
  object, and `` new t`x` `` is `` new (t`x`)() ``: the tag's result is the
  constructor. A tagged template in an optional chain (`` a?.b`x` ``) is a
  `SyntaxError` (swc).
- **Call evaluation order.** A call evaluates its callee first — for a
  member callee the base, then the key, then the `[[Get]]` (a getter runs
  here) — then the arguments left to right, then `IsCallable` and the call;
  `new` evaluates the constructor, then the arguments, then
  `IsConstructor`. The wasm call pushes the arguments before the callee, so a
  callee expression is first evaluated into a temp unless every argument is
  side-effect free; a method read lands in a temp before the arguments run.
  A builtin-named method call with side-effecting arguments (§6.4) resolves
  the method first — a constant marker naming the fast path whose test the
  receiver passes (no lookup), else `__get` — and after the arguments a fast
  path runs on its marker (an array or promise path also re-tests the
  receiver, whose own properties the arguments may have changed); any other
  receiver calls the method read first.

### 7.5 Objects & arrays

- **Object literals** with identifier, string, numeric, and computed keys,
  shorthand properties, methods, and getters/setters; keys are defined in
  source order with `__define`, `proto = %Object.prototype%`. A method,
  accessor, or anonymous function / arrow value under a computed (or
  numeric) key gets its `name` at run time from the key, converted once
  before the value (`SetFunctionName`: the string key, `"[desc]"` for a
  symbol, `""` for a symbol without a description; `get ` / `set `
  prefixed). A `__proto__: v`
  entry (identifier or string key; not computed, shorthand, method or
  accessor) defines no property: `v` is evaluated in source order and becomes
  the `[[Prototype]]` when it is an object or `null`, else it is ignored
  (ECMA-262 B.3.1). Two such entries in one literal are an early
  `SyntaxError` (a destructuring assignment pattern may repeat it). A spread
  `...o` is `CopyDataProperties` (§6.3) in source order: from the first
  spread on, the literal is built incrementally on a temp — the entries
  before it, then each spread, run of later entries, and `__proto__` entry
  in turn — so later properties overwrite spread keys and vice versa; a
  `__proto__` key copied by a spread is an ordinary own property. A getter
  or setter defined after a spread or a computed entry is a partial
  descriptor (`__define_own`): `{get a() {}, ...o, set a(v) {}}` keeps the
  getter unless the spread turned `a` into a data property.
- **Property access** `o.k`, `o["k"]`, `o[expr]` (§6.3): own keys, then the
  prototype chain; a missing key reads `undefined`. Writes add properties
  dynamically; `delete` removes them.
- **Destructuring.** Array patterns (holes, defaults, rest, nesting) and object
  patterns (identifier, string, numeric, and computed keys, rename, defaults,
  nesting, rest) in
  declarations, parameters, and `for-of` / `for-in` heads; destructuring
  assignment in any expression position (`x = [a, b] = c`, `f(({a} = o))`,
  a `?:` arm, a `for` update, an arrow body), with member targets (`[o.x] =
  xs`, `({...o.r} = src)`); its value is the RHS. It desugars to statements
  (the RHS into a temp, then each target in order) wrapped in an IR
  `Block { stmts, value }` node whose value is that temp: the emitter runs
  the stack-neutral statements in place, under any operands already pushed,
  and a generator body linearizes a block holding a suspend into its CFG
  (§7.8) after spilling the earlier operands. A numeric or computed pattern
  key is evaluated and converted (`ToPropertyKey`, once; a symbol stays a
  symbol, `1.0` / `0x10` are `"1"` / `"16"`) in source order — before the
  assignment target reference, which precedes the read and then the default.
  Each property is read once (a default tests the stashed read). An object
  rest is a fresh ordinary object filled by `CopyDataProperties` minus the
  destructured keys, the converted computed keys (symbols included) among
  them. An object pattern checks `RequireObjectCoercible` before its first
  key expression, target reference, rest, or read, so a nullish source
  throws a `TypeError` before anything else runs.
  An array pattern runs the iteration protocol lazily
  (§7.8): `GetIterator`, one `IteratorStep` per
  hole or element (an element reads `undefined` once done; a hole reads no
  value), a rest drains, and `IteratorClose` follows the pattern unless the
  iterator is done (an empty pattern closes without stepping) or a default,
  nested pattern or target throws — never after a throw from the iterator
  itself.
- **Arrays.** Literals (with elisions), indexing, element stores with growth by
  index, named properties, `.length` reads and writes, and the builtins of
  §7.9 (§5.4).
- **Computed dispatch.** `x[k]` and `x[k] = v` take an inline element path for
  an array receiver and a number key, and `__get` / `__array_set` / `__set`
  with `__to_key` otherwise.

### 7.6 Classes, construction, `instanceof`

- **Classes are constructor functions.** `class C { … }` binds `C` to a
  `TYPE_FUNCTION` block (`CONSTRUCTOR | CLASS_CTOR`); a class without a
  constructor gets a synthesized one. `C.prototype` is a non-writable object
  whose non-enumerable `constructor` is `C`; methods and accessors are defined
  on it; static methods, accessors, and fields are own properties of `C`.
  Methods may be generator, `async` or async generator methods (§7.8); `this`
  and `super.x` work in their bodies across suspensions.
- **Computed member names.** `[expr]` names a method, accessor, or field
  (static or not). Every computed key is evaluated and converted
  (`ToPropertyKey`) once, in source order, at class definition — right after
  the heritage, before any field initializer runs; instance fields reuse the
  converted keys on each construction. The function `name` is set at run
  time from the key (`"[desc]"` for a symbol, `"get "` / `"set "` prefixed for
  accessors), as for an anonymous function or class value under a computed
  key (`{[k]: class {}}` too). A computed accessor half is a partial
  descriptor (`__define_own`), so `get [k]()` / `set [k](v)` share one
  property. A computed `"constructor"` is an ordinary method; a computed
  static `"prototype"` throws a `TypeError` (the literal forms stay early
  errors). A numeric literal member name (`1() {}`, `static 2() {}`,
  `get 3() {}`, `0x10 = 1`) is converted the same way, to its canonical
  numeric string.
  Calling `C` without `new` throws `TypeError: Class constructor C cannot be
  invoked without 'new'`.
- **Fields.** Instance fields (`f = init`, or `f;` for `undefined`) are
  defined with `__define` in source order before the constructor body (for a
  derived class, right after `super(..)` returns). Static field initializers
  run after the class binding exists, in source order, with `this` = the class
  and the static `super` scope (`super.x` reads the class's current
  prototype, as in a static method).
- **Inheritance.** `class B extends E` evaluates `E` once at class definition
  (any expression; it must be a constructor or `null`), links
  `B.prototype.[[Prototype]]` to `E.prototype` and `B.[[Prototype]]` to `E`
  (statics are inherited). `B` is `SELF_ALLOC`: its `this` is in TDZ until
  `super(args)` constructs it with `new.target` from `B`'s prototype at the
  call (`GetSuperConstructor`: `E` unless `Object.setPrototypeOf(B, ..)`
  moved it; read before the arguments, a non-constructor throws `TypeError:
  Super constructor is not a constructor` after them); touching `this` first
  throws `ReferenceError: Must call super constructor in derived class before
  accessing 'this' …`. `super.m(args)` calls the parent prototype's method with
  the current `this`. A derived class without a constructor forwards its
  arguments. Builtin parents (`Error`, `Map`, `Set`, `Promise`, `Date`,
  `Array`) work through the same path: their constructors allocate from
  `new.target.prototype`.
- **`super` properties.** In a class method, accessor or constructor (and
  an arrow inside one), `super.x` / `super[k]` is a
  reference whose base is the home object's `[[Prototype]]`, read at each
  reference after `k` is evaluated and before it converts (`GetSuperBase`;
  `IrNode::SuperBase`) — the home object is the class's `prototype` for an
  instance member (constructor and instance field initializers included)
  and the class itself for a static one (static field initializers and
  static blocks included), so a later `Object.setPrototypeOf` of either is
  observed. The class keeps both home objects in two hidden cells, created
  and filled around its `DefineProps` only when a member references
  `super` (the constructor closure exists before the class value). The
  receiver is the current `this`, read first (TDZ-checked in a derived
  constructor), before `k` is evaluated. A read is `__super_get` (a getter
  runs with `this`); `super.x = v` is `__super_set` (§6.3: an inherited
  setter runs with `this`, otherwise `this` gets the own property, the
  prototype is unchanged, and a read-only, setter-less or non-extensible
  failure throws a `TypeError`), with `k` converted after `v` runs. Compound,
  `**=`, logical and `++` / `--` forms read through `__super_get` and store
  through `__super_set` on the same base, converting `k` once before the
  right-hand side (a short-circuited logical assignment skips the
  right-hand side and the store); a `null` base (`class extends null`, or a
  prototype set to `null`) throws a `TypeError` before `k` converts. `delete super.x` throws a `ReferenceError` (§7.2). They work
  in generator and async methods across suspensions.
- **`new X(args)`.** `IsConstructor(X)` (a function with `CONSTRUCTOR`; else
  `TypeError: X is not a constructor`); unless `X` is `SELF_ALLOC`, `this` = a
  fresh object whose `proto` is `Get(X, "prototype")` when that is an object,
  else `%Object.prototype%`; `$__this = this`, `$__new_target = X`, call with
  the construct marker. The result is the return value when it is an object,
  else `this` (return override). Plain `function` declarations and
  expressions are constructible.
- **`new.target`** reads `$__new_target` under the construct marker, else
  `undefined`.
- **`instanceof`** is `InstanceofOperator` for any right-hand side: a
  non-object right-hand side throws a `TypeError`; `GetMethod(C,
  @@hasInstance)` (`__symbol_get`) is called with `this = C` and its result
  coerced with `ToBoolean` (a non-callable one throws `TypeError:
  Symbol.hasInstance is not a function`). The builtin
  `%Function.prototype%[@@hasInstance]` (non-writable, non-enumerable,
  non-configurable; `OrdinaryHasInstance(this, V)`) runs inline, as does an
  absent hook after `IsCallable(C)` (else a `TypeError`):
  `OrdinaryHasInstance` is `false` for a non-callable `C`, restarts
  `InstanceofOperator` on a bound `C`'s target, and otherwise walks the left
  operand's `proto` chain against `Get(C, "prototype")`; primitives yield
  `false`. An unshadowed, sealed `Array` / `Function` / `Date` / `Object` /
  Error-family right-hand side takes a fast arm behind a run-time intact test
  of its `@@hasInstance` lookup (§6.4): a `proto` walk for the constructor's
  static prototype (`Object.create(null) instanceof Object` and
  `Object.setPrototypeOf([], null) instanceof Array` are `false`,
  `Object.create(Array.prototype) instanceof Array` is `true`).
- **Bound functions** (§5.5) under `new` construct their target; a bound
  non-constructor is not a constructor (`TypeError: B is not a constructor`).

**Private names** (phase 5a, done): private instance and static fields,
methods and accessors (generator, async and async generator methods
included) — names, `%penv`, resolution, storage, brands, the helpers,
instance and static initialization, every operation, `#x in o`,
invisibility and the early errors — and static blocks.

- **Names.** A private name is a `TYPE_SYMBOL` block with header flag `8`
  `SYMBOL_FLAG_PRIVATE` (bits `1`/`2` stay clear, `4` is `REGISTERED`;
  a private symbol is never registered) and description `"#x"`, minted by
  the new `IrNode::PrivateName(desc)` (the `new_symbol` sequence of
  `emit/builtins/symbol.rs`). Only **fields** (instance and static) get one
  per name; methods and accessors are not keyed per name (brands, below).
- **Private environment.** Each evaluation of a class body declaring a
  private name allocates `%penv` (an `IrNode::Array` literal), an internal
  dense array held in one `hidden_local` of the class scope (captured by
  value, like the heritage locals; never a user value); a class declaring
  none allocates nothing.
  Slot `0` is the class value (stored once it exists, before any static
  element runs; `undefined` until then), slot `1` the instance brand (a
  private symbol, description = the first instance private method or
  accessor's `"#m"`; `undefined` when the class has none), then one slot per
  private field (its symbol), per method (its closure), and two per accessor
  (getter, setter; `undefined` when absent), in declaration order. The array
  and the brand / field symbols are created after the heritage and before
  the computed keys, so every closure of the body (methods, accessors,
  constructor, field initializers, static blocks, computed keys, nested
  functions, arrows and classes) reads them through `read_hidden`; method
  closures are stored into the array later (new IR
  `PrivEnvLoad { env, slot }` / `PrivEnvStore { env, slot, value }`, a
  direct access at `ELEMS_DATA_OFFSET + 8·slot`; the array never grows),
  which is why they are not separate by-value locals. A class expression
  in a loop or a factory therefore mints distinct names, brands and method
  closures per evaluation.
- **Resolution** is lexical at compile time: `LowerCtx::private_scopes`
  (new) is a stack of `{ frame, penv_id, names }`, each name
  `{ "#x", kind: Field(slot) | Method(slot) | Accessor(get, set), static }`
  (`static` matters only for methods and accessors, whose brand it picks).
  `lower_class` pushes the scope after lowering the heritage (which sees only
  the enclosing classes' names) and pops it with the class scope. `#x`
  resolves to the innermost class declaring it, so a nested class shadows
  an outer name and a nested class not declaring `#x` uses the outer one.
  An unresolved or repeated name never reaches lowering: `early.rs` rejects
  it first (below).
- **Storage.** A private field is an own entry keyed by its symbol in the
  object's ordinary `TYPE_PROPS` table, attrs `WRITABLE` only; the instance
  brand is an entry keyed by the brand symbol, attrs `0`, value `undefined`.
  Private entries are never `ENUMERABLE` (an invariant the renderer and
  every enumerable-only walk rely on). Static private fields are entries of
  the class. Static methods and accessors need no entry: the class is
  their only possible holder, so their brand check is `o === %penv[0]`
  (after an object test, so the `undefined` placeholder never matches).
- **Helpers** (new, `emit/props.rs`; `name` is the static `"#x"` string):
  `__private_find(o, key)` → i32 `1` iff `o` is an object-family block
  whose own table holds `key` (no prototype walk, no `__fn_materialize`:
  a `LAZY` function holds no private entry; a primitive is `0`);
  `__private_add(o, key, v, attrs, name)` (PrivateFieldAdd /
  PrivateMethodOrAccessorAdd): present → `TypeError: Cannot initialize #x
  twice on the same object`, else `__define`, which skips its
  not-extensible check for a `SYMBOL_FLAG_PRIVATE` key, so adds work on
  non-extensible, sealed and frozen objects;
  `__private_get(o, key, name)` / `__private_set(o, key, v, name)` (which
  returns `v`) on a field: absent (a primitive included, no `ToObject`) →
  `TypeError: Cannot read private member #x from an object whose class did
  not declare it` / `Cannot write private member #x to an object whose class
  did not declare it`; a set ignores attrs.
  `__private_brand(o, v, brand, name, mode)` (returns `v`): `o` must hold
  `brand` — an own entry keyed by it when it is a private symbol (a field
  or the instance brand), else be `brand` itself, an object (a static
  member's class) — or the read / write message (`mode`) is thrown;
  `__private_in(o, brand, name)` (a boolean): a non-object `o` throws the
  `in` message, else the same test. The messages are new `__prop_throw`
  modes `21`–`27` (the `"#x"` string as the key; `26`, `Private method is
  not writable`, takes the empty string; `27`, the `in` message, the key
  `"#x' in " + String(o)` that `__private_in` builds). The IR nodes are
  `PrivateAdd { obj, key, value, attrs, name }`, `PrivateGet { obj, key,
  name }` and `PrivateSet { obj, key, value, name }` (`key` a
  `PrivEnvLoad`), `PrivateBrand { obj, value, brand, name, write }`
  (evaluates `obj`, then a write's `value`, then checks; yields `value`,
  else `obj`), `PrivateThrow { error, name }` and `PrivateIn { obj, brand,
  name }`.
- **Methods and accessors.** `o.#m` brand-checks (`__private_find(o,
  brand)`, or the slot-`0` identity when static), throwing the read / write
  message above naming `#m`, then: a method reads its closure (one per
  class evaluation, so every instance sees the same function); a getter
  is called with `this = o`; a missing getter throws `TypeError: '#x' was
  defined without a getter`. A write after the brand check throws
  `TypeError: Private method is not writable` for a method, calls the
  setter, or throws `TypeError: '#x' was defined without a setter`. A getter
  read is an `IrNode::ThisCall` whose callee snapshots the checked receiver
  into its `this_local`; a setter write snapshots `o`, then calls the setter
  with the brand-checked value (`o`, `v`, check, call; the expression yields
  `v`). Closures
  are lowered like `lower_class_method` with `name` `"#m"` / `"get #x"` /
  `"set #x"` (generator, async and async generator methods included),
  instance ones with the prototype `super` scope, static ones with
  `is_static`, and stored in source order right after the class value's
  `DefineProps`; then `%penv[0]` is set, then the inner name is bound.
- **Instance initialization** (`lower_field_inits`, on constructor entry or
  right after `super(..)` returns, on the `this` it returned): first
  `__private_add(this, brand, undefined, 0, "#m")` when the class has
  instance private methods or accessors, then every field — public
  (`__define`) and private (`__private_add(.., WRITABLE, "#x")`) — in source
  order; an anonymous function initializer is named `"#x"`. A base
  constructor returning an existing object lets a derived class add to it
  once; the second add throws.
- **Static elements.** Static fields, static private fields and static
  blocks run in one source-order pass after slot `0` and the inner name are
  set, `this` = the class; a static private field is
  `__private_add(C, key, v, WRITABLE, "#x")`. Statics are not inherited:
  `Sub.#x` throws the read message (the lookup is own-only and the identity
  check fails).
- **Operations.** `o.#x = v` evaluates `o`, then `v`, then sets; compound
  and logical assignment and `++`/`--` evaluate `o` once into a temp, get
  (throwing when absent), compute, set; destructuring targets (`[this.#x] =
  xs`, `({a: this.#x} = o)`) are member targets ending in a private set;
  `o?.#x`, `o?.a.#x`, `o?.#m()` short-circuit as usual; `o.#m(args)` reads
  (brand check before the arguments) and calls with `this = o` (an
  `IrNode::ThisCall` on the receiver temp; with a suspend in an argument,
  generator linearization spills the callee and the receiver and rebinds
  it, like a user method call). There is no
  `super.#x` (a parse error). `#x in o`: `o` not an object → `TypeError:
  Cannot use 'in' operator to search for '#x' in <o>` (`__private_in`,
  rendering `o` with `String` as `PROP_THROW_IN` does); else
  `__private_find` of the field or brand symbol, or the slot-`0` identity;
  no prototype, getter or proxy is consulted.
- **Invisibility.** The private symbol is only ever passed as a key
  argument of the helpers above: no expression yields it, so `typeof`,
  `description`, `String(sym)`, `__get`, `__set`, `__has`, `__has_own`,
  `__delete`, `__own_desc` (`getOwnPropertyDescriptor`), `__own_enum`,
  `__define_own` (`defineProperty`) and `__to_key` can never receive it.
  Walks that read raw table keys must skip it: `__own_keys`' symbol pass
  (`OWN_SYMBOLS`: `getOwnPropertySymbols`; `OWN_ALL_KEYS`: object rest
  `__rest`, `Object.assign`, `defineProperties` / `create`,
  `getOwnPropertyDescriptors`, `Reflect.ownKeys` and object spread) lists a symbol key only when it lacks `SYMBOL_FLAG_PRIVATE`; the
  string modes (`Object.keys/values/entries`, `getOwnPropertyNames`,
  `JSON.stringify`, `__for_in_keys`) already skip every symbol.
  `__set_integrity` (`freeze` / `seal` / `preventExtensions`) leaves private
  entries' attrs untouched and `__test_integrity` (`isFrozen` /
  `isSealed`) ignores them. The renderer (`emit/render.rs`) lists
  only `ENUMERABLE` entries, so it never shows one. A private key appended to a watched intrinsic's table only
  sets its dirty bit. A proxy keeps private entries in its own table
  (§6.11), so private operations act on the proxy and never trap.

**Static blocks** (phase 5a, built in P4). `static { .. }` lowers to a lifted
zero-parameter method function (`lower_lifted_function`, method kind, not
a constructor, `name` `""`, the static `super` scope so `super.x` reads
the class's current prototype) created and called immediately with `this` = the
class at its source position in the static-element pass; the call gives it
its own `var` scope, `new.target` `undefined`, the class-scope captures
(inner name, `%penv`) and exception propagation for free. One closure is
allocated per block per class evaluation. The call is an `IrNode::ThisCall`
on the class local, so the lifted body's own `this` is the class (an arrow
inside reads it lexically) and the construct marker is absent; the capture
and arity pre-passes visit the block as a zero-parameter function.

**Early errors** (`codegen/early.rs`, run over the whole program before
lowering, so unreached code is checked too; spanned):

- `private name #x is not defined`: a `#x` (member access, optional chain,
  `#x in o`, nested functions and classes included) that no enclosing class
  body declares. A class heritage sees only the enclosing classes' names; its
  computed keys, field initializers, methods and static blocks see its own.
- `duplicate private name #x`: a name declared twice in one class body
  (fields, methods, accessors and `accessor #x`, static or not), except
  exactly one getter plus one setter of the same static-ness.
- `private fields cannot be deleted`: `delete` of `o.#x`, parenthesised
  (`delete (o.#x)`) or ending an optional chain (`delete o?.#x`,
  `delete o?.a.#x`).
- `unexpected private name #x`: a bare `#x` anywhere except as the left
  operand of `in` (`(#x) in o`, `#x + 1`).
- `invalid private name #x`: a declared or referenced name whose first
  character is not an IdentifierStart (`#\u200Dx`, `#\u200Cx`: swc checks a
  leading `\u` escape only as an IdentifierPart).
- In a static block: `arguments` anywhere outside a nested non-arrow
  function (computed keys of a nested class included), `await` as a binding
  or reference outside nested functions and arrows, `await` / `yield`
  expressions, `return`, `super(..)`, `break` / `continue` / labels crossing
  the block, and lexical redeclarations (the block is a function-like scope:
  its top-level function declarations are var-scoped). swc reports several of
  these first, with its own messages.

swc's parser itself rejects `#constructor` (`Classes can't have a private
field named '#constructor'.`), `super.#x`, and a private name as an object
literal or object pattern key.

### 7.7 Errors

- `Error`, `TypeError`, `RangeError`, `ReferenceError`, `SyntaxError`,
  `EvalError`, `URIError`, `AggregateError` are `SELF_ALLOC` constructors
  (callable with or without `new`). An error object is a `CLASS_ERROR` object whose `proto` is
  `new.target.prototype` (one of the static `%…Error.prototype%` blocks unless
  subclassed) with own non-enumerable `message` (only when one was given) and
  `stack` (`"Name: message"`, a single line, no frames); `name` is inherited.
  `InstallErrorCause`: when the second argument is an object with a `cause`
  property (own or inherited, even `undefined`), the instance gets an own
  non-enumerable `cause` = `options.cause`, read after the message's
  `ToString`; otherwise it has no `cause`.
  `Error.prototype.toString` formats `Name: message`.
- `AggregateError(errors, message, options)` takes the message second and
  reads all three arguments first (past the `K` slots from a spread call's
  argument array), then runs `InstallErrorCause` from `options` and adds an
  own non-enumerable `errors` array of the values iterating `errors` yields
  (`TypeError` when it is not iterable). It always runs its constructor
  body.
- `throw` accepts any value.
- **Engine-raised errors** are real error objects linked to the same static
  prototypes, catchable with `try`/`catch` and recognised by `instanceof`:
  `TypeError: x is not a function` / `o.m is not a function`,
  `TypeError: x is not a constructor`,
  `TypeError: Cannot read properties of null (reading 'k')`,
  `TypeError: Cannot set properties of undefined (setting 'k')`,
  `TypeError: x is not iterable`,
  `TypeError: Converting circular structure to JSON`,
  `ReferenceError: x is not defined`,
  `ReferenceError: Cannot access 'x' before initialization`,
  `RangeError: Invalid array length`, a `JSON.parse` `SyntaxError`, the
  `%ThrowTypeError%` `TypeError`, and others.
- Out of memory throws a `RangeError` (`message` `"out of memory"`), always the
  same object, catchable like any other; only an out-of-memory that finds the
  reserve already spent ends the program (§5.6).

### 7.8 Iteration, generators & async

**Iteration protocol.** Every consumer opens an iterator record
(`IterRecord`, `ir.rs`: the iterator, its cached `next`, a state and the last
result, held in locals or generator frame cells; it never escapes, so it is
not a heap block) and drives it with the record nodes:

- **`GetIterator`** (`IrNode::GetIterator`, the internal `Protocol`
  builtins of `emit/builtins/iter.rs`): `GetMethod(value, @@iterator)`
  (`TypeError: <x> is not iterable` when absent), its call (a non-object
  result throws `TypeError: Result of the Symbol.iterator method is not an
  object`), then `next` read once, so later changes to `next` are
  unobservable. A source passing its guarded fast path (§6.4) is walked
  directly instead, decided once at this point. `IrNode::OpenIterator`
  always opens the protocol iterator (`yield*`), or with `is_async` the
  async record (below).
- **`IteratorStep` / `IteratorValue`**: call the cached `next`; a non-object
  result throws `TypeError: Iterator result <x> is not an object`;
  `ToBoolean(result.done)` ends the walk; `value` is read only when not done.
  Any throw from the step leaves the record done, so it is never closed
  afterwards.
- **`IteratorClose`** (`IrNode::IteratorClose`): `GetMethod(iterator,
  "return")` and its call on a record not yet done; for a throw completion
  any throw of the close is discarded and the original rethrown; otherwise
  a throwing `return()` propagates and a non-object result throws.
- **`IterableToList`** (`SpreadElem::Spread`, `ProtocolOp::ToList`): drain,
  never close.

| consumer | steps | closes on |
|---|---|---|
| `for-of` (§7.3) | one per iteration | `break`, `return`, a labelled jump, a throw from the body or the head's destructuring |
| array destructuring (§7.5) | one per element or hole; a rest drains; an empty pattern steps nothing | after the pattern unless done; a throw from a default, nested pattern or target |
| spread (array, call, `new`), `AggregateError` | drain | never |
| Map/Set constructors (`AddEntriesFromIterable`), `Object.fromEntries` | one per entry | an adder or define throw, a non-object entry, a throwing entry read |
| `Array.from`, `Math.sumPrecise` | one per value | a `mapFn` or define throw; a non-number value (`sumPrecise`) |
| `Promise.all` / `allSettled` / `any` / `race` (§6.7) | one per value | a throw not raised by the iterator itself |
| `yield*` | forwarded `next` / `throw` / `return` | a `throw` on a delegate without `throw` |
| `Iterator.from`, the helpers (§7.9) | on demand | the helper's `return()`, a throwing callback, an early result |
| `for await`, async `yield*`, `Array.fromAsync` | awaited per step (below) | `AsyncIteratorClose` (the `return()` result awaited) |

Codegen wraps a closing consumer's steps (a `for-of` body, a pattern whose
elements can throw) as `try { BODY } catch (e) { IteratorClose(rec, throw);
throw e } finally { IteratorClose(rec) }` over the `finally` routing of §6.6;
a record that is done (exhausted, or its step threw) is left alone.

**Generators.** A `function*` — declaration (top-level or nested, hoisted like
any nested function declaration), expression, or object-literal / class
generator method (`*m(){}`, instance or static) — compiles to two uniform-ABI
functions: a **constructor** that allocates a `TYPE_GENERATOR` handle and its
frame without running the body, and a **state machine** `SM(sent, gen, …)` that
resumes at `gen.state`. Every generator local lives in a frame cell, so locals
survive suspension. The constructor is an ordinary lifted function value (not a
constructor, so `new` throws a `TypeError`): it binds the parameters when
called — defaults, destructuring and rest, exactly like an ordinary function,
so a binding error throws from the call before any `next()` — into cells the
body shares by reference, and seeds a `this` cell from its call's receiver. A
binding the body captures from an enclosing function is a frame slot holding
that function's own cell (read through the constructor's environment), so the
capture is by reference and outlives the creating call. `arguments` in the body
is the constructor's (unmapped) arguments object, built at the call. A named generator
expression binds its name, read-only, inside its body. The body is lowered to a control-flow
graph of basic blocks (`codegen/generator.rs`); control flow crossing a suspend
point — `if`, loops, `switch`, `for-of`/`for-in`, `try`/`catch`/`finally`,
`break`/`continue`/`return` — becomes block jumps dispatched by a
`loop { br_table(state) }`, while regions without a suspend keep ordinary wasm
control flow. A `yield` may appear inside a larger expression
(`1 + (yield 2)`, a loop condition, a destructuring assignment's RHS or
default, a computed pattern key); the expression is linearized around it, and
a compiler temporary that later statements read across the suspend (the
source of `const { [yield k]: v } = o`) lives in a frame cell.

- Each generator function owns a `prototype` (writable, non-enumerable,
  non-configurable): a fresh object inheriting `%GeneratorPrototype%`
  (`next`, `return`, `throw`), with no own `constructor`. A generator object
  inherits its function's `prototype` read at call time, or
  `%GeneratorPrototype%` when that is not an object; so `g() instanceof g`.
  `it.next(v)` sends `v` and returns a fresh `{ value, done }` object;
  `throw(e)` throws at the suspended `yield` (reaching an enclosing `catch`)
  and `return(v)` injects a return completion there: the state machine
  resumes with `v` pending and the mode set, and the resumed `yield` enters
  its return block, which runs the enclosing `finally` blocks (a `finally`
  may `yield` again or override the value) before completing with
  `{ value: v, done: true }`. Before the first `next` or after completion
  both complete the generator directly. These are ordinary methods: a user
  object's own `next` is called like any method. An `it.next(v)` call site
  runs the resume inline while `it` is a generator without own properties
  whose `proto` is `%GeneratorPrototype%`, or an object without own
  properties inheriting it (its function's untouched `prototype`), and
  `%GeneratorPrototype%` is clean (§6.4); otherwise it is a method call.
  `%GeneratorFunction.prototype%` is not modeled: a generator function
  inherits `%Function.prototype%`.
- `yield* g` is an expression whose value is the delegate's return value.
  The CFG builder expands it into a loop over the protocol iterator of `g`
  (`next` read once): the outer generator's `next(v)` / `throw(e)` /
  `return(v)` resume its `yield` into per-mode blocks that call the
  delegate's `next(v)` / `throw(e)` / `return(v)` (`GetMethod`); a delegate
  without `throw` is closed, then a `TypeError` is thrown; one without
  `return` completes the outer generator with `return v`; a non-object
  result throws a `TypeError`. Only `done` is read per step: while not done
  the outer `next()` / `throw()` / `return()` returns the delegate's result
  object itself (spec `GeneratorYield(innerResult)`; the state machine
  flags the handle's `128` bit, §5.2), and its `value` is read once, as the
  delegate's return value, when done. A generator whose body has a `yield*`
  (bit `64`) is iterated through the protocol, never by direct state-machine
  resumes. Each delegated step calls the cached `next` (or the
  `throw` / `return` method read per step); there is no fast path for a
  clean generator delegate. In an async generator the delegate is opened
  with the async `GetIterator` and every inner result (the `return()` of a
  closed delegate included) is awaited before its check (ES2025 §15.5.5); a
  non-object result throws `Iterator result <x> is not an object` in both
  forms.

**`async`/`await`.** An `async function` (in every position a `function*` may
take, including `async` methods; no `prototype`) or `async` arrow is a generator whose `await e` is a `yield e`; its
function value is a wrapper that builds the generator and hands it to the guest
async driver `__async_spawn` (§6.7), returning the result promise; each `await`
takes `PromiseResolve(%Promise%, v)` and resumes the body from the microtask
queue (one tick for a native promise); a parameter binding error
rejects that promise instead of throwing. An `async` arrow keeps arrow
semantics: no `prototype`, not a constructor, and `arguments` / `new.target`
are the enclosing function's, and so is `this` (§7.4). All generator control-flow support applies to `await`; a rejected
awaited promise throws into the body at the `await`, so `try`/`catch` around
`await` works. Top-level `await` is supported in `main`.

**Async generators.** An `async function*` (in every position a `function*`
may take, including `async *m(){}` methods) compiles like a generator whose
`await e` is a `yield` of `e` flagged `GEN_AWAIT` on the handle, `yield e` is
`yield (await e)`, and `return e` is `return (await e)`. Its function owns a
`prototype` inheriting `%AsyncGeneratorPrototype%`, which async generator
objects inherit as generator objects do theirs; the handle carries `GEN_ASYNC`,
so it is neither a sync iterable nor a valid receiver of the
`%GeneratorPrototype%` methods. `%AsyncGeneratorPrototype%`'s `next(v)`,
`throw(e)` and `return(v)` (guest builtins, no host support) append a request
`{ kind, value, promise }` to the handle's queue and return its fresh promise
(a bad receiver gets a rejected one); the queue is then served in order unless
the body is busy:

- `next(v)` / `throw(e)` resume the body (sending `v`, or throwing `e` at the
  suspended `yield`). A `yield` fulfills the front request with
  `{ value, done: false }`, completion with `{ value, done: true }`, and a
  throw out of the body rejects it; the next request is then served.
- An `await` (flagged suspend) leaves the body busy and registers promise
  reactions on `PromiseResolve(%Promise%, operand)` (a throw of it is thrown
  at the `await`); its settlement resumes the body (a rejection throws at the
  `await`) and continues serving the queue from the microtask queue. Requests
  made meanwhile, from outside or from the body itself, wait in the queue.
- An abrupt request before the body started completes it. On a completed
  generator `next` fulfills `{ undefined, done: true }`, `throw(e)` rejects
  with `e`, and `return(v)` awaits `v` and fulfills `{ v, done: true }` (or
  rejects). `return(v)` at a suspended `yield` resumes the body with a
  return completion there, which awaits `v`, then runs the enclosing
  `finally` blocks like a generator's.
- `yield*` in an async generator delegates through the async protocol (see
  generators above).

**Async iteration.** The async `GetIterator` (`IrNode::OpenIterator` with
`is_async`, `emit/builtins/async_iter.rs`) calls `@@asyncIterator` (a
non-object result throws); without one it wraps the sync iterator of
`@@iterator` in `CreateAsyncFromSyncIterator`, and with neither throws
`TypeError: <x> is not async iterable`. `%AsyncFromSyncIteratorPrototype%`
(hidden, `CLASS_ITERATOR` kind `32`, a 48-byte block holding the sync record)
has guest `next` / `return` / `throw`: each calls the sync method and returns
a promise for `{ value, done }` after `Promise.resolve(value)`; a rejected
value closes the sync iterator unless done. A missing `return` fulfills
`{ value, done: true }`; a missing `throw` closes the sync iterator, then
rejects with `TypeError: The iterator does not provide a 'throw' method`.
The consumers (`for await`, async `yield*`) await `next()` through the
state machine (`AsyncIteratorNext` / `AsyncIteratorComplete` /
`AsyncIteratorReturn`); the wrapper's builtin bodies need a positional
slot, so a program with a `for await` has `K` ≥ 1 (§4.2).
`Array.fromAsync` is a builtin promise-reaction state machine over a state
block (`emit/builtins/array.rs`): it returns a promise at once (every abrupt
completion rejects it), takes `@@asyncIterator`, else `@@iterator` through
the async-from-sync wrapper, else an array-like, and awaits each `next()`
result, array-like element and mapped value (one microtask turn per step); a
`mapFn` throw or rejection, or a failed define, closes the iterator
(`AsyncIteratorClose`, its `return()` result awaited) before rejecting. Its
reactions read the settled value from slot 0, so they trap when `K` = 0.

**Promises.** `new Promise(executor)`, `Promise.resolve`, `Promise.reject`,
`Promise.all` / `allSettled` / `any` / `race`, `Promise.prototype.then` /
`catch` / `finally` (§6.7).

### 7.9 Built-ins

Builtins are ordinary function objects installed on the realm (§6.4); they are
first-class values, can be shadowed, replaced, or called through `call` /
`apply`, and see their arguments at run time (missing ones are `undefined`,
extra ones ignored). Inline code is only a guarded fast path over them.

| area | supported |
|---|---|
| global | `globalThis`, `NaN`, `Infinity`, `undefined`, `isNaN`, `isFinite`, `parseInt(s, radix)`, `parseFloat(s)` (both, like `Number(s)`, skip JS whitespace: Unicode `WhiteSpace`, `LineTerminator`, U+FEFF) |
| `console` | `log` |
| `Object` | `Object(v)` / `new Object(v)` (ToObject), `Object.keys`, `Object.values`, `Object.entries`, `Object.defineProperty`, `defineProperties`, `getOwnPropertyDescriptor`, `getOwnPropertyDescriptors`, `getOwnPropertyNames`, `getOwnPropertySymbols`, `preventExtensions`, `isExtensible`, `seal`, `isSealed`, `freeze`, `isFrozen` (§6.3; primitives: the setters return them, `isExtensible` is `false`, `isSealed` / `isFrozen` are `true`), `create`, `getPrototypeOf`, `setPrototypeOf`, `assign`, `hasOwn`, `is`, `fromEntries` (over any iterable; a bad entry closes its iterator); `Object.prototype.hasOwnProperty`, `isPrototypeOf`, `propertyIsEnumerable`, `toString` (`[object Tag]`: a string `@@toStringTag` on the receiver's chain, else the builtin tag; `Map`, `Set`, `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry`, `Promise`, `Generator`, `AsyncGenerator`, `Symbol`, `Math`, `JSON` and the Array / String / Map / Set Iterator prototypes own a configurable-only `@@toStringTag`), `toLocaleString`, `valueOf`, the `__proto__` accessor, `__defineGetter__`, `__defineSetter__`, `__lookupGetter__`, `__lookupSetter__` |
| `Function` | `Function.prototype.call`, `apply` (`CreateListFromArrayLike`: arrays, `arguments`, any object with `length`), `bind` (bound function exotic objects, §5.5), `toString` (always the `NativeFunction` form `function name() { [native code] }`: no source text is kept; the name is omitted unless it is an `IdentifierName`, optionally after `get `/`set `); `%Function.prototype%` `length`/`name`, `caller`/`arguments` `%ThrowTypeError%` accessors, `[@@hasInstance]` (`OrdinaryHasInstance`; non-writable, non-enumerable, non-configurable); `Function(..)` throws `TypeError` |
| `Array` | `Array(n)` / `new Array(..)`, `Array.isArray`, `Array.of`, `Array.from(items, mapFn, thisArg)` (any iterable through its iterator, closed when `mapFn` or the define throws; else an array-like; `this` constructed when a constructor), `Array.fromAsync(items, mapFn, thisArg)` (the same over an async or sync iterable or an array-like, every element and mapped value awaited, §7.8); `push` / `pop` (variadic), `map`, `filter`, `forEach`, `reduce`, `reduceRight` (with or without an initial value; callbacks receive `(element, index, array)` and `thisArg`; every callback of the higher-order methods gets all its specified arguments — `reduce` / `reduceRight` `(acc, element, index, array)`, `sort` comparators `(a, b)` — the first `K` positionally and, when there are more, all of them in the spread argument array (§4.3): the inline loop allocates it once and refills it per call, and only when some user function gathers its arguments (a rest parameter or `arguments`) or the callback is a builtin, so other callbacks keep the plain `K`-slot call), `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`, `at`, `indexOf`, `lastIndexOf`, `includes`, `join`, `slice`, `concat`, `flat`, `flatMap`, `keys`, `values` (also `[@@iterator]`), `entries` (Array Iterators over `ToObject(this)`, re-reading `length` per step), `toString`, `toLocaleString` (`Invoke(element, "toLocaleString")` per element; `null` / `undefined` / holes are empty), and the mutating `splice`, `sort(comparefn)` (a stable bottom-up merge sort; `undefined`s last, holes after them), `reverse`, `shift`, `unshift`, `fill`, `copyWithin`, and the change-by-copy `with`, `toReversed`, `toSorted(comparefn)` (the same stable sort), `toSpliced` (each reads every index by `Get`, so holes become `undefined`, and returns a plain `ArrayCreate` array, never a species instance) — every method generic over array-likes; `map` / `filter` / `slice` / `splice` / `concat` / `flat` / `flatMap` create their result by `ArraySpeciesCreate` (`constructor[@@species]`; `%Array%`, `%Map%`, `%Set%` and `%Promise%` have the builtin `get [Symbol.species]` returning `this`), `concat` spreads the items `IsConcatSpreadable` accepts (`@@isConcatSpreadable`, else `IsArray`), and `Array.prototype[@@unscopables]` is the spec's `null`-prototype list |
| `String` | `String(v)`, `new String(v)` (wrapper, virtual indices §5.3); `String.fromCharCode`, `String.fromCodePoint` (`RangeError` for a non-code-point; lone surrogates allowed), `String.raw` (over any `{ raw: array-like }`); `at`, `charAt`, `charCodeAt`, `codePointAt`, `concat`, `endsWith`, `includes`, `indexOf`, `isWellFormed`, `lastIndexOf`, `localeCompare` (code-unit order, not locale collation), `normalize` (all four forms; an unknown form is a `RangeError`), `padEnd`, `padStart`, `repeat` (a result past `MAX_STRING_LENGTH` code units is a `RangeError`, as for `padStart` / `padEnd`), `slice`, `split`, `startsWith`, `substring`, `toLowerCase`, `toUpperCase` (full Unicode default case mapping — `SpecialCasing`, `Final_Sigma` — per well-formed run, lone surrogates passed through, in the guest from static tables, `emit/unicode.rs`; a result past `MAX_STRING_LENGTH` code units is a `RangeError`), `toLocaleLowerCase`, `toLocaleUpperCase` (no locale data: the same mappings), `toWellFormed`, `trim`, `trimEnd`, `trimStart` (JS whitespace, one table), `toString`, `valueOf`, `[@@iterator]` (a String Iterator stepping one code point per `next` — a lead and trail surrogate pair is one two-unit string, a lone surrogate its own unit — through `__str_iter_next`); `search`, `match`, `matchAll`, `replace`, `replaceAll`, `split` call an object argument's `@@search` / `@@match` / `@@matchAll` / `@@replace` / `@@split` method with `(O, …)` before `ToString(this)` (`matchAll` / `replaceAll` first require an `IsRegExp` argument's `flags` to hold `g`), and `includes` / `startsWith` / `endsWith` throw a `TypeError` for an `IsRegExp` argument (`ToBoolean(arg[@@match])`); `search`, `match`, `replace`, `replaceAll` with string patterns (`$$`, `$&`, `` $` ``, `$'` substitutions; function replacers get `(match, position, string)`; the RegExp-syntax check of a `search` / `match` pattern runs in the guest); `.length`, indexing. Every generic method is `RequireObjectCoercible(this)` then `ToString(this)`; all run in the guest on code units (`emit/builtins/string.rs`; `normalize` over the generated Unicode tables of `emit/unicode.rs`: full decomposition, canonical ordering, then canonical composition for NFC / NFKC), surrogate pairs combined by `codePointAt` / `fromCodePoint` / `isWellFormed` |
| `Number` / `Boolean` | `Number(v)`, `Boolean(v)`, wrappers via `new`; `Number.isInteger`, `isSafeInteger`, `isFinite`, `isNaN` (no coercion), `Number.parseFloat` / `parseInt` (the global functions); `EPSILON`, `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `MAX_VALUE`, `MIN_VALUE`, `NaN`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`; `Number.prototype.toString(radix)`, `toFixed`, `toExponential`, `toPrecision`, `toLocaleString` (= `toString()`), `valueOf`; `Boolean.prototype.toString`, `valueOf` |
| `Math` | `sumPrecise(items)` (any iterable of numbers, else a `TypeError` after closing it; summed exactly in a fixed-point limb accumulator and rounded once; `NaN` / opposite infinities give `NaN`, no value or only `-0` gives `-0`), and every ES2025 function: `floor`, `ceil`, `round`, `trunc`, `abs`, `sqrt`, `sign`, `min`, `max`, `hypot` (variadic), `pow`, `clz32`, `fround`, `imul`, `f16round` (ES2026; binary16 through the guest `__f16_encode` / `__f16_decode`, §5.8.6) in wasm; `cbrt`, `log`, `log2`, `log10`, `log1p`, `exp`, `expm1`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh` as guest helpers (`emit/libm.rs`, a port of the `libm` crate's musl/msun algorithms, so results are the same on every host); `random` on the host (§6.1); `E`, `LN10`, `LN2`, `LOG10E`, `LOG2E`, `PI`, `SQRT1_2`, `SQRT2` |
| `JSON` | `JSON.stringify(value, replacer, space)` (replacer function or property list, `space` number/string (a string's first 10 code units), `toJSON`, wrappers unwrapped, functions and symbols skipped, cycles throw, a lone surrogate written as `\udxxx`), `JSON.parse(text, reviver)` (in-wasm recursive descent, `\uXXXX` escapes incl. lone surrogates, reviver walk) |
| `Map` / `Set` | `new Map(iterable)`, `new Set(iterable)`; Map `get`/`set`/`has`/`delete`/`clear`/`forEach`/`size`/`keys`/`values`/`entries` (also `[@@iterator]`); Set `add`/`has`/`delete`/`clear`/`forEach`/`size`/`values` (also `keys` and `[@@iterator]`)/`entries`; the iterators walk the live backing entries, so entries appended during iteration are seen |
| `WeakMap` / `WeakSet` | `new WeakMap(iterable)`, `new WeakSet(iterable)` (calling either without `new` is a `TypeError`; the iterable's entries go through the `set` / `add` read from the new object, like `Map` / `Set`); WeakMap `get`/`set`/`has`/`delete`, WeakSet `add`/`has`/`delete`. A key must be `CanBeHeldWeakly`: an object, or a symbol not in the `Symbol.for` registry (well-known symbols qualify). `set` / `add` of another key throws `TypeError: Invalid value used as weak map key` / `… in weak set`; `get` / `has` / `delete` of one return `undefined` / `false`. A wrong receiver throws `TypeError: Method WeakMap.prototype.set called on incompatible receiver` (per method). Entries die with their key: a value is reachable through the map only while its key is (ephemerons, §5.7); no size, iteration or `clear` |
| `WeakRef` / `FinalizationRegistry` | `new WeakRef(target)` (`TypeError: WeakRef: target must be an object` unless `CanBeHeldWeakly`), `WeakRef.prototype.deref()` (the target, or `undefined` once collected); creation and every successful `deref()` keep the target alive until the end of the current job (`AddToKeptObjects`, cleared at the next microtask checkpoint, §6.7). `new FinalizationRegistry(cleanup)` (`TypeError: FinalizationRegistry: cleanup must be callable`), `register(target, held, token)` (`TypeError` for a target that cannot be held weakly, `SameValue(target, held)`, or a token that is neither `undefined` nor holdable weakly; returns `undefined`), `unregister(token)` (`TypeError` for a token that cannot be held weakly; removes every cell of `token`, collected or not, and returns whether any was removed). After a collection finds a registered target dead, the next `__drain()` checkpoint calls `cleanup(held)` once per cell (§6.7); a throwing `cleanup` ends the run like an uncaught exception. `cleanupSome` is not provided (a proposal, not ES2025). Both constructors require `new`; subclasses construct |
| `Promise` | §6.7, §7.8; `then` / `finally` create their result through `SpeciesConstructor(this, %Promise%)` (`get [Symbol.species]` on `%Promise%`); `Promise.resolve` / `Promise.reject` construct from `this` (`PromiseResolve(C, x)` / `NewPromiseCapability(C)`; a non-constructor `this` throws `TypeError`); `Promise.all`, `allSettled`, `any` (all rejected: an `AggregateError` with `errors`), `race` over any iterable (§6.7) |
| `Date` | `new Date()`, `new Date(value)` (a Date's time value, else `ToPrimitive(value)`: a string is parsed as by `Date.parse`, anything else `ToNumber`), `new Date(year, month[, date, hours, minutes, seconds, ms])` (UTC fields: `MakeDay` / `MakeTime`, `TimeClip`), `Date()` (current time as a string), `Date.now()`, `Date.UTC(year[, month, ..])`, `Date.parse(string)` (`ToString`, then a guest-side parser of exactly the Date Time String Format — `YYYY[-MM[-DD]]` or `±YYYYYY` (not `-000000`), optional `THH:mm[:ss[.sss]]` and `Z` / `±HH:mm`, range-checked fields, `24:00` only as `24:00:00.000`; date-only forms are UTC, a date-time without offset is local time = UTC — and the `toString` (`Www Mmm DD YYYY HH:mm:ss GMT±hhmm[ (..)]`) / `toUTCString` outputs; anything else is `NaN`, the result `TimeClip`ped); `getTime`, `valueOf`, `getFullYear`, `getMonth`, `getDate`, `getDay`, `getHours`, `getMinutes`, `getSeconds`, `getMilliseconds`, the `getUTC*` twins, `getTimezoneOffset` (`0`), Annex B `getYear`; `setMilliseconds`, `setSeconds`, `setMinutes`, `setHours`, `setDate`, `setMonth`, `setFullYear`, the `setUTC*` twins (same body, distinct functions), `setTime`, Annex B `setYear` (`thisTimeValue` first, then `ToNumber` of every argument in order, then a `NaN` time value returns `NaN` unstored — `setFullYear` / `setYear` start from `+0` — else `TimeClip` is stored and returned); `toISOString` (`RangeError` on an invalid date; expanded `±YYYYYY` years), `toJSON`, `toString` (`Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)`), `toDateString` (`Thu Jan 01 1970`), `toTimeString` (`00:00:00 GMT+0000 (Coordinated Universal Time)`), `toUTCString` (`Thu, 01 Jan 1970 00:00:00 GMT`; Annex B `toGMTString` is the same function object), `toLocaleString` / `toLocaleDateString` / `toLocaleTimeString` (the `toString` / `toDateString` / `toTimeString` texts) — the string forms sign a negative year and pad it to four digits (`-0001`) and render an invalid date as `Invalid Date` —, `[@@toPrimitive]` (configurable only) — local time is UTC (`LocalTZA = 0`), computed in wasm |
| `Symbol` | `Symbol(desc)` (a fresh symbol; `new Symbol()` throws `TypeError`, though `Symbol` is a constructor for `IsConstructor`), `Symbol.for` / `Symbol.keyFor` (the hidden registry; `keyFor` of a non-symbol throws), the 13 well-known symbols (non-writable, non-enumerable, non-configurable); `Symbol.prototype.toString` (`Symbol(desc)`), `valueOf`, the `description` getter (a program that names `description` compiles the accessor dispatch of `__get`), `[@@toPrimitive]`, `[@@toStringTag]` `"Symbol"` (both configurable only), each over `thisSymbolValue` (a symbol or a `CLASS_SYMBOL` wrapper) |
| `BigInt` | §6.10: `BigInt(v)` (a number must be integral, else `RangeError`; strings in every `StringIntegerLiteral` radix, else `SyntaxError`; `new BigInt()` throws `TypeError`, though `BigInt` is a constructor for `IsConstructor`), `BigInt.asIntN` / `asUintN`; `BigInt.prototype.toString(radix)`, `toLocaleString` (`toString()`, no locale data), `valueOf`, `@@toStringTag`; `Number(1n)`; every arithmetic, bitwise, shift, relational, equality and update operator on BigInts (§7.2) |
| errors | §7.7 |
| generators | `%GeneratorPrototype%` `next`, `return`, `throw` |
| iterators | `%IteratorPrototype%[@@iterator]`, `%AsyncIteratorPrototype%[@@asyncIterator]` (both return `this`); the `next` of the Array, String, Map and Set Iterator prototypes (a receiver that is not an iterator of the family throws `TypeError: next method called on incompatible receiver`); `Iterator` (abstract: calling it, or `new` with `new.target === Iterator`, throws `TypeError`; a subclass constructs), `Iterator.prototype` (`%IteratorPrototype%`, non-writable) with `constructor` (`%Iterator%`) and `@@toStringTag` (`"Iterator"`) accessors whose setters are `SetterThatIgnoresPrototypeProperties` (a `TypeError` on a non-object or on `Iterator.prototype` itself; an own data property on any other receiver); `Iterator.from(O)` (`GetIteratorFlattenable`: strings are iterated, other primitives throw `TypeError`; an iterator inheriting `Iterator.prototype` is returned as is, any other is wrapped in a `CLASS_ITERATOR` object of kind `16` over `%WrapForValidIteratorPrototype%`, whose `next` / `return` forward to the wrapped record, `next` read once); the lazy helpers `Iterator.prototype.map` / `filter` / `take` / `drop` / `flatMap` (`this` must be an object; the argument is validated first — a non-callable callback is a `TypeError`, a `take` / `drop` limit goes through `ToNumber` and `NaN` or a negative integer is a `RangeError` — closing `this` on failure; then `next` is read once and a helper object is returned without stepping). A helper's `next` steps the underlying iterator only on demand; a throwing callback, or a `flatMap` result that is not an object (a string included) or not iterable, closes the underlying iterator; `take` closes it once the limit is reached; a `next` / `return` while the helper runs throws `TypeError`; `return()` closes the underlying iterator (and a `flatMap`'s open inner one first) and completes the helper. The eager helpers `reduce` / `toArray` / `forEach` / `some` / `every` / `find` validate `this` and the callback the same way, read `next` once and consume the iterator, passing a counter as the callback's last argument (`reduce` without an initial value starts from the first value with counter `1`, and throws `TypeError` on an empty iterator); a throwing callback closes the iterator under its throw, `some` / `every` / `find` close it on an early result, and a throw from `next` or a result's `value` propagates without closing. A program that names `Iterator`, `constructor`, `getPrototypeOf` or `setPrototypeOf` compiles the accessor dispatch of `__get` / `__set` |
| `ArrayBuffer` | §5.8.7: `new ArrayBuffer(length, { maxByteLength })` (calling it without `new` is a `TypeError`; `ToIndex` `RangeError`s, a length or maximum past `2^29` is `RangeError: Array buffer allocation failed`; subclassable through `new.target`), `ArrayBuffer.isView` (`true` only for a typed array or `DataView`), `get [@@species]`; prototype getters `byteLength`, `maxByteLength`, `resizable`, `detached`; `resize`, `slice` (through `SpeciesConstructor`), `transfer`, `transferToFixedLength` (both detach the receiver), `@@toStringTag`; rendered as `ArrayBuffer { [Uint8Contents]: <..>, byteLength: n }` (§5.8.8). No immutable buffers, no `SharedArrayBuffer` (§5.8.9) |
| typed arrays | §5.8.7, stage T3a: `%TypedArray%` (abstract: calling or constructing it is a `TypeError`) with `from`, `of`, `get [@@species]`; `Int8Array`, `Uint8Array`, `Uint8ClampedArray`, `Int16Array`, `Uint16Array`, `Int32Array`, `Uint32Array`, `Float16Array` (stage T5a; IEEE binary16 rounding half to even straight from the `f64`), `Float32Array`, `Float64Array` (every constructor overload incl. length-tracking views over resizable buffers, `BYTES_PER_ELEMENT`, subclassable through `new.target`); `%TypedArray.prototype%` getters `buffer`, `byteLength`, `byteOffset`, `length`, `get [@@toStringTag]`, and `keys` / `values` / `entries` / `[@@iterator]`; stage T3b: `at`, `copyWithin`, `every`, `fill`, `filter`, `find`, `findIndex`, `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`, `join`, `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`, `set` (array-like and typed-array sources; same-buffer overlap as if the source were cloned), `slice`, `some`, `sort` (numeric with `-0 < +0` and `NaN` last, or by comparator; stable), `subarray` (length-tracking result from a length-tracking source with `end` `undefined`), `toLocaleString`, `toReversed`, `toSorted`, `with`, and `toString` (the `Array.prototype.toString` function object) — each starts with `ValidateTypedArray` (`set` / `subarray` only check the receiver) and re-validates after user code as ECMA-262 does (a detached or shrunk buffer reads `undefined` and ignores writes mid-iteration); `map` / `filter` / `slice` / `subarray` create through `TypedArraySpeciesCreate`, `toReversed` / `toSorted` / `with` the receiver's kind; the integer-indexed exotic hooks (§5.8.5); rendered as `Uint8Array(3) [ 1, 2, 3 ]` (§5.8.8); stage T5a: `Uint8Array.fromBase64(string, options)`, `Uint8Array.fromHex(string)`, `Uint8Array.prototype.toBase64(options)`, `toHex()`, `setFromBase64(string, options)`, `setFromHex(string)` (both alphabets, `omitPadding`, every `lastChunkHandling` mode, ASCII whitespace skipped in base64; `setFrom*` return `{ read, written }` and keep the bytes decoded before a `SyntaxError`). **Target, not yet available** (stage T5b): `BigInt64Array`, `BigUint64Array` |
| `DataView` | §5.8.7: `new DataView(buffer, byteOffset, byteLength)` (calling it without `new` is a `TypeError`; a non-`ArrayBuffer` `buffer` or a detached one is a `TypeError`, an offset or length outside the buffer a `RangeError`, re-checked after `GetPrototypeFromConstructor`; an `undefined` `byteLength` over a resizable buffer makes a length-tracking view; subclassable through `new.target`); getters `buffer`, `byteLength`, `byteOffset` (the last two throw `TypeError` on an out-of-bounds or detached view); `get` / `set` of `Int8`, `Uint8`, `Int16`, `Uint16`, `Int32`, `Uint32`, `Float16`, `Float32`, `Float64` (big-endian unless `littleEndian` is truthy; `ToIndex(byteOffset)`, the setter's `ToNumber(value)`, `ToBoolean(littleEndian)`, then the out-of-bounds `TypeError` and the range `RangeError`; a NaN reads as the canonical NaN); `@@toStringTag`; rendered as `DataView { byteLength: n, byteOffset: n, buffer: ArrayBuffer { .. } }` (§5.8.8). The BigInt pairs arrive with stage T5b (§5.8.9) |

No input produces an invalid wasm module.

---

## 8. Diagnostics

**Compile-time errors** — parse failures from swc and every codegen rejection —
are `jsc_core::error::CompileError { span: Option<Span>, msg }`. The CLI renders
a spanned error as a rustc-style caret block:

```text
error: cannot assign to constant binding: c
  --> <eval>:1:14
   |
 1 | const c = 1; c = 2
   |              ^
```

The label is `<eval>` for `-e`/`-p` input and the file path otherwise
(`jsc-wasm` uses `<source>`). Columns count characters; a multi-column span
underlines its full width. Rendering never panics: span offsets are clamped to
the source and snapped outward to character boundaries (a span starting inside
a multi-byte character points at that character), and a CRLF line is printed
without its `\r`. A span-less error prints its bare message. Because
`CompileError` implements `std::error::Error`, it travels through `anyhow` and
is recovered with `downcast_ref`.

Representative messages: `cannot assign to constant binding: c`,
`regular expression literals are not supported`, `with statements are not supported`,
`async generator functions are unsupported`, `Invalid left-hand side in
assignment`, `private name #x is not defined`.

**Run-time errors** carry no source span:

- An uncaught JS exception fails the run with `Uncaught <rendered value>`
  (e.g. `Uncaught TypeError: Cannot read properties of null (reading 'x')`,
  `Uncaught 1`); an unhandled rejection with `Uncaught (in promise) <value>`.
- Uncaught heap exhaustion fails with `Uncaught RangeError: out of memory`, as
  does an out-of-memory that finds the reserve spent (the static line,
  §5.6).
- Remaining machine faults surface as wasmtime traps.

---

## 9. CLI

`crates/jsc/src/main.rs`, node-like:

| command | behaviour |
|---|---|
| `jsc FILE` | compile and run the file; only `console.log` output is printed |
| `jsc -e SRC` / `--eval SRC` | compile and run inline source; only `console.log` output is printed |
| `jsc -p SRC` / `--print SRC` | compile inline source with `print_completion` and run it; the module prints the completion value (last expression statement) after the final drain |
| `jsc build FILE -o OUT` | write the wasm command module to `OUT` (default options); `-o -` writes to stdout. The artifact is a self-contained WASI preview-1 command (§6.1): `wasmtime run OUT`, `node:wasi` (`version: 'preview1'`, `wasi.start(instance)`) or `@cdlab/jsc`'s polyfill run it |

Exactly one of `-e`, `-p`, or `FILE` is required (a clap argument group);
`-e`/`-p`/`-o` accept values beginning with `-` (`jsc -p "-0"`). `console.log`
writes each line to stdout with one `fd_write` while the module runs. The CLI
runs modules on `jsc_core::run`, a wasmtime runner with four hand-written
preview-1 imports; the process exit code is the module's `proc_exit` code (`0`
when `_start` returns), and a trap prints its message to stderr and exits `1`.

**Exit codes.**

| code | meaning |
|---|---|
| `0` | success |
| `1` | uncaught exception, unhandled rejection, out of memory, runtime trap, or I/O failure (unreadable input, unwritable output) |
| `2` | compile error (parse or lowering); also clap usage errors |

Errors are printed to stderr.

---

## 10. Conformance (test262)

`crates/jsc-test262` runs the official tc39/test262 suite against the release
`jsc` binary, using the official harness files unmodified.

- **Checkout.** `scripts/test262-fetch.sh` clones test262 at a pinned commit
  (`2e0a56762801e275a9fdf96dc49d90ba0cddcf63`) into `.tmp/test262`; it is
  idempotent.
- **Harness.** At startup the runner reads every `.tmp/test262/harness/*.js`,
  checks that `sta.js` + `assert.js` compile and run (a setup error otherwise),
  and probes every other include together with `sta.js` + `assert.js` (`doneprintHandle.js`
  after the host `print` prelude, as async tests use it); only the includes
  that compile are usable, and it prints their list.
- **Eligibility** (`filter.rs`). A test is skipped when it has a `module`,
  `CanBlockIsFalse`, or `CanBlockIsTrue` flag (or a `resolution`-phase
  negative), is `noStrict`-only, uses a feature listed in
  `tests/test262/unsupported-features.txt` (exact names or `prefix*`), or needs a
  harness include that is not usable (`async` tests implicitly need
  `doneprintHandle.js`). A test flagged both `raw` and `async` is skipped as
  `flag:async`: a raw source leaves no place to prepend `$DONE`.
  `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry` and
  `symbols-as-weakmap-keys` are not listed (phase 4), nor is `BigInt`
  (phase 5b, §6.10); `host-gc-required`
  stays, because a test cannot force a collection without a host hook
  (`$262.gc`), which jsc does not provide.
- **Assembly** (`assemble.rs`). `raw` tests run byte-for-byte; all others are
  `"use strict";` + `sta.js` + `assert.js` + the listed includes + the test
  source, all unmodified. `async` tests additionally get the runner-owned host
  prelude `function print(message) { console.log(message); }` right after the
  directive (test262 `INTERPRETING.md` makes `print` host-defined) and
  `doneprintHandle.js` as the first include (once, even if listed).
- **Outcome** (`outcome.rs`). Each test runs as `jsc FILE` with a timeout
  (default 10 s), stdout and stderr captured to files; exit codes map to pass /
  fail / compile-error / timeout, with negative tests matched on parse versus
  runtime phase and the uncaught error name (builtin errors render as
  `Uncaught TypeError: …`, a failed assertion as `Uncaught Test262Error { … }`).
  An `Uncaught (in promise) …` report (exit 1) is host rejection tracking,
  not an uncaught exception of the script, so it counts as normal completion
  (INTERPRETING.md's pass rule): a non-negative test passes, a runtime
  negative test fails. A non-negative `async` test that completes normally
  passes only if stdout has a line
  exactly `Test262:AsyncTestComplete` and none starting
  `Test262:AsyncTestFailure`; otherwise it fails with that failure line (or
  `missing Test262:AsyncTestComplete`) as the detail in
  `.tmp/test262-results.tsv`. Negative async tests follow the negative rules.
- **Baseline.** `tests/test262/baseline.txt` lists every passing test path.
  `--check` fails if any baseline test no longer passes; `--update-baseline`
  rewrites it to the current passes (with PATH_PREFIX filters, only the entries
  under those prefixes are replaced).
- **History.** `--history` appends `commit pass eligible total` for a full run to
  `tests/test262/history.tsv`; earlier rows include:

  | commit | pass | eligible | total |
  |---|---|---|---|
  | `319208e` | 10185 | 23208 | 47665 |
  | `d0f5e6a` | 10269 | 26388 | 47665 |
  | `15be3e0` | 14301 | 30008 | 47665 |
  | `968dd97` | 19243 | 30014 | 47665 |
  | `eee36ef` | 19378 | 30014 | 47665 |

  The current metric is its last row: **29496 / 33051 eligible tests pass
  (≈ 89.2 %)**; `baseline.txt` holds the same 29496 paths. The drop in
  rate at `d0f5e6a` is the 3180 newly eligible `async` tests (84 pass); at
  `15be3e0` the 3620 tests including `propertyHelper.js` became eligible
  (1413 pass).

**Running it** (from the repository root):

```bash
bash scripts/test262-fetch.sh
cargo build --release -p jsc
cargo run --release -p jsc-test262 -- --check            # regression gate
cargo run --release -p jsc-test262 -- built-ins/Array    # PATH_PREFIX filter
cargo run --release -p jsc-test262 -- --update-baseline --history
```

Options: `--test262 DIR` (default `.tmp/test262`), `--jsc PATH` (default
`target/release/jsc`), `--jobs N` (default: available parallelism),
`--timeout SECS`, `--check`, `--update-baseline`, `--history` (full runs only),
and positional `PATH_PREFIX` filters. Per-test results are written to
`.tmp/test262-results.tsv`. The runner exits `0` on success, `1` when `--check`
finds a regression, and `2` on a setup error.

---

## 11. CI gates

All workflows use Rust toolchain `1.99.1`.

### `ci.yml` — Rust gate

**Triggers.** Manual only (`workflow_dispatch`): run it from the Actions tab
or with `gh workflow run ci.yml --ref <branch>`. Pushes and pull requests do
not run it.

**Job `rust`** (fmt + clippy + doctests + deny + shear + typos):

- `cargo fmt --all --check`
- `typos`
- `cargo clippy --all-targets --workspace` (deny-warnings from the workspace lints)
- `cargo test --doc --workspace`
- `cargo shear`
- `cargo run -p unicode-tables -- --check` (`emit/unicode_data.rs` matches the generator)
- `cargo deny check`

**Job `test`** (matrix of 4): `cargo nextest run --workspace --no-fail-fast
--partition count:N/4`, so the suite runs in four parallel quarters and every
failure is reported (includes the opcount snapshot gate, §6.4; the M1 harness
tests in `crates/jsc/tests/test262_harness.rs` print a notice and pass when the
test262 checkout is absent, as in this job). Dev and test builds use
`opt-level = 1` for the workspace and `opt-level = 3` for dependencies
(`Cargo.toml`), because every test compiles and runs JS programs through swc,
the emitter and wasmtime.

**Job `gc-stress`** (matrix of 4, 60-minute timeout): `JSC_GC_STRESS=127
cargo nextest run -p jsc-core --partition count:N/4`, the jsc-core suite once
more with every program compiled with the `gc_stress` interval 127 (§5.6:
every allocation collects for the first 30 000 collections, then every 127th;
the shared test helpers read the variable; the `gc_*` canaries always use
interval 1; design/runtime-memory.md §7.1 `gc_stress_run_suite`), excluding
the opcount binary and the `memory` binary (heap-cap and million-allocation
tests); collection must not change any output.

**Job `test262`** (baseline regression gate, 90-minute timeout): fetch test262
(cached on the hash of `scripts/test262-fetch.sh`), `cargo build --release -p
jsc`, the M1 harness tests (`cargo test --release -p jsc --test
test262_harness`, after asserting `.tmp/test262/harness/assert.js` exists so
they cannot skip), then `cargo run --release -p jsc-test262 -- --check`.

### `web.yml` — JS/TS gate

**Triggers.** `push` to `main` and `pull_request` on `crates/**`, `packages/**`,
`apps/**`, `scripts/**`, `Cargo.toml`, `Cargo.lock`, `package.json`, `bun.lock`,
`.github/workflows/web.yml`; plus manual `workflow_dispatch`.

**Job `web`** (biome + typecheck + build + test): install Rust with the
`wasm32-unknown-unknown` target, `wasm-pack`, and Bun; `bun install
--frozen-lockfile`; build `@cdlab/jsc` (wasm artifact + TS dist); `bun run lint`
(biome); typecheck `@cdlab/jsc` and `@cdlab/jsc-web`; build `@cdlab/jsc-web`;
test `@cdlab/jsc` (vitest).

### `release.yml` — release binary

**Triggers.** `push` of a `v*` tag, or `workflow_dispatch` with a `version`
input (the workflow then creates and pushes that tag).

**Job `release`.** Installs the `x86_64-unknown-linux-musl` target and
`musl-tools`, runs `cargo build --profile dist --target
x86_64-unknown-linux-musl --locked -p jsc`, packages the binary as
`jsc-linux-x64.tar.gz`, and publishes it with `gh release create
--generate-notes`.

---

## 12. Building a static release

```bash
# One-time: add the musl target.
rustup target add x86_64-unknown-linux-musl

# Build the static, stripped, LTO-optimized binary.
cargo build --profile dist --target x86_64-unknown-linux-musl -p jsc

# Result:
target/x86_64-unknown-linux-musl/dist/jsc
```

`x86_64-unknown-linux-musl` links statically against musl libc, so the binary has
no dynamic-loader or glibc-version dependency. The host may need `musl-tools`
(`musl-gcc`) for transitive C shims; `release.yml` installs it.

`[profile.dist]` (root `Cargo.toml`):

| setting | value | rationale |
|---|---|---|
| `inherits` | `"release"` | start from the release profile (`lto = "fat"`, `codegen-units = 1`) |
| `opt-level` | `3` | jsc embeds wasmtime; compiler throughput matters more than the last kilobytes |
| `lto` | `"fat"` | whole-program link-time optimization |
| `codegen-units` | `1` | maximal cross-unit optimization |
| `strip` | `true` | drop symbols and debug info |
| `panic` | `"abort"` | no unwinding tables; runtime paths never panic |

---
## 13. Current limitations

Each item below is current behaviour of this tree.

**Input & semantics model**

- TypeScript is type-stripped, never type-checked; `.tsx`/JSX is rejected.
- A single script is compiled; there are no ES modules (`import`/`export`) and
  no `import()`.
- Strict-mode semantics only: sloppy-mode constructs (`with`, implicit globals)
  are rejected or throw, and the test262 runner skips `noStrict` tests.
- TDZ is enforced only for module cells and parameters; reading a
  block-scoped binding before its declaration inside the same function
  resolves to the enclosing binding (or the global object) instead of
  throwing.
- A top-level function declaration is not assignable (`f = ..` is a compile
  error) in a program whose top-level declarations stay `main` bindings
  (§6.4).
- An assignment to an unresolvable name evaluates its right-hand side before
  the `HasProperty` check throws the `ReferenceError`.
- `-p` completion values come from expression statements and `try`
  statements; `switch` and loops contribute none.
- Runtime shaking (§4.2) gives up on any member access whose key is a
  variable or another non-literal, non-arithmetic expression, `a[i]` in a
  loop included, and on any use of `JSON.stringify`, `Object.create`,
  `Object.defineProperty` / `defineProperties` or descriptor reflection:
  such a module carries the whole runtime. The Unicode tables
  and the realm image are in every module.
- The call-stack guard (§6.6) counts user-function calls only. Recursion
  inside the runtime that depends on data depth (`JSON.parse` /
  `JSON.stringify` nesting, the console renderer, a chain of proxies whose
  levels forward without a trap: each level is a nested helper call) is
  unguarded, and a host
  with a smaller stack than the `jsc` runner (`node:wasi`, a browser) can
  exhaust it before 10 000 calls: both end in a host trap, not a
  `RangeError`.

**Values & operators**

- Strings are Latin-1 / UTF-16 code units (§5.2): `.length`, indexing,
  comparison and every guest `String.prototype` method are exact for any
  text, lone surrogates included. `localeCompare` is code-unit order, not
  locale collation, and the `toLocale*Case` methods use no locale data;
  there are no ropes, so `s = s + x` in a loop is `O(n^2)`. `search` / `match` / `matchAll` interpret a string
  pattern as RegExp source: a pattern with RegExp syntax throws a
  `TypeError`, and `matchAll` (a RegExp string iterator) always throws one.
- Symbol-keyed properties work on every object, and `@@iterator`,
  `@@toPrimitive`, `@@toStringTag`, `@@hasInstance`, `@@species`
  (`ArraySpeciesCreate`, and `SpeciesConstructor` in
  `Promise.prototype.then` / `finally`), `@@isConcatSpreadable` and an
  object argument's `@@match` / `@@matchAll` / `@@replace` / `@@search` /
  `@@split` to a String method are honoured, and
  `Array.prototype[@@unscopables]` exists (`with` is rejected, so nothing
  reads it).
- `Symbol.iterator` and the other `Symbol.*` statics are not folded at
  compile time: a sealed `Symbol` compiles to the intrinsic's address, and the
  member is an ordinary `__get` on it.
- Not available: `RegExp` (literals and constructor),
  the BigInt typed arrays, `Intl`, `eval`.
  The `Function` constructor throws `TypeError`.
- A BigInt holds at most `2^24` bits (`RangeError: Maximum BigInt size
  exceeded`, §6.10.1; V8 allows `2^30`), and `BigInt.prototype.toLocaleString` ignores its locales
  and options (no `Intl`): the 6 locale tests of
  `intl402/BigInt/prototype/toLocaleString` fail (§6.10).
- The builtin library is exactly §7.9.
- Binary data has no `SharedArrayBuffer`, `Atomics` or immutable buffers
  (`transferToImmutable`, `sliceToImmutable`), and no host detach outside
  the test262 runner's `$262.detachArrayBuffer` (a program detaches an
  `ArrayBuffer` only with `transfer` / `transferToFixedLength`); once the
  rest of the §5.8 target lands, still no `BigInt64Array` /
  `BigUint64Array` / `DataView` BigInt methods before stage T5b; §5.8.9.

**Object model**

- Property tables are linear-scanned; there are no inline caches.
- Array-like lengths are clamped to `2^32 - 1` by the generic loops of `map`,
  `filter`, `forEach`, `reduce`, `join`, `concat` and `flat` (`map` sizes its
  `ArraySpeciesCreate` by the full length, so past `2^32 - 1` it throws the
  `RangeError` first); every other `Array.prototype` method uses the full
  `ToLength` range.
- `String.prototype` and `Number.prototype` are ordinary objects, not a
  `String` / `Number` exotic object: `String.prototype.valueOf()` throws
  `TypeError`, and `Object.prototype.toString.call(Number.prototype)` is
  `"[object Object]"`.
- The remaining test262 failures of the phase-3 built-in families (347 of
  built-ins/{Array 50, String 158, Function 133, Math 1, Number 4,
  AggregateError 1}; NativeErrors has none; counted at `61f46be` from the CI
  per-test results, after phase 5a removed the `void`, tagged-template and
  `for-in` head causes; a few tests those causes hid now stop at `RegExp` or
  the `Function` constructor; minus the 14 `Array.fromAsync` tests the spec
  thenable assimilation fixes), by cause and owner:

  | cause | owner | Array | String | Function | Math | Number | AggregateError |
  |---|---|---|---|---|---|---|---|
  | `RegExp` | phase 5b | 28 | 131 | 3 | | | |
  | `Function` constructor | unsupported (§14 open decisions) | | 4 | 124 | | | 1 |
  | `eval` / `Proxy` / `ArrayBuffer` | `eval` unsupported; the rest phase 5b | 21 | 19 | 4 | 1 | | |
  | `String.prototype` / `Number.prototype` are not exotic (above) | object model | | 3 | | | 4 | |
  | `Array.fromAsync` over an array-like of length `2^53 - 1` (timeout) | — | 1 | | | | | |
  | strict `caller` access (`caller/prop-desc`) | — | | | 1 | | | |
  | CR line terminators hide the frontmatter `includes` (`nativeFunctionMatcher.js`) | runner | | | 1 | | | |
  | `localeCompare` canonical equivalence (code-unit order, above) | — | | 1 | | | | |
- An array in dictionary mode (§5.4) keeps every element in its linear-scan
  property table, so element access on a large frozen or attributed array is
  `O(n)`; the renderer prints an accessor element as `undefined` and omit a
  non-enumerable one.
- `Object.defineProperties` (and `Object.create`'s `Properties`) reads the
  enumerable own keys of its `Properties` argument once, before reading any
  descriptor.
- `.stack` on errors is `"Name: message"` with no frames.

**Functions & classes**

- An `arguments` object has no own `callee` (the strict-mode
  `%ThrowTypeError%` accessor is absent): `arguments.callee` reads
  `undefined`.
- `super` outside a class body (an object-literal method) is a compile
  error.

**Generators & async**

- A `yield`/`await` that would carry a per-node compiler temporary across the
  suspend is a compile error (`super.m(yield x)`, `o[yield k]?.(yield v)`).
- `yield*` has no fast path for a clean generator delegate: each delegated
  step is a call to the cached `next` (or the `throw` / `return` method).
- An async generator object renders as `Object [Generator]`; functions
  render as `[Function: f]` whatever their kind.

**Date**

- The local time zone is UTC (`LocalTZA = 0`): the calendar-field
  constructor `new Date(y, m, ..)` reads its fields as UTC, each local
  getter/setter equals its `UTC` twin and `getTimezoneOffset()` is `0`.
- `Date.parse` accepts only the Date Time String Format and the `toString` /
  `toUTCString` outputs; any other text (RFC 2822 variants, `toDateString`
  output, month names in other positions) is `NaN`.
- No `Intl`: `toLocaleString` / `toLocaleDateString` / `toLocaleTimeString`
  ignore their arguments and return the `toString` / `toDateString` /
  `toTimeString` texts.

**Runtime**

- Case-mapping tables are Unicode 17.0.0 (Rust std) and normalization tables
  Unicode 18.0.0 (`icu_normalizer` 2.4.0), so behaviour can differ only on code
  points new in 18.0.
- An `Error` whose `name` or `message` is a non-string object closing a cycle
  gets no `<ref *N>` marker.
- The renderer's `Error` `name` / `message` lookup materializes a lazy
  function on the prototype chain.
- An out-of-memory in an allocation that hands back a block from the reserve
  (§5.6) is thrown at the next exception check, so straight-line code
  without a check (arithmetic, local writes, a store into a fresh literal)
  may run in between; the output (`console.log`) never does.
- Map / Set backing arrays never shrink: `delete` and `clear` leave
  tombstones (§5.2), so set / delete churn grows the backing without bound
  and iteration walks the tombstones (the collector does not compact them:
  a running `forEach` or iterator holds a slot index).
- Collections run only when an allocation exhausts the budget (§5.6); there
  is no host hook to force one, so when a `WeakRef` empties or a
  `FinalizationRegistry` cleanup runs depends on allocation volume, and a
  program that stops allocating may never see either. Cleanup jobs run only
  at a `__drain()` checkpoint (§6.7), one per checkpoint. A
  `WeakMap` / `WeakSet` lookup is a linear scan of its store.

---

## 14. Roadmap

Phases run in dependency order; each later phase builds on the earlier ones.
Conformance is measured on the eligible test262 subset (section 10).

| Phase | Theme | Depends on | Milestone |
|---|---|---|---|
| 2 | Object model | — | M1: the official test262 `sta.js` + `assert.js` compile unchanged; M2: 30% of eligible tests pass |
| 3 | Built-in library + string semantics | 2 | M3: about 50% |
| 4 | Runtime independence + memory management | 2, 3 | M4: modules import only WASI and run on wasmtime, Node, and browsers; GC |
| 5 | Language completeness | 2, 4 | M5: about 70% |
| 6 | Performance | 4, 5 | M6: benchmark suite and regression gate |
| 7 | Productization | throughout | 1.0 |

### Phase 2 — Object model

**Status: complete** (M1 and M2 reached; 43.9 % of eligible tests pass). 2a
fixed the items in the first bullet below; 2b designed the object model; 2c
implemented it (property tables, arrays, functions and classes as objects, the
realm and global object, builtins on prototypes behind guarded fast paths) and
moved the test262 runner onto the official harness. The planned JavaScript
prelude was not needed: 2c delivered the built-ins as in-wasm functions
installed on the realm's prototypes, and phase 3 extends that set.

- Fixes that do not wait for the model: property writes on primitive receivers
  (pointer guard), `"abc".foo`, `1 + null`, `-0` from unary minus, unhandled
  rejections exiting 0, the `Math.pow` hang, `yield*` over arrays, closures
  inside generator/async bodies, hoisting of nested function declarations,
  destructuring into member targets.
- Every heap object, functions and arrays included, gets a growable property
  table; lookups walk the prototype chain; statically known shapes keep their
  current fast paths as optimizations over the generic path.
- Built-in prototypes and constructors become real objects; plain functions are
  constructible and can carry properties.
- Arrays get a capacity/length layout: amortized `push`, growth by index,
  `push` on captured arrays.
- The GC design is decided here; implementation lands in phase 4.

### Phase 3 — Built-in library and strings

**Status: complete** (M3 reached: 72.0 % of eligible tests pass, §10).
Wave 3a delivered property descriptors and `Object` reflection, `bind`, the
ES2025 `Array` / `String` / `Number` / `Math` methods, `AggregateError`, Date
TimeClip, the async test262 runner, and language completions pulled forward
from phase 5 (comma, generator and async expressions / methods, async
generators, non-simple parameters, early errors, `var` hoisting and global
bindings). Wave 3c delivered `Symbol` (registry, wrappers, the 13 well-known
symbols and their hooks), symbol-keyed properties and computed class member
names (pulled forward from phase 5); the generic iteration protocol
(iterator records, `IteratorClose` on abrupt exits, lazy array
destructuring, `yield*` and generator `return` / `throw` routing, Map/Set
tombstones for live iterators); async iteration (`for await`, `yield*` in
async generators, `%AsyncFromSyncIteratorPrototype%`, `Array.fromAsync`);
the `Iterator` global with `Iterator.from` and the lazy and eager helpers;
the `Promise` combinators (`all`, `allSettled`, `any`, `race`) with
`this`-aware `Promise.resolve` / `reject`, and `Math.sumPrecise`; and
Latin-1 / UTF-16 strings (code-unit `.length`, indexing and comparison,
lossless literals, the `String.prototype` methods in the guest, the
code-point string iterator, full case mapping, `normalize`).

### Phase 4 — Runtime independence and memory

**Status: complete** (stages R1-R4 and M1-M3, `design/runtime-memory.md`
§7). Every module is a self-contained WASI preview-1 command that imports
exactly `fd_write`, `clock_time_get`, `random_get` and `proc_exit`, exports
exactly `_start` and `memory`, and runs under the `jsc` CLI's wasmtime
runner, `wasmtime run`, `@cdlab/jsc` and `node:wasi`; the Rust host semantics
and the TypeScript shim are deleted. Memory is managed by the guest
collector (§5.6, §5.7) with catchable out-of-memory, and the weak
collections, `WeakRef` and `FinalizationRegistry` are built on its weak
steps.

- Modules import only WASI (output and clock); promises and the microtask queue
  run in the guest; the Rust host semantics and the TypeScript shim are removed.
- A precise collector (mark-sweep, shadow-stack roots); catchable out-of-memory.
- `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry`.
- Browsers run modules through a small WASI polyfill.

### Phase 5 — Language completeness

- Private fields and methods, static blocks, object spread and rest,
  destructuring assignment anywhere, tagged templates, labeled statements,
  `super` property assignment (phase 5a, done).
- All remaining early errors.
- ES modules: `import`/`export` across files, bundled at compile time into one
  module.
- `RegExp`, typed arrays, `ArrayBuffer`, `DataView`; `BigInt` (phase 5b,
  done: §6.10).
- Phase 5b binary data (`ArrayBuffer`, typed arrays, `DataView`): designed
  in §5.8 (type tags `24`..`26`; `20` is `TYPE_PROXY`, `21` is
  `TYPE_BIGINT`, `22` / `23` spare), implemented in stages T2, T3a, T4, T3b, T5a, T5b, T6
  (§5.8.10).
- `Proxy` and `Reflect` (phase 5b, done; §6.11, stages P2a-P4).

### Phase 6 — Performance

- Specialization from TypeScript types and static inference: unboxed numeric
  locals, inline caches for property access, escape analysis.
- `wasm-opt` on emitted modules; size and startup budgets.
- A benchmark suite (Richards, DeltaBlue, and similar) compared against porffor
  and QuickJS-on-wasm, with a CI regression gate.

### Phase 7 — Productization

- Published npm package and Rust crates with a stable embedding API;
  multi-platform release binaries.
- A deployed playground, documentation site, and a test262 trend chart from
  `tests/test262/history.tsv`.
- Source maps, `.stack` with real frames, watch mode.
- Optional WASI preview 2 / component output.

### Open decisions

- `eval` and `new Function`: unsupported (recommended), or an embedded
  interpreter.
- `Intl`: unsupported, or an optional module.
- Sloppy mode: strict mode only, permanently or not.
- `RegExp` engine: written for the guest, or an existing engine compiled in
  (decide before phase 5).
  Proposed design: `design/regexp.md` (a guest engine, awaiting approval).
