// `for...of` over the builtin iterables: arrays, strings, Map, and a
// generator (the guarded fast paths of the iteration protocol).
function* count(n) {
  for (let i = 0; i < n; i++) yield i
}
const arr = []
for (let i = 0; i < 100; i++) arr.push(i)
let sum = 0
for (const x of arr) sum += x
let chars = 0
for (const c of 'abcdefghijklmnopqrstuvwxyz') if (c !== 'q') chars++
const m = new Map()
for (let i = 0; i < 20; i++) m.set('k' + i, i)
let msum = 0
for (const [k, v] of m) msum += v + k.length
let gsum = 0
for (const g of count(100)) gsum += g
console.log(sum, chars, msum, gsum)
