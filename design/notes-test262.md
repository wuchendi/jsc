# Notes: test262 failure clusters and the M1 harness walkthrough

Input notes for `design/object-model.md` (phase 2c). Describes `main @ b1dc6b9`.
Every `path:line` citation is against that commit. Sibling notes cover heap
layouts (`notes-heap.md`), codegen dispatch (`notes-codegen.md`) and
arrays/strings/GC (`notes-runtime.md`); this note only uses those areas as far
as a failure cluster points at them.

## 1. Run and totals

Command (from the worktree root):

```
export PATH=/root/.cargo/bin:$PATH
bash scripts/test262-fetch.sh            # test262 @ 2e0a5676 (scripts/test262-fetch.sh:5)
cargo build --release -p jsc
cargo run --release -p jsc-test262 -- --check --timeout 30
```

`--timeout 30` instead of the default 10 s (`crates/jsc-test262/src/main.rs:47`)
because the host was heavily loaded during the run (load average > 100 on 8
cores). The scratch directory `.tmp/test262-run`
(`crates/jsc-test262/src/main.rs:25`) was symlinked to tmpfs for the same
reason. There were **0 timeouts**, so the raised limit did not change any
classification. Wall clock: 465.6 s with 8 jobs.

Per-test results are the TSV `.tmp/test262-results.tsv`
(`crates/jsc-test262/src/main.rs:24`, written by `results_tsv` at
`crates/jsc-test262/src/main.rs:221-231`; columns path / outcome label /
first non-empty stderr line, `crates/jsc-test262/src/main.rs:150-160`). A copy
is at `/tmp/bkd-jsc-m2rpoz4i/results.tsv` (not committed).

| Metric | Value |
| --- | --- |
| Total tests | 47665 |
| Eligible (run) | 22992 |
| Pass | **5197 (22.6 % of eligible, 10.9 % of total)** |
| compile-error | 16714 |
| fail (runtime) | 1081 |
| timeout | 0 |
| `--check` | 0 regressed, 2 newly passing (exit 0) |

The previous history entry is `5a97d3c 5195 22992 47665`
(`tests/test262/history.tsv:6`); the +2 are the two newly passing tests
(`built-ins/Math/pow/int32_min-exponent.js`,
`language/expressions/exponentiation/int32_min-exponent.js`).

Top skip reasons (24673 skipped): `flag:async` 5484, `feature:Temporal` 4094,
`include:propertyHelper.js` 3074, `noStrict` 1529, `feature:BigInt` 1173,
`feature:TypedArray` 943, `feature:Symbol.iterator` 876, `feature:Symbol` 836,
`flag:module` 830, `feature:regexp-unicode-property-escapes` 679.

Largest eligible directories (pass / eligible):

| Directory | Pass | Eligible | Pass % |
| --- | ---: | ---: | ---: |
| language/expressions | 2098 | 6592 | 31.8 |
| language/statements | 1763 | 5285 | 33.4 |
| built-ins/Array | 70 | 2498 | 2.8 |
| built-ins/Object | 5 | 2377 | 0.2 |
| built-ins/String | 106 | 928 | 11.4 |
| built-ins/Iterator | 1 | 563 | 0.2 |
| built-ins/RegExp | 0 | 557 | 0.0 |
| built-ins/Date | 11 | 328 | 3.4 |
| built-ins/Function | 0 | 293 | 0.0 |
| built-ins/Set | 19 | 285 | 6.7 |
| built-ins/Number | 72 | 266 | 27.1 |

`built-ins/Array` + `built-ins/Object` + `built-ins/Function` are 5168
eligible tests at 1.5 % pass; they are the main object-model target.

## 2. Failure clusters

Method: every eligible non-pass test (17795 = 16714 + 1081) is put in exactly
one cluster by its TSV detail (the **first** diagnostic or the first stderr
line), using an ordered regex classifier
(`/tmp/bkd-jsc-m2rpoz4i/scripts/cluster.py`, not committed). jsc stops at the
first compile error, so a cluster count is "tests whose *first* blocker is X";
a test may hide further blockers behind it (see the yield factors in §3).

Cluster sums: A 3758, B 1874, C 1478, D 334, E 2079, F 763, G 5703, H 450,
R 1081, Z 275 → 17795.

### A. Builtins are not values (3758)

Builtin names exist only as call/`new`/`instanceof` special cases; as a plain
identifier they fall through `lower_ident` to `undeclared variable`
(`crates/jsc-core/src/codegen/expr.rs:391-408`, message at
`crates/jsc-core/src/codegen/expr.rs:405`). Unknown callees end at
`call to undeclared function` (`crates/jsc-core/src/codegen/expr.rs:731-735`,
`crates/jsc-core/src/codegen/expr.rs:869`).

