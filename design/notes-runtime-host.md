# Notes: runtime host inventory, WASI target, deletions (R-T1)

Intermediate notes for `design/runtime-memory.md` sections 1 (inventory),
2 (WASI target) and 4 (deletions + runners). R-T3 folds this file into the
design doc and deletes it. Every claim is grounded in the tree at `c34cd36`
(`bkd/9n288v23`); paths are repository-relative.

---

## 1. Inventory of the host boundary

### 1.1 The import section today

Every module imports exactly seven functions from module `"env"`
(`abi::IMPORT_MODULE`, `crates/jsc-core/src/abi.rs:684`), declared in
`crates/jsc-core/src/emit/mod.rs:1488-1528`, fixed at function indices `0..=6`
(`IMPORT_FUNC_COUNT = 7`, `emit/mod.rs:147`; index assignment
`emit/sections.rs:63-72`; import types `emit/sections.rs:180-207`). No memory,
table or global is imported. `DESIGN.md` §6.1 lists the same seven.

| # | module / name | wasm signature | Rust implementer | TS implementer | semantics |
|---|---|---|---|---|---|
| 0 | `env.print` | `(bits: i64, flag: i32) -> ()` | closure in `run/harness.rs:153-179` | `print`, `packages/jsc/src/shim.ts:674` | `console.log` piece: `flag` `0` = value + space, `1` = value + newline, `2` = newline only (`abi.rs` `PRINT_FLAG_*`); value rendered by `format_value(decode(..))` |
| 1 | `env.__convert` | `(bits: i64, op: i32) -> i64` | closure in `run/harness.rs:189-227` | `convert`, `shim.ts:691` | op `0` ToString of a primitive (string returned unchanged, else a new heap string via `__new_string`), op `1` ToNumber (StringToNumber rules), op `3` parseFloat of the string form |
| 2 | `env.__method` | `(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64` | `harness.rs:235-249` -> `run/host.rs::method_dispatch` (`host.rs:101`) | `method` / `methodDispatch`, `shim.ts:709`, `shim.ts:291` | ops `0` array `indexOf`, `1` array `includes`, `4`/`5` upper/lower case, `17` `Number#toString(radix)`, `22` `localeCompare`, `23` `normalize`, `40`/`41`/`42` `toFixed`/`toExponential`/`toPrecision`, `43` Math (`arg1` = `MATH_FN_*` code incl. `random`) |
| 3 | `env.__promise` | `(op: i32, a: i64, b: i64) -> i64` | `harness.rs:253-261` -> `run/promise.rs::dispatch` (`promise.rs:179`) | `promiseDispatch`, `shim.ts:623` | ops `0` resolve, `1` reject, `2` top-level await, `3` finally, `4` new pending, `5` settle-fulfill, `6` settle-reject, `7` async spawn (`abi.rs` `PROMISE_OP_*`) |
| 4 | `env.__then` | `(p: i64, on_f: i64, on_r: i64) -> i64` | `harness.rs:262-275` -> `promise.rs::then` (`promise.rs:222`) | `promiseThen`, `shim.ts:667` | allocate the chained promise, register / queue the reaction |
| 5 | `env.__now` | `() -> f64` | closure in `harness.rs:280-284` (`SystemTime::now()`, `0` before the epoch) | `__now: () => Date.now()`, `shim.ts:731` | wall clock in ms (fractional on the Rust host; the guest TimeClips it, `emit/object.rs:1072-1079`) |
| 6 | `env.__parse_int` | `(s: i64, radix: i64) -> i64` | closure in `harness.rs:287-299` -> `host.rs::parse_int_str` | `__parse_int`, `shim.ts:733` | `parseInt(ToString(s), ToNumber(radix))` |

Every non-void import result passes through `oom_guard` (`harness.rs:52-62`,
shim `oomGuard` `shim.ts:189`), which traps when `__heap_ptr` holds the
`HEAP_PTR_OOM` sentinel.

### 1.2 Guest call sites per import

Found by grepping `emit/**` for `helpers.<import>` / the op constants.

| import | call sites (function) |
|---|---|
| `print` (4) | `emit/expr.rs:3063`, `:3074` (`emit_console_log`, `expr.rs:3051`); `expr.rs:3101`, `:3130` (`emit_console_log_spread`, `expr.rs:3084`). The `console.log` function value (`Builtin::ConsoleLog`, `realm.rs:452`) lowers to the same nodes (`builtins/namespace.rs:42-47`). |
| `__convert` (12) | ToString: `convert.rs:253` (`emit_to_string`, the shared `__to_string`), `object.rs:2189` (`emit_index_string_key`, canonical numeric key round trip), `object.rs:2933` (`emit_nullish_throw` message), `object.rs:3021` (`emit_prop_throw` message), `props.rs:2932` (`emit_private_in` message), `runtime.rs:1209` (`emit_json_str` number), `mod.rs:1690` (passes the index to `emit_json_str`). ToNumber: `value.rs:104` (`emit_to_number_helper` = `__to_number`), `object.rs:2186` (`emit_index_string_key`), `runtime.rs:1353`, `:1365` (`emit_loose_eq`), `json_parse.rs:523` (`emit_json_number`). parseFloat: `numeric.rs:1032` (`emit_convert_call`). |
| `__method` (7) | `builtins/array.rs:591` (`emit_search`: indexOf/includes fast path on an index-less array), `numeric.rs:1116` (`emit_method_call` inline `indexOf`/`includes`), `expr.rs:1415` (`emit_proto_guard` fast path), `builtins/string.rs:626` (`emit_host_op`: case map / `localeCompare` / `normalize`, ops chosen in `realm.rs:1658-1661`), `builtins/number.rs:222` (`emit_number_to_string`, radix != 10), `builtins/number.rs:336` (`emit_number_format`), `numeric.rs:358` (`emit_host_math`, codes `numeric.rs:322-349`, `random` via `MATH_FN_RANDOM`) |
| `__promise` (14) | `expr.rs:2367` (`emit_promise_call`, used by `expr.rs:667` reject, `:683` finally, `:696` top-level `Await`, `:728` `AsyncSpawn`), `expr.rs:2427` (`emit_promise_resolve`), `expr.rs:2460`, `:2548` (`emit_promise_ctor`: new + settle-reject on executor throw), `runtime.rs:499` (`emit_settler`, the two `new Promise` settler bodies), `builtins/promise.rs:606` (`emit_resolve`), `builtins/generator.rs:391` (`settle_front`), `:505` (`emit_async_method`), `builtins/array.rs:1539`, `:1565`, `:1606`, `:1624`, `:1652`, `:1770` (`Array.fromAsync`) |
| `__then` (7) | `expr.rs:716` (`PromiseThen` node), `builtins/promise.rs:413` (`emit_then`), `:899` (`combinator_value`), `builtins/generator.rs:441` (`await_value`, async generators), `builtins/async_iter.rs:734` (`%AsyncFromSyncIteratorPrototype%`), `builtins/array.rs:1631`, `:1668` (`Array.fromAsync`) |
| `__now` (2) | `object.rs:997` (`emit_date_new_node`, no argument), `object.rs:1074` (`emit_date_now_node`) |
| `__parse_int` (1) | `numeric.rs:1019` (`emit_convert_call`, `ConvertOp::ParseInt`; the global `parseInt` value reaches it via `builtins/global.rs:103`) |

