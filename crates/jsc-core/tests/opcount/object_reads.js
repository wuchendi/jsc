const p = { x: 1, y: 2, z: 3, name: "pt" };
let acc = 0;
for (let i = 0; i < 25; i++) {
  acc += p.x + p.y * p.z + p["x"];
}
console.log(acc, p.name);
