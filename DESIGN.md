# jsc — Design

> An ahead-of-time **JavaScript → WebAssembly** compiler written in Rust, with
> conformance tracked by test262. jsc parses source with swc, lowers it to an
> in-house IR, emits a WebAssembly module with `wasm-encoder`, and runs that
> module on wasmtime (natively) or in a JS shim (in the browser).

This is the authoritative technical spec. It describes the compiler as it is
today: the pipeline, the value and memory model, the module ABI, the object
model, the runtime and host imports, the supported language surface,
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
  lives in one linear memory with a bump allocator (§4, §5).

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
   (interned strings, function data, the realm image, the intern table).
4. **run** (`run/`, feature `native-run`) — instantiates the module on wasmtime
   with a `Linker` that supplies the seven host imports (§6.1), calls `main`,
   drains the microtask queue, checks for an uncaught exception or unhandled
   rejection, and decodes the result jsval against a snapshot of linear memory.

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
│   │   │   ├── abi_drift_tests.rs # pins abi.rs layouts; checks packages/jsc/src/abi.ts
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
│   │   │   │   ├── sections.rs        # type / function index layout, globals, data segment
│   │   │   │   ├── props.rs           # property tables, interning, __get/__set/...
│   │   │   │   ├── realm.rs           # realm image: intrinsics registry, Builtin roster
│   │   │   │   ├── builtins/          # builtin bodies by family (array, string, json, ...)
│   │   │   │   ├── convert.rs         # ToPrimitive, ToString, `+`, relational, join
│   │   │   │   ├── expr.rs  stmt.rs   # node / statement emission
│   │   │   │   ├── numeric.rs         # arithmetic, comparisons, Math, conversions
│   │   │   │   ├── value.rs           # jsval helpers (__box_f64, __to_number, type tests)
│   │   │   │   ├── array.rs  object.rs function.rs generator.rs
│   │   │   │   ├── json_parse.rs      # in-wasm JSON.parse
│   │   │   │   └── runtime.rs         # __alloc, strings, Map/Set, equality, trampolines
│   │   │   └── run/               # wasmtime host
│   │   │       ├── harness.rs         # Linker, host imports, main call, OOM + uncaught checks
│   │   │       ├── host.rs            # __method dispatch, allocation through guest exports
│   │   │       ├── promise.rs         # promise engine + microtask queue + async driver
│   │   │       └── value.rs           # jsval decode + Node-style rendering
│   │   └── tests/             # integration tests by theme (arrays, properties, intrinsics,
│   │                          #   intrinsic_guards, to_primitive, wrappers, ...) and the
│   │                          #   opcount snapshot gate (opcount.rs + opcount/)
│   ├── jsc/                   # binary: clap CLI (src/main.rs) + CLI integration tests
│   ├── jsc-test262/           # test262 conformance runner (§10)
│   └── jsc-wasm/              # wasm-bindgen boundary: `compile(src) -> Uint8Array`
├── packages/jsc/              # @cdlab/jsc: compile() + run() in JS, host shim
│   └── src/  index.ts  shim.ts  abi.ts  value.ts
├── apps/jsc-web/              # browser playground (Vite + React + CodeMirror)
├── scripts/                   # build-wasm.sh, ensure-wasm.sh, test262-fetch.sh
└── tests/test262/             # unsupported-features denylist, baseline, history
```

**`jsc-core`.** The library. `#![forbid(unsafe_code)]`. The default feature
`native-run` pulls in `wasmtime` (`cranelift`, `runtime`, `std`); building with
`default-features = false` leaves frontend + codegen + emit only. Dependencies:
`swc_ecma_parser 46`, `swc_ecma_ast 29`, `swc_ecma_visit 29`, `swc_common 26`,
`swc_ecma_transforms_base 50`, `swc_ecma_transforms_typescript 56`,
`wasm-encoder 0.262`, `anyhow`, optional `wasmtime 49`; `wasmparser 0.262` is
a dev-dependency.

**`jsc`.** A thin `clap` shell (§9). `#![forbid(unsafe_code)]`.

**`jsc-wasm`.** `crate-type = ["cdylib", "rlib"]`, depends on `jsc-core` with
`default-features = false`. Exposes one `#[wasm_bindgen]` function,
`compile(src) -> Result<Vec<u8>, JsError>`; a spanned error becomes the caret
block labelled `<source>`. Its lint table mirrors the workspace's but relaxes
`unsafe_code` to `deny` because the `wasm-bindgen` glue expands to generated
`unsafe` shims. `wasm-pack` builds it with `wasm-opt -Oz`.

**`@cdlab/jsc`** (`packages/jsc`). `compile(src)` returns module bytes from the
wasm-bindgen artifact; `run(src)` compiles, instantiates the module against a
TypeScript host shim (`shim.ts`, which implements the same seven `env` imports and
promise engine as `run/`), calls `main`, and returns `{ result, stdout }`.
`abi.ts` and `value.ts` are ports of `abi.rs` and `run/value.rs`; a Rust test
(`abi_drift_tests.rs`) parses `abi.ts` and fails when any of its constants
differs from `abi.rs`. The wasm artifact is gitignored;
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
the host clock — calls the `__box_f64(x: f64) -> i64` helper
(`x == x ? reinterpret(x) : QNAN`), because wasm may produce a sign-bit or
payload-carrying NaN (`0/0` on x86, or a NaN operand such as `undefined`
propagating its tag bits). Integer-to-float conversions cannot produce NaN and
reinterpret directly. Consequently NaN tests compare bits directly, and a
computed NaN can never alias a boolean, `null`, `undefined`, or pointer.

**`typeof`.** `typeof` yields an interned heap string: the type index
(`number`, `boolean`, `undefined`, `object`, `string`, `function`, `symbol`
— `abi::TYPEOF_STRINGS`) selects the data-segment offset of the matching word,
so `typeof x === "number"`, concatenation, and `.length` all go through
ordinary string machinery. `TYPE_FUNCTION` blocks report `"function"`, strings
`"string"`, `TYPE_SYMBOL` blocks `"symbol"`, every other heap kind `"object"`. `typeof` of an unresolvable
global name is `"undefined"` and never throws.

**Truthiness.** Falsy is exactly `false`, `null`, `undefined`, `±0`, `NaN`, and
the empty string; every other value — including `[]`, `{}`, wrapper objects, and
every function — is truthy.

### 4.2 Module layout

