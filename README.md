# jsc

An ahead-of-time JavaScript/TypeScript → WebAssembly compiler in Rust, conformance tracked by [test262](https://github.com/tc39/test262).

```console
$ jsc -p "[1,2,3,4].filter(x => x % 2 === 0).map(x => x * 10).reduce((a,b) => a+b, 0)"
60

$ jsc -e 'console.log("sum", [1,2,3].reduce((a,b) => a+b, 0))'
sum 6
```

## What it is

- **A compiler, not an interpreter.** Every program becomes a validated WebAssembly module (emitted with [`wasm-encoder`](https://crates.io/crates/wasm-encoder)). The CLI runs it on [`wasmtime`](https://wasmtime.dev/) or writes the bytes out with `jsc build`.
- **An embeddable library.** The whole pipeline lives in the `jsc-core` crate; `jsc` is a thin CLI over it.
- **An in-browser compiler.** `crates/jsc-wasm` compiles `jsc-core` itself to wasm (wasm-bindgen); the `@cdlab/jsc` package (`packages/jsc`) wraps it with `compile(src)` / `run(src)` for the browser and Node, and `apps/jsc-web` is a playground built on that package.
- **TypeScript by type stripping only** — no type checking. `.ts` / `.mts` / `.cts` files have their types removed by swc's TypeScript strip pass before codegen; see [TypeScript](#typescript).

## Install

From source (Rust 1.99+):

```bash
git clone https://github.com/cdLab996/jsc.git
cd jsc
cargo install --path crates/jsc      # installs the `jsc` binary
# or: cargo build --release          # -> target/release/jsc
```

Pushing a `v*` tag builds a static Linux x86_64 binary (`jsc-linux-x64.tar.gz`) and attaches it to a [GitHub Release](https://github.com/cdLab996/jsc/releases); other platforms build from source.

The browser package: `bun install`, then `bun run build:wasm` (builds `crates/jsc-wasm` into `packages/jsc/wasm`) and `bun run dev:web` for the playground.

## Usage

`jsc` follows `node`'s command-line shape:

```bash
jsc program.js                    # compile and run a file; prints only console output
jsc -e 'console.log(40 + 2)'      # compile and run an inline source string
jsc -p '40 + 2'                   # run inline source and print its completion value
jsc build program.js -o out.wasm  # write the WebAssembly module (-o - writes to stdout)
jsc program.ts                    # TypeScript: types are stripped, not checked
```

`console.log` streams to stdout while the module runs; `-p` prints the completion value afterwards, Node-style (`[ 1, 4, 9 ]`). Exactly one of FILE, `-e`, `-p` is accepted.

| Exit code | Meaning |
| --------- | ------- |
| `0` | success |
| `1` | uncaught exception (`Uncaught <value>` on stderr), runtime trap, or I/O failure |
| `2` | compile error (a caret diagnostic pointing at the source span) or invalid command line |

```console
$ jsc -e 'throw new TypeError("boom")'
Uncaught TypeError: boom
$ echo $?
1
```

### TypeScript

The dialect is chosen by FILE extension:

- `.ts`, `.mts`, `.cts` — TypeScript. swc parses the file as TypeScript and its strip pass erases type syntax (annotations, `interface`, `type`, generics, `as`, `!`); nothing is type-checked.
- `.tsx` — rejected (exit `2`); JSX is not supported.
- any other extension, and all `-e` / `-p` input — JavaScript, so type syntax there is a compile error.

`enum` and `namespace` are lowered by the strip pass to `var`-based JavaScript and run like any other declaration. Invalid input in either dialect — including errors swc recovers from, such as strict-mode early errors — is a compile error (exit `2`). The `@cdlab/jsc` package compiles JavaScript only.

## Conformance

test262 is the primary metric. `crates/jsc-test262` runs the official suite at a pinned commit against the release `jsc` binary:

- `scripts/test262-fetch.sh` checks out tc39/test262 into `.tmp/test262`.
- Tests run against the official harness files in that checkout, unmodified: `"use strict";` + `sta.js` + `assert.js` + the test's includes + the test source, compiled and run as `jsc FILE`. Includes that do not compile make their tests ineligible.
- `tests/test262/unsupported-features.txt` lists the feature tags that make a test ineligible (Symbol, Proxy, Reflect, BigInt, RegExp, typed arrays, WeakMap/WeakRef, Intl, Temporal, …). Tests flagged `module` or `noStrict` (and `raw` tests flagged `async`) are ineligible as well; other `async` tests run with the official `doneprintHandle.js` and pass on `Test262:AsyncTestComplete`.
- `tests/test262/baseline.txt` is the set of passing tests; `tests/test262/history.tsv` records `pass` / `eligible` / `total` per commit from a full run (`--history`).

Current result (last row of `tests/test262/history.tsv`): **19378 pass / 30014 eligible (64.6%) / 47665 total**.

```bash
bash scripts/test262-fetch.sh
cargo build --release -p jsc
cargo run --release -p jsc-test262                      # full run with a per-directory report
cargo run --release -p jsc-test262 -- built-ins/Array   # only tests under a path prefix
cargo run --release -p jsc-test262 -- --check           # fail if any baseline test regressed
cargo run --release -p jsc-test262 -- --update-baseline # rewrite baseline.txt (only the filtered prefixes, if any)
cargo run --release -p jsc-test262 -- --history         # append a row to history.tsv
```

CI (`.github/workflows/`):

- `ci.yml` — manual only (`workflow_dispatch`: the Actions tab or `gh workflow run ci.yml --ref <branch>`): `cargo fmt --check`, `typos`, `cargo clippy` (warnings denied), `cargo nextest`, doctests, `cargo shear`, `cargo deny`; plus a test262 job that fetches test262, runs the M1 harness tests (`crates/jsc/tests/test262_harness.rs`: the official `sta.js` + `assert.js` compile and run), and runs `jsc-test262 --check` against the baseline.
- `web.yml` — on push to `main` and pull requests that touch `crates/**`, `packages/**`, `apps/**`, `scripts/**`, Cargo or bun manifests (and manual dispatch): builds `@cdlab/jsc`, runs `biome` lint, builds the playground, and runs the package tests.
- `release.yml` — on `v*` tags (and manual dispatch): builds the static musl binary and publishes the release.

## Supported language

| Area | What works |
| ---- | ---------- |
| **Values** | `number`, `boolean`, `undefined`, `null`, `string`, arrays, objects, functions, `Map`, `Set`, `Promise`, `Date`, generators |
| **Operators** | comma, arithmetic, bitwise and shift, comparison, `==`/`===` and negations, logical and `??`, `typeof`, `instanceof`, `in`, `delete`, compound assignment, `++`/`--`, optional chaining `?.`; `ToPrimitive` / `ToNumber` / `ToString` call user `valueOf` / `toString` |
| **Bindings** | `var` hoisted to function scope (top-level `var` and function declarations are global-object properties), block-scoped `let`/`const` (assigning a `const` is a compile error); array/object destructuring with defaults and rest; spread in array literals and calls |
| **Objects** | dynamic properties with a prototype chain and property descriptors (writable / enumerable / configurable, accessors), extensibility, `seal` / `freeze`; object literals with computed keys, methods, getters/setters and `__proto__`; arrays with holes (read through the prototype chain), growth by index, sparse storage and `length` writes |
| **Control flow** | `if`, `for`, `while`, `do`/`while`, `for-of`, `for await`, `for-in`, `switch`, labels, ternary |
| **Functions** | declarations, expressions, arrows, closures (capture by reference), default and rest parameters, `arguments`; functions are objects; `call`/`apply`; missing arguments are `undefined`, extra arguments are ignored; generator and `async` functions, expressions, methods and async generators, top-level `await`; `bind`; parameter TDZ |
| **Classes** | class declarations and expressions, fields, methods, constructors, `extends` any constructor (including builtins)/`super`, `new.target`, static methods and fields, getters/setters |
| **Built-ins** | function objects on real prototypes, usable as values: `console.log`, `Math`, `Object`, `String`/`Number`/`Boolean` (and wrapper objects), `parseInt`/`parseFloat`, `isNaN`/`isFinite`, `globalThis`, `Object` reflection (`defineProperty(ies)`, `getOwnPropertyDescriptor(s)`, `create`, `getPrototypeOf` / `setPrototypeOf`, `assign`, `keys` / `values` / `entries`, …), the ES2025 `Array`, `String`, `Number` and `Math` methods, `Map`/`Set`, `JSON.parse`/`stringify` (reviver, replacer, `space`), `Date` |
| **Async** | `Promise` (`new Promise`, `resolve`/`reject`, `.then`/`.catch`) with a microtask queue |
| **Errors** | `throw`, `try`/`catch`/`finally`; `Error` and the six native error types with `AggregateError`, with `.name`, `.message`, `cause`, `.stack` and `instanceof`; engine errors (`null` member access, calling a non-function, unresolved names, …) are catchable error objects; running out of memory ends the run with `RangeError` |

Unsupported syntax is rejected at compile time with a source-span diagnostic; calling a missing method throws `TypeError` at run time.

## How it works

```
source ──swc──▶ AST ──codegen──▶ IR ──emit──▶ .wasm ──run──▶ result
```

- **frontend** — parses source with [`swc`](https://swc.rs/).
- **codegen** — lowers the AST to a small IR: scopes, closure capture analysis, generator/async state machines. Top-level bindings captured by top-level functions live in module cells.
- **emit** — IR → wasm. Every value is a NaN-boxed `i64` (numbers are raw `f64` bits, with NaN canonicalized on every rebox through `__box_f64`; other kinds are tagged in the quiet-NaN space). Strings, arrays, objects, closures and environments live in linear memory behind a type-byte header, allocated by `__alloc`, a bump allocator that grows memory on demand up to 512 MiB. Objects keep their own properties in interned-key tables and link a prototype; builtins live on a static realm image, with guarded inline fast paths for common calls. Exceptions use manual unwinding (no wasm exception-handling proposal). `Map`/`Set` keys compare with SameValueZero (`__same_value_zero`).
- **run** — instantiates the module on `wasmtime`, provides the host imports, drains the microtask queue, and formats the result.

Details are in [DESIGN.md](DESIGN.md).

## Current limitations

- **Strict-mode semantics only.** Sloppy-mode constructs such as `with` or assigning an undeclared variable are compile errors.
- **Language gaps.** No object spread, private `#` members, static blocks, or computed class member names.
- **No GC.** The heap is bump-allocated and never freed; the 512 MiB cap ends a run with `RangeError: out of memory`.
- **Not self-contained.** The module imports seven host functions from the `env` namespace (`print`, `__convert`, `__method`, `__promise`, `__then`, `__now`, `__parse_int`), provided by the `wasmtime` host in `jsc-core` and by the JS shim in `@cdlab/jsc`; it is not a WASI module.
- **Byte strings.** Strings are byte sequences, not UTF-16: `"é".length` is `2`; string methods throw `TypeError` where a non-ASCII position would otherwise be wrong.
- **Missing built-ins.** No `RegExp` (regex literals are rejected), `Symbol`, `Proxy`, `Reflect`, `BigInt`, `WeakMap`/`WeakSet`/`WeakRef`, typed arrays, `eval` or the `Function` constructor; no generic iteration protocol (`Symbol.iterator`); no ES modules.

## Roadmap

The built-in library and UTF-16 strings, self-contained WASI modules with GC, the remaining language features (ES modules, `RegExp`, `Symbol`, …), performance work, and productization; see [DESIGN.md](DESIGN.md#14-roadmap).

## License

[Apache-2.0](LICENSE).