| ID | Cluster | Count | Detail / breakdown | Examples |
| --- | --- | ---: | --- | --- |
| A1 | Builtin constructor or namespace used as a value (`Array.prototype.x.call`, `String.prototype`, `typeof Math`, `Object.prototype.toString`) | 2595 | Array 577, String 348, Number 306, Object 303, Date 182, Iterator 141, Function 108, Promise 107, Math 101, Set 87, RegExp 78, JSON 60, Boolean 52, WeakMap 43, Map 41, WeakSet 37 | `built-ins/Array/constructor.js`, `built-ins/Function/prototype/bind/15.3.4.5-2-4.js`, `built-ins/Object/getOwnPropertyDescriptor/15.2.3.3-4-71.js` |
| A2 | Builtin error constructor as a value, nearly always `assert.throws(ReferenceError, …)` / `SyntaxError` / unshimmed names | 508 | ReferenceError 345, SyntaxError 82, Error 38, … | `language/expressions/arrow-function/dflt-params-ref-later.js`, `built-ins/AsyncFunction/is-not-a-global.js` |
| A3 | Missing global functions | 500 | `eval` 352 (call + value), `isNaN` 84, `encodeURI*`/`decodeURI*` 38, `isFinite` 5, … | `language/eval-code/direct/cptn-nrml-empty-block.js`, `language/expressions/addition/S11.6.1_A3.1_T2.3.js` |
| A4 | Builtin constructor called as a function (`Object(x)`, `Function(..)`, `Symbol()`, `Array(..)`, `Error(..)`) | 155 | Function 64, Object 28, Symbol 23, Error 9, Array 9 | `built-ins/Object/S15.2.1.1_A1_T3.js`, `built-ins/Function/S15.3.2.1_A1_T5.js` |

### B. Object reflection statics (1874)

`lower_object_call` accepts only `keys`/`values`/`entries`
(`crates/jsc-core/src/codegen/op.rs:258-274`); anything else is
`unsupported Object method` (`crates/jsc-core/src/codegen/op.rs:270`).

| ID | Cluster | Count | Breakdown | Examples |
| --- | --- | ---: | --- | --- |
| B1 | Descriptor / prototype / integrity family | 1828 | defineProperty 1260, defineProperties 161, create 137, getOwnPropertyDescriptor 85, getPrototypeOf 37, preventExtensions 36, isExtensible 27, getOwnPropertyNames 24, freeze 24, seal 14, setPrototypeOf 9, isFrozen 7, getOwnPropertyDescriptors 5, isSealed 2 | `built-ins/Array/length/define-own-prop-length-error.js`, `built-ins/Object/create/15.2.3.5-2-2.js`, `built-ins/Object/defineProperty/15.2.3.6-4-162.js` |
| B2 | Other statics | 46 | is 13, assign 12, groupBy 10, hasOwn 4, fromEntries 3, … | `built-ins/Object/groupBy/invalid-property-key.js` |

### C. `new` on builtin constructors (1478)

`lower_new` handles user classes, `Map`/`Set` (empty only), `Promise`, `Date`,
`Error`/`TypeError`/`RangeError`; everything else is
`unsupported constructor: <name>` (`crates/jsc-core/src/codegen/expr.rs:1649`,
`crates/jsc-core/src/codegen/expr.rs:1693-1698`).

| ID | Cluster | Count | Examples |
| --- | --- | ---: | --- |
| C1 | Primitive wrappers / `new Object`: String 302, Boolean 209, Number 173, Object 112 | 796 | `built-ins/Array/S15.4_A1.1_T8.js`, `built-ins/String/prototype/lastIndexOf/S15.5.4.8_A4_T2.js`, `language/types/undefined/S8.1_A4.js` |
| C2 | `new Array(..)` | 279 | `built-ins/Array/S15.4.2.1_A1.1_T3.js`, `built-ins/Array/isArray/15.4.3.2-0-6.js` |
| C3 | `new Function(..)` (runtime compilation) | 66 | `built-ins/Function/15.3.2.1-11-8-s.js` |
| C4 | Other builtins: RegExp 180, ArrayBuffer 92, DataView 17, SyntaxError 11, WeakMap/WeakSet 14, Proxy 8, EvalError 4, … | 337 | `built-ins/Object/create/15.2.3.5-4-172.js`, `built-ins/Array/isArray/15.4.3.2-1-12.js` |

### D. Plain-function constructors (334)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| D1 | `new F(..)` where `F` is a user `function` (`Con` 60, `DummyError` 48, `ConstructFun` 39, `foo`, `FACTORY`, `F`, …) | 267 | same site as C (`crates/jsc-core/src/codegen/expr.rs:1693-1698`): only declared classes reach `lower_new_class` (`crates/jsc-core/src/codegen/expr.rs:1660-1662`) | `built-ins/Array/isArray/15.4.3.2-2-1.js`, `built-ins/Array/prototype/map/15.4.4.19-9-9.js`, `language/statements/try/S12.14_A17.js` |
| D2 | `new` on a non-identifier target (`new obj.f()`, `new (function(){})`) | 67 | `crates/jsc-core/src/codegen/expr.rs:1650-1656` | `built-ins/Error/prototype/S15.11.4_A4.js`, `built-ins/Iterator/prototype/some/non-constructible.js` |

