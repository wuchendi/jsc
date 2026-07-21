# jsc

A from-scratch, ahead-of-time **JavaScript → WebAssembly** compiler in Rust. No JIT, no interpreter — `jsc` parses your source, lowers it through a small IR, emits a real `.wasm` module, and runs it, all in one process.

```console
$ jsc -e "[1,2,3,4].filter(x => x % 2 === 0).map(x => x * 10).reduce((a,b) => a+b, 0)"
60

$ jsc -e 'function counter(){ let c = 0; return () => ++c } let n = counter(); n(); n(); n()'
3

$ jsc -e 'let xs = [1,2,3]; console.log("sum", xs.reduce((a,b)=>a+b,0)); xs.map(x => x*x)'
sum 6
[ 1, 4, 9 ]
```

## Why

`jsc` is a **teaching compiler** — small enough to read end to end, real enough to compile closures, arrays, and exceptions down to WebAssembly. It's built to show how a dynamic language actually reaches a static target:

- **Real backend, not a tree-walker.** Every program becomes a validated `wasm` module (via [`wasm-encoder`](https://crates.io/crates/wasm-encoder)) and runs on [`wasmtime`](https://wasmtime.dev/) — you can dump the bytes with `--emit wasm`.
- **One value, cleverly packed.** All runtime values are a single **NaN-boxed `i64`**: numbers ride as raw `f64` bits; `true`/`false`/`null`/`undefined` and heap pointers are tagged in the quiet-NaN space. No tagged unions, no per-type boxing.
- **A real heap.** Strings, arrays, objects, functions, and closure environments live in a **bump-allocated linear-memory heap** behind a 4-byte type-byte header.
- **Closures that actually close.** First-class functions compile to a `funcref` table + `call_indirect` with a uniform env-last ABI; captured variables are boxed cells, captured by reference.
- **A front end you don't have to write.** Parsing is [`swc`](https://swc.rs/) (the same parser production tools use); `jsc` owns everything after the AST.

It is a deliberately bounded subset (see [Non-goals](#non-goals)), documented campaign by campaign in [`DESIGN.md`](DESIGN.md).

## Install

From source (needs Rust 1.96+):

```bash
git clone https://github.com/cdLab996/jsc.git
cd jsc
cargo install --path crates/jsc      # installs the `jsc` binary
# or just: cargo build --release     # -> target/release/jsc
```

A prebuilt static Linux x86_64 binary is attached to each [GitHub Release](https://github.com/cdLab996/jsc/releases) (push a `v*` tag to build it). On macOS and Windows, build from source — `cargo` cross-compiles cleanly.

## Usage

```bash
jsc -e "40 + 2"                         # evaluate an inline program
jsc path/to/program.js                  # run a program from a file
jsc -e "40 + 2" --emit wasm > out.wasm  # emit the raw WebAssembly module instead of running
```

`jsc` prints the program's **final expression value** — numbers bare, strings unquoted, arrays and objects Node-style. `console.log(...)` streams to stdout **during** execution (via a host import), before that final line. An **uncaught exception** prints `Uncaught <value>` to stderr and exits non-zero:

```console
$ jsc -e 'let r = 0; try { throw new Error("boom") } catch (e) { r = e.message } r'
boom

$ jsc -e 'throw "kaboom"'
Error: Uncaught kaboom
$ echo $?
1
```

## Supported language

| Area | What works |
| ---- | ---------- |
| **Values** | `number` (f64), `boolean`, `undefined`, `null`; heap `string`, `array`, `object`, `function`, `Map`, `Set`, `Promise`, `Date`, generator |
| **Operators** | `+ - * / % **`, `< <= > >=`, `== != === !==`, `&& \|\| ! ??`, `typeof`, `instanceof`, unary `-`/`!`, string `+`, compound `+= … **=`, `++`/`--` |
| **Bindings** | `let` / `const` with **block scoping**; reassigning a `const` is a **compile error**; array/object **destructuring** with defaults & rest; **spread** `...` (arrays, calls, strings) |
| **Control flow** | `if`/`else`, `for`, `while`, `for-of`, `for-in`, `switch`/`case`/`default`/`break`, ternary `?:`, optional chaining `?.` |
| **Functions** | declarations, arrow functions, **closures** (capture-by-reference), recursion, higher-order, default & rest params; **`function*`/`yield`/`yield*`** generators; **`async`/`await`** |
| **Classes** | fields, methods, `constructor`, **`extends`/`super`**, **static** methods & fields, instance **getters/setters**, multi-hop `instanceof` |
| **Built-ins** | `console.log`; `Math.*`; `String`/`Number`/`Boolean`/`parseInt`/`parseFloat`; array `push`/`pop`/`map`/`filter`/`reduce`/`forEach`/`indexOf`/`includes`/`join`/`slice`; string `toUpperCase`/`toLowerCase`/`indexOf`/`slice`/`charAt`/`trim`/`repeat`/`startsWith`/`endsWith`/`padStart`/`padEnd`/`substring`/`split`; `Object.keys`/`values`/`entries`, `in`, `delete`; `Map`/`Set`; `JSON.stringify`/`parse`; `Date` (`now`/`getTime`/`getFullYear`…/`toISOString`) |
| **Async** | `Promise` (`new Promise`, `resolve`/`reject`, `.then`/`.catch`), a microtask queue drained at program end, top-level `await`, and `async`/`await` functions |
| **Errors** | `throw`, `try`/`catch`/`finally` (`finally` runs on every exit); `Error`/`TypeError`/`RangeError` with `.name`/`.message`/`.stack` and real `instanceof` identity; the engine's own `TypeError` is catchable |

## How it works

```
source ──swc──▶ AST ──codegen──▶ IR ──emit──▶ .wasm ──wasmtime──▶ result
        frontend.rs    codegen/      ir.rs   emit/          run/
```

The compiler core is the **`jsc-core`** crate; **`jsc`** is a thin `clap` CLI over it.

- **`frontend`** — wraps `swc` to turn source text into an AST.
- **`ir`** — a small enum IR the AST lowers into.
- **`codegen`** — AST → IR: scopes, operators, functions, closure capture analysis.
- **`emit`** — IR → WebAssembly bytes with `wasm-encoder`: the NaN-box value model, the linear-memory heap + bump allocator, and the runtime helpers.
- **`run`** — loads the module on `wasmtime`, wires the host imports (`console.log`, conversions, array/string methods, the promise engine, the wall clock), drains the microtask queue, and formats the result.

Exceptions use a portable **manual-unwinding** model (two module globals + a codegen handler-label stack + a call-site check-and-propagate) rather than the wasm exception-handling proposal. See [`DESIGN.md`](DESIGN.md) for the bit layout, heap format, ABI, and per-campaign notes.

## Non-goals

`jsc` targets a growing-but-fixed subset; these remain **out of scope**:

- No **`RegExp`**; no `Symbol`, `Proxy`, `Reflect`, `WeakMap`/`WeakRef`, or `BigInt`.
- **Computed property keys** `{[k]: v}`, **static** accessors, and **object-literal** getters/setters are rejected (the object model is fixed-shape); array **out-of-bounds assignment** does not grow the array.
- The emitted `.wasm` **declares host imports** (`console.log`, conversions, the promise/date/JSON helpers) satisfied by jsc's `wasmtime` host — it is **not** self-contained; there is **no GC** (bump-allocated, no free).
- Accessors and the engine's built-in errors dispatch on **statically-known** shapes where noted; a few low-level faults are still wasm traps rather than catchable exceptions.
- Strings are ASCII-first (byte-indexed `.length`); no full UTF-16/Unicode semantics. `.stack` is a single `"Name: message"` line (no frames).

## License

[Apache-2.0](LICENSE).