**Types.** `main` has type `() -> i64`. Every user function (declarations,
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
| `0`–`6` | host imports `print`, `__convert`, `__method`, `__promise`, `__then`, `__now`, `__parse_int` (§6.1) |
| `7` | `main` (exported as `"main"`) |
| `8 … 7+n` | the `n` user functions |
| then | runtime helpers in a fixed order (§6.2): `__alloc` … `__to_number` |
| then | `__invoke1`, `__async_step`, the two `new Promise` settlers, `__new_string`, `__new_array`, `__new_promise`, `__has_own` |
| then | the builtin bodies, one per `Builtin::ALL` entry (`emit/realm.rs`, 142 entries) |
| last | `__to_primitive`, `__to_string`, `__add`, `__compare`, `__array_join`, `__to_object`, `__json_rep`, `__json_indent`, `__json_revive` |

**Table.** A funcref table of exactly `n + 2 + 142` entries (min = max), filled
by one active element segment: slots `0 … n-1` are the user functions, slots
`n` and `n + 1` the two settlers, then the builtin bodies in `Builtin::ALL`
order. A function value's `func_idx` is a slot in this table. Every module has
the table, including one without user functions.

**Memory.** One linear memory, `min = 1` page, `max = 8192` pages (512 MiB,
`abi::MEMORY_MAX_PAGES`), exported as `"memory"`.

**Globals.**

| index | name | type | exported as | role |
|---|---|---|---|---|
| 0 | `$__heap_ptr` | `i32` | `__heap_ptr` | bump-allocator cursor (§5.6) |
| 1 | `$exc_pending` | `i32` | `__exc_pending` | `1` while an exception propagates |
| 2 | `$exc_value` | `i64` | `__exc_value` | the in-flight thrown jsval |
| 3 | `$cf_kind` | `i32` | — | pending `return`/`break`/`continue` routed through `finally` |
| 4 | `$cf_value` | `i64` | — | routed return value / labeled-loop target |
| 5 | `$__this` | `i64` | — | `this` carrier, initialized to `undefined` (`abi::THIS_GLOBAL_INDEX`) |
| 6–8 | `$__json_data`, `$__json_pos`, `$__json_len` | `i32` | — | `JSON.parse` cursor |
| 9–11 | `$__intern_base`, `$__intern_cap`, `$__intern_count` | `i32` | — | intern table location, slot count, occupancy (§5.3) |
| 12 | `$__new_target` | `i64` | — | `new.target` carrier, initialized to `undefined` |
| 13 | `$__spread_args` | `i64` | — | argument array of a spread call / construct, initialized to `undefined` |
| 14 | `$__intrinsics_dirty` | `i32` | — | one dirty bit per watched intrinsic (§6.4) |
| 15+ | module cells | `i64` | — | one cell pointer per module binding (`15 + slot`, §7.1) |

**Exports.** `main`, `memory`, `__heap_ptr`, `__exc_pending`, `__exc_value`,
`__invoke1(fn: i64, arg: i64) -> i64`, `__async_step(gen: i64, sent: i64) -> i64`,
and the allocator and guest constructors `__alloc(size: i32) -> i32`,
`__new_string(len: i32, two_byte: i32) -> i32`, `__new_array(len: i32) -> i32`,
`__new_promise() -> i32` (§6.8), and `__promise_self_error() -> i64` (the
self-resolution `TypeError`, §6.7; the last defined function).

**Static region (data segments).** Everything below the initial heap pointer is
laid out by the compiler, in this order:

| range | contents |
|---|---|
| `0 … 7` | null guard; the `u32` at `abi::FN_INFO_ADDR = 4` holds the base of the function info table |
| from `HEAP_BASE = 8` | every distinct string literal and static key, one `TYPE_STRING` block each, 8-byte aligned, `flags = INTERNED` (plus `ARRAY_INDEX` for a canonical index spelling) |
| next | the function info table: one 8-byte entry `[name: u32 interned key][length: u32]` per funcref slot (user functions, the two settlers, the builtins) |
| next | one static 32-byte `TYPE_FUNCTION` block per user function (`LAZY`, no properties, `proto = %Function.prototype%`, `env = undefined`): the single function object of a top-level declaration |
| next | the realm image (§6.4) |
| next | the intern table: a power-of-two array of `u32` slots (§5.3) |

`$__heap_ptr` starts right after the intern table. Identical literals share one
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
  arguments (a rest parameter, `arguments`, a variadic builtin) reads the
  elements beyond `K` from it.
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
| 1 | `gc` | reserved for the phase-4 mark bit; always `0` |
| 2 | `flags` | per kind, `0` = default (below) |
| 3 | `class` | `TYPE_OBJECT` sub-class (§5.2); `0` for every other kind |

Every block base is 8-byte aligned (`BLOCK_ALIGN = 8`): `__alloc` rounds every
request up and static blobs are padded, so `i64` slots at block offsets ≡ 0
(mod 8) are naturally aligned. Multi-byte fields are little-endian.

The **object family** — `TYPE_ARRAY`, `TYPE_OBJECT`, `TYPE_FUNCTION`,
`TYPE_MAP`, `TYPE_SET`, `TYPE_PROMISE`, `TYPE_GENERATOR`, `TYPE_DATE` — shares
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

### 5.2 Block layouts

| type byte | kind | layout (offsets from the block base) | size | `typeof` |
|---|---|---|---|---|
| `0` `TYPE_STRING` | string | header, `len: u32 @4` (`STR_LEN_OFFSET`, UTF-16 code units), data `@8` (`STR_DATA_OFFSET`): Latin-1 bytes, or UTF-16LE units with `TWO_BYTE` | `8 + (len << two_byte)`, rounded | `string` |
| `1` `TYPE_ARRAY` | array handle | prefix, `len: u32 @16`, `elems: u32 @20` → `TYPE_ELEMS` (`0` while capacity is 0) | 24 | `object` |
| `2` `TYPE_OBJECT` | object | prefix; wrapper classes add `[[PrimitiveValue]]: i64 @16` (`WRAPPER_VALUE_OFFSET`) | 16 / 24 | `object` |
| `3` `TYPE_FUNCTION` | function | prefix, `func_idx: u32 @16`, reserved `u32 @20`, `env: i64 @24` | 32 | `function` |
| `4` `TYPE_ENV` | closure environment (internal) | header, `count: u32 @4`, `cell: i64 × count @8` | `8 + 8·count` | — |
| `5` `TYPE_CELL` | boxed binding (internal) | header, `value: i64 @4` | 12 | — |
| `6` `TYPE_MAP` | `Map` | prefix, `size: u32 @16`, pad, `entries: i64 @24` → backing array | 32 | `object` |
| `7` `TYPE_SET` | `Set` | same as `TYPE_MAP` | 32 | `object` |
| `8` `TYPE_PROMISE` | promise | prefix, `state: i32 @16`, pad, `value: i64 @24` | 32 | `object` |
| `9` `TYPE_GENERATOR` | generator / async generator | prefix, `sm_idx: u32 @16`, `state: i32 @20`, `done: i32 @24`, `limits: u32 @28`, `env: i64 @32`, `queue: u32 @40`, `queue_tail: u32 @44` | 48 | `object` |
| `10` `TYPE_DATE` | `Date` | prefix, `ms: f64 @16` | 24 | `object` |
| `11` `TYPE_ACCESSOR` | accessor pair (internal) | header, pad, `getter: i64 @8`, `setter: i64 @16` | 24 | — |
| `12` `TYPE_PROPS` | own-property table (internal) | §5.3 | `16 + 16·cap` | — |
| `13` `TYPE_ELEMS` | array element store (internal) | header, `cap: u32 @4`, `elem: i64 × cap @8` | `8 + 8·cap` | — |
| `14` `TYPE_SYMBOL` | Symbol primitive | header (flags: `4` `SYMBOL_FLAG_REGISTERED`; bits `1`/`2` always clear), reserved `u32 @4` = 0, `description: i64 @8` (string or `undefined`) | 16 | `symbol` |

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
exhausted), `[[NextIndex]]: u32` at `24` (an array index, a string byte
index, a Map/Set backing slot index), `kind: u8` at `28`: `0`..`2` array
keys / values / entries, `3` string, `4`..`6` map keys / values / entries,
`7` / `8` set values / entries; no host reads the payload).

Notes per kind:

- **Strings** are primitives with no prefix and no own table; `len` and
  `.length` count UTF-16 code units. The form is canonical: a string is
  one-byte (Latin-1) iff every unit is `<= 0xFF`, so every producer narrows
  (literals, slices, `JSON.parse`, host results), strings of different width
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
  shrunk (it keeps its tombstones until a collector exists, §13). `size` is
  the live entry
  count. Lookup is a linear scan with `__same_value_zero` that skips
  tombstones; the renderers skip them too.
- **Promise** blocks mirror settlement (`state` ∈ pending `0` / fulfilled `1` /
  rejected `2`, plus the settled `value`) so a memory snapshot can render them;
  reactions live in the host engine (§6.7).
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
  are consumed by `__get`/`__set` and never observed as values (the renderers
  print `[Getter]`, `[Setter]`, `[Getter/Setter]`).

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
  rebuilt at `max(4, 2·live)` (the same capacity when tombstones free half),
  copying live entries in order and keeping the watch flag and bit, and the
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
Literal-key sites pass the constant key address; computed keys go through
`__to_key(v, insert)` (`ToPropertyKey`: an interned string is its own key, a
symbol is its own key — its block address, never interned —, an object is
converted with `ToPrimitive` (hint string; a `Symbol` wrapper yields its
symbol) first, an integer `0 ≤ v < 2^32 - 1` is formatted in the guest,
anything else goes through `__to_string`). `__define` sets the table's
`HAS_SYMBOL_KEYS` when it appends a symbol key. Lookups use `insert = 0` but still intern a canonical
array index, so an element is found by an index key no property used. The
table is never pruned.

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
  left by growth, and truncation produce holes. A hole is not an own
  property: `hasOwnProperty` and own-key enumeration skip it and the
  renderers print `<N empty items>`, while `[[Get]]` and `HasProperty` (and
  so `in`, member reads, spread, destructuring, `for-of`, `JSON.stringify`,
  and every `Array.prototype` method) continue along the prototype chain
  exactly as for an index at or past `length`. Every element read that meets
  a hole calls `__proto_elem(o, i)`: `HOLE` at once when
  `__protos_indexless(o)` (no prototype is an array or has a table flagged
  `HAS_INDEX_KEYS`), else `HasProperty` then `Get` of the index key with
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
  test, the `Array.prototype` bodies' array branches, the host search/slice)
  skip a dictionary array, which takes the generic per-index path. The mode is
  never left.
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
  Error family, Map, Set, Promise, Date).
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

### 5.6 `__alloc` and memory growth

`__alloc(size: i32) -> i32` is a bump allocator over the `$__heap_ptr` global,
which starts at the first free offset after the static region (the module
declares enough initial pages to hold that region):

1. Round `size` up to 8; `old = __heap_ptr`; `end = u64(old) + u64(size)`
   (computed in `i64`, so it cannot wrap).
2. Fast path: if `end <= memory.size * 64 KiB`, set `__heap_ptr = end`, zero the
   4 header bytes, and return `old`.