### E. Classes as values and subclassing (2079)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| E1 | Class expressions | 1635 | `crates/jsc-core/src/codegen/expr.rs:1987` | `language/expressions/class/dstr/private-gen-meth-dflt-obj-ptrn-id-init-fn-name-class.js`, `language/statements/variable/dstr/obj-ptrn-id-init-fn-name-class.js` |
| E2 | `extends` a builtin (Iterator 145, Set 18, Promise 14, Error 14, Object 13, …) or a non-identifier (17) | 254 | `crates/jsc-core/src/codegen/stmt.rs:180-195` | `built-ins/Error/isError/error-subclass.js`, `built-ins/Iterator/prototype/chunks/exhaustion-does-not-call-return.js` |
| E3 | Getters/setters in object literals (160) and static class accessors (30) | 190 | `crates/jsc-core/src/codegen/expr.rs:1556-1560`, `crates/jsc-core/src/codegen/stmt.rs:1533-1535` | `built-ins/Array/prototype/toSorted/comparefn-not-a-function.js`, `language/expressions/array/spread-obj-mult-spread-getter.js` |

### F. Non-static property keys (763)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| F1 | Computed / numeric literal keys (608), object-pattern keys (41), computed class names (90), object spread (24). 533 of them are `built-ins/Array` array-likes such as `{ 0: x, length: 2 }` | 763 | `prop_name_key` interns compile-time string keys only (`crates/jsc-core/src/codegen/expr.rs:1927-1940`) | `built-ins/Array/prototype/every/15.4.4.16-3-1.js`, `built-ins/Array/from/elements-added-after.js` |

### G. Language features outside the object model (5703)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| G1 | Private class members | 805 | `crates/jsc-core/src/codegen/stmt.rs:1570-1573` | `language/statements/class/dstr/private-meth-dflt-obj-ptrn-id-init-skipped.js` |
| G2 | Generator / async forms not yet lowered: generator function expressions/methods 885, generator class methods 300, async functions 130, async class methods 73, generator params 34, yield in try 28 | 1456 | `crates/jsc-core/src/codegen/function.rs:52-62` (may change: iqxrfhn5 works on generator closures) | `language/statements/variable/dstr/obj-ptrn-id-init-fn-name-gen.js`, `language/expressions/object/dstr/meth-ary-ptrn-elision-exhausted.js` |
| G3 | `this statement is not supported`: almost all are the **empty statement** `;` (e.g. `class C {…};`, `function f(){};`) | 739 | catch-all at `crates/jsc-core/src/codegen/stmt.rs:111`, message at `crates/jsc-core/src/codegen/stmt.rs:1829-1835` (may change: iqxrfhn5) | `language/statements/class/dstr/meth-ary-ptrn-rest-ary-elem.js`, `language/types/string/S8.4_A10.js` |
| G4 | Regex literals | 445 | `crates/jsc-core/src/codegen/expr.rs:1989` | `built-ins/RegExp/S15.10.2.8_A3_T29.js` |
| G9 | Other syntax / builtin arity gaps | 2258 | for-of patterns 289, bitwise/shift operators 173 (`crates/jsc-core/src/codegen/op.rs:415`) + compound 126 (`crates/jsc-core/src/codegen/op.rs:434`) + `~` 14, array holes 133 (`crates/jsc-core/src/codegen/expr.rs:1477`), comma 133 (`crates/jsc-core/src/codegen/expr.rs:1988`), `new Set(iterable)` 132 / `new Map(iterable)` 12, `new Date(y,m,..)` 80, `void` 78, assignment targets 152, captured `array.push` 60, catch patterns 58, missing/arity-limited array & string methods ~330, tagged templates 36, static blocks 34, `instanceof` RHS 32 (`crates/jsc-core/src/codegen/expr.rs:1607-1646`), labels 28, `delete` 9, … | `language/expressions/compound-assignment/…`, `built-ins/Array/S15.4.5.2_A3_T1.js` |

### H. Exotic objects and the global object (450)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| H1 | `arguments` | 290 | undeclared identifier (`crates/jsc-core/src/codegen/expr.rs:405`) | `built-ins/Array/prototype/forEach/15.4.4.18-1-15.js`, `language/statements/function/params-dflt-ref-arguments.js` |
| H2 | `this` outside a method (top-level / global `this`) | 160 | `crates/jsc-core/src/codegen/expr.rs:232-240` | `built-ins/Infinity/15.1.1.2-0.js`, `built-ins/global/S10.2.3_A2.1_T3.js` |

### R. Runtime failures (1081)

