export interface Example {
  /** Short label for the picker. */
  label: string
  /** One-line hint at what the snippet shows off. */
  note: string
  source: string
}

/**
 * Snippets tuned to show what the compiler actually handles — each one
 * exercises a different corner of the supported language.
 */
export const EXAMPLES: Example[] = [
  {
    label: 'Hello',
    note: 'console.log streams while the program runs',
    source: `console.log('compiled to WebAssembly,')
console.log('running in your browser.')
1 + 1`,
  },
  {
    label: 'Closures',
    note: 'A counter that captures its state by reference',
    source: `function makeCounter() {
  let n = 0
  return () => ++n
}

const next = makeCounter()
next()
next()
next() // -> 3`,
  },
  {
    label: 'Recursion',
    note: 'A recursive function with the ternary operator',
    source: `function fib(n) {
  return n < 2 ? n : fib(n - 1) + fib(n - 2)
}

fib(10) // -> 55`,
  },
  {
    label: 'FizzBuzz',
    note: 'Loops, else-if, and building a string',
    source: `let out = ''
for (let i = 1; i <= 15; i++) {
  if (i % 15 === 0) out += 'FizzBuzz '
  else if (i % 3 === 0) out += 'Fizz '
  else if (i % 5 === 0) out += 'Buzz '
  else out += i + ' '
}

out.trim()`,
  },
  {
    label: 'Classes',
    note: 'this, methods calling methods, and new',
    source: `class Vec {
  constructor(x, y) {
    this.x = x
    this.y = y
  }
  dot(o) {
    return this.x * o.x + this.y * o.y
  }
  len2() {
    return this.dot(this)
  }
}

const v = new Vec(3, 4)
v.len2() // -> 25`,
  },
  {
    label: 'Array methods',
    note: 'map / filter / reduce over a real heap array',
    source: `[1, 2, 3, 4, 5, 6]
  .filter((n) => n % 2 === 0)
  .map((n) => n * n)
  .reduce((a, b) => a + b, 0) // -> 56`,
  },
  {
    label: 'Destructuring',
    note: 'Patterns, defaults, rest, and spread',
    source: `const [first, ...rest] = [10, 20, 30]
const { x, y = 2 } = { x: 40 }
const merged = [...rest, first]

console.log('x + y =', x + y)
merged // -> [ 20, 30, 10 ]`,
  },
  {
    label: 'Template + Map',
    note: 'Template literals and the Map collection',
    source: `const prices = new Map()
prices.set('pen', 3)
prices.set('mug', 12)

const total = prices.get('pen') + prices.get('mug')
const summary = \`total: \${total}\`
summary // -> "total: 15"`,
  },
  {
    label: 'Sets',
    note: 'for-of over an array, dedup with a Set',
    source: `const seen = new Set()
for (const n of [1, 2, 2, 3, 3, 3]) {
  seen.add(n)
}

seen.size // -> 3`,
  },
  {
    label: 'JSON',
    note: 'Serialize a nested object with JSON.stringify',
    source: `const user = {
  name: 'ada',
  langs: ['analytical', 'engine'],
}

JSON.stringify(user)`,
  },
  {
    label: 'Errors',
    note: 'throw / try / catch / finally',
    source: `let message = ''
try {
  throw new Error('boom')
} catch (e) {
  message = e.message
} finally {
  message = message + ' (handled)'
}

message // -> "boom (handled)"`,
  },
]
