// Run a `jsc build` artifact under `node:wasi` and compare its stdout with an
// expected file. Exits non-zero on a mismatch or a non-zero module exit code.
//
//   node scripts/node-wasi-smoke.mjs <file.wasm> <expected-stdout-file>

import { closeSync, mkdtempSync, openSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { WASI } from 'node:wasi'

const [wasmPath, expectedPath] = process.argv.slice(2)
if (!wasmPath || !expectedPath) {
  console.error(
    'usage: node scripts/node-wasi-smoke.mjs <file.wasm> <expected-stdout-file>',
  )
  process.exit(2)
}

const dir = mkdtempSync(join(tmpdir(), 'jsc-node-wasi-'))
const stdoutPath = join(dir, 'stdout')
const stdoutFd = openSync(stdoutPath, 'w')
let exitCode
try {
  const wasi = new WASI({
    version: 'preview1',
    returnOnExit: true,
    stdout: stdoutFd,
    stderr: 2,
  })
  const module = await WebAssembly.compile(readFileSync(wasmPath))
  const instance = await WebAssembly.instantiate(
    module,
    wasi.getImportObject(),
  )
  exitCode = wasi.start(instance)
} finally {
  closeSync(stdoutFd)
}
const actual = readFileSync(stdoutPath, 'utf8')
rmSync(dir, { recursive: true, force: true })
const expected = readFileSync(expectedPath, 'utf8')

let failed = false
if (exitCode !== 0) {
  console.error(`exit code ${exitCode}, expected 0`)
  failed = true
}
if (actual !== expected) {
  console.error('stdout mismatch')
  console.error(`--- expected (${expectedPath})`)
  console.error(expected)
  console.error('+++ actual')
  console.error(actual)
  failed = true
}
if (failed) process.exit(1)
console.log(`ok: ${wasmPath} runs under node:wasi`)