### 1.3 Inventory by area

#### A. Output / console and value rendering

- **Host work.** `print` decodes the jsval against the live memory
  (`run/value.rs::decode`, `value.rs:162`) and renders it with
  `value.rs::format_value` (`value.rs:789`): Node-console style for every kind
  (`Value` enum `value.rs:16-138`: numbers, strings unquoted, symbols, arrays
  with hole runs and named props, objects in own-key order, class instances via a
  `constructor` data-chain lookup without running getters, wrappers, `arguments`,
  `CLASS_ERROR` objects as `name: message`, accessors, functions/classes with
  names from the function info table, `Map(N)`/`Set(N)`, promises by state,
  generators, Dates in ISO form `value.rs:1032`, cycles `<ref *N>` /
  `[Circular *N]`). Native output writes a lone surrogate as U+FFFD
  (`decode_string` is lossy, DESIGN §6.8).
- **Only `console.log` exists.** `codegen/op.rs:49` makes only `log` inline;
  `console.warn(1)` is an ordinary member call on the namespace object
  (`codegen/tests.rs:717`) and the realm's `console` object has only `log`
  (`realm.rs:2170`, DESIGN §7.9 table `console | log`). There is no stderr
  writer in the guest; stderr text today is produced only by the CLI
  (`crates/jsc/src/main.rs:79-86`).
- **Guest code that already exists.** Date text (`object.rs:1591-1660`,
  `emit_date_string`), `toISOString`, `Function.prototype.toString`
  (`builtins/function.rs:579`), `Error.prototype.toString`, string
  concatenation / `__concat`, JSON quoting (`emit_json_escape`). Nothing walks a
  value Node-console style; the whole of `format_value` is host-only.
- **Shim.** `value.ts` (`formatValue` / `decode`) is a port of `value.rs`;
  `print` in `shim.ts:674` appends to an in-memory string.

#### B. Number -> string

| operation | today | guest code present |
|---|---|---|
| `ToString(Number)` (every `'' + n`, template, `join`, `String(n)`, JSON number, numeric key canonicalization) | `__convert` op 0 -> `value.rs::format_number` (`value.rs:714`, shortest digits via Rust `{:e}`, `value.rs:744-786`) | none: no dtoa in `emit/**` (grep for `ryu`/`grisu`/`shortest`/`dtoa` finds nothing) |
| `Number#toString(radix != 10)` | `__method` 17 -> `host.rs::number_to_radix_string` (`host.rs:338`, V8 `DoubleToRadixCString` port) | radix validation + radix-10 path in `builtins/number.rs:174-230` |
| `toFixed` / `toExponential` / `toPrecision` | `__method` 40/41/42 -> `host.rs::number_to_fixed` / `number_to_exponential` / `number_to_precision` (`host.rs:523-608`, exact decimal via `format!("{:.1100}")`, `host.rs:469`) | `thisNumberValue`, `ToIntegerOrInfinity`, range `RangeError`s, non-finite text (`builtins/number.rs:263-336`) |
| ToString of `true`/`false`/`null`/`undefined` | `__convert` op 0 | interned literals exist in the static region; trivial |
| ToString of a symbol inside internal message builders | `__convert` op 0 renders `Symbol(desc)` (`value.rs::to_js_string`) — reached from `emit_prop_throw`, `emit_nullish_throw`, `emit_private_in`, which do not go through `__to_string` | `emit_symbol_descriptive_string` (`numeric.rs`, used by `String(sym)`) |

Stale claims to fix while moving: `run/harness.rs:180-183`, `shim.ts:686-690`
and DESIGN §6.1 say `__convert` op 0 sees a Date or a function. It cannot:
`emit_to_string` (`convert.rs:225-260`) runs `__to_primitive` on every object
before the host call, and Dates/functions have guest `toString`s. The host only
ever sees primitives (number, boolean, null, undefined, plus symbols from the
three message builders above).

#### C. String -> number

| operation | today | guest code present |
|---|---|---|
| ToNumber of a string (`__to_number`, loose equality, numeric key test, JSON numbers) | `__convert` op 1 -> `host.rs::value_to_number` -> `str_to_number` (`host.rs:619`, `:643`): JS whitespace trim, `0x`/`0o`/`0b`, `Infinity`, decimal via Rust `str::parse::<f64>` (correctly rounded) | the JSON scanner validates JSON number syntax in the guest before calling the host (`json_parse.rs:429-525`) |
| `parseFloat` | `__convert` op 3 -> `host.rs::parse_float_str` (`host.rs:745`) | `ToString` of the argument (`numeric.rs:1023-1033`) |
| `parseInt` | `__parse_int` -> `host.rs::parse_int_str` (`host.rs:687`): radix 10 correctly rounded via `parse::<f64>`, other radices digit accumulation | `ToString(s)` in the guest (`numeric.rs:1006-1019`) |

