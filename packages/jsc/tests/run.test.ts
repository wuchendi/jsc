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

it('formats array holes and named properties Node-console style', async () => {
  expect((await run('[1,,3]')).result).toBe('[ 1, <1 empty item>, 3 ]')
  expect((await run('[,,1]')).result).toBe('[ <2 empty items>, 1 ]')
  expect((await run('[,]')).result).toBe('[ <1 empty item> ]')
  expect((await run('let a=[1]; a[3]=4; a')).result).toBe(
    '[ 1, <2 empty items>, 4 ]',
  )
  expect((await run('let a=[1,2]; a.foo=1; a')).result).toBe('[ 1, 2, foo: 1 ]')
  expect((await run('let a=[1,,3]; delete a[0]; a')).result).toBe(
    '[ <2 empty items>, 3 ]',
  )
  expect((await run("[1,,3].join('-')")).result).toBe('1--3')
  expect((await run('[1,,3].includes(undefined)')).result).toBe('true')
  expect((await run('[1,,3].indexOf(undefined)')).result).toBe('-1')
  expect((await run('[1,,3].slice(0)')).result).toBe('[ 1, <1 empty item>, 3 ]')
  expect((await run('String([1,,3])')).result).toBe('1,,3')
})

it('writes array length and renders sparse arrays like the host', async () => {
  expect((await run('let a=[1,2,3]; a.length=0; a')).result).toBe('[]')
  expect((await run('let a=[1]; a.length=5; a')).result).toBe(
    '[ 1, <4 empty items> ]',
  )
  expect((await run('let a=[1,2,3]; a.length-=1; a.push(9); a')).result).toBe(
    '[ 1, 2, 9 ]',
  )
  expect(
    (
      await run(
        "let r=''; try { [].length = -1 } catch (e) { r = (e instanceof RangeError) + '|' + e.message } r",
      )
    ).result,
  ).toBe('true|Invalid array length')
  expect((await run('const a=[]; a[1e6]=1; a')).result).toBe(
    '[ <1000000 empty items>, 1 ]',
  )
  expect(
    (
      await run(
        'const a=[]; a[1e6]=1; a.length + "|" + a[1e6] + "|" + Object.keys(a)',
      )
    ).result,
  ).toBe('1000001|1|1000000')
  expect((await run('let a=[1,2]; a[5000]=3; a.foo=4; a')).result).toBe(
    '[ 1, 2, <4998 empty items>, 3, foo: 4 ]',
  )
  expect(
    (
      await run(
        'let a=[1]; a[3000]=2; a.indexOf(2) + "|" + a.includes(undefined) + "|" + a.join("") + "|" + String(a).length',
      )
    ).result,
  ).toBe('3000|true|12|3002')
  expect((await run('let a=[1]; a[3000]=2; a.slice(2999)')).result).toBe(
    '[ <1 empty item>, 2 ]',
  )
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

it('reads and builds array handles through the shim', async () => {
  const src =
    'let a = [1, [2, 3], {x: [4]}]; let b = a; for (let i = 0; i < 9; i++) a.push(i); ' +
    "console.log(b.length, b[1], b[11], 'x,y'.split(','), a.slice(1, 3), a.indexOf(8), a.includes(5))"
  expect((await run(src)).stdout).toBe(
    '12 [ 2, 3 ] 8 [ x, y ] [ [ 2, 3 ], { x: [ 4 ] } ] 11 true\n',
  )
})

it('parses parseInt with a radix through the shim', async () => {
  expect((await run("parseInt('ff', 16)")).result).toBe('255')
  expect((await run("parseInt('0xff')")).result).toBe('255')
  expect((await run("parseInt('0xff', 10)")).result).toBe('0')
  expect((await run("parseInt('101', 2)")).result).toBe('5')
  expect((await run("parseInt('z', 36)")).result).toBe('35')
  expect((await run("parseInt('12', 37)")).result).toBe('NaN')
  expect((await run("parseInt('  -7f', 16)")).result).toBe('-127')
  expect((await run("1 / parseInt('-0')")).result).toBe('-Infinity')
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

it('rejects a promise resolved with itself with a TypeError', async () => {
  const executor = await run(
    'let res; const p = new Promise(r => res = r); res(p); p.then(() => console.log("fulfilled"), e => console.log(e instanceof TypeError, e.message))',
  )
  expect(executor.stdout).toBe(
    'true Chaining cycle detected for promise #<Promise>\n',
  )
  const chained = await run(
    'let p2 = Promise.resolve(1).then(() => p2); p2.catch(e => console.log(e instanceof TypeError))',
  )
  expect(chained.stdout).toBe('true\n')
  const asyncSelf = await run(
    'let p; async function f() { await null; return p } p = f(); p.catch(e => console.log(e.constructor === TypeError))',
  )
  expect(asyncSelf.stdout).toBe('true\n')
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

it('throws a rejected await into the async body like the Rust host', async () => {
  expect(
    (
      await run(
        'async function f(){ try { await Promise.reject(new Error("boom")) } catch (e) { return "caught " + e.message } } f().then(v => console.log(v))',
      )
    ).stdout,
  ).toBe('caught boom\n')
  expect(
    (
      await run(
        'async function f(){ try { await Promise.reject("r") } finally { console.log("fin") } } f().catch(e => console.log("rejected", e))',
      )
    ).stdout,
  ).toBe('fin\nrejected r\n')
  expect(
    (
      await run(
        'async function f(){ try { await Promise.reject(42) } catch (e) { console.log(e, typeof e) } } f()',
      )
    ).stdout,
  ).toBe('42 number\n')
  expect(
    (
      await run(
        'async function f(){ let log = ""; try { await Promise.reject("a") } catch (e) { log = log + e } try { const v = await Promise.resolve("b"); log = log + v; const w = await 3; log = log + w } catch (e) { log = log + "!" } return log } f().then(v => console.log(v))',
      )
    ).stdout,
  ).toBe('ab3\n')
  expect(
    (
      await run(
        'async function f(){ try { await Promise.reject(1) } catch (e) { return e + 1 } } await f()',
      )
    ).result,
  ).toBe('2')
  await expect(
    run('async function f() { await Promise.reject("nope"); return 1 } f(); 0'),
  ).rejects.toThrow(/^Uncaught \(in promise\) nope$/)
})

it('backs Date.now() with the host clock', async () => {
  expect((await run('typeof Date.now()')).result).toBe('number')
  const before = Date.now()
  const now = Number((await run('Date.now()')).result)
  expect(now).toBeGreaterThanOrEqual(before)
  expect(now).toBeLessThanOrEqual(Date.now())
  expect((await run('new Date(0)')).result).toBe('1970-01-01T00:00:00.000Z')
})

it('renders clipped and expanded-year dates like toISOString', async () => {
  expect((await run('new Date(1e20)')).result).toBe('Invalid Date')
  expect((await run('new Date(8.64e15)')).result).toBe(
    '+275760-09-13T00:00:00.000Z',
  )
  expect((await run('new Date(-8.64e15)')).result).toBe(
    '-271821-04-20T00:00:00.000Z',
  )
})

it('rejects run() on an unhandled promise rejection', async () => {
  await expect(run('Promise.reject("x"); 0')).rejects.toThrow(
    /^Uncaught \(in promise\) x$/,
  )
  await expect(run('async function f() { throw "t" } f(); 0')).rejects.toThrow(
    /^Uncaught \(in promise\) t$/,
  )
  await expect(
    run('Promise.reject("a"); Promise.reject("b"); 0'),
  ).rejects.toThrow(/^Uncaught \(in promise\) a$/)
  await expect(run('Promise.reject(1); throw 2')).rejects.toThrow(
    /^Uncaught 2$/,
  )
  const late = await run(
    'const p = Promise.reject(1); Promise.resolve(0).then(() => p.catch(() => {})); 0',
  )
  expect(late.result).toBe('0')
})

it('rejects run() on an unhandled top-level await rejection', async () => {
  await expect(run('await Promise.reject("boom")')).rejects.toThrow(
    /^Uncaught boom$/,
  )
})

it('reports a host allocation past the memory cap like the Rust host', async () => {
  // Same program as `host_initiated_out_of_memory` in
  // crates/jsc-core/tests/memory.rs: guest strings fill ~448 MiB of the
  // 512 MiB cap, then the 64 MiB `s.slice(1)` host allocation runs out.
  const src =
    "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } " +
    'let t = s + s; let u = t + s; s.slice(1).length'
  await expect(run(src)).rejects.toThrow(/^Uncaught RangeError: out of memory$/)
}, 60_000)

it('reports a guest allocation past the memory cap like the Rust host', async () => {
  // Same program as `huge_allocation_is_out_of_memory` in
  // crates/jsc-core/tests/memory.rs: the guest's own `__alloc` for the 64M
  // element spread fails inside `main` with no host import involved.
  const src =
    "let s = 'x'; for (let i = 0; i < 26; i++) { s = s + s } " +
    'let a = [...s]; a.length'
  await expect(run(src)).rejects.toThrow(/^Uncaught RangeError: out of memory$/)
}, 60_000)

it('renders cyclic structures Node-console style', async () => {
  const cases: [string, string][] = [
    [
      'let a = [1, 0]; a[1] = a; console.log(a)',
      '<ref *1> [ 1, [Circular *1] ]\n',
    ],
    [
      'const a = [1]; a.push(a); console.log(a)',
      '<ref *1> [ 1, [Circular *1] ]\n',
    ],
    [
      'let o = {self: null, x: 1}; o.self = o; console.log(o)',
      '<ref *1> { self: [Circular *1], x: 1 }\n',
    ],
    [
      'let o = {x: [0]}; o.x[0] = o; console.log(o)',
      '<ref *1> { x: [ [Circular *1] ] }\n',
    ],
    [
      'let a = {b: null}; let b = {a: a}; a.b = b; console.log(a)',
      '<ref *1> { b: { a: [Circular *1] } }\n',
    ],
    [
      'let a = [1, 0, 0]; let b = [2, 0]; a[1] = b; b[1] = b; a[2] = a; console.log(a)',
      '<ref *2> [ 1, <ref *1> [ 2, [Circular *1] ], [Circular *2] ]\n',
    ],
    [
      'let a = [1, 0]; let b = [2, a, 0]; a[1] = b; b[2] = b; console.log(a)',
      '<ref *1> [ 1, <ref *2> [ 2, [Circular *1], [Circular *2] ] ]\n',
    ],
    [
      'let s = {v: 1}; let o = {p: s, q: s}; console.log(o)',
      '{ p: { v: 1 }, q: { v: 1 } }\n',
    ],
  ]
  for (const [src, out] of cases) expect((await run(src)).stdout).toBe(out)
  expect((await run('let a = [1, 0]; a[1] = a; a')).result).toBe(
    '<ref *1> [ 1, [Circular *1] ]',
  )
})

it('renders object-family values like the Rust host', async () => {
  const { stdout, result } = await run(
    "const m = new Map(); m.set(1, 'a'); m.set(2, 'b'); m.delete(2); " +
      'const s = new Set(); s.add(1); s.add(2); ' +
      'function* gen() { yield 1 } function f(x) { return x } ' +
      'console.log(m, s, m.size, s.size, m.get(1), new Date(5), f, gen(), ' +
      'Promise.resolve(4), Promise.reject(1).catch(() => 0)); ' +
      '[m, s, new Date(0), Promise.resolve(f)]',
  )
  expect(stdout).toBe(
    'Map(1) Set(2) 1 2 a 1970-01-01T00:00:00.005Z [Function: f] ' +
      'Object [Generator] Promise { 4 } Promise { <pending> }\n',
  )
  expect(result).toBe(
    '[ Map(1), Set(2), 1970-01-01T00:00:00.000Z, Promise { [Function: f] } ]',
  )
})

it('renders dictionary objects like the Rust host', async () => {
  const cases: [string, string][] = [
    [
      'class A { constructor(){ this.x = 1 } m(){} } let a = new A(); for (const k in a) console.log(k); console.log(Object.keys(a), JSON.stringify(a), a)',
      'x\n[ x ] {"x":1} A { x: 1 }\n',
    ],
    ['let n = 5; console.log(n.x, true.y)', 'undefined undefined\n'],
    [
      "class A { m(){} } console.log('m' in new A(), Object.keys({b:1, 2:1, a:1, 1:1}))",
      'true [ 1, 2, b, a ]\n',
    ],
    [
      'let o = {}; o.x = 1; o.y = 2; delete o.x; o.z = 3; console.log(o)',
      '{ y: 2, z: 3 }\n',
    ],
    [
      "let k = 'a' + 'b'; let o = { ab: 1 }; o[1] = 2; console.log(o[k], k in o, o['1'], o)",
      '1 true 2 { 1: 2, ab: 1 }\n',
    ],
    [
      "let k = 'c'; let o = {[k]: 1, 2.5: 'b', 1: 'a'}; console.log(Object.entries(o), o)",
      '[ [ 1, a ], [ c, 1 ], [ 2.5, b ] ] { 1: a, c: 1, 2.5: b }\n',
    ],
    [
      "console.log(new TypeError('boom'), [new Error()], Object.keys({'@x': 1}))",
      'TypeError: boom [ Error ] [ @x ]\n',
    ],
  ]
  for (const [src, out] of cases) expect((await run(src)).stdout).toBe(out)
})

it('renders errors by their class byte like the Rust host', async () => {
  const cases: [string, string][] = [
    [
      "let e = new TypeError('bad'); console.log(e instanceof TypeError, e instanceof Error, e instanceof RangeError, e.name, e.message, Object.keys(e).length)",
      'true true false TypeError bad 0\n',
    ],
    [
      "console.log(new RangeError('r'), [new Error()])",
      'RangeError: r [ Error ]\n',
    ],
    [
      "let e = new Error('m'); e.name = 'X'; console.log(e, { e })",
      'X: m { e: X: m }\n',
    ],
    [
      'try { null.x } catch (e) { console.log(e) }',
      "TypeError: Cannot read properties of null (reading 'x')\n",
    ],
    [
      "console.log({ name: 'Error', message: 'x' })",
      '{ name: Error, message: x }\n',
    ],
  ]
  for (const [src, out] of cases) expect((await run(src)).stdout).toBe(out)
})

it('renders functions, classes, instances, and accessors like the Rust host', async () => {
  const cases: [string, string][] = [
    [
      'class A { constructor(){ this.x = 1 } } class B extends A {} console.log(A, B, new A(), new B())',
      '[class A] [class B extends A] A { x: 1 } B { x: 1 }\n',
    ],
    [
      'function f(){} f.x = 1; let g = () => 0; console.log(f, g, () => 0)',
      '[Function: f] { x: 1 } [Function: g] [Function (anonymous)]\n',
    ],
    [
      'let o = { get x(){ return 1 }, set x(v){ this.y = v } }; o.x = 5; console.log(o.x, o.y, o)',
      '1 5 { x: [Getter/Setter], y: 5 }\n',
    ],
    ['class E {} console.log(new E(), E.prototype)', 'E {} {}\n'],
    [
      'class E extends Error {} class M extends Map {} console.log(E, M, Object, TypeError)',
      '[class E extends Error] [class M extends Map] [Function: Object] [Function: TypeError]\n',
    ],
  ]
  for (const [src, out] of cases) expect((await run(src)).stdout).toBe(out)
})

it('converts values with JS ToString like the Rust host', async () => {
  const cases: [string, string][] = [
    ['console.log(String([1, 2]))', '1,2\n'],
    ["console.log([1, [2, 3]].join('-'))", '1-2,3\n'],
    ["console.log('' + [1, 'a', null, undefined])", '1,a,,\n'],
    [
      'console.log(String({}), `${{ a: 1 }}`)',
      '[object Object] [object Object]\n',
    ],
    [
      "console.log(String(new TypeError('x')), String(new Error()), '' + new RangeError('r'))",
      'TypeError: x Error RangeError: r\n',
    ],
    [
      "let a = [1, 0]; a[1] = a; console.log(String(a), a.join('-'))",
      '1, 1-\n',
    ],
    [
      "console.log('x'.padStart(4, [1, 2]), 'a1,2b'.split([1, 2]))",
      '1,2x [ a, b ]\n',
    ],
  ]
  for (const [src, out] of cases) expect((await run(src)).stdout).toBe(out)
})

it('converts objects with OrdinaryToPrimitive like the Rust host', async () => {
  const both =
    "const b = { valueOf() { return 2 }, toString() { return 'str' } };"
  const sta =
    "function Test262Error(message) { this.message = message || '' } " +
    "Test262Error.prototype.toString = function () { return 'Test262Error: ' + this.message };"
  const cases: [string, string][] = [
    [`${both} String(b)`, 'str'],
    [`${both} '' + b`, '2'],
    [`${both} +b`, '2'],
    [`${both} \`\${b}\``, 'str'],
    [`${both} [b, b].join('-')`, 'str-str'],
    ['+new Date(0)', '0'],
    ['[] + {}', '[object Object]'],
    ['+{}', 'NaN'],
    ['String(function f() {})', 'function f() { [native code] }'],
    [`${sta} String(new Test262Error('m'))`, 'Test262Error: m'],
    [`${sta} '' + new Test262Error('m')`, 'Test262Error: m'],
  ]
  for (const [src, out] of cases) expect((await run(src)).result).toBe(out)
  await expect(
    run('String({ valueOf() { return {} }, toString() { return {} } })'),
  ).rejects.toThrow('TypeError: Cannot convert object to primitive value')
})

it('renders wrapper objects Node-console style like the Rust host', async () => {
  const { stdout } = await run(
    "console.log(new String('abc'), new Number(1), new Boolean(true), [Object('x')])",
  )
  expect(stdout).toBe(
    "[String: 'abc'] [Number: 1] [Boolean: true] [ [String: 'x'] ]\n",
  )
  expect((await run('new Number(-0)')).result).toBe('[Number: -0]')
  expect((await run('let w = new Number(2); w.x = 1; w')).result).toBe(
    '[Number: 2] { x: 1 }',
  )
  expect((await run("let w = new String('ab'); w.x = 1; w")).result).toBe(
    "[String: 'ab'] { x: 1 }",
  )
  expect((await run("String(new String('ab')) + new Number(3)")).result).toBe(
    'ab3',
  )
})

it('runs the string and number prototype host ops like the Rust host', async () => {
  expect((await run("'abcabc'.indexOf('b', 2)")).result).toBe('4')
  expect((await run("'abc'.includes('a', 1)")).result).toBe('false')
  expect((await run("'abc'.startsWith('b', 1)")).result).toBe('true')
  expect((await run("'abc'.endsWith('b', 2)")).result).toBe('true')
  expect((await run("'a,b,c'.split(',', 2)")).result).toBe('[ a, b ]')
  expect((await run('(255).toString(16) + (0.5).toString(2)')).result).toBe(
    'ff0.1',
  )
  expect((await run('(-255).toString(36)')).result).toBe('-73')
})

it('runs the number formatting and Math host ops like the Rust host', async () => {
  expect((await run('(1.25).toFixed(1) + (2.5).toFixed(0)')).result).toBe(
    '1.33',
  )
  expect((await run('(123456).toExponential(2)')).result).toBe('1.23e+5')
  expect((await run('(123456).toExponential()')).result).toBe('1.23456e+5')
  expect((await run('(0.000123).toPrecision(2)')).result).toBe('0.00012')
  expect(
    (await run('Math.log(1) + Math.cos(0) + Math.atan2(0, 1)')).result,
  ).toBe('1')
  expect((await run('Math.cbrt(27) + Math.log2(8)')).result).toBe('6')
  expect((await run('1 / Math.sin(-0)')).result).toBe('-Infinity')
  expect((await run('Math.acosh(0.5)')).result).toBe('NaN')
  expect((await run('const r = Math.random(); r >= 0 && r < 1')).result).toBe(
    'true',
  )
})

it('runs the String statics and remaining string methods like the host', async () => {
  const cases: [string, string][] = [
    ['String.fromCharCode(104, 105, 233)', 'hié'],
    [
      'String.fromCharCode(0xd83d, 0xde00) === String.fromCodePoint(0x1f600)',
      'true',
    ],
    ["String.raw({ raw: ['a', 'b', 'c'] }, 1, 2)", 'a1b2c'],
    ["'abc'.at(-1) + 'abc'.charCodeAt(1) + 'abc'.codePointAt(0)", 'c9897'],
    ["'abc'.charCodeAt(5)", 'NaN'],
    ["'abc'.at(5)", 'undefined'],
    ["'a'.concat('b', 1, null)", 'ab1null'],
    ["'canal'.lastIndexOf('a', 2) + ',' + 'canal'.lastIndexOf('')", '1,5'],
    ["'a'.localeCompare('b') + ',' + 'b'.localeCompare('a')", '-1,1'],
    ["'abc'.normalize('NFKD')", 'abc'],
    ["'hello'.search('ll')", '2'],
    ["'hello'.match('ll').index", '2'],
    ["'abc'.replace('b', \"[$`$&$'$$$1]\")", 'a[abc$$1]c'],
    ["'xaya'.replaceAll('a', (m, p) => p)", 'x1y3'],
    ["'ab'.replaceAll('', '-')", '-a-b-'],
    ["'éa'.replace('a', 'b')", 'éb'],
    ["'straße'.toUpperCase()", 'STRASSE'],
    ["' a　'.trim()", 'a'],
    [
      "try { 'éa'.slice(1) } catch (e) { e.message }",
      'String.prototype.slice does not support non-ASCII strings yet',
    ],
    ["'éaèa'.indexOf('a', 2) + ',' + 'éaèa'.lastIndexOf('a', 2)", '3,1'],
    ["'éa'.replace('a', (m, p) => p) + 'éa'.startsWith('a', 1)", 'é1true'],
    ["try { 'a'.normalize('x') } catch (e) { e.name }", 'RangeError'],
    ["try { 'a.b'.search('.') } catch (e) { e.name }", 'TypeError'],
    ['try { String.fromCharCode(0xd800) } catch (e) { e.name }', 'TypeError'],
  ]
  for (const [src, expected] of cases) {
    expect((await run(src)).result, src).toBe(expected)
  }
})

it('renders symbols like the Rust host', async () => {
  expect((await run('typeof Symbol()')).result).toBe('symbol')
  expect((await run('Symbol("a")')).result).toBe('Symbol(a)')
  expect((await run('Symbol()')).result).toBe('Symbol()')
  expect((await run('[Symbol("a")]')).result).toBe('[ Symbol(a) ]')
  expect((await run('({ a: 1, [Symbol("b")]: 2 })')).result).toBe(
    '{ a: 1, [Symbol(b)]: 2 }',
  )
  expect((await run('let o = {}; o[Symbol("b")] = 2; o.a = 1; o')).result).toBe(
    '{ a: 1, [Symbol(b)]: 2 }',
  )
  await expect(run('throw Symbol("a")')).rejects.toThrow(
    /^Uncaught Symbol\(a\)$/,
  )
})
