// Spread over the builtin iterables: arrays, strings, Map, and a generator,
// into array literals and call arguments.
function* count(n) {
  for (let i = 0; i < n; i++) yield i
}
function add3(a, b, c) {
  return a + b + c
}
const arr = []
for (let i = 0; i < 100; i++) arr.push(i)
const copy = [...arr, ...arr]
const chars = [...'abcdefghijklmnopqrstuvwxyz']
const m = new Map()
for (let i = 0; i < 20; i++) m.set('k' + i, i)
const entries = [...m]
const gen = [...count(100)]
console.log(copy.length, chars.length, entries.length, gen.length, add3(...arr))
