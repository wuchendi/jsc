import { expect, it } from 'vitest'

import { compile, run } from '../src/index.js'
import { createShim } from '../src/shim.js'

// Expected `result` strings are the EXACT output of `cargo run -p jsc -- -e <prog>`
// (jsc-core's `format_value`); the shim must reproduce them byte for byte.

it('compiles source to a WebAssembly module', async () => {
  const bytes = await compile('1 + 2')
  // The module preamble is the wasm magic number `\0asm`.
  expect(Array.from(bytes.subarray(0, 4))).toEqual([0x00, 0x61, 0x73, 0x6d])
})

it('runs arithmetic', async () => {
  expect((await run('1+2')).result).toBe('3')
})

it('captures console.log output', async () => {
  const { stdout } = await run("console.log('hi')")
  expect(stdout).toContain('hi')
})

it('concatenates strings without quotes', async () => {
  expect((await run("'a'+'b'")).result).toBe('ab')
})

it('formats arrays Node-console style', async () => {
  expect((await run('[1,2,3]')).result).toBe('[ 1, 2, 3 ]')
})

it('formats objects Node-console style', async () => {
  expect((await run('({a:1,b:2})')).result).toBe('{ a: 1, b: 2 }')
})

it('calls a function', async () => {
  expect((await run('const f=(x)=>x*2; f(21)')).result).toBe('42')
})

it('dispatches a host string method (exercises the shim allocator)', async () => {
  expect((await run('let s="hi"; s.toUpperCase()')).result).toBe('HI')
})

it('dispatches a host array method that allocates a heap string', async () => {
  expect((await run("[1,2,3].join('-')")).result).toBe('1-2-3')
})

it('dispatches the string methods through the shim', async () => {
  expect((await run("'  hi  '.trim()")).result).toBe('hi')
  expect((await run("'ab'.repeat(3)")).result).toBe('ababab')
  expect((await run("'hello'.startsWith('he')")).result).toBe('true')
  expect((await run("'hello'.endsWith('lo')")).result).toBe('true')
  expect((await run("'hello'.includes('ell')")).result).toBe('true')
  expect((await run("'5'.padStart(3,'0')")).result).toBe('005')
  expect((await run("'hello'.substring(3,1)")).result).toBe('el')
  expect((await run("'a,b,c'.split(',').length")).result).toBe('3')
})

it('links every emitted module against the shim (no LinkError)', async () => {
  expect((await run('1+1')).result).toBe('2')
})

it('provides every env import an emitted module declares', async () => {
  const module = await WebAssembly.compile(
    (await compile('1')) as Uint8Array<ArrayBuffer>,
  )
  const provided = Object.keys(createShim().imports.env ?? {})
  for (const imp of WebAssembly.Module.imports(module)) {
    expect(imp.module).toBe('env')
    expect(provided).toContain(imp.name)
  }
})

it('runs a Promise chain through the microtask queue', async () => {
  const chain = await run(
    'Promise.resolve(1).then(x => x + 1).then(x => console.log(x))',
  )
  expect(chain.stdout).toBe('2\n')
  expect(chain.result).toBe('Promise { undefined }')
  const caught = await run(
    'Promise.reject("bad").catch(e => console.log("caught", e))',
  )
  expect(caught.stdout).toBe('caught bad\n')
  expect((await run('Promise.reject(3).catch(e=>e)')).result).toBe(
    'Promise { 3 }',
  )
  const fin = await run('Promise.resolve(1).finally(()=>console.log("fin"))')
  expect(fin.stdout).toBe('fin\n')
  expect(fin.result).toBe('Promise { 1 }')
})

it('renders promises by settlement', async () => {
  expect((await run('Promise.resolve(42)')).result).toBe('Promise { 42 }')
  expect(
    (await run('const p = Promise.resolve(1).then(x=>x+1); p')).result,
  ).toBe('Promise { 2 }')
})

it('runs microtasks after synchronous code', async () => {
  const { stdout, result } = await run(
    'Promise.resolve(1).then(()=>console.log("micro")); console.log("sync")',
  )
  expect(stdout).toBe('sync\nmicro\n')
  expect(result).toBe('undefined')
})

it('runs async/await with top-level await', async () => {
  const { stdout, result } = await run(
    'async function f(){ const v = await Promise.resolve(5); console.log("in", v); return v*2 } console.log("sync"); const r = await f(); console.log("r", r); r',
  )
  expect(stdout).toBe('sync\nin 5\nr 10\n')
  expect(result).toBe('10')
})

it('backs Date.now() with the host clock', async () => {
  expect((await run('typeof Date.now()')).result).toBe('number')
  const before = Date.now()
  const now = Number((await run('Date.now()')).result)
  expect(now).toBeGreaterThanOrEqual(before)
  expect(now).toBeLessThanOrEqual(Date.now())
  expect((await run('new Date(0)')).result).toBe('1970-01-01T00:00:00.000Z')
})

it('rejects run() on an unhandled top-level await rejection', async () => {
  await expect(run('await Promise.reject("boom")')).rejects.toThrow(
    /^Uncaught boom$/,
  )
})
