# jsc — Design

> A Rust-native **ahead-of-time (AOT) JavaScript compiler**. jsc parses JavaScript
> with swc, lowers it to an in-house IR, emits WebAssembly, and executes the module
> on wasmtime — no external toolchain at runtime.

This is the authoritative architecture spec. It covers the pipeline, the value and
memory model, the ABI, and the language as it grew from the MVP onward through the core language
feature waves (all implemented). Each wave has its own reference section in §8.

## Contents

- [1. Goal & MVP milestone](#1-goal--mvp-milestone)
- [2. Architecture: end-to-end pipeline](#2-architecture-end-to-end-pipeline)
- [3. Design decisions](#3-design-decisions)
- [4. Module & crate layout](#4-module--crate-layout)
- [5. Constraints (hard locks)](#5-constraints-hard-locks)
- [6. CI gates](#6-ci-gates)
- [7. Building a static release](#7-building-a-static-release)
- [8. Language reference](#8-language-reference)
- [9. Diagnostics (compile-time)](#9-diagnostics-compile-time)
- [10. Roadmap](#10-roadmap)

---

## 1. Goal & MVP milestone

A Rust-native AOT compiler for JavaScript. The first milestone compiles a
numeric-expression subset end to end and runs it, proving every pipeline stage is
wired.

**MVP acceptance — the one command that must work:**

```
jsc -e "40 + 2"
42
```

`jsc -e "<expr>"` parses the expression, compiles it to a wasm module exporting a
single function, runs that function on wasmtime, and prints the returned number.

MVP scope is exactly: number literals, the binary operators `+ - * /`, and
parenthesized grouping. Everything else (variables, control flow, functions, the
full jsval type model) is roadmap (§10).

**Success criteria (verifiable):**

- `jsc -e "40 + 2"` prints `42`.
- `jsc -e "6 * (1 + 6)"` prints `42` (precedence + parens).
- `jsc -e "9 / 2"` prints `4.5` (f64 division, not integer).
- All CI gates (§6) pass on the workspace.

---

## 2. Architecture: end-to-end pipeline

```
JS source ──▶ swc AST ──▶ in-house IR ──▶ wasm bytes ──▶ result
  (-e str)   (frontend)   (codegen)      (emit)          (run)
             swc_ecma_    enum IrNode    wasm-encoder     wasmtime
             parser
```

Five stages, one per pipeline hop:

1. **frontend** — hand the source string to `swc_ecma_parser`, get back an
   `swc_ecma_ast` `Expr`/`Module`. swc owns lexing and parsing (precedence,
   associativity, parentheses), so jsc never writes a parser.
2. **codegen** — walk the swc AST and build the in-house IR (`IrNode`). This is the
   AST → IR lowering.
3. **emit** — walk the IR and produce wasm bytes with `wasm-encoder`.
4. **run** — load the wasm bytes into wasmtime, instantiate, call the exported
   entry function, read its return value.
5. **CLI** — a thin `clap` shell that reads `-e <expr>`, drives stages 1–4, and
   prints the result. The `-e`/`--eval` arg sets `allow_hyphen_values = true` so a
   leading-minus expression (e.g. `jsc -e "-0"`, `jsc -e "-5 * 2"`) is taken as the
   value rather than parsed as an unknown flag.

The **IR is the stable seam** between frontend and backend: swc AST shapes never
reach `emit`, and wasm concerns never reach `codegen`. This is the single most
important boundary for the later feature waves (§10) — new language features are
added by extending `IrNode` and the two walks over it, not by touching swc or
wasmtime glue.

---

## 3. Design decisions

Three choices define jsc's shape:

- **The IR is the stable seam.** A Rust `enum IrNode` sits between the swc AST and the
  wasm backend — idiomatic, exhaustively matchable, compiler-checked. swc AST shapes
  never reach `emit`; wasm concerns never reach `codegen`. New language features are
  added by extending `IrNode` and the two walks over it, not by touching the parser or
  the wasmtime glue.
- **Emit wasm directly.** The backend produces a wasm module straight from the IR with
  `wasm-encoder` — no C or text intermediate. The result is a real, validatable module
  you can inspect with `--emit wasm`.
- **Embed the runtime.** `wasmtime` runs the module in-process, so there is no external
  toolchain at runtime — parse, compile, and execute all happen inside one binary.

Values are a single NaN-boxed `i64`, and all heap data lives in one bump-allocated
linear-memory arena (§8). This keeps the value model uniform and the
backend free of per-type carrier plumbing.

---

## 4. Module & crate layout

A Cargo workspace: a core library crate (`jsc-core`) plus a thin binary crate (`jsc`).

As the language subset grew (MVP → the exceptions wave), the three pipeline stages `codegen`,
`emit`, and `run` outgrew single files and were split — swc-style — into
**directory modules** with one submodule per concern and a `tests.rs` submodule
holding that stage's unit tests. The single-file stages (`frontend`, `ir`) and
`lib` keep their code in the `.rs` file but extract their unit tests into a
sibling `tests` submodule. Integration tests are reorganized by theme under each
crate's `tests/`, sharing a `tests/common/` helper module.

```
jsc/
├── Cargo.toml              # [workspace]; shared lint table, MSRV, edition, [profile.dist]
├── crates/
│   ├── jsc-core/           # library: the compiler pipeline
│   │   ├── src/
│   │   │   ├── lib.rs          # pipeline entry; tests.rs sibling
│   │   │   ├── frontend.rs     # swc parse wrapper; tests.rs sibling
│   │   │   ├── ir.rs           # IrProgram / IrNode types; tests.rs sibling
│   │   │   ├── codegen/        # AST -> IR, split by concern
│   │   │   │   ├── mod.rs          # driver + shared lowering state
│   │   │   │   ├── expr.rs         # expression lowering
│   │   │   │   ├── stmt.rs         # statement lowering
│   │   │   │   ├── op.rs           # operator mapping
│   │   │   │   ├── function.rs     # function / closure lifting
│   │   │   │   ├── capture.rs      # captured-binding analysis
│   │   │   │   ├── arity.rs        # call-arity handling
│   │   │   │   ├── scope.rs        # lexical scope / bindings
│   │   │   │   └── tests.rs        # codegen unit tests
│   │   │   ├── emit/           # IR -> wasm (wasm-encoder), split by concern
│   │   │   │   ├── mod.rs          # driver + module builder
│   │   │   │   ├── sections.rs     # wasm section assembly
│   │   │   │   ├── expr.rs         # expression opcode emission
│   │   │   │   ├── stmt.rs         # statement / control-flow emission
│   │   │   │   ├── numeric.rs      # f64 / numeric ops
│   │   │   │   ├── value.rs        # jsval encode helpers
│   │   │   │   ├── array.rs        # array heap ops
│   │   │   │   ├── object.rs       # object heap ops
│   │   │   │   ├── function.rs     # function bodies / call frames
│   │   │   │   ├── runtime.rs      # runtime helpers / host imports
│   │   │   │   └── tests.rs        # emit unit tests
│   │   │   └── run/            # wasmtime load + call, split by concern
│   │   │       ├── mod.rs          # run entry
│   │   │       ├── harness.rs      # wasmtime engine/instance harness
│   │   │       ├── host.rs         # __convert / __method host imports
│   │   │       ├── value.rs        # jsval decode + JS-style render
│   │   │       └── tests.rs        # run unit tests
│   │   └── tests/             # integration tests by theme
│   │       ├── common/mod.rs      # shared compile-and-run helpers
│   │       ├── arrays.rs  console.rs  conversions.rs  errors.rs
│   │       ├── closures.rs  functions.rs  math.rs  strings.rs
│   │       ├── regression.rs      # cross-stage regression
│   │       └── unsupported.rs     # rejected-syntax guards
│   └── jsc/                # binary: clap shell, `jsc -e`
│       ├── src/main.rs
│       └── tests/             # CLI integration tests by theme
│           ├── common/mod.rs      # shared CLI-invocation helpers
│           ├── cli_modes.rs  arrays.rs  builtins.rs  closures.rs
│           ├── control_flow.rs  errors.rs  functions.rs  numbers.rs
│           ├── objects.rs  operators.rs  strings.rs
│           └── regression.rs
├── DESIGN.md               # this document
└── docs/                   # plan + task tracking
```

**IR (MVP subset).** For the MVP the IR represents a folded arithmetic expression
tree over f64:

```rust
enum IrNode {
    Num(f64),                                          // number literal
    Binary { op: BinOp, lhs: Box<IrNode>, rhs: Box<IrNode> },
}
enum BinOp { Add, Sub, Mul, Div }
```

- **All numbers are f64.** JavaScript's number type is IEEE-754 double, so `9 / 2`
  is `4.5`. This maps one-to-one to wasm's `f64` value type, making `emit` trivial.
- **Parenthesization is not an IR node.** swc resolves grouping and precedence
  during parsing, so the AST tree shape already encodes it.
- **Constant folding is optional** for the MVP — emitting `f64.add` over two
  `f64.const` operands produces the same result at run time.

**Wasm emission (MVP).** One function `main` with signature `() -> f64`; its body is
a post-order walk of the IR emitting `f64.const` / `f64.add` / `f64.sub` /
`f64.mul` / `f64.div`, then `end`. Exported as `"main"`. No memory, no imports, no
globals — sections used: Type, Function, Export, Code. `run` calls `main`, receives
an `f64`, and the CLI formats it JS-style (integral doubles print without `.0`).

---

## 5. Constraints (hard locks)

Hard acceptance gates, not preferences:

- **Toolchain.** MSRV **1.96**, Rust **edition 2024** (pinned via
  `rust-toolchain.toml`).
- **Safety.** `#![forbid(unsafe_code)]` in every crate.
- **Lints.** Deny warnings via the workspace-inherited `[workspace.lints]` table
  (`warnings = deny`; `-D` is not needed at the clippy call site). No panics in
  runtime paths (`unwrap_used` / `expect_used` / `panic` denied).
- **swc from crates.io.** The frontend tracks the latest published majors
  (`crates/jsc-core/Cargo.toml`): `swc_ecma_parser = "42"`,
  `swc_ecma_ast = "26"`, `swc_common = "24"`. The workspace is self-contained
  and builds on any machine — no local checkout required.
- **Pure-Rust dependencies.** `wasm-encoder` (emit), `wasmtime` (run), `clap` (CLI).
- **Workspace layout** as described in §4.

---

## 6. CI gates

The full gate runs in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
on push and pull_request, pinned to toolchain **1.96.0**:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --workspace` (deny-warnings inherited from lints)
- `cargo nextest run --workspace`
- `cargo test --doc --workspace` (doctests)
- `cargo deny check` (supply chain)
- `cargo shear` (unused-dependency check)
- `typos`

Because swc is consumed from crates.io (§5), every gate — including the ones that
compile the workspace (clippy / nextest / doctests / shear) — runs unconditionally
on GitHub-hosted runners. A `release (musl, profile dist)` job also builds the
portable static binary (§7).

---

## 7. Building a static release

For a portable, self-contained binary, build against the musl target with the
optimized `[profile.dist]` profile (defined in the root `Cargo.toml`):

```
# One-time: add the musl target (not installed by default).
rustup target add x86_64-unknown-linux-musl

# Build the static, stripped, LTO-optimized jsc binary.
cargo build --profile dist --target x86_64-unknown-linux-musl -p jsc

# Result:
target/x86_64-unknown-linux-musl/dist/jsc
```

`x86_64-unknown-linux-musl` links statically against musl libc, producing a binary
with no dynamic-loader or glibc-version dependency — it runs on any x86-64 Linux.
The system may also need `musl-tools` (`musl-gcc`) for some transitive C shims.

**`[profile.dist]`** inherits `release`, then tightens for distribution:

| Setting | Value | Rationale |
|---|---|---|
| `inherits` | `"release"` | start from the release profile |
| `opt-level` | `3` | jsc embeds wasmtime — favor compiler throughput over shaving KB; `"z"` is the size-first alternative |
| `lto` | `"fat"` | whole-program link-time optimization |
| `codegen-units` | `1` | maximize cross-unit optimization (slower build, smaller/faster binary) |
| `strip` | `true` | drop symbols/debug info |
| `panic` | `"abort"` | no unwinding tables; runtime paths never unwind (panics are lint-denied) |

`opt-level = 3` (not `"z"`) is the deliberate choice: because jsc embeds the
wasmtime runtime, compiler execution speed matters more than the last few kilobytes,
and fat LTO + a single codegen unit + `strip` already keep the artifact compact.

A commented, `if: false` `release-musl` job in the CI workflow documents this build
as an artifact-producing step; enable it once the swc path deps (§6) are provisioned
on the runner.

---

## 8. Language reference

The language grew in numbered feature waves, each landed and documented as a
unit. The subsections below are the authoritative per-wave reference; all are
implemented and on `main`.

### Core language subset

The MVP compiled a single folded arithmetic expression. This wave grew that
into a small but real **numeric-program** language: multi-statement top-level
programs, local variables, control flow, and user-defined recursive functions —
all still in the single `f64` domain. The CLI now runs whole programs from
either `-e "<source>"` or a file argument (`jsc <file>`).

**Language features now supported:**

- **f64 numerics.** Number literals and the binary operators `+ - * /`, plus
  parenthesized grouping, exactly as in the MVP.
- **`let` / `const` + assignment.** Declarations bind an identifier to a wasm
  local; a later `x = expr` reassigns it. Reads and writes resolve to a local
  index during lowering.
- **`if` / `else`, `while`, and comparisons.** The comparison operators
  `< > <= >= === !==` produce a boolean encoded as `1.0`/`0.0`; `if`/`else` and
  `while` branch and loop on truthiness (nonzero is true).
- **Functions / recursion.** `function f(a, b) { .. return .. }` declarations and
  `f(x, y)` calls, including self-recursion (e.g. `fib`) and forward calls to a
  function declared later in the program.

**The numeric-domain decision (f64, no NaN-boxing).** Every value in this subset
is an `f64`. Booleans are `1.0`/`0.0`; a bare `return;` yields `NaN` standing in
for `undefined`. This is deliberate: it keeps the IR in one type, maps one-to-one
onto wasm's `f64`, and defers the NaN-boxed `jsval` type model (which unifies
number / boolean / null / undefined / pointer and unlocks strings and objects)
to a later wave (§10). Nothing in this subset needs memory, GC, or a type tag.

**How programs map to wasm.** The IR (`crate::ir`) now carries a program, not a
lone expression tree:

- **`IrProgram`** = the exported `main` entry function (top-level `stmts` plus its
  `locals` count) and a vector of user-defined `IrFunction`s.
- **Locals.** Each `let`/`const` and each parameter is a wasm `f64` local; the
  lowering counts them so `emit` can declare `(local f64 …)` up front. Reads emit
  `local.get`; an assignment emits `local.tee` (store and yield the value).
- **Structured control.** `IrStmt::If` lowers to a wasm structured `if`/`else`
  block and `IrStmt::While` to a `block`+`loop` pair with a conditional
  `br_if`/`br` — no raw jumps, matching wasm's structured control flow.
- **Functions.** Each `IrFunction` becomes its own wasm function with signature
  `(f64 … ) -> f64`; `main` is function index `0` and exported as `"main"`, and
  user functions take indices `1..=n`, so a `Call` is a direct `call` to a known
  index. Arguments are evaluated left-to-right onto the stack; the callee's `f64`
  result is left in place. `run` still just calls the exported `main` and reads
  back one `f64`.

Sections used by `emit` grow accordingly (Type, Function, Export, Code); there is
still no memory, no imports, and no globals.

### The NaN-boxed value model (`jsval`)

The base subset kept every value a raw `f64`. This wave replaces that single
numeric domain with a **NaN-boxed `jsval`**: `number`, `boolean`, `null`, and
`undefined` become distinct first-class values that coexist in one 64-bit word,
flow through the whole pipeline, and are decoded back to their real JS type in
`run.rs`. This unlocks `typeof`, real truthiness, and strict equality over mixed
types — none of which an f64-only value could express.

**Bit layout.** A `jsval` is a 64-bit value. `QNAN = 0x7FF8_0000_0000_0000`
(exponent all ones + the quiet bit). A value is a **NUMBER** iff
`(bits & QNAN) != QNAN`, or it is the canonical number-NaN (`0x7FF8_0000_0000_0000`,
tag `0b000`); numbers are stored as raw `f64` bits (`f64::to_bits(x)`). A boxed
non-number sets `QNAN` plus a 3-bit tag in payload bits `[50:48]`:

| tag (bits 50:48) | type | encoding | notes |
|---|---|---|---|
| `0b000` | (reserved) number NaN | `0x7FF8_0000_0000_0000` | canonical NaN; still a NUMBER |
| `0b001` | boolean | `FALSE = 0x7FF9_0000_0000_0000`, `TRUE = 0x7FF9_0000_0000_0001` | value in bit 0 |
| `0b010` | null | `0x7FFA_0000_0000_0000` | |
| `0b011` | undefined | `0x7FFB_0000_0000_0000` | |
| `0b100` | typeof-result string immediate | `0x7FFC_0000_0000_0000 + index` | payload low bits = index into `TYPEOF_STRINGS` |

`TYPEOF_STRINGS = ["number", "boolean", "undefined", "object", "string",
"function"]`; the value model only produces indices `0..3`.

**The i64-carrier decision.** The wasm value type carrying a `jsval` is **`i64`**,
not `f64`. A NaN-boxed value lives in the NaN space of a double, and the wasm spec
permits an implementation to **canonicalize a NaN payload** both on `f64`
arithmetic and at the wasmtime return boundary — either of which would silently
rewrite the tag/payload and destroy the type. Carrying the value as `i64` is
bit-preserving (integer ops and the `i64` return path never canonicalize), so the
`jsval` bits survive intact. Numbers are still real doubles: arithmetic
reinterprets the `i64` operands to `f64` (`f64.reinterpret_i64`), computes with
`f64.*`, and reinterprets the result back to `i64` (`i64.reinterpret_f64`). The
exported entry signature therefore changes from `() -> f64` to `() -> i64` — the
one intentional contract change of the campaign.

**The `run.rs` Value seam.** `run.rs` decodes the returned `i64` bits into a typed
`Value` and formats it JS-style:

```rust
pub enum Value { Number(f64), Bool(bool), Null, Undefined, TypeStr(&'static str) }
pub fn decode(bits: u64) -> Value;        // per the layout above
pub fn format_value(v: &Value) -> String; // Number -> format_number; Bool -> "true"/"false";
                                          // Null -> "null"; Undefined -> "undefined"; TypeStr -> the word
pub fn compile_and_run(src: &str) -> anyhow::Result<Value>; // CLI prints format_value(&value)
```

`format_number` is reused for the `Number` arm (integral doubles print without a
trailing `.0`). It follows the ECMAScript `Number::toString` fixed-vs-exponential
thresholds: a value renders in exponential form (lowercase `e`, explicit sign,
no leading zeros in the exponent — e.g. `1e+21`, `1.5e+22`, `1e-7`) when its
decimal exponent puts it `>= 1e21` or in `0 < |x| < 1e-6`, and in fixed form
otherwise (`1e-6` => `0.000001`, `123` => `123`, `4.5` => `4.5`). The shortest
round-tripping digit string is recovered from Rust's `{:e}` shortest mantissa
(digits + `n = exp + 1`). The non-finite and zero cases bypass the digit path:
`NaN` => `"NaN"`, `±Infinity` => `"Infinity"`/`"-Infinity"`, and both `+0`/`-0` =>
`"0"` (JS `String(-0)` is `"0"`; the stored f64 sign is left intact so arithmetic
still sees `-0`, e.g. `1 / -0` => `-Infinity`). The bare `NaN` / `Infinity`
identifiers are read-only global number literals that lower to the canonical `f64`
constants.

**Semantics.** *Truthiness* — falsy is exactly `false`, `null`, `undefined`,
`0`/`-0`, and `NaN`; everything else is truthy. `if` / `while` conditions (and the
ternary condition, below) test truthiness by this rule instead of the base subset `!= 0.0`.
*Strict equality* (`===` / `!==`) is type-then-value: different types => `false`;
same type => compare by value (numbers numerically, with `NaN === NaN` **false**
and `+0 === -0` **true**; booleans bit-for-bit; `null`/`undefined` reflexively
equal). The result is a boxed boolean. *Conditional (ternary) `cond ? a : b`* —
`cond` is tested for truthiness and only the selected branch is evaluated,
yielding that branch's jsval.

**Documented simplifications.** Strings, objects, and arrays as real values are out
of scope (the string type onward); they need linear memory + an arena + GC. `typeof` returns a
**boxed type-string immediate** (tag `0b100`, an index into `TYPEOF_STRINGS`) —
*not* a heap string, and not a general string type. All four in-scope types
(`number`, `boolean`, `null`, `undefined`) are **immediate** — The value model introduces **no
linear memory** anywhere in the pipeline. Loose equality (`==`) and JS coercion
(`ToNumber`, `+` string concat) are likewise deferred to the string type onward, as is the
logical-not operator `!` (truthiness is reached only through `if` / `while` and
the ternary condition in the base numeric subset).

### Strings: the first heap type

The base numeric subset kept every `jsval` an **immediate** — number, boolean,
`null`, `undefined`, and the `typeof` type-word all fit in the 64-bit word. The
This adds the **first heap-allocated type: the string**. A string does not fit
in 64 bits, so this wave introduces **linear memory** into the emitted module, a new
NaN-box **pointer** variant (tag `0b101`) whose payload is a 32-bit byte offset
into that memory, compile-time **data segments** for string literals, and a
runtime **bump allocator** for strings built at run time. The `jsval` carrier
(`i64`) and the pipeline shape are unchanged; the change is that a `jsval` can now
be a **pointer** into a heap, and the host reads that pointer back into text.

**Linear memory.** The module gains one Memory, `min = 1 page` (64 KiB), **no
max**, exported as `"memory"` so the host (`run.rs`) can read string bytes back
*after* `main` returns. The module never emits `memory.grow`; programs are assumed
to stay within one page, and **heap overflow beyond one page is undefined** in
this learning subset — there are **no bounds checks**. Offset `0` is reserved as a
null guard; static literal data begins at offset `8`.

**String representation.** A string lives in memory as a length-prefixed UTF-8
blob: a little-endian `u32` `len` at the pointer, followed by exactly `len` UTF-8
bytes.

```text
ptr        ptr+4
┌──────────┬──────────────────────────┐
│ len:u32  │ utf8 bytes (len of them)  │
└──────────┴──────────────────────────┘
```

`len` and `.length` use the **byte** length (ASCII-first: for ASCII input this
equals JS `.length`). JS **UTF-16** `.length` semantics and non-ASCII correctness
are **deferred** — a documented simplification.

**The pointer jsval variant.** This adds one new NaN-box tag, `0b101` (the next
free tag after the value model's typeof-string `0b100`), marking a heap pointer.
`POINTER_BITS = 0x7FFD_0000_0000_0000` (QNAN set plus tag `0b101`); a string jsval
is `QNAN | (0b101 << 48) | (ptr & 0xFFFF_FFFF)` — the 32-bit byte offset in the
low bits. The tag table extends the value model's:

| tag (bits 50:48) | type | encoding | notes |
|---|---|---|---|
| `0b000` | (reserved) number NaN | `0x7FF8_0000_0000_0000` | canonical NaN; still a NUMBER |
| `0b001` | boolean | `FALSE = 0x7FF9_0000_0000_0000`, `TRUE = 0x7FF9_0000_0000_0001` | value in bit 0 |
| `0b010` | null | `0x7FFA_0000_0000_0000` | |
| `0b011` | undefined | `0x7FFB_0000_0000_0000` | |
| `0b100` | typeof-result string immediate | `0x7FFC_0000_0000_0000 + index` | payload low bits = index into `TYPEOF_STRINGS` |
| `0b101` | **heap string pointer** (NEW) | `0x7FFD_0000_0000_0000 + (ptr & 0xFFFF_FFFF)` | payload low 32 bits = byte offset into linear memory |

`typeof` a pointer → index **4** (`"string"`) in `TYPEOF_STRINGS` (the value model already
reserved it). A string is the only heap type so far; objects and arrays (which
would reuse the pointer mechanism with different payload layouts) are the object & array types onward.

**Static literal data + bump allocator.** String **literals** are emitted as
active wasm **data segments** at compile-time-assigned offsets (each
`[len][bytes]`, packed from base offset `8`); a literal lowers to a pointer jsval
with no run-time allocation. Strings built at run time (concat, indexing) come
from a **bump allocator**: a mutable wasm global `__heap_ptr: i32` initialized to
`HEAP_BASE` (the first free offset after all static literal data) and a helper
`__alloc(size: i32) -> i32` that returns the current `__heap_ptr` and bumps it —
**no free, no GC**. To keep user-function wasm indices stable (`main` is index
`0`, user functions `1..=n`), the five heap helpers — `__alloc`, `__concat`,
`__str_eq`, `__str_cmp`, `__str_index` — are appended (in that order) as the
**last** wasm functions, after all user functions; `__concat` and `__str_index`
call `__alloc`.

**The `run.rs` boundary change.** the value model's `run` returned only the raw `i64` bits;
The string type must also hand the host the linear-memory bytes, since a pointer jsval is only
meaningful against the memory it indexes. `run` reads the exported `"memory"` and
returns the bytes alongside the result bits; `decode` gains the memory as a
parameter and a pointer arm that reads `[len][bytes]` into a new `Value::Str`;
`format_value` prints a `Str` as its raw text **without** surrounding quotes
(console style). Every the value model immediate arm is unchanged.

```rust
pub enum Value { Number(f64), Bool(bool), Null, Undefined, TypeStr(&'static str), Str(String) }
pub fn decode(bits: u64, memory: &[u8]) -> Value; // pointer arm reads [len][bytes]
pub fn format_value(v: &Value) -> String;         // Str -> raw text, no quotes
```

**Operations.** *`.length`* loads the `u32` at `ptr` and reboxes it as a number.
*Concatenation & `+` coercion* makes `+` a runtime type check: if **either**
operand is a string, the **other** is coerced to its string form (`ToString`:
`"x" + 8` → `"x8"`, `8 + "x"` → `"8x"`, `"x" + true` → `"xtrue"`, `"x" + null` →
`"xnull"`) and the two are concatenated (`__alloc(len1 + len2 + 4)`, copy, write
header → new pointer); when neither is a string it is the numeric add — so
`40 + 2` still yields `42`. The full numeric `ToPrimitive`/`ToNumber` path is
**not** implemented: `+` with a non-string operand like `null` or a boolean is
**not** coerced to a number (`1 + null` does not fold to `1`) — a documented
boundary. *Indexing `s[i]`* allocates a fresh one-char string; an out-of-bounds
index (`i >= length` or negative) reads `undefined`. *Strict `===` / `!==`* compare pointer
operands by byte content (`__str_eq`); *relational `< > <= >=`* compare
lexicographically by bytes (`__str_cmp`); the immediate cases keep the value model paths.
*String truthiness* — `""` (len `0`) is falsy, any non-empty string is truthy.

**Documented simplifications.** One page, no `memory.grow`, no bounds checks
(overflow is undefined); byte-length `.length` (UTF-16 / non-ASCII deferred); a
bump allocator with no free and no GC. Objects and arrays, template literals,
string methods beyond `.length` / indexing, loose equality (`==`), and the
logical-not operator `!` are all deferred to **the object & array types onward**. The `main` export signature
stays `() -> i64`; the string type *adds* the exported `"memory"` — the one intentional
contract addition of the campaign.

### Objects & arrays

This wave added the first heap type, the **string**, backed by linear
memory, a bump allocator (`__alloc`), and a NaN-box **pointer** jsval (tag
`0b101`). This adds the **second and third heap types: the array and the object**,
reusing that machinery unchanged — the same memory, the same allocator, the same
boxed-32-bit-offset pointer form. Each new type gets its own NaN-box **pointer
tag** (`0b110` array, `0b111` object) and its own in-memory layout. The `jsval`
carrier (`i64`) and the pipeline shape are unchanged; the change is that a pointer
jsval can now refer to an array or an object as well as a string, and computed
access (`x[i]`) becomes a **runtime tag dispatch**.

**Reused the string type machinery.** No new memory, allocator, or `run.rs` plumbing beyond
the two new decode arms: one Memory (`min = 1 page`, no max, exported `"memory"`,
no grow, no bounds checks, offset `0` null guard); the bump allocator
(`__heap_ptr` global + `__alloc` — no free, no GC); and the pointer jsval form
`QNAN | (tag << 48) | (ptr & 0xFFFF_FFFF)`. Arrays and objects are built at run
time with the **same** `__alloc`.

**The two new pointer tags.** This adds `0b110` (array) and `0b111` (object), the
two remaining free tags after the string type's string `0b101`. With
`ARRAY_BITS = 0x7FFE_0000_0000_0000` and `OBJECT_BITS = 0x7FFF_0000_0000_0000`,
an array jsval is `ARRAY_BITS | (ptr & 0xFFFF_FFFF)` and an object jsval is
`OBJECT_BITS | (ptr & 0xFFFF_FFFF)` — the byte offset in the low 32 bits, exactly
as for the string pointer. The tag table extends the string type's:

| tag (bits 50:48) | type | encoding | notes |
|---|---|---|---|
| `0b000` | (reserved) number NaN | `0x7FF8_0000_0000_0000` | canonical NaN; still a NUMBER |
| `0b001` | boolean | `FALSE = 0x7FF9_0000_0000_0000`, `TRUE = 0x7FF9_0000_0000_0001` | value in bit 0 |
| `0b010` | null | `0x7FFA_0000_0000_0000` | |
| `0b011` | undefined | `0x7FFB_0000_0000_0000` | |
| `0b100` | typeof-result string immediate | `0x7FFC_0000_0000_0000 + index` | payload low bits = index into `TYPEOF_STRINGS` |
| `0b101` | heap string pointer | `0x7FFD_0000_0000_0000 + (ptr & 0xFFFF_FFFF)` | payload low 32 bits = byte offset |
| `0b110` | **heap array pointer** (NEW) | `0x7FFE_0000_0000_0000 + (ptr & 0xFFFF_FFFF)` | payload low 32 bits = byte offset into linear memory |
| `0b111` | **heap object pointer** (NEW) | `0x7FFF_0000_0000_0000 + (ptr & 0xFFFF_FFFF)` | payload low 32 bits = byte offset into linear memory |

`typeof` an array pointer and `typeof` an object pointer both → index **3**
(`"object"`) in `TYPEOF_STRINGS` (matching JS, where `typeof []` is `"object"`).
**All eight tag values `000..111` are now assigned — the 3-bit tag space is
exhausted.** A fourth heap type (the operator & scoping wave onward) cannot get a fresh tag; it must switch to a
**header-byte discriminator** inside the allocated blob (one reused tag for "heap
object", first byte selects the concrete kind). This is a documented boundary.

**Array layout.** An array is a length-prefixed run of `jsval` slots:

```text
ptr        ptr+4        ptr+12                   ptr+4+8*(len-1)
┌──────────┬───────────┬───────────┬─── … ───┬───────────────┐
│ len:u32  │ elem_0:i64│ elem_1:i64│         │ elem_{len-1}  │
│ (LE)     │           │           │         │      :i64     │
└──────────┴───────────┴───────────┴─── … ───┴───────────────┘
```

`len` is a little-endian `u32` at the pointer; element `k` is a full 8-byte
`jsval` at `ptr + 4 + 8*k` (unaligned `i64` load/store — wasm permits this with
`align=0`). Elements are full jsvals, so arrays nest. `.length` reads the `u32` at
`ptr` — the **same header read** the string type's `StrLen` uses, so `.length` is uniform for
strings and arrays. `arr[i]` read = `i64.load` at `ptr + 4 + 8*i` for an in-range
`i`; an out-of-bounds index (`i >= len` or negative) reads `undefined` rather than
loading past the block. `arr[i] = v` = `i64.store` at `ptr + 4 + 8*i`
(**existing index only** — growth is deferred). The
literal `[a,b,c]` lowers to `__alloc(4 + 8*n)`, write `len = n`, write each
element. `typeof []` → `"object"`; an array is **always truthy** (even `[]`).

**Object layout.** An object is a count-prefixed run of `(key, value)` entries,
each 16 bytes:

```text
ptr        ptr+4                       ptr+4+16                    ptr+4+16*(n-1)
┌──────┬─────────────┬───────────┬─────────────┬───────────┬─ … ─┬─────────────┬───────────┐
│ n:u32│ key_ptr_0   │ val_0     │ key_ptr_1   │ val_1     │     │ key_ptr_{n-1}│ val_{n-1} │
│ (LE) │   :i64      │   :i64    │   :i64      │   :i64    │     │    :i64      │   :i64    │
└──────┴─────────────┴───────────┴─────────────┴───────────┴─ … ─┴─────────────┴───────────┘
```

`n` is a little-endian `u32` at the pointer; entry `k` lives at `ptr + 4 + 16*k`,
its key jsval at `ptr + 4 + 16*k` and its value jsval at `ptr + 4 + 16*k + 8`.
Keys are the string type heap-string pointers; literal keys reuse the compile-time static
string data (the existing `collect_strings` interning), so key `a` shares its
`[len][bytes]` blob with any `"a"` literal. The literal `{a:1,b:2}` lowers to
`__alloc(4 + 16*n)`, write `n`, write each `(key_ptr, val)`. Property read
`obj.a` / `obj["a"]` linear-scans the `n` entries, comparing each stored key by
**byte content** via the string type `__str_eq` helper, and returns the matching value
jsval (or `undefined` if no key matches). Property assignment `obj.a = v` /
`obj["a"] = v` does the same scan and stores `v` into the matching entry's value
slot (**existing keys only** — adding a new key is a documented no-op /
undefined). `typeof {}` → `"object"`; an object is **always truthy** (even `{}`).

**Shared `x[i]` dispatch.** Computed access `x[i]` and computed assignment
`x[i] = v` share syntax with the string type string indexing, so the emitter cannot pick the
operation statically — it **runtime-dispatches on the pointer tag** of `x`:
string (`0b101`) → the string type `__str_index`; array (`0b110`) → element `i64.load` /
`i64.store`; object (`0b111`) → the object key scan (keyed on the string jsval).
The emitter branches with an `if`/`else` chain (not `select`), so only the
matching path is reached — the same trap-safety discipline the string type uses for its
pointer guard. New heap helpers (the object get/set scan loop) are appended
**after** the string type helper `__str_index`, keeping `main` = index `0`, user
functions `1..=n`, and the string type helper order (`__alloc`, `__concat`, `__str_eq`,
`__str_cmp`, `__str_index`) unchanged.

**The `run.rs` boundary change.** `decode(bits, &memory)` already threads the
exported memory. This adds an **array arm** (tag `0b110`) that reads `len` then
recursively decodes each element jsval, and an **object arm** (tag `0b111`) that
reads `n` then decodes each key (string pointer → `String`) and recursively
decodes each value jsval. `Value` gains `Array(Vec<Value>)` and
`Object(Vec<(String, Value)>)`; `format_value` prints them Node-console style
(`[ 1, 2, 3 ]`, `{ a: 1, b: 2 }`). The milestones only assert numeric / `typeof`
results, so exact print formatting is not milestone-tested.

```rust
pub enum Value {
    Number(f64), Bool(bool), Null, Undefined, TypeStr(&'static str),
    Str(String), Array(Vec<Value>), Object(Vec<(String, Value)>),
}
pub fn decode(bits: u64, memory: &[u8]) -> Value; // array + object arms recurse
pub fn format_value(v: &Value) -> String;         // [ .. ] / { .. } console style
```

**Documented simplifications.** Element assignment and property assignment target
**existing** slots only — growing an array, adding a new object key, `push`, and
`delete` are deferred; assigning through a missing index/key is a no-op /
undefined. The tag space is exhausted (a fourth heap type needs a header-byte
discriminator). Array/object methods (`map` / `keys` / …), prototypes,
`for-of` / `for-in`, spread, computed / shorthand object keys, `JSON`,
`Array.isArray`, loose equality (`==`), and the logical-not operator `!` are all
deferred to **the operator & scoping wave onward**. There is still no free and no GC. The exported `"memory"`
from the string type is reused, not re-added.

### Statements, operators, `const` immutability & block scoping

This wave rounded out the heap types (array, object) and **exhausted the
3-bit NaN-box tag space**. The operator & scoping wave turns the numeric-expression subset into a **small
imperative language** without touching the value representation: it adds the
**short-circuit logical operators** (`!`, `&&`, `||`, `??`), **compound assignment**
(`+=`, `-=`, `*=`, `/=`, `%=`) and **increment/decrement** (`++`/`--`, prefix +
postfix), the **C-style `for` loop**, **`switch`/`case`/`default`/`break`** with JS
fall-through, **lexical block scoping** (a fresh scope per `{}` block; `for`-`let`
block-scoped), and **`const` immutability** as a **compile-time error**. Every one
is a **frontend + codegen + emit lowering**: no new tag, no new heap layout, and —
unlike the string type/the object & array types — **no `run.rs` change** (no new `Value` variant, no new decode
arm). The `i64` jsval carrier and the whole early-waves pipeline is unchanged.

**Reused machinery (unchanged).** The NaN-boxed `i64` jsval (immediates + string
`0b101` / array `0b110` / object `0b111` pointers); the JS-truthiness lowering
(`emit_cond`) behind `if`/`while`/`?:`; the flat per-function wasm local frame; the
`block`/`loop`/`if`/`br`/`br_if` encoders; strict `===` (`BinOp::Eq`); and the
function-index order (`main`=0, user funcs `1..=n`, then the heap helpers
`__alloc`, `__concat`, `__str_eq`, `__str_cmp`, `__str_index`, `__obj_get`,
`__obj_set`). **This adds no helper function**, so every index is stable.

**Short-circuit logical lowering.** `!e` is eager: evaluate `e`, apply the
truthiness test, produce the **boolean** inverse (the operator deferred since the value model).
`&&`, `||`, `??` **evaluate the left operand exactly once** into a temp local `t`,
then branch — and the result is the **operand jsval**, not a coerced boolean:

```text
<eval a> ; local.tee t          ;; a into temp, value also available for the test
<test on t>                      ;; truthy(t) for && / ||   |   nullish(t) for ??
if (result i64)
  <yield t or b per operator>    ;; b (the right) is evaluated in exactly one arm
else
  <yield b or t per operator>
end
```

`a && b` yields `b` iff `truthy(t)` else `t` (`0 && x` → `0`, `1 && "s"` → `"s"`);
`a || b` yields `t` iff `truthy(t)` else `b` (`"" || "x"` → `"x"`); `a ?? b` yields
`b` iff `t` is **`null`/`undefined`** (a **nullish** test — `t === null ||
t === undefined`, *not* truthiness, so `0 ?? 5` → `0`) else `t`. The right operand
`b` sits in one branch arm, so it runs only when the operator does not
short-circuit; each short-circuit operator uses one temp local. The IR gains
`Not(Box<IrNode>)` and `Logical { op: {And,Or,Nullish}, lhs, rhs }`.

**Compound assignment + inc/dec.** `target op= e` desugars to `target = target op e`
with the **target read once**; `op ∈ {Add,Sub,Mul,Div,Mod}`, where **`BinOp::Mod`**
(`%`, `f64` remainder) is new. The **shipped** target set is a plain **local**
identifier: `local.get`/`op`/`local.tee`, reusing the `+` string/number overload so
`s += t` concatenates. **Compound assignment to a member target — `a[i] op= e` /
`o.k op= e` — is a documented deferral** (`Err("unsupported compound assignment
target")`; plain `=` member stores are unaffected): the read-once-into-temps
member-target lowering is left to the closures wave onward. `++x`/`--x` (prefix) yield the **new**
value; `x++`/`x--` (postfix) read `old`, write `old ± 1`, and yield **`old`** — also
local-identifier targets only. `op=` / `++` / `--` on a **`const`** target is a
compile-time error.

**C-style `for`.** `for (init; test; update) body` **desugars** to a `while`:

```text
{                       ;; for-scope: a for-let binding lives here only
  init ;                ;; a let/const Decl, an Expr, or empty
  while (test) {        ;; missing test => literal `true`
    body                ;; body is its own block scope
    update ;            ;; an Expr or empty; runs after body, before next test
  }
}
```

The `for`-init `let`/`const` is scoped to the loop (invisible after; no collision
with an outer name). Because the operator & scoping wave has **no `continue`** (deferred), the plain
`while` desugaring is exact — nothing can skip the `update`. This reuses the
existing `IrStmt::While` + `Decl`/`Expr`; no dedicated `For` IR node is required.

**`switch` — fall-through + `break`.** `switch (disc) { case v: … default: … }`
evaluates the discriminant **once** into a temp `td`, compares each `case` label to
`td` with **strict `===`**, and on the first match runs that case's statements
**and every subsequent case's statements in source order** (JS fall-through) until
a **`break`** or the switch end; `default` runs when no case matches. It lowers to
a wasm `block` (the break target) wrapping a compare-and-branch entry dispatch
followed by the straight-line, concatenated case bodies; each `break` is a `br` out
of that block. The IR gains `Switch { disc, cases: Vec<(Option<IrNode>,
Vec<IrStmt>)> }` (a `None` label = `default`) and a `Break` statement. `break` in
The operator & scoping wave is **switch-only** (loop `break`/`continue` are deferred).

**Lexical block scoping.** Block scope is a **compile-time name-resolution** model,
not a runtime frame. A flat per-function name table is replaced by a **scope
stack**: entering any `{}` block (function body, `if`/`else`, `while`/`for` body,
`for`-init, bare block) pushes a frame; leaving pops. A `let`/`const` declares its
name in the **innermost** frame and allocates a **fresh wasm local** (locals are
never reused across sibling blocks — the flat wasm frame just grows; a documented
simplification, no liveness reuse). A reference resolves by walking the stack
**innermost-first**, so an inner `let x` **shadows** an outer `x`; sibling blocks
at the same depth may reuse a name for **distinct** bindings. `let x=1; { let x=2; }
x` → `1`. A `for`-let binding lives in an enclosing for-scope popped at loop end, so
it does not escape the loop.

**`const` immutability.** Each binding records `mutable` in its scope frame
(`let` → true, `const` → false). Any assignment to a name — `x = e`, `x op= e`,
`++x`/`x++`/`--x`/`x--` — resolves the name and checks the flag; assigning to a
`const` binding is a **compile-time error** (the frontend/codegen returns
`Err("assignment to constant variable")` and **no wasm is emitted**). `const`
without an initializer is a compile error. `const` object/array **contents** stay
mutable — `const o = {}; o.k = v` is allowed; only **rebinding** `o` errors —
matching JS.

**The `run.rs` boundary (unchanged).** the operator & scoping wave introduces **no new `Value` variant and
no new decode arm**. Every the operator & scoping wave program still returns one of the existing decoded
values (number, boolean, `null`, `undefined`, typeof-word, string, array, object);
`decode` and `format_value` are untouched. Unlike the string type and the object & array types, the operator & scoping wave does **not**
move the `run.rs` boundary — it is entirely a frontend + codegen + emit change.

**Documented simplifications / deferrals (the closures wave onward).** **compound assignment to a
member target** (`a[i] op= e` / `o.k op= e` — only local-identifier compound and
`++`/`--` shipped in the operator & scoping wave), `for-of` / `for-in`, labeled
statements and labeled `break`/`continue`, **`continue`** (excluded so the `for` →
`while` desugaring stays a plain loop — a `continue` must still run the `update`,
needing a labeled block/loop restructure), `do…while`, **TDZ** runtime error
semantics (the operator & scoping wave block scoping is compile-time name resolution only — no
use-before-declare trap), **closures capturing block scope** (no closures exist
yet), loose equality `==` / `!=` (`switch` uses strict `===`), the **comma
operator**, and **`**` exponentiation** (this wave adds `%` but not `**`) are all deferred
to the closures wave onward.

### First-class functions & closures

This wave turned the subset into a small imperative language without
touching the value representation. The closures wave makes **functions first-class values** and
adds **closures** — and to do so it must first **reclaim NaN-box tag space**, which
The object & array types exhausted. The three per-type heap tags (string `0b101`, array `0b110`, object
`0b111`) collapse into a **single heap-pointer tag** plus a **type-byte header** in
each heap block; that frees the tag budget for a new heap type — the **function
value** — and sets the pattern for the exceptions wave error objects. On that base the closures wave lands the
The operator & scoping wave-deferred **member-compound assignment** (`a[i] op= e`, `o.k op= e`), **function
values** (a `function`/arrow used as a value, passed as an argument, called through
a funcref **Table** with `call_indirect`), **arrow functions**, and **closures** (a
returned function that captures its enclosing bindings **by reference** through a
heap **environment of boxed cells**). The `i64` jsval carrier and the frontend →
codegen → emit → run pipeline shape are unchanged; the heap **encoding** moves from
"tag per type" to "one heap tag + header byte", and two module pieces are added (a
funcref Table + element segment, and the box/closure helpers).

**Reused machinery (unchanged carrier).** The NaN-boxed `i64` jsval with immediates
(number, boolean, `null`, `undefined`, typeof-word) — **unchanged**. One Memory
(`min = 1 page`, no max, exported `"memory"`, no grow, no bounds checks, offset `0`
null guard); the bump allocator (`__heap_ptr` + `__alloc` — no free, no GC); the
truthiness lowering (`emit_cond`); the operator & scoping wave scope stack + `const` check; strict `===`
(`BinOp::Eq`); and the function-index order (`main`=0, user funcs `1..=n`, then heap
helpers). Only the heap **pointer encoding** changes, and new module pieces are
appended.

**(a) The header-byte heap scheme.** Today a heap value's concrete type is carried
in the **NaN-box tag** (`0b101`/`0b110`/`0b111`) and the block begins directly with
the per-type payload; all eight 3-bit tags are assigned, so **the tag space is
exhausted**. This replaces the three per-type heap tags with a **single
heap-pointer tag** — `0b101` remains the sole "this jsval is a heap pointer" tag
(`HEAP_BITS = 0x7FFD_0000_0000_0000`; `0b110`/`0b111` retire to free/reserved) —
and moves the concrete kind into a **type byte at offset `0` of the heap block**.
Every block gains a header: byte `0` = type byte, bytes `1..4` = reserved padding,
so the existing per-type payload begins at **`HEADER_SIZE = 4`** (unchanged in
shape, just shifted):

```text
ptr        ptr+1   ptr+4  (= HEADER_SIZE)
┌────────┬────────┬──────────────────────────────────────────────┐
│ type:u8│ pad ×3 │ len:u32 / n:u32  +  payload (as the string type/the object & array types)       │
└────────┴────────┴──────────────────────────────────────────────┘
```

| type byte | kind | payload (from offset `HEADER_SIZE`) | user-visible |
|---|---|---|---|
| `0` | string | `len:u32` + `len` UTF-8 bytes | yes |
| `1` | array | `len:u32` + `len` × `jsval:i64` | yes |
| `2` | object | `n:u32` + `n` × `(key_ptr:i64, val:i64)` | yes |
| `3` | **function** (NEW) | `func_idx:u32` + `env:i64` | yes (`typeof` → `"function"`) |
| `4` | **closure env** (NEW, internal) | `count:u32` + `count` × `cell_ptr:i64` | no |
| `5` | **boxed cell** (NEW, internal) | `value:i64` | no |
| `6` | (reserved) **error object** — The exceptions wave | — | — |

Consequences are wide but mechanical: every allocation writes the type byte then
the payload (optionally via `__alloc_typed(type, payload_size)`); every
`len`/`n`/element/entry/char load-store shifts by `HEADER_SIZE` (string `len` at
`ptr + HEADER_SIZE`, array element `k` at `ptr + HEADER_SIZE + 4 + 8*k`, object
entry `k` at `ptr + HEADER_SIZE + 4 + 16*k`); `typeof`, the object & array types `x[i]` runtime
dispatch, and the `run.rs` decode dispatch on the **type byte** instead of the
NaN-box tag. **No user-visible behavior changes** — the refactor is validated
entirely by the base-subset-through-operators regression staying result-for-result green; it is the
campaign's highest-risk stage and ships first.

**(b) Function-value representation.** A function value is a **heap block, type byte
`3`**, holding a wasm function-table index and an environment pointer:

```text
ptr        ptr+HEADER_SIZE   ptr+HEADER_SIZE+4
┌────────┬────────┬───────────────┬───────────────┐
│ type=3 │ pad ×3 │ func_idx:u32  │ env:i64       │
└────────┴────────┴───────────────┴───────────────┘
```

`func_idx` indexes the module's new funcref **Table** (populated by an active
**element segment** listing every callable function); `env` points at the closure
**environment** (below) or is the **`null`** immediate for a non-capturing function.
A `function`/arrow literal lowers to `__alloc` of this block. `typeof` a function
value → `"function"`; a function value is **always truthy**. A `function foo(){}`
declaration is **both** directly callable by name (unchanged) **and** bound to a
function value in scope, so `let f = foo; f(…)` works.

Calls to a function **value** cannot resolve the callee statically, so they lower to
**`call_indirect`** against the Table. Since `call_indirect` needs one function type
but source functions vary in arity, the closures wave adopts a **uniform ABI**:

```text
(a0: i64, a1: i64, …, a_{K-1}: i64, env: i64) -> i64      ;; K = max arity in the program
```

Positional args are a fixed `K` slots (`K` = the **maximum arity across the
program's functions**, computed at emit time — a single per-program signature, not
a hard-coded constant; `K` ≥ every function's arity by construction) followed by a
**trailing `env`** slot (the closure environment, or `undefined` for a non-capturing
function). **Missing args are `undefined`; extra args are ignored** — matching JS and
letting one signature serve every function. **`env` reaches the callee as this
trailing argument, pushed by the *caller*:** a direct name-call pushes
`env = undefined` (a top-level declaration never captures); an indirect
`call_indirect` reads the callee's **own** `env` out of its function block
(`+HEADER_SIZE+4`) and pushes that. `env` is **last**, not first, so the callee data
(env + table index) is needed only *after* the arguments are on the stack — the
callee is evaluated once, its block base stashed in a scratch local, and env then
index read back-to-back, so a nested indirect call in an argument can never clobber
it. Inside a closure body `env` is read from wasm local `K` (the last parameter). One
body and one ABI serve every source function. Arrow functions (`(a,b)=>a+b`,
`x=>x*x`, expression- or block-bodied) and function expressions/nested function
declarations all lift to top-level functions and use the **same** representation and
ABI — they differ from a `function` value only in syntax (no `this`, no `arguments`;
both deferred to the built-ins wave).

**(c) Closure environment.** A function created inside another may **reference** the
enclosing bindings; when it **escapes** (is returned/stored) those bindings must
outlive the enclosing call. The closures wave captures **by reference** through a heap
**environment of boxed cells**. A compile-time **capture analysis** over the operator & scoping wave
scope stack marks a binding referenced from an **inner** function as **captured**;
a captured binding is promoted to a **boxed cell** — heap block type `5`,
`[type=5][pad][value:i64]` (`__cell_get`/`__cell_set`) — while non-captured bindings
stay flat wasm locals (zero overhead for the common case). When a capturing function
literal is evaluated, the enclosing function builds a **closure env** — heap block
type `4`, `[type=4][pad][count:u32][cell_ptr_0]…[cell_ptr_{count-1}]` — holding the
**cell pointers** for the captured bindings, and stores it in the function value's
`env` slot. Inside the closure body a captured name resolves to `env[slot] → cell →
value`. Because the env holds **cell pointers** and both the enclosing function and
the closure read/write the **same** cells, capture is **by reference**: the returned
`counter` and any other holder share the one `c` cell, so `n(); n(); n()` yields
`3`. Cells and envs are bump-allocated and never freed (no GC — a closure-heavy
program can exhaust the one page; undefined, as elsewhere).

**Member-compound assignment (the operator & scoping wave deferral).** `a[i] op= e` / `o.k op= e` (and
member `++`/`--`) land with the **object + index/key read once**: eval `a`,`i` (or
`o`) into temps, load the slot (array `i64.load` at `ta + HEADER_SIZE + 4 + 8*ti`;
object via `__obj_get`), combine with `e` via `op ∈ {Add,Sub,Mul,Div,Mod}`, store
back (array `i64.store`; object `__obj_set`); prefix yields the new value, postfix
yields `old`. Existing index/key only; `const` **rebinding** still errors while
member mutation of a `const`-bound object/array is allowed.

**The `run.rs` boundary (moves once).** `decode(bits, memory)` recognizes the single
heap tag, reads the **type byte** at `ptr`, and dispatches string (`0`) / array (`1`)
/ object (`2`) — each shifted by `HEADER_SIZE` — plus a new **function** arm (`3`);
env/cell type bytes (`4`/`5`) are internal and never appear as a top-level value.
`Value` gains a unit `Function` variant printed Node-console style; the shipped
decoder does not recover a function's source name, so it always prints
`[Function (anonymous)]`. Every immediate arm is unchanged.

```rust
pub enum Value {
    Number(f64), Bool(bool), Null, Undefined, TypeStr(&'static str),
    Str(String), Array(Vec<Value>), Object(Vec<(String, Value)>),
    Function,          // NEW — printed as [Function ...]
}
pub fn decode(bits: u64, memory: &[u8]) -> Value; // heap arm reads the type byte
pub fn format_value(v: &Value) -> String;         // Function -> "[Function ...]"
```

**Documented simplifications / deferrals (the built-ins wave onward).** `this` / method calls
(`o.m()`, `this` binding), **default parameters**, **rest / spread**, **generators**
(`function*`, `yield`), **async / await**, `Function.prototype.{bind,apply,call}`,
the **array higher-order methods** `map` / `filter` / `reduce` (expressible once
functions are first-class, but the method surface is the built-ins wave onward), **loose equality**
`==` / `!=`, **`**` exponentiation**, and **`for-of` / `for-in`** are all deferred
to the built-ins wave onward. The closures wave gives functions identity, arity (missing → `undefined`, extra →
ignored), and lexical capture — but no receiver binding, no argument adapters, and
no iteration protocols. There is still no GC.

**Boundary hardening — how each deferred construct fails.** Integration
hardening pinned the out-of-scope boundary so a deferred construct never silently
produces a wrong value. Most fail at **compile time** with a clear message:
`this`, method-shorthand object properties (`{ m(){} }`), default parameters
(`a = 1`), rest parameters / spread (`...`), `for-of` / `for-in`, `==` / `!=`, and
`**` were already rejected by the lowering; **`async` and generator (`function*`)
functions** were *not* — the lowering ignored the flags and compiled the body as an
ordinary function (an `async` body must return a `Promise`, a `function*` a
generator), so this adds an explicit reject at every function-building site
(declaration, function expression, arrow). `Function.prototype.{bind,apply,call}`
fail at **run time** (a `.bind`/`.call` member read off a function value traps
rather than mis-dispatching). **Remaining known gap:** the array higher-order
methods `map` / `filter` / `reduce` / `forEach` are *not* cleanly rejected — a
named-property read on an array/string (`a.map`, `a.foo`) reuses the object & array types `x[i]`
runtime dispatch, which coerces the string key and returns garbage instead of the
JS `undefined`, so `a.map(f)` mis-evaluates. A clean fix belongs to the object & array types array
runtime (there is no static type to reject `a.map` at compile time without also
rejecting valid object reads `o.map`), so it is deferred with the rest of the
the built-ins wave onward method surface rather than patched here.

### Built-in functions & the runtime

This wave made **functions first-class values** without touching the value
representation. The built-ins wave gives the subset a **standard-library surface and real program
output**: `console.log` as a wasmtime **host import** (so a program's output no longer
rides on its final-expression value), the `Math` numeric functions + constants, the
global type **conversions** (`String`/`Number`/`Boolean`/`parseInt`/`parseFloat`),
**growable arrays** (a `realloc` on the bump allocator + `push`/`pop`), the **array
higher-order methods** `map`/`filter`/`reduce`/`forEach` (invoking a jsval function
callback through the closures wave `call_indirect` **uniform ABI** — this **fixes** the
The closures wave-documented gap where `a.map` mis-evaluated through the `x[i]` dispatch), and the
remaining everyday **array/string methods**. The `i64` jsval carrier, the closures wave
header-byte heap encoding, and the frontend → codegen → emit → run pipeline shape are
all **unchanged**; it adds **one host import** (`console.log`), a family of runtime
`__`-helpers, and a **`realloc`** on the allocator, and teaches the lowering to
**recognize built-in calls** at the call/member sites.

**Reused machinery (unchanged carrier + heap encoding).** The NaN-boxed `i64` jsval
and the closures wave single-heap-tag + type-byte header (`HEADER_SIZE = 4`; type bytes `0`
string, `1` array, `2` object, `3` function, `4` env, `5` cell) — **unchanged**. The built-ins wave
adds **no** new NaN-box tag and **no** new heap type byte: growable arrays reuse type
byte `1`, callbacks are ordinary type-`3` function values, and `console.log` renders
the existing decoded `Value` set. The funcref Table + `call_indirect` uniform ABI
(`(a0..a_{K-1}, env) -> i64`, `env` **last**), the bump allocator, the truthiness
lowering (`emit_cond`), the coercion arms (`+`/`ToNumber`/`ToString`), and the operator & scoping wave/the closures wave
scope stack are all reused; the only structural change under the hood is the allocator
gaining a **`realloc`** (still bump-only, no free).

**(a) `console.log` host import.** Through the closures wave a program's observable result is its
**final expression**, decoded once at the `run.rs` boundary. This adds `console.log` as
a wasmtime **host import** — the first time the guest calls back into the host — so
**output** is decoupled from the returned value. The module imports `"console"`.`"log"`;
`console.log(a, b, …)` writes its `argc` jsvals into a memory region and calls
`$console_log(argc:i32, args_ptr:i32)`; the wasmtime `Linker` satisfies it with a host
closure over the guest `Memory` that reads `argc` `i64` jsvals at `args_ptr`,
**`decode`s + `format_value`s each with the same functions the boundary uses**, joins
with a single space, and writes `line + "\n"` to stdout:

```text
console.log(40+2)     => prints  42
console.log("hi")     => prints  hi          (string UNQUOTED)
console.log(1,2,3)    => prints  1 2 3       (space-separated)
console.log(true,null)=> prints  true null
```

Numbers render shortest-round-trip, strings **unquoted**, `true`/`false`/`null`/
`undefined` literally, arrays/objects in `format_value` shape, functions
`[Function (anonymous)]`. `console.log` evaluates to `undefined`. A program may print
many times and end on any (or no) value; the CLI keeps printing the decoded final
value for expression programs, but the built-ins wave tests that assert **stdout** drive it through
`console.log`.

**(b) `Math`, conversions.** `Math` is a **built-in namespace recognized at the
member/call site**, not a heap object (and **not** a passable value). `Math.floor`/
`ceil`/`round`/`trunc`/`abs`/`sqrt` lower to the wasm `f64` ops (`round` = `floor(x +
0.5)` for JS round-half-**up**, unlike wasm `f64.nearest`'s round-half-to-even);
`Math.min`/`max` fold `f64.min`/`max` over the `ToNumber`-coerced args seeded from
the identity infinity (so `Math.min()` => `Infinity`, `Math.max()` => `-Infinity`,
and any `NaN` argument propagates); `Math.pow` →
a runtime `__math_pow` helper (no wasm `f64.pow` opcode); `Math.PI`/`Math.E` emit
`f64.const` at the member-read site. The global **conversions** are recognized by
callee name over the existing coercion arms: `String(x)` = `ToString` → a **string
jsval** (so `typeof String(42)` is `"string"`), `Number(x)` = `ToNumber`
(`Number("42")` => `42`, `Number(true)` => `1`, non-numeric → `NaN`), `Boolean(x)` =
The value model truthiness, `parseInt(s)` = a leading **base-10** integer off the string-coerced
arg (`parseInt("42px")` => `42`), `parseFloat(s)` = a leading decimal. All three of
`String`/`Number`/`Boolean` route through the **same** helpers as `+`/`typeof`/
truthiness so conversions and coercion agree.

**(c) Growable arrays.** the object & array types arrays are fixed-length. The built-ins wave extends the allocator with
`__realloc(ptr, old_size, new_size)` — bump a **fresh** block, `memory.copy` the old
contents, return the new pointer; the old block is **abandoned** (still no free, no
GC). `Array.prototype.push(v)` grows the block to `HEADER_SIZE + 4 + 8*(len+1)` (exact
one-slot growth), writes `v` at element `len`, stores the new `len`, and — because the
realloc **changes the array pointer** — writes the possibly-new pointer back into the
array's **binding** (local or boxed cell), returning the new length. `pop()` reads the
last element and lowers `len` (no realloc). So `let a=[1,2]; a.push(3); a.length` => `3`
and `a[2]` => `3` through the same binding.

**(d) Higher-order array methods (fixing the closures wave gap).** `map`/`filter`/`reduce`/
`forEach` are the first **runtime** clients of the closures wave's first-class functions: each
receives a **function-value jsval** callback and invokes it through the closures wave
`call_indirect` uniform ABI. This is the stage that **fixes** the closures wave-documented gap
where a named-property read on an array (`a.map`) fell through the object & array types `x[i]` runtime
dispatch and mis-evaluated (the boundary-hardening rules). A helper calls the callback `cb`
**exactly like the closures wave's indirect call** — read `func_idx` (`cb+HEADER_SIZE`) and `env`
(`cb+HEADER_SIZE+4`), push the element then `env` (**env last**), pad to `K` with
`undefined`, `call_indirect`. Callbacks receive `(element)` only (the `(index, array)`
extra params are the exceptions wave onward). `map` builds a new same-length array of `cb(x)`; `filter` a new
(growable) array of the elements where `cb(x)` is truthy; `reduce(cb, init)` folds
`acc = cb(acc, x)` from the **required** `init`; `forEach` calls `cb(x)` for side
effects and returns `undefined` (`let s=0; [1,2,3,4].forEach(x=>{s=s+x}); s` => `10`,
the callback mutating a captured the closures wave cell). Recognition is **call-position only**, so a
valid object property `let o={map:1}; o.map` still reads `1`.

**(e) Array & string methods.** The everyday non-callback methods, recognized as
built-in method calls on an array/string receiver and lowered to runtime helpers over
the closures wave heap layout. Array: `indexOf(v)` (strict `===` scan, `-1` if absent),
`includes(v)`, `join(sep)` (`ToString` each element + the string-coerced `sep`
between), `slice(a,b)` (a new array, negative-index/clamp). String (**ASCII**,
byte-indexed): `toUpperCase`/`toLowerCase` (map the `a–z`/`A–Z` bytes), `indexOf(sub)`
(byte offset or `-1`), `slice(a,b)` (a new string, negative-index/clamp), `charAt(i)`
(a one-char string or empty). `[1,2,3].join("-")` => `"1-2-3"`; `"HELLO".slice(1,3)`
=> `"EL"`.

**The `run.rs` boundary (one addition).** This adds **no** `Value` variant and **no**
decode arm — every the built-ins wave value is an existing the closures wave kind. The boundary changes in exactly
one way: the wasmtime `Linker` gains the `console.log` host import, and its formatter
**reuses `format_value`** so a printed line and a decoded final value render
identically. `decode` / `format_value` are otherwise unchanged from the closures wave.

```rust
// run.rs — unchanged decode/format; the Linker gains one host import.
pub fn decode(bits: u64, memory: &[u8]) -> Value;    // the closures wave, unchanged
pub fn format_value(v: &Value) -> String;            // the closures wave, unchanged — reused by console.log
// linker.func_wrap("console", "log", |caller, argc: i32, ptr: i32| { … render each jsval … })
```

**Documented simplifications / deferrals (the exceptions wave onward).** `this` / method calls on **user**
objects (`o.m()`, `this` binding), `Object.keys`/`values`/`entries`, **JSON**,
**`Date`**, **`RegExp`**, **`Map`**/**`Set`**, **`Promise`** / async / await,
**generators**, loose equality `==`/`!=`, **`**`** exponentiation, **`for-of`**/
**`for-in`**, **spread**, **default / rest params**, the `Number`/`String` **prototype**
surface beyond §(e) (`toFixed`, `padStart`, `repeat`, `split`, …), `Math.floor` **as a
value**, the callback `(index, array)` extra params, `reduce` without an init, the
`parseInt` radix, and **locale / full-Unicode** correctness (the built-ins wave string case + search
are **ASCII**, byte-indexed, no normalization) are all deferred to the exceptions wave onward. The built-ins wave gives the
subset a built-in library surface and a print channel — but no user-object method
dispatch, no reflection/serialization, and no new host/heap types. There is still no
GC (the `realloc` abandons the old block; a push-heavy loop can exhaust the one page).

**Delivered — reconciled with the shipped code.** The sketch above holds;
the concrete runtime diverges in three named ways. **(1) Three host imports, not one.**
The module declares **three** host functions in the **`"env"`** namespace (function
indices 0/1/2): `env.print(bits: i64, flag: i32)` (backs `console.log`, per-arg flag for
space vs. newline), `env.__convert(bits: i64, op: i32) -> i64` (backs `String`/`Number`/
`parseInt`/`parseFloat`), and `env.__method(recv: i64, op: i32, arg0: i64, arg1: i64) ->
i64` (backs the non-callback array/string methods, dispatched host-side on the receiver's
runtime type). All three reuse the boundary's `decode`/`format_value`. **(2) Host-side
heap allocation via the exported `__heap_ptr` global.** The bump allocator's free offset
is a wasm global exported as `"__heap_ptr"`; `env.__convert`/`env.__method` build strings
and arrays directly into guest linear memory by reading and bumping that exported global,
so a host-built heap value is visible to the guest with no copy-back. **(3) Arity-scan
bump for higher-order callbacks.** The uniform `call_indirect` arity `K` is widened at any
`.map`/`.filter`/`.forEach` call to `max(K, 2)` and any `.reduce` call to `max(K, 3)`,
because those helpers push extra positional args (`(element, index)` / `(acc, element,
index)`) beyond the callback's own params; without the bump a `call_indirect` type
mismatch would trap. The **delivered limitations** carried into the exceptions wave onward: `Math.pow`
truncates fractional exponents (no `f64.pow`), `parseInt` is base-10 only (radix/`0x`
ignored), string case/search/slice/`charAt` are ASCII byte-indexed, `push`/`pop` update
only a plain-local/cell binding (an aliased copy is not), and `reduce` requires an init
value.

### Exception handling: `throw` / `try` / `catch` / `finally`

This wave gave the subset a **standard-library surface**. The exceptions wave — the **final**
campaign — gives it **error handling**: `throw`, `try` / `catch` / `finally`, and
`Error` / `TypeError` / `RangeError` objects. The mechanism is a **manual-unwinding**
model — **no wasm exception-handling proposal** — so the emitted module stays portable
on plain `wasm-encoder` + wasmtime. Two mutable module globals **`$exc_pending`** (i32,
0/1) and **`$exc_value`** (i64 jsval) carry the in-flight exception; codegen keeps a
**handler-label stack**; a **call-site check-and-propagate** runs after every
user-function / callback call; and `run.rs` surfaces an uncaught top-level exception as
a distinct, non-panic `Uncaught <value>` error. The `i64` jsval carrier, the closures wave
header-byte heap encoding, and the frontend → codegen → emit → run pipeline shape are
all **unchanged**; it adds **two module globals**, three IR nodes (`Throw`, `Try`,
`NewError`), the handler-label plumbing, an `Error` heap object, and the uncaught
surfacing.

**Reused machinery (unchanged carrier + heap encoding).** The NaN-boxed `i64` jsval
and the closures wave single-heap-tag + type-byte header (`HEADER_SIZE = 4`; type bytes `0`
string, `1` array, `2` object, `3` function, `4` env, `5` cell, `6` reserved error) —
**unchanged**. This adds **no** new NaN-box tag: the `Error` object is an **ordinary
heap object** (the existing object byte `2`) carrying `name`/`message` — the reserved
error byte `6` was left **unused** (an `Error` needs no distinct layout), and the
unwinding state lives in **two module globals**, not in the value encoding. The funcref Table + `call_indirect` uniform ABI (`env` last), the bump
allocator (+ the built-ins wave `__heap_ptr` global / `__realloc`), the truthiness lowering, the
coercion arms, and the operator & scoping wave/the closures wave scope stack are all reused; the only structural additions
are the two globals and the codegen handler plumbing (inert when nothing throws).

**(a) Unwinding infrastructure.** Two **mutable, exported** module globals:
`$exc_pending` (i32, init `0`; `1` = an exception is in flight) and `$exc_value` (i64
jsval, init `0`; the thrown value), exported under the names `__exc_pending` /
`__exc_value` for `run.rs` to read back. `throw expr` evaluates `expr`, `global.set
$exc_value`, `global.set $exc_pending = 1`, then transfers control to the nearest
handler — `br` to the innermost `try`'s catch-dispatch block label if inside a `try` in
the **same function**, else **early-return** a dummy `i64` from the current function.
After **every** call into user code (direct user calls and `call_indirect`) codegen
emits a **check-and-propagate**:

```wat
;; … call $user_fn  (or call_indirect …)
global.get $exc_pending
if
  ;; br to the innermost handler label, or early-return if the handler stack is empty
end
```

Built-in / host-import calls known not to throw (`console.log`, `Math.*`, the
conversion/method helpers) need **no** check — only user code raises a jsval exception.
Codegen keeps a **handler-label stack** of catch-dispatch labels: entering a `try`
pushes its label, leaving pops it; `throw` and the call-site checks branch to the
**top**, or early-return when empty.

**(b) `try` / `catch`.** `try { BODY } catch (e) { HANDLER }` wraps `BODY` in a wasm
`block` whose end is the **catch-dispatch** (its label pushed on the handler stack for
`BODY`'s duration). After the block: `global.get $exc_pending; if { clear pending; bind
$exc_value to the catch param `e` in a fresh scope; run HANDLER }`. On the no-throw path
`$exc_pending` is `0`, the dispatch is skipped, and `BODY`'s effects stand; on the
exceptional path the `catch` clears the flag and handles the value.

```text
try { throw 42 } catch (e) { e }                              => 42
try { throw "boom" } catch (e) { e }                          => boom
let r=0; try { r = 42 } catch(e){ r = -1 } r                  => 42   (no-throw path)
```

**(c) `finally`.** `try{}finally{}` and `try{}catch{}finally{}` — the `finally` body is
emitted so it runs on **every** exit from the protected region: the normal fall-through,
every exceptional (throw) branch, and a `return` / `break` / `continue` that leaves the
`try` body. After `finally`, **if `$exc_pending` is still set**, re-propagate (branch to
the outer handler / return) — a normally-completing `finally` does **not** swallow a
pending exception.

```text
let x=0; try { x=40 } finally { x = x + 2 } x                 => 42   (finally normal)
let x=0; try { throw 1 } catch(e){ x=40 } finally { x=x+2 } x => 42   (finally on throw)
```

**`finally` on every exit edge.** The `finally` landing block converges **all** ways
control leaves the protected region — the normal fall-through, the *exceptional* `br`
(throw) edges, and a `return` / `break` / `continue` that exits the `try` body. Each
abrupt exit is recorded as a **deferred action** before the finalizer runs; when
`finally` completes normally the pending action resumes (return the stashed value,
branch to the enclosing loop/switch target, or re-propagate the exception). A `finally`
that **itself** completes abruptly — a `return`, `throw`, `break`, or `continue` inside
the `finally` — **overrides** the pending action (standard JS completion semantics):
`try { return 1 } finally { return 2 }` returns `2`; `try { throw 1 } finally { return 2 }`
swallows the throw and returns `2`; `try { return 5 } finally { throw 9 }` discards the
return and propagates `9`. `finally` is therefore correct on every normal, throw,
`return`, `break`, and `continue` path.

**(d) `Error` objects.** `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
are recognized at the `new`-call site (like the built-ins wave built-ins, only when unshadowed) and
lower to the **same IR as the object literal** `{ name: "<Ctor>", message: <arg0> }` —
an ordinary object (byte `2`) carrying `.name` (the constructor name) and `.message`
(the string-coerced arg; empty for no arg). Member reads `e.name` / `e.message` reuse the
existing member-read path, and `typeof (new Error("x"))` returns `"object"`. `throw`
accepts **any** jsval — `Error` is just the idiomatic payload.

```text
try { throw new Error("bad") } catch (e) { e.message }        => bad
typeof (new Error("x"))                                        => object
```

**(e) Propagation, nesting & rethrow.** A `throw` in a deep callee early-returns; each
caller's call-site check re-branches, unwinding frame by frame to the nearest enclosing
`try`. Nested `try` pushes the inner label above the outer; an inner-`catch` throw
continues to the outer. `throw e` inside a `catch` is an ordinary throw from within a
handler.

```text
function f(){ throw 42 } function g(){ f() } try { g() } catch(e){ e }   => 42   (propagation)
try { try { throw 1 } catch(e){ throw e + 41 } } catch(e){ e }           => 42   (nested / rethrow)
```

**The `run.rs` boundary (uncaught surfacing).** This adds **no** `Value` variant and
**no** decode arm — a thrown value is an ordinary jsval. The boundary changes in
exactly one way: **after** invoking `main`, `run.rs` checks the exported `$exc_pending`
global; if set, it renders the exported `$exc_value` with `format_value` and returns a
distinct, **non-panic** `Uncaught <value>` `Err`. Otherwise the run proceeds exactly as
Every wave through the built-ins (decode the final value / the `console.log` output stands).

```rust
// run.rs — after invoking main, before decoding the result:
// let pending: i32 = exc_pending_global.get(&mut store);
// if pending != 0 {
//   let bits: i64 = exc_value_global.get(&mut store);
//   return Err(anyhow!("Uncaught {}", format_value(&decode(bits as u64, mem))));
// }
pub fn decode(bits: u64, memory: &[u8]) -> Value;    // the closures wave/the built-ins wave, unchanged
pub fn format_value(v: &Value) -> String;            // the closures wave/the built-ins wave, unchanged — reused for the Uncaught render
```

**Documented final boundary (the exceptions wave is the last campaign).** The existing **runtime traps**
(division-by-zero, `push` on a non-array, …) **stay wasm traps** — they are **not**
converted into catchable JS errors; this is the explicit boundary between the JS-level
exception model and the machine-level trap surface. (An out-of-bounds array/string index
read is **not** in this set: it returns `undefined` rather than trapping, so
`try { a[999] } catch(e){}` neither throws nor traps — `a[999]` is simply `undefined`.)
Also out of scope, as the documented final
boundary (of which **`error.stack`**, **`.name`**, and **`instanceof` object
identity** later landed in the [Error-identity section](#error-object-identity-instanceof--name--stack)):
**custom `Error` subclasses via `class`**,
**`Error.captureStackTrace`**, **Promise / async rejection**, and (carried from the built-ins wave)
loose equality `==`/`!=`, `**`, `for-of`/`for-in`, `this` / methods on user objects,
spread, default/rest params, generators, and `Date`/`RegExp`/`Map`/`Set`/JSON. Because
The final wrap-up writes the definitive core-language
summary enumerating what the subset supports and this final boundary.

### Syntax sugar

A wave of **syntactic sugar** layered on top of the core subset. Every feature here is
**pure front-end lowering**: each surface form is rewritten into IR that the core subset already
emit, so no new ABI constant, no new runtime import, and no new wasm-emit path is
introduced — the additions live entirely in `frontend`/`codegen` and reuse existing
`IrNode`/`IrStmt` shapes. swc's `EsSyntax` already parses all of these forms; the work is
in the AST → IR lowering. Valid-program behaviour is unchanged for any program that did
not use these forms.

**Template literals → `+` string concatenation.** A `` `a${x}b` `` (`Expr::Tpl`) lowers to
a left-folded chain of `IrNode::Binary { op: BinOp::Add }` alternating cooked-quasi string
literals and interpolated expressions: `Str(q0) + x + Str(q1) + …`. This reuses `Add`'s
existing **string-coercion** path (the same one that makes `"x" + 8` yield `"x8"`), so an
interpolated number/boolean is stringified exactly as `+` already does — no dedicated
"template" IR node. An empty leading/trailing quasi lowers to `Str("")`. Tagged templates
are **not** in scope.

**Exponentiation `**` / `**=` → `MathOp::Pow`.** The binary `a ** b` (`BinExpr` with
`BinaryOp::Exp`) lowers to `IrNode::Math { op: MathOp::Pow, .. }` — the same inline-pow IR
`Math.pow` already uses, so no new emit code. `**` is **right-associative** (`2 ** 3 ** 2`
= `2 ** (3 ** 2)` = 512), which swc's parse tree already encodes. The compound `x **= e`
desugars, like the other compound assignments, to `x = x ** e` — an assignment whose RHS is
the `MathOp::Pow` node — so it flows through the existing lvalue-assignment path (bindings,
array elements, and object properties alike).

**`for-of` over arrays/strings → counted index `while`.** `for (const v of ITER)`
(`Stmt::ForOf`) lowers to an `IrStmt::While`: `ITER` is evaluated **once** into a temp
local and its `.length` is cached **once** into a second temp, then an index counter walks
`0..length`, binding `v` to `ITER[i]` at the top of each iteration. The loop variable is
**block-scoped** per iteration (a captured `let` binding gets a fresh cell each pass), and
the index counter lives in an outer scope so `continue` re-runs the increment via the
`While.update` slot — matching the C-style-`for` desugar. The iterable is restricted to
**arrays and strings**; a statically non-iterable literal (e.g. a number) is a **spanned
codegen rejection** (`for-of iterable must be an array or string`), and `for-await-of`,
destructuring loop bindings, and `for-in` remain rejected.

**Optional chaining `?.` (`a?.b` / `a?.[i]` / `a?.()`) → nullish short-circuit to
`undefined`.** An `Expr::OptChain` is **flattened** (`collect_chain_links`) into its root
base plus an ordered list of member/index/call links, where each link records whether it
was written with `?.` (optional) or `.` (a plain continuation link such as the `.c` in
`a?.b.c`, which swc nests in the same tree). Each **optional** link evaluates its base once
into a fresh temp, tests it for nullishness (`temp === null || temp === undefined`, built
from an `IrNode::Logical`), and routes the short-circuit through the **`else` arm of a
`Ternary`**: a nullish base yields `undefined` and the entire remaining chain is skipped;
otherwise the link (member access, computed index, or call) is applied to the temp and
evaluation continues. Non-optional links are applied unconditionally.

**Logical assignment `??=` / `||=` / `&&=` → conditional assign via `LogicalOp`.** These
compound assignments map (`map_logical_assign_op`) to the existing
`LogicalOp::{Nullish, Or, And}` rather than to an eager `BinOp`, preserving JS
**short-circuit** semantics: `x ??= e` assigns `e` only when `x` is `null`/`undefined`,
`x ||= e` only when `x` is falsy, and `x &&= e` only when `x` is truthy — so `e` (and the
store) is skipped otherwise. They reuse the same lvalue targets as the arithmetic
compounds.

**`do-while` + labeled `break`/`continue` → loop lowering + label→target control flow.**
`do { body } while (cond)` (`Stmt::DoWhile`) lowers to an `IrStmt::While` whose `body` is
prefixed so the body runs **once before** the condition is first tested. Labels extend the
loop IR with a `label: Option<u32>` id (and `IrStmt::Break(Option<u32>)` /
`IrStmt::Continue(Option<u32>)`): a `name:` label allocates a **fresh label id**
(`fresh_label`), binds `name → id` while its immediately-nested loop body is lowered, and
stamps that id onto the loop's `While.label`. A `break name` / `continue name` resolves
`name` to its id and carries `Some(id)`, targeting **that** loop even from inside a nested
loop; an unlabeled `break`/`continue` carries `None` and targets the nearest enclosing
loop/switch as before. A label on a non-loop statement is a **spanned rejection**. Intervening
`finally` clauses still run on the way to a labeled target, exactly as for the
unlabeled forms.

### Modern syntax: destructuring, spread, default/rest params & optional chaining

A second wave layered on top of the sugar wave, adding **destructuring**, **parameter defaults**,
and **rest/spread**. Unlike the sugar wave, the modern-syntax wave is **not** pure front-end lowering: rest/spread need
real heap arrays and the uniform calling convention had to grow a dynamic argument
count, so the modern-syntax wave touches `abi.rs`, the runtime helpers, and the emit paths as well as
`codegen`.

**Array destructuring in `let`/`const`/`var` declarations → hidden temp + indexed
reads.** `const [a, b] = e` evaluates `e` **once** into a hidden temp local, then binds
each target to an indexed read (`temp[0]`, `temp[1]`, …) via the shared
`lower_binding_pattern` helper. It supports **holes** (`[, b]` skips index 0 without
binding), **defaults** (`[a = 9]` lowers to `temp[0] === undefined ? 9 : temp[0]`),
a **rest element** (`[a, ...r]` binds `r` to the tail slice of the temp), and arbitrary
**nesting** (`[[a], {b}]`), where each nested pattern recurses on the corresponding
indexed read as its own sub-temp.

**Object destructuring in declarations → keyed reads + rest copy.** `const {x} = e`
reads each named property off the once-evaluated temp. It supports **rename**
(`{x: a}` binds `a` to `temp.x`), **defaults** (`{z = 7}` guards an `undefined`
property with the same `=== undefined ? default : value` ternary), **nesting**
(`{p: {q}}`), and a **rest property** (`{a, ...rest}`), where `rest` is built by the
`__obj_rest` runtime helper — a key-excluding shallow copy that clones the source
object while omitting the already-bound keys. Both array and object patterns share the
one `lower_binding_pattern` entry point.

**Default parameters `f(a, b = 2)` → per-call prologue guard.** Each defaulted parameter
emits an `if (param === undefined) param = default;` guard at the **top of the function
body**, run **per call**, so an omitted (or explicitly `undefined`) argument takes the
default. A default expression may **reference earlier parameters** (`f(a, b = a + 1)`)
because the guards run in declaration order after all slots are bound. `FnSig` gained a
**required-argument count** so callers and the prologue know how many leading params are
mandatory.

**Rest parameters `f(...args)` → trailing args collected into a real array, dynamic
argc.** A trailing rest parameter binds to a **heap array** holding every argument beyond
the fixed ones. Supporting this required widening the **uniform fixed-arity ABI**: the
canonical wasm function signature became `(a0 … a_{K-1}, env, argc) -> i64` — the fixed
positional slots, the closure `env` slot, and a new **`argc`** (actual argument count)
threaded through **direct calls, indirect/closure calls, and higher-order calls** alike.
`abi.rs` remains the **single source** for these slot/arity constants; `argc` lets the
rest-collection code know where the real arguments stop.

**Spread in call args and array literals `f(...a)`, `[...a, ...b]` → `__array_concat`.**
A spread source is flattened by the `__array_concat` runtime helper, which builds a fresh
**heap array** from its inputs. In an **array literal**, the concatenated array is the
value. In a **call**, the flattened elements are spread into the K fixed positional slots
with the true length passed as `argc`. Spread-aware `Math.max`/`Math.min` **fold** over
the concatenated array rather than taking fixed positional operands.

**Parameter destructuring `f([a, b])`, `g({x, y})` → `lower_binding_pattern` on a temp
slot.** A destructuring parameter is received into an ordinary temp parameter slot, then
`lower_binding_pattern` (the same helper the declaration forms use) unpacks it in the
prologue. It **composes** with defaults (`f([a] = [])`) and with rest, since all three go
through the shared binding path.

**Caveat (no GC):** rest parameters and spread/array-literal concatenation allocate their
arrays (and `__obj_rest` its object copy) on the **bump heap**, which is never reclaimed —
fine for the short programs jsc targets, but every such allocation is permanent for the
run. **ABI limit:** a spread whose **runtime** length exceeds the fixed positional arity
`K` **drops the surplus arguments** — an inherent consequence of the fixed-arity ABI
(a call site reserves exactly `K` slots), and outside the modern-syntax wave milestones.

### OOP: `this`, object methods & `class`

**`this` + method calls on object literals.** Object literals gained **method**
and **shorthand-method** properties (`{ m(){..} }` ≡ `{ m: function(){..} }`, a
first-class function value that carries `this`) and **shorthand** properties (`{ n }` ≡
`{ n: n }`). A call whose callee is a member expression, `obj.m(args)` — once the builtin
array/string/`Math`/`console` method names have been ruled out — is a **user method call**
that binds `this = obj` for the callee. It lowers to a dedicated `IrNode::MethodCallUser`
(kept distinct from `IrNode::CallIndirect`, which resets `this`, and from the builtin
`IrNode::MethodCall`).

**This-binding model — a global carrier + per-function local snapshot.** The uniform
closure ABI is **unchanged** (`(a0 … a_{K-1}, env, argc) -> i64`); `this` rides a dedicated
mutable wasm **global** `$__this` (a jsval `i64`, index `THIS_GLOBAL_INDEX` in `abi.rs`,
initialized to `undefined`) rather than an extra parameter:

- A **method call** `obj.m(args)` evaluates `obj` once into a temp, evaluates the
  arguments, reads the callee function value at `obj.m` (the ordinary `__obj_get` member
  path), sets `$__this = obj` **immediately before** the `call_indirect` (after every
  sub-expression is evaluated, so a nested method call in an argument cannot leak its
  receiver), then performs the normal indirect call.
- A **plain call** (`f(args)`, an indirect closure call, a spread call, a higher-order
  callback) resets `$__this = undefined` right before the call, so a callee invoked
  without a receiver sees `this === undefined`.
- **Every function/arrow prologue** that reads `this` snapshots `$__this` into a
  function-local slot as its **first** statement, and a `this` expression reads that local.
  This makes the global-clobber safe: a nested `this.other()` re-sets the global just
  before its own call, but the enclosing frame already captured its own `this` into a local
  at entry, so it is unaffected after the nested call returns. The snapshot is emitted
  **only when the body actually reads `this`** — a `this`-free function needs none, and its
  IR is unchanged — a pure optimization with no observable effect on the mandated model.

**Out of scope (rejected with spans).** Getters/setters (`get x(){}` / `set x(v){}`),
computed method names (`[k](){}`), and async/generator methods are rejected. A **top-level
`this`** (outside any function, with no receiver) is rejected. **Arrows do not yet get
lexical `this`:** an arrow snapshots `$__this` like any function rather than capturing its
enclosing `this` — a documented limitation for a future refinement (an arrow used as a
callback therefore sees `this === undefined`, since a plain call resets the carrier).

**`class` declaration + `constructor` + `new`.** A class name binds a **heap
object** holding two reserved, non-JS-writable **sigil keys** (interned string constants
in `abi.rs`): `@@constructor` = the constructor lowered as an **ordinary uniform-ABI
function value** (`(a0 … a_{K-1}, env, argc) -> i64`, the same closure ABI as every other
function — a class with no `constructor` gets a synthesized empty one), and `@@prototype`
= **one shared prototype object per class**. `new A(args)` (`IrNode::New`)
**allocates a fresh instance object** that is **pre-slotted** with the `@@prototype` link
plus one slot per own field, sets `$__this = instance`, runs the constructor with the
supplied arguments, **discards the constructor's return value**, and yields the instance
— so `typeof (new A()) === "object"`. The object model has **no dynamic key growth**, so
every own property must exist at allocation time: the instance shape is computed by a
**compile-time field-shape scan** of the class body, and the constructor may only
assign into slots that scan already reserved.

**Member/index-write correctness fix.** This fixed a pre-existing
**scratch-clobber** bug in member/index assignment. `emit_index_set_node` /
`emit_index_node` stashed the receiver / key / value in the **low** scratch slots and then
re-emitted child sub-expressions over **those same slots**, so a **computed RHS**
(`o.v = 3 + 4`, `this.x = a + b`) overwrote the stash mid-emit and produced a wrong value
(or a trap in a constructor). The fix **relocates the set/get stash to dedicated HIGH
scratch slots** (disjoint read vs. write banks) and extends the fixed scratch bank
accordingly, so evaluating the RHS can no longer touch the stashed receiver/key.
**Documented limitation:** **same-kind nesting** — `a[b[i]]` (index inside index) or a
chained store-in-store `a[i] = (b[j] = k)` — reuses the same fixed high slots and remains
**out of scope**; correct nesting would need a **depth-indexed scratch allocator**.

**class methods (shared via the prototype).** Each **non-constructor** method is
lowered **once** and stored on the class's **single shared `@@prototype` object** — never
copied per instance. A **member read** resolves **own properties first**, then falls back
to the instance's `@@prototype`. The fallback lives inside the `__obj_get` runtime helper,
which **recurses on the `@@prototype` slot** (own wins; the recursion terminates because a
prototype object itself carries no `@@prototype`). `MethodCallUser`'s callee read **reuses
the same member path**, so `obj.m` (read) and `obj.m()` (call) share the identical
own-then-prototype fallback. The fallback is emitted **only when a class exists** in the
program, so class-free programs keep the exact prior plain-object read behavior.

**class fields (`field = init`).** An instance **field declaration** is desugared
to a synthetic `this.<field> = <init>` statement and **prepended, in source order**, to
the constructor body *before* lowering — so field initializers run **before** the
constructor body and a later field init may read an earlier field through `this`. The
**pre-slotted instance shape** is the **union** of `{field declarations}` and
`{constructor `this.<f> = ` assignments}`, **deduplicated by first occurrence**. A class
that has fields but **no explicit constructor** synthesizes a default constructor
consisting of just those field inits. An **uninitialized field** `f;` desugars to
`this.f = undefined`.

**Out of scope for the class wave (all rejected WITH SPANS; future work):** `extends` / `super` /
inheritance, **static** members (methods and fields), getters/setters, **private `#`**
members, **computed** method/field names, and class **expressions**. The arrow
limitation also carries forward: **arrows do not yet get lexical `this`.**

**`extends` / `super` and `static` members.** Inheritance shipped later (see the classes suite). **Static** members follow the same object-model discipline:
`static m(){}` and `static f = init` are lowered onto the class **VALUE** object (alongside
the reserved `@@constructor` / `@@prototype` entries), so `C.m()` / `C.f` read them directly
off the class. A static method is an ordinary lifted function whose `this` binds to the class
on a `C.m()` call. A static field's initializer that is a **bare literal** (`static x = 1`) is
evaluated inline in the class value; **any other** initializer — one that references the class,
an **earlier** static, or `this` — is **pre-slotted `undefined`** and assigned **after** the
class binding exists (`C.<key> = <init>`), in **source declaration order**, so a later static
observes the earlier ones and `this` inside the initializer resolves to the class. An
uninitialized `static f;` is `undefined`. Still out of scope: **static inheritance** (a derived
class's value has no constructor-side prototype link, so it does not see a parent's statics),
static **blocks**, and static **getter/setter/async/generator/computed** members (spanned
rejections, static or not).

**Instance accessors (`get x(){}` / `set x(v){}`).** An **instance getter/setter** is a
property whose read/write runs a function. Each accessor property lowers to ONE
**`TYPE_ACCESSOR`** heap block (`[type=11][pad×4][getter:i64][setter:i64]`, `IrNode::MakeAccessor`)
holding the getter and setter as ordinary **lifted function values** (`undefined` for an absent
half); a `get x` and `set x` sharing the name `x` collapse into the same box. The box is stored
under the property name on the class's **shared `@@prototype`**, right alongside the plain method
values — so it is resolved by the **same own→prototype `__obj_get` fallback** and is **inherited**
by a subclass through the prototype-chain link. A **member/computed READ** (`obj.x` / `obj[k]`)
whose resolved value is a `TYPE_ACCESSOR` box invokes the **getter** with `this` = the receiver
(the stashed `scratch + 17`); a **WRITE** (`obj.x = v`) first resolves the key **through the
prototype chain** and, on an accessor hit, invokes the **setter** with `this` = the receiver and
the assigned value, discarding its result. A half-declared accessor degrades to JS semantics:
reading a **set-only** property yields `undefined`; assigning a **get-only** property is a silent
**no-op** (non-strict). Both halves read `this`/`super` through the this-carrier exactly like a
method, so a getter/setter body freely calls other methods and reads fields. A getter/setter that
**throws** propagates like any call — the get/set invocation sites track their enclosing `if`
frames, so a `try`/`catch` around the read/write observes the throw at the right handler
depth. The getter/setter's own arity is folded into the uniform ABI **`K`** by the class-body
arity scan (a setter's lone parameter has no arg-bearing call site, so it must widen `K` there).

The invocation paths are **gated** on a compile-time sentinel (`@@accessor`, interned iff the
program builds any `MakeAccessor`), so an accessor-free program keeps **byte-identical**
member-read/write emission — the same idiom as the `@@prototype` gate on the prototype fallback.
Still out of scope: **static** accessors, **object-literal** accessors, and **computed**
accessor names (all spanned rejections).

### Collection utilities: iteration, reflection & serialization

A wave of **object/collection utilities** built on the fixed-shape object model and
the growable-array model. Every the collections wave primitive is an **in-wasm** implementation — no new host
import is added — so the guest stays self-contained and the built-ins wave host-shim debt is not
widened. The `__convert` import is reused for number formatting where needed; no other
import is touched. Objects remain **fixed-shape** (compile-time field scan; no dynamic key
growth), which is the single constraint that shapes every decision below — most visibly the
`JSON.parse` deferral.

**`for-in` over object/array/string keys → counted index loop + `ObjectKey`
primitive.** `for (const k in obj)` (`Stmt::ForIn`) lowers, like `for-of`, to a counted
index `while`: the operand is evaluated **once** into a temp, its **key count** is cached
**once** (`IrNode::StrLen` reused as the count query), and an index counter walks `0..count`,
binding the loop variable to `IrNode::ObjectKey { iter, index }` — a reusable
key-**enumeration** primitive — as the **first** body statement (a captured `let` gets a
fresh cell per iteration; the increment lives in the `While.update` slot so `continue` still
advances it). For an **object** operand `ObjectKey` yields the own key **heap-string** in
insertion order; for an **array/string** operand it yields the decimal **index string**
(`"0"`, `"1"`, …). Hidden reserved keys are **skipped**: any key whose first payload byte is
`@` (the `@@prototype` / `@@constructor` sigils of the class wave) is not enumerated. Only a **single
`const`/`let`/`var` identifier head** is supported; a **destructuring/assignment head**,
`for await`, or `using` head is a **spanned rejection**, as is a statically-known
**non-enumerable** operand (a number/boolean/null literal → `for-in operand must be an
object, array, or string`). The head opens a lexical scope, so the binding is not visible
after the loop.

**`Object.keys` / `Object.values` / `Object.entries` → in-wasm enumerators over the
key table.** Each lowers to a dedicated runtime helper (`__obj_keys` / `__obj_values` /
`__obj_entries`) that linear-scans the object's `(key_ptr, val)` entry table `0..n` and
appends each non-reserved entry to a **fresh growable array**: `keys` collects the key
heap-strings, `values` the stored jsvals, and `entries` a `[key, value]` two-element array
per entry. Reserved `@`-prefixed keys are skipped by the same first-byte guard
(`emit_key_not_reserved`) that `for-in` uses, so the class wave prototype/constructor sigils never
leak into reflection output. The returned arrays are ordinary heap arrays (grow via `push`,
render as `[…]`).

**`in` + `delete` → own-key probe + compaction (no tombstone).** The `in` operator
(`"k" in obj`) lowers to the `__obj_has(obj_ptr, key_ptr)` runtime helper: an **own-key
only** membership probe (**no prototype walk** — a prototype-inherited method is *not* an own
key), returning `1`/`0`. `delete obj.k` lowers to `__obj_delete`, which **compacts the entry
out** rather than tombstoning it: on a key hit it `memory.copy`-shifts the trailing entries
down over the removed slot and **decrements the entry count `n`**. Because the deleted slot
physically disappears, every consumer **auto-skips** it with no per-consumer tombstone check —
`__obj_get` misses (`undefined`), `__obj_has` returns `0`, and every index-based enumerator
(`for-in`, `Object.keys/values/entries`) sees the shortened table. Compaction over
tombstoning trades a one-time `memory.copy` for keeping the hot read/enumerate paths
branch-free.

**`JSON.stringify` → fully in-wasm recursive serializer.** `JSON.stringify(v)` lowers
to `IrNode::JsonStringify`, backed by the recursive `__json_str(v) -> str` helper (with
`__json_escape` for string quoting). It serializes by value type: `null`/`true`/`false`/
numbers via the reused **`__convert`** import (no new host import for number formatting),
strings via `__json_escape` (quote + escape), arrays as `[…]` recursing on each element, and
objects as `{"k":…}` recursing on each own value. It matches JS's **hole rules**: an
`undefined`/function **element** in an array serializes as `"null"`, whereas an
`undefined`/function **value** in an object is **omitted** entirely (key and value dropped).
The optional `replacer`/`space` second and third arguments are **unsupported and rejected**.

**`JSON.parse` — originally deferred, now delivered (see the loose-equality section).** It was first deferred
`JSON.parse`, reasoning that object keys are **interned static-string pointers** fixed at
compile time and `__obj_set` **cannot add a key**. That reasoning was **too strong** and has
since been **reversed** (see the loose-equality section): the fixed-shape constraint only blocks *mutating* an
existing object's key set, not *allocating a fresh* object with a runtime-sized entry table,
and every consumer matches keys by **byte content**. `JSON.parse` is now a fully in-wasm
recursive-descent parser (`emit/json_parse.rs`), no new host import. The paragraph is kept for
history; the current design is described in the loose-equality section.

**`Map` / `Set` → new heap types over a growable flattened entry array.** `Map` and
`Set` get **two new heap types**, `TYPE_MAP = 6` and `TYPE_SET = 7` (`abi.rs`), whose handle
block is `[type][pad×3][size: u32][entries…]` backed by a **growable flattened `(key, value)`
array** (a Set stores each element as its own key with an ignored value slot). `new Map()` /
`new Set()` (`IrNode::MapNew`) allocate a fresh empty handle. Lookup is a **linear scan** via
strict `===` (the `__js_eq` kernel factored out of `emit_strict_eq`, packaged so `__map_find`
can call it): `__map_find` returns the address of the first key-matching slot or `-1`. The
methods — `set` / `get` / `has` / `delete` and `.size` for `Map`, `add` / `has` / `delete`
and `.size` for `Set` — **dispatch at runtime on the receiver's TYPE byte**, so a same-named
method on a plain object or class instance still routes to the **user method call**
(`MethodCallUser`) fallback rather than the collection op. `set` on an existing key **updates
in place** (size unchanged); `add` de-dupes by `===`. `typeof (new Map()) === "object"`, and
`run.rs` decodes only the `size` word (the backing entries are not walked) to render
`Map(N)` / `Set(N)`.

**Out of scope for the collections wave (rejected with spans / omitted):** **computed property keys**
(`{[k]: v}`), **`WeakMap` / `WeakSet`**, **`Object.assign` / `Object.freeze`**, the
**Map/Set iteration protocols** (`forEach` / `entries` / `keys` / `values` beyond the listed
methods), and **Symbol keys**. `Map`/`Set` **constructor arguments** (`new Map([[k, v]])`)
are also rejected — collections start empty and are populated via `set`/`add`. **`JSON.parse`**
was originally deferred here pending dynamic key-growth, but has since **shipped** in the loose-equality section (an
in-wasm parser that builds objects with byte-compared keys — no fixed-shape scan needed).

**Caveat (no GC):** the reflection helpers (`Object.keys/values/entries`), `JSON.stringify`'s
string builder, and every `Map`/`Set` grow allocate on the **bump heap**, which is never
reclaimed (§10) — fine for the short programs jsc targets, but a grow-heavy `Map` loop
accumulates abandoned backing arrays.

### Unified scoping: functions & methods reference enclosing bindings & class names

Before this campaign, the lexical-capture rules and the class-name binding were
**scoped to arrow/closure bodies**: an arrow could close over an enclosing `let`/`const` and a
top-level `class C` could be `new C()`'d only at its declaration scope. A **`function`
declaration** or a **class method** body, by contrast, could neither read an enclosing
`let`/`const` (the closures wave analyzed only arrow/function-*expression* bodies as nested frames) nor
resolve the class name from inside a constructor/method (`new C()` inside a method failed with
`unsupported constructor: C`). This campaign closes both gaps so **all** function forms —
arrows, function expressions, function declarations, and methods — share **one** scoping rule:
a body may reference any enclosing binding, whether an outer `let`/`const` or the class's own
name, matching JS lexical semantics. Two milestones, both purely codegen (no IR, ABI, or
run.rs change — the closures wave boxed-cell / closure-env machinery is *reused*, not extended):

**Gap 1 — the class name resolves inside its own constructor and method bodies (`new C()`
everywhere).** The class name is **bound and its instance shape recorded before** the
constructor and method bodies are lowered, so a `new C()` referencing the class from inside any
of them resolves to the binding instead of failing. Capture analysis binds the name **first**
and analyzes each method body as a **nested frame** (mirroring the constructor), so a body that
references the name marks it **captured across the function boundary**. A captured class name
routes through the **same boxed cell** an arrow uses (per the closure-capture rules): the cell is allocated **empty
first**, the ctor/method closures capture that live cell pointer, and it is **filled with the
fully-built class value afterwards** — so a `new C()` running later reads back the completed
class. The builtin `new TypeError(..)`/`new Error(..)` path inside a method body is unchanged
(it never routes through the user-class binding). Verified end to end: a method that `new C()`s
a fresh instance, a method that recurses through `new` of its own class (`fact()`), and a
factory method building a linked-node chain.

**Gap 2 — `function` declarations and methods capture enclosing `let`/`const` (read + write +
nested).** the closures wave's capture analysis is extended to **top-level function declarations**: each
declaration body is analyzed **nested under the main frame**, so a reference reaching an
enclosing `let`/`const` marks **both** the binding captured **and** the declaration as
*capturing*. Pass 1 then **partitions** the hoisted declarations: a **non-capturing** one keeps
the **direct-call fast path unchanged** (hoisted, called by name with `env = undefined` — zero
overhead, the common case), while a **capturing** one is **demoted to a closure** (a boxed cell
in `main`, lowered at its source position via the existing `lower_nested_fn_decl` path, and
called indirectly with a trailing `env`). Capture is **by reference** through the shared cell,
so a declaration that **writes** an enclosing `let` (`function add(x){ s = s + x }`) mutates the
one cell every holder sees, and a **nested** `function` inside a `function` captures locally the
same way. **Class methods already route through the lifted-function mechanism**, so they capture
enclosing bindings identically — a method reading an outer `let base` needs no extra machinery.
The combined case (a method that **both** `new C()`s its own class *and* closes over an enclosing
`let`) is covered by a dedicated L3 integration test.

**No-capture fast path preserved.** The whole point of the partition is that the **overwhelming
common case pays nothing**: a `function` declaration that references no enclosing binding stays
a **hoisted, directly-called** top-level function with `env = undefined` — identical wasm to
before this campaign. Only a declaration that *actually* reaches an outer `let`/`const` is
demoted; the full regression stays result-for-result green because non-capturing programs
compile unchanged.

**Bounded rejections the runtime guard kept (each a precise **span** diagnostic, not a silent miscompile).**
Demotion narrows a capturing declaration's reachability, so three forms are rejected at compile
time rather than mis-lowered:

1. **Sibling-call of a demoted capturing function.** A demoted closure is bound only within
   `main`, so another **top-level function** cannot reach it: a call to / reference of it from a
   sibling top-level function is rejected — *"`f` closes over enclosing vars, cannot be
   called/referenced from another top-level function"* — instead of emitting an unresolved call.
2. **Use-before-declaration of a captured binding.** A demoted declaration **loses hoisting**
   (it is lowered at its source position, not raised to the top), so calling it **before** its
   captured binding is declared (`f(); let n=41; function f(){ return n }`) is an error — an
   intended narrowing versus JS's hoisting, surfaced with a span rather than reading an
   uninitialized cell.
3. **Out-of-scope class forms** (as of this campaign). Class-name capture (Gap 1) did **not**
   extend to `extends`/`super`, `static` members/fields/blocks, or getter/setter accessors — at
   the time these were the pre-existing spanned the class wave rejections. `extends`/`super`,
   `static` members, and **instance** getter/setter **accessors** have since shipped;
   what remains rejected is **static** accessors, static **blocks**, **object-literal** accessors,
   and **computed** accessor names.

The net effect: **one** lexical-scoping rule across every function form, the zero-overhead
direct-call path intact for non-capturing declarations, and every construct the campaign does
not yet support failing loudly with a span.

### String-method surface & the unknown-method guard

The built-ins wave (e) method surface shipped the everyday non-callback array/string methods but
left the receiver-method lowering **open at the bottom**: a method name it did not recognize
(`"x".zzz()`, `[1,2].flat()`, `let o={}; o.foo()`) fell through to an unresolved
`call_indirect` and surfaced as the wasm-validator failure `failed to compile: wasm...` — a
leaky, locationless miscompile rather than a language diagnostic. This campaign closes that
boundary and, on top of the now-guarded lowering, fills out the string-method set that real
programs (the motivating FizzBuzz below) reach for. Two milestones, both purely codegen +
host-side runtime (no `run.rs` boundary change — every value stays an existing the closures wave/the built-ins wave kind):

**An unknown method is a clean compile-time diagnostic, never invalid wasm.** The
receiver-method lowering is **guarded**: a call whose method name is not a recognized built-in
for its receiver kind (nor a supported user method call, per the object-method rules) is rejected with the spanned
diagnostic *"method '`<name>`' is not supported"* (§9) instead of emitting an unresolved
`call_indirect`. The invariant is total — **no input may produce `failed to compile: wasm...`**;
every out-of-scope method fails loudly at its source span. The guard sits **below** the existing
built-in and user-method dispatch, so it only fires for genuinely unrecognized names and changes
no valid program's behavior.

**The static-shape unknown-method extension.** The diagnostic
closed the *invalid-wasm* case (a primitive/array-literal receiver, or a program with no funcref
table). It left a *wrong-value* gap: a program that DOES define functions could call an unknown
method on an object that itself carries methods — `let o = { f() { return 1 } }; o.bar()` — and
the generic path read `o.bar` → `undefined`, then used its bits as a funcref-table index that
happened to hit a real function, returning a **wrong value** (valid wasm, so the diagnostic gate passed).
The static check closes this with a **static-shape** guard. A pre-pass ([`analyze_captures`], reusing its total
traversal) records, for every `let`/`const` binding initialized directly with an
all-static-key **object literal**, that binding's own-key set — keyed by [`binding_id`] and kept
only when the binding's name is **never reassigned** anywhere (a name-based over-approximation, so
the recorded shape is provably fixed; the object model has **no dynamic key growth** (the object model), so a
key absent from the literal is absent forever). At a user-method call whose receiver is a direct
object literal or such a binding, a statically-known method name that is not among the shape's keys
is rejected with the same *"method '`<name>`' is not supported"* span. The guard is **sound by
construction** — it never fires on a receiver whose shape is not statically fixed (a reassigned
binding, a `new`-constructed instance, a computed/dynamic key, a call result), so runtime dispatch
is unchanged for every valid program.

**The dynamic-shape unknown-method runtime guard.**
The static check's soundness left exactly those unprovable receivers — a `new`-instance, a reassigned binding, a
computed key, a call result — to **runtime dispatch**, where a genuinely absent method still read
`undefined` and dispatched its bits as a funcref-table index: a **wrong value**, or, when the index
was out of range or its callee's type mismatched, a **cryptic wasm trap** with no source context.
The runtime guard closes this last gap with a **runtime** guard emitted in [`emit_method_call_user`]: immediately
after the member read resolves the callee jsval, and **before** the `call_indirect`, codegen checks
the value is a heap **function** (the [`emit_is_function`] tag test — a pointer whose block TYPE
byte is `TYPE_FUNCTION`). When it is not, the call raises a clean exception — the fixed message
*"TypeError: called value is not a function"* stashed into `$exc_value`, `$exc_pending` set — and
unwinds through the ordinary the exceptions wave machinery, so it is **catchable** by an enclosing `try`/`catch`
and otherwise surfaces as `Uncaught TypeError: called value is not a function` (a non-zero exit),
never a trap. The message blob is interned by [`collect_node`] at every method-call site (the
`MethodCallUser` path and the unknown-collection-method `MapMethod` fallback). A valid method call —
the callee IS a function — takes the same path as before at the cost of one tag test, so no valid
program's behaviour changes. Together the invalid-wasm fix, the static-shape check, and the runtime guard
(dynamic-shape wrong-value/trap) make **every** unknown-method call fail loudly: a compile-time span
where the absence is provable, a runtime `TypeError` where it is not.

**The string-method set completed.** The built-ins wave (e) non-callback method family gained the
remaining everyday string methods, recognized on a string receiver and lowered to the **same
`env.__method` host import** (dispatched host-side on the receiver's TYPE byte, per the runtime method-dispatch rules) — each
producing a **new** heap string (or, for `split`, a new array of new strings), never mutating the
receiver:

- `trim()` / `trimStart()` / `trimEnd()` — strip ASCII whitespace from both ends / the start /
  the end.
- `repeat(n)` — the receiver concatenated `n` times (`n = 0` => `""`).
- `startsWith(sub)` / `endsWith(sub)` — a boolean prefix / suffix test (the argument is
  string-coerced).
- `padStart(len, pad)` / `padEnd(len, pad)` — pad to BYTE length `len` with `pad` (default a
  space), truncated to fit; a no-op once already `len` bytes.
- `substring(a, b)` — the `[a, b)` substring by BYTE index (JS clamp semantics).
- `split(sep)` — a new array of substrings; an empty `sep` splits into one element per byte.
- `includes(sub)` — substring membership on a string receiver (the array form already shipped in
  the built-ins wave (e)).

Milestones: `" hi ".trim()` => `"hi"`; `"ab".repeat(3)` => `"ababab"`;
`"hello".startsWith("he")` => `true`; `" Hello ".trim().toUpperCase()` => `"HELLO"`. Method
**arity mismatches** on the new names are compile errors, matching the existing built-in arity
style. The whitelist sits **above** the diagnostic guard, so these names now lower to their host helper
while every still-unknown name (`"x".zzz()`, `[1,2].flat()`) keeps failing with the clean
diagnostic.

**FizzBuzz-with-trim capstone.** The wave's motivating program — a classic `for`-loop FizzBuzz
that builds each line, pads it, and `.trim()`s the padding off before `console.log` — now
compiles and runs, printing the canonical `1 / 2 / Fizz / 4 / Buzz / … / FizzBuzz` lines. It
composes the loop, `%`, `===`, string concatenation, `String(i)`, `.trim()`, and the print
channel end to end, and is pinned by an integration smoke test.

**Method-resolution rules — the consolidated decision procedure.** The three fixes above each closed one failure mode of the
same lowering boundary; this is the single ordered rule set that call-expression lowering now
follows for a member call `recv.name(args)` (a computed `recv[expr](args)` is treated as a
dynamic key throughout). The steps are tried **top to bottom**; the first that matches wins, so a
guard only fires once every dispatch above it has declined:

1. **Recognized built-in method.** If `name` is a built-in for the receiver's *static kind* — a
   string method (`trim`, `toUpperCase`, `startsWith`, … including the added string methods), an array
   method (`push`, `map`, `filter`, `includes`, …), or a supported collection/number/`Math` member — the call lowers to its intrinsic or the shared `env.__method` host import (dispatched
   host-side on the receiver's TYPE byte, per the runtime method-dispatch rules). **Arity is checked at compile time**; a wrong
   argument count is a spanned diagnostic, matching the built-in style.

2. **User-defined method.** Otherwise, if the call is a user method — an object-literal
   method, a class method, or a `this.m()` inside a method body — it lowers to the user-method path
   (`MethodCallUser` → funcref-table `call_indirect`, env-last ABI).

3. **Provable-absence rejections at compile time.** A user-method call whose receiver's shape is
   *statically knowable* and provably lacks `name` never reaches emit:
   - **Invalid-wasm fix** — the receiver is a statically-primitive / array-literal value, or the whole program
     defines no functions (no funcref table). The generic path could only emit an unresolved
     `call_indirect`, so the call is rejected up front.
   - **Static-shape guard** — the receiver is a direct object literal, or a `let`/`const` binding whose initializer
     is an all-static-key object literal and whose name is **never reassigned** (recorded by the
     [`analyze_captures`] pre-pass, keyed by [`binding_id`]); the object model has no dynamic key
     growth, so a key absent from the literal is absent forever. A statically-known `name`
     outside that fixed key set is rejected.

   Both raise the identical **compile-time span diagnostic** *"method '`<name>`' is not supported"*
   (§9), pointing at the offending call.

4. **Unprovable-absence guard at runtime.** When the receiver's shape is **not** statically fixed —
   a `new`-constructed class instance (`new C().bar()`), a function-parameter receiver, an array
   element, a reassigned binding, a computed/dynamic key, a dynamic `this.m()`, or a call result —
   absence cannot be proven at compile time, so dispatch proceeds but is guarded. [`emit_method_call_user`]
   resolves the callee jsval from the member read and, **before** the `call_indirect`, tests it is a
   heap **function** ([`emit_is_function`] — a pointer whose block TYPE byte is `TYPE_FUNCTION`). If it
   is not, the call raises the fixed **runtime** exception *"TypeError: called value is not a
   function"* through the ordinary the exceptions wave unwinding path (**catchable** by an enclosing `try`/`catch`,
   otherwise `Uncaught TypeError: called value is not a function` with a non-zero exit) — never a
   wrong value and never a cryptic wasm trap.

**Residual bound (closed by the Error-identity work — see [the Error-object identity section](#error-object-identity-instanceof--name--stack)).**
The runtime guard's thrown value was originally a **plain heap string** stashed straight into
`$exc_value`, because jsc had **no `Error`/`TypeError` object identity** at the member-call boundary:
a `catch (e)` bound `e` to that string, so `e instanceof TypeError` was not modelled. This later work
upgraded the guard to throw a **real branded `TypeError` object** (`emit_method_call_user` now emits
[`internal_type_error`] into `$exc_value`), so `catch (e) { e instanceof TypeError }` holds and
`e.name`/`e.message`/`e.stack` read back; the uncaught render is unchanged
(`Uncaught TypeError: called value is not a function`). The message is still **generic** — the fixed
`"called value is not a function"`, with **no receiver-expression text** (it does not name the
offending method or receiver) — an acceptable residual for the totality guarantee.

**Invariants.** (a) *Totality* — no input can reach the wasm validator as `failed to compile:
wasm...`; every unknown method fails loudly, at its source span where absence is provable and as a
catchable runtime `TypeError` where it is not. (b) *Zero cost to valid programs* — a recognized
built-in or a genuine user method takes exactly the path it took before; the only added work is the
single tag test in step 4, on the dynamic-receiver path alone. (c) *Soundness* — steps 3–4 never
fire on a call that a correct JS engine would dispatch, because the static guard's shapes are provably
fixed and the runtime guard only rejects a callee that is *not a function* at runtime.

### Error-object identity: `instanceof` / `.name` / `.stack`

The exceptions wave gave `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)` a heap
representation — a plain `{ name, message }` object (type byte `TYPE_OBJECT`) —
but **no identity**: the three kinds were structurally indistinguishable, there
was no `.stack`, and `instanceof` was unimplemented (it fell through
`map_binary_op` to *"unsupported operator"*). This later work closes that gap
while keeping errors ordinary byte-`2` objects — no new heap TYPE byte.

**(a) The identity marker `@@errorkind`.** `lower_new` now brands each error
object with a **hidden reserved property** `@@errorkind` (`abi::ERROR_KIND_KEY`)
whose value is a number jsval encoding the kind — `Error` = `0`, `TypeError` =
`1`, `RangeError` = `2` (`abi::ERROR_KIND_{ERROR,TYPE,RANGE}`). Like
`@@prototype`/`@@constructor`, the `@@` prefix makes it **invisible** to
`for-in` / `Object.keys` / `JSON.stringify` (the `emit_key_not_reserved` guard
skips any key whose first byte is `@`), yet `__obj_get` reads it back. The value
is never `undefined`, so "the object is an error" is testable as "the loaded
marker `!== undefined`", and the exact kind by strict-equality against the
constants. A plain object that merely *looks* like an error
(`{ name: "Error", message: "x" }`) lacks the marker, so it is **not**
`instanceof Error` — matching JS.

**(b) `.stack`.** The error object gains a regular `stack` string property:
`"<name>: <message>"`, or just `"<name>"` when the constructor took no argument
(jsc keeps **no runtime call frames**, so there is no ` at …` section — a
documented single-line approximation). The message sub-expression is **teed into
a fresh temp** so it is evaluated exactly once: `new Error(f())` runs `f` a
single time even though both `message` and `stack` reference it.

**(c) `instanceof` (user classes + builtin errors).** `x instanceof RHS` lowers
to `IrNode::InstanceOf { value, kind: InstanceOfKind }` (recognized beside the
`in` operator in `lower_expr`). The RHS must be a **bare identifier**, dispatched
by name at lowering:

- a **user class** (matched first, so `class Error {}` *shadows* the builtin) →
  `kind = Class(C["@@prototype"])`; the test compares the receiver's own
  `@@prototype` slot to the class's shared prototype by **pointer identity** — a
  single hop, exact under the current no-`extends` model, so `new C() instanceof
  C` holds and `new C() instanceof D` does not;
- an **unshadowed** `Error` / `TypeError` / `RangeError` → the hidden-brand check
  (`Error` matches any present `@@errorkind`; `TypeError`/`RangeError` match their
  exact kind), so a `TypeError` is `instanceof TypeError` **and** `instanceof
  Error`, but a base `Error` is neither `instanceof TypeError` nor `RangeError`;
- anything else (a non-identifier, `Object`/`Array`, or a `let`-shadowed name) →
  a **clean span diagnostic**. jsc models no general prototype-chain `instanceof`.

Emission (`emit_instanceof_node`) guards on `emit_is_object` **first** — only a
heap object (type byte `TYPE_OBJECT`) can be an instance — so a non-heap or
non-object operand (`5`, `null`, an array, a `Map`) yields `false` **without
trapping** and without a false positive. (This deliberately uses an
object-type-byte guard rather than a `typeof (recv) === "object"` prefix: on the
current runtime a `typeof` result is a boxed *immediate*, and `===` only
byte-compares two heap *strings*, so `typeof x === "object"` is `false` for every
`x` — it cannot gate the test.) Both prototypes (user-class case) and the
`@@errorkind` marker (error case) are read via `__obj_get`, whose own-key scan
finds the receiver's own slot before any prototype walk. The needed keys are
interned by `collect_node` even when the program constructs no error/instance in
that position.

**(d) Error-aware rendering.** `format_value` special-cases a decoded object
carrying `@@errorkind`: it renders as its single-line stack `"<name>: <message>"`
(or `"<name>"` with no message), the way a JS console prints an error's first
line, instead of the generic `{ name: …, message: … }`. This changes an uncaught
error to `Uncaught Error: boom` and `console.log(new Error("x"))` to `Error: x`.

**(e) The internal runtime-guard `TypeError` becomes a real error.** The dynamic-receiver
*"called value is not a function"* guard ([the string-method / unknown-method-guard section](#string-method-surface--the-unknown-method-guard))
now throws a **branded `TypeError` object** (`internal_type_error`) instead of a
bare string, so `catch (e) { e instanceof TypeError }` holds and
`e.name`/`e.message`/`e.stack` read back. Its uncaught render is unchanged
(`Uncaught TypeError: called value is not a function`).

```js
new Error("boom").stack                                   => Error: boom
new Error().stack                                         => Error
new TypeError("t") instanceof Error                       => true
new Error("e") instanceof TypeError                       => false
5 instanceof Error                                        => false     // no trap
new Map() instanceof Error                                => false     // Map is not a byte-2 object
({ name: "Error", message: "x" }) instanceof Error        => false     // no marker
class C {} new C() instanceof C                            => true      // prototype hop
class C {} new C() instanceof Error                       => false
class Error {} new Error() instanceof Error               => true      // user class shadows builtin
Object.keys(new Error("x")).join(",")                     => name,message,stack
try { throw new TypeError("nope") } catch (e) { e.name }  => TypeError
```

**Known limitations (documented, acceptable for a learning compiler).** `.stack`
is single-line (no frames). `instanceof` covers user classes (a `@@prototype`
hop — generalized to a full chain walk for `extends` in [the typeof / extends section](#typeof-string-equality--class-extends-super-multi-hop-instanceof)),
the three builtin error constructors, and the runtime-type-byte builtins
`Array`/`Object`/`Function`/`Date` (added by the instanceof-builtins wave).
`.name`/`.message`/`.stack` remain **enumerable**
(real Error
properties are non-enumerable), so `Object.keys(err)` returns
`["name","message","stack"]` and `JSON.stringify(err)` includes them — a
consequence of the fixed-shape object model having no per-key enumerable flag.
`.stack` uses the `<name>: ` prefix whenever a constructor argument was given, so
a runtime-empty message still renders `"<name>: "` (only the syntactic no-arg
form drops the colon).

### `typeof` string equality + class `extends` (`super`, multi-hop `instanceof`)

Two follow-ons this work exposed: the `typeof` guard idiom did not work, and
`instanceof` (plus the whole class model) was single-level.

**(a) `typeof` now yields a real interned heap string (the immediate is retired).**
Previously a `typeof` result was a NaN-boxed **immediate** (tag `0b100`, low bits =
index into `TYPEOF_STRINGS`), a *second* string representation distinct from heap
strings (tag `0b101`). The strict-equality kernel only byte-compares two heap
strings and otherwise falls to a raw-bits identity compare, so `typeof x === "…"`
was **always `false`** — and every other string context (`+`, `.length`, …) would
have needed the same special-casing. Rather than teach each consumer about the
immediate, jsc **retires the immediate at the producer**: `emit_typeof_node` keeps
its `0..5` type-index computation but maps that index to the **interned
data-segment offset** of the matching word (a branchless nested-`select` fold) and
ORs `HEAP_BITS`, so a `typeof` result **is** an ordinary heap string. Interning
gives the same no-allocation property the immediate had. Consequences:

- `===`/`!==`, `+` concatenation, `.length`, indexing, and `console.log` all flow
  through the **existing string machinery with zero `typeof` special cases** — the
  entire leaky second representation (and its bug class) is gone.
- `collect_node` interns the six words whenever a program uses `typeof`.
- `Value::TypeStr` and the `0b100` decode arm are **deleted** — a `typeof` result
  decodes as a normal `Value::Str`; `format_value` / `str_to_number` lose their
  `TypeStr` arms; the truthiness path loses its dead non-heap `typeof` case (a
  `typeof` word is a non-empty heap string, hence truthy via the string path).
- NaN-box tag `0b100` is **retired** in `abi.rs` (like `0b110`/`0b111`).
  `abi::TYPEOF_STRINGS` remains the single source of the words shared by the
  emitter and the runtime.

**(b) `class B extends A`.** A derived class is parsed from a **named** superclass
declared earlier (other superclass expressions and an unknown name are clean span
diagnostics). The runtime model reuses the existing single-shared-prototype shape:

- **Inherited methods (free, via the prototype chain).** `B`'s shared prototype
  object gains a reserved `@@prototype` link to `A`'s prototype. `__obj_get`'s
  existing **prototype-fallback recursion** then walks the whole chain: `b.m` misses
  `B`'s own instance slots, follows the instance's `@@prototype` to `B`'s prototype,
  misses there, follows *its* `@@prototype` to `A`'s prototype, and resolves — so
  inherited and overridden methods just work with no new emit path.
- **Inherited fields.** `ClassInfo.fields` for a derived class is the parent's
  **flattened** field list followed by its own, so `new B()` pre-slots every field
  the whole constructor chain assigns (the object model has no dynamic key growth).
- **`super(args)`** lowers to `IrNode::ThisCall { callee: A["@@constructor"], args,
  this_local }` — a call that keeps the **current** `this` (the enclosing method's
  snapshot) rather than a fresh receiver, so the parent constructor's `this.f = …`
  mutates the same instance. **`super.m(args)`** is the same node with
  `callee = A["@@prototype"].m` (resolved up the parent's own chain). A subclass
  with **no constructor** reuses the parent's constructor value directly (an implicit
  forwarding `constructor(...a){ super(...a) }`); a subclass that declares fields but
  no constructor is rejected (the alias would skip its field init). `super` outside a
  derived class body, and a `super.m` that resolves to a non-function, are handled
  (a compile diagnostic and a branded runtime `TypeError` respectively). The
  superclass binding is force-captured into a cell so the lifted body can read it.
- **Multi-hop `instanceof`.** `InstanceOfKind::Class` now **walks** the receiver's
  `@@prototype` chain (a small `block`/`loop` over two dedicated scratch slots),
  matching the class's prototype at any depth: `new C() instanceof A` holds for
  `C extends B extends A`. The `emit_is_object` guard and the one-hop base case are
  unchanged, so non-object operands still yield `false` without trapping.

```js
typeof 1 === "number"                                     => true
typeof x === "string" ? "s" : "o"    // (x a string)      => s
"boolean" === typeof true                                 => true
class A{constructor(n){this.n=n} dbl(){return this.n*2}}
class B extends A{} new B(5).dbl()                        => 10       // inherited ctor + method
class A{greet(){return "A"}}
class B extends A{greet(){return super.greet()+"B"}}
new B().greet()                                           => AB       // override + super.m()
class A{} class B extends A{} class C extends B{}
new C() instanceof A                                      => true     // multi-hop
```

**Known limitations (documented).** The default forwarding constructor covers a
subclass with **no own fields**; a field-declaring subclass must write an explicit
constructor that calls `super()`. `super`/`instanceof` are lexical to the class
being lowered (no dynamic `[[HomeObject]]`). Field-initializer ordering relative
to `super()` is not modelled (jsc pre-allocates the instance, so `this` is always
available). (The builtin `instanceof Array`/`Object`/`Function`/`Date` are added
by the instanceof-builtins wave — see [the instanceof-builtins / Date section](#instanceof-built-ins--date).)

### Promises, microtasks & top-level `await`

The async wave opens with a **host-driven promise engine**. A promise's
*settlement* lives in the guest heap as a new block type
`TYPE_PROMISE` (`[type=8][pad×3][state:i32][value:i64]`, `state` ∈
pending/fulfilled/rejected) so a bare memory snapshot still renders it
(`Promise { 42 }`, `Promise { <pending> }`, `Promise { <rejected> reason }`); the
*scheduling* — each pending promise's reaction list and the FIFO **microtask
queue** — lives host-side in the `wasmtime` store data (`run::promise`), keyed by
the promise block's base offset. This mirrors the built-ins wave philosophy of reimplementing
complex operations host-side (`__convert`/`__method`), where dynamic data
structures are trivial and panic-free in Rust rather than hand-emitted wasm.

**Surface.** `Promise.resolve(v)` / `Promise.reject(v)` (recognized syntactically
like `Object`/`JSON`, only when unshadowed); `new Promise(executor)` (see below);
`p.then(onF, onR)` / `p.catch(onR)` (one chained result promise, passthrough for a
missing handler, rejection skips a fulfillment-only `.then`); `p.finally(fn)` (run
on both settlements, original settlement passed through unless `fn` throws); and
top-level **`await p`**.
`.then`/`.catch`/`.finally` are claimed for any receiver — a user object method of
those names is a documented collision — and the engine treats a non-promise
receiver as an already-fulfilled value (so `await 5` → `5`).

**Two new host imports + a trampoline.** `__promise(op, a, b)` backs
resolve/reject/await/finally; `__then(p, onF, onR)` backs then/catch
(`IMPORT_FUNC_COUNT` 3 → 5). Reaction callbacks are guest closures, so the engine
runs them by **re-entering the guest** through an exported `__invoke1(fn, arg)`
trampoline (a hand-emitted `call_indirect` under the uniform env-last ABI). A
callback that throws sets `$exc_pending` (the exceptions wave mechanism); `__invoke1` returns
normally and the engine reads + clears the flag to reject the chained promise.

**`new Promise(executor)` — native settler functions.** The constructor allocates
a pending promise `P`, then hands the executor `resolve`/`reject` as first-class
FUNCTION values pointing at two hand-emitted **settler** functions (appended to the
funcref table after the user functions) with `P` stored directly in their **env
slot** (not a closure environment). Calling `resolve(v)` therefore reaches
`__settle_resolve`, which reads `P` from its env and drives the host settle logic
(`__promise(SETTLE_FULFILL, P, v)`) — no bound-native-function value kind is
needed, just a repurposed env slot. The executor runs synchronously; if it throws,
the emitted constructor catches `$exc_pending` and rejects `P` (a throwing executor
rejects rather than propagating). `resolve`/`reject` may be stored and called later
from a microtask, which the `await`/after-`main` drain naturally reaches. (A
consequence: capture analysis must descend into an awaited expression so a closure
nested under `await new Promise(...)` still has its captures boxed.)

**No suspension needed.** There is no state-machine/CPS transform: `async function`
bodies stay rejected, so an `await` only ever appears at the **top level** (in
`main`), where "block until settled" is just "drive the microtask queue until the
awaited promise settles" — a synchronous host loop. Because the runtime has no
timers or I/O, the microtask is the *only* async primitive, so draining the queue
fully models the event loop: top-level `await` drains until its target settles
(surfacing a rejection through `$exc_pending`/`$exc_value` as an uncaught throw),
and the harness drains any remaining queue **after `main`** so unawaited `.then`
chains still run. Resolving a promise with another promise **adopts** it (chains
via a passthrough reaction).

**Known limitations (documented, deferred).**
`Promise.all`/`race`/`allSettled`, `async function` bodies (`await` inside a
function requires real suspension), thenable assimilation of non-promise objects,
and unhandled-rejection reporting are all out of scope here.

### Generators (state-machine transform)

A `function*` cannot use porffor's native fiber stack — jsc emits ONE stackless
wasm module — so `yield` is compiled by a **source→state-machine** transform.
Each generator declaration lowers to TWO ordinary uniform-ABI functions:

- a **constructor** (the name `g` binds to) that allocates a `TYPE_GENERATOR`
  handle `[type=9][sm_idx][state][done][env]` plus its **frame** — a `TYPE_ENV`
  block of boxed cells, one per generator local — and returns the handle WITHOUT
  running the body; and
- a **state machine** `SM(sent, gen, …, env)` whose body is a `br_table` dispatch
  on `gen.state` (read from the handle) into one **segment** per suspend point.

The frame reuses the closure-cell machinery: the body is lowered in an
`env_backed` frame so every local resolves to `EnvLoad + cell` (a `CellGet`/
`CellSet`), which is exactly what lets a local — and `++`/`+=`/`push` on it —
survive a `yield` (a `call_indirect` return). `gen.next(v)` (`IrNode::GenResume`)
drives the SM through `call_indirect`, passing the frame as the trailing `env`
argument, and builds the `{ value, done }` result as an ordinary object; a
non-generator receiver throws a branded `TypeError`.

**Straight-line bodies.** `yield` as a whole statement (`yield e;`), an
assignment RHS (`x = yield e`), or a returned value (`return yield e`); each
`yield` ends a segment (store next state, return the value); `return` / fall-off
sets `done = 1`.

**Control flow crossing a yield (CFG flattening).** The lowered body is built
into a **control-flow graph** of basic blocks: a straight-line run is one block,
and `if`/`while`/`for` (which desugar to `If`/`While`) crossing a `yield` are
flattened into blocks linked by `Jump` / `Branch` / `Yield` terminators, with
`break`/`continue`/`return` becoming jumps. The state machine is a
`loop { br_table (gen.state) }`: a `Yield`/`Return` block returns; a
`Jump`/`Branch` block stores the next block index and re-dispatches. A
self-contained inner control-flow region with no suspend and no escaping jump
keeps its ordinary wasm lowering (emitted verbatim inside a block).

**Consuming a generator.** `yield* inner` (delegation) desugars to a driving
`while` over `inner.next()`. `for (const x of gen)` drives the iterator protocol:
the iterable is evaluated once and a runtime `IsGenerator` test picks between
driving it via `.next()` (`{ value, done }`) and the ordinary array/string index
walk — the array path is unchanged. `[...gen]` / `f(...gen)` SPREAD drains a
generator into an array through the `__gen_to_array` guest helper (a
`call_indirect` drive loop collecting each yielded value; the generator's `return`
value is excluded), selected by the same runtime `IsGenerator` test so array
spread is untouched. A `for-of`/`for-in` whose OWN body `yield`s inside a generator
is rejected (its hidden index/length temporaries do not persist across a suspend;
use an indexed `for` over `.length`).

**Still rejected (deferred).** A `yield` inside `try`/`catch`/`finally` (handler
state across a suspend), inside a `switch`, or in a loop/`if` CONDITION; a `yield`
nested in a larger expression; a generator EXPRESSION / arrow / method; a nested
closure inside a generator body; and non-simple parameters. `.return()`/`.throw()`
are also deferred.

### `async`/`await` (a generator driven by a promise)

An `async function` is compiled as a **generator whose `await` is a `yield`**,
driven by a host-side promise engine — reusing the generator state-machine transform and
the microtask queue wholesale, so almost no new machinery is added. A top-level
`async function f(params) { … }` lowers to exactly the generator pair — a state machine
(the body, with `await e` lowered to `yield e`) plus a frame — but the name `f`
binds to a **wrapper** whose whole body is `return AsyncSpawn(MakeGenerator{…})`:
it builds the generator handle without running the body and hands it to the host,
which returns the async function's result **promise**. (`await e` becomes a
generator `yield e` under an `async_body` flag set while lowering the body; the CFG
segmentation then treats it identically to a `yield`, so all of the control-flow
flattening — `await` inside `if`/`while`/`for`, `break`/`continue`/`return` across
a suspend — works for free.)

**The host async driver.** `AsyncSpawn` lowers to the `__promise` import with a
new `PROMISE_OP_ASYNC_SPAWN` op; the operand is the fresh generator handle. The
host (`run::promise`) allocates the result promise `P`, then **drives** the
generator: `__async_step(gen, sent)` — a new exported trampoline, the single-step
sibling of the generator `__gen_to_array` drain — `call_indirect`s the state machine with
`(sent, gen)` and returns the yielded/returned value, leaving the handle's `done`
flag and any thrown exception (`__exc_pending`) for the host to read. Each step:

- **threw** → reject `P` with the thrown value (a synchronous `throw`, or an
  awaited rejection surfaced below);
- **done** → fulfill `P` with the return value (a returned promise is **adopted**,
  matching spec);
- **suspended** (a `yield`/`await`) → the yielded value is the awaited operand;
  register an **async continuation** on it (via `Promise.resolve` semantics —
  `register_source` with `async_gen = Some(gen)`), so when it settles the engine
  resumes the generator with the settled value (`drive_async` again) on fulfillment
  or rejects `P` on rejection.

The continuation rides the SAME microtask queue and reaction lists as `.then` — a
`Job`/`Reaction` simply carries an optional `async_gen` handle, and `run_job`
routes it to `drive_async` instead of a guest callback. So the first step runs
synchronously inside the `f()` call (re-entering the guest exactly as a `.then`
reaction does), and each `await` yields to the queue, which top-level `await` and
the after-`main` drain turn as usual. A synchronous (no-`await`) body therefore
returns an already-fulfilled promise; a body that awaits returns a pending one.

**Rejection of an awaited promise propagates straight out** (rejecting `P`) rather
than throwing back into the body: there is no `gen.throw`, and a `try`/`catch`
around an `await` is already rejected by the CFG (a `yield` inside `try`), so the
observable behaviour — an uncaught `await` rejection becomes the function's
rejection — is exactly right for the supported surface.

**Still rejected (deferred).** An async function EXPRESSION / arrow / method, an
`async function*` (async generator), and — inherited from the generator transform
— an `await` inside `try`/`switch`/a loop-or-`if` CONDITION, an `await` nested in a
larger expression (so `x += await e` is out; use `let t = await e; x += t`), and
non-simple parameters.

### Loose equality (`==` / `!=`) + `[...str]` string spread

**Loose equality `==`/`!=`.** `BinOp::LooseEq`/`LooseNe` (mapped from swc's `EqEq`/
`NotEq`) lower to the `__loose_eq(a, b) -> i32` runtime kernel, boxed to a boolean
jsval (negated for `!=`) with a `select` — the same shape as strict `===`. The
kernel implements JS **Abstract Equality** for jsc's value types: each boolean
operand is first **normalized to a number** (`true`→`1`, `false`→`0`); then if
either operand is `null`/`undefined` the result is `true` iff **both** are (nullish
values equal each other and nothing else); two numbers compare as `f64` (so
`NaN == NaN` is false); two strings by byte content (`__str_eq`); a number and a
string coerce the **string** to a number via the existing `__convert` `ToNumber`
host import and compare as `f64` (`1 == "1"`, `"" == 0` → true); anything else
compares by **raw identity bits** (two distinct objects differ, the same object is
equal). **Documented v1 cut:** object↔primitive `ToPrimitive` coercion is NOT
modeled, so `[] == 0` and `[1] == "1"` are `false` (not `true` as in spec) — a rare
footgun; the common `x == null`, `1 == "1"`, `true == 1` cases are exact. Strict
`===` and every other internal equality (optional-chaining nullish, destructuring
defaults, `switch`/`case`, Map/Set keys) stay strict and are untouched.

**`[...str]` string spread.** Spreading a **string** into an array literal now
explodes it into its characters. A pre-existing bug had `[...str]` fall through to
`__array_concat`, which assumed an array source and reinterpreted the string's
UTF-8 bytes as boxed jsvals (garbage floats). The `SpreadElem::Spread` emitter now
tests the source type at runtime: a STRING goes through the new
`__str_to_array(s) -> arr` helper (allocate a `TYPE_ARRAY` of `len` slots, fill
element `k` with `__str_index(s, k)` — a one-byte heap string), a GENERATOR through
`__gen_to_array`, and any other source (an array) straight through, all
funnelling into `__array_concat`. The character walk is **byte-indexed** (matching
for-of / indexing over a string), so multi-byte UTF-8 splits per byte — the same
limitation the rest of the string surface carries. `[..."abc"]` → `["a","b","c"]`.

**`JSON.parse` — un-deferring the earlier backlog item.** It had rejected `JSON.parse`
as needing dynamic object key-growth the fixed-shape object model lacks. That
reasoning was **too strong**: the fixed-shape constraint only blocks *mutating* an
existing object's key set (`__obj_set` writes existing slots only) — it never
blocked *allocating a fresh* object with a **runtime-determined entry table**. And
every object consumer (`__obj_get`, `__obj_has`, `for-in`, `Object.keys`) matches
keys by **byte content** (`__str_eq`), so a parsed object whose keys are runtime
heap strings resolves correctly against a literal access like `parsed.name` (which
interns `"name"` and compares by content). So `JSON.parse` is now a fully **in-wasm
recursive-descent parser** (`crates/jsc-core/src/emit/json_parse.rs`), consistent
with the in-wasm `JSON.stringify` — **no new host import**, so the browser
shim debt does not widen.

- **Cursor.** Three module globals set once at `__json_parse` entry —
  `$__json_data` (the input's UTF-8 payload offset), `$__json_pos`, `$__json_len` —
  drive the recursion. A parse runs synchronously to completion, so nothing
  re-enters and disturbs them.
- **Grammar.** `__json_value` skips whitespace then dispatches on the current byte:
  `{`→`__json_object`, `[`→`__json_array`, `"`→`__json_string`, `t`/`f`/`n`→a
  verified `true`/`false`/`null` literal, else `__json_number`. Numbers scan the
  token substring and reuse the existing `__convert` `ToNumber` import (as
  `JSON.stringify` reuses `ToString`). Strings decode into a fresh heap string,
  handling `\" \\ \/ \b \f \n \r \t`.
- **Runtime-key objects.** `__json_object` accumulates each `(key, value)` as two
  elements of a growable array via `__array_push`, then `__array_to_object`
  materializes a fresh `TYPE_OBJECT` block sized to the member count, copying the
  pairs into its entry table. This is the key primitive the earlier deferral said was
  missing — it just never required the *mutable* key-growth the deferral assumed.
- **The argument** is coerced to a string via the `__convert` `ToString` import
  (matching spec `ToString`), so `JSON.parse(42)` parses `"42"`.
- **Errors.** Malformed input calls `__json_throw`, which stashes a **branded
  `Error`** (`.name` `SyntaxError` — jsc has no `SyntaxError` kind, so it is branded
  base `Error`, and `e instanceof Error` holds) in `$exc_value` and unwinds via the
  the exceptions wave mechanism; each helper checks `$exc_pending` after a recursive call and
  propagates, and the emit site issues the final `emit_propagate_if_pending` so a
  `try`/`catch` around `JSON.parse` catches it. Covers an unexpected/absent token,
  a mismatched literal, an unterminated string/array/object, and **trailing
  content** after the value.
- **v1 cuts (documented).** String `\u` escapes are unsupported (they throw); the
  **reviver** argument is rejected at codegen; each parsed string over-allocates its
  worst-case decoded length on the bump heap (never reclaimed, §10); a JSON key
  literally beginning with `@` would be hidden from enumeration by the reserved-key
  guard (accessible only by explicit access).

### `instanceof` built-ins + `Date`

**Runtime-type-byte `instanceof`.** `instanceof` now recognizes four more
right-hand sides beyond user classes and the branded error constructors:
`Array`, `Function`, `Date`, and `Object`. Like the error names, they are matched
syntactically from an **unshadowed** bare identifier (a user `let Array = ..`
shadows the builtin and falls through to the clean compile error). Instead of a
`@@prototype` hop, they test the receiver's runtime **TYPE BYTE**
(`InstanceOfKind::Array`/`Function`/`Date`/`Object`, emitted by
`emit_type_byte_instanceof`): `Array`/`Function`/`Date` match their own byte, and
`Object` matches **any heap pointer whose byte is not `TYPE_STRING`** — so arrays,
plain objects, functions, maps, sets, promises, generators, errors, and dates are
all `instanceof Object`, while a primitive string/number/boolean/`null`/
`undefined` is not. A non-pointer operand is never an instance (the type-byte read
is gated behind an `is_pointer` check, so it never dereferences a bogus offset).

```js
[] instanceof Array                => true
[] instanceof Object               => true
(function(){}) instanceof Function => true
new Error("x") instanceof Object   => true
"s" instanceof Object              => false   // a primitive string is not an object
5 instanceof Object                => false
```

**`Date` — a heap block over an f64 millisecond timestamp.** A new block type
`TYPE_DATE` (`[type=10][pad×3][pad×4][ms:f64]`) carries the one f64 payload:
milliseconds since the Unix epoch, the same value `Date.now()` / `d.getTime()`
yield. `typeof aDate` is `"object"` (an unrecognized type byte maps to the base
"object" arm). Because a number jsval's raw i64 *is* its f64 bit pattern, the
payload moves between the block and a number jsval by a bare
`reinterpret` — no boxing.

- **Construction.** `new Date(ms)` stores `Number(ms)` (the argument is coerced
  through the existing `__convert` `ToNumber` import, so `new Date(true)` is `1`
  and `new Date("2020")` is `2020` — jsc parses **no** date strings, a documented
  cut). `new Date()` seeds the payload from the host wall clock. The
  calendar-field constructor (`new Date(y, m, d, …)`) and a spread argument are
  rejected at codegen.
- **The host clock — one new import.** `env.__now() -> f64` (import index 5, the
  first host import added since the promise engine's `__promise`/`__then`) returns the current
  time as ms since the epoch; the wasmtime harness backs it with
  `SystemTime::now()`. It backs both `new Date()` and the static `Date.now()`
  (which returns a plain number). This is jsc's **only** source of runtime
  non-determinism, so its tests assert structural properties (`typeof`,
  `instanceof`, monotonicity) rather than exact values.
- **`d.getTime()`.** Claimed for any receiver (like the promise/generator instance
  methods); a `TYPE_DATE` receiver's f64 payload is read back as a number, and any
  other receiver yields `NaN` (jsc models no method-receiver `TypeError` here — a
  documented cut).
- **The field methods — guest-side, no host round-trip.** `valueOf()`,
  `getFullYear()`, `getMonth()` (0-based), `getDate()`, `getDay()` (0=Sunday),
  `getHours()`, `getMinutes()`, `getSeconds()`, and `toISOString()` all compute
  **in wasm** from the stored f64 ms — the clock is the only host piece. They
  dispatch on the receiver TYPE BYTE exactly like the Map/Set methods
  (`IrNode::DateMethod`, `DateOp`): a `TYPE_DATE` block computes the field, and any
  other receiver **falls back to an ordinary user method call** `recv.<name>(args)`,
  so the names stay usable as plain user object methods. `valueOf() == getTime() ==`
  the stored ms. The calendar fields run the same civil-from-days algorithm as the
  renderer (floor-divide `ms` by `86_400_000` for the day count, `mod` for the
  time-of-day) over six dedicated i64 scratch slots; every field is **UTC**. A
  non-finite timestamp yields `NaN` (numeric) or `"Invalid Date"` (`toISOString`,
  an interned literal). `toISOString()` builds a fresh fixed-width 24-byte heap
  string (`YYYY-MM-DDTHH:MM:SS.mmmZ`) by writing each zero-padded digit at its known
  offset — an ordinary string that concatenates and has `.length`. Documented cut:
  the 4-digit year field assumes a year in `0..=9999`.
- **Rendering.** `console.log(new Date(0))` prints the ISO 8601 UTC string
  `1970-01-01T00:00:00.000Z`; the run stage decodes the payload to `Value::Date`
  and derives the civil date with Howard Hinnant's `civil_from_days` algorithm
  (correct for pre-epoch/negative timestamps via `div_euclid`/`rem_euclid`). A
  non-finite payload renders `Invalid Date`.

```js
new Date(0)                        => 1970-01-01T00:00:00.000Z
new Date(1710000000000)            => 2024-03-09T16:00:00.000Z
new Date(-86400000)                => 1969-12-31T00:00:00.000Z
new Date(1500).getTime()           => 1500
new Date(0) instanceof Date        => true
typeof Date.now()                  => number
new Date(1609459200000).getFullYear()  => 2021
new Date(0).getDay()               => 4          // 1970-01-01 was a Thursday
new Date(0).toISOString()          => 1970-01-01T00:00:00.000Z
```

**Browser-shim debt (documented).** `Date` adds the `env.__now` import to every
emitted module. The TypeScript browser shim (`packages/jsc`) has not tracked the
host imports since the promise engine (it still lacks `__promise`/`__then`), so it does not yet
supply `__now` either; wiring the promise/date host surface into the shim is
tracked separately from the Rust compiler campaigns.

## 9. Diagnostics (compile-time)

jsc reports **compile-time** errors — both **parse rejections** (swc refuses the
source) and **codegen rejections** (an unsupported construct, an undeclared variable,
a `const` reassignment) — in a **rustc/swc-style** block: an `error: <msg>` line, a
`--> <label>:LINE:COL` locator, the offending source line, and a `^` **caret** under
the span (a multi-column span underlines its full width, `^^^`).

```text
error: cannot assign to constant binding: c
  --> <eval>:1:14
   |
 1 | const c = 1; c = 2
   |              ^
```

The **label** names the source: `<eval>` for `-e` / `--eval` inline input, otherwise
the file path.

**Mechanism.** Every swc AST node carries a `Span` (a byte range into the source).
`jsc_core::error::CompileError { span: Option<Span>, msg }` is the plumbing that lets a
codegen failure hold onto the **offending node's span** at the error site; span-less
errors use `CompileError::msg`. Because `CompileError` is a `std::error::Error`, anyhow's
blanket conversion applies, so existing `?` / `anyhow!` sites keep compiling and the CLI
recovers the span with `err.downcast_ref::<CompileError>()`. The CLI — which holds the
original source string and the label at the print boundary — translates the span's
`BytePos` into a `line:col`, echoes the source line, and draws the caret; a span-less
error falls back to anyhow's bare message. **Parser errors route through the same path**:
the swc parse error's `.span()` is wrapped into a spanned `CompileError`, so an
unterminated `(` renders the same caret block rather than a locationless
`failed to parse source: …`.

**Scope.** Spans and carets apply to **compile-time errors only**. **Runtime** errors —
an uncaught exception surfaced as `Uncaught <value>` and wasm traps — are **out of
scope**: they carry **no source span** and render no caret block. The compile-time
diagnostics do **not** change any valid program's behavior or output.

---

## 10. Roadmap

The pipeline and IR are chosen so features are additive. Waves 1–5 below (the
foundational MVP waves) have all landed, and §8 documents **many further landed
campaigns past the exceptions wave** — the syntax / OOP / collections waves, the unknown-method guard,
Error-object identity, `typeof`/`extends`, **The async wave** (Promises, generators,
`async`/`await`), loose equality + string spread + `JSON.parse`, `instanceof`
built-ins + `Date`, and class **static** members + instance **getters/setters**. The
items below are the genuinely open follow-on waves.

**Landed.**

1. ✅ **`let` / variables** — identifier resolution, wasm locals; IR gained
   `Local` / `Assign` and `IrStmt::Decl`.
2. ✅ **`if` / conditionals** — `IrStmt::If`, wasm structured `if`/`else` blocks.
3. ✅ **`while` / loops** — `IrStmt::While`, wasm `block`/`loop` + `br_if`.
4. ✅ **functions** — `IrNode::Call` + multiple wasm functions; call convention
   formalized (index `0` = `main`, user functions `1..=n`).
5. ✅ **type model (jsval) + heap types** — the NaN-boxed `u64` `jsval` unifying
   number / boolean / null / undefined / heap pointer with raw `f64` shipped in
   **The value model**, and the heap types it unlocks followed: strings (**The string type**), arrays &
   objects (**The object & array types**), statements / operators / block scoping (**The operator & scoping wave**),
   first-class functions & closures (**The closures wave**), built-in functions & the runtime
   (**The built-ins wave**), and `throw` / `try` / `catch` / `finally` exceptions (**The exceptions wave**).
   Linear memory, the bump allocator (`__alloc`), data segments, and JS coercion
   (`+` string concat, `ToNumber`, truthiness) all arrived with this wave.

Each wave extended `IrNode`/`IrStmt` and the codegen/emit walks without
re-architecting the frontend (swc) or the host (wasmtime).

**Open (future waves).** These are the items §8 explicitly deferred past the exceptions wave; each
is additive in the same way:

1. **Self-contained wasm output** — the emitted module currently declares three
   host imports in the `"env"` namespace (`console.log`, `__convert`, `__method`)
   satisfied by the wasmtime `Linker`, so a program cannot run without jsc's host.
   A self-contained target would move those helpers into the guest (or emit a
   WASI/component wrapper) so the `.wasm` runs standalone.
2. **Memory reclamation / GC** — every heap type is bump-allocated (`__alloc`) with
   **no free**; `__realloc` abandons the old block. A push-heavy or closure-heavy
   loop can exhaust the single page. A collector (or at least page growth + reuse)
   is the next memory-model step.
3. **Array out-of-bounds assignment semantics** — OOB *reads* already match JS
   (`a[999]` → `undefined`), but `arr[i] = v` writes an **existing index only**;
   growing an array by assigning past its length (sparse growth) is deferred.
4. **Host/guest semantic de-duplication** — The built-ins wave's `__convert` / `__method` host
   imports reimplement coercion, formatting, and heap allocation host-side
   (mirroring the guest `__alloc` and decode paths). Consolidating so one
   implementation backs both sides removes a class of drift bugs.
5. **Differential testing** — there is no oracle harness cross-checking jsc output
   against a reference JS engine; adding one would catch semantic divergences the
   hand-written tests miss.
6. **Dynamic object key-growth (computed keys)** — objects are **fixed-shape**: keys are interned static-string pointers reserved by a compile-time field
   scan, and `__obj_set` can only write slots that scan already created — it cannot
   add a key. This is why **computed property keys** `{[k]: v}` are still rejected.
   (`JSON.parse` — once blocked by this — has since **shipped**: because
   `__obj_get` compares keys **by byte content**, a parser can build objects with
   runtime-allocated keys without any fixed-shape scan; see the loose-equality section. Computed literal keys
   would still want a growable key table or a hash-map object representation.)
7. **`RegExp`** — regex literals `/…/` and `new RegExp(…)` are rejected; a pattern
   engine (e.g. a host-backed `regex` import) is a self-contained follow-on.
