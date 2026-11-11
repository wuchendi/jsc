/**
 * `@cdlab/jsc` public API.
 *
 * - {@link compile} runs JavaScript source through the Rust compiler
 *   (`crates/jsc-wasm`, loaded as a wasm-bindgen module) and returns the emitted
 *   WebAssembly module bytes.
 * - {@link run} compiles the source, instantiates the emitted command module
 *   against the WASI polyfill ({@link createWasi}) alone, calls `_start`, and
 *   returns its output and exit code.
 *
 * The wasm-bindgen glue uses an async `init()` (wasm-pack `--target web`), so
 * both entry points are async even though the underlying Rust `compile` is
 * synchronous.
 */

import initGlue, { compile as wasmCompile } from '../wasm/jsc_wasm.js'
import { createWasi, Exit } from './wasi.js'

/** Options of {@link compile} and {@link run}. */
export interface CompileOptions {
  /** Have the module render its completion value after the final drain. */
  printCompletion?: boolean
}

/** What a finished program produced. */
export interface RunResult {
  /** Everything written to fd 1, without the completion line. */
  stdout: string
  /** Everything written to fd 2: the `Uncaught ...` reports. */
  stderr: string
  /** The `proc_exit` code, or `0` when `_start` returned. */
  exitCode: number
  /** The rendered completion value, on a clean exit under `printCompletion`. */
  completion?: string
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
export async function compile(
  src: string,
  options?: CompileOptions,
): Promise<Uint8Array> {
  await ensureInit()
  return wasmCompile(src, options)
}

/**
 * Compile and run JavaScript `src`.
 *
 * An uncaught exception or unhandled rejection is not a rejection of this
 * promise: it is the `Uncaught ...` line on `stderr` and `exitCode` 1. The
 * promise rejects on a compile error or a guest trap.
 */
export async function run(
  src: string,
  options?: CompileOptions,
): Promise<RunResult> {
  const bytes = await compile(src, options)
  const wasi = createWasi()
  // Compile to a Module first, then instantiate: the `(Module, imports)`
  // overload returns the `Instance` directly (no `{ instance }` wrapper). The
  // glue types `compile` as a generic `Uint8Array`; the bytes are genuinely
  // `ArrayBuffer`-backed (wasm-bindgen returns a fresh `.slice()`), so the
  // narrowing to `BufferSource` is sound.
  const module = await WebAssembly.compile(bytes as Uint8Array<ArrayBuffer>)
  // The module imports only the four WASI functions and exports only
  // `memory` and `_start`.
  const instance = await WebAssembly.instantiate(module, wasi.imports)
  wasi.bind(instance.exports.memory as WebAssembly.Memory)

  let exitCode = 0
  try {
    ;(instance.exports._start as () => void)()
  } catch (e) {
    if (!(e instanceof Exit)) throw e
    exitCode = e.code
  }

  // On a clean exit the completion is the module's last fd 1 write: one
  // `fd_write` of the rendered value and a newline.
  const completion =
    options?.printCompletion && exitCode === 0
      ? wasi.popStdoutWrite().slice(0, -1)
      : undefined
  return {
    stdout: wasi.stdout(),
    stderr: wasi.stderr(),
    exitCode,
    ...(completion === undefined ? {} : { completion }),
  }
}
