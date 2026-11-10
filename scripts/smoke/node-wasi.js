// Smoke program for scripts/node-wasi-smoke.mjs (web.yml): console output,
// a promise reaction and an async function resumed by the module's own
// microtask queue.
console.log('answer', 42)
Promise.resolve('resolved').then((v) => console.log('then', v))
async function f() {
  const v = await Promise.resolve(7)
  console.log('awaited', v * 6)
}
f()
console.log('sync done')
