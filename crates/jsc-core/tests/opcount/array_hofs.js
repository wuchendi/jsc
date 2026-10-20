const xs = [];
for (let i = 0; i < 30; i++) xs.push(i);
const doubled = xs.map((x) => x * 2);
const evens = doubled.filter((x) => x % 4 === 0);
const total = evens.reduce((a, b) => a + b, 0);
let seen = 0;
xs.forEach((x) => { seen += x; });
console.log(doubled.length, evens.length, total, seen, xs.indexOf(7), xs.includes(31));
