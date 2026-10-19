# Notes: compile-time object specialization, builtin dispatch, functions/classes

Fact notes feeding `design/object-model.md` (phase 2b). Every current-state claim
cites `path:line` at commit `b1dc6b9`. "Probe" lines record the observed output of
the CLI built from that commit (`jsc -p SRC` / `jsc -e SRC`, exit code in
brackets); they illustrate a cited code path and are not separate claims.

Out of scope here (sibling notes): heap block layouts and host imports
(`notes-heap.md`), arrays/strings/primitives/GC (`notes-runtime.md`), test262
clusters and harness M1 (`notes-test262.md`).

Concurrent workstreams that may change what is described below:

- **mr9syrjv** (runtime-semantics, `emit/**`, `run/**`): primitive-receiver
  property-write guard, string property reads (`"abc".foo`), `+` coercion, `**`,
  unhandled rejections, `parseInt` radix.
- **iqxrfhn5** (codegen-language, `codegen/**`): closures in generators, nested
  function hoisting, member destructuring targets, `try` completion values,
  capture gaps, `yield*` over arrays.

Sections touching their scope carry a *May change* note.

## 0. Orientation

- `codegen::lower` runs three passes: collect top-level function declarations
  and run capture analysis (`crates/jsc-core/src/codegen/mod.rs:69-86`), assign
  direct-call indices to declarations (`crates/jsc-core/src/codegen/mod.rs:88-114`),
  lower `main` before the declaration bodies so class shapes are recorded first
  (`crates/jsc-core/src/codegen/mod.rs:158-185`), then lower each declaration and
  append lifted functions (`crates/jsc-core/src/codegen/mod.rs:187-204`).
- Shared lowering state holds the declaration table, the uniform arity `K`, the
  lifted-function accumulator, the capture set, the class table, module cells and
  the static object shapes (`crates/jsc-core/src/codegen/mod.rs:218-262`).
- The output is `IrProgram` (`crates/jsc-core/src/ir.rs:1295`); every builtin is
  a dedicated `IrNode` variant (`crates/jsc-core/src/ir.rs:356-1097`), so the
  set of "known" operations is closed at the IR level.

## 1. Fixed-shape objects

### 1.1 Where a key set is decided

There is no shape/hidden-class concept in the IR: an object is an `IrNode::Object`
list of `(String, IrNode)` entries (`crates/jsc-core/src/ir.rs:542-548`) whose
key set is fixed at allocation. Key sets come from these sources only:

| source | where the key list is built | notes |
|---|---|---|
| object literal | `lower_object` `crates/jsc-core/src/codegen/expr.rs:1516-1571` | keys must be identifier/string (`prop_name_key` `crates/jsc-core/src/codegen/expr.rs:1930-1941`); spread `crates/jsc-core/src/codegen/expr.rs:1521-1527` and getters/setters `crates/jsc-core/src/codegen/expr.rs:1556-1562` are rejected; methods lift to function values `crates/jsc-core/src/codegen/expr.rs:1542-1555` |
| `new C()` instance | `lower_new_class` `crates/jsc-core/src/codegen/expr.rs:1831-1878` | entries = `@@prototype` link first, then one `undefined` slot per scanned field (`crates/jsc-core/src/codegen/expr.rs:1855-1860`) |
| class value | `lower_class_decl` `crates/jsc-core/src/codegen/stmt.rs:490-501` | `@@constructor`, `@@prototype`, then static members |
| class prototype | `crates/jsc-core/src/codegen/stmt.rs:373-441` | methods, accessor boxes, and (derived class) a `@@prototype` link to the parent prototype |
| `new Error/TypeError/RangeError` | `crates/jsc-core/src/codegen/expr.rs:1745-1750` | `name`, `message`, `stack`, `@@errorkind` |
| engine-raised TypeError | `internal_type_error` `crates/jsc-core/src/emit/mod.rs:396` | same four keys |
| `JSON.parse` result | in-wasm parser, runtime keys (`crates/jsc-core/src/ir.rs:949-962`) | only path that allocates objects with keys unknown to codegen |
| object rest `{a, ...r}` | `__obj_rest` (`crates/jsc-core/src/ir.rs:549-561`) | copy minus excluded keys |

### 1.2 The class field scan

- The instance shape of a class is the union of field declarations and every
  `this.<name> = ..` assignment target in the constructor body, in first-seen
  order: `collect_instance_fields` `crates/jsc-core/src/codegen/stmt.rs:1643-1649`,
  `record_this_field` `crates/jsc-core/src/codegen/stmt.rs:1652-1672` (identifier
  or string-literal computed key only).
- The walk recurses through statements/expressions
  (`crates/jsc-core/src/codegen/stmt.rs:1677-1825`) but not into nested
  function/arrow bodies (`crates/jsc-core/src/codegen/stmt.rs:1640-1642`).
- Field declarations `f = init` are desugared to synthetic `this.f = init`
  statements prepended to the constructor body
  (`crates/jsc-core/src/codegen/stmt.rs:279-282`, `field_init_stmt`
  `crates/jsc-core/src/codegen/stmt.rs:1603-1633`), so they are picked up by the
  same scan (`crates/jsc-core/src/codegen/stmt.rs:310`).
- A derived class prepends the parent's flattened field list
  (`crates/jsc-core/src/codegen/stmt.rs:311-323`).
- The result is recorded by class **name** in `Shared::classes`
  (`crates/jsc-core/src/codegen/stmt.rs:331-336`, `ClassInfo`
  `crates/jsc-core/src/codegen/mod.rs:264-275`) before the bodies are lowered, and
  read back by `new` (`crates/jsc-core/src/codegen/expr.rs:1837-1843`).
- Consequences: a field first assigned in a method (not the constructor), from an
  arrow inside the constructor, or via a dynamic key `this[k]` is not pre-slotted,
  so the write throws the add-property `TypeError` (§1.4). A subclass that declares
  fields without a constructor is rejected
  (`crates/jsc-core/src/codegen/stmt.rs:352-359`).

