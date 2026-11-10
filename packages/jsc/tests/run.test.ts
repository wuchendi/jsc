import { describe, expect, it } from 'vitest'

import { compile, run } from '../src/index.js'
import { createWasi, Exit } from '../src/wasi.js'

const WASI = 'wasi_snapshot_preview1'

/** `src`'s rendered completion value; the program must exit cleanly. */
async function evalText(src: string): Promise<string | undefined> {
  const done = await run(src, { printCompletion: true })
  expect(done.exitCode, done.stderr).toBe(0)
  return done.completion
}

it('compiles source to a WebAssembly module', async () => {
  const bytes = await compile('1 + 2')
  // The module preamble is the wasm magic number `\0asm`.
  expect(Array.from(bytes.subarray(0, 4))).toEqual([0x00, 0x61, 0x73, 0x6d])
})

it('links_wasi_polyfill', async () => {
  const module = await WebAssembly.compile(
    (await compile('1')) as Uint8Array<ArrayBuffer>,
  )
  // Exactly the four preview-1 functions, in import order, and nothing else.
  expect(
    WebAssembly.Module.imports(module).map(
      (i) => `${i.kind} ${i.module}.${i.name}`,
    ),
  ).toEqual([
    `function ${WASI}.fd_write`,
    `function ${WASI}.clock_time_get`,
    `function ${WASI}.random_get`,
    `function ${WASI}.proc_exit`,
  ])
  expect(
    WebAssembly.Module.exports(module)
      .map((e) => `${e.kind} ${e.name}`)
      .sort(),
  ).toEqual(['function _start', 'memory memory'])
  // The polyfill provides exactly those four, so the module instantiates
  // against it alone.
  const provided = createWasi().imports
  expect(Object.keys(provided)).toEqual([WASI])
  expect(Object.keys(provided[WASI] ?? {})).toEqual([
    'fd_write',
    'clock_time_get',
    'random_get',
    'proc_exit',
  ])
  await WebAssembly.instantiate(module, provided)
})

it('runs console.log to stdout with exit code 0', async () => {
  expect(await run("console.log('hi', 1); console.log()")).toEqual({
    stdout: 'hi 1\n\n',
    stderr: '',
    exitCode: 0,
  })
})

it('decodes the UTF-8 the guest writes', async () => {
  const { stdout } = await run('console.log("é😀", "\\uD800")')
  expect(stdout).toBe('é😀 \uFFFD\n')
  expect((await run('console.log("\\uFEFFa")')).stdout).toBe('\uFEFFa\n')
})

it('separates the completion from what the program printed', async () => {
  const done = await run("console.log('a'); 1 + 2", { printCompletion: true })
  expect(done).toEqual({
    stdout: 'a\n',
    stderr: '',
    exitCode: 0,
    completion: '3',
  })
  expect(await evalText('[1, 2]')).toBe('[ 1, 2 ]')
  expect((await run('1 + 2')).completion).toBeUndefined()
})

it('reports an uncaught exception on stderr with exit code 1', async () => {
  expect(
    await run("console.log('before'); throw new TypeError('boom')", {
      printCompletion: true,
    }),
  ).toEqual({
    stdout: 'before\n',
    stderr: 'Uncaught TypeError: boom\n',
    exitCode: 1,
  })
})

it('reports an unhandled rejection after the drain with exit code 1', async () => {
  const unhandled = await run(
    'Promise.reject("a"); Promise.reject("b"); console.log("sync")',
  )
  expect(unhandled).toEqual({
    stdout: 'sync\n',
    stderr: 'Uncaught (in promise) a\n',
    exitCode: 1,
  })
  const handledLate = await run(
    'const p = Promise.reject(1); Promise.resolve(0).then(() => p.catch(() => {}))',
  )
  expect(handledLate.exitCode).toBe(0)
  expect((await run('await Promise.reject("boom")')).stderr).toBe(
    'Uncaught boom\n',
  )
  // The uncaught exception wins and skips the drain.
  expect((await run('Promise.reject(1); throw 2')).stderr).toBe('Uncaught 2\n')
})

it('runs promise reactions in order, after synchronous code', async () => {
  const { stdout, exitCode } = await run(
    'Promise.resolve(1).then(x => { console.log("then", x); return x + 1 }).then(x => console.log("then", x)); ' +
      'async function f() { console.log("body"); const v = await 5; console.log("resumed", v) } f(); ' +
      'console.log("sync")',
  )
  expect(exitCode).toBe(0)
  expect(stdout).toBe('body\nsync\nthen 1\nresumed 5\nthen 2\n')
})

