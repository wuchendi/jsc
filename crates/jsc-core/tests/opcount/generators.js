// Explicit `it.next()` / `it.return()` on generator objects (the
// %GeneratorPrototype% fast path through each function's own `prototype`).
function* count(n) {
  for (let i = 0; i < n; i++) yield i
}
let sum = 0
const it = count(200)
let r = it.next()
while (!r.done) {
  sum += r.value
  r = it.next()
}
const early = count(10)
early.next()
console.log(sum, early.return(7).value, early.next().done)
