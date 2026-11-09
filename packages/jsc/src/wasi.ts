/**
 * The in-memory `wasi_snapshot_preview1` polyfill `run()` uses in Node and the
 * browser alike. It provides exactly the four functions an emitted module
 * imports, so a module needing any other WASI function fails to instantiate.
 */

const ERRNO_SUCCESS = 0
const ERRNO_BADF = 8
const ERRNO_INVAL = 28

const FD_STDOUT = 1
const FD_STDERR = 2

const CLOCK_REALTIME = 0
const CLOCK_MONOTONIC = 1

/** The most bytes one `crypto.getRandomValues` call accepts. */
const RANDOM_CHUNK_BYTES = 65_536

/** Thrown by `proc_exit` to unwind the guest back to the `_start` caller. */
export class Exit extends Error {
  readonly code: number

  constructor(code: number) {
    super(`exit ${code}`)
    this.code = code
  }
}

/** One run's WASI imports and captured output streams. */
export interface Wasi {
  /** The `wasi_snapshot_preview1` part of the import object. */
  imports: WebAssembly.Imports
  /** Give the imports the instantiated module's memory. */
  bind: (memory: WebAssembly.Memory) => void
  /** Everything written to fd 1 so far. */
  stdout: () => string
  /** Everything written to fd 2 so far. */
  stderr: () => string
  /** Remove the most recent fd 1 write from {@link Wasi.stdout} and return it. */
  popStdoutWrite: () => string
}

/** Build a fresh polyfill. One per run, so output does not bleed across runs. */
export function createWasi(): Wasi {
  let memory!: WebAssembly.Memory
  let stdout = ''
  let stderr = ''
  let lastStdoutWrite = 0
  // `ignoreBOM` keeps a leading U+FEFF the program printed. Streaming keeps a
  // code point that spans two iovecs whole.
  const stdoutDecoder = new TextDecoder('utf-8', { ignoreBOM: true })
  const stderrDecoder = new TextDecoder('utf-8', { ignoreBOM: true })

  const fd_write = (
    fd: number,
    iovs: number,
    iovsLen: number,
    nwritten: number,
  ): number => {
    if (fd !== FD_STDOUT && fd !== FD_STDERR) return ERRNO_BADF
    const decoder = fd === FD_STDOUT ? stdoutDecoder : stderrDecoder
    const view = new DataView(memory.buffer)
    let text = ''
    let written = 0
    for (let i = 0; i < iovsLen; i++) {
      const ptr = view.getUint32(iovs + 8 * i, true)
      const len = view.getUint32(iovs + 8 * i + 4, true)
      text += decoder.decode(new Uint8Array(memory.buffer, ptr, len), {
        stream: true,
      })
      written += len
    }
    if (fd === FD_STDOUT) {
      lastStdoutWrite = stdout.length
      stdout += text
    } else {
      stderr += text
    }
    view.setUint32(nwritten, written, true)
    return ERRNO_SUCCESS
  }

  const clock_time_get = (
    id: number,
    _precision: bigint,
    time: number,
  ): number => {
    let now: bigint
    if (id === CLOCK_REALTIME) now = BigInt(Date.now()) * 1_000_000n
    else if (id === CLOCK_MONOTONIC)
      now = BigInt(Math.round(performance.now() * 1e6))
    else return ERRNO_INVAL
    new DataView(memory.buffer).setBigUint64(time, now, true)
    return ERRNO_SUCCESS
  }

  const random_get = (buf: number, bufLen: number): number => {
    const bytes = new Uint8Array(memory.buffer, buf, bufLen)
    for (let i = 0; i < bufLen; i += RANDOM_CHUNK_BYTES) {
      crypto.getRandomValues(bytes.subarray(i, i + RANDOM_CHUNK_BYTES))
    }
    return ERRNO_SUCCESS
  }

  const proc_exit = (code: number): never => {
    throw new Exit(code)
  }

  return {
    imports: {
      wasi_snapshot_preview1: {
        fd_write,
        clock_time_get,
        random_get,
        proc_exit,
      },
    },
    bind: (bound) => {
      memory = bound
    },
    stdout: () => stdout,
    stderr: () => stderr,
    popStdoutWrite: () => {
      const write = stdout.slice(lastStdoutWrite)
      stdout = stdout.slice(0, lastStdoutWrite)
      return write
    },
  }
}