3. Otherwise grow: try `memory.grow(max(need, memory.size))` (at least doubling);
   if that fails, retry with exactly `need` pages.
4. If both fail (the 8192-page / 512 MiB maximum is reached), store the sticky
   sentinel `abi::HEAP_PTR_OOM` (`-1`, i.e. `0xFFFF_FFFF`) in `__heap_ptr` and
   execute `unreachable`.

Host-built values (strings and arrays returned by `__convert` and `__method`,
promises created by the promise engine) are allocated by calling the exported
guest constructors (§6.8), never by bumping the pointer host-side. Every host
import result goes through `oom_guard`, which turns a sentinel heap pointer
into a trap.

**Out of memory.** After `main` (and after the post-`main` microtask drain) the
harness checks the sentinel; if set, the run fails with
`Uncaught RangeError: out of memory` — no wasm backtrace — regardless of the trap
it caused. Because the sentinel makes every later allocation fail, an
out-of-memory condition inside `try` is not catchable; the program terminates.

**No free.** Nothing is ever freed. Store and table growth abandons the old
block. Long-running allocation-heavy programs consume memory monotonically up
to the 512 MiB cap.

### 5.7 Garbage collection (design; implemented in phase 4)

The layouts above are chosen so a collector can be added without relayout:

- **Algorithm.** Precise, non-moving, stop-the-world mark-sweep. Non-moving
  because handles are identities (promise reactions are keyed by block base)
  and emitted code holds raw `i32` bases. No write barriers; the only heap-store
  sites are `__set`/`__define`, element stores, and cell stores.
- **Allocation.** Segregated size classes (16..256 by 16, then powers of two to
  2 KiB) in 64 KiB pages with a per-page class header, so no per-block size
  word; large objects in a list with a 16-byte pre-header. `__alloc` collects
  when the allocation budget since the last collection is spent, then grows
  memory; the 512 MiB cap and the OOM sentinel stay.
- **Roots.** A shadow stack in linear memory: each function reserves slots for
  jsvals live across a safepoint (any call that may allocate), clears them on
  entry, and pops them on every exit including the exception paths; raw `i32`
  bases are re-derived from spilled jsvals after safepoints. Global roots are
  `$exc_value`, `$cf_value`, `$__this`, `$__new_target`, `$__spread_args`, and
  the module cells. The static region is a root region: static object-family
  blocks are traced, never swept. With the promise engine in the guest
  (phase 4) the host holds no roots.
- **Tracing.** A jsval slot is a pointer iff the exact `QNAN` + heap-tag test
  holds (`HOLE` is not a pointer). Raw `u32` references are traced by kind:
  the object-family prefix (`props`, `proto`), an array's `elems` store (slots
  `0..len`), a property table's live keys and values, a function's or
  generator's `env`, Map/Set `entries`, a promise's `value`, a wrapper's
  `[[PrimitiveValue]]`, env cells, a cell's value, an accessor's getter and
  setter.
- **Intern table.** Weak: the sweep clears slots whose string is unmarked.
- **Reserved now.** Header byte 1, 8-byte alignment, every internal store a
  headered block, every reference at a kind-known offset, host allocation only
  through exports, and truncation writing `HOLE`.

---

## 6. Runtime

### 6.1 Host imports

Every module imports exactly seven functions from the `"env"` namespace
(`abi::IMPORT_MODULE`), declared in this order (function indices `0`–`6`):

| # | import | signature | backs |
|---|---|---|---|
| 0 | `print` | `(bits: i64, flag: i32) -> ()` | `console.log` |
| 1 | `__convert` | `(bits: i64, op: i32) -> i64` | primitive ToString / ToNumber, `parseFloat` |
| 2 | `__method` | `(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64` | array search, string case mapping / `localeCompare` / `normalize`, `Number.prototype` digit formatting, host `Math` functions |
| 3 | `__promise` | `(op: i32, a: i64, b: i64) -> i64` | promise primitives, top-level `await`, `async` spawn |
| 4 | `__then` | `(p: i64, on_f: i64, on_r: i64) -> i64` | `then` / `catch` |
| 5 | `__now` | `() -> f64` | `Date.now()`, `new Date()`, `Date()` |
| 6 | `__parse_int` | `(s: i64, radix: i64) -> i64` | `parseInt` |

The native host is `run/harness.rs` (wasmtime `Linker::func_wrap`); the browser
host is `packages/jsc/src/shim.ts`. Both decode jsvals with the same rules
(`run/value.rs` / `value.ts`).

- **`print` flags.** `0` = format the value and append a space (every argument
  but the last), `1` = format and append a newline (the last argument),
  `2` = newline only (zero-argument `console.log()`). Values render with the same
  `format_value` used for the CLI's `-p` output (§6.9).
- **`__convert` ops.** `0` ToString, `1` ToNumber (StringToNumber rules),
  `3` parseFloat. The guest runs `ToPrimitive` first (§6.5), so the host sees
  primitives, plus a `Date` (its `toString` text). A string argument to op `0` is returned unchanged; any
  other result string is allocated through `__new_string`.
- **`__method` ops.** `0` `indexOf` / `1` `includes` (an array receiver:
  element search), `4` `toUpperCase`, `5` `toLowerCase` (full Unicode
  default case mapping — `SpecialCasing`, `Final_Sigma` — over each run of
  well-formed text; the guest maps one-byte receivers itself and calls these
  only for two-byte receivers and the one-byte `µ` / `ß` / `ÿ` uppercase
  cases),
  `17` `Number.prototype.toString(radix)` (a number receiver and an integral
  radix in `2..=36`), `22` `localeCompare` (code-unit order, not locale
  collation), `23` `normalize` (`NFC` / `NFD` / `NFKC` / `NFKD`), `40` `toFixed`, `41` `toExponential`, `42` `toPrecision` (a
  finite number receiver and an integral digit count already range-checked
  in the guest; `undefined` for a `toExponential` without digits; rendered
  from the exact decimal expansion, ties away from zero), `43` a `Math`
  function (`arg1` the `MATH_FN_*` code: `cbrt`, `log`, `log2`, `log10`,
  `log1p`, `exp`, `expm1`, the trigonometric and hyperbolic functions and
  their inverses, `atan2` with `x` in `arg0`, and `random`; the Rust host
  uses the platform libm and a `xorshift64*` generator, the JS shim its own
  `Math`). Every other `String.prototype` method and `String.fromCharCode` /
  `fromCodePoint` run in the guest on code units (§7.9). Both hosts read a
  string receiver losslessly (the native host through `string_units`, the
  shim as JS strings): a lone surrogate passes through case mapping and
  `normalize` unchanged. An unknown `normalize` form returns the refusal
  sentinel `METHOD_RANGE_ERROR` (`undefined | 2`), which the calling builtin
  throws as a `RangeError`. The
  calling string builtin coerces the receiver (`ToString`) and its arguments
  in the guest first, so user `toString`/`valueOf` run in the guest and the
  host only sees primitives.
- **`__promise` ops.** `0` resolve, `1` reject, `2` await, `3` finally,
  `4` new pending promise, `5` settle-fulfill, `6` settle-reject,
  `7` async spawn.
- **`__now`.** `SystemTime::now()` since the Unix epoch in milliseconds (`0` if
  the clock is before the epoch). It is the only source of non-determinism.
- **`__parse_int`.** Parses the formatted string form of `s` in radix
  `ToNumber(radix)` (`undefined` for an omitted radix).

### 6.2 Guest runtime helpers

Everything else runs in wasm, emitted per module by `emit/`. The helpers, in
function-index order:

