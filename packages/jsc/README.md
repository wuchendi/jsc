# @cdlab/jsc

Compile JavaScript to WebAssembly and run it — in the browser or Node — with no
native runtime. The compiler is the Rust `jsc-core` pipeline (parse → codegen →
emit) shipped as a wasm-bindgen module (`crates/jsc-wasm`); the emitted program
module runs on a small in-memory WASI polyfill plus a JS shim.

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

## Host parity

`run()` executes the emitted command module without the Rust `wasmtime` runner:

- `src/wasi.ts` is an in-memory `wasi_snapshot_preview1` polyfill (`fd_write`,
  `clock_time_get`, `random_get`, `proc_exit`) that captures stdout and stderr.
  Console output, the `Uncaught ...` reports and the completion value are
  rendered inside the module, so there is no formatter on the JS side.
- `src/shim.ts` ports the six `env` imports (`__convert` / `__method` /
  `__promise` / `__then` / `__now` / `__parse_int`) and the promise engine
  (microtask queue) from `crates/jsc-core/src/run/host.rs` + `run/promise.rs`.
  Every heap block it creates is allocated by the guest's exported constructors
  (`__new_string` / `__new_promise`).
- `src/value.ts` ports the NaN-box decoder and `ToString` text those imports
  need from `crates/jsc-core/src/run/value.rs`.
- `src/abi.ts` copies the runtime ABI constants from
  `crates/jsc-core/src/abi.rs`.

Every module imports those four WASI functions and six `env` functions, so it
instantiates here and under the Rust runner, but not yet under a plain WASI
host. The Rust files are the source of truth for the `env` side; the TS ports
are maintained by hand, so a Rust-side change to the ABI, value encoding, or
host behaviour must be mirrored here or the shim drifts silently.
`tests/run.test.ts` exercises the polyfill and shim end to end against the
compiled `jsc-wasm` module.
