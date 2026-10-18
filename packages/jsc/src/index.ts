/**
 * `@cdlab/jsc` public API.
 *
 * - {@link compile} runs JavaScript source through the Rust compiler
 *   (`crates/jsc-wasm`, loaded as a wasm-bindgen module) and returns the emitted
 *   WebAssembly module bytes.
 * - {@link run} compiles the source, instantiates the emitted module in a JS
 *   shim ({@link createShim}), calls its exported `main`, and decodes the
 *   result plus anything `console.log` printed.
 *
 * The wasm-bindgen glue uses an async `init()` (wasm-pack `--target web`), so
 * both entry points are async even though the underlying Rust `compile` is
 * synchronous.
 */

import initGlue, { compile as wasmCompile } from '../wasm/jsc_wasm.js'
import {
  EXC_PENDING_EXPORT_NAME,
  EXC_VALUE_EXPORT_NAME,
  MEMORY_EXPORT_NAME,
} from './abi.js'
import { createShim } from './shim.js'
import { asU64, decode, formatValue } from './value.js'

/** The decoded result of running a program: the final value and captured stdout. */
export interface RunResult {
  /** The last expression statement's value, formatted the way JavaScript prints it. */
  result: string
  /** Everything `console.log` printed, in execution order. */
  stdout: string
}

// The wasm-bindgen module initializes once; every later call reuses it.
let initPromise: Promise<void> | null = null

/** Load the artifact bytes: read from disk under Node, fetch elsewhere (browser/workers). */
async function loadWasm(url: URL): Promise<BufferSource | Response> {
  if (typeof process !== 'undefined' && process.versions?.node) {
    // `fetch` of a `file:` URL is unreliable under Node; read the bytes directly.
    const { readFile } = await import('node:fs/promises')
    return readFile(url)
  }
  return fetch(url)
}

/** Initialize the wasm-bindgen module exactly once. */
function ensureInit(): Promise<void> {
  if (initPromise === null) {
    initPromise = (async () => {
      const url = new URL('../wasm/jsc_wasm_bg.wasm', import.meta.url)
      await initGlue({ module_or_path: await loadWasm(url) })
    })()
  }
  return initPromise
}

/**
 * Compile JavaScript `src` to the emitted WebAssembly module bytes.
 *
 * Throws a JS `Error` on a compile failure; when the underlying diagnostic
 * carries a source span, the message is the compiler's caret block.
 */
export async function compile(src: string): Promise<Uint8Array> {
  await ensureInit()
  return wasmCompile(src)
}

/**
 * Compile and run JavaScript `src`: instantiate the emitted module in the JS
 * shim, call `main`, and decode the result plus captured `console.log` output.
 *
 * Throws `Error("Uncaught <value>")` when the program throws an exception that
 * is never caught (mirrors `harness.rs`'s uncaught-exception check).
 */
export async function run(src: string): Promise<RunResult> {
  const bytes = await compile(src)
  const shim = createShim()
  // Compile to a Module first, then instantiate: the `(Module, imports)`
  // overload returns the `Instance` directly (no `{ instance }` wrapper). The
  // glue types `compile` as a generic `Uint8Array`; the bytes are genuinely
  // `ArrayBuffer`-backed (wasm-bindgen returns a fresh `.slice()`), so the
  // narrowing to `BufferSource` is sound.
  const module = await WebAssembly.compile(bytes as Uint8Array<ArrayBuffer>)
  const instance = await WebAssembly.instantiate(module, shim.imports)
  shim.bindInstance(instance)

  const main = instance.exports.main as () => bigint
  const bits = asU64(main())

  // Drain microtasks scheduled but never awaited, UNLESS `main` completed
  // abruptly with a pending exception (mirrors `harness.rs`'s after-`main`
  // `drain_after_main`).
  const pendingGlobal = instance.exports[EXC_PENDING_EXPORT_NAME] as
    | WebAssembly.Global
    | undefined
  const excPending = (): number =>
    pendingGlobal ? Number(pendingGlobal.value) : 0
  if (excPending() === 0) shim.drainAfterMain()

  // View linear memory AFTER the drain so a promise settled by a microtask
  // renders its final state (the buffer may also have grown/detached).
  const mem = instance.exports[MEMORY_EXPORT_NAME] as
    | WebAssembly.Memory
    | undefined
  const memBytes = mem ? new Uint8Array(mem.buffer) : new Uint8Array(0)

  // Uncaught-exception check: a set `$exc_pending` after `main` means an
  // exception propagated all the way out. Render `$exc_value` and surface it.
  if (excPending() !== 0) {
    const valueGlobal = instance.exports[EXC_VALUE_EXPORT_NAME] as
      | WebAssembly.Global
      | undefined
    const valueBits = valueGlobal ? asU64(valueGlobal.value as bigint) : 0n
    throw new Error(`Uncaught ${formatValue(decode(valueBits, memBytes))}`)
  }

  return { result: formatValue(decode(bits, memBytes)), stdout: shim.stdout() }
}