| group | helpers |
|---|---|
| allocation, strings | `__alloc`, `__concat`, `__str_eq`, `__str_cmp`, `__str_index` (all on code units, below) |
| interning (§5.3) | `__str_hash`, `__intern_slot`, `__intern_find`, `__intern`, `__index_value`, `__index_string`, `__str_exotic_index(o, key)`, `__to_key` |
| property tables (§6.3) | `__prop_find(o, key)`, `__define(o, key, v, attrs)`, `__fn_materialize(o)`, `__get(recv, key)`, `__set(recv, key, v)`, `__has(key, obj)`, `__delete(obj, key)`, `__own_keys(obj, mode)`, `__for_in_keys(obj)`, `__rest(src, excl)` |
| arrays (§5.4) | `__array_reserve(base, need)`, `__array_push(base, v)`, `__array_concat(dst, src)`, `__array_set(recv, key, v)`, `__array_set_length(arr, v)`, `__sparse_get(arr, i)`, `__sparse_set(arr, i, v)`, `__protos_indexless(o)`, `__proto_elem(o, i)`, `__proto_set(o, i, v)`, `__array_define(recv, key, v)`, `__sparse_define(arr, i, v)` |
| JSON (stringify) | `__json_escape(str)`, `__json_str(v)` — the primitive serializer: finite numbers via `__convert` ToString, `NaN`/`±Infinity`/`undefined` as `null`, keywords, escaped strings, any heap kind `null` |
| Map/Set, equality | `__same_value_zero`, `__map_find`, `__map_set`, `__map_get`, `__map_has`, `__map_delete`, `__loose_eq`, `__str_to_array` |
| JSON (parse) | `__json_ws`, `__json_throw`, `__json_string`, `__json_number`, `__json_value`, `__json_array`, `__json_object`, `__json_parse` |
| misc | `__gen_to_array(gen)`, `__nullish_throw(recv, key, set)`, `__prop_throw(recv, key, mode)`, `__box_f64(x)`, `__to_number(v)` |
| re-entry | `__invoke1(fn, arg)`, `__async_step(gen, sent)`, the two `new Promise` settlers |
| constructors | `__new_string(len, two_byte)`, `__new_array(len)`, `__new_promise()` |
| realm | `__has_own(key, obj)`, then the builtin bodies (§6.4) |
| conversions (§6.5) | `__to_primitive(v, hint)`, `__to_string(v)`, `__add(a, b)`, `__compare(a, b, op)`, `__array_join(recv, sep)`, `__to_object(v)` |
| JSON (§7.9) | `__json_rep(holder, key, value, replacer, list, stack)` — `SerializeJSONProperty` (`toJSON`, replacer function with `this = holder`, property list, wrapper unwrapping, cycle `TypeError` via the `stack` array); `__json_indent(s, gap)` — re-indent compact text for `space`; `__json_revive(holder, key, reviver)` — `InternalizeJSONProperty` |
| descriptors (§6.3) | `__define_own(o, key, desc, value, get, set)`, `__to_desc(attrs, list)`, `__own_desc(obj, key)`, `__set_integrity(obj, level)`, `__test_integrity(obj, level)` |
| promises (§6.7) | `__promise_self_error()` — the self-resolution `TypeError`, exported for the hosts |
| well-known symbols | `__symbol_get(v, sym)` — `Get(v, sym)`, short-circuited to `undefined` unless a table on `v`'s chain has `HAS_SYMBOL_KEYS` (the consult sites of the interim hook guards, §13) |

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

`__to_number(v)` runs `ToPrimitive` (hint number) in the guest, then the host
ToNumber on the primitive; numeric call sites skip it when the operand already
is a number; a symbol throws `TypeError`. `__to_object(v)` wraps a string,
symbol, number, or boolean in a fresh wrapper object (§5.2) and returns any
object unchanged.

### 6.3 Property algorithms

`__get`, `__set`, `__has`, `__delete`, `__define`, `__own_keys`,
`__for_in_keys`, `__rest`, and the descriptor helpers (`emit/props.rs`,
`emit/props/descriptor.rs`) are the only code that reads or writes property
tables. Keys are interned `u32`s.

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
  renderers): integer keys ascending (array elements skipping holes, then
  table keys flagged `ARRAY_INDEX` sorted numerically when the table has
  `HAS_INDEX_KEYS`), then the other string keys in insertion order, then — in
  `OWN_ALL_KEYS` only — the symbol keys in insertion order. `mode` selects
  enumerable string keys, values, entries, all string keys (`OWN_ALL`,
  `getOwnPropertyNames`), all symbol keys (`OWN_SYMBOLS`,
  `getOwnPropertySymbols`), or every key (`OWN_ALL_KEYS`: object rest,
  `Object.assign`, `defineProperties` / `create`,
  `getOwnPropertyDescriptors`, which check enumerability per key); every
  string-key mode skips symbol keys, and the symbol walks are skipped when the
  table lacks `HAS_SYMBOL_KEYS`. Values and entries follow EnumerableOwnProperties: the key list is
  taken once, then each key is re-checked as own and enumerable (`__own_enum`)
  when visited and skipped if a getter deleted it or made it non-enumerable;
  a key a getter adds is not visited. Only a getter can run between the
  snapshot and a visit, so the re-check (and the `[[Get]]` of every key) runs
  only when the own table holds an accessor entry; otherwise array elements
  load from the store directly.
- **`for-in`** (`__for_in_keys`): a snapshot taken at loop entry — own
  enumerable string keys, then each prototype's enumerable keys not already
  seen (a key seen at a nearer level, enumerable or not, shadows it).
- **Object rest** (`__rest`): a fresh object with every own enumerable property
  of the source (string keys, then symbol keys; an object source re-checks
  each key as own and enumerable when visited) whose key is not in the
  exclusion list.

### 6.4 Realm, global object, and fast paths