### 1.3 The static-shape side table (diagnostics only)

- Capture analysis records `let`/`const` bindings initialized with an all-static-key
  object literal (`record_shape_candidate`
  `crates/jsc-core/src/codegen/capture.rs:189-198`, `object_literal_keys`
  `crates/jsc-core/src/codegen/capture.rs:661-677`) and drops any whose **name** is
  ever assigned anywhere in the program
  (`crates/jsc-core/src/codegen/capture.rs:49-56`; the name-based `assigned` set is
  documented at `crates/jsc-core/src/codegen/mod.rs:296-303`).
- The table (`Shared::object_shapes`, `crates/jsc-core/src/codegen/mod.rs:255-261`)
  is consulted only by `receiver_object_shape`
  (`crates/jsc-core/src/codegen/expr.rs:1101-1110`) for the unknown-method
  diagnostic (§5). It is **not** used to resolve property slots.

### 1.4 How an access site resolves today

No property access is resolved to a slot or offset at compile time. Every site
is a by-name runtime lookup:

- Read `o.k` lowers to `IrNode::Index { obj, index: Str("k") }`
  (`crates/jsc-core/src/codegen/expr.rs:97-102`); `o[e]` to the same node with a
  computed key (`crates/jsc-core/src/codegen/expr.rs:103-106`).
- `emit_index_node` (`crates/jsc-core/src/emit/object.rs:1329-1447`) dispatches on
  the receiver's type byte at run time: Map/Set → `size` header
  (`crates/jsc-core/src/emit/object.rs:1358-1380`), object → `__obj_get`
  (`crates/jsc-core/src/emit/object.rs:1384-1395`), array → bounds-checked element
  (`crates/jsc-core/src/emit/object.rs:1397-1422`), string → one-byte string
  (`crates/jsc-core/src/emit/object.rs:1427-1438`), anything else → the unchecked
  string path (`crates/jsc-core/src/emit/object.rs:1439-1441`).
- `__obj_get` linearly scans entries comparing keys by byte content via `__str_eq`
  (`crates/jsc-core/src/emit/runtime.rs:494-519`), then, if the program interned
  `@@prototype`, rescans for the own `@@prototype` slot and recurses
  (`crates/jsc-core/src/emit/runtime.rs:590-647`; gate at
  `crates/jsc-core/src/emit/mod.rs:1073-1077`). A miss yields `undefined`
  (`crates/jsc-core/src/emit/runtime.rs:650-653`).
- Literal keys are interned static strings written as `HEAP_BITS | offset`
  (`crates/jsc-core/src/emit/object.rs:56-66`), so two sites naming the same
  literal key share one key pointer; the lookup still compares bytes.
- When the program defines an accessor, every member read appends a getter
  postlude (`crates/jsc-core/src/emit/expr.rs:233-241`,
  `emit_accessor_get` `crates/jsc-core/src/emit/expr.rs:601-680`); the gate is the
  interned `@@accessor` sentinel (`crates/jsc-core/src/emit/mod.rs:903-906`).
- `.length` is claimed syntactically for **every** receiver as `IrNode::StrLen`
  (`crates/jsc-core/src/codegen/expr.rs:94-96`, also in optional chains
  `crates/jsc-core/src/codegen/expr.rs:348-350`); `emit_str_len` reads the u32
  header at `+HEADER_SIZE` with no type check
  (`crates/jsc-core/src/emit/expr.rs:1017-1036`).
  Probe: `jsc -p 'let o = {length: 7, a: 1}; o.length'` → `2` (the entry count, not the `length` property).

Write `o.k = v` / `o[e] = v`:

- Lowered to `IrNode::IndexSet` (`crates/jsc-core/src/codegen/expr.rs:1288-1333`);
  `.length` is not an assignable target (`crates/jsc-core/src/codegen/expr.rs:1293-1305`).
  Compound and update forms lower to `IndexOpSet`
  (`crates/jsc-core/src/codegen/expr.rs:1307-1325`,
  `crates/jsc-core/src/codegen/expr.rs:1420-1461`).
- `emit_index_set_node` (`crates/jsc-core/src/emit/object.rs:1533-1591`): object →
  `__obj_set`, which stores only into an existing key and returns a hit flag
  (`crates/jsc-core/src/emit/runtime.rs:656-670`); a miss calls `__prop_throw`
  (`crates/jsc-core/src/emit/object.rs:1596-1615`,
  `crates/jsc-core/src/emit/object.rs:1696-1701`). Arrays store in bounds, throw the
  grow error past the end, and throw the add-property error for non-index keys or
  other receivers (`crates/jsc-core/src/emit/object.rs:1625-1692`). With accessors,
  the key is first resolved through the chain to find a setter
  (`crates/jsc-core/src/emit/object.rs:1715-1797`).
- *May change (mr9syrjv):* the write path on primitive receivers.

### 1.5 Unknown-key behaviour summary