| ID | Cluster | Count | Root cause | Examples |
| --- | --- | ---: | --- | --- |
| R1 | `Cannot add property '<k>': dynamic properties are not supported yet` — writing a key the fixed shape lacks (keys: `constructor` 7, `i` 6, `foo` 4, reserved words as names, …) | 110 | `__prop_throw` (`crates/jsc-core/src/emit/object.rs:2102`), message `crates/jsc-core/src/emit/mod.rs:452-455` | `built-ins/Array/S15.4_A1.1_T4.js`, `built-ins/Object/keys/15.2.3.14-6-5.js`, `language/types/reference/S8.7_A7.js` |
| R2 | `Cannot grow array by index assignment yet` | 34 | `crates/jsc-core/src/emit/mod.rs:458` (same helper, `grow == 1`) | `built-ins/Array/15.4.5.1-5-1.js`, `built-ins/Array/property-cast-number.js` |
| R3 | `called value is not a function`: a method name the static shape does not have — mostly missing builtin methods (`Date.prototype.getUTCMonth`/`toDateString`, `Function.prototype.bind`, `Array.prototype.entries`, `Set.prototype.clear`, Iterator helpers, `hasOwnProperty`) | 283 | runtime guard `crates/jsc-core/src/emit/mod.rs:362-370` | `built-ins/Date/prototype/toDateString/invalid-date.js`, `built-ins/Function/prototype/bind/15.3.4.5-8-1.js`, `language/statements/class/elements/syntax/valid/grammar-static-ctor-meth-valid.js` |
| R4 | Negative (early-error) test compiled and ran `$DONOTEVALUATE()` — missing early errors (redeclarations, `switch` lexical scope, escaped keywords) | 166 | parser/scope checks, not object model | `language/block-scope/syntax/redeclaration/class-name-redeclaration-attempt-with-class.js`, `language/statements/try/early-catch-lex.js` |
| R5 | Uncaught user `Test262Error` thrown by a legacy test body (printed as `Uncaught { @@prototype: … }`, `crates/jsc-test262/src/outcome.rs:9-17`); dominant root cause is ToPrimitive not calling user `valueOf`/`toString` | 63 | coercion (may change: mr9syrjv `+` coercion) | `built-ins/String/S15.5.1.1_A1_T7.js`, `language/expressions/addition/S11.6.1_A2.2_T1.js` |
| R6 | Property read on `undefined` where a builtin/own property was expected | 40 | `crates/jsc-core/src/emit/mod.rs:416-420` | `language/expressions/optional-chaining/optional-call-preserves-this.js` |
| R7 | Assertion mismatches. Includes ~54 tests where a **function's `.name` reads as a one-character non-string** (`SameValue(«" "», «"arrow"»)`): property reads on function values return garbage today (probe: `function a(){} typeof a.foo` → `"string"`, `typeof a.prototype` → `"string"`, `a.foo.length` → 1, `a.foo.charCodeAt` → not a function). Also 41 "expected TypeError, nothing thrown", 32 "expected Test262Error got TypeError" | 310 | function heap layout read as an object (cross-reference `notes-heap.md`) | `language/expressions/arrow-function/dstr/ary-ptrn-elem-id-init-fn-name-arrow.js`, `built-ins/Array/prototype/includes/return-abrupt-tonumber-length.js` |
| R9 | wasm traps (62), panics (5: `crates/jsc-core/src/emit/expr.rs:158`, `crates/jsc-core/src/error.rs:125`), exit 0 with an unexpected result and empty stderr (3), other (5) | 75 | — | `built-ins/Function/15.3.5.4_2-24gs.js`, `language/white-space/mongolian-vowel-separator.js` |

### Z. Other undeclared identifiers (275)

Sloppy-mode implicit globals, unresolvable references expected to throw
`ReferenceError` at run time (jsc rejects at compile time), test-local names
used before declaration across scopes (`x` 86, `y` 23,
`unresolvableReference` 30). Same site as A
(`crates/jsc-core/src/codegen/expr.rs:405`). Not object model, except the part
that needs a global object for `ReferenceError`-at-runtime semantics.

### Include-gated tests (skipped, not in the 22992)

Tests skipped only because an include has no jsc shim (first unshimmed include
per test, `crates/jsc-test262/src/filter.rs:103-105`): propertyHelper.js 3074,
decimalToHexString.js 82, detachArrayBuffer.js 62, nativeFunctionMatcher.js
57, nativeErrors.js 14, regExpUtils.js 12, promiseHelper.js 10, other 47 —
3358 in total. `propertyHelper.js` is the single largest lever outside the
eligible set.

## 3. Object-model attribution and estimates

Feature vocabulary used below (the design doc may rename them):

- **OM-dict**: runtime property add/delete on ordinary objects, computed and
  numeric keys (shape transitions or dictionary mode).
- **OM-desc**: property attributes (writable/enumerable/configurable),
  accessor properties on ordinary objects, `[[DefineOwnProperty]]`,
  extensibility; the `Object.*` reflection statics on top of it.
- **OM-proto**: `[[Prototype]]` as a mutable data slot, `Object.create`,
  `.constructor`, generic `OrdinaryHasInstance` for `instanceof`.
- **OM-fn**: functions are objects: own `name`/`length`/`prototype` and
  arbitrary properties; `[[Construct]]` for plain `function`s with
  `F.prototype`; class constructors as values with `C.prototype`.
