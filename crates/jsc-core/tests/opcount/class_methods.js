class Shape {
  constructor(name) { this.name = name; this.hits = 0; }
  area() { return 0; }
  get label() { return this.name + ":" + this.area(); }
  set tag(v) { this.hits = this.hits + v; }
}
class Rect extends Shape {
  constructor(w, h) { super("rect"); this.w = w; this.h = h; }
  area() { return this.w * this.h; }
}
let total = 0;
for (let i = 1; i <= 10; i++) {
  const r = new Rect(i, i + 1);
  r.tag = i;
  total += r.area() + r.hits;
}
console.log(total, new Rect(2, 3).label, new Shape("s").label);
