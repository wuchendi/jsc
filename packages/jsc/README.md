# @cdlab/jsc

Compile JavaScript to WebAssembly and run it — in the browser or Node — with no
native runtime. The compiler is the Rust `jsc-core` pipeline (parse → codegen →
emit) shipped as a wasm-bindgen module (`crates/jsc-wasm`); the emitted program
module is a pure WASI preview-1 command that runs on a small in-memory polyfill.

## Usage

```ts
import { compile, run } from '@cdlab/jsc'

// Emit the WebAssembly module bytes for a program.
const bytes: Uint8Array = await compile('1 + 2')

// Compile, instantiate, run, and capture the program's output.
const { stdout, stderr, exitCode, completion } = await run(
  "console.log('hi'); 1 + 2",
  { printCompletion: true },
)
// stdout === 'hi\n', stderr === '', exitCode === 0
// completion === '3'
```

Both `compile` and `run` are async: the wasm-bindgen glue (wasm-pack
`--target web`) initializes with an async `init()`, cached after the first call.

- `compile(src, options?)` throws a JS `Error` on a compile failure. When the diagnostic
  carries a source span, the message is the compiler's rustc/swc-style caret
  block, so a browser can show exactly where compilation failed.
- `run(src, options?)` resolves to `{ stdout, stderr, exitCode, completion? }`.
  `completion` is present only under `printCompletion` on a clean exit.
- An uncaught exception or unhandled rejection does not reject: it is the
  `Uncaught ...` line on `stderr` and `exitCode` 1. `run` rejects on a compile
  error or a guest trap.

## Runtime

The emitted module is a WASI command: it imports exactly four
`wasi_snapshot_preview1` functions (`fd_write`, `clock_time_get`,
`random_get`, `proc_exit`) and exports exactly `_start` and `memory`. The whole
runtime (console rendering, the `Uncaught ...` reports, the promise engine, the
completion value) lives inside the module, so there is no JS-side runtime.

`run()` instantiates it against `src/wasi.ts`, an in-memory polyfill of those
four functions that captures fd 1 and fd 2 as strings; any other import is
absent, so a module that needs one fails to instantiate. The same polyfill is
used in Node and the browser.

A `jsc build FILE -o OUT.wasm` artifact also runs under any other preview-1
host, e.g. `node:wasi`:

```js
import { readFile } from 'node:fs/promises'
import { WASI } from 'node:wasi'

const wasi = new WASI({ version: 'preview1', returnOnExit: true })
const module = await WebAssembly.compile(await readFile('OUT.wasm'))
const instance = await WebAssembly.instantiate(module, wasi.getImportObject())
const exitCode = wasi.start(instance)
```

`tests/run.test.ts` exercises the polyfill end to end against the compiled
`jsc-wasm` module.