- **OM-builtins**: builtin constructors / namespaces / prototypes as real
  objects (`Array.prototype.map`, `String`, `TypeError` as values), builtin
  methods found by prototype lookup.
- **OM-wrap**: primitive wrapper objects and ToObject.
- **OM-exotic**: array exotic length/growth, `arguments`, global object /
  global `this`.
- **OM-sub**: subclassing builtins (`extends Error/Set/Promise/Iterator`).

"Unblocked" = tests whose first blocker the feature removes. "Est. pass" =
unblocked × yield, where yield is a judgement of how many of those tests have
no further blocker (calibration point: of eligible tests that compile today,
5197 / 6278 = 83 % pass; built-ins tests chain more missing builtins, so their
yields are set lower).

| Cluster | Count | Object model fixes? | Features needed | Yield | Est. pass |
| --- | ---: | --- | --- | ---: | ---: |
| A1 builtins as values | 2595 | yes (plus the methods themselves) | OM-builtins, OM-fn | 35 % | ~900 |
| A2 error ctors as values | 508 | yes | OM-builtins, OM-proto (`thrown.constructor === C`) | 60 % | ~300 |
| A3 missing global fns | 500 | no (`eval` 352 is out of AOT scope; `isNaN`/URI fns are library) | — | — | 0 |
| A4 ctor called as fn | 155 | partial (`Object(x)`, `Array(n)`, `Error(m)`; not `Function`/`Symbol`) | OM-builtins, OM-wrap | 25 % | ~40 |
| B1 Object reflection | 1828 | yes | OM-desc, OM-proto | 55 % | ~1000 |
| B2 other Object statics | 46 | yes (library on top) | OM-dict, OM-desc | 65 % | ~30 |
| C1 wrappers / `new Object` | 796 | yes | OM-wrap, OM-builtins | 40 % | ~320 |
| C2 `new Array(..)` | 279 | yes | OM-exotic (array length), OM-builtins | 50 % | ~140 |
| C3 `new Function` | 66 | no (runtime compilation) | — | — | 0 |
| C4 other builtin `new` | 337 | partial (error family only; RegExp/ArrayBuffer/DataView/Proxy no) | OM-builtins | 12 % | ~40 |
| D1 plain-fn constructors | 267 | yes | OM-fn, OM-proto | 60 % | ~160 |
| D2 `new` non-identifier | 67 | partial | OM-fn | 45 % | ~30 |
| E1 class expressions | 1635 | partial: OM-fn gives classes a value representation; lowering class expressions is codegen work (many also hit G1/G2) | OM-fn + codegen | 40 % | ~650 (codegen-gated) |
| E2 extends builtins | 254 | yes for the extends mechanism; 145 are Iterator helpers which also need the Iterator library | OM-sub, OM-builtins | 30 % | ~75 |
| E3 literal / static accessors | 190 | yes | OM-desc | 50 % | ~95 |
| F1 computed/numeric keys | 763 | yes | OM-dict | 50 % | ~380 |
| G1 private members | 805 | no (private names are a separate per-class mechanism) | — | — | 0 |
| G2 generator/async forms | 1456 | no | — | — | 0 |
| G3 empty statement | 739 | no (codegen) | — | — | 0 |
| G4 regex | 445 | no | — | — | 0 |
| G9 other syntax/builtins | 2258 | partial (~150: `instanceof` RHS, `delete`, `toString`/`values` methods, computed class fields) | OM-proto, OM-dict, OM-builtins | 50 % | ~75 |
| H1 `arguments` | 290 | yes | OM-exotic | 60 % | ~175 |
| H2 global `this` | 160 | yes | OM-exotic (global object) | 50 % | ~80 |
| R1 add property | 110 | yes | OM-dict | 80 % | ~90 |
| R2 array growth | 34 | yes | OM-exotic (array length) | 85 % | ~30 |
| R3 missing methods | 283 | partial: OM-builtins makes them ordinary prototype functions; each still has to be written | OM-builtins | 30 % | ~85 |
| R4 early errors | 166 | no | — | — | 0 |
| R5 ToPrimitive | 63 | partial (method lookup via prototype) | OM-proto | 30 % | ~20 |
| R6 undefined reads | 40 | partial | OM-builtins | 25 % | ~10 |
| R7 assertion mismatches | 310 | partial: the ~54 `fn.name` cases fully, others mixed | OM-fn | — | ~80 |
| R9 crashes | 75 | no | — | — | 0 |
| Z1 other undeclared | 275 | no (mostly scope/sloppy) | — | — | 0 |

Totals: object-model-attributable estimated gain **~4,155** tests outside E1,
**~4,800** including E1 once class expressions are lowered — from 5197 to
roughly 9.4k–10k of 22992 (41–44 %). Roughly 6,600 failures (A3, C3, G1–G4,
most of G9, R4, R9, Z1) are not object-model work.

Per-stage grouping for the migration plan (each stage assumes the previous
ones are in):