The host decodes the operand with `decode` (lossy text; a lone surrogate
becomes U+FFFD, harmless for parsers per DESIGN §6.8).

#### D. String case mapping, `localeCompare`, `normalize`

- `toUpperCase` / `toLowerCase` (+ `toLocale*`): the guest maps one-byte
  receivers itself and calls `__method` 4/5 only for two-byte receivers and the
  one-byte `µ`/`ß`/`ÿ` uppercase cases (`builtins/string.rs:537-610`
  `emit_case_map`). Host: `host.rs::map_case` (`host.rs:203`), Rust std
  `str::to_uppercase`/`to_lowercase` per well-formed run (SpecialCasing,
  `Final_Sigma`), lone surrogates pass through. Shim: JS
  `toUpperCase`/`toLowerCase` (`shim.ts:319`).
- `localeCompare`: `__method` 22, code-unit order (`host.rs:166-175`). The guest
  already has `__str_cmp` (code-unit order, `emit/runtime.rs`), so this op is
  redundant today.
- `normalize`: `__method` 23 -> `host.rs::normalize_units` (`host.rs:231`) with
  `icu_normalizer` (`ComposingNormalizerBorrowed` / `DecomposingNormalizerBorrowed`,
  optional dependency enabled by `native-run`, `crates/jsc-core/Cargo.toml`);
  form validation also host-side, refusal sentinel `METHOD_RANGE_ERROR`
  (`abi.rs`) thrown as `RangeError` by `emit_host_op` (`string.rs:616-640`).
  Shim: JS `String.prototype.normalize` (`shim.ts:343`).
- Host reads strings losslessly via `value.rs::string_units` (`value.rs:318`)
  and allocates results through `host.rs::alloc_units` (`host.rs:61`) ->
  guest `__new_string`.

#### E. Array search (`indexOf` / `includes`)

`__method` 0/1 -> `host.rs::array_index_of` / `array_includes`
(`host.rs:303`, `:313`), strict equality / SameValueZero over the element
store. Only used as a fast path for an index-less, proto-intact array
(`builtins/array.rs:562-592`, `numeric.rs:1090-1118`, `expr.rs:1337-1415`); the
same builtin already has the generic guest loop as its slow path
(the rest of `emit_search`). No new guest code is needed beyond routing the fast path
to a guest scan (or dropping it).

#### F. Math

- Transcendentals (`cbrt`, `log`, `log2`, `log10`, `log1p`, `exp`, `expm1`,
  `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`,
  `asinh`, `acosh`, `atanh`; 20 codes `MATH_FN_CBRT..ATANH`, `abi.rs`) -> `__method`
  43 -> `host.rs::math_fn` (`host.rs:413`, platform libm via std). Shim uses
  JS `Math.*` (`shim.ts:242-286`), so Node/browser and native results can
  differ in the last ulp today.
- Guest already: `abs`, `floor`, `ceil`, `trunc`, `round`, `sqrt`, `sign`,
  `min`/`max`, `hypot`, `pow` (`emit_pow`), `imul`, `clz32`, `fround`,
  `sumPrecise` — everything with an exact wasm instruction or a guest algorithm
  (`numeric.rs::emit_math_call`; `host_math_code`, `numeric.rs:322-349`,
  returns `None` for them).
- `Math.random` -> `MATH_FN_RANDOM` -> `host.rs::random` (`host.rs:445`),
  `xorshift64*` seeded from std `RandomState`. Shim: `Math.random()`
  (`shim.ts:285`).

#### G. Promises, microtasks, async drivers

- **Split today.** Settlement (state @16, value @24) lives in the guest
  `TYPE_PROMISE` block; scheduling lives in the host `PromiseEngine`
  (`promise.rs:88-98`): FIFO `queue: VecDeque<Job>`, `reactions:
  HashMap<u32, Vec<Reaction>>` keyed by block base, `rejected: BTreeSet<u32>`
  (ascending base = creation order because blocks are bump-allocated),
  `handled: HashSet<u32>`. Mirrored in `shim.ts:397-672`.
- **Host algorithms to move:** `resolve` (`promise.rs:274`), `finally`
  (`:289`), `await_promise` (`:323`, drains to empty then reads the state;
  rejection sets `__exc_pending`/`__exc_value`), `spawn_async` / `drive_async` /
  `async_step` (`:360`, `:381`, `:417`), `drain` / `run_job` (`:473`, `:484`),
  `settle` (`:530`, settle-once, self-resolution `TypeError` via exported
  `__promise_self_error`, adoption of another promise via `register_source`),
  `register_source` (`:598`, non-promise source = already fulfilled; no thenable
  assimilation, DESIGN §13), `invoke1` (`:658`), `drain_after_main` (`:257`,
  first unhandled rejection by creation order).
- **Guest code that already exists** (scheduling-agnostic): `new Promise`
  settler functions (`runtime.rs:488`), `then` with species /
  `NewPromiseCapability` (`builtins/promise.rs:349`), combinators
  (`builtins/promise.rs:823+`), `Promise.resolve`/`reject` with `this`
  (`builtins/promise.rs:580`), async generator request queue and driver
  (`builtins/generator.rs:378-505`), `%AsyncFromSyncIteratorPrototype%`
  (`builtins/async_iter.rs`), `Array.fromAsync` (`builtins/array.rs:1530-1770`),
  the `__invoke1` and `__async_step` trampolines (`emit/runtime.rs`, exported
  only for the hosts). What is missing in the guest is exactly the
  `PromiseEngine`: queue, reaction lists, settle/adopt, unhandled tracking, the
  async-function driver and the drain loop.

#### H. Date clock

