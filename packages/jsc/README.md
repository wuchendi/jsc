# @cdlab/jsc

Compile JavaScript to WebAssembly and run it — in the browser or Node — with no
native runtime. The compiler is the Rust `jsc-core` pipeline (parse → codegen →
emit) shipped as a wasm-bindgen module (`crates/jsc-wasm`); the emitted program
module is instantiated by a small JS shim.

## Usage

```ts
import { compile, run } from '@cdlab/jsc'

// Emit the WebAssembly module bytes for a program.
const bytes: Uint8Array = await compile('1 + 2')

// Compile, instantiate, run, and decode the result + captured console output.
const { result, stdout } = await run("console.log('hi'); 1 + 2")
// result === '3'
// stdout === 'hi\n'
```

Both `compile` and `run` are async: the wasm-bindgen glue (wasm-pack
`--target web`) initializes with an async `init()`, cached after the first call.

- `compile(src)` throws a JS `Error` on a compile failure. When the diagnostic
  carries a source span, the message is the compiler's rustc/swc-style caret
  block, so a browser can show exactly where compilation failed.
- `run(src)` throws `Error("Uncaught <value>")` when the program throws an
  exception that is never caught.

## Known debt

`run()` executes the emitted module in a **TypeScript shim** that duplicates
Rust runtime semantics:

- `src/value.ts` ports the NaN-box decoder and value formatter from
  `crates/jsc-core/src/run/value.rs`.
- `src/shim.ts` ports the host imports (`print` / `__convert` / `__method`) and
  the bump-allocator from `crates/jsc-core/src/run/host.rs` +
  `run/harness.rs`.
- `src/abi.ts` copies the runtime ABI constants from
  `crates/jsc-core/src/abi.rs`.

These are hand-maintained duplicates of the Rust single-source-of-truth, so a
change on the Rust side can silently drift the shim. This is **accepted
temporary debt**, to be paid down by a later D1/C4 campaign (e.g. by running the
same wasm-side decode/host logic instead of re-implementing it in TS).