**Realm image.** Every intrinsic is a static block in the data segment, laid
out by `emit/realm.rs` from one registry of `(intrinsic, key, attrs, value)`
entries (`key` is an interned string or a well-known symbol; `value` is a
builtin method, a string, a number, `undefined`, another intrinsic, a
well-known symbol, or an accessor pair). Each intrinsic is an object-family
block (a `TYPE_FUNCTION` block when callable) followed by a full `TYPE_PROPS`
table (flagged `PROPS_HAS_SYMBOL_KEYS` when it holds a symbol key); then one
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
`Array.prototype[@@unscopables]` (`proto` `null`), and
`%WrapForValidIteratorPrototype%` (`next` / `return`, inheriting
`%IteratorPrototype%`). Each
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
| `GetIterator` (for-of, spread, destructuring, `Array.from`, `Object.fromEntries`, the Map/Set constructors) | a clean array / `arguments` walked by index, a string by code point, a `Map` / `Set` by backing entry, a generator by direct state-machine resumes (`design/symbol-strings.md` §5.3); decided once per loop | the receiver test of `ProtoGuard` for its family (a generator: the `next` call site's, and its body has no `yield*`), plus clear bits for its iterator prototype and `%IteratorPrototype%` (a string: `%String.prototype%`'s); `arguments`: its own `@@iterator` is still the data property `%Array.prototype.values%`. Unless intrinsics-sealed, only with dirty tracking (a builtin body or a spread tests the bits only when the program tracks them; a spread source or an unsealed `GetIterator` turns tracking on) | the protocol: `GetMethod(@@iterator)`, its call, `next` read once (the internal `Protocol` builtins) |
| `x instanceof C` for an unshadowed, sealed `Array` / `Function` / `Date` / `Object` / Error-family `C` | a `proto` walk for the constructor's static prototype (`%Array.prototype%`, `%Function.prototype%`, `%Date.prototype%`, `%Object.prototype%`, an error prototype); the operand's direct `proto` is the first link, so a replaced `proto` is honoured | a run-time intact test: `C` and each constructor on its static chain up to `%Function.prototype%` (a native error: `%Error%`) has its realm `proto` and no own `@@hasInstance` (a table without `HAS_SYMBOL_KEYS` passes inline, else a `__prop_find` miss decides, so `%Array%`'s own `get [Symbol.species]` keeps the arm), so `GetMethod(C, @@hasInstance)` is the immutable `%Function.prototype%[@@hasInstance]` | `InstanceofOperator` (§7.6) on the realm constructor |
| `x instanceof C` otherwise | `InstanceofOperator` (§7.6) | — | — |
| `ArraySpeciesCreate` / `SpeciesConstructor` (`%Promise%`) in a builtin body | `ArrayCreate` / the host promise op, no `@@species` read | `constructor` is `%Array%` / `%Promise%` and its bit is clear, with dirty tracking | `Get(C, @@species)` |

The shared **`MethodCall`** builtin is the generic call for every guarded site:
`__get(recv, key)`, `IsCallable` (else `TypeError: recv.m is not a function`),
then a call with `this = recv`; it also throws for a nullish receiver. Its
`env` packs the interned key and the error text, so each site costs one
`call_indirect`. Array fast paths keep the `props == 0` and `proto` checks
even when the program is intrinsics-sealed: the sealed analysis does not cover
an own property shadowing a builtin (`a.push = f` is honoured).

**Opcount gate.** `crates/jsc-core/tests/opcount.rs` compiles every program in
`tests/opcount/*.js` and compares, per program, the static operator count of
`main` and the user functions, the `call_indirect` / property-helper / host
call sites, and the executed instruction count (wasmtime fuel) against
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
  neither yields one. The host never sees an object that has a user
  `toString`/`valueOf`.
- **`ToString`** (`__to_string`): a string is itself; an object is converted
  with hint string first; a symbol throws `TypeError` (only a called
  `String(sym)` yields `"Symbol(desc)"`); the remaining primitives are
  formatted by the host (`__convert` op `0`, ECMAScript `Number::toString`
  digits).
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
- At the boundary the harness reads `__exc_pending`; if set, it renders
  `__exc_value` with `format_value` and fails with `Uncaught <value>`.

### 6.7 Promises, microtasks, and the async driver

Promise *settlement* lives in the guest heap (`TYPE_PROMISE`); *scheduling* —
each promise's reaction list and the FIFO microtask queue — lives in the host
(`run/promise.rs`, mirrored by the shim), keyed by the promise block's offset.

- The `Promise.resolve` / `Promise.reject` / `Promise.prototype.finally`
  builtins and top-level `await` call `__promise`; `then` and `catch` call
  `__then`, which returns the chained promise. Missing handlers pass the
  settlement through. Resolving with a promise adopts it; other thenables are
  not assimilated (§13). Resolving a promise with itself — through an executor
  `resolve`, a reaction returning its chained promise, or an `async` body
  returning its own result — rejects it with a fresh `TypeError` ("Chaining
  cycle detected for promise #<Promise>"), built by the exported
  `__promise_self_error()` guest constructor; both hosts check it in `settle`. `then` on a receiver that is not a
  promise throws a `TypeError`. `then` and `finally` take their result
  promise's constructor from `SpeciesConstructor(this, %Promise%)`: for
  `%Promise%` the host ops above; for any other `C` (a subclass, a user
  `@@species`) `then` builds `NewPromiseCapability(C)` (constructing `C` with
  an internal executor closure) and registers two internal reaction closures
  through `__then` that settle the capability, and `finally` (also on any
  non-promise object) calls `this.then(thenFinally, catchFinally)` with the
  spec's closures (`PromiseResolve(C, onFinally())` then a value / thrower
  thunk). An awaited non-promise is treated as already
  fulfilled (`await 5` → `5`).
- `Promise.resolve` / `Promise.reject` construct from `this`:
  `PromiseResolve(C, x)` (a promise whose `constructor` is `C` is returned
  as is) / `NewPromiseCapability(C)`; `C === %Promise%` takes the host op
  (for `resolve` only a non-promise `x`), and so does the inline
  `Promise.resolve(x)` of a never-written `Promise` global. The combinators
  `Promise.all` / `allSettled` / `any` / `race` (`emit/builtins/promise.rs`)
  build `NewPromiseCapability(this)`, read `this.resolve` once, open the
  iterable's iterator record (design/symbol-strings.md §5.1) and per value
  call `resolve` with `this`, then `Invoke(next, "then", «onF, onR»)` with
  internal element closures (`Builtin::Species(SpeciesOp::Element)`, `env =
  [state, index, alreadyCalled]` over a shared `[values,
  remainingElements, settle]`). A throw closes the iterator unless it came
  from the iterator itself, then rejects the result promise. `this ===
  %Promise%` with its builtin `resolve`, its and `%Promise.prototype%`'s
  dirty bits clear and a `%Promise%` instance without own properties skips
  the `resolve` call and registers the reactions through `__then`.
- **Re-entry.** Reactions are guest closures; the engine runs them by calling the
  exported `__invoke1(fn, arg)` trampoline, which issues the uniform-ABI
  `call_indirect`. A callback that throws sets `$exc_pending`; the engine reads
  and clears it and rejects the chained promise.
- **`new Promise(executor)`** allocates a pending promise `P` (op `4`), then
  passes the executor two function values whose table slots are the
  settler functions (slots `n`, `n + 1`) and whose `env` holds `P` directly.
  Calling them settles `P` (ops `5`/`6`, settle-once). A throwing executor
  rejects `P`.
- **Draining.** There are no timers or I/O, so the microtask queue is the only
  asynchronous source. Top-level `await` drains the queue until its operand
  settles (a rejection surfaces as an uncaught throw). After `main` returns
  normally the harness drains whatever remains, so unawaited `then` chains run.
  If `main` ends with a pending exception, the queue is not drained.
- **`async` functions** (§7.8) are spawned with op `7`: the host allocates the
  result promise, drives the body's state machine one step at a time through the
  exported `__async_step(gen, sent)` trampoline, fulfills on completion, rejects
  on a throw, and on each `await` registers a continuation on the awaited value
  that resumes the generator from the microtask queue.
- **Async generators** (§7.8) need no host support: their request queue and
  driver are guest builtins that allocate (op `4`) and settle (ops `5`/`6`)
  each request's promise and register their `await` continuations with
  `__then`.
- **Unhandled rejections.** After the final drain, the first promise that was
  rejected and never got a handler fails the run with
  `Uncaught (in promise) <value>` (exit 1); a late `catch` still counts as a
  handler.

### 6.8 Host boundary

The host reads layouts but never builds object-family blocks by hand: every
string, array, and promise the Rust host or the shim creates is allocated by
re-entering the exported `__new_string(len, two_byte)` (header, `TWO_BYTE`
flag and length in code units written; the host fills `len` Latin-1 bytes or
UTF-16LE units, choosing the width from the content so the block is
canonical, §5.2), `__new_array(len)` (a handle with a `cap = len` store of
`undefined`), or `__new_promise()` (pending, `%Promise.prototype%`). Layout
knowledge lives in the emitter plus read-only decoders.

Host reads, all through `abi.rs` constants (and `abi.ts`, kept equal by the
drift test): string `len @4` (`STR_LEN_OFFSET`) / data `@8`
(`STR_DATA_OFFSET`) at the width of `TWO_BYTE`; array `len @16` and the
`TYPE_ELEMS` store (and a sparse array's index keys); promise `state @16` /
`value @24`; generator `done @24`; Map/Set `size @16`; Date `ms @16`; property
tables, the function info table, and `class` bytes for rendering. The Rust
decoder `string_units` is lossless (a Latin-1 byte is its own unit), and the
string host ops (case mapping, `localeCompare`, `normalize`) read their
operands through it; `normalize` uses `icu_normalizer` (an optional dependency
enabled by the `native-run` feature). `decode_string` renders text for output
and the number parsers (ToNumber, `parseInt`, `parseFloat`) as lossy text, a
lone surrogate becoming U+FFFD (safe for the number parsers: a lone surrogate
is never whitespace or a digit). `alloc_units` allocates a result through
`__new_string`, narrowing when every unit is `<= 0xFF`. The shim's
`stringText` builds the JS string with `String.fromCharCode` over chunked
`Uint8Array` / `Uint16Array` views (no `TextDecoder`: `latin1` would be
windows-1252 and `utf-16le` would replace lone surrogates). Host-held
jsvals (reaction lists, the job queue) stay valid because no block moves.

### 6.9 Value rendering

`run/value.rs` decodes a jsval against a memory snapshot into a `Value` and
`format_value` renders it Node-console style (the shim's `value.ts` mirrors
it):

- numbers by the ECMAScript Number::toString rules (shortest round-trip digits,
  exponential form at `>= 1e21` and `< 1e-6`, `-0` prints `0`); strings
  unquoted, also inside containers, with a lone surrogate written as U+FFFD
  on the native output (what Node writes; the shim hands the JS string
  through); symbols `Symbol(a)` / `Symbol()`;
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
  `[Symbol: Symbol(a)]`, from their `[[PrimitiveValue]]` (a String wrapper's
  virtual indices are not listed);
- `Map(N)` / `Set(N)`, `Promise { 42 }` / `Promise { <pending> }` /
  `Promise { <rejected> r }`, dates as ISO 8601 UTC (`Invalid Date` for a
  `NaN` time value);
- cycles as `<ref *1> { self: [Circular *1] }`.

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
  dedicated `i64` global (index `15 + slot`), allocated in `main`'s prologue
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
  ToInt32 / ToUint32 on the operands.
- **Relational** `< > <= >=` (§6.5).
- **Strict equality** `===` / `!==`: numbers numerically (`NaN !== NaN`,
  `+0 === -0`), strings by byte content, everything else by identity bits.
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
- **Conditional** `c ? a : b`, optional chaining `a?.b`, `a?.[k]`, `a?.()`,
  `a?.m()` (a nullish base short-circuits the rest of the chain to
  `undefined`).
- **Comma** `a, b, c` evaluates every operand left to right and yields the last
  (a plain value: `(0, o.m)()` calls without a receiver, while parentheses
  alone keep the reference: `(o.m)()` calls with `this = o`); `yield`/`await`
  operands suspend in order.
- **`typeof`**, **`instanceof`** (§7.6), **`in`** (`__has`), **`delete`**
  (`__delete`).
- **Member access on `null`/`undefined`** throws a `TypeError`
  (`Cannot read properties of null (reading 'x')`), catchable like any throw.

### 7.3 Control flow

`if`/`else`, `while`, `do … while`, C-style `for`, `for-of`, `for-in`, `switch`
(strict `===` matching, fall-through, `default`), `break` / `continue` (with
labels on loops), `return`, `throw`, `try`/`catch`/`finally` (with or without a
catch binding).

- **`for-of`** runs the iteration protocol (`design/symbol-strings.md`
  §5.1-§5.3): `GetIterator` (`@@iterator`, called once; `next` read once;
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
  (and the Array Iterator bits are clean). `for await` is rejected
  at compile time. The loop head declares its binding with `const`/`let`
  (identifier or pattern) or assigns a pattern (`for ([a] of xs)`).
- **`for-in`** iterates the key snapshot of §6.3; strings and arrays list their
  index keys first.

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
  (collected from the positional slots and `$__spread_args`), and array/object
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
  `SyntaxError` (a destructuring assignment pattern may repeat it).
- **Property access** `o.k`, `o["k"]`, `o[expr]` (§6.3): own keys, then the
  prototype chain; a missing key reads `undefined`. Writes add properties
  dynamically; `delete` removes them.
- **Destructuring.** Array patterns (holes, defaults, rest, nesting) and object
  patterns (identifier and string keys, rename, defaults, nesting, rest via
  `__rest`) in declarations, parameters, and `for-of` heads; destructuring
  assignment as an expression statement, with member targets (`[o.x] = xs`).
  An array pattern runs the iteration protocol lazily
  (`design/symbol-strings.md` §5.2): `GetIterator`, one `IteratorStep` per
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
  errors).
  Calling `C` without `new` throws `TypeError: Class constructor C cannot be
  invoked without 'new'`.
- **Fields.** Instance fields (`f = init`, or `f;` for `undefined`) are
  defined with `__define` in source order before the constructor body (for a
  derived class, right after `super(..)` returns). Static field initializers
  run after the class binding exists, in source order, with `this` = the class.
- **Inheritance.** `class B extends E` evaluates `E` once at class definition
  (any expression; it must be a constructor or `null`), links
  `B.prototype.[[Prototype]]` to `E.prototype` and `B.[[Prototype]]` to `E`
  (statics are inherited). `B` is `SELF_ALLOC`: its `this` is in TDZ until
  `super(args)` constructs it from `E` with `new.target`; touching `this` first
  throws `ReferenceError: Must call super constructor in derived class before
  accessing 'this' …`. `super.m(args)` calls the parent prototype's method with
  the current `this`. A derived class without a constructor forwards its
  arguments. Builtin parents (`Error`, `Map`, `Set`, `Promise`, `Date`,
  `Array`) work through the same path: their constructors allocate from
  `new.target.prototype`.
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
- Out of memory is reported as `Uncaught RangeError: out of memory` and is not
  catchable (§5.6).

### 7.8 Iteration, generators & async

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
(`1 + (yield 2)`, a loop condition); the expression is linearized around it.

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
  clean generator delegate.

**`async`/`await`.** An `async function` (in every position a `function*` may
take, including `async` methods; no `prototype`) or `async` arrow is a generator whose `await e` is a `yield e`; its
function value is a wrapper that builds the generator and hands it to the host
async driver (§6.7), returning the result promise; a parameter binding error
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
  reactions on the operand (`Promise.resolve` semantics); its settlement
  resumes the body (a rejection throws at the `await`) and continues serving
  the queue from the microtask queue. Requests made meanwhile, from outside or
  from the body itself, wait in the queue.
- An abrupt request before the body started completes it. On a completed
  generator `next` fulfills `{ undefined, done: true }`, `throw(e)` rejects
  with `e`, and `return(v)` awaits `v` and fulfills `{ v, done: true }` (or
  rejects). `return(v)` at a suspended `yield` resumes the body with a
  return completion there, which awaits `v`, then runs the enclosing
  `finally` blocks like a generator's.
- `yield*` in an async generator is a compile error.

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
| `Object` | `Object(v)` / `new Object(v)` (ToObject), `Object.keys`, `Object.values`, `Object.entries`, `Object.defineProperty`, `defineProperties`, `getOwnPropertyDescriptor`, `getOwnPropertyDescriptors`, `getOwnPropertyNames`, `getOwnPropertySymbols`, `preventExtensions`, `isExtensible`, `seal`, `isSealed`, `freeze`, `isFrozen` (§6.3; primitives: the setters return them, `isExtensible` is `false`, `isSealed` / `isFrozen` are `true`), `create`, `getPrototypeOf`, `setPrototypeOf`, `assign`, `hasOwn`, `is`, `fromEntries` (over any iterable; a bad entry closes its iterator); `Object.prototype.hasOwnProperty`, `isPrototypeOf`, `propertyIsEnumerable`, `toString` (`[object Tag]`: a string `@@toStringTag` on the receiver's chain, else the builtin tag; `Map`, `Set`, `Promise`, `Generator`, `AsyncGenerator`, `Symbol`, `Math`, `JSON` and the Array / String / Map / Set Iterator prototypes own a configurable-only `@@toStringTag`), `toLocaleString`, `valueOf`, the `__proto__` accessor, `__defineGetter__`, `__defineSetter__`, `__lookupGetter__`, `__lookupSetter__` |
| `Function` | `Function.prototype.call`, `apply` (`CreateListFromArrayLike`: arrays, `arguments`, any object with `length`), `bind` (bound function exotic objects, §5.5), `toString` (always the `NativeFunction` form `function name() { [native code] }`: no source text is kept; the name is omitted unless it is an `IdentifierName`, optionally after `get `/`set `); `%Function.prototype%` `length`/`name`, `caller`/`arguments` `%ThrowTypeError%` accessors, `[@@hasInstance]` (`OrdinaryHasInstance`; non-writable, non-enumerable, non-configurable); `Function(..)` throws `TypeError` |
| `Array` | `Array(n)` / `new Array(..)`, `Array.isArray`, `Array.of`, `Array.from(items, mapFn, thisArg)` (any iterable through its iterator, closed when `mapFn` or the define throws; else an array-like; `this` constructed when a constructor); `push` / `pop` (variadic), `map`, `filter`, `forEach`, `reduce`, `reduceRight` (with or without an initial value; callbacks receive `(element, index, array)` and `thisArg`; every callback of the higher-order methods gets all its specified arguments — `reduce` / `reduceRight` `(acc, element, index, array)`, `sort` comparators `(a, b)` — the first `K` positionally and, when there are more, all of them in the spread argument array (§4.3): the inline loop allocates it once and refills it per call, and only when some user function gathers its arguments (a rest parameter or `arguments`) or the callback is a builtin, so other callbacks keep the plain `K`-slot call), `find`, `findIndex`, `findLast`, `findLastIndex`, `some`, `every`, `at`, `indexOf`, `lastIndexOf`, `includes`, `join`, `slice`, `concat`, `flat`, `flatMap`, `keys`, `values` (also `[@@iterator]`), `entries` (Array Iterators over `ToObject(this)`, re-reading `length` per step), `toString`, `toLocaleString` (`Invoke(element, "toLocaleString")` per element; `null` / `undefined` / holes are empty), and the mutating `splice`, `sort(comparefn)` (a stable bottom-up merge sort; `undefined`s last, holes after them), `reverse`, `shift`, `unshift`, `fill`, `copyWithin`, and the change-by-copy `with`, `toReversed`, `toSorted(comparefn)` (the same stable sort), `toSpliced` (each reads every index by `Get`, so holes become `undefined`, and returns a plain `ArrayCreate` array, never a species instance) — every method generic over array-likes; `map` / `filter` / `slice` / `splice` / `concat` / `flat` / `flatMap` create their result by `ArraySpeciesCreate` (`constructor[@@species]`; `%Array%`, `%Map%`, `%Set%` and `%Promise%` have the builtin `get [Symbol.species]` returning `this`), `concat` spreads the items `IsConcatSpreadable` accepts (`@@isConcatSpreadable`, else `IsArray`), and `Array.prototype[@@unscopables]` is the spec's `null`-prototype list |
| `String` | `String(v)`, `new String(v)` (wrapper, virtual indices §5.3); `String.fromCharCode`, `String.fromCodePoint` (`RangeError` for a non-code-point; lone surrogates allowed), `String.raw` (over any `{ raw: array-like }`); `at`, `charAt`, `charCodeAt`, `codePointAt`, `concat`, `endsWith`, `includes`, `indexOf`, `isWellFormed`, `lastIndexOf`, `localeCompare` (code-unit order, not locale collation), `normalize` (all four forms; an unknown form is a `RangeError`), `padEnd`, `padStart`, `repeat` (a result past `MAX_STRING_LENGTH` code units is a `RangeError`, as for `padStart` / `padEnd`), `slice`, `split`, `startsWith`, `substring`, `toLowerCase`, `toUpperCase` (full Unicode mappings, lossless; a one-byte receiver maps in the guest), `toLocaleLowerCase`, `toLocaleUpperCase` (no locale data: the same mappings), `toWellFormed`, `trim`, `trimEnd`, `trimStart` (JS whitespace, one table), `toString`, `valueOf`, `[@@iterator]` (a String Iterator stepping one code point per `next` — a lead and trail surrogate pair is one two-unit string, a lone surrogate its own unit — through `__str_iter_next`, §5.4); `search`, `match`, `matchAll`, `replace`, `replaceAll`, `split` call an object argument's `@@search` / `@@match` / `@@matchAll` / `@@replace` / `@@split` method with `(O, …)` before `ToString(this)` (`matchAll` / `replaceAll` first require an `IsRegExp` argument's `flags` to hold `g`), and `includes` / `startsWith` / `endsWith` throw a `TypeError` for an `IsRegExp` argument (`ToBoolean(arg[@@match])`); `search`, `match`, `replace`, `replaceAll` with string patterns (`$$`, `$&`, `` $` ``, `$'` substitutions; function replacers get `(match, position, string)`; the RegExp-syntax check of a `search` / `match` pattern runs in the guest); `.length`, indexing. Every generic method is `RequireObjectCoercible(this)` then `ToString(this)`; all but the host case-mapping path (§6.1), `localeCompare` and `normalize` (host, §6.1) run in the guest on code units (`emit/builtins/string.rs`), surrogate pairs combined by `codePointAt` / `fromCodePoint` / `isWellFormed` |
| `Number` / `Boolean` | `Number(v)`, `Boolean(v)`, wrappers via `new`; `Number.isInteger`, `isSafeInteger`, `isFinite`, `isNaN` (no coercion), `Number.parseFloat` / `parseInt` (the global functions); `EPSILON`, `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`, `MAX_VALUE`, `MIN_VALUE`, `NaN`, `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`; `Number.prototype.toString(radix)`, `toFixed`, `toExponential`, `toPrecision`, `toLocaleString` (= `toString()`), `valueOf`; `Boolean.prototype.toString`, `valueOf` |
| `Math` | `sumPrecise(items)` (any iterable of numbers, else a `TypeError` after closing it; summed exactly in a fixed-point limb accumulator and rounded once; `NaN` / opposite infinities give `NaN`, no value or only `-0` gives `-0`), and every ES2025 function: `floor`, `ceil`, `round`, `trunc`, `abs`, `sqrt`, `sign`, `min`, `max`, `hypot` (variadic), `pow`, `clz32`, `fround`, `imul` in wasm; `cbrt`, `log`, `log2`, `log10`, `log1p`, `exp`, `expm1`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `random` on the host (§6.1); `E`, `LN10`, `LN2`, `LOG10E`, `LOG2E`, `PI`, `SQRT1_2`, `SQRT2` |
| `JSON` | `JSON.stringify(value, replacer, space)` (replacer function or property list, `space` number/string (a string's first 10 code units), `toJSON`, wrappers unwrapped, functions and symbols skipped, cycles throw, a lone surrogate written as `\udxxx`), `JSON.parse(text, reviver)` (in-wasm recursive descent, `\uXXXX` escapes incl. lone surrogates, reviver walk) |
| `Map` / `Set` | `new Map(iterable)`, `new Set(iterable)`; Map `get`/`set`/`has`/`delete`/`clear`/`forEach`/`size`/`keys`/`values`/`entries` (also `[@@iterator]`); Set `add`/`has`/`delete`/`clear`/`forEach`/`size`/`values` (also `keys` and `[@@iterator]`)/`entries`; the iterators walk the live backing entries, so entries appended during iteration are seen |
| `Promise` | §6.7, §7.8; `then` / `finally` create their result through `SpeciesConstructor(this, %Promise%)` (`get [Symbol.species]` on `%Promise%`); `Promise.resolve` / `Promise.reject` construct from `this` (`PromiseResolve(C, x)` / `NewPromiseCapability(C)`; a non-constructor `this` throws `TypeError`); `Promise.all`, `allSettled`, `any` (all rejected: an `AggregateError` with `errors`), `race` over any iterable (§6.7) |
| `Date` | `new Date()`, `new Date(ms)`, `new Date(year, month[, date, hours, minutes, seconds, ms])` (UTC fields: `MakeDay` / `MakeTime`, `TimeClip`), `Date()` (current time as a string), `Date.now()`; `getTime`, `valueOf`, `getFullYear`, `getMonth`, `getDate`, `getDay`, `getHours`, `getMinutes`, `getSeconds`, `toISOString`, `toJSON`, `toString` (`Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)`), `[@@toPrimitive]` (configurable only) — all UTC, computed in wasm |
| `Symbol` | `Symbol(desc)` (a fresh symbol; `new Symbol()` throws `TypeError`, though `Symbol` is a constructor for `IsConstructor`), `Symbol.for` / `Symbol.keyFor` (the hidden registry; `keyFor` of a non-symbol throws), the 13 well-known symbols (non-writable, non-enumerable, non-configurable); `Symbol.prototype.toString` (`Symbol(desc)`), `valueOf`, the `description` getter, `[@@toPrimitive]`, `[@@toStringTag]` `"Symbol"` (both configurable only), each over `thisSymbolValue` (a symbol or a `CLASS_SYMBOL` wrapper) |
| errors | §7.7 |
| generators | `%GeneratorPrototype%` `next`, `return`, `throw` |
| iterators | `%IteratorPrototype%[@@iterator]`, `%AsyncIteratorPrototype%[@@asyncIterator]` (both return `this`); the `next` of the Array, String, Map and Set Iterator prototypes (a receiver that is not an iterator of the family throws `TypeError: next method called on incompatible receiver`); `Iterator` (abstract: calling it, or `new` with `new.target === Iterator`, throws `TypeError`; a subclass constructs), `Iterator.prototype` (`%IteratorPrototype%`, non-writable) with `constructor` (`%Iterator%`) and `@@toStringTag` (`"Iterator"`) accessors whose setters are `SetterThatIgnoresPrototypeProperties` (a `TypeError` on a non-object or on `Iterator.prototype` itself; an own data property on any other receiver); `Iterator.from(O)` (`GetIteratorFlattenable`: strings are iterated, other primitives throw `TypeError`; an iterator inheriting `Iterator.prototype` is returned as is, any other is wrapped in a `CLASS_ITERATOR` object of kind `16` over `%WrapForValidIteratorPrototype%`, whose `next` / `return` forward to the wrapped record, `next` read once); no iterator helpers yet. A program that names `Iterator`, `constructor`, `getPrototypeOf` or `setPrototypeOf` compiles the accessor dispatch of `__get` / `__set` |

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
`regular expression literals are not supported`, `BigInt literals are not
supported`, `with statements are not supported`, `private class fields are not
supported`, `class static blocks are not supported`,
`async generator functions are unsupported`, `a label may only be applied to a loop`,
`destructuring assignment is only supported as an expression statement`.

**Run-time errors** carry no source span:

- An uncaught JS exception fails the run with `Uncaught <rendered value>`
  (e.g. `Uncaught TypeError: Cannot read properties of null (reading 'x')`,
  `Uncaught 1`); an unhandled rejection with `Uncaught (in promise) <value>`.
- Heap exhaustion fails with `Uncaught RangeError: out of memory` (§5.6).
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
  A non-negative `async` test that exits 0 passes only if stdout has a line
  exactly `Test262:AsyncTestComplete` and none starting
  `Test262:AsyncTestFailure`; otherwise it fails with that failure line (or
  `missing Test262:AsyncTestComplete`) as the detail in
  `.tmp/test262-results.tsv`. Negative async tests follow the negative rules.
- **Baseline.** `tests/test262/baseline.txt` lists every passing test path.
  `--check` fails if any baseline test no longer passes; `--update-baseline`
  rewrites it to the current passes (with PATH_PREFIX filters, only the entries
  under those prefixes are replaced).
- **History.** `--history` appends `commit pass eligible total` for a full run to
  `tests/test262/history.tsv`. The current metric is its last row:

  | commit | pass | eligible | total |
  |---|---|---|---|
  | `319208e` | 10185 | 23208 | 47665 |
  | `d0f5e6a` | 10269 | 26388 | 47665 |
  | `15be3e0` | 14301 | 30008 | 47665 |
  | `968dd97` | 19243 | 30014 | 47665 |
  | `eee36ef` | 19378 | 30014 | 47665 |

  i.e. **19378 / 30014 eligible tests pass (≈ 64.6 %)**, out of 47665 tests in
  the pinned checkout; `baseline.txt` holds the same 19378 paths. The drop in
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

**Job `rust`** (fmt + clippy + test + deny + shear + typos):

- `cargo fmt --all --check`
- `typos`
- `cargo clippy --all-targets --workspace` (deny-warnings from the workspace lints)
- `cargo nextest run --workspace` (includes the opcount snapshot gate, §6.4;
  the M1 harness tests in `crates/jsc/tests/test262_harness.rs` print a notice
  and pass when the test262 checkout is absent, as in this job)
- `cargo test --doc --workspace`
- `cargo shear`
- `cargo deny check`

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

**Values & operators**

- Strings are Latin-1 / UTF-16 code units (§5.2): `.length`, indexing,
  comparison and every guest `String.prototype` method are exact for any
  text, lone surrogates included. `localeCompare` is code-unit order, not
  locale collation, and the `toLocale*Case` methods use no locale data; a
  key with a lone surrogate cannot name a function (a method, accessor or
  anonymous function under it is a compile error: function names are host
  text); there are no ropes, so `s = s + x` in a loop is `O(n^2)`. `search` / `match` / `matchAll` interpret a string
  pattern as RegExp source: a pattern with RegExp syntax throws a
  `TypeError`, and `matchAll` (a RegExp string iterator) always throws one.
- No `void`.
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
- Not available: `RegExp` (literals and constructor), `BigInt`, `Proxy`, `Reflect`,
  `WeakMap`/`WeakSet`/`WeakRef`, typed arrays, `ArrayBuffer`, `Intl`, `eval`.
  The `Function` constructor throws `TypeError`.
- The builtin library is exactly §7.9.
- The `Math` transcendental functions follow the host libm (Rust) or the JS
  engine (shim), which may differ in the last ulp.

**Object model**

- Property tables are linear-scanned; there are no inline caches.
- No object spread (`{...o}`); object patterns accept only identifier and
  string keys; `catch` parameters must be identifiers.
- Array-like lengths are clamped to `2^32 - 1` by the generic loops of `map`,
  `filter`, `forEach`, `reduce`, `join`, `concat` and `flat` (`map` sizes its
  `ArraySpeciesCreate` by the full length, so past `2^32 - 1` it throws the
  `RangeError` first); every other `Array.prototype` method uses the full
  `ToLength` range.
- `Array.prototype`, `String.prototype` and `Number.prototype` are ordinary
  objects, not an array / `String` / `Number` exotic object:
  `Array.isArray(Array.prototype)` is `false`, `Array.prototype.length` is
  `undefined`, `String.prototype.valueOf()` throws `TypeError`, and
  `Object.prototype.toString.call(Number.prototype)` is `"[object Object]"`.
- The remaining test262 failures of the phase-3 built-in families (after
  element reads through the prototype chain, L2-V T3, and `var` hoisting /
  global declarations, L2-V T1: 514 of built-ins/{Array 174, String 191,
  Function 135, Math 9, Number 5}; NativeErrors and AggregateError have
  none), by cause and owner:

  | cause | owner | Array | String | Function | Math | Number |
  |---|---|---|---|---|---|---|
  | `RegExp` | never in phase 3 | 26 | 95 | 3 | | |
  | `Function` constructor | never in phase 3 | | 3 | 117 | | |
  | `eval` / `Proxy` / `BigInt` / `ArrayBuffer` | never in phase 3 | 16 | 12 | 3 | 1 | |
  | builtin prototypes are not exotic (above) | object model | 4 | 3 | | | 4 |
  | `asyncTest` used by a test without the `async` flag | L2-R (runner) | 67 | | | | |
  | `nativeFunctionMatcher.js` harness | L2-R (runner) | | | 1 | | |
  | `void` | L2-L | 5 | 36 | 7 | | 1 |
  | tagged templates, `yield*` in async generators, `for-in` heads | L2-L | 4 | 4 | | | |
  | `Symbol` / `@@toStringTag` | Y1–Y4 (landed; recount at the next baseline) | 24 | 7 | 1 | | |
  | iterator methods (`keys` / `values` / `entries`, `Array.fromAsync`, `Math.sumPrecise`) | D3 | 19 | | | 8 | |
  | UTF-16 strings (non-ASCII positions, `isWellFormed` / `toWellFormed`) | U1/U2 (landed; recount at the next baseline) | | 29 | | | |
  | strict `caller` access (`15.3.5.4_2-11gs`, `caller/prop-desc`) | — | | | 2 | | |
- An array in dictionary mode (§5.4) keeps every element in its linear-scan
  property table, so element access on a large frozen or attributed array is
  `O(n)`; the renderers print an accessor element as `undefined` and omit a
  non-enumerable one.
- `Object.defineProperties` (and `Object.create`'s `Properties`) reads the
  enumerable own keys of its `Properties` argument once, before reading any
  descriptor.
- `.stack` on errors is `"Name: message"` with no frames.

**Functions & classes**

- Destructuring assignment works only as an expression statement; it is
  rejected in expression position (`x = [a] = b`), and a `for-of` head must
  declare its binding or be a pattern (`for (a of xs)` is rejected).
- Not supported: private `#` members, static blocks.
- An `arguments` object has no own `callee` (the strict-mode
  `%ThrowTypeError%` accessor is absent): `arguments.callee` reads
  `undefined`.
- A numeric or BigInt literal class member key (`class { 1() {} }`) is a
  compile error.

**Generators & async**

- No `for await` and no `Symbol.asyncIterator`; `yield*` is rejected in an
  async generator.
- A `yield`/`await` that would carry a per-node compiler temporary across the
  suspend is a compile error (`super.m(yield x)`, `o[yield k]?.(yield v)`).
- `yield*` has no fast path for a clean generator delegate: each delegated
  step is a call to the cached `next` (or the `throw` / `return` method).
- An async generator object renders as `Object [Generator]`; functions
  render as `[Function: f]` whatever their kind.
- Thenables are not assimilated: `await t` and `Promise.resolve(t)` on a
  non-promise thenable yield `t` itself.

**Date**

- UTC only: no local time zone (the calendar-field constructor
  `new Date(y, m, ..)` reads its fields as UTC), and no string parsing
  (`new Date("…")` is `Invalid Date`).

**Runtime**

- The emitted module imports seven host functions from `env` (§6.1); it is not
  self-contained and cannot run without jsc's wasmtime host or the
  `@cdlab/jsc` shim. Runtime semantics are implemented three times (guest wasm,
  the Rust host, the TypeScript shim).
- No garbage collection (§5.7): memory only grows, up to 512 MiB, and
  exhaustion is not catchable. The intern table never shrinks.
- Map / Set backing arrays never shrink: `delete` and `clear` leave
  tombstones (§5.2), so set / delete churn grows the backing without bound
  (phase 4, `design/symbol-strings.md` Risk R6).

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

**Status: wave 3a complete** (M3 reached: 64.6 % of eligible tests pass).
3a delivered property descriptors and `Object` reflection, `bind`, the ES2025
`Array` / `String` / `Number` / `Math` methods, `AggregateError`, Date TimeClip,
the async test262 runner, and language completions pulled forward from phase 5
(comma, generator and async expressions / methods, async generators,
non-simple parameters, early errors, `var` hoisting and global bindings).
Wave 3c (`Symbol`, the iteration protocol incl. async iteration, Latin-1 /
UTF-16 strings) is in progress; its design is `design/symbol-strings.md`.
Its stage Y1 is done: `Symbol` (registry, wrappers, the 13 well-known
symbols), symbol-keyed properties, and computed class member names (pulled
forward from phase 5); the well-known symbol hooks throw until Y2 / Y4 (§13).
Its stage U1 is done: Latin-1 / UTF-16 strings with code-unit `.length`,
indexing and comparison, lossless literals, and the `String.prototype`
methods in the guest. Its stage U2 is done: the code-point string iterator,
full lossless case mapping, `normalize` in all four forms, and lossless
code-unit `localeCompare`.

- The remaining built-ins on the realm's prototypes: `Date` parsing and local
  time. `Object` reflection is done: property descriptors, integrity levels,
  array dictionary mode, every `Object` static and `Object.prototype` method
  of §7.9 (with `__proto__` and the legacy accessor methods); the test262
  runner includes `propertyHelper.js` tests.
- `Symbol` and a generic iteration protocol behind for-of, spread, and
  destructuring.
- Latin-1 / UTF-16 dual string representation with spec `.length`, indexing,
  and surrogate pairs, the string iterator, full case mapping and `normalize`
  (done in U1 / U2).

### Phase 4 — Runtime independence and memory

- Modules import only WASI (output and clock); promises and the microtask queue
  run in the guest; the Rust host semantics and the TypeScript shim are removed.
- A precise collector (mark-sweep, shadow-stack roots); catchable out-of-memory.
- `WeakMap`, `WeakRef`, `FinalizationRegistry`.
- Browsers run modules through a small WASI polyfill.

### Phase 5 — Language completeness

- Private fields and methods, static blocks, object spread.
- `for await` and async `yield*`, labeled statements, all remaining early
  errors.
- ES modules: `import`/`export` across files, bundled at compile time into one
  module.
- `RegExp`, `BigInt`, typed arrays, `ArrayBuffer`, `DataView`, `Proxy`,
  `Reflect`.

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
- WASI preview 1 or preview 2 as the module target (decide before phase 4).