| operation | lowering | unknown key / unexpected receiver |
|---|---|---|
| read `o.k`, `o[k]` | `Index` (`crates/jsc-core/src/codegen/expr.rs:97-106`) | `undefined` after own scan + prototype walk (`crates/jsc-core/src/emit/runtime.rs:590-653`) |
| write `o.k = v` | `IndexSet` (`crates/jsc-core/src/codegen/expr.rs:1326-1331`) | `TypeError` add-property (`crates/jsc-core/src/emit/object.rs:1596-1615`) |
| `delete o.k` | `ObjectDelete` (`crates/jsc-core/src/codegen/expr.rs:192-216`) | no-op, yields `true` (`crates/jsc-core/src/emit/object.rs:388-404`); a hit compacts the entry table (`crates/jsc-core/src/emit/runtime.rs:1436-1449`) (probe: `delete o.b` on `{a: 1}` → `true`) |
| `k in o` | `ObjectHas` (`crates/jsc-core/src/codegen/expr.rs:137-140`) | own keys only, no prototype walk (`crates/jsc-core/src/emit/runtime.rs:1382-1392`) (probe: `"m" in new A()` for a prototype method `m` → `false`) |
| `for (k in o)` | `StrLen` count + `ObjectKey` per index (`crates/jsc-core/src/codegen/stmt.rs:1403-1456`) | object → raw stored key at entry `i` (`crates/jsc-core/src/emit/object.rs:1461-1503`), no reserved-key filter (probe: `for (const k in new A())` prints `@@prototype` then `x`); array/string → `String(i)`; literal non-object heads rejected (`crates/jsc-core/src/codegen/stmt.rs:1393-1399`) |
| `Object.keys/values/entries` | `ObjectBuiltin` (`crates/jsc-core/src/codegen/op.rs:258-290`) | insertion order; keys whose first byte is `@` are skipped (`crates/jsc-core/src/emit/runtime.rs:1122-1148`) (probe: `Object.keys({"@a": 1, b: 2})` → `[ b ]`) |

Ordering: entries are stored in literal/field-scan order
(`crates/jsc-core/src/emit/object.rs:54-76`) and enumerated in storage order, so
there is no integer-key-first ordering (probe: `Object.keys({b: 1, "1": 2})` → `[ b, 1 ]`).

## 2. Syntactic builtin dispatch

### 2.1 Call sites (`lower_call`)

`lower_call` (`crates/jsc-core/src/codegen/expr.rs:516-737`) tries the following
in order; the first match wins. "Shadow check" means the base identifier must
not resolve to a user binding (`self.resolve(name).is_none()`). "Any receiver"
means the method name is claimed regardless of the receiver expression, so a
user method of that name is unreachable through `recv.name(..)`.

| # | pattern | handler / IR | shadow check | arity check | citation |
|---|---|---|---|---|---|
| 1 | `super(..)` | `lower_super_ctor_call` → `ThisCall` | n/a | none | `crates/jsc-core/src/codegen/expr.rs:518-520` |
| 2 | `super.m(..)` | `lower_super_method_call` → `ThisCall` | n/a | none | `crates/jsc-core/src/codegen/expr.rs:525-527` |
| 3 | any call with a spread argument | `lower_spread_call` | see §2.3 | — | `crates/jsc-core/src/codegen/expr.rs:530-532` |
| 4 | `console.log(..)` | `ConsoleLog` | `console` | none | `crates/jsc-core/src/codegen/expr.rs:542-544`, `crates/jsc-core/src/codegen/op.rs:14-23` |
| 5 | `Math.<m>(..)`: floor ceil round trunc abs sqrt min max pow | `MathCall` | `Math` | 1 / 2 (`pow`) / any (`min`,`max`); unknown `m` is a compile error | `crates/jsc-core/src/codegen/expr.rs:549-553`, `crates/jsc-core/src/codegen/op.rs:58-93` |
| 6 | `<recv>.push(x)`, `<recv>.pop()` | `ArrayPush` / `ArrayPop` | none (any receiver) | 1 / 0 | `crates/jsc-core/src/codegen/expr.rs:558-563`; receiver must be an identifier bound to a plain (uncaptured) local, else compile error `crates/jsc-core/src/codegen/expr.rs:888-909` |
| 7 | `<recv>.map/filter/forEach(cb)`, `.reduce(cb, init)` | `ArrayHof` | none (any receiver) | 1 / 2 | `crates/jsc-core/src/codegen/expr.rs:568-573`, `crates/jsc-core/src/codegen/expr.rs:944-991` |
| 8 | `<recv>.indexOf includes join slice toUpperCase toLowerCase charAt trim trimStart trimEnd repeat startsWith endsWith padStart padEnd substring split` | `MethodCall` (host `__method`) | none (any receiver) | per op `crates/jsc-core/src/codegen/expr.rs:1011-1037` | `crates/jsc-core/src/codegen/expr.rs:580-585`, `crates/jsc-core/src/codegen/op.rs:100-121` |
| 9 | `Object.keys/values/entries(o)` | `ObjectBuiltin` | `Object` | 1; other names compile error | `crates/jsc-core/src/codegen/expr.rs:590-594`, `crates/jsc-core/src/codegen/op.rs:184-198`, `crates/jsc-core/src/codegen/op.rs:258-290` |
| 10 | `JSON.stringify(v)`, `JSON.parse(s)` | `JsonStringify` / `JsonParse` | `JSON` | exactly 1; other names compile error | `crates/jsc-core/src/codegen/expr.rs:599-603`, `crates/jsc-core/src/codegen/op.rs:296-361` |
| 11 | `Promise.resolve/reject(v?)` | `PromiseResolve` / `PromiseReject` | `Promise` | 0-1; other names compile error | `crates/jsc-core/src/codegen/expr.rs:608-612`, `crates/jsc-core/src/codegen/op.rs:205-251` |
| 12 | `Date.now()` | `DateNow` | `Date` | 0 | `crates/jsc-core/src/codegen/expr.rs:616-631` |
| 13 | `<recv>.getTime()` | `DateGetTime` (non-Date yields `NaN`, `crates/jsc-core/src/ir.rs:1066-1072`) | none (any receiver) | 0 | `crates/jsc-core/src/codegen/expr.rs:636-647` |
| 14 | `<recv>.valueOf getFullYear getMonth getDate getDay getHours getMinutes getSeconds toISOString()` | `DateMethod`: type-byte test, else user-method fallback | none, but falls back | none | `crates/jsc-core/src/codegen/expr.rs:652-657`, `crates/jsc-core/src/codegen/op.rs:385-398`, fallback `crates/jsc-core/src/emit/object.rs:624-684` |
| 15 | `<recv>.then/catch/finally(..)` | `PromiseThen` / `PromiseFinally` | none (any receiver) | ≤2 / ≤1 | `crates/jsc-core/src/codegen/expr.rs:663-668`, `crates/jsc-core/src/codegen/expr.rs:743-777` |
| 16 | `<recv>.set/get/has/add/delete(..)` | `MapMethod`: type-byte test, else user-method fallback | none, but falls back | none | `crates/jsc-core/src/codegen/expr.rs:674-679`, `crates/jsc-core/src/codegen/op.rs:368-377`, fallback `crates/jsc-core/src/emit/object.rs:1112-1180` |
| 17 | `<recv>.next(v?)` | `GenResume` (non-generator throws `TypeError`) | none (any receiver) | extra args dropped | `crates/jsc-core/src/codegen/expr.rs:684-691` |
| 18 | any other `<recv>.m(..)` / `<recv>[k](..)` | `lower_method_call_user` → `MethodCallUser` (§3.2) | — | none | `crates/jsc-core/src/codegen/expr.rs:697-699` |
| 19 | `<non-ident callee>(..)` | `CallIndirect` | — | none | `crates/jsc-core/src/codegen/expr.rs:700-707` |
| 20 | `x(..)`, `x` a binding | `CallIndirect` | — | none | `crates/jsc-core/src/codegen/expr.rs:712-716` |
| 21 | `f(..)`, `f` a top-level function declaration | `Call` (direct) | binding wins over declaration | none (pad/truncate) | `crates/jsc-core/src/codegen/expr.rs:717-724` |
| 22 | `String/Number/Boolean/parseInt/parseFloat(x)` | `ConvertCall` | binding and declaration | 1 (`parseInt` 1-2, radix dropped) | `crates/jsc-core/src/codegen/expr.rs:725-730`, `crates/jsc-core/src/codegen/op.rs:127-178` |
| 23 | anything else named | compile error `call to undeclared function` | — | — | `crates/jsc-core/src/codegen/expr.rs:731-735` |

