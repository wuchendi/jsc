const s = "Hello, World";
let out = "";
for (let i = 0; i < 10; i++) {
  out = out + s.toUpperCase().slice(0, 5) + s.charAt(i) + s.indexOf("o") + s.toLowerCase().includes("world");
}
console.log(out.length, s.split(", ").join("|"), s.trim(), s.startsWith("Hell"));