`__now` only (`object.rs:997`, `:1074`). All Date arithmetic, parsing
(`builtins/date_parse.rs`), TimeClip (`object.rs` `emit_time_clip`) and string
forms are guest code already.

#### I. Errors, exit, out of memory

- Uncaught exception: after `main`, the harness reads the exported
  `__exc_pending` / `__exc_value` globals (`harness.rs:343-355`) and returns
  `Err("Uncaught <format_value>")`; the CLI prints it to stderr and exits 1
  (`main.rs:83-86`).
- Unhandled rejection: `Err("Uncaught (in promise) <value>")` after the drain
  (`harness.rs:356-361`), exit 1.
- The drain is skipped when `main` ended with a pending exception
  (`harness.rs:315-331`).
- Out of memory: `__alloc` stores `HEAP_PTR_OOM` and executes `unreachable`
  (DESIGN §5.6); the harness reports `Uncaught RangeError: out of memory`
  regardless of the trap (`harness.rs:47`, `:305-313`, `:325-327`). Not
  catchable.
- Any other trap (e.g. call-stack exhaustion): the wasmtime error propagates;
  CLI exit 1 with the trap text, no `Uncaught` prefix.
- Compile errors: CLI exit 2 (`main.rs:79-82`), unaffected by this phase.

#### J. test262 harness hooks

- No `$262` object exists anywhere (`crates/jsc-test262/src/**` has no
  reference; `IsHTMLDDA` and `host-gc-required` are in
  `tests/test262/unsupported-features.txt:51`, `:62`).
- `print` is a runner prelude in JS, `function print(message) {
  console.log(message); }` (`crates/jsc-test262/src/assemble.rs:14`), used by
  `doneprintHandle.js`; async outcome read from stdout lines
  `Test262:AsyncTestComplete` / `Test262:AsyncTestFailure`
  (`outcome.rs` `classify_async`).
- Runtime-negative outcome: exit 1 + stderr containing `Uncaught <Name>`
  (`outcome.rs` `verdict` / `uncaught_name`); compile-error: exit 2 or stderr
  starting `error:`.

#### K. Anything else

- `__alloc` and `__new_array` are exported (`emit/mod.rs:1591-1598`) but no
  host uses them (grep over `run/`, `packages/jsc/src`, `tests/`: only `abi.ts`
  mentions `NEW_ARRAY_EXPORT_NAME`; `ALLOC_EXPORT_NAME` has no user). Dead
  exports today.
- The completion value: `main` returns the last expression statement's jsval
  (`DESIGN` §4.2, `harness.rs:301`); the CLI's `-p` decodes and renders it
  (`main.rs:111-114`), and almost all Rust tests assert on it (see §3.6).

### 1.4 Host -> guest calls (exports the host invokes)

| export | signature | invoked by | re-entrant from inside an import? |
|---|---|---|---|
| `main` | `() -> i64` | `harness.rs:301`, `index.ts:87` | no (entry) |
| `memory` | memory | every decode/alloc | — |
| `__heap_ptr` | global i32 | `oom_guard`, `exhausted` (`harness.rs:52`, `:305`) | — |
| `__exc_pending` / `__exc_value` | globals | uncaught check (`harness.rs:319-355`); promise engine reads/clears them around every guest callback (`promise.rs::invoke1`) and sets them for an awaited rejection (`promise.rs:342-347`) | yes |
| `__new_string(len, two_byte) -> i32` | func | `host.rs::alloc_units` from `__convert` / `__method` | yes, nested inside an import |
| `__new_promise() -> i32` | func | `promise.rs::alloc_promise` (`:703`) | yes |
| `__promise_self_error() -> i64` | func | `promise.rs::settle` (`:530`) | yes |
| `__invoke1(fn, arg) -> i64` | func | `promise.rs::invoke1` (`:658`) | yes: guest -> `__promise`(await) -> host drain -> `__invoke1` -> guest -> `__then`/`__promise` -> host ... arbitrarily deep |
| `__async_step(gen, sent) -> i64` | func | `promise.rs::async_step` (`:417`) | yes, same chain |
| `__alloc`, `__new_array` | func | nobody | — |

After the move none of these are host-invoked; `__invoke1` / `__async_step`
become internal helpers and every export except `_start` and `memory` is
deleted (§2.4).

---

## 2. WASI target

### 2.1 Recommendation: WASI preview 1

Recommend `wasi_snapshot_preview1`, core module, command shape.

- **wasmtime.** `wasmtime-wasi` ships a preview-1 linker
  (`wasmtime_wasi::p1::add_to_linker_sync` + `WasiP1Ctx`, built by
  `WasiCtxBuilder::build_p1`; exact module paths to be checked against 50.0.1
  at implementation) and in-memory pipes for capture; fuel metering is an
  engine `Config` and is unaffected. The workspace already pins wasmtime 50.0.1
  (`crates/jsc-core/Cargo.toml`); `wasmtime-wasi` must be the same version.
- **Node.** `node:wasi` implements preview 1 only (`version: 'preview1'`).
- **Browsers.** Preview 1 needs a JS object with four functions (§2.2); a
  hand-written polyfill is on the order of 100 lines. Preview 2 needs the
  component model: jco transpilation to core modules plus generated JS glue and
  the `@bytecodealliance/preview2-shim` runtime per run — a build-time tool and a
  far larger runtime for four calls.
- **Component-model cost.** Preview 2 output means emitting a component
  (canonical ABI lifting/lowering of `wasi:cli/stdout`, `wasi:clocks`,
  `wasi:random`, `wasi:cli/exit`) around the core module; nothing in the
  language surface needs resources, streams or async I/O.
- **Roadmap fit.** DESIGN §14 phase 7 already lists "Optional WASI preview 2 /
  component output"; preview 1 now closes the §14 open decision "WASI preview 1
  or preview 2 as the module target" without blocking a later component wrapper
  (a preview-1 module can be adapted to a component with the standard
  `wasi_snapshot_preview1` adapter).