The "any receiver" rows silently hijack user methods of the same name. Probes:
`let o = { push(x) { return 1 } }; o.push(2)` → `2` (an `ArrayPush` on the
object, not the user method); `let o = { map(x) { return 1 } }; o.map(2)` →
`[ 1 ]`; `let o = { a: 5 }; o.map((x) => x + 1)` → `[ a1 ]`. Only rows 14 and 16
test the receiver type and fall back to `MethodCallUser`.

*May change (mr9syrjv):* nullish guards on the `then/catch/finally` and `next`
receivers, and the `next` claim itself (campaign plan item R7).

### 2.2 Non-call sites recognized by name

| pattern | handler | citation |
|---|---|---|
| `Math.PI`, `Math.E` (unshadowed `Math`) | folded `Num` constant | `crates/jsc-core/src/codegen/expr.rs:83-92` |
| `undefined`, `NaN`, `Infinity` (no binding, no declaration) | literals | `crates/jsc-core/src/codegen/expr.rs:400-403` |
| `<recv>.length` (any receiver) | `StrLen` | `crates/jsc-core/src/codegen/expr.rs:94-96` |
| `<Map/Set>.size` | type-byte intercept inside every `Index` | `crates/jsc-core/src/emit/object.rs:1358-1380` |
| `a ** b`, `x **= e` | `MathCall { Pow }` | `crates/jsc-core/src/codegen/expr.rs:131-134`, `crates/jsc-core/src/codegen/expr.rs:1184-1213` |
| unary `+x` | `ConvertCall { ToNumber }` | `crates/jsc-core/src/codegen/expr.rs:178-184` |
| `new Map()`, `new Set()` (no args) | `MapNew` | `crates/jsc-core/src/codegen/expr.rs:1668-1681` |
| `new Promise(executor)` | `PromiseCtor` | `crates/jsc-core/src/codegen/expr.rs:1685-1687`, `crates/jsc-core/src/codegen/expr.rs:1795-1823` |
| `new Date()`, `new Date(ms)` | `DateNew` | `crates/jsc-core/src/codegen/expr.rs:1690-1692`, `crates/jsc-core/src/codegen/expr.rs:1758-1789` |
| `new Error/TypeError/RangeError(msg?)` | inline `Object` with brand | `crates/jsc-core/src/codegen/expr.rs:1693-1750` |
| `x instanceof Error/TypeError/RangeError/Array/Function/Date/Object` | `InstanceOf` kind | `crates/jsc-core/src/codegen/expr.rs:1631-1645` |

Note: `new X(..)` for the error constructors is matched by name without a shadow
check (`crates/jsc-core/src/codegen/expr.rs:1693`), unlike `Map`/`Set`/`Promise`/
`Date` (probe: `function f(Error) { return new Error("m") } f(1).message` → `m`).

### 2.3 Spread calls

`lower_spread_call` (`crates/jsc-core/src/codegen/expr.rs:793-873`) accepts only
`Math.max/min(...)` (`MathSpread`, `crates/jsc-core/src/codegen/expr.rs:813-828`),
non-member indirect callees, bindings and declarations; every member callee
(builtin or user method) is rejected
(`crates/jsc-core/src/codegen/expr.rs:834-840`), as are the conversions
(`crates/jsc-core/src/codegen/expr.rs:861-866`).

### 2.4 Builtins that exist only as syntax

None of the following names is a value. Reading one as an identifier falls
through `lower_ident` to `undeclared variable`
(`crates/jsc-core/src/codegen/expr.rs:391-409`):
`console`, `Math`, `JSON`, `Object`, `Promise`, `Date`, `Map`, `Set`, `Error`,
`TypeError`, `RangeError`, `String`, `Number`, `Boolean`, `parseInt`,
`parseFloat`, `Array`, `Function`, `globalThis`. Consequences (probes: `let m = Math` → `undeclared variable: Math`; `console.error("x")` → `undeclared variable: console`; `globalThis` → `undeclared variable: globalThis`):