| Stage | Features | Clusters | Est. gain |
| --- | --- | --- | ---: |
| S1 harness (M1) | OM-fn + minimal OM-builtins (`String`, `Array.prototype.map`, `Object.prototype.toString` as values) | D1, D2, R7 (fn.name), A2 via the real `assert.throws` | ~570 |
| S2 dictionary objects | OM-dict | R1, F1, B2, part of G9 | ~540 |
| S3 descriptors + prototypes | OM-desc, OM-proto | B1, E3, R5, part of G9 | ~1,150 |
| S4 builtins as objects + wrappers | OM-builtins, OM-wrap | A1, A4, C1, C2, C4, R3, R6 | ~1,535 |
| S5 exotics + subclassing | OM-exotic, OM-sub | H1, H2, R2, E2 | ~360 |
| (codegen) class expressions | OM-fn + codegen | E1 | ~650 |

In addition, S3+S4 make `propertyHelper.js` compilable (it needs
`Object.getOwnPropertyDescriptor`/`defineProperty`/`getOwnPropertyNames`,
`Array.isArray`, `Function.prototype.call.bind`, `delete obj[name]` —
`.tmp/test262/harness/propertyHelper.js:27-34`, `:140`), which moves 3074
skipped tests into the eligible set. At ~35 % yield that is ~1,000 more
passes, with the eligible denominator growing to ~26,000.

## 4. M1: the official harness, construct by construct

### 4.1 How the runner assembles tests today

- The runner reads jsc's **own** shim harness from `tests/test262/harness`
  (`crates/jsc-test262/src/main.rs:19`, `crates/jsc-test262/src/main.rs:120-134`),
  not the official `.tmp/test262/harness`. Only `sta.js`, `assert.js` and
  `compareArray.js` exist there; every other file in that directory counts as
  "shimmed" (`crates/jsc-test262/src/main.rs:142`), and any other include
  skips the test (`crates/jsc-test262/src/filter.rs:103-105`).
- Strict-mode programs are `"use strict";` + `sta.js` + `assert.js` + the
  listed includes + the rewritten test source
  (`crates/jsc-test262/src/assemble.rs:28-41`); `raw` tests run verbatim
  (`crates/jsc-test262/src/assemble.rs:30`).
- Two lexical rewrites run on the test source only
  (`crates/jsc-test262/src/assemble.rs:7-23`):
  `assert.throws(<Name>,` → `assert_throws_<Name>(` for the four names in
  `THROWS_SHIMMED` (`crates/jsc-test262/src/assemble.rs:48`,
  `crates/jsc-test262/src/assemble.rs:57-88`), then every `assert.<ident>(` →
  `assert_<ident>(` (`crates/jsc-test262/src/assemble.rs:91-117`).
- The shim `sta.js` makes `Test262Error` a class and drops `thrower`
  (`tests/test262/harness/sta.js:2-14`). The shim `assert.js` flattens
  `assert.*` into `assert_*` functions and throws
  `new Error("Test262Error: " + message)` instead of `new Test262Error`
  (`tests/test262/harness/assert.js:1-18`, e.g. `:48`, `:70`). The generic
  `assert_throws` compares `thrown.constructor` and fails honestly
  (`tests/test262/harness/assert.js:117-125`); the shimmed variants use
  `instanceof` plus `.name` (`tests/test262/harness/assert.js:134-172`).
- Consequence: assertion failures report `Uncaught Error: Test262Error: …`,
  and runtime-negative tests expecting `Test262Error` cannot match the
  uncaught name (`crates/jsc-test262/src/outcome.rs:9-17`,
  `crates/jsc-test262/src/outcome.rs:79-85`).

Include usage across the 22992 eligible tests (strict mode 22965, raw 27):
22551 have no includes, 413 include `compareArray.js`. Harness surface used by
test bodies: `assert.sameValue` in 12916 files, `assert.throws` 4495,
`new Test262Error`/`throw Test262Error` in the body 4048, `$DONOTEVALUATE`
3547, `assert.compareArray` 415, `Test262Error.thrower` 33. Outside the
eligible set, `doneprintHandle.js` matters for the 5484 `flag:async` tests
(`$DONE`) and `propertyHelper.js` for 3074 skipped tests.

### 4.2 Probes

Scratch files under `.tmp/m1/` (not committed), run with
`./target/release/jsc FILE`; construct probes are prefixed with
`"use strict";` like runner programs.

The **official** `sta.js` alone (`.tmp/test262/harness/sta.js`, copied
verbatim) fails to compile:

```
error: unsupported constructor: Test262Error
   --> .tmp/m1/sta-only.js:23:13
 23 |   throw new Test262Error(message);
```

`sta.js` + `assert.js` (+ `assert.sameValue(1, 1)`) fails with the same
diagnostic. jsc reports only the first error, so the official `assert.js` was
probed incrementally on top of a class-based `Test262Error`, removing each
blocker in turn:

1. `error: undeclared variable: String` at `assert.js:179`
   (`Array.prototype.map.call(arrayLike, String)`); `Array` in the same
   expression is equally undeclared (standalone probe:
   `error: undeclared variable: Array`).