### 2.2 The remaining imports

Module `"wasi_snapshot_preview1"`, exactly these four (in this order, so the
import function indices are `0..=3`):

| import | signature | replaces |
|---|---|---|
| `fd_write` | `(fd: i32, iovs: i32, iovs_len: i32, nwritten: i32) -> i32` (errno) | `env.print` (fd 1); the CLI's `eprintln!` of uncaught errors / unhandled rejections / OOM (fd 2) |
| `clock_time_get` | `(id: i32, precision: i64, time: i32) -> i32` (writes a u64 ns count at `time`) | `env.__now` (`id = 0` realtime; ms = ns / 1e6, then the existing TimeClip) |
| `random_get` | `(buf: i32, buf_len: i32) -> i32` | the seed of `MATH_FN_RANDOM` (`host.rs::random`); the PRNG itself moves into the guest |
| `proc_exit` | `(rval: i32) -> ()` (does not return) | the CLI's exit-1 mapping of uncaught errors / unhandled rejections / OOM |

Not imported: `args_get` / `args_sizes_get` / `environ_get` /
`environ_sizes_get` — the language surface has no `process.argv` / `process.env`
(only `console.log` exists, §1.3 A), so nothing would read them. `fd_read`,
`path_*`, `poll_oneoff` are likewise unused (no timers or I/O, DESIGN §6.7).

Every other current import (`__convert`, `__method`, `__promise`, `__then`,
`__parse_int`) is replaced by guest code, not by a WASI call (sizes in §4.7).

### 2.3 Exit-code and stream contract

Must reproduce today's CLI contract (DESIGN §9, `main.rs:16-17`) so
`jsc-test262` (`outcome.rs`) and `crates/jsc/tests/**` keep passing unchanged.

| outcome | today | under WASI |
|---|---|---|
| normal completion | `run` Ok, exit 0 | `_start` returns (no `proc_exit` call): exit 0 in wasmtime, `wasi.start()` returns 0 in Node |
| uncaught exception | harness `Err("Uncaught <render>")`, CLI stderr + exit 1 | guest writes `Uncaught <render>\n` to fd 2, `proc_exit(1)` |
| unhandled rejection | `Err("Uncaught (in promise) <render>")`, exit 1 | guest writes `Uncaught (in promise) <render>\n` to fd 2 after the final drain, `proc_exit(1)` |
| out of memory | `Uncaught RangeError: out of memory`, exit 1 | until catchable OOM lands: the OOM path writes that line from a static, pre-built buffer (no allocation) to fd 2 and calls `proc_exit(1)` instead of `unreachable`; with catchable OOM it is an ordinary uncaught `RangeError` |
| trap (stack exhaustion, internal `unreachable`) | anyhow error text, exit 1 | runner-defined: the `jsc` CLI keeps "message to stderr, exit 1"; other WASI hosts report the trap their own way (not part of the module contract) |
| compile error | exit 2 | unchanged (never reaches a module) |

- **stdout vs stderr.** `console.log` is the only console method (§1.3 A), so
  all program output is fd 1; fd 2 carries only the runtime's own `Uncaught …`
  lines. If `console.error` / `console.warn` are added later they go to fd 2,
  `info`/`debug` to fd 1 (Node behaviour) — not required by this phase.
- **Buffering.** Today `print` streams each piece to the sink immediately
  (`harness.rs:177`). Recommended: one `fd_write` per `console.log` call (the
  line assembled in a guest buffer, iovec count 1), so output interleaves with
  fd 2 exactly as now and a trap loses at most the current line.