- `let m = Math`, `f(Math.max)`, `console.error(..)` (a `MethodCallUser` on
  `console`) and `Object.assign(..)` (rejected by name,
  `crates/jsc-core/src/codegen/op.rs:267-273`) do not compile.
- `Error("x")` without `new` is `call to undeclared function`
  (`crates/jsc-core/src/codegen/expr.rs:731-735`).
- `x instanceof Map` / `Promise` and any non-identifier right-hand side are
  compile errors (`crates/jsc-core/src/codegen/expr.rs:1607-1647`).
- Builtin prototype methods are not values either: `[].push` without a call is an
  ordinary `Index` read on an array with a string key (→ `undefined`,
  `crates/jsc-core/src/emit/object.rs:1397-1422`).

## 3. Functions and classes

### 3.1 Function values and the uniform ABI

- Every function body is lowered to an `IrFunction` with the uniform layout
  `K` positional slots, `env`, `argc` (`crates/jsc-core/src/codegen/function.rs:155-164`);
  `K` is the program-wide maximum arity, at least 2 when a generator/async
  function exists (`crates/jsc-core/src/codegen/mod.rs:116-134`).
- A function value is `IrNode::FuncValue { table_index, captures }`
  (`crates/jsc-core/src/ir.rs:402-419`), emitted as a `TYPE_FUNCTION` block holding
  the funcref index at `+HEADER_SIZE` and the env at `+HEADER_SIZE+4`
  (`crates/jsc-core/src/emit/expr.rs:850-860`). The block has no property table.
- Top-level declarations get fixed wasm indices (`crates/jsc-core/src/codegen/mod.rs:96-114`);
  naming one in value position yields a non-capturing `FuncValue`
  (`crates/jsc-core/src/codegen/expr.rs:394-398`). Top-level bindings they use
  live in module cells, not an env (`crates/jsc-core/src/codegen/mod.rs:82-86`).
- Arrows, function expressions, nested declarations, object-literal methods and
  class members all go through `lower_lifted_function`
  (`crates/jsc-core/src/codegen/function.rs:345-386`), which reserves a table
  slot, lowers the body in a new frame, and captures cells by reference.
  A function expression's own name is not bound
  (`crates/jsc-core/src/codegen/expr.rs:31-44`); a nested declaration is bound at
  its source position and is not visible in its own body
  (`crates/jsc-core/src/codegen/function.rs:388-415`).
  *May change (iqxrfhn5):* nested declaration hoisting and capture gaps.

### 3.2 Call paths

