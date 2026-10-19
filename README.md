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

From source (Rust 1.96+):

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

`enum` and `namespace` are not supported: the strip pass lowers them to `var`-based JavaScript that codegen rejects with an `undeclared variable` compile error. Invalid input in either dialect — including errors swc recovers from, such as strict-mode early errors — is a compile error (exit `2`). The `@cdlab/jsc` package compiles JavaScript only.

## Conformance

test262 is the primary metric. `crates/jsc-test262` runs the official suite at a pinned commit against the release `jsc` binary:

- `scripts/test262-fetch.sh` checks out tc39/test262 into `.tmp/test262`.
- `tests/test262/harness/` holds the harness includes; `tests/test262/unsupported-features.txt` lists the feature tags that make a test ineligible (Symbol, Proxy, Reflect, BigInt, RegExp, typed arrays, WeakMap/WeakRef, Intl, Temporal, …). Tests flagged `module`, `async`, or `noStrict` are ineligible as well.
- `tests/test262/baseline.txt` is the set of passing tests; `tests/test262/history.tsv` records the pass count per commit.

Current result (last row of `tests/test262/history.tsv`): **5230 pass / 22992 eligible (22.7%) / 47665 total**.

```bash
bash scripts/test262-fetch.sh
cargo build --release -p jsc
cargo run --release -p jsc-test262                      # full run with a per-directory report
cargo run --release -p jsc-test262 -- built-ins/Array   # only tests under a path prefix
cargo run --release -p jsc-test262 -- --check           # fail if any baseline test regressed
cargo run --release -p jsc-test262 -- --update-baseline # rewrite baseline.txt
cargo run --release -p jsc-test262 -- --history         # append a row to history.tsv
```

CI (`.github/workflows/`):

- `ci.yml` — on push to `main` and on pull requests that touch Rust paths (`crates/**`, Cargo manifests and lockfile, toolchain/lint configs, `tests/test262/**`, `scripts/test262-fetch.sh`): `cargo fmt --check`, `typos`, `cargo clippy` (warnings denied), `cargo nextest`, doctests, `cargo shear`, `cargo deny`; plus a test262 job that runs `jsc-test262 --check` against the baseline.
- `web.yml` — on push to `main` and pull requests that touch `crates/**`, `packages/**`, `apps/**`, `scripts/**`, Cargo or bun manifests (and manual dispatch): builds `@cdlab/jsc`, runs `biome` lint, builds the playground, and runs the package tests.
- `release.yml` — on `v*` tags (and manual dispatch): builds the static musl binary and publishes the release.

## Supported language

| Area | What works |
| ---- | ---------- |
| **Values** | `number`, `boolean`, `undefined`, `null`, `string`, arrays, objects, functions, `Map`, `Set`, `Promise`, `Date`, generators |
| **Operators** | arithmetic, comparison, `==`/`===` and negations, logical and `??`, `typeof`, `instanceof`, `in`, `delete`, compound assignment, `++`/`--`, optional chaining `?.`; ToNumber coercion for arithmetic, unary, update and relational operators |
| **Bindings** | block-scoped `let`/`const` (assigning a `const` is a compile error); array/object destructuring with defaults and rest; spread in array literals and calls |
| **Control flow** | `if`, `for`, `while`, `for-of`, `for-in`, `switch`, labels, ternary |
| **Functions** | declarations, arrows, closures (capture by reference), default and rest parameters; missing arguments are `undefined`, extra arguments are ignored; `function*` generators; `async` functions and top-level `await` |
| **Classes** | fields, methods, constructors, `extends`/`super`, static methods and fields, instance getters/setters |
| **Built-ins** | `console.log`, `Math`, `String`/`Number`/`Boolean`, `parseInt`/`parseFloat`, common array and string methods, `Object.keys`/`values`/`entries`, `Map`/`Set`, `JSON.parse`/`stringify`, `Date` |
| **Async** | `Promise` (`new Promise`, `resolve`/`reject`, `.then`/`.catch`) with a microtask queue |
| **Errors** | `throw`, `try`/`catch`/`finally`; `Error`/`TypeError`/`RangeError` with `.name`, `.message`, `.stack` and `instanceof`; member access on `null`/`undefined` throws a catchable `TypeError`; running out of memory throws `RangeError` |

Unsupported syntax, and unknown methods on statically known receivers, are rejected at compile time with a source-span diagnostic; an unknown method on a dynamically shaped receiver throws `TypeError` at run time.

## How it works

```
source ──swc──▶ AST ──codegen──▶ IR ──emit──▶ .wasm ──run──▶ result
```

- **frontend** — parses source with [`swc`](https://swc.rs/).
- **codegen** — lowers the AST to a small IR: scopes, closure capture analysis, generator/async state machines. Top-level bindings captured by top-level functions live in module cells.
- **emit** — IR → wasm. Every value is a NaN-boxed `i64` (numbers are raw `f64` bits, with NaN canonicalized on every rebox through `__box_f64`; other kinds are tagged in the quiet-NaN space). Strings, arrays, objects, closures and environments live in linear memory behind a type-byte header, allocated by `__alloc`, a bump allocator that grows memory on demand up to 512 MiB. Exceptions use manual unwinding (no wasm exception-handling proposal). `Map`/`Set` keys compare with SameValueZero (`__same_value_zero`).
- **run** — instantiates the module on `wasmtime`, provides the host imports, drains the microtask queue, and formats the result.

Details are in [DESIGN.md](DESIGN.md).

## Current limitations

- **Strict-mode semantics only.** Sloppy-mode constructs such as `with` or assigning an undeclared variable are compile errors.
- **Fixed-shape objects.** Computed keys (`{[k]: v}`), object spread, object-literal accessors and private class fields are rejected; adding a property to an existing object or assigning past the end of an array is a silent no-op.
- **No GC.** The heap is bump-allocated and never freed; the 512 MiB cap ends a run with `RangeError: out of memory`.
- **Not self-contained.** The module imports six host functions from the `env` namespace (`print`, `__convert`, `__method`, `__promise`, `__then`, `__now`), provided by the `wasmtime` host in `jsc-core` and by the JS shim in `@cdlab/jsc`; it is not a WASI module.
- **Byte strings.** Strings are byte sequences, not UTF-16: `"é".length` is `2`.
- **Missing built-ins.** No `RegExp` (regex literals are rejected), `Symbol`, `Proxy`, `Reflect`, `BigInt`, `WeakMap`/`WeakSet`/`WeakRef`, typed arrays, `eval`, or `Function.prototype.call`/`apply`/`bind`; no ES modules.
- **Restricted forms.** Class and generator expressions, async arrows and async generators are rejected.

## Roadmap

A generic object model with a dynamic property-lookup fallback, UTF-16 strings, a JavaScript prelude for built-ins, self-contained WASI modules, GC, and ES modules.

## License

[Apache-2.0](LICENSE).