2. `error: undeclared variable: JSON` at `assert.js:27`
   (`typeof JSON !== "undefined"` — `typeof` on a builtin namespace is
   rejected too).
3. `error: undeclared variable: Object` at `assert.js:47`
   (`Object.prototype.toString.call(value)`).
4. Then it compiles, and fails at run time on the first property write to a
   function: `Uncaught TypeError: Cannot add property '_isSameValue': dynamic
   properties are not supported yet` (`assert.js:64`).

### 4.3 `sta.js`

| Construct | Harness line | Compiles today? (exact result) | Needed feature |
| --- | --- | --- | --- |
| `function Test262Error(message) { … this.message = … }` | `.tmp/test262/harness/sta.js:13-16` | the declaration alone compiles and runs (`this` assignment inside a plain function is accepted) | — |
| `this instanceof Test262Error` (plain function on the RHS) | `.tmp/test262/harness/sta.js:14` | blocked by the `new` below; `instanceof` RHS is limited to classes and a builtin list (`crates/jsc-core/src/codegen/expr.rs:1607-1646`) | OM-proto (`OrdinaryHasInstance` over `F.prototype`) |
| `new Test262Error(message)` | `.tmp/test262/harness/sta.js:14`, `:23` | no: `error: unsupported constructor: Test262Error` (`crates/jsc-core/src/codegen/expr.rs:1693-1698`) | OM-fn (`[[Construct]]` for plain functions) |
| `Test262Error.prototype.toString = function () {…}` | `.tmp/test262/harness/sta.js:18-20` | plain function: runs, `Uncaught TypeError: Cannot add property 'toString': dynamic properties are not supported yet`; with `Test262Error` as a class: `Uncaught TypeError: Cannot set properties of undefined (setting 'toString')` (a class's `.prototype` reads as `undefined`) | OM-fn (`F.prototype` object), OM-dict |
| `Test262Error.thrower = function (message) {…}` | `.tmp/test262/harness/sta.js:22-24` | runs, `Uncaught TypeError: Cannot add property 'thrower': dynamic properties are not supported yet` | OM-fn (properties on functions) |
| `function $DONOTEVALUATE() { throw "…"; }` | `.tmp/test262/harness/sta.js:26-28` | yes; uncaught string prints `Uncaught Test262: This statement should not be evaluated.` | — |

### 4.4 `assert.js`

| Construct | Harness line | Compiles today? | Needed feature |
| --- | --- | --- | --- |
| `isNegativeZero`, `isPrimitive` (`typeof` checks) | `.tmp/test262/harness/assert.js:16-22` | yes (identical code is in the shim, `tests/test262/harness/assert.js:20-26`) | — |
| `switch (value === null ? 'null' : typeof value)` with fall-through cases | `.tmp/test262/harness/assert.js:25-37` | yes (probe prints `1 s null`) | — |
| `typeof JSON !== "undefined" ? JSON.stringify(value) : …` | `.tmp/test262/harness/assert.js:27` | no: `error: undeclared variable: JSON` | OM-builtins (`JSON` as a value; `typeof` of a builtin) |
| `String(value) + "n"` (bigint branch), `String(value)` calls | `.tmp/test262/harness/assert.js:29`, `:36`, `:44` | yes (`String` call is a conversion builtin, `crates/jsc-core/src/codegen/expr.rs:725-730`) | — |
| `try { … } catch (err) { if (err.name === 'TypeError') … throw err; }` | `.tmp/test262/harness/assert.js:43-50` | yes (probe: `err.name === "TypeError"` → `true`) | — |
| `Object.prototype.toString.call(value)` | `.tmp/test262/harness/assert.js:47` | no: `error: undeclared variable: Object` | OM-builtins (`Object.prototype` object, `Function.prototype.call`) |
| `function assert(mustBeTrue, message)` called with 1 or 2 args | `.tmp/test262/harness/assert.js:53-62` | yes (probe: missing args read `undefined`) | — |
| `assert._toString(…)` — call through a property of a function | `.tmp/test262/harness/assert.js:59`, `:90`, `:106` | runs only after the property exists; today reading a function property yields a garbage value (`typeof a.foo` → `"string"`) | OM-fn |
| `throw new Test262Error(message)` | `.tmp/test262/harness/assert.js:61`, `:80`, `:92`, … | no: `error: unsupported constructor: Test262Error` | OM-fn |
| `assert._isSameValue = function (a, b) {…}` and every `assert.X = function` | `.tmp/test262/harness/assert.js:64`, `:74`, `:95`, `:111`, `:147` | runs, `Uncaught TypeError: Cannot add property '_isSameValue': dynamic properties are not supported yet` | OM-fn (arbitrary own properties on functions) |
| unreachable `return;` after `throw` inside `catch` | `.tmp/test262/harness/assert.js:80-81`, `:114-116` | yes (probe) | — |
| `message += ' '` on a parameter | `.tmp/test262/harness/assert.js:84-88` | yes (same as the shim, `tests/test262/harness/assert.js:65-69`) | — |
| `typeof func !== "function"` | `.tmp/test262/harness/assert.js:113` | yes | — |
| `thrown.constructor !== expectedErrorConstructor` | `.tmp/test262/harness/assert.js:130` | compiles, but `.constructor` is not the class (probe: `e.constructor === E` → `false`) and builtin errors cannot be passed as values (`error: undeclared variable: TypeError`) | OM-proto (`constructor` on prototypes), OM-builtins |
| `expectedErrorConstructor.name`, `thrown.constructor.name` | `.tmp/test262/harness/assert.js:131-132`, `:143` | compiles; a class's `.name` reads `undefined`, a function's `.name` reads a garbage value | OM-fn (`name` own property) |
| `typeof message === 'symbol'` / `message.toString()` | `.tmp/test262/harness/assert.js:150-152` | yes (probe) | — |
| `compareArray.format = function (arrayLike) {…}` | `.tmp/test262/harness/assert.js:178-180` | runs into the same `Cannot add property` error | OM-fn |
| `Array.prototype.map.call(arrayLike, String)` | `.tmp/test262/harness/assert.js:179` | no: `error: undeclared variable: String` (then `Array`) | OM-builtins (`Array.prototype`, `String` as a function value) |
| `var format = compareArray.format; format(actual)` | `.tmp/test262/harness/assert.js:162-163` | depends on function properties | OM-fn |
| `assert._formatIdentityFreeValue = formatIdentityFreeValue`, `assert._toString = formatSimpleValue` (function declarations as values) | `.tmp/test262/harness/assert.js:182-184` | the value side compiles (a function name in value position becomes a function value, `crates/jsc-core/src/codegen/expr.rs:393-397`); the store hits `Cannot add property` | OM-fn |

### 4.5 `doneprintHandle.js`, `compareArray.js`, `propertyHelper.js`

- `compareArray.js` is empty upstream (`.tmp/test262/harness/compareArray.js:1-7`);
  `compareArray` lives in `assert.js`.
- `doneprintHandle.js`: `print(msg)` →
  `error: call to undeclared function: print`
  (`.tmp/test262/harness/doneprintHandle.js:9`); `'name' in error` and
  `error.message` compile (probe prints `true m`). Needs a host `print`
  (not object model) plus the async runner path; it gates the 5484
  `flag:async` tests, not object-model work.
- `propertyHelper.js` (after the shim harness):
  `error: undeclared variable: Array` at `var __isArray = Array.isArray;`
  (`.tmp/test262/harness/propertyHelper.js:27`). It also needs
  `Object.defineProperty`/`getOwnPropertyDescriptor`/`getOwnPropertyNames` as
  values, `Function.prototype.call.bind(...)` and
  `Object.prototype.hasOwnProperty`/`propertyIsEnumerable`
  (`.tmp/test262/harness/propertyHelper.js:28-34`), and `delete obj[name]`
  (`.tmp/test262/harness/propertyHelper.js:140`, `:204`): OM-desc,
  OM-builtins, OM-dict.

### 4.6 What M1 needs and what changes when the real harness compiles

Minimum feature set for the official `sta.js` + `assert.js`: OM-fn (plain
function `[[Construct]]`, `F.prototype`, arbitrary own properties, `name`),
OM-proto (`instanceof` over a plain function, `.constructor`), and three
builtin values: `String` (as a function value), `Array.prototype.map`
(+ `.call`), `Object.prototype.toString` (+ `.call`), with `typeof JSON`
resolving. These are all inside S1 and the start of S4.

Runner changes once the official harness compiles:

1. Point `HARNESS_DIR` at the official harness and read includes from there
   (`crates/jsc-test262/src/main.rs:19`, `:120-134`); delete
   `tests/test262/harness/` (no compatibility shim is kept).
2. Delete both rewrites, `THROWS_SHIMMED` and their tests
   (`crates/jsc-test262/src/assemble.rs:7-23`, `:43-117`, `:119-203`);
   `program` becomes plain concatenation.
3. The `shimmed` set (`crates/jsc-test262/src/main.rs:142`) becomes "include
   files that compile"; `include:` skips
   (`crates/jsc-test262/src/filter.rs:103-105`) shrink as includes compile,
   and the eligible count grows (propertyHelper.js alone +3074).
4. Failures throw a real `Test262Error`; once uncaught class/function
   instances print their constructor name, runtime-negative tests expecting
   `Test262Error` can match (`crates/jsc-test262/src/outcome.rs:9-17`), and
   the 32 "expected Test262Error but got TypeError" mismatches in R7 are
   judged by the real `assert.throws`.
5. `assert.throws(C, f)` works for any constructor, which removes the A2
   cluster's blocker (508 tests) instead of the four-name shim.
6. The baseline must be regenerated (`--update-baseline`): moving from shims to
   the real harness can flip tests in both directions, for example tests that
   pass today only because the shim `assert_throws_TypeError` checks
   `instanceof` + `.name` rather than constructor identity.