| path | IR | callee check | `this` |
|---|---|---|---|
| direct `f(..)` | `Call` (`crates/jsc-core/src/codegen/expr.rs:717-724`) | none needed (static index) | reset to `undefined` (`crates/jsc-core/src/emit/expr.rs:57-74`) |
| value `x(..)`, `(expr)(..)`, `o?.()` | `CallIndirect` (`crates/jsc-core/src/codegen/expr.rs:700-716`, `crates/jsc-core/src/codegen/expr.rs:363-379`) | **none**: only the no-funcref-table case throws (`crates/jsc-core/src/emit/expr.rs:1284-1291`) (probes: `let x = 5; x()` → `undefined`; with a declared `function g() { return 1 }`, `let x = {}; x()` → `1`, i.e. the object's entry count is read as funcref index 0) | reset to `undefined` (`crates/jsc-core/src/emit/expr.rs:1294-1296`) |
| method `o.m(..)`, `o[k](..)` | `MethodCallUser` (`crates/jsc-core/src/codegen/expr.rs:1117-1165`) | `emit_is_function` guard → branded `TypeError` (`crates/jsc-core/src/emit/expr.rs:540-566`) | receiver, set just before the call (`crates/jsc-core/src/emit/expr.rs:573-575`) |
| `super(..)`, `super.m(..)` | `ThisCall` (`crates/jsc-core/src/codegen/expr.rs:429-492`) | same guard (`crates/jsc-core/src/emit/expr.rs:716-731`) | enclosing `this` (`crates/jsc-core/src/emit/expr.rs:738-740`) |
| `new C(..)` | `New` (`crates/jsc-core/src/codegen/expr.rs:1831-1878`) | none (ctor is always `C["@@constructor"]`) | fresh instance (`crates/jsc-core/src/emit/expr.rs:806-808`) |
| accessor get/set | inside `Index`/`IndexSet` | `emit_is_function` (`crates/jsc-core/src/emit/expr.rs:637-638`, `crates/jsc-core/src/emit/object.rs:1751-1752`) | receiver |

An optional call `o?.m()` is a chain link that lowers to `CallIndirect` on the
read member value (`crates/jsc-core/src/codegen/expr.rs:363-379`), so it does not
bind `this` (probe: `o?.m()` and `o.m?.()` with `m() { return this === undefined }` → `true`).

### 3.3 `this`

- Carrier: the `__this` wasm global (`crates/jsc-core/src/abi.rs:228-235`). Method
  calls set it, plain calls reset it (`crates/jsc-core/src/emit/expr.rs:465-473`).
- Every lowered function body, including arrows, is marked `allows_this`
  (`crates/jsc-core/src/codegen/function.rs:166-168`); the slot is allocated on the
  first `this` read (`crates/jsc-core/src/codegen/scope.rs:211-227`) and filled by
  a prologue snapshot of `__this` (`crates/jsc-core/src/codegen/function.rs:225-238`).
  Arrows therefore see the `this` of their own call, not the lexical one
  (`DESIGN.md:1057-1058`).
- Top-level `this` is rejected (`crates/jsc-core/src/codegen/expr.rs:236-241`);
  inside a deferred static initializer `this` is the class binding
  (`crates/jsc-core/src/codegen/expr.rs:232-235`).

### 3.4 Constructors, `F.prototype`, function properties, `new.target`

- `new` accepts only an identifier callee (`crates/jsc-core/src/codegen/expr.rs:1650-1657`)
  naming a class recorded in `Shared::classes`
  (`crates/jsc-core/src/codegen/expr.rs:1661-1663`) or one of the builtins in §2.2.
  A plain function is not constructible: `unsupported constructor: F`
  (`crates/jsc-core/src/codegen/expr.rs:1693-1698`).
- The class check is by **name** in the class table, ahead of scope resolution, so
  an inner binding that shadows a class name still takes the class path
  (`crates/jsc-core/src/codegen/expr.rs:1661`, also `instanceof`
  `crates/jsc-core/src/codegen/expr.rs:1621`) (probe: `class A {..} function f(A) { return new A() } f(5)` takes the class path on the parameter and throws `Cannot read properties of undefined (reading '@@prototype')`).
- The constructor's return value is discarded; `new` always yields the
  pre-allocated instance (`crates/jsc-core/src/emit/expr.rs:830-835`).
- `F.prototype`, `F.name`, `F.length`, `f.x = 1`: a function block has no
  property table. A read falls into the "anything else" arm of `emit_index_node`
  (`crates/jsc-core/src/emit/object.rs:1439-1441`); a write raises the
  add-property `TypeError` (`crates/jsc-core/src/emit/object.rs:1688-1690`) —
  probes: `f.prototype` prints bytes of the function block read as a string; `f.x = 1` → `Uncaught TypeError: Cannot add property 'x': dynamic properties are not supported yet`; `new f()` → compile error `unsupported constructor: f`. `f.call/apply/bind` reach `MethodCallUser` and throw `called value is not a function` are rejected at run time
  (test `crates/jsc-core/tests/unsupported.rs:104-110`).
- `new.target` and other meta properties fall to the generic
  `this expression is not supported` (`crates/jsc-core/src/codegen/expr.rs:243`,
  `crates/jsc-core/src/codegen/expr.rs:1982-1992`).

### 3.5 Classes

- `lower_class_decl` (`crates/jsc-core/src/codegen/stmt.rs:169-553`) builds the
  class value as an ordinary `TYPE_OBJECT` with `@@constructor`, `@@prototype`
  and the statics (`crates/jsc-core/src/codegen/stmt.rs:490-501`); keys from
  `crates/jsc-core/src/abi.rs:237-251`. `typeof C` is therefore `"object"`
  (probe: `class C {} typeof C` → `object`).
- Constructor: a lifted function over field initializers + body
  (`crates/jsc-core/src/codegen/stmt.rs:279-306`, `crates/jsc-core/src/codegen/stmt.rs:362-364`);
  a derived class without one aliases the parent constructor value
  (`crates/jsc-core/src/codegen/stmt.rs:349-361`).
- Instance methods live once on the shared prototype object
  (`crates/jsc-core/src/codegen/stmt.rs:366-383`). Instance accessors collapse to
  one `MakeAccessor` box per name (`crates/jsc-core/src/codegen/stmt.rs:384-431`,
  `crates/jsc-core/src/ir.rs:593-610`).
- Static methods and literal-initialized static fields are entries of the class
  object; other static initializers are pre-slotted `undefined` and assigned
  after the binding exists via `IndexSet` with `this` = class
  (`crates/jsc-core/src/codegen/stmt.rs:443-484`,
  `crates/jsc-core/src/codegen/stmt.rs:535-551`). Statics are not inherited
  (`crates/jsc-core/src/codegen/stmt.rs:448-449`). `C.m()` is an ordinary
  `MethodCallUser` with `this` = the class object.
- `extends`: the superclass must be a bare identifier naming an earlier class
  declaration (`crates/jsc-core/src/codegen/stmt.rs:178-197`), so `extends Error`
  fails with `superclass \`Error\` is not a declared class`. The subclass
  prototype gets a `@@prototype` link to the parent prototype
  (`crates/jsc-core/src/codegen/stmt.rs:432-441`).
- `super(..)` reads `Parent["@@constructor"]` and calls it with the current
  `this` (`crates/jsc-core/src/codegen/expr.rs:429-451`); `super.m(..)` reads
  `Parent["@@prototype"].m` (`crates/jsc-core/src/codegen/expr.rs:456-492`);
  computed `super[..]` is rejected (`crates/jsc-core/src/codegen/expr.rs:473-479`).
  `super` is resolved by the class **name** saved in `current_super`
  (`crates/jsc-core/src/codegen/stmt.rs:338-341`), not a home object.
- Rejected forms: class expressions (`crates/jsc-core/src/codegen/expr.rs:1987`),
  computed/async/generator methods and static accessors
  (`crates/jsc-core/src/codegen/stmt.rs:1533-1548`), private members and static
  blocks (`crates/jsc-core/src/codegen/stmt.rs:1570-1578`).

### 3.6 `instanceof`

- Codegen resolves the right-hand side to an `InstanceOfKind`
  (`crates/jsc-core/src/codegen/expr.rs:1607-1647`, enum
  `crates/jsc-core/src/ir.rs:252-275`): a recorded class (checked first), then
  unshadowed builtin names, else compile error.
- Emit (`crates/jsc-core/src/emit/object.rs:217-353`): `Array`/`Function`/`Date`
  compare the type byte, `Object` is "heap and not a string"
  (`crates/jsc-core/src/emit/object.rs:235-243`, `crates/jsc-core/src/emit/object.rs:363-381`);
  a class walks the receiver's `@@prototype` chain by pointer identity against
  `C["@@prototype"]` (`crates/jsc-core/src/emit/object.rs:253-302`); the error
  kinds test the `@@errorkind` brand (`crates/jsc-core/src/emit/object.rs:303-343`).
  No `Symbol.hasInstance`, no `C.prototype` property.

### 3.7 Global bindings / `globalThis`

- There is no global object. Name resolution is: lexical scopes
  (`crates/jsc-core/src/codegen/scope.rs:354-383`), then top-level function
  declarations, then the three global literals, else `undeclared variable`
  (`crates/jsc-core/src/codegen/expr.rs:391-409`). Builtin names are matched
  per call shape (§2), not resolved.
- Top-level bindings referenced from top-level functions live in module cells held
  in wasm globals (`crates/jsc-core/src/codegen/mod.rs:166-179`, `DESIGN.md:560-568`).
- `globalThis` and top-level `this` are not available
  (`crates/jsc-core/src/codegen/expr.rs:236-241`).

## 4. Fast-path mapping

### 4.1 What is statically resolved today (must not regress)

Statically resolved sites are those whose generated code contains no by-name
lookup:

1. Direct calls to top-level declarations (`IrNode::Call`,
   `crates/jsc-core/src/codegen/expr.rs:717-724`): a wasm `call` with a constant index
   (`crates/jsc-core/src/emit/expr.rs:57-74`).
2. `Math.*` (inline `f64` ops), `Math.PI/E` (constants), `**`.
3. `console.log` (host `print` per argument).
4. Conversions, `JSON.*`, `Object.keys/values/entries`, `Promise.*`, `Date.*`,
   `new Map/Set/Promise/Date/Error`.
5. Array/string method families (§2.1 rows 6-8, 13, 15, 17): resolved by name to a
   fixed helper, with the receiver type decided at run time inside the helper.
6. `.length` (`StrLen`: one `i32.load`).
7. Class instance allocation with the field-scan pre-sizing and the constant
   `@@constructor` read.
8. `instanceof` with a builtin right-hand side (type byte or brand test).

Property reads/writes and user method calls are **not** statically resolved
(§1.4): they already pay `__obj_get`'s byte-compare scan, so a generic model must
not make them slower, but there is no slot-level fast path to preserve.

### 4.2 Mapping each specialization to a guarded fast path

| specialization | statically provable (no guard) | guard checkable at the site | generic fallback |
|---|---|---|---|
| direct `f(..)` to a declaration | binding never assigned (today assignment to a declaration name is a compile error because it is not a binding, `crates/jsc-core/src/codegen/expr.rs:1256-1262`) | none needed if the no-assignment rule is kept; otherwise a syntactic `assigned`-set check like `crates/jsc-core/src/codegen/capture.rs:49-56` | `CallIndirect` with an `IsCallable` check |
| `Math.*`, `JSON.*`, `Object.*`, `Promise.*`, `Date.now`, conversions | name unshadowed (current rule) **and** the program never writes a property of the builtin namespace nor lets it escape (only `Name.member` uses) — a whole-program syntactic scan | otherwise: load `Name.member` once and compare against the builtin function identity, then inline | generic `Get` + `Call` |
| claimed array/string names (rows 6-8) | receiver is an array/string literal or a binding proven to hold one, **and** the builtin prototype slot is intact (see next column) | receiver type byte `TYPE_ARRAY`/`TYPE_STRING` plus a program-global "builtin prototypes unmodified" flag cleared by any write to a builtin prototype | generic method lookup on the receiver's prototype chain, then `Call` |
| `then/catch/finally`, `next`, `getTime` | receiver provably a promise/generator/date | type byte (`TYPE_PROMISE`/`TYPE_GENERATOR`/`TYPE_DATE`) + intact flag | generic method call (removes today's claim of the name for every receiver) |
| Map/Set and Date method names | — | already shaped as type byte + user-method fallback (`crates/jsc-core/src/emit/object.rs:1112-1180`, `crates/jsc-core/src/emit/object.rs:624-684`); add the intact flag | the existing `MethodCallUser` fallback becomes the generic path |
| `.length` | receiver statically array/string | type byte array/string | generic `Get(o, "length")` (fixes the any-receiver claim of §1.4) |
| `o.k` on a static-shape binding | binding in `object_shapes` (`crates/jsc-core/src/codegen/capture.rs:49-56`) **and** never deleted from nor dynamically extended — today `delete` compacts entries and shifts slots (`crates/jsc-core/src/emit/runtime.rs:1436-1449`), so slot indices are only stable without `delete` | key pointer identity at the predicted entry index (one `i64.load` + `i64.eq`, enabled by interned literal keys `crates/jsc-core/src/emit/object.rs:56-66`) | generic own + prototype lookup |
| `new C()` pre-sizing | the field scan (§1.2) | none: keep it as the initial capacity/shape of the instance; writes outside the scan go to generic add-property instead of throwing | generic `[[Set]]` with growth |
| `o.m()` on a class instance | receiver is `new C()` bound to a never-reassigned binding, `m` is a prototype method and not a scanned field | prototype-slot identity check (prototypes become mutable objects) | generic method lookup + `IsCallable` |
| `x instanceof C` for a class declaration | `C` unshadowed class declaration | none beyond today's chain walk, if `C.prototype` stays non-writable for classes (spec: class `prototype` is non-writable) | `OrdinaryHasInstance` / `@@hasInstance` |
| `x instanceof Error` etc. | builtin names unshadowed | brand test stays valid once `Error.prototype` is real only if user subclasses also carry the brand | generic prototype walk against `Error.prototype` |

Probe for the first row: `function f() { return 1 } f = 2` → compile error
`undeclared variable: f`.

### 4.4 Current specialization defects the generic path must not inherit

Each item is a place where today's specialization gives a wrong result rather
than falling back; the generic design should treat them as bugs, not as
behaviour to preserve:

- `.length` claimed for every receiver (§1.4).
- "Any receiver" method names hijack user methods (§2.1 rows 6-8, 13, 15, 17).
- Hidden `@@` keys live in the ordinary entry table: `for-in` enumerates them
  (§1.5), and the `Object.keys` filter hides any user key starting with `@`.
- `CallIndirect` has no callee check (§3.2).
- Optional calls drop the receiver as `this` (§3.2).
- Class identity is by name, not by binding, for `new`/`instanceof`/`extends`/`super`
  (§3.4, §3.5); `new Error(..)` ignores shadowing (§2.2).
- `in` ignores the prototype chain (§1.5); constructor return values are
  discarded (§3.4).

### 4.3 Proposed no-regression guard

A static instruction-budget snapshot test, built only on existing infrastructure:

- Corpus: a fixed set of small programs, one per row of §4.1 (recursive `fib`
  via direct call, `Math.*`, `console.log`, array HOFs + `push`, string methods,
  Map/Set, a class with methods/accessors/`extends`, object-literal reads,
  `for-in`, `JSON.stringify/parse`).
- Mechanism: compile in-process through the public pipeline
  (`frontend::parse` `crates/jsc-core/src/frontend.rs:33`,
  `codegen::lower` `crates/jsc-core/src/codegen/mod.rs:52`,
  `emit::emit` `crates/jsc-core/src/emit/mod.rs:888`), then walk the code section
  with `wasmparser` (already a dev-dependency,
  `crates/jsc-core/Cargo.toml:27-28`; operator reading as in
  `crates/jsc-core/src/emit/tests.rs:19-25` and
  `crates/jsc-core/src/emit/tests.rs:385-386`). Record per program: total
  operators in `main` and user functions, number of `call_indirect`, and number
  of `call`s to the property helpers (`__obj_get`/`__obj_set`) and host imports.
- Snapshot: a checked-in TSV next to the test (format like
  `tests/test262/history.tsv`), compared with a `--check`/`--update`-style flow
  modelled on the test262 baseline gate (`crates/jsc-test262/src/baseline.rs:28-45`,
  CLI flags `crates/jsc-test262/src/main.rs:18`, CI job `DESIGN.md:935-937`).
  Any increase fails CI; a decrease requires regenerating the snapshot.
- Complement with structural IR assertions in the codegen unit tests, which
  already assert specific nodes (e.g. direct `Call { func: 1 }` at
  `crates/jsc-core/src/codegen/tests.rs:575-583`): each corpus program must keep
  `Call`, `MathCall`, `ConsoleLog`, `ArrayHof`, … rather than degrading to
  `MethodCallUser`/`CallIndirect`.
- A wall-clock benchmark gate is not recommended as the primary guard: CI has no
  stable runner timing, while the operator counts are deterministic.

## 5. Unknown-method family and the G1/G2 guards

### 5.1 Current layers

| layer | what it catches | where | tests |
|---|---|---|---|
| M1: no funcref table | a program with no functions cannot emit `call_indirect`; method/indirect calls throw the branded `TypeError` instead | `crates/jsc-core/src/emit/expr.rs:533-538`, `crates/jsc-core/src/emit/expr.rs:1284-1291`, `has_funcs` `crates/jsc-core/src/emit/mod.rs:830-834`, `crates/jsc-core/src/emit/mod.rs:899` | `crates/jsc-core/tests/unsupported.rs:261-273` |
| G1a: static primitive/array-literal receiver | `"x".zzz()`, `[1].flat()`, template receivers → compile error `method 'zzz' is not supported` | `crates/jsc-core/src/codegen/expr.rs:1131-1133`, `static_primitive_receiver` `crates/jsc-core/src/codegen/expr.rs:1972-1978`, message `crates/jsc-core/src/codegen/mod.rs:207-216` | `crates/jsc-core/tests/unsupported.rs:118-137`, `crates/jsc-core/tests/unsupported.rs:253-259` |
| G1b: static object shape | method name absent from a known literal shape → same compile error | `crates/jsc-core/src/codegen/expr.rs:1140-1145` with §1.3 | `crates/jsc-core/tests/unsupported.rs:138-164` |
| G1c: unknown builtin static | `Math.foo`, `Object.assign`, `JSON.foo`, `Promise.all` → compile errors | `crates/jsc-core/src/codegen/op.rs:69-73`, `crates/jsc-core/src/codegen/op.rs:267-273`, `crates/jsc-core/src/codegen/op.rs:357-359`, `crates/jsc-core/src/codegen/op.rs:231-237` | — |
| G2: dynamic receiver | resolved member is not a function → branded `TypeError: called value is not a function` | `crates/jsc-core/src/emit/expr.rs:540-566` (`MethodCallUser`), `crates/jsc-core/src/emit/expr.rs:716-731` (`ThisCall`), error object `crates/jsc-core/src/emit/mod.rs:359-405` | `crates/jsc-core/tests/unsupported.rs:165-251` |

Gap: `CallIndirect` has no G2 guard (§3.2), so calling a non-function value
through a binding or an optional call is not covered (see the §3.2 probes: a non-function object can even dispatch to an unrelated function).

### 5.2 Under a generic object model

- G1a and G1b become incorrect: once builtin prototypes are real objects a program
  may add `String.prototype.zzz` or grow an object after allocation, so "absent at
  compile time" no longer implies "absent at run time". Both checks, the
  `object_shapes` side table and `unknown_method_error`, should be deleted (no
  compatibility shims), and the tests in
  `crates/jsc-core/tests/unsupported.rs:118-164` rewritten as run-time
  `TypeError` expectations.
- G1c: `Math.foo(..)` and `Object.assign(..)` become ordinary member reads on
  real namespace objects; an absent member yields `undefined` and the call throws
  at run time. The per-name rejection in `op.rs` disappears as the prelude
  supplies the methods.
- G2 becomes the spec `Call` step (`IsCallable` check) and should apply to every
  call path, including `CallIndirect` and optional calls, closing the §5.1 gap.
  The message should name the callee expression (`o.bar is not a function`) to
  match engines; the branded-object construction can stay.
- M1 remains needed only while emission can lack a funcref table; if builtin
  prelude functions always populate the table, it disappears.
- The static shape knowledge survives only as fast-path input (§4.2), never as a
  diagnostic.
