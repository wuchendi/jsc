const o = { a: 1, b: 2, c: 3, d: 4 }
let keys = ''
let sum = 0
for (let r = 0; r < 5; r++) {
  for (const k in o) {
    keys = keys + k
    sum += o[k]
  }
}
console.log(keys, sum)
