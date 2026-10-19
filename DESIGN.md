# jsc — Design

> An ahead-of-time **JavaScript → WebAssembly** compiler written in Rust, with
> conformance tracked by test262. jsc parses source with swc, lowers it to an
> in-house IR, emits a WebAssembly module with `wasm-encoder`, and runs that
> module on wasmtime (natively) or in a JS shim (in the browser).

This is the authoritative technical spec. It describes the compiler as it is
today: the pipeline, the value and memory model, the module ABI, the runtime and
host imports, the supported language surface, diagnostics, tooling, and the
known limitations.

## Contents

- [1. Overview](#1-overview)
- [2. Pipeline](#2-pipeline)
- [3. Crate & package layout](#3-crate--package-layout)
- [4. Value model & module ABI](#4-value-model--module-abi)
- [5. Heap & allocator](#5-heap--allocator)
- [6. Runtime & host imports](#6-runtime--host-imports)
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
wasm module whose exported `main` function runs the top-level code.

- **Output.** A wasm module (`jsc build FILE -o OUT.wasm`), or an immediate run of
  that module on an embedded wasmtime (`jsc FILE`, `jsc -e`, `jsc -p`).
- **Embeddable core.** `jsc-core` is a library: `frontend::parse` →
  `codegen::lower` → `emit::emit` produce module bytes; the optional `run` stage
  (feature `native-run`) executes them on wasmtime.
- **In the browser.** `crates/jsc-wasm` compiles `jsc-core` (without the `run`
  stage) to wasm via `wasm-bindgen`; the `@cdlab/jsc` package (`packages/jsc`)
  wraps it with a TypeScript host shim, and `apps/jsc-web` is a browser
  playground on top of that package.
- **Input language.** JavaScript, or TypeScript reduced to JavaScript by type
  stripping (no type checking). `frontend::parse(src, dialect)` takes a
  `Dialect`: the CLI selects `Dialect::Ts` for `.ts` / `.mts` / `.cts` files,
  rejects `.tsx`, and uses `Dialect::Js` for every other file and for `-e` /
  `-p`; `jsc-wasm` and the test262 harness always use `Dialect::Js`.
- **Semantics target.** Full JavaScript semantics is the direction; the compiled
  subset follows strict-mode rules (§7, §13). Conformance is measured by the
  test262 runner (§10).

Three structural decisions shape the implementation:

- **The IR is the seam.** A Rust `enum IrNode` / `IrStmt` (`crates/jsc-core/src/ir.rs`)
  sits between the swc AST and the wasm backend. swc AST shapes never reach
  `emit`; wasm concerns never reach `codegen`. Language features are added by
  extending the IR and the two walks over it.
- **Emit wasm directly.** The backend builds the module straight from the IR with
  `wasm-encoder` — no C, text, or LLVM intermediate.
- **One value word.** Every JS value is a single NaN-boxed `i64`; all heap data
  lives in one bump-allocated linear memory (§4, §5).

---

## 2. Pipeline

```
JS source ──▶ swc AST ──▶ IR ──────────▶ wasm bytes ──▶ result
            frontend     codegen         emit           run (wasmtime)
            swc_ecma_    IrProgram /     wasm-encoder   or the JS shim
            parser       IrNode/IrStmt                  (@cdlab/jsc)
```

1. **frontend** (`frontend.rs`) — hands the source to `swc_ecma_parser` with
   `Syntax::Es(EsSyntax::default())` (`Dialect::Js`) or
   `Syntax::Typescript(TsSyntax::default())` (`Dialect::Ts`) and returns a
   `swc_ecma_ast::Program`. A fatal parse error, or else the first recoverable
   error swc collected (e.g. a strict-mode early error), becomes a spanned
   `CompileError` (§8). `Dialect::Ts` programs then run through swc's
   `resolver` and TypeScript `strip` pass, so codegen only sees plain
   ECMAScript. `strip` lowers `enum` and `namespace` to `var`-based code that
   codegen rejects (`undeclared variable`, §13).
2. **codegen** (`codegen/`) — walks the AST and builds an `IrProgram`: the `main`
   entry (top-level statements), the lifted user functions, and per-program
   facts (uniform arity `K`, module-cell count). This stage owns scoping,
   capture analysis, desugaring, and every compile-time rejection.
3. **emit** (`emit/`) — walks the IR and produces the module: types, imports,
   function bodies, the runtime helper functions, the funcref table, memory,
   globals, exports, data segments for interned strings.
4. **run** (`run/`, feature `native-run`) — instantiates the module on wasmtime
   with a `Linker` that supplies the six host imports (§6), calls `main`,
   drains the microtask queue, checks for an uncaught exception, and decodes the
   result jsval against a snapshot of linear memory.

`run::compile_and_run` and `run::compile_and_run_capturing` chain all four
stages; the CLI and `jsc-wasm` call the stages individually.

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
│   │   │   ├── error.rs           # CompileError (message + optional span) + caret renderer
│   │   │   ├── frontend.rs        # swc parse wrapper
│   │   │   ├── ir.rs              # IrProgram / IrFunction / IrNode / IrStmt
│   │   │   ├── codegen/           # AST -> IR
│   │   │   │   ├── mod.rs             # driver, shared lowering state, module cells
│   │   │   │   ├── expr.rs  stmt.rs   # expression / statement lowering
│   │   │   │   ├── op.rs              # operator + Math mapping
│   │   │   │   ├── function.rs        # function / arrow / method lifting
│   │   │   │   ├── capture.rs         # capture analysis, object-shape facts
│   │   │   │   ├── arity.rs           # uniform-ABI K scan
│   │   │   │   ├── scope.rs           # scope stack, binding resolution, TDZ guard
│   │   │   │   └── generator.rs       # generator / async state-machine CFG
│   │   │   ├── emit/              # IR -> wasm
│   │   │   │   ├── mod.rs             # module builder, globals, Helpers index table
│   │   │   │   ├── sections.rs        # type / import / function index layout
│   │   │   │   ├── expr.rs  stmt.rs   # node / statement emission
│   │   │   │   ├── numeric.rs         # arithmetic, comparisons, Math, conversions
│   │   │   │   ├── value.rs           # jsval helpers (__box_f64, __to_number, type tests)
│   │   │   │   ├── array.rs  object.rs function.rs generator.rs
│   │   │   │   ├── json_parse.rs      # in-wasm JSON.parse
│   │   │   │   └── runtime.rs         # __alloc, trampolines, Map/Set, equality kernels
│   │   │   └── run/               # wasmtime host
│   │   │       ├── harness.rs         # Linker, host imports, main call, OOM + uncaught checks
│   │   │       ├── host.rs            # __method dispatch, host heap allocation
│   │   │       ├── promise.rs         # promise engine + microtask queue + async driver
│   │   │       └── value.rs           # jsval decode + JS-style rendering
│   │   └── tests/             # integration tests by theme (arrays, equality, memory,
│   │                          #   nan_canonical, promises, to_number, unsupported, ...)
│   ├── jsc/                   # binary: clap CLI (src/main.rs) + CLI integration tests
│   ├── jsc-test262/           # test262 conformance runner (§10)
│   └── jsc-wasm/              # wasm-bindgen boundary: `compile(src) -> Uint8Array`
├── packages/jsc/              # @cdlab/jsc: compile() + run() in JS, host shim
│   └── src/  index.ts  shim.ts  abi.ts  value.ts
├── apps/jsc-web/              # browser playground (Vite + React + CodeMirror)
├── scripts/                   # build-wasm.sh, ensure-wasm.sh, test262-fetch.sh
└── tests/test262/             # harness shims, denylist, baseline, history
```

**`jsc-core`.** The library. `#![forbid(unsafe_code)]`. The default feature
`native-run` pulls in `wasmtime` (`cranelift`, `runtime`, `std`); building with
`default-features = false` leaves frontend + codegen + emit only. Dependencies:
`swc_ecma_parser 46`, `swc_ecma_ast 29`, `swc_common 26`,
`swc_ecma_transforms_base 50`, `swc_ecma_transforms_typescript 56`,
`wasm-encoder 0.262`, `anyhow`, optional `wasmtime 49`.

**`jsc`.** A thin `clap` shell (§9). `#![forbid(unsafe_code)]`.

**`jsc-wasm`.** `crate-type = ["cdylib", "rlib"]`, depends on `jsc-core` with
`default-features = false`. Exposes one `#[wasm_bindgen]` function,
`compile(src) -> Result<Vec<u8>, JsError>`; a spanned error becomes the caret
block labelled `<source>`. Its lint table mirrors the workspace's but relaxes
`unsafe_code` to `deny` because the `wasm-bindgen` glue expands to generated
`unsafe` shims. `wasm-pack` builds it with `wasm-opt -Oz`.

**`@cdlab/jsc`** (`packages/jsc`). `compile(src)` returns module bytes from the
wasm-bindgen artifact; `run(src)` compiles, instantiates the module against a
TypeScript host shim (`shim.ts`, which implements the same six `env` imports and
promise engine as `run/`), calls `main`, and returns `{ result, stdout }`.
`abi.ts` and `value.ts` are ports of `abi.rs` and `run/value.rs`. The wasm
artifact is gitignored; `scripts/ensure-wasm.sh` rebuilds it (via
`scripts/build-wasm.sh` → `wasm-pack build crates/jsc-wasm --target web`) when
missing or stale.

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

**Why `i64`, not `f64`.** The wasm spec allows NaN payloads to be canonicalized
by `f64` arithmetic and at the host boundary, which would destroy tags. `i64`
moves are bit-preserving. Numeric operations reinterpret operands to `f64`
(`f64.reinterpret_i64`), compute with `f64.*`, and rebox the result.

**NaN canonicalization.** The number NaN is *always* exactly `QNAN`. Every site
that turns an `f64` back into a jsval — arithmetic, `Math.*`, a `Date` payload,
the host clock — calls the `__box_f64(x: f64) -> i64` helper
(`x == x ? reinterpret(x) : QNAN`), because wasm may produce a sign-bit or
payload-carrying NaN (`0/0` on x86, or a NaN operand such as `undefined`
propagating its tag bits). Integer-to-float conversions cannot produce NaN and
reinterpret directly. Consequently NaN tests compare bits directly, and a
computed NaN can never alias a boolean, `null`, `undefined`, or pointer.

**`typeof`.** `typeof` yields an interned heap string: the type index
(`number`, `boolean`, `undefined`, `object`, `string`, `function` —
`abi::TYPEOF_STRINGS`) selects the data-segment offset of the matching word, so
`typeof x === "number"`, concatenation, and `.length` all go through ordinary
string machinery. Heap kinds other than string and function report `"object"`.

**Truthiness.** Falsy is exactly `false`, `null`, `undefined`, `±0`, `NaN`, and
the empty string; every other value — including `[]`, `{}`, and every function —
is truthy.

### 4.2 Module layout

**Types.** `main` has type `() -> i64`. Every user function (declarations,
expressions, arrows, methods, constructors, generator state machines) shares
**one** uniform type:

```text
(a0: i64, …, a_{K-1}: i64, env: i64, argc: i64) -> i64
```

`K` is the program-wide maximum positional arity, computed by `codegen/arity.rs`
from every function's parameter count and every call site's argument count
(including callback-invoking built-ins, promise executors, and setters, which
are called with one argument by assignment). `env` is the closure environment
(`undefined` for a non-capturing callee); `argc` is the actual argument count as
a number jsval. A single signature lets `call_indirect` serve every function.

**Function index space.**

| index | function |
|---|---|
| `0`–`5` | host imports `print`, `__convert`, `__method`, `__promise`, `__then`, `__now` |
| `6` | `main` (exported as `"main"`) |
| `7 … 6+n` | the `n` user functions |
| then | runtime helpers, in a fixed order: `__alloc`, `__concat`, `__str_eq`, `__str_cmp`, `__str_index`, `__obj_get`, `__obj_set`, `__array_push`, `__obj_rest`, `__array_concat`, `__obj_keys`, `__obj_values`, `__obj_entries`, `__obj_has`, `__obj_delete`, `__json_escape`, `__json_str`, `__same_value_zero`, `__map_find`, `__map_set`, `__map_get`, `__map_has`, `__map_delete`, `__loose_eq`, `__str_to_array`, the `JSON.parse` family (`__json_ws`, `__json_throw`, `__json_string`, `__json_number`, `__json_value`, `__json_array`, `__json_object`, `__array_to_object`, `__json_parse`), `__gen_to_array`, `__nullish_throw`, `__box_f64`, `__to_number` |
| last | `__invoke1`, `__async_step`, and the two `new Promise` settler functions |

**Table.** When the program has functions, a funcref table of exactly `n + 2`
entries (min = max) is filled by an active element segment with the user
functions and the two settler functions; a function value's `func_idx` is a slot
in this table.

**Memory.** One linear memory, `min = 1` page, `max = 8192` pages (512 MiB,
`abi::MEMORY_MAX_PAGES`), exported as `"memory"`.

**Globals.**

| index | name | type | exported as | role |
|---|---|---|---|---|
| 0 | `$__heap_ptr` | `i32` | `__heap_ptr` | bump-allocator cursor (§5) |
| 1 | `$exc_pending` | `i32` | `__exc_pending` | `1` while an exception propagates |
| 2 | `$exc_value` | `i64` | `__exc_value` | the in-flight thrown jsval |
| 3 | `$cf_kind` | `i32` | — | pending `return`/`break`/`continue` routed through `finally` |
| 4 | `$cf_value` | `i64` | — | routed return value / labeled-loop target |
| 5 | `$__this` | `i64` | — | `this` carrier, initialized to `undefined` |
| 6–8 | `$__json_data`, `$__json_pos`, `$__json_len` | `i32` | — | `JSON.parse` cursor |
| 9+ | module cells | `i64` | — | one cell pointer per module binding (§7.1) |

**Exports.** `main`, `memory`, `__heap_ptr`, `__exc_pending`, `__exc_value`,
`__invoke1(fn: i64, arg: i64) -> i64`, `__async_step(gen: i64, sent: i64) -> i64`.

### 4.3 Calling convention

- **Direct calls.** A call to a statically known, non-capturing top-level
  function emits `call` with its arguments, then `undefined` padding up to `K`
  slots, then `env = undefined`, then `argc`.
- **Indirect calls.** A call through a function *value* evaluates the callee
  once, stashes the block base, pushes the arguments and padding, then reads the
  callee's own `env` (`+HEADER_SIZE+4`) and `func_idx` (`+HEADER_SIZE`) and issues
  `call_indirect`. `env` is last so the callee data is read only after every
  argument (which may itself contain calls) has been evaluated.
- **Arity.** Calls are not arity-checked: missing arguments read as `undefined`,
  extra arguments are ignored, and `argc` carries the real count so a trailing
  rest parameter collects exactly `[arg_p, …, arg_{argc-1}]`. A spread call
  (`f(...xs)`) fills the `K` positional slots from the flattened array; elements
  beyond `K` are dropped (§13).
- **`this`.** `this` rides the `$__this` global rather than a parameter. A method
  call `obj.m(args)` evaluates `obj` and the arguments, then sets
  `$__this = obj` immediately before `call_indirect`; a plain call resets it to
  `undefined`. A function that reads `this` snapshots `$__this` into a local as
  the first statement of its prologue, so nested calls cannot clobber it.
- **Exceptions.** After every call into user code the caller checks
  `$exc_pending` and branches to its nearest handler or returns early (§6.3).

---

## 5. Heap & allocator

### 5.1 Block layouts

Every heap block starts with a 4-byte header: the **type byte** at offset 0 and
three padding bytes; the payload starts at `HEADER_SIZE = 4`. Multi-byte
fields are little-endian.

| type byte | kind | layout | `typeof` |
|---|---|---|---|
| `0` `TYPE_STRING` | string | `[0][pad×3][len:u32][utf8 bytes × len]` | `string` |
| `1` `TYPE_ARRAY` | array | `[1][pad×3][len:u32][elem:i64 × len]` | `object` |
| `2` `TYPE_OBJECT` | object | `[2][pad×3][n:u32][(key:i64, val:i64) × n]` | `object` |
| `3` `TYPE_FUNCTION` | function value | `[3][pad×3][func_idx:u32][env:i64]` | `function` |
| `4` `TYPE_ENV` | closure environment (internal) | `[4][pad×3][count:u32][cell_ptr:i64 × count]` | — |
| `5` `TYPE_CELL` | boxed binding (internal) | `[5][pad×3][value:i64]` | — |
| `6` `TYPE_MAP` | `Map` handle | `[6][pad×3][size:u32][entries:i64]` | `object` |
| `7` `TYPE_SET` | `Set` handle | `[7][pad×3][size:u32][entries:i64]` | `object` |
| `8` `TYPE_PROMISE` | promise | `[8][pad×3][state:i32][value:i64]` | `object` |
| `9` `TYPE_GENERATOR` | generator handle | `[9][pad×3][sm_idx:u32][state:i32][done:i32][env:i64]` | `object` |
| `10` `TYPE_DATE` | `Date` | `[10][pad×3][pad×4][ms:f64]` | `object` |
| `11` `TYPE_ACCESSOR` | accessor pair (internal) | `[11][pad×3][pad×4][getter:i64][setter:i64]` | — |

Notes per kind:

- **Strings** store UTF-8 bytes; `len` and `.length` are byte counts (§13).
  String literals are interned into active data segments starting at offset `8`
  (offset `0` is a null guard), so a literal costs no run-time allocation and
  identical literals (including object keys) share one blob.
- **Arrays** hold full jsvals, so they nest. `arr[i]` for `0 <= i < len` is an
  `i64.load` at `ptr + HEADER_SIZE + 4 + 8*i`; an out-of-range read yields
  `undefined`.
- **Objects** are a count-prefixed entry table; keys are heap-string jsvals
  compared by byte content (`__str_eq`). Insertion order is table order.
  Reserved keys beginning with `@@` — `@@prototype`, `@@constructor`,
  `@@errorkind` — are hidden from enumeration (`for-in`, `Object.keys/values/
  entries`, `JSON.stringify`) by a first-byte `@` check, yet `__obj_get` reads
  them.
- **Map / Set** handles point (`entries`) at a growable array holding
  `(key, value)` pairs flattened into even/odd slots (a Set's value slot is
  `undefined`). Growth reallocates the backing array and rewrites `entries` in
  place, so the handle jsval never moves. Lookup is a linear scan with
  `__same_value_zero`.
- **Promise** blocks mirror settlement (`state` ∈ pending `0` / fulfilled `1` /
  rejected `2`, plus the settled `value`) so a memory snapshot can render them;
  reactions live in the host engine (§6.4).
- **Generator** handles carry the state-machine table index, the resume state,
  the `done` flag, and the frame (a `TYPE_ENV` of cells holding every generator
  local).
- **Accessor** blocks are stored under a property name on a class prototype and
  are always consumed by the member get/set path; they are never observed as
  values.

### 5.2 `__alloc` and memory growth

`__alloc(size: i32) -> i32` is a bump allocator over the `$__heap_ptr` global,
which starts at the first free offset after all static data:

1. `old = __heap_ptr`; `end = u64(old) + u64(size)` (computed in `i64`, so it
   cannot wrap).
2. Fast path: if `end <= memory.size * 64 KiB`, set `__heap_ptr = end` and return
   `old`.
3. Otherwise grow: try `memory.grow(max(need, memory.size))` (at least doubling);
   if that fails, retry with exactly `need` pages.
4. If both fail (the 8192-page / 512 MiB maximum is reached), store the sticky
   sentinel `abi::HEAP_PTR_OOM` (`-1`, i.e. `0xFFFF_FFFF`) in `__heap_ptr` and
   execute `unreachable`.

Host-side allocations (strings and arrays built by `__convert`, `__method`, the
promise engine) bump the same exported `__heap_ptr` global and grow the same
memory, so host-built values are visible to the guest without copying. Every
host import result goes through `oom_guard`, which turns a sentinel heap pointer
into a trap.

**Out of memory.** After `main` (and after the post-`main` microtask drain) the
harness checks the sentinel; if set, the run fails with
`Uncaught RangeError: out of memory` — no wasm backtrace — regardless of the trap
it caused. Because the sentinel makes every later allocation fail, an
out-of-memory condition inside `try` is not catchable; the program terminates.

**No free, no GC.** Nothing is ever freed. Reallocation (`push` growth, Map/Set
growth) copies into a fresh block and abandons the old one. Long-running
allocation-heavy programs consume memory monotonically up to the 512 MiB cap.

---

## 6. Runtime & host imports

### 6.1 Host imports

Every module imports exactly six functions from the `"env"` namespace
(`abi::IMPORT_MODULE`), declared in this order (function indices `0`–`5`):

| # | import | signature | backs |
|---|---|---|---|
| 0 | `print` | `(bits: i64, flag: i32) -> ()` | `console.log` |
| 1 | `__convert` | `(bits: i64, op: i32) -> i64` | `String`, `Number`/ToNumber, `parseInt`, `parseFloat` |
| 2 | `__method` | `(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64` | non-callback array/string methods |
| 3 | `__promise` | `(op: i32, a: i64, b: i64) -> i64` | promise primitives, top-level `await`, `async` spawn |
| 4 | `__then` | `(p: i64, on_f: i64, on_r: i64) -> i64` | `.then` / `.catch` |
| 5 | `__now` | `() -> f64` | `Date.now()`, `new Date()` |

The native host is `run/harness.rs` (wasmtime `Linker::func_wrap`); the browser
host is `packages/jsc/src/shim.ts`. Both decode jsvals with the same rules
(`run/value.rs` / `value.ts`).

- **`print` flags.** `0` = format the value and append a space (every argument
  but the last), `1` = format and append a newline (the last argument),
  `2` = newline only (zero-argument `console.log()`). Values render with the same
  `format_value` used for the CLI's `-p` output.
- **`__convert` ops.** `0` ToString (a string argument is returned unchanged;
  otherwise the formatted text is allocated as a heap string), `1` ToNumber,
  `2` parseInt (base 10), `3` parseFloat.
- **`__method` ops.** `0` `indexOf`, `1` `includes`, `2` `join`, `3` `slice`,
  `4` `toUpperCase`, `5` `toLowerCase`, `6` `charAt`, `7` `trim`, `8` `trimStart`,
  `9` `trimEnd`, `10` `repeat`, `11` `startsWith`, `12` `endsWith`,
  `13` `padStart`, `14` `padEnd`, `15` `substring`, `16` `split`. The host
  dispatches on the receiver's type byte for the overloaded array/string names.
- **`__promise` ops.** `0` resolve, `1` reject, `2` await, `3` finally,
  `4` new pending promise, `5` settle-fulfill, `6` settle-reject,
  `7` async spawn.
- **`__now`.** `SystemTime::now()` since the Unix epoch in milliseconds (`0` if
  the clock is before the epoch). It is the only source of non-determinism.

### 6.2 Guest runtime helpers

Everything else runs in wasm, emitted per module by `emit/`: allocation
(`__alloc`), string concatenation/equality/comparison/indexing, object get/set
with prototype fallback, array push/concat/spread, object rest, reflection
(`Object.keys/values/entries`, `in`, `delete`), `JSON.stringify` and
`JSON.parse`, Map/Set operations, the equality kernels (`__same_value_zero`,
`__loose_eq`), ToNumber (`__to_number`), NaN canonicalization (`__box_f64`), the
nullish-receiver guard (`__nullish_throw`), generator draining
(`__gen_to_array`), and the host re-entry trampolines (`__invoke1`,
`__async_step`).

`__to_number(v)` implements ToPrimitive-then-ToNumber for non-number operands:
an array's primitive is its `join()` string (via `__method`), which is then
parsed; every other value goes to `__convert` op `1` (strings by the
StringToNumber rules, `true`/`false` → `1`/`0`, `null` → `0`, `undefined` and
other heap kinds → `NaN`). Numeric call sites skip the helper when the operand
already is a number.

### 6.3 Exceptions (manual unwinding)

jsc does not use the wasm exception-handling proposal. Two exported globals
carry the in-flight exception: `$exc_pending` and `$exc_value`.

- `throw e` stores `e` in `$exc_value`, sets `$exc_pending = 1`, then branches to
  the innermost enclosing `try` handler label in the same function, or returns a
  dummy value from the function.
- After every call into user code (direct, `call_indirect`, callback-invoking
  helpers), the caller checks `$exc_pending` and propagates the same way. Pure
  built-ins that cannot throw need no check.
- `try { B } catch (e) { H }` wraps `B` in a block whose label is pushed on the
  codegen handler stack; after the block, if `$exc_pending` is set it is cleared
  and `$exc_value` is bound to `e` in a fresh scope for `H`.
- `finally` runs on every exit edge — normal fall-through, throw, and any
  `return`/`break`/`continue` leaving the protected region. An abrupt exit is
  recorded in `$cf_kind`/`$cf_value` before the finalizer runs and resumed after
  it; a finalizer that itself completes abruptly overrides the pending action
  (`try { return 1 } finally { return 2 }` returns `2`). A pending exception is
  re-propagated after a normally completing `finally`.
- At the boundary the harness reads `__exc_pending`; if set, it renders
  `__exc_value` with `format_value` and fails with `Uncaught <value>`.

### 6.4 Promises, microtasks, and the async driver

Promise *settlement* lives in the guest heap (`TYPE_PROMISE`); *scheduling* —
each promise's reaction list and the FIFO microtask queue — lives in the host
(`run/promise.rs`, mirrored by the shim), keyed by the promise block's offset.

- `Promise.resolve` / `Promise.reject` / `p.finally` / top-level `await` lower to
  `__promise`; `p.then(onF, onR)` and `p.catch(onR)` lower to `__then`, which
  returns the chained promise. Missing handlers pass the settlement through.
  Resolving with a promise adopts it. A non-promise receiver or awaited operand
  is treated as already fulfilled (`await 5` → `5`).
- **Re-entry.** Reactions are guest closures; the engine runs them by calling the
  exported `__invoke1(fn, arg)` trampoline, which issues the uniform-ABI
  `call_indirect`. A callback that throws sets `$exc_pending`; the engine reads
  and clears it and rejects the chained promise.
- **`new Promise(executor)`** allocates a pending promise `P` (op `4`), then
  passes the executor two function values whose table slots are the
  hand-emitted settler functions and whose `env` slot holds `P` directly.
  Calling them settles `P` (ops `5`/`6`, settle-once). A throwing executor
  rejects `P`.
- **Draining.** There are no timers or I/O, so the microtask queue is the only
  asynchronous source. Top-level `await` drains the queue until its operand
  settles (a rejection surfaces as an uncaught throw). After `main` returns
  normally the harness drains whatever remains, so unawaited `.then` chains run.
  If `main` ends with a pending exception, the queue is not drained.
- **`async` functions** (§7.8) are spawned with op `7`: the host allocates the
  result promise, drives the body's state machine one step at a time through the
  exported `__async_step(gen, sent)` trampoline, fulfills on completion, rejects
  on a throw, and on each `await` registers a continuation on the awaited value
  that resumes the generator from the microtask queue.
- Unhandled rejections are not reported: a program whose only output is a
  rejected promise exits `0`.

### 6.5 Value rendering

`run/value.rs` decodes a jsval against a memory snapshot into a `Value`
(number, boolean, null, undefined, string, array, object, function, map, set,
promise, date, …) and `format_value` renders it Node-console style: numbers by
the ECMAScript Number::toString rules (shortest round-trip digits, exponential
form at `>= 1e21` and `< 1e-6`, `-0` prints `0`), strings unquoted at top level,
`[ 1, 2, 3 ]`, `{ a: 1, b: 2 }`, `[Function (anonymous)]`, `Map(N)` / `Set(N)`,
`Promise { 42 }` / `Promise { <pending> }` / `Promise { <rejected> r }`, dates as
ISO 8601 UTC (`Invalid Date` for a non-finite payload). An object carrying
`@@errorkind` renders as its single-line stack, `Name: message`.

---

## 7. Language surface

The compiled language is the strict-mode subset described below. Anything not
listed is rejected at compile time with a spanned diagnostic (§8) or noted in
§13.

### 7.1 Bindings & scoping

- **Declarations.** `let`, `const`, `var`, function declarations, class
  declarations. Assigning to an undeclared name, or reading one, is a compile
  error (`undeclared variable: x`).
- **Block scoping.** Name resolution walks a compile-time scope stack: every
  `{}` block, loop body, `for` head, and `catch` clause pushes a frame. Each
  `let`/`const` gets a fresh wasm local; inner declarations shadow outer ones.
- **`const`.** Rebinding a `const` (by `=`, compound assignment, `++`/`--`) is a
  compile-time error; mutating the referenced object/array is allowed.
- **Closures.** Capture analysis marks every binding referenced from an inner
  function. Captured bindings are promoted to heap **cells** (`TYPE_CELL`);
  uncaptured bindings stay plain wasm locals. A capturing function value
  carries a `TYPE_ENV` of cell pointers, so capture is **by reference** — every
  closure and the defining scope share one cell. A captured `for-of`/`for-in`
  loop binding gets a fresh cell per iteration; a C-style `for (let …)` binding
  is one cell shared by all iterations (§13).
- **Function declarations.** A top-level function declaration is hoisted and
  called directly by index with `env = undefined`. A declaration nested inside
  another function is lifted like an arrow and bound to its function value at
  its source position (not hoisted, and not visible inside its own body).
- **Module cells.** A top-level binding that is referenced from inside a
  top-level function is stored in a **module cell**: a cell whose pointer lives
  in a dedicated `i64` global (index `9 + slot`), allocated in `main`'s prologue
  before any user code runs. Top-level functions reach it through the global, so
  they stay hoisted and directly callable from anywhere, including from other
  top-level functions. The cell holds a TDZ hole (an `undefined`-tagged box
  with a nonzero payload) until the declaration executes; reading or writing it
  earlier throws a branded `ReferenceError`
  (`Cannot access 'n' before initialization`).
- **Class names** are bound before the class body is lowered, so constructors and
  methods can `new` their own class.

### 7.2 Operators & coercion

- **Arithmetic** `+ - * / % **` (and the compound forms `+= -= *= /= %= **=`),
  unary `-` / `+`, prefix/postfix `++` / `--`.
  - `- * / % **`, unary `-`/`+`, and `++`/`--` apply **ToNumber** to each operand
    (`"3" * "4"` → `12`, `null * 2` → `0`, `-"3"` → `-3`, `+[]` → `0`; a string
    binding incremented by `++` becomes a number).
  - Binary `+` concatenates when either operand is a string (the other is
    rendered with the console formatter) and otherwise adds the two operands as
    numbers *without* ToNumber on non-number operands (§13).
  - `**` and `Math.pow` share one inline implementation: repeated multiply for
    the integer part of the exponent plus bit-by-bit square roots for the
    fraction; negative exponents reciprocate.
- **Relational** `< > <= >=`: two strings compare lexicographically by bytes;
  otherwise both operands are converted with ToNumber and compared as `f64`.
- **Strict equality** `===` / `!==`: type then value — numbers numerically
  (`NaN !== NaN`, `+0 === -0`), strings by byte content, everything else by
  identity bits.
- **Loose equality** `==` / `!=` (`__loose_eq`): booleans are first normalized
  to numbers; `null` and `undefined` equal each other and nothing else; two
  numbers compare as `f64`; two strings by content; number vs. string converts
  the string with ToNumber; anything else compares identity. Object-to-primitive
  conversion is not modelled (`[] == 0` is `false`).
- **SameValueZero** (`__same_value_zero`) backs Map/Set keys: strings by content,
  numbers by `f64` equality or both NaN (`+0` equals `-0`), everything else by
  identity. `Array.prototype.includes` also treats NaN as equal to NaN.
- **Logical** `!`, `&&`, `||`, `??` (each evaluates its left operand once and
  yields an operand value, not a coerced boolean) and `&&=`, `||=`, `??=` (short
  circuit: the store is skipped when not needed).
- **Conditional** `c ? a : b`, optional chaining `a?.b`, `a?.[k]`, `a?.()`
  (a nullish base short-circuits the rest of the chain to `undefined`).
- **`typeof`**, **`instanceof`** (§7.5), **`in`** (own-key probe), **`delete`**
  (removes an own key by compacting the entry table).
- **Member access on `null`/`undefined`** — reading or writing a property of a
  nullish receiver throws a branded `TypeError`
  (`Cannot read properties of null (reading 'x')`), catchable like any throw.

### 7.3 Control flow

`if`/`else`, `while`, `do … while`, C-style `for`, `for-of` (arrays, strings,
generators), `for-in` (own enumerable keys of objects; index strings of arrays
and strings), `switch` (strict `===` matching, fall-through, `default`),
`break` / `continue` (with labels on loops), `return`, `throw`,
`try`/`catch`/`finally` (with or without a catch binding).

A `for` loop with a `let` head is scoped to the loop; `continue` still runs the
update clause. Labels may only be applied to loops.

The CLI's `-p` mode prints the value of the **last expression statement**
evaluated by `main`; statement completion values of `try`, `switch`, and loops
are not tracked.

### 7.4 Functions

- **Forms.** Function declarations, function expressions, arrows (expression- or
  block-bodied), object-literal methods and shorthand methods, class
  constructors/methods/accessors. All are lifted to top-level wasm functions
  with the uniform ABI (§4.2); a function value is a `TYPE_FUNCTION` block.
- **Parameters.** Default values (evaluated per call in a prologue, may reference
  earlier parameters), trailing rest parameters (collected into a real array
  using `argc`), and array/object destructuring patterns (composable with
  defaults).
- **Spread.** In calls and array literals; sources may be arrays, strings
  (exploded per byte), or generators (drained via `__gen_to_array`).
- **`this`.** Bound by method calls (`obj.m()`), by `new`, and in static class
  methods called as `C.m()`. A plain call sees `this === undefined`. Top-level
  `this` is rejected.

### 7.5 Objects & arrays

- **Object literals** with identifier and string keys, shorthand properties,
  methods, and spread-free construction. Property reads (`o.k`, `o["k"]`,
  `o[expr]`) resolve own keys first, then walk `@@prototype` links. A missing key
  reads `undefined`.
- **Fixed shape.** An object's key set is decided when it is allocated (an object
  literal's keys, or a class's field scan). Assignment updates existing keys;
  assigning a key the object does not have throws a `TypeError` (§13). `delete`
  removes keys.
- **Destructuring.** Array patterns (holes, defaults, rest, nesting) and object
  patterns (rename, defaults, nesting, rest via `__obj_rest`) in declarations
  and parameters.
- **Arrays.** Literals, indexing, in-range element assignment, `.length`, and
  the methods in §7.9. `push` grows the array by reallocation and writes the new
  pointer back to the receiver's binding.
- **Computed dispatch.** `x[i]` and `x[i] = v` dispatch at run time on the
  receiver's type byte: string → one-character string, array → element,
  object/Map/etc. → keyed lookup with the key converted to a string.

### 7.6 Classes

- **Declaration.** `class C { … }` binds `C` to a heap object holding hidden
  `@@constructor` (an ordinary uniform-ABI function value; a class without a
  constructor gets a synthesized one) and `@@prototype` (one shared prototype
  object per class).
- **Instances.** `new C(args)` allocates an instance pre-slotted with the
  `@@prototype` link and every field the class body and its constructor chain
  assign (a compile-time field-shape scan), sets `this`, runs the constructor,
  and yields the instance. Field initializers (`f = init`, or `f;` for
  `undefined`) run in source order before the constructor body.
- **Methods** live once on the shared prototype and are found through the
  prototype fallback in `__obj_get`.
- **Accessors.** Instance `get x()` / `set x(v)` lower to one `TYPE_ACCESSOR`
  block on the prototype. A read resolving to it calls the getter with
  `this` = receiver; a write resolves the key through the prototype chain and
  calls the setter. A set-only property reads `undefined`; writing a get-only
  property is ignored. The accessor paths are emitted only when the program
  defines an accessor.
- **Static members.** `static m(){}` and `static f = init` live on the class
  object. A literal initializer is placed inline; any other initializer runs
  after the class binding exists, in source order, with `this` = the class.
- **Inheritance.** `class B extends A` (a named, earlier-declared superclass)
  links `B`'s prototype to `A`'s, so inherited and overridden methods resolve
  through the chain. `super(args)` calls `A`'s constructor with the current
  `this`; `super.m(args)` calls the parent prototype's method. A subclass
  without a constructor forwards its arguments to the parent constructor.
- **`instanceof`.** The right-hand side must be an identifier:
  - a user class → walk the receiver's `@@prototype` chain comparing against the
    class prototype by identity (multi-level);
  - unshadowed `Error` / `TypeError` / `RangeError` → check the `@@errorkind`
    brand (`Error` matches any brand);
  - unshadowed `Array` / `Function` / `Date` → the receiver's type byte;
    `Object` → any heap pointer that is not a string.
  Non-object operands yield `false` without trapping. Any other right-hand side
  is a compile error.

### 7.7 Errors

- `new Error(msg)`, `new TypeError(msg)`, `new RangeError(msg)` build ordinary
  objects with `name`, `message`, `stack` (`"Name: message"`, or `"Name"` with no
  argument — single line, no frames), and the hidden `@@errorkind` brand
  (`0` Error, `1` TypeError, `2` RangeError). The message expression is
  evaluated once.
- `throw` accepts any value.
- **Engine-raised errors** are real branded objects, catchable with
  `try`/`catch` and recognised by `instanceof`:
  - `TypeError: called value is not a function` — a method call whose callee is
    not a function (an unknown method on a receiver whose shape is not known at
    compile time, a non-callable `super.m`, a non-generator `.next()` receiver);
  - `TypeError: Cannot read properties of null|undefined (reading 'k')` —
    member access on a nullish receiver;
  - `ReferenceError: Cannot access 'x' before initialization` — a module cell
    read in its TDZ (branded with the base `Error` kind, `.name`
    `ReferenceError`);
  - a `JSON.parse` syntax error — an `Error`-branded object with `.name`
    `SyntaxError`.
- Out of memory is reported as `Uncaught RangeError: out of memory` and is not
  catchable (§5.2).

### 7.8 Iteration, generators & async

**Generators.** A top-level `function*` declaration compiles to two
uniform-ABI functions: a **constructor** that allocates a `TYPE_GENERATOR`
handle and its frame without running the body, and a **state machine**
`SM(sent, gen, …)` that resumes at `gen.state`. Every generator local lives in a
frame cell, so locals survive suspension. The body is lowered to a control-flow
graph of basic blocks (`codegen/generator.rs`); `if`/`while`/`for`, `break`,
`continue`, and `return` crossing a `yield` become block jumps dispatched by a
`loop { br_table(state) }`, while regions without a suspend keep ordinary wasm
control flow.

- `yield e;`, `x = yield e`, `return yield e`; `it.next(v)` sends `v` and
  returns a fresh `{ value, done }` object. `.next` is claimed for every
  receiver: a non-generator receiver throws
  `TypeError: called value is not a function`.
- `yield* g` delegates to another generator.
- `for-of` over a generator, spread of a generator (`[...g()]`, `f(...g())`):
  a run-time generator test selects the iterator path; arrays and strings keep
  the indexed path.

**`async`/`await`.** A top-level `async function` declaration is a generator whose
`await e` is a `yield e`; the declared name binds to a wrapper that builds the
generator and hands it to the host async driver (§6.4), returning the result
promise. All generator control-flow support applies to `await`. A rejection of
an awaited promise rejects the function's result promise. Top-level `await` is
supported in `main`.

**Promises.** `new Promise(executor)`, `Promise.resolve`, `Promise.reject`,
`.then`, `.catch`, `.finally` (§6.4). The `.then`/`.catch`/`.finally` names are
claimed for every receiver.

### 7.9 Built-ins

Built-ins are recognized syntactically at the call or member site when the name
is not shadowed by a user binding; they are not first-class values.

| area | supported |
|---|---|
| output | `console.log(...)` |
| `Math` | `floor`, `ceil`, `round` (half-up), `trunc`, `abs`, `sqrt`, `min`, `max` (variadic, spread-aware), `pow`; constants `PI`, `E` |
| conversions | `String(x)`, `Number(x)`, `Boolean(x)`, `parseInt(s)` (base 10), `parseFloat(s)` |
| globals | `NaN`, `Infinity`, `undefined` |
| array methods | `push(v)`, `pop()`, `map`, `filter`, `forEach` (callbacks receive `(element, index)`), `reduce(cb, init)` (init required; callback receives `(acc, element, index)`), `indexOf`, `includes`, `join`, `slice` |
| string methods | `toUpperCase`, `toLowerCase`, `indexOf`, `includes`, `slice`, `substring`, `charAt`, `trim`, `trimStart`, `trimEnd`, `repeat`, `startsWith`, `endsWith`, `padStart`, `padEnd`, `split`; `.length`, indexing |
| `Object` | `Object.keys`, `Object.values`, `Object.entries` |
| `JSON` | `JSON.stringify(v)` (in-wasm; `undefined`/functions become `null` in arrays and are omitted in objects), `JSON.parse(s)` (in-wasm recursive descent; argument coerced with ToString; objects built with runtime keys) |
| `Map` / `Set` | `new Map()`, `new Set()`; `set`/`get`/`has`/`delete`/`.size`; `add`/`has`/`delete`/`.size` |
| `Promise` | see §7.8 |
| `Date` | `new Date()`, `new Date(ms)` (argument via ToNumber), `Date.now()`, `getTime`, `valueOf`, `getFullYear`, `getMonth`, `getDate`, `getDay`, `getHours`, `getMinutes`, `getSeconds`, `toISOString` — all UTC, computed in wasm |
| errors | `Error`, `TypeError`, `RangeError` constructors |

Map/Set and Date method names dispatch on the receiver's type byte at run time
and fall back to an ordinary user method call for other receivers, so the names
stay usable on user objects. Built-in method arity is checked at compile time.

**Method resolution.** For `recv.name(args)` the lowering tries, in order:

1. a recognized built-in for the receiver (above);
2. a user method call (`MethodCallUser`: member read with prototype fallback,
   then `call_indirect` with `this = recv`);
3. compile-time rejection when absence is provable — the receiver is a
   primitive/array literal, the program has no functions, or the receiver is an
   object literal / never-reassigned binding initialized with an all-static-key
   object literal whose key set lacks `name`
   (`method '<name>' is not supported`);
4. otherwise a run-time check that the resolved callee is a function before the
   `call_indirect`, throwing `TypeError: called value is not a function`.

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
underlines its full width. A span-less error prints its bare message. Because
`CompileError` implements `std::error::Error`, it travels through `anyhow` and
is recovered with `downcast_ref`.

Representative messages: `undeclared variable: x`, `call to undeclared
function: f`, `cannot assign to constant binding: c`, `method 'sort' is not
supported`, `regular expression literals are not supported`, `BigInt literals
are not supported`, `with statements are not supported`, `class expressions are
not supported`, `unsupported object key: …`, `a label may only be applied to a
loop`, `a \`yield\` inside try/catch/finally is not supported`.

**Run-time errors** carry no source span:

- An uncaught JS exception fails the run with `Uncaught <rendered value>`
  (e.g. `Uncaught TypeError: Cannot read properties of null (reading 'x')`,
  `Uncaught 1`).
- Heap exhaustion fails with `Uncaught RangeError: out of memory` (§5.2).
- Remaining machine faults surface as wasmtime traps.

---

## 9. CLI

`crates/jsc/src/main.rs`, node-like:

| command | behaviour |
|---|---|
| `jsc FILE` | compile and run the file; only `console.log` output is printed |
| `jsc -e SRC` / `--eval SRC` | compile and run inline source; only `console.log` output is printed |
| `jsc -p SRC` / `--print SRC` | compile and run inline source, then print the completion value (last expression statement) |
| `jsc build FILE -o OUT` | write the wasm module to `OUT`; `-o -` writes to stdout |

Exactly one of `-e`, `-p`, or `FILE` is required (a clap argument group);
`-e`/`-p`/`-o` accept values beginning with `-` (`jsc -p "-0"`). `console.log`
streams to stdout while the module runs.

**Exit codes.**

| code | meaning |
|---|---|
| `0` | success |
| `1` | uncaught exception, out of memory, runtime trap, or I/O failure (unreadable input, unwritable output) |
| `2` | compile error (parse or lowering); also clap usage errors |

Errors are printed to stderr.

---

## 10. Conformance (test262)

`crates/jsc-test262` runs the official tc39/test262 suite against the release
`jsc` binary.

- **Checkout.** `scripts/test262-fetch.sh` clones test262 at a pinned commit
  (`2e0a56762801e275a9fdf96dc49d90ba0cddcf63`) into `.tmp/test262`; it is
  idempotent.
- **Eligibility** (`filter.rs`). A test is skipped when it has a `module`,
  `async`, `CanBlockIsFalse`, or `CanBlockIsTrue` flag (or a `resolution`-phase
  negative), is `noStrict`-only, uses a feature listed in
  `tests/test262/unsupported-features.txt` (exact names or `prefix*`), or needs a
  harness include jsc does not shim. `raw` tests run byte-for-byte; all others run
  in strict mode.
- **Assembly** (`assemble.rs`). Strict programs are `"use strict";` + the jsc
  harness shims from `tests/test262/harness/` (`sta.js`, `assert.js`,
  `compareArray.js`) + the test source. Because jsc functions cannot carry
  properties, `assert.throws(Name,` and `assert.<ident>(` calls in the test
  source are rewritten to plain `assert_throws_Name(` / `assert_<ident>(`
  functions.
- **Outcome** (`outcome.rs`). Each test runs as `jsc FILE` with a timeout
  (default 10 s); exit codes map to pass / fail / compile-error / timeout, with
  negative tests matched on parse versus runtime phase and the uncaught error
  name.
- **Baseline.** `tests/test262/baseline.txt` lists every passing test path.
  `--check` fails if any baseline test no longer passes; `--update-baseline`
  merges the current passes into it.
- **History.** `--history` appends `commit pass eligible total` for a full run to
  `tests/test262/history.tsv`. The current metric is its last row:

  | commit | pass | eligible | total |
  |---|---|---|---|
  | `5a97d3c` | 5195 | 22992 | 47665 |

  i.e. **5195 / 22992 eligible tests pass (≈ 22.6 %)**, out of 47665 tests in the
  pinned checkout; `baseline.txt` holds the same 5195 paths.

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

**Triggers.** `push` to `main` and `pull_request`, both restricted to these
paths: `crates/**`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`clippy.toml`, `rustfmt.toml`, `deny.toml`, `_typos.toml`, `tests/test262/**`,
`scripts/test262-fetch.sh`, `.github/workflows/ci.yml`. Changes elsewhere
(JS/TS packages, apps, other scripts, docs) do not run it.

**Job `rust`** (fmt + clippy + test + deny + shear + typos):

- `cargo fmt --all --check`
- `typos`
- `cargo clippy --all-targets --workspace` (deny-warnings from the workspace lints)
- `cargo nextest run --workspace`
- `cargo test --doc --workspace`
- `cargo shear`
- `cargo deny check`

**Job `test262`** (baseline regression gate, 90-minute timeout): fetch test262
(cached on the hash of `scripts/test262-fetch.sh`), `cargo build --release -p
jsc`, then `cargo run --release -p jsc-test262 -- --check`.

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

- TypeScript is type-stripped, never type-checked; `.tsx`/JSX is rejected, and
  `enum` / `namespace` are compile errors (their stripped `var` forms are not
  resolved by codegen).
- A single script is compiled; there are no ES modules (`import`/`export`) and
  no `import()`.
- Strict-mode semantics only: sloppy-mode constructs (`with`, implicit globals)
  are rejected, and the test262 runner skips `noStrict` tests.
- TDZ is enforced only for module cells; reading a block-scoped binding before
  its declaration inside the same function reads the earlier binding or
  `undefined` instead of throwing.
- `-p` completion values come from expression statements only (§7.3).

**Values & operators**

- Strings are length-prefixed UTF-8 with byte semantics: `.length`, indexing,
  `slice`, `charAt`, `split("")`, string spread, and `for-of` over a string all
  work per byte (`"é".length` is `2`). Case mapping and trimming are ASCII-only.
- Binary `+` without a string operand does not apply ToNumber to non-number
  operands (`1 + null`, `1 + true`, `true + true` yield `NaN`); other numeric
  operators do.
- String conversion of objects and arrays uses the console formatter
  (`"" + [1, 2]` is `"[ 1, 2 ]"`, `"" + {}` is `"{}"`), not
  `Array.prototype.join` / `"[object Object]"`.
- Loose equality does not apply ToPrimitive to objects (`[] == 0` is `false`).
- No bitwise or shift operators (`& | ^ ~ << >> >>>` and their compound forms),
  no comma/sequence expressions, no `void`.
- `parseInt` ignores a radix argument (always base 10).
- Not available: `RegExp` (literals and constructor), `Symbol`, `BigInt`,
  `Proxy`, `Reflect`, `WeakMap`/`WeakSet`/`WeakRef`, typed arrays,
  `ArrayBuffer`, `Intl`, `eval`, `arguments`. `Object`, `Array`, `Number`, etc.
  exist only as the specific built-ins listed in §7.9 (no `Array.isArray`,
  `Object.assign`, `Number.isInteger`, …).

**Object model**

- Objects are fixed-shape: computed keys (`{[k]: v}`) and numeric literal keys
  are rejected, and writing a property the object was not allocated with throws
  a `TypeError` (`let o = {}; o.k = 1` throws `Cannot add property 'k': dynamic
  properties are not supported yet`). Dynamic properties arrive with the wave-2
  object model.
- Array assignment past the end throws a `TypeError` (`a[5] = 2` on a
  one-element array throws `Cannot grow array by index assignment yet`); a
  non-index key (`a.foo = 1`, `a[-1] = 1`) throws the add-property error;
  `.length` is read-only.
- A named property read on a string receiver goes through index dispatch rather
  than returning `undefined` (`"abc".foo` reads a character).
- Built-in prototypes are not modelled: there is no generic method lookup, so
  unrecognized built-in methods (`sort`, `find`, `flat`, `at`, `charCodeAt`,
  `toFixed`, `toString`, …) are rejected; `Function.prototype.call/apply/bind`
  are unavailable.
- `.name`, `.message`, and `.stack` on errors are enumerable; `.stack` has no
  frames.
- Uncaught user-class instances render as plain objects (including hidden
  `@@prototype` keys), not by class name.

**Functions & classes**

- Arrows do not capture lexical `this`; an arrow sees the `this` of the call
  that invoked it (usually `undefined`).
- Closures created in a C-style `for (let i …)` loop share one `i` binding
  rather than capturing a per-iteration copy.
- Function declarations nested inside another function are not hoisted and
  cannot call themselves by name.
- A spread call delivers at most `K` arguments (the program's maximum declared
  arity or call-site argument count); surplus spread elements are dropped.
- `array.push` takes exactly one argument; `reduce` requires an initial value.
- Not supported: class expressions, private `#` members, static blocks, static
  or object-literal accessors, computed member names, async or generator
  methods, superclass expressions other than an earlier-declared class name, a
  field-declaring subclass without an explicit constructor.

**Generators & async**

- Generators and async functions must be top-level declarations: no nested
  declarations, expressions, arrows, or methods; no `async function*`.
- `.next(…)` is reserved for generators; a user object's own `next` method
  cannot be called (§7.8).
- `yield`/`await` are not allowed inside `try`/`catch`/`finally`, inside
  `switch`, in a loop or `if` condition, nested inside a larger expression, or in
  the body of a `for-of`/`for-in` inside a generator; generator bodies cannot
  contain nested closures; parameters must be simple identifiers.
- `.return()` / `.throw()` on generators are not available; `yield*` delegates
  only to generators.
- No `Promise.all` / `race` / `allSettled` / `any`; no thenable assimilation of
  non-promise objects; no unhandled-rejection reporting.

**Runtime**

- The emitted module imports six host functions from `env` (§6.1); it is not
  self-contained and cannot run without jsc's wasmtime host or the
  `@cdlab/jsc` shim. Runtime semantics are implemented three times (guest wasm,
  the Rust host, the TypeScript shim).
- No garbage collection; memory only grows, up to 512 MiB, and exhaustion is
  not catchable.
- `Date` has no string parsing, no calendar-field constructor, no local time
  zone; years are rendered in `0..=9999`.

---

## 14. Roadmap

Forward-looking work, in rough order:

1. **Generic object-model fallback.** A real property model behind the
   compile-time specializations: dynamic key growth, computed keys, array
   growth, and a generic property-lookup path for receivers whose shape is not
   known statically.
2. **Latin-1 / UTF-16 strings.** A dual string representation with JS `.length`
   and indexing semantics, done together with the object-model rework.
3. **JS prelude on prototypes.** Built-in methods installed on real prototype
   objects from a JavaScript prelude instead of being recognized syntactically.
4. **Self-contained WASI module.** Modules that import only WASI preview1
   (output and clock); the custom `env` imports, the Rust host semantics, and the
   TypeScript shim are removed.
5. **Garbage collection.** A precise collector over the existing linear memory
   and NaN-boxed values.
6. **ES modules.** `import`/`export` across multiple source files.
