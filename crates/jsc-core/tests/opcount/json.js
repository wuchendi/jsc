const data = { name: "jsc", list: [1, 2, 3], nested: { ok: true, none: null } };
const text = JSON.stringify(data);
const back = JSON.parse(text);
const again = JSON.parse('{"a":[1,2,{"b":"c"}],"d":1.5}');
console.log(text, back.list.length, back.nested.ok, again.d, JSON.stringify(again));
