import { expect, it } from 'vitest'

import { compile, run } from '../src/index.js'

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

it('dispatches the M2 string methods through the shim', async () => {
  expect((await run("'  hi  '.trim()")).result).toBe('hi')
  expect((await run("'ab'.repeat(3)")).result).toBe('ababab')
  expect((await run("'hello'.startsWith('he')")).result).toBe('true')
  expect((await run("'hello'.endsWith('lo')")).result).toBe('true')
  expect((await run("'hello'.includes('ell')")).result).toBe('true')
  expect((await run("'5'.padStart(3,'0')")).result).toBe('005')
  expect((await run("'hello'.substring(3,1)")).result).toBe('el')
  expect((await run("'a,b,c'.split(',').length")).result).toBe('3')
})
