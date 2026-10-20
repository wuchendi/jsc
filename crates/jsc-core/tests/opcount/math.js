let acc = 0
for (let i = 1; i <= 50; i++) {
  acc +=
    Math.sqrt(i) +
    Math.floor(i / 3) +
    Math.abs(-i) +
    Math.max(i, 7) +
    Math.min(i, 9)
  acc += Math.PI * i + i ** 2 + Math.round(i / 7) + Math.ceil(i / 4)
}
console.log(Math.floor(acc))