- **Encoding.** The guest encodes UTF-16 / Latin-1 string data to UTF-8,
  writing a lone surrogate as U+FFFD (today's native behaviour, DESIGN §6.9).
  The shim currently hands lone surrogates through as JS strings; under the
  polyfill both paths decode the same UTF-8 bytes, so they converge.
- **Module shape: command, not reactor.** Export `_start: () -> ()` and
  `memory` only. `_start` = `main` body; then, unless `$exc_pending`, the guest
  drain loop (§2.4); then the uncaught / unhandled-rejection report and exit.
  This matches today's order exactly (`harness.rs:310-361`: drain skipped when
  `main` threw; unhandled check after the drain; memory read after the drain).
  No wasm `start` section (a start function runs before the host can bind
  memory; Node's `wasi.start` requires `_start`).
- **Completion value (`jsc -p`).** `_start` returns nothing, so the value must
  be printed by the guest: an emit option (e.g. `print_completion`) makes
  `_start` print the completion value with the console renderer to fd 1 after
  the drain and before the exit checks — same text and order as
  `main.rs:111-114` today.

### 2.4 Microtask drain and top-level await in the guest

- The queue, reaction lists and unhandled tracking become guest state: the
  queue a growable ring of job records in the heap (rooted by a global), each
  pending promise's reaction list hung off its `TYPE_PROMISE` block (new
  `reactions` slot; `abi.rs` layout + DESIGN §5.2 change), `handled` a header
  flag bit, and rejected-unhandled promises kept in a creation-ordered list
  (not by base address: with a collector, base order is no longer creation
  order — today's `BTreeSet<u32>` relies on bump allocation, `promise.rs:91-93`).
- `__drain()` runs jobs until the queue is empty; `__invoke1` /
  `__async_step` are called directly (no host re-entry). Top-level `await p`
  (`PROMISE_OP_AWAIT`, `expr.rs:696`) becomes: mark `p` handled, `__drain()`,
  read the state, throw on rejection (same as `promise.rs:323-355`).
- `_start` calls `__drain()` after `main` iff `$exc_pending == 0`.

---

## 3. Deletions and runners

### 3.1 Rust host code

| file | LOC | fate |
|---|---|---|
| `crates/jsc-core/src/run/host.rs` | 782 | delete (all semantics move to the guest) |
| `crates/jsc-core/src/run/promise.rs` | 784 | delete |
| `crates/jsc-core/src/run/value.rs` | 1092 | delete (`decode`, `format_value`, `to_js_string`, `format_number`, `Value`); nothing outside `run/` may keep a decoder |
| `crates/jsc-core/src/run/harness.rs` | 408 | rewrite as a ~100-150 line WASI runner: engine (+ optional fuel), `Linker` with preview-1 imports, stdout/stderr sinks (real or in-memory pipes), call `_start`, map `proc_exit` / trap to an exit status. Public API becomes roughly `run(wasm, Sinks, fuel: Option<u64>) -> Result<RunOutcome { code, fuel }>` |
| `crates/jsc-core/src/run/mod.rs` | 115 | shrink: drop the NaN-box doc block (it lives in `abi.rs` / DESIGN §4.1) and every layout re-export (`mod.rs:67-115`) |
| `crates/jsc-core/src/run/tests.rs` | 1324 (130 tests) | see §3.6 |
| `abi.rs` | 948 | delete `IMPORT_MODULE`, the seven `*_IMPORT_NAME`s, `PRINT_FLAG_*`, `CONVERT_OP_*`, `METHOD_OP_*`, `METHOD_RANGE_ERROR`, `MATH_FN_*`, `PROMISE_OP_*` (become guest-internal or vanish), `INVOKE_EXPORT_NAME`, `ASYNC_STEP_EXPORT_NAME`, `ALLOC_EXPORT_NAME`, `NEW_*_EXPORT_NAME`, `PROMISE_SELF_ERROR_EXPORT_NAME`, `HEAP_PTR_EXPORT_NAME`, `EXC_*_EXPORT_NAME` (`abi.rs:684-945`); add the WASI module/import names; layout constants stay |
| `Cargo.toml` of `jsc-core` | — | `native-run` = `wasmtime` + `wasmtime-wasi` (same version); `icu_normalizer` leaves the runner (see §4 risk on where normalization data comes from) |

Stays: the `native-run` feature gate (`lib.rs:22-23`) so `jsc-wasm` keeps
building without wasmtime.

### 3.2 TypeScript shim (`packages/jsc`)

| file | LOC | fate |
|---|---|---|
| `src/shim.ts` | 863 | delete; replaced by `src/wasi.ts`, a preview-1 polyfill of exactly `fd_write` (UTF-8 decode, append to stdout/stderr strings), `clock_time_get` (realtime `Date.now() * 1e6` as a BigInt; monotonic via `performance.now()`), `random_get` (`crypto.getRandomValues`, chunked at 65536 bytes) and `proc_exit` (throw an exit sentinel carrying the code); any other import is absent so instantiation fails loudly |
| `src/value.ts` | 1014 | delete (the guest renders) |
| `src/abi.ts` | 302 | delete (no layout knowledge left in JS); `abi_drift_tests.rs::abi_ts_matches_rust_abi` goes with it |
| `src/index.ts` | 127 | rewrite: `run(src)` compiles, instantiates against the polyfill, calls `_start`, catches the exit sentinel, returns `{ stdout, stderr, exitCode }` (and the completion text when compiled with the print-completion option); no `main` / globals / memory reads |
| `tests/run.test.ts` | 737 (52 tests) | rewrite: the ~30 "like the Rust host" parity tests lose their purpose (one renderer now); keep a smaller set asserting stdout/stderr/exit code through the polyfill (links with exactly four imports, promise ordering, uncaught/unhandled exit 1, OOM line, Date clock, `Math.random` range) |

`apps/jsc-web/src/App.tsx:38-45` reads `out.result` / `out.stdout`; it follows
the new `RunResult` shape.

**Node.** Recommended default: the same polyfill in Node and the browser, so
`run()` captures output in memory in both. `node:wasi` writes to real file
descriptors (capturing needs temp files), so it is the documented way to run a
`jsc build` artifact under Node, not the package's `run()` path.

### 3.3 CLI (`crates/jsc`)

- `jsc FILE` / `-e`: compile, then `jsc_core::run::run` with the real
  stdout/stderr as WASI fds; process exit code = the module's exit code (0 / 1);
  a trap prints its message to stderr and exits 1; compile errors unchanged
  (exit 2).
- `-p`: compile with the print-completion option; nothing is decoded on the
  Rust side (`main.rs:111-114` goes away).
- `jsc build FILE -o OUT`: unchanged; the artifact now runs under
  `wasmtime run OUT.wasm` and `node:wasi` as is.
- `crates/jsc/tests/**` (295 tests via `CARGO_BIN_EXE_jsc`) pin the CLI
  contract and should pass unchanged; any that assert a trap message verbatim
  need a look.

### 3.4 test262 runner (`crates/jsc-test262`)

- Already process-based: it spawns `jsc FILE` and reads exit code + captured
  stdout/stderr files (`exec.rs:34-85`), and the crate has no `jsc-core`
  dependency (`crates/jsc-test262/Cargo.toml`). With §2.3's contract preserved,
  `exec.rs`, `outcome.rs` and `assemble.rs` need no change: async tests still
  print `Test262:AsyncTestComplete` through the `print` prelude ->
  `console.log` -> `fd_write(1)`; `Uncaught <Name>` still lands on stderr with
  exit 1.
- `$262` stays absent (not part of this phase; `$262.gc` could follow the
  collector, with `host-gc-required` removed from the denylist then).
- Expected baseline effect: neutral, except where the guest replacements are
  more or less exact than the host (number formatting, libm, normalization);
  the regression gate decides.

### 3.5 opcount harness (`crates/jsc-core/tests/opcount.rs`)

- `measure` (`opcount.rs:61-114`) calls `run::run_metered`; it becomes the WASI
  runner with fuel on and an in-memory stdout pipe.
- The `host_calls` column counts `call` targets below the import count
  (`opcount.rs:55-60`); after the change it counts calls to WASI imports
  (`fd_write`, `clock_time_get`, `random_get`, `proc_exit`). Recommend keeping
  the column under that meaning.
- Fuel rises for every program: formatting, parsing and the promise engine were
  free host time (the snapshot reasons already note this, e.g.
  `snapshot.tsv` `fib` row: "host time is not fuel"). One re-baseline commit
  with a reason per row; `push_fuel_is_linear` (`opcount.rs:228`) is unaffected.

### 3.6 Tests: deleted vs rewritten

| suite | count | fate |
|---|---|---|
| `run/tests.rs` decoder unit tests (`decode_*`, `format_value_covers_every_arm`, `number_boxing_roundtrips`, `object_literal_roundtrips_to_heap_object`, `function_value_decode_reads_type_byte`, `heap_type_byte_*`, helpers `string_block` / `push_array`) | ~15 | delete with `value.rs` |
| `run/tests.rs` language tests via `compile_and_run` | ~115 | move to `tests/` (they are integration tests) on the new helpers |
| `run/value.rs` `mod tests::exponential_thresholds` | 1 | delete; re-express as guest output tests (`String(1e21)` etc.) |
| `abi_drift_tests.rs::abi_ts_matches_rust_abi` | 1 | delete (no `abi.ts`) |
| `abi_drift_tests.rs` layout tests (`object_family_layout_matches_design`, `property_table_layout_matches_design`, `symbol_layout_matches_design`) | 3 | keep |
| `crates/jsc-core/tests/common/mod.rs` helpers | — | rewrite on the WASI runner: `assert_output` compares captured stdout; `assert_uncaught` compares stderr + exit 1; `assert_eval` / `assert_number` / `assert_string` / `assert_bool` / `assert_nan` / `assert_str` / `assert_type_str` compile with the print-completion option and compare the rendered text |
| helper call sites in `crates/jsc-core/tests/*.rs` | `assert_string` 892, `assert_number` 743, `assert_bool` 680, `assert_output` 533, `assert_uncaught` 279, `assert_eval` 261, `assert_str` 69, `compile_and_run` 38, `assert_rejected` 35, `assert_nan` 29, `assert_type_str` 10, `compile_and_run_capturing` 5 | unchanged call sites if the helpers keep their signatures |
| direct `jsc_core::run::*` users | `tests/concat_coercion.rs`, `tests/memory.rs`, `tests/opcount.rs`, `tests/unsupported.rs`, `crates/jsc/tests/typescript.rs`, `crates/jsc/src/main.rs` | rewrite against the new runner API |
| `packages/jsc/tests/run.test.ts` | 52 | rewrite (§3.2) |
| `crates/jsc/tests/**`, `crates/jsc-test262` unit tests | 295 + runner tests | unchanged |

Only one `assert_number` call compares `-0.0`
(`grep assert_number(..-0.0)`); text comparison renders `-0` as `0`, so that
case becomes `Object.is(x, -0)` under `assert_bool`.

### 3.7 In-browser compiler (`crates/jsc-wasm`)

`jsc-wasm` depends on `jsc-core` with `default-features = false` and exposes
only `compile(src)` (`crates/jsc-wasm/src/lib.rs:27-37`); it never links `run`.
It keeps working unchanged, with two caveats: (a) whatever static data the
emitter embeds for guest Unicode / number / libm support (§4) is compiled into
`jsc_wasm_bg.wasm` too, growing the playground download; (b) the playground's
"result" panel needs `compile` to accept the print-completion option (e.g.
`compile(src, { printCompletion })`).

### 3.8 CI (`.github/workflows`)

- `ci.yml` job `test`: unchanged commands; the suite now runs through
  wasmtime-wasi. `cargo deny check` / `cargo shear` must accept
  `wasmtime-wasi`'s dependency tree (`deny.toml` license / ban lists).
- `ci.yml` job `test262`: unchanged commands.
- `web.yml`: unchanged steps; `bun run --filter @cdlab/jsc test` runs the
  rewritten vitest suite. Optional addition: a smoke step that builds one
  program with `jsc build` and runs it under `node:wasi` to prove a third host.
- `release.yml`: unchanged.

---

## 4. Facts for R-T3

### 4.1 Numbers

- Imports today: 7 (`env`), host-invoked exports: 11 (`main`, `memory`,
  `__heap_ptr`, `__exc_pending`, `__exc_value`, `__invoke1`, `__async_step`,
  `__new_string`, `__new_promise`, `__promise_self_error`; plus `__alloc` and
  `__new_array` exported but unused). After: 4 imports
  (`wasi_snapshot_preview1`), 2 exports (`_start`, `memory`).
- Guest import call sites today: `print` 4, `__convert` 12, `__method` 7,
  `__promise` 14, `__then` 7, `__now` 2, `__parse_int` 1 (§1.2).
- Deleted LOC: `run/host.rs` 782, `run/promise.rs` 784, `run/value.rs` 1092,
  `run/harness.rs` 408 (rewritten to ~120), `run/tests.rs` 1324 (~15 tests
  deleted, ~115 moved), `packages/jsc/src/shim.ts` 863, `value.ts` 1014,
  `abi.ts` 302; `abi.rs` loses ~260 lines of import/export/op constants
  (`abi.rs:684-945`). Total Rust host semantics removed ≈ 3,060 LOC; TS ≈ 2,180.
- Test counts: `jsc-core` integration 1423 `#[test]`s, CLI 295, `run/tests.rs`
  130, `abi_drift_tests.rs` 4, vitest 52; opcount corpus 14 programs.

### 4.2 Guest-code gaps (rough LOC of emitter Rust)

| gap | replaces | rough size |
|---|---|---|
| Console renderer (port of `format_value` + `decode` walk over live memory: property tables, own-key order, class-instance name, errors, wrappers, functions via fn-info, Map/Set size, promises, Dates via existing ISO code, holes/sparse, symbol keys, `<ref *N>` cycles) | `print`, `-p` | 1,500-2,000 |
| UTF-8 writer + `fd_write` line buffer + stderr reporter + `_start` epilogue (drain, uncaught / unhandled reports, `proc_exit`) | `print`, CLI error mapping | 300-400 |
| Shortest `Number::toString` (Ryu / Schubfach; ≈10 KiB power-of-ten table for Ryu) | `__convert` op 0 | 500-700 |
| Exact decimal expansion (`toFixed` / `toExponential(f)` / `toPrecision`, bignum up to 1074 fractional digits) | `__method` 40-42 | 300-450 |
| Radix `toString` (port of `host.rs:338-411`, pure f64) | `__method` 17 | 120-180 |
| StringToNumber / `parseFloat` / `parseInt` with correct rounding (Eisel-Lemire fast path + big-decimal fallback) | `__convert` 1/3, `__parse_int` | 600-900 |
| libm port of 20 functions (`atan2` included) (the `libm` 0.2.16 sources for these plus kernels are ≈2,970 lines) | `__method` 43 | 2,500-3,500 |
| `Math.random` PRNG + `random_get` seeding | `MATH_FN_RANDOM` | 40-60 |
| Full Unicode case mapping (tables + `Final_Sigma`) | `__method` 4/5 | 300-450 + tables |
| Normalization NFC/NFD/NFKC/NFKD (decomposition, CCC reorder, composition; `icu_normalizer_data` 2.4.0 baked source is ≈415 KiB) | `__method` 23 | 600-900 + tables |
| `localeCompare` via `__str_cmp`; array `indexOf`/`includes` fast path via guest scan | `__method` 22, 0, 1 | < 100 |
| Promise engine (queue, reactions on the block, settle/adopt/self-resolution, unhandled list, async driver, drain, top-level await) | `__promise`, `__then` | 1,000-1,300 |
| Clock read | `__now` | 20-30 |

Total ≈ 7,800-11,000 LOC of emitter code, dominated by libm, the renderer and
number conversion.

### 4.3 Risks and open questions (with recommended defaults)

1. **Completion value for tests and `-p`.** ~2,800 assertion calls compare a
   decoded `Value`. Default: an emit option prints the completion value with the
   guest renderer; helpers keep their signatures and compare text (expected
   numbers formatted by a tiny test-only JS-number formatter in
   `tests/common`).
2. **libm fidelity.** The native host uses platform libm and the shim JS
   `Math`, so results already differ between hosts. Default: hand-port the
   `libm` crate (musl/FreeBSD msun) algorithms into the emitter for the 20
   functions, emitted only once per module; differential-test against Rust std
   on a fixed sample. Alternative to evaluate: splice a precompiled wasm object
   into the module (needs function-index relocation in the emitter).
3. **Unicode tables size and source.** Case-mapping and normalization data
   would ship in every module (all builtins are installed in every realm, so
   there is no usage-based dead-code removal) and in `jsc_wasm_bg.wasm`.
   Default: derive case tables at emit time from Rust std
   (`char::to_uppercase`/`to_lowercase` over all scalars — the same data the
   host uses today); derive normalization tables from `icu_normalizer` (+ CCC
   data) at build time into a compact checked-in table; measure module size and
   set a budget in the design. Never gate the data on source-text heuristics (a
   computed key could reach `normalize` and would then be silently wrong).
4. **Fuel / opcount.** All host work becomes metered; the snapshot must be
   re-baselined in one commit; set a per-program fuel budget for the renderer
   so `console.log` cost stays visible.
5. **Unhandled-rejection order.** Today's "first by block base" equals creation
   order only under bump allocation. Default: a creation-ordered list (or
   sequence number) in the guest.
6. **OOM reporting without allocation.** Default: a static pre-built
   `Uncaught RangeError: out of memory\n` buffer + static iovec in the data
   segment; superseded by catchable OOM.
7. **Stale documentation** (fix when moving): `harness.rs:180-183`,
   `shim.ts:686-690` and DESIGN §6.1 claim `__convert` sees Dates/functions; it
   only sees primitives (plus symbols from three message builders, §1.3 B).
   The guest ToString used by message builders must render a symbol as
   `Symbol(desc)` without throwing.
8. **Cross-host divergence disappears.** Shim vs native differences (JS
   `toUpperCase`, `toString(radix)`, `Math.*`, `Math.random`, lone surrogates in
   output) vanish once one guest implementation exists — a correctness gain; no
   parity tests needed afterwards.
9. **`wasmtime-wasi` footprint.** It may pull `tokio` / `cap-std` and enable
   wasmtime's component-model feature, growing the CLI binary and the deny/shear
   surface. Default: use `wasmtime-wasi` `p1` (true WASI conformance for free);
   if `cargo deny` or binary size objects, implement the four imports with
   `Linker::func_wrap` in the runner (~120 lines, same contract as the JS
   polyfill).
10. **Clock / random errors.** Default: `clock_time_get` errno != 0 yields time
    value `0` (today's pre-epoch behaviour, `harness.rs:280-284`); `random_get`
    failure seeds from the clock rather than aborting.
11. **Node path.** Default: the package polyfill everywhere (in-memory capture);
    `node:wasi` only documented for running `jsc build` output.
12. **Trap exit codes in foreign hosts.** wasmtime CLI and Node report traps
    their own way; only the `jsc` CLI promises exit 1. Document that the module
    contract covers exits 0/1 via `_start` return / `proc_exit`.
13. **Thenables** remain unassimilated (DESIGN §13); the guest engine port
    keeps current behaviour and does not widen scope.
14. **`$262`** stays absent; `$262.gc` and `host-gc-required` revisit after the
    collector lands.
15. **Dead exports today** (`__alloc`, `__new_array`): delete in the same change.
16. **Stack depth.** Guest renderer and drain recurse inside wasm; deep values
    or long reaction chains now consume wasm stack instead of host stack.
    Default: iterative drain loop; renderer depth bounded by the existing cycle
    guard plus a nesting cap matching Node's `depth` only if a test requires it
    (today's renderer has no depth cap, `value.rs:174`).
