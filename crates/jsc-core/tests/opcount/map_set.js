const m = new Map()
const s = new Set()
for (let i = 0; i < 20; i++) {
  m.set('k' + (i % 7), i)
  s.add(i % 5)
}
let sum = 0
for (let i = 0; i < 7; i++) {
  if (m.has('k' + i)) sum += m.get('k' + i)
}
m.delete('k0')
console.log(m.size, s.size, sum, s.has(3), s.has(9))