it('reports running out of memory with the static line', async () => {
  // A guest allocation past the 512 MiB cap: the 64M element spread.
  const guest = await run(
    "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } " +
      'let a = [...s]; a.length',
    { printCompletion: true },
  )
  expect(guest).toEqual({
    stdout: '',
    stderr: 'Uncaught RangeError: out of memory\n',
    exitCode: 1,
  })
}, 60_000)

it('reads the Date clock through clock_time_get', async () => {
  const before = Date.now()
  const now = Number(await evalText('Date.now()'))
  expect(now).toBeGreaterThanOrEqual(before)
  expect(now).toBeLessThanOrEqual(Date.now())
})

it('keeps Math.random in range', async () => {
  expect(
    await evalText(
      'let ok = true; for (let i = 0; i < 1000; i++) { const r = Math.random(); ok = ok && r >= 0 && r < 1 } ok',
    ),
  ).toBe('true')
})

describe('wasi polyfill', () => {
  type Fns = {
    fd_write: (fd: number, iovs: number, len: number, out: number) => number
    clock_time_get: (id: number, precision: bigint, time: number) => number
    random_get: (buf: number, len: number) => number
    proc_exit: (code: number) => never
  }

  function setup() {
    const memory = new WebAssembly.Memory({ initial: 4 })
    const wasi = createWasi()
    wasi.bind(memory)
    return {
      memory,
      wasi,
      fns: wasi.imports[WASI] as unknown as Fns,
      view: new DataView(memory.buffer),
    }
  }

  it('reassembles a code point split across iovecs', () => {
    const { memory, wasi, fns, view } = setup()
    new Uint8Array(memory.buffer).set([0xf0, 0x9f, 0x98, 0x80, 0x0a], 100)
    view.setUint32(0, 100, true)
    view.setUint32(4, 2, true)
    view.setUint32(8, 102, true)
    view.setUint32(12, 3, true)
    expect(fns.fd_write(2, 0, 2, 40)).toBe(0)
    expect(view.getUint32(40, true)).toBe(5)
    expect(wasi.stderr()).toBe('😀\n')
    expect(wasi.stdout()).toBe('')
  })

  it('pops only the most recent stdout write', () => {
    const { memory, wasi, fns, view } = setup()
    new Uint8Array(memory.buffer).set([0x61, 0x62], 100)
    view.setUint32(0, 100, true)
    view.setUint32(4, 1, true)
    fns.fd_write(1, 0, 1, 40)
    view.setUint32(0, 101, true)
    fns.fd_write(1, 0, 1, 40)
    expect(wasi.popStdoutWrite()).toBe('b')
    expect(wasi.stdout()).toBe('a')
  })

  it('rejects a descriptor other than stdout and stderr', () => {
    expect(setup().fns.fd_write(3, 0, 0, 40)).toBe(8)
  })

  it('reads the realtime and monotonic clocks in nanoseconds', () => {
    const { fns, view } = setup()
    const before = BigInt(Date.now()) * 1_000_000n
    expect(fns.clock_time_get(0, 1_000_000n, 16)).toBe(0)
    expect(view.getBigUint64(16, true)).toBeGreaterThanOrEqual(before)
    expect(fns.clock_time_get(1, 1n, 16)).toBe(0)
    const first = view.getBigUint64(16, true)
    expect(fns.clock_time_get(1, 1n, 16)).toBe(0)
    expect(view.getBigUint64(16, true)).toBeGreaterThanOrEqual(first)
    expect(fns.clock_time_get(9, 1n, 16)).toBe(28)
  })

  it('fills a buffer larger than one getRandomValues chunk', () => {
    const { memory, fns } = setup()
    expect(fns.random_get(0, 100_000)).toBe(0)
    // The tail lands in the second chunk; 34,464 zero bytes will not happen.
    const tail = new Uint8Array(memory.buffer, 65_536, 34_464)
    expect(tail.some((b) => b !== 0)).toBe(true)
    expect(
      new Uint8Array(memory.buffer, 100_000, 16).every((b) => b === 0),
    ).toBe(true)
  })

  it('throws Exit carrying the code from proc_exit', () => {
    const { fns } = setup()
    expect(() => fns.proc_exit(3)).toThrow(Exit)
    try {
      fns.proc_exit(3)
    } catch (e) {
      expect((e as Exit).code).toBe(3)
    }
  })
})
