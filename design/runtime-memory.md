# Phase 4 design: runtime independence (WASI) and memory management

**Status: approved 2026-11-09** (Q1-Q24 at their recommended defaults; thenable
assimilation added to R4; two PRs: R1-R4, then M1-M3).

Design for DESIGN §14 phase 4. Every claim about current behaviour cites the
tree at `c34cd36` (`origin/main`, PR #7); paths are repository-relative,
`emit/` = `crates/jsc-core/src/emit/`, `run/` = `crates/jsc-core/src/run/`,
`abi.rs` = `crates/jsc-core/src/abi.rs`, `shim.ts` / `value.ts` / `abi.ts` /
`index.ts` = `packages/jsc/src/*`. Proposed names (globals, helpers,
constants) are normative unless §8 lists them as a decision.

Contents: 0 Summary · 1 Inventory · 2 WASI target · 3 Guest replacements ·
4 Deletions and runners · 5 Memory management · 6 Weak collections ·
7 Stages · 8 Open questions · DESIGN.md changes on implementation.

---

## 0. Summary

**Status: Proposed — awaiting approval.** Nothing is implemented until the
user approves this document and answers (or accepts the defaults of) §8.

**Goals (milestone M4 of the phase):**

1. Every emitted module imports only WASI preview 1 (`fd_write`,
   `clock_time_get`, `random_get`, `proc_exit`) and exports only `_start` and
   `memory`. The same `.wasm` runs unchanged under wasmtime (the `jsc` CLI and
   `wasmtime run`), Node (`node:wasi` or the package polyfill) and browsers
   (a ~100-line polyfill).
2. All runtime semantics live in the guest: console rendering, number
   formatting and parsing, case mapping, normalization, `Math`
   transcendentals, `Math.random`, the clock read, the promise engine and the
   microtask queue. The Rust host semantics (`run/host.rs`, `run/promise.rs`,
   `run/value.rs`) and the TypeScript shim (`shim.ts`, `value.ts`, `abi.ts`)
   are deleted outright.
3. A precise, non-moving, stop-the-world mark-sweep collector with
   shadow-stack roots, size-classed pages, and a catchable out-of-memory
   `RangeError`.
4. `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry` (and
   `symbols-as-weakmap-keys`).

**Non-goals:** WASI preview 2 / component output (DESIGN §14 phase 7);
`process.argv` / `process.env` / file or network I/O / timers; `$262` and
`host-gc-required`; moving, generational or incremental collection; lazy
sweep; Map/Set backing compaction;
`console.error` / `console.warn` (only `console.log` exists today, §1.3 A);
`Intl`.

**Rules applied throughout.** No backward compatibility: every superseded
import, export, op constant, decoder, flag, doc and test is deleted in the
stage that supersedes it; no aliases, no shims, no "keep the TS shim for a
while". Throw or be correct: a host import is removed only when its guest
replacement is complete; an interim stage never stubs a semantic.

**Size of the change** (estimates, §7): ≈ 10,000-14,000 LOC of new emitter
code, ≈ 3,060 LOC Rust host semantics and ≈ 2,180 LOC TypeScript deleted;
test262 neutral through R1-R4/M1/M2, up to ≈ +310 at M3.

---

## 1. Inventory of host imports today

### 1.1 The import section

Every module imports exactly seven functions from module `"env"`
(`abi::IMPORT_MODULE`, `abi.rs:684`), declared in `emit/mod.rs:1488-1528`,
fixed at function indices `0..=6` (`IMPORT_FUNC_COUNT = 7`, `emit/mod.rs:147`;
index assignment `emit/sections.rs:63-72`; types `emit/sections.rs:180-207`).
No memory, table or global is imported.

| # | import | wasm signature | Rust implementer | TS implementer | semantics |
|---|---|---|---|---|---|
| 0 | `env.print` | `(bits: i64, flag: i32) -> ()` | closure `run/harness.rs:153-179` | `print`, `shim.ts:674` | `console.log` piece; flag `0` value + space, `1` value + newline, `2` newline only (`abi.rs` `PRINT_FLAG_*`); `format_value(decode(..))` |
| 1 | `env.__convert` | `(bits: i64, op: i32) -> i64` | closure `harness.rs:189-227` | `convert`, `shim.ts:691` | op `0` ToString of a primitive (string returned as is, else a new string via `__new_string`), op `1` ToNumber (StringToNumber), op `3` parseFloat |
| 2 | `env.__method` | `(recv: i64, op: i32, arg0: i64, arg1: i64) -> i64` | `harness.rs:235-249` → `run/host.rs::method_dispatch` (`host.rs:101`) | `methodDispatch`, `shim.ts:291`, `:709` | ops `0`/`1` array `indexOf`/`includes`, `4`/`5` upper/lower case, `17` `Number#toString(radix)`, `22` `localeCompare`, `23` `normalize`, `40`/`41`/`42` `toFixed`/`toExponential`/`toPrecision`, `43` Math (`arg1` = `MATH_FN_*`, incl. `random`) |
| 3 | `env.__promise` | `(op: i32, a: i64, b: i64) -> i64` | `harness.rs:253-261` → `run/promise.rs::dispatch` (`promise.rs:179`) | `promiseDispatch`, `shim.ts:623` | `PROMISE_OP_*`: `0` resolve, `1` reject, `2` top-level await, `3` finally, `4` new pending, `5` settle-fulfill, `6` settle-reject, `7` async spawn |
| 4 | `env.__then` | `(p: i64, on_f: i64, on_r: i64) -> i64` | `harness.rs:262-275` → `promise.rs::then` (`promise.rs:222`) | `promiseThen`, `shim.ts:667` | allocate the derived promise, register / queue the reaction |
| 5 | `env.__now` | `() -> f64` | closure `harness.rs:280-284` (`SystemTime::now()`, `0` before the epoch) | `Date.now()`, `shim.ts:731` | wall clock ms (fractional natively; the guest TimeClips, `emit/object.rs:1072-1079`) |
| 6 | `env.__parse_int` | `(s: i64, radix: i64) -> i64` | closure `harness.rs:287-299` → `host.rs::parse_int_str` | `__parse_int`, `shim.ts:733` | `parseInt(ToString(s), ToNumber(radix))` |

Every non-void import result passes through `oom_guard` (`harness.rs:52-62`;
shim `oomGuard`, `shim.ts:189`), which traps when `__heap_ptr` holds the
`HEAP_PTR_OOM` sentinel.

### 1.2 Guest call sites

| import | sites | call sites (function) |
|---|---|---|
| `print` | 4 | `emit/expr.rs:3063`, `:3074` (`emit_console_log`, `:3051`); `:3101`, `:3130` (`emit_console_log_spread`, `:3084`). The `console.log` value (`Builtin::ConsoleLog`, `emit/realm.rs:452`) lowers to the same nodes (`emit/builtins/namespace.rs:42-47`). |
| `__convert` | 12 | ToString: `emit/convert.rs:253` (`emit_to_string` = `__to_string`), `emit/object.rs:2189` (`emit_index_string_key`), `:2933` (`emit_nullish_throw`), `:3021` (`emit_prop_throw`), `emit/props.rs:2932` (`emit_private_in`), `emit/runtime.rs:1209` (`emit_json_str`), `emit/mod.rs:1690`. ToNumber: `emit/value.rs:104` (`__to_number`), `emit/object.rs:2186`, `emit/runtime.rs:1353`, `:1365` (`emit_loose_eq`), `emit/json_parse.rs:523`. parseFloat: `emit/numeric.rs:1032` (`emit_convert_call`). |
| `__method` | 7 | `emit/builtins/array.rs:591` (`emit_search` fast path), `emit/numeric.rs:1116` (inline `indexOf`/`includes`), `emit/expr.rs:1415` (`emit_proto_guard` fast path), `emit/builtins/string.rs:626` (`emit_host_op`: case map / `localeCompare` / `normalize`, ops picked in `emit/realm.rs:1658-1661`), `emit/builtins/number.rs:222` (`emit_number_to_string`, radix ≠ 10), `:336` (`emit_number_format`), `emit/numeric.rs:358` (`emit_host_math`, codes `numeric.rs:322-349`) |
| `__promise` | 14 | `emit/expr.rs:2367` (`emit_promise_call`, used by `:667` reject, `:683` finally, `:696` top-level `Await`, `:728` `AsyncSpawn`), `:2427` (`emit_promise_resolve`), `:2460`, `:2548` (`emit_promise_ctor`), `emit/runtime.rs:499` (`emit_settler`), `emit/builtins/promise.rs:606` (`emit_resolve`), `emit/builtins/generator.rs:391` (`settle_front`), `:505` (`emit_async_method`), `emit/builtins/array.rs:1539`, `:1565`, `:1606`, `:1624`, `:1652`, `:1770` (`Array.fromAsync`) |
| `__then` | 7 | `emit/expr.rs:716` (`PromiseThen`), `emit/builtins/promise.rs:413` (`emit_then`), `:899` (`combinator_value`), `emit/builtins/generator.rs:441` (`await_value`), `emit/builtins/async_iter.rs:734`, `emit/builtins/array.rs:1631`, `:1668` |
| `__now` | 2 | `emit/object.rs:997` (`emit_date_new_node`), `:1074` (`emit_date_now_node`) |
| `__parse_int` | 1 | `emit/numeric.rs:1019` (`emit_convert_call`, `ConvertOp::ParseInt`; global `parseInt` via `emit/builtins/global.rs:103`) |

### 1.3 Inventory by area

| area | host work today | guest code that already exists |
|---|---|---|
| A. output | `print` decodes against live memory (`run/value.rs::decode`, `value.rs:162`) and renders with `format_value` (`value.rs:789`); `value.ts` is a port. Only `console.log` exists: `codegen/op.rs:49` inlines only `log`, the realm `console` object has only `log` (`emit/realm.rs:2170`). No guest stderr writer; stderr text comes only from the CLI (`crates/jsc/src/main.rs:79-86`). Native output writes a lone surrogate as U+FFFD. | Date text (`emit/object.rs:1591-1660`, `emit_date_string`), `toISOString`, `Function.prototype.toString` (`emit/builtins/function.rs:579`), `Error.prototype.toString`, `__concat`, JSON quoting (`emit_json_escape`). Nothing walks a value console-style. |
| B. number → string | `ToString(Number)`: `__convert` 0 → `value.rs::format_number` (`value.rs:714`, Rust `{:e}`, `:744-786`). Radix: `__method` 17 → `host.rs::number_to_radix_string` (`host.rs:338`, V8 `DoubleToRadixCString` port). `toFixed`/`toExponential`/`toPrecision`: `__method` 40-42 → `host.rs:523-608` (exact decimal via `format!("{:.1100}")`, `host.rs:469`). Symbols in three message builders → `value.rs::to_js_string`. | no dtoa in `emit/**`; radix validation + radix-10 path `emit/builtins/number.rs:174-230`; `thisNumberValue`, `ToIntegerOrInfinity`, `RangeError`s, non-finite text `number.rs:263-336`; `emit_symbol_descriptive_string` (`numeric.rs`) |
| C. string → number | ToNumber: `__convert` 1 → `host.rs::str_to_number` (`host.rs:643`; trim, `0x`/`0o`/`0b`, `Infinity`, decimal via `str::parse::<f64>`). parseFloat: `__convert` 3 → `host.rs::parse_float_str` (`:745`). parseInt: `__parse_int` → `host.rs::parse_int_str` (`:687`; radix 10 via `parse::<f64>`, other radices f64 digit accumulation). Operands decoded lossily (lone surrogate → U+FFFD). | JSON number syntax validated in the guest (`emit/json_parse.rs:429-525`); `ToString` of parse arguments (`numeric.rs:1006-1033`); `String.prototype.trim` |
| D. case / compare / normalize | Case: guest maps one-byte receivers and calls `__method` 4/5 only for two-byte receivers and one-byte `µ`/`ß`/`ÿ` uppercase (`emit/builtins/string.rs:537-610`, `emit_case_map`); host `host.rs::map_case` (`:203`, Rust std per well-formed run, SpecialCasing, `Final_Sigma`, lone surrogates pass through); shim JS `toUpperCase` (`shim.ts:319`). `localeCompare`: `__method` 22, code-unit order (`host.rs:166-175`). `normalize`: `__method` 23 → `host.rs::normalize_units` (`:231`, `icu_normalizer` 2.4.0, optional dep under `native-run`, `crates/jsc-core/Cargo.toml:16,20`); unknown form → `METHOD_RANGE_ERROR`, thrown as `RangeError` by `emit_host_op` (`string.rs:616-640`); shim JS `normalize` (`shim.ts:343`). Reads via `value.rs::string_units` (`:318`), results via `host.rs::alloc_units` (`:61`) → `__new_string`. | one-byte case mapping; `__str_cmp` (code-unit order, `emit/runtime.rs`) makes op 22 redundant today |
| E. array search | `__method` 0/1 → `host.rs::array_index_of` / `array_includes` (`:303`, `:313`), only as a fast path for index-less, proto-intact arrays (`emit/builtins/array.rs:562-592`, `numeric.rs:1090-1118`, `expr.rs:1337-1415`) | the generic slow path of `emit_search` |
| F. Math | 20 transcendentals (`MATH_FN_CBRT..ATANH`: `cbrt log log2 log10 log1p exp expm1 sin cos tan asin acos atan atan2 sinh cosh tanh asinh acosh atanh`) → `__method` 43 → `host.rs::math_fn` (`:413`, platform libm); shim JS `Math` (`shim.ts:242-286`): hosts differ in the last ulp today (DESIGN §13). `Math.random` → `host.rs::random` (`:445`, `xorshift64*` seeded from `RandomState`); shim `Math.random()` (`shim.ts:285`). | `abs floor ceil trunc round sqrt sign min max hypot pow imul clz32 fround sumPrecise` (`numeric.rs::emit_math_call`; `host_math_code` returns `None`, `numeric.rs:322-349`) |
| G. promises | Settlement (`state @16`, `value @24`) in the guest `TYPE_PROMISE`; scheduling in the host `PromiseEngine` (`promise.rs:88-98`): FIFO `queue: VecDeque<Job>`, `reactions: HashMap<u32, Vec<Reaction>>` by block base, `rejected: BTreeSet<u32>` (ascending base = creation order under bump allocation, `promise.rs:91-93`), `handled: HashSet<u32>` (any reaction ever registered). Algorithms: `resolve` (`:274`), `finally` (`:289`), `await_promise` (`:323`), `spawn_async`/`drive_async`/`async_step` (`:360`, `:381`, `:417`), `drain`/`run_job` (`:473`, `:484`), `settle` (`:530`, settle-once, self-resolution `TypeError` via `__promise_self_error`, adoption via `register_source`), `register_source` (`:598`, non-promise = fulfilled; no thenables), `invoke1` (`:658`), `drain_after_main` (`:257`). Mirrored in `shim.ts:397-672`. | `new Promise` settlers (`emit/runtime.rs:488`), `then` with species / `NewPromiseCapability` (`emit/builtins/promise.rs:349`), combinators (`:823+`), `Promise.resolve`/`reject` (`:580`), async-generator request queue and driver (`emit/builtins/generator.rs:378-505`), `%AsyncFromSyncIteratorPrototype%` (`emit/builtins/async_iter.rs`), `Array.fromAsync` (`emit/builtins/array.rs:1530-1770`), `__invoke1` / `__async_step` trampolines (`emit/runtime.rs`) |
| H. clock | `__now` only | all Date arithmetic, parsing (`emit/builtins/date_parse.rs`), TimeClip (`emit_time_clip`), string forms |
| I. errors / exit / OOM | Uncaught: harness reads `__exc_pending`/`__exc_value` after `main` (`harness.rs:343-355`) → `Err("Uncaught <render>")`, CLI stderr + exit 1 (`main.rs:83-86`). Unhandled rejection: `Err("Uncaught (in promise) <render>")` after the drain (`harness.rs:356-361`). Drain skipped when `main` threw (`harness.rs:315-331`). OOM: `__alloc` stores `HEAP_PTR_OOM` and executes `unreachable` (`emit/runtime.rs:82-86`); harness reports `Uncaught RangeError: out of memory` (`harness.rs:47`, `:305-313`, `:325-327`); not catchable. Other traps: exit 1 with the trap text. Compile errors: exit 2 (`main.rs:79-82`). | `$exc_pending` / `$exc_value` manual unwinding (DESIGN §6.6) |
| J. test262 hooks | no `$262` (`IsHTMLDDA`, `host-gc-required` in `tests/test262/unsupported-features.txt:51`, `:62`); `print` is a JS prelude (`crates/jsc-test262/src/assemble.rs:14`); async outcome from stdout `Test262:AsyncTestComplete`/`Failure` (`outcome.rs::classify_async`); runtime-negative = exit 1 + stderr `Uncaught <Name>` (`outcome.rs::verdict`/`uncaught_name`) | — |
| K. other | `__alloc`, `__new_array` exported (`emit/mod.rs:1591-1598`) but unused by any host (dead today). The completion value: `main` returns the last expression statement's jsval (DESIGN §4.2, `harness.rs:301`); CLI `-p` decodes it (`main.rs:111-114`); ≈ 2,800 test assertions read it (§4.6). | — |

**Stale claim resolved.** `run/harness.rs:180-183`, `shim.ts:686-690` and
DESIGN §6.1 say `__convert` op 0 sees a Date or a function. It cannot:
`emit_to_string` (`emit/convert.rs:225-260`) runs `__to_primitive` on every
object first. The host only sees primitives, plus symbols from the three
message builders (`emit_prop_throw`, `emit_nullish_throw`, `emit_private_in`)
that bypass `__to_string`. The guest ToString used by those builders must
render a symbol as `Symbol(desc)` without throwing (§3.2).

### 1.4 Host → guest exports today

| export | signature | invoked by | re-entrant inside an import |
|---|---|---|---|
| `main` | `() -> i64` | `harness.rs:301`, `index.ts:87` | no |
| `memory` | memory | every decode / alloc | — |
| `__heap_ptr` | global i32 | `oom_guard`, `exhausted` (`harness.rs:52`, `:305`) | — |
| `__exc_pending` / `__exc_value` | globals | uncaught check (`harness.rs:319-355`); `promise.rs::invoke1`, `promise.rs:342-347` | yes |
| `__new_string(len, two_byte) -> i32` | func | `host.rs::alloc_units` | yes |
| `__new_promise() -> i32` | func | `promise.rs::alloc_promise` (`:703`) | yes |
| `__promise_self_error() -> i64` | func | `promise.rs::settle` | yes |
| `__invoke1(fn, arg) -> i64` | func | `promise.rs::invoke1` | yes, arbitrarily deep: guest → `__promise`(await) → host drain → `__invoke1` → guest → … |
| `__async_step(gen, sent) -> i64` | func | `promise.rs::async_step` | yes |
| `__alloc`, `__new_array` | func | nobody | — |

Final state: exports `_start` and `memory` only; `__invoke1` / `__async_step`
become internal helpers; everything else is deleted.

---

## 2. Target

### 2.1 WASI preview 1 (recommended) vs preview 2

**Decision (Q1 default): `wasi_snapshot_preview1`, core module, command
shape.** This closes the DESIGN §14 open decision.

| criterion | preview 1 | preview 2 |
|---|---|---|
| wasmtime | four hand-written `Linker::func_wrap` imports (Q8), in-memory buffers for capture; fuel unaffected | component model in the runner |
| Node | `node:wasi` (`version: 'preview1'`) | not supported by `node:wasi`; needs jco |
| browsers | four functions, ~100-line polyfill | jco transpile + `@bytecodealliance/preview2-shim` per run |
| emitter | four `(import ...)` entries | canonical-ABI lifting/lowering of `wasi:cli/stdout`, `wasi:clocks`, `wasi:random`, `wasi:cli/exit` around the core module |
| future | a preview-1 module adapts to a component with the standard adapter (phase 7) | — |

### 2.2 The remaining imports

Module `"wasi_snapshot_preview1"`, exactly these four, in this order
(function indices `0..=3`; new `abi::WASI_MODULE` and four `WASI_*_NAME`
constants; `IMPORT_FUNC_COUNT = 4`):

| # | import | signature | replaces | use |
|---|---|---|---|---|
| 0 | `fd_write` | `(fd: i32, iovs: i32, iovs_len: i32, nwritten: i32) -> i32` (errno) | `env.print`; the CLI's `eprintln!` of `Uncaught …` | fd 1 = `console.log`, fd 2 = runtime reports |
| 1 | `clock_time_get` | `(id: i32, precision: i64, time: i32) -> i32` (writes u64 ns at `time`) | `env.__now` | `id = 0` (realtime), `precision = 1_000_000` |
| 2 | `random_get` | `(buf: i32, buf_len: i32) -> i32` | the seed of `host.rs::random` | 16 bytes, once (§3.6) |
| 3 | `proc_exit` | `(rval: i32) -> ()` (no return) | the CLI's exit-1 mapping | exit 1 after a report |

Not imported: `args_*`, `environ_*`, `fd_read`, `path_*`, `poll_oneoff` —
nothing in the language surface reads them (no `process`, no timers or I/O,
DESIGN §6.7). The other five current imports are replaced by guest code
(§3), not by WASI calls.

All WASI scratch (one iovec, `nwritten`, the 8-byte clock result, the
16-byte random seed buffer) lives at fixed addresses in the static region
(new `abi::WASI_SCRATCH`, 40 bytes), so no WASI call allocates.

### 2.3 Exit-code and stream contract

Reproduces today's CLI contract (DESIGN §9, `main.rs:16-17`) so
`crates/jsc-test262` (`outcome.rs`) and `crates/jsc/tests/**` pass unchanged.

| outcome | today | under WASI |
|---|---|---|
| normal completion | exit 0 | `_start` returns (no `proc_exit`): exit 0 in wasmtime, `wasi.start()` returns 0 in Node |
| uncaught exception | `Uncaught <render>` on stderr, exit 1 | guest writes `Uncaught <render>\n` to fd 2, `proc_exit(1)` |
| unhandled rejection | `Uncaught (in promise) <render>`, exit 1 | guest writes `Uncaught (in promise) <render>\n` to fd 2 after the final drain, `proc_exit(1)` |
| out of memory (R1..M1) | `Uncaught RangeError: out of memory`, exit 1, not catchable | `__alloc`'s OOM path writes that line from a static pre-built buffer (static iovec, no allocation) to fd 2 and calls `proc_exit(1)` instead of `unreachable` |
| out of memory (M2+) | — | catchable `RangeError` (§5.8); only "reserve exhausted while OOM pending" still takes the static-buffer + `proc_exit(1)` path |
| `fd_write` errno ≠ 0 | CLI I/O failure → exit 1 (`main.rs:67`) | `proc_exit(1)` (a partial write is retried until complete) |
| trap (stack exhaustion, internal `unreachable`) | trap text, exit 1 | runner-defined: the `jsc` CLI keeps "message to stderr, exit 1"; other hosts report traps their own way (not part of the module contract) |
| compile error | exit 2 | unchanged |

- **stdout / stderr.** All program output is fd 1 (only `console.log`
  exists); fd 2 carries only the runtime's `Uncaught …` lines.
- **Buffering.** One `fd_write` per `console.log` call: the line (all
  arguments, separators and the newline) is assembled in a guest buffer and
  written with one iovec. Interleaving with fd 2 matches today (`print`
  streams each piece immediately, `harness.rs:177`); a trap loses at most the
  current line.
- **Encoding.** The guest encodes Latin-1 / UTF-16 string data to UTF-8; a
  lone surrogate is written as U+FFFD (today's native behaviour, DESIGN
  §6.9). The shim's pass-through of lone surrogates disappears: every host
  decodes the same bytes.
- **Module shape: command.** Exports `_start: () -> ()` and `memory` only.
  No wasm `start` section (a start function runs before the host binds
  memory; `node:wasi` requires `_start`). Memory stays exported, not
  imported, maximum 8192 pages (512 MiB).

### 2.4 `_start` and drain-before-exit

`_start` (emitted by `emit/function.rs`, replacing the exported `main`):

```text
_start:
  v = <main body>                       ; completion value jsval (§4.6)
  if !$exc_pending: __drain()            ; microtasks, checkpoints, finalization (§3.4, §6)
  if $exc_pending:
      __report(fd 2, "Uncaught ", $exc_value); proc_exit(1)
  if p = __first_unhandled():            ; creation order (§3.4)
      __report(fd 2, "Uncaught (in promise) ", p.value); proc_exit(1)
  if print_completion: __report(fd 1, "", v)
  return
```

This is today's order exactly (`harness.rs:310-361`: drain skipped when
`main` threw, uncaught before unhandled, both after the drain, and
`main.rs:111-114` prints the completion only on success). `v` is rooted in
`_start`'s frame across the drain (§5.3). `__report` renders with the guest
renderer (§3.1) into the output buffer and issues one `fd_write`.

Top-level `await p` (`PROMISE_OP_AWAIT`, `emit/expr.rs:696`) becomes: mark
`p` handled, `__drain()`, read the state, throw the reason on rejection
(same semantics as `promise.rs:323-355`).

---

## 3. Guest replacements

Each replacement is emitted once per module as runtime helpers (`emit/`
helper emitters, DESIGN §6.2). All builtins are installed in every realm, so
there is no usage-based dead-code removal and none is attempted: gating
helpers or tables on source-text heuristics would be silently wrong for a
computed member access.

### 3.1 `console.log` and value rendering

**Helpers.** `__render(v, depth_path)` appends the console form of `v` to
the output buffer; `__out_*` manage the buffer; `__report(fd, prefix, v)`
and the `console.log` lowering (`emit_console_log`,
`emit_console_log_spread`) call them. The output buffer is a growable
`TYPE_RAW` block (§5.4) rooted by the global `$__out_buf` and holding UTF-8
bytes; it is reset (length `0`) after every `fd_write`.

**Parity source.** `run/value.rs` (`decode_heap`, `format_value`,
`format_wrapper`, `quote_key`, `format_entries`, `format_date_iso`) is the
reference; `value.ts` is a port of it and adds nothing. The guest renderer
reproduces today's output byte for byte, so `assert_output` expectations do
not change:

| value | rendered as | source |
|---|---|---|
| number | ECMAScript `Number::toString` (§3.2); `-0` → `0` | `value.rs:714` |
| boolean / `null` / `undefined` | `true` / `null` / `undefined` | `value.rs:792-795` |
| string | raw text, **unquoted at the top level and nested inside containers** (today's deliberate behaviour; e.g. `[ a, b ]` for `['a','b']`) | `value.rs:796-798`; DESIGN §6.9 |
| symbol | `Symbol(desc)` / `Symbol()` | `value.rs:800-802` |
| array | `[ e0, e1 ]`, `[]` empty, hole runs `<1 empty item>` / `<N empty items>`, then own named props `k: v` | `value.rs:803-833` |
| ordinary object | `{ a: 1, b: 2 }` / `{}`, own enumerable props in own-key order, enumerable symbol keys last as `[Symbol(a)]: 1`, accessors `[Getter]` / `[Setter]` / `[Getter/Setter]` | `format_entries` `value.rs:1010`; `value.rs:861-865` |
| class instance | `A { x: 1 }` when a `constructor` data property on the proto chain names a function other than `Object` (data lookups only, no getter runs) | `value.rs:837`; `lookup_data` `:675` |
| wrapper | `[Number: 1]`, `[Number: -0]`, `[String: 'ab']` (quoted), `[Boolean: true]`, `[Symbol: Symbol(a)]`, then own props; String wrapper indices not listed | `format_wrapper` `value.rs:915` |
| `arguments` | `[Arguments] { '0': 1 }` (non-identifier keys single-quoted) | `value.rs:841-848`, `quote_key` `:997` |
| error (`CLASS_ERROR`) | `Name: message`, or `Name` when the message is empty; name/message by data lookup along the chain | `value.rs:849-858` |
| function / class | `[Function: f]`, `[Function (anonymous)]`, `[class A]`, `[class A extends B]`, `[class (anonymous)]`, names from the function info table, then own enumerable props | `value.rs:866-890`; `function_name` `:690` |
| Map / Set | `Map(N)` / `Set(N)` (size only) | `value.rs:893-894` |
| promise | `Promise { v }`, `Promise { <pending> }`, `Promise { <rejected> v }` | `value.rs:896-904` |
| generator | `Object [Generator]` | `value.rs:906` |
| Date | ISO 8601 UTC (expanded `±YYYYYY` outside `0..=9999`), `Invalid Date` for NaN — reuses the guest `toISOString` digits (`emit_date_string`) | `format_date_iso` `value.rs:1032` |
| cycles | containers `TYPE_ARRAY`/`TYPE_OBJECT`/`TYPE_PROMISE`/`TYPE_FUNCTION` are cycle-guarded: a block already on the recursion path renders `[Circular *N]`; the target renders `<ref *N> …`; `N` numbered in discovery order | `Guard` `value.rs:140-153`, `decode_heap` `:203-230` |
| other type byte | `undefined` (decode is total) | `value.rs:240` |

Implementation notes:

- The recursion path is a stack of block bases in a `TYPE_RAW` scratch
  block (rooted); the ref table is a second one. Because `<ref *N>` is a
  prefix of a container whose cycle is discovered only while rendering its
  children, the renderer records the output offset at container entry and,
  on exit with an assigned ref, inserts `<ref *N> ` at that offset
  (memmove within the buffer).
- Rendering never runs user code (data lookups only, `__prop_find`), never
  throws, and has no depth cap (today none, `value.rs:174`; Q14).
- Private names are never enumerable (DESIGN §6.9) and never listed.
- M3 adds `WeakMap { <items unknown> }`, `WeakSet { <items unknown> }`,
  `WeakRef {}` and `FinalizationRegistry {}`.

Changing nested-string quoting to Node's (`[ 'a', 'b' ]`) is a separate,
user-visible decision (Q3); this design keeps parity.

### 3.2 Number → String

**Algorithm: Ryu (Adams 2018) shortest round-trip, size-optimized tables
(`RYU_OPTIMIZE_SIZE` variant, `d2s_small_table`).** Justification:

- Ryu is proven to produce the shortest digit string that round-trips and,
  among those, the closest to the exact value with ties to even — exactly
  ECMA-262 `Number::toString` step 5 and its note — so output equals today's
  Rust `{:e}` (`value.rs:744-786`), which has the same contract.
- No fallback path (Grisu needs a bignum fallback on ≈ 0.5 % of inputs).
- The small-table variant computes the 128-bit power-of-5 multipliers from
  26 + 13 + 15 base entries plus offset words: **< 1 KiB** of data per module
  (the full table is ≈ 9.9 KiB, Q6). Cost: a few extra 64×64 multiplies per
  conversion.
- Wasm has no 64×64→128 multiply; `__umul128` is emitted from four
  `i64.mul` of 32-bit halves (≈ 15 ops).

| operation | guest design |
|---|---|
| `ToString(Number)` (`__to_string`, `emit_index_string_key`, JSON, `join`, templates) | NaN / ±Infinity / ±0 special cases; else `__ryu_d2d(bits) -> (digits: u64, exp10: i32)`, then the spec layout of `format_finite_positive` (fixed for `-6 < n <= 21`, exponential otherwise, `e+`/`e-`). Digits written into a static 32-byte scratch, one `__new_string` (Latin-1) at the end. |
| ToString of booleans / `null` / `undefined` | static interned strings |
| symbol in message builders | `emit_symbol_descriptive_string` (never throws); the three builders call the guest ToString variant that accepts symbols |
| `toFixed(f)` / `toExponential(f)` / `toPrecision(p)` | exact decimal expansion: `x = m·2^e`; `e >= 0` → bignum `m << e` (≤ 1024 bits); `e < 0` → bignum `m·5^(-e)` (≤ 2,548 bits, 80 u32 limbs) with the decimal point `-e` digits from the right; digits by repeated division by `10^9`; round to the requested digit count, ties to the larger `n` (spec "pick the larger n", = today's "ties away from zero", `host.rs:523-608`). `toExponential()` without digits uses the Ryu shortest digits. The expansion has at most 768 significant digits (`m·5^1074` for subnormals; ≤ 309 for `e >= 0`) plus a decimal-point position; bignum limbs (512 B) and the digit buffer (800 B) live in a static scratch `NUM_SCRATCH` (2 KiB). Existing guest range checks / `RangeError`s / non-finite text (`number.rs:263-336`) stay. |
| `Number#toString(radix ≠ 10)` | port of `host.rs::number_to_radix_string` (`host.rs:338-411`, V8 `DoubleToRadixCString`: integer part by repeated division, fraction by multiplication with the half-ulp delta), pure f64, buffer in `NUM_SCRATCH` (1,100 chars) |

`NUM_SCRATCH` is safe as a static buffer: number formatting runs no user
code, is not re-entrant, and allocates only the final string after the
digits are complete (a collection at that allocation does not touch static
data).

### 3.3 StringToNumber, `parseFloat`, `parseInt`

**Algorithm: Clinger fast path + Simple Decimal Conversion slow path** (port
of Rust `core::num::dec2flt`'s `parse_number` fast path and
`slow::parse_long_mantissa` / `decimal::Decimal`, i.e. Nigel Tao's
algorithm). Always correctly rounded (round-half-even), matching today's
`str::parse::<f64>`. Eisel-Lemire is not included (its 128-bit power table is
≈ 10.4 KiB per module); it is a phase-6 option (Q7).

| step | design |
|---|---|
| input | operate on code units directly (Latin-1 or UTF-16); no lossy decode (a lone surrogate is never whitespace or a digit, DESIGN §6.8) |
| trim | the existing guest `StrWhiteSpaceChar` predicate (`trim`) |
| `StringToNumber` | empty → `0`; `0x`/`0o`/`0b` → non-decimal integer (below); `[+-]Infinity`; else `StrDecimalLiteral` must consume the whole trimmed string, else `NaN` |
| decimal scan | up to 19 significant digits into a u64 mantissa + exponent; digits beyond 19 set `many_digits` |
| fast path | `!many_digits`, mantissa `<= 2^53`, `-22 <= exp10 <= 22` → `f64(m) * 10^e` or `/ 10^-e` (exact; 23-entry f64 table, 184 B); plus the "disguised" fast path (`exp10 > 22`, `m·10^(exp10-22) <= 2^53`) |
| slow path | `Decimal` buffer of 768 digits + `decimal_point` + `truncated`, scaled by left/right shifts by powers of two until in `[0.5, 1)`, then rounded; tables: 65-entry `u16` left-shift table + the packed power-of-5 digits (≈ 1.4 KiB total). The 768-digit buffer is a static scratch `DEC_SCRATCH` (no allocation, not re-entrant) |
| non-decimal (`0x`, `0o`, `0b`, `parseInt` with radix `2,4,8,16,32`) | exact: accumulate bits into a u64 window, keep a sticky bit for discarded bits, round half to even at 53 bits, scale by `2^k` (spec requires exact results for these radices) |
| `parseFloat` | trim leading whitespace; longest prefix that is a `StrDecimalLiteral` (incl. `Infinity`, no hex); same decimal core; empty prefix → `NaN` |
| `parseInt` | spec §19.2.5: trim, sign, `R = ToInt32(radix)` (0 → 10; 16 strips `0x`), digit prefix; `R = 10` → decimal core (correctly rounded; the spec's "may zero digits after the 20th" licence is not used); power-of-two radices → exact path; other radices → f64 digit accumulation (spec-permitted approximation, identical to `host.rs:687`) |
| JSON numbers | `emit/json_parse.rs:523` calls the decimal core on the already validated slice |

Expected change vs today: `parseInt` with a power-of-two radix and more than
53 significant bits becomes exactly rounded (today f64 accumulation); no
other output changes.

### 3.4 Promise engine, microtask queue, async drivers

Replaces `run/promise.rs` (784 LOC), `shim.ts:397-672`, the `__promise` /
`__then` imports, and every host → guest re-entry (`__invoke1`,
`__async_step`, `__new_promise`, `__promise_self_error`). Semantics are
ported from the host engine (same settle-once, same self-resolution
`TypeError`), with one approved change: **thenables are assimilated per the
spec**. The resolve function does `Get(resolution, "then")` (an abrupt
completion rejects), and a callable `then` enqueues a
`PromiseResolveThenableJob` with fresh once-only resolving functions, for
native promises too; `await` and `Promise.resolve` use `PromiseResolve` (a
promise of the same constructor is returned as is). Where the old host's
adoption shortcut and the spec order jobs differently, the spec wins and
the DESIGN §13 thenable limitation is deleted.

**`TYPE_PROMISE` layout (40 bytes, was 32 — `PROMISE_BLOCK_SIZE`):**

| offset | field | notes |
|---|---|---|
| 0..16 | object-family prefix | |
| 16 | `state: u32` | `PROMISE_PENDING/FULFILLED/REJECTED` (unchanged) |
| 20 | `reactions: u32` | raw ref to the newest reaction record (prepend list), `0` = none |
| 24 | `value: i64` | unchanged |
| 32 | `seq: u32` | creation sequence number from the global `$__promise_seq` |
| 36 | `rejected_next: u32` | raw link in the rejected-promise list |

Header flags (byte 2) for promises: bit `2` `HANDLED` (`[[PromiseIsHandled]]`
in today's sense: any reaction was ever registered, `promise.rs:96-97`),
bit `4` `IN_REJECTED_LIST`. Byte 1 stays reserved for the collector (§5.2) —
this resolves the R-T1 note's "handled as a header flag bit" against the GC
epoch.

**Reaction / job record** (one `TYPE_ENV` of 6 jsval slots — the same block
is first a reaction, then a queued job, so settling never allocates):

| slot | content |
|---|---|
| 0 | `on_fulfilled` callback, or the async-function generator handle |
| 1 | `on_rejected` callback (`undefined` = passthrough) |
| 2 | `result`: the derived promise (`HEAP` jsval) or `undefined` |
| 3 | `kind`: small integer — bit 0 `FINALLY`, bit 1 `ASYNC_RESUME`, bits 2-3 the source state once queued |
| 4 | the settlement value (written when queued) |
| 5 | `next`: `HEAP | record` or `0` (never a raw `u32`, cf. F2 §5.5) |

**Globals:** `$__job_head`, `$__job_tail` (raw `i32` refs to job records;
roots), `$__promise_seq`, `$__rejected_head` (raw ref, root).

**Helpers** (all guest, internal; names normative):

| helper | ports | behaviour |
|---|---|---|
| `__promise_new(proto) -> i32` | `alloc_promise` (`promise.rs:703`) | allocate, `seq = $__promise_seq++` |
| `__promise_settle(p, state, v)` | `settle` (`:530`) | settle-once; `v == p` → reject with a fresh `TypeError` (inline, no export); `v` a promise → adopt via `__promise_react(v, …)` (register as source, `register_source` `:598`); else write state/value, reverse `reactions` in place and append each record to the job queue with slot 3/4 filled; a rejection with no reactions and not `HANDLED` links `p` into the rejected list (`IN_REJECTED_LIST`) |
| `__promise_react(p, on_f, on_r, result, kind)` | `then` (`:222`), `register_source` | set `HANDLED`; pending → allocate a record and prepend; settled → allocate and enqueue directly |
| `__promise_then(p, on_f, on_r) -> jsval` | `__then` import | derived promise (species / capability stays in `emit/builtins/promise.rs:349`) + `__promise_react` |
| `__promise_resolve`, `__promise_finally` | `resolve` (`:274`), `finally` (`:289`) | as today |
| `__async_spawn(gen) -> jsval` | `spawn_async` / `drive_async` (`:360`, `:381`) | result promise, first `__async_step`, on `await` register an `ASYNC_RESUME` record on the awaited promise |
| `__run_job(rec)` | `run_job` (`:484`) | select the callback by state; non-callable → passthrough; else `__invoke1` (direct call, no host); finally semantics; settle `result` from the outcome (`$exc_pending` cleared and turned into a rejection) |
| `__drain()` | `drain` (`:473`), `drain_after_main` (`:257`) | iterative loop: pop head, run, until empty; then the **checkpoint**: prune handled promises from the rejected list, `ClearKeptObjects` (M3), finalization cleanup jobs (M3), and loop again if they queued jobs |
| `__first_unhandled() -> jsval` | `drain_after_main` | walk the rejected list, skip `HANDLED`, return the promise with the **minimum `seq`** (today: minimum base, `promise.rs:91-93`, which equals creation order only under bump allocation; with free-list reuse it would not), or `0` |

The job queue is an intrusive FIFO of the records themselves (no ring
buffer, no growth, no allocation at settle time). The drain is a loop, not
recursion, so long reaction chains consume no wasm stack. HTML's
"HostPromiseRejectionTracker" maps to: `reject` with no handler → link into
the rejected list; `handle` → set `HANDLED` (list entry pruned at the next
checkpoint). Reported once, after the final drain, as today.

`__invoke1` and `__async_step` (`emit/runtime.rs`) stay as internal
trampolines and lose their exports; `__new_promise` and
`__promise_self_error` disappear (inlined into `__promise_new` /
`__promise_settle`). `abi::PROMISE_OP_*` are deleted; the emitted call sites
of §1.2 call the helpers above directly.

### 3.5 Date clock via `clock_time_get`

`__now() -> f64`: `clock_time_get(0, 1_000_000, WASI_SCRATCH+clock)`;
errno ≠ 0 → `0` (today's pre-epoch behaviour, `harness.rs:280-284`); else
`f64(ns) / 1e6` (fractional like the Rust host), then the existing TimeClip
(`emit/object.rs:1072-1079`). Call sites `emit/object.rs:997`, `:1074`
unchanged apart from the callee.

### 3.6 `Math.random` via `random_get`

**Seeded `xorshift128+`** (V8's generator) in two `i64` globals
`$__rng0/$__rng1`, seeded lazily on the first call with
`random_get(WASI_SCRATCH+seed, 16)`. On errno ≠ 0 the seed is the clock
nanoseconds mixed with `splitmix64`; an all-zero state is replaced by a fixed
non-zero constant. Result: `f64::from_bits((s0 >> 12) | 0x3FF0…) - 1.0`
(uniform in `[0, 1)`, 52 bits). Per-call `random_get` is rejected: a host
call per number for no statistical gain (the spec requires no
cryptographic quality). ≈ 40-60 LOC; replaces `host.rs::random` (`:445`)
and `MATH_FN_RANDOM`.

### 3.7 Unicode tables (case mapping, normalization)

**Today:** case mapping by Rust std (`host.rs::map_case`, `:203`) / JS
engine (shim); normalization by `icu_normalizer` 2.4.0 on the host
(`host.rs::normalize_units`, `:231`). After R3 both are guest code driven by
static tables.

| table | content | encoding | budget |
|---|---|---|---|
| case runs (upper, lower) | simple mappings as runs `(start: u24, len: u8, delta: i32, stride: u8)` (stride 1 = constant delta, 2 = alternating upper/lower pairs) | sorted by `start`, binary search | ≤ 6 KiB |
| special casing | unconditional multi-unit mappings (`ß → SS`, ligatures, `İ` lower, Greek iota-subscript uppers), ≈ 110 entries `(cp: u24, len: u8, units…)` | sorted, binary search | ≤ 2 KiB |
| `Final_Sigma` | algorithmic (cased / case-ignorable scan), as Rust std; needs `Cased` and `Case_Ignorable` ranges | sorted ranges `(start: u24, end: u24)` | ≤ 3 KiB |
| CCC | non-zero canonical combining classes as runs `(start: u24, len: u8, ccc: u8)` | sorted, binary search | ≤ 3 KiB |
| decompositions | canonical + compatibility full decompositions (precomputed recursive), Hangul algorithmic; index `(cp: u24, flags: u8 [compat?], off: u16, len: u8)` + `u24` expansion pool | sorted index, binary search | ≤ 52 KiB |
| compositions | primary composites `(first: u24, second: u24 → composite: u24)` excluding the composition exclusions | sorted by `(first, second)` | ≤ 8 KiB |
| **total per module** | | | **≤ 74 KiB, hard cap 96 KiB** (asserted by a test, §7 R3) |

- **Data segment layout.** One active data segment placed in
  the static region after the existing static blocks and before the
  64 KiB-aligned heap start (§5.4); table base addresses are emitter
  constants recorded in `Helpers` (no new globals). Tables are raw data, not
  headered blocks: never traced, never swept, never referenced by a jsval.
- **Generation.** A new workspace tool crate `tools/unicode-tables`
  (`publish = false`, not a dependency of `jsc-core`) generates the
  checked-in `crates/jsc-core/src/emit/unicode_data.rs` (`pub(crate) static
  …: &[u8]`). Case and `Final_Sigma` data come from Rust std
  (`char::to_uppercase` / `to_lowercase` over all scalars, the same data the
  host uses today); CCC, decompositions and compositions from
  `icu_normalizer` / `icu_properties` (the same data `normalize` uses today).
  The generated file records `char::UNICODE_VERSION` and the icu crate
  versions in a header comment. CI runs `cargo run -p unicode-tables --
  --check` (fails on drift). Emit-time derivation is rejected: it would
  re-scan 1.1 M scalars per compile and put `icu_*` into `jsc_wasm_bg.wasm`.
- **Algorithms.** Case mapping per well-formed run with lone surrogates
  passed through (today's rule, DESIGN §6.1); one-byte fast path in
  `emit_case_map` stays. `normalize`: decompose (full, recursive tables),
  canonical reordering by CCC (stable insertion sort per non-starter run),
  canonical composition for NFC/NFKC; Latin-1 strings below U+0300 that are
  already NFC/NFD return the receiver. Form validation moves to the guest
  (`RangeError` directly; `METHOD_RANGE_ERROR` deleted).
- **`jsc_wasm_bg.wasm`** carries the tables once (≈ 74 KiB) plus the new
  emitter code; acceptable for the playground (Q5).

### 3.8 `Math` transcendentals

Hand-port of the `libm` crate (musl / FreeBSD msun) algorithms for the 20
functions of §1.3 F into emitter helpers (`emit/libm.rs`), emitted once per
module, pure f64/i64 wasm (≈ 2,500-3,500 emitter LOC; the `libm` 0.2.16
sources for these plus kernels are ≈ 2,970 lines). This removes the
platform-libm vs JS-engine divergence (DESIGN §13 "Math transcendental
functions follow the host libm") — results are deterministic on every host.
Differential test against Rust std on a fixed sample (§7 R3). Splicing a
precompiled wasm object is rejected for phase 4 (needs function-index
relocation in the emitter; Q4).

### 3.9 Remaining `__method` ops

| op | replacement |
|---|---|
| `0`/`1` array `indexOf`/`includes` fast path | guest scan loop over the `TYPE_ELEMS` store (strict equality / `__same_value_zero`) in `emit_search`, `numeric.rs:1090-1118`, `expr.rs:1337-1415` |
| `22` `localeCompare` | `__str_cmp` (already code-unit order) |
| `17`, `40`-`42` | §3.2 |
| `4`/`5`, `23` | §3.7 |
| `43` | §3.8, §3.6 |

---

## 4. Deletions and runners

### 4.1 Rust host code

| file | LOC | fate |
|---|---|---|
| `run/host.rs` | 782 | delete (per area in R2/R3, file gone at R3) |
| `run/promise.rs` | 784 | delete (R4) |
| `run/value.rs` | 1092 | delete: formatting in R1, decoding helpers with their last user (R3); nothing outside `run/` keeps a decoder |
| `run/harness.rs` | 408 | rewrite as a ≈ 120-150-line WASI runner (§4.3) |
| `run/mod.rs` | 115 | shrink: drop the NaN-box doc block and the layout re-exports (`mod.rs:67-115`) |
| `run/tests.rs` | 1324 (130 tests) | §4.6 |
| `abi.rs` | 948 | delete `IMPORT_MODULE`, the seven `*_IMPORT_NAME`s, `PRINT_FLAG_*`, `CONVERT_OP_*`, `METHOD_OP_*`, `METHOD_RANGE_ERROR`, `MATH_FN_*`, `PROMISE_OP_*`, `INVOKE_EXPORT_NAME`, `ASYNC_STEP_EXPORT_NAME`, `ALLOC_EXPORT_NAME`, `NEW_*_EXPORT_NAME`, `PROMISE_SELF_ERROR_EXPORT_NAME`, `HEAP_PTR_EXPORT_NAME`, `HEAP_PTR_OOM`, `EXC_*_EXPORT_NAME` (`abi.rs:684-945`); add `WASI_MODULE`, the four WASI names, `START_EXPORT_NAME`, `WASI_SCRATCH`, layout constants of §3/§5 |
| `crates/jsc-core/Cargo.toml` | — | `native-run = ["dep:wasmtime"]`; no WASI crate (Q8); `icu_normalizer` removed from `jsc-core` (moves to `tools/unicode-tables`) |

The `native-run` feature gate (`crates/jsc-core/src/lib.rs:22-23`) stays so
`jsc-wasm` builds without wasmtime.

### 4.2 TypeScript package `@cdlab/jsc`

| file | LOC | fate |
|---|---|---|
| `src/shim.ts` | 863 | delete (R4; shrinks per stage before) |
| `src/wasi.ts` | new (R1) | preview-1 polyfill of exactly `fd_write` (UTF-8 decode, append to stdout / stderr strings), `clock_time_get` (realtime `BigInt(Date.now()) * 1_000_000n`; monotonic via `performance.now()`), `random_get` (`crypto.getRandomValues`, chunks of 65,536 bytes), `proc_exit` (throws an `Exit` sentinel carrying the code); any other import is absent so instantiation fails loudly |
| `src/value.ts` | 1014 | delete (formatting R1, rest R3) |
| `src/abi.ts` | 302 | delete (R4); `abi_drift_tests.rs::abi_ts_matches_rust_abi` goes with it |
| `src/index.ts` | 127 | rewrite (R1): `run(src)` compiles, instantiates against `wasi.ts` (plus the shrinking `env` shim until R4), calls `_start`, catches `Exit`, returns `{ stdout, stderr, exitCode }` (+ completion text via the print-completion option); no `main`, globals or memory reads |
| `tests/run.test.ts` | 737 (52 tests) | rewrite: the ≈ 30 "like the Rust host" parity tests are deleted (one renderer); keep tests for stdout / stderr / exit code through the polyfill: exactly four imports, promise ordering, uncaught and unhandled exit 1, OOM line, Date clock, `Math.random` range |

`apps/jsc-web/src/App.tsx:38-45` follows the new `RunResult` shape.
**Node** (Q9 default): `run()` uses the same polyfill in Node and the
browser (in-memory capture); `node:wasi` is the documented way to run a
`jsc build` artifact, not the package's `run()` path.

### 4.3 Runners

| runner | change |
|---|---|
| `run/harness.rs` → WASI runner | `run(wasm, Sinks, fuel: Option<u64>) -> Result<RunOutcome { code: i32, fuel: u64 }>`: engine (+ fuel), `Linker` with the four hand-written preview-1 imports (Q8), stdout/stderr = real fds or in-memory buffers, instantiate, call `_start`, map the `proc_exit` exit error to `code`, propagate other traps as `Err`. Until R4 the linker additionally `func_wrap`s the remaining `env` imports. No `unwrap`/`expect`/`panic` (workspace lints). |
| CLI `crates/jsc` | `jsc FILE` / `-e`: compile, run with real fds, process exit = module code; trap → stderr + exit 1; compile errors exit 2. `-p`: compile with `print_completion` (§4.6); `main.rs:111-114` deleted. `jsc build FILE -o OUT`: unchanged; the artifact runs under `wasmtime run` and `node:wasi` from R4 on. |
| test262 `crates/jsc-test262` | none: process-based (`exec.rs:34-85`), no `jsc-core` dependency; with §2.3 preserved `exec.rs`, `outcome.rs`, `assemble.rs` are unchanged. `$262` stays absent. |
| opcount `crates/jsc-core/tests/opcount.rs` | `measure` (`opcount.rs:61-114`) uses the WASI runner with fuel and an in-memory stdout. `host_calls` (`opcount.rs:55-60`) keeps counting calls to function indices below the import count (WASI calls after R4). Fuel rises (formatting, parsing and the promise engine were free host time; e.g. the `fib` row reason "host time is not fuel"): one re-baseline commit per stage with a reason per row. `push_fuel_is_linear` (`opcount.rs:228`) unaffected. |
| `crates/jsc-wasm` | depends on `jsc-core` with `default-features = false`, exposes `compile(src)` (`crates/jsc-wasm/src/lib.rs:27-37`), never links `run`. Keeps working; gains `compile(src, { printCompletion })` (R1) for the playground result panel; grows by the emitter code and tables (§3.7). |

### 4.4 CI (`.github/workflows`)

| workflow | change |
|---|---|
| `ci.yml` `test` | unchanged commands; suites run through the hand-written WASI imports. New step (R3): `cargo run -p unicode-tables -- --check`. |
| `ci.yml` `test262` | unchanged |
| `web.yml` | unchanged steps (vitest suite rewritten). New smoke step (R4): `jsc build` one program and run it under `node:wasi` to prove a third host. |
| `release.yml` | unchanged |

Per the standing rule, implementation L3s do not run tests locally; L1 runs
these gates in GitHub Actions.

### 4.5 Deleted LOC

Rust host semantics ≈ 3,060 LOC (`host.rs` 782, `promise.rs` 784,
`value.rs` 1092, `harness.rs` 408 → ≈ 130, `abi.rs` ≈ −260); TypeScript
≈ 2,180 (`shim.ts` 863, `value.ts` 1014, `abi.ts` 302). Imports 7 → 4,
exports 12 (`main`, `memory`, 3 globals, 7 helper functions incl. the 2 dead) → 2.

### 4.6 Tests: deleted vs rewritten

**Completion value (Q2).** ≈ 2,800 assertion calls compare the decoded
completion `Value`. New emit option `CompileOptions::print_completion`:
`_start` renders the completion value with the guest renderer to fd 1 after
the drain (§2.4). Test helpers keep their signatures and compare text.

| suite | count | fate |
|---|---|---|
| `run/tests.rs` decoder unit tests (`decode_*`, `format_value_covers_every_arm`, `number_boxing_roundtrips`, `object_literal_roundtrips_to_heap_object`, `function_value_decode_reads_type_byte`, `heap_type_byte_*`, helpers `string_block` / `push_array`) | ≈ 15 | delete with `value.rs` |
| `run/tests.rs` language tests via `compile_and_run` | ≈ 115 | move to `crates/jsc-core/tests/` on the new helpers |
| `value.rs::tests::exponential_thresholds` | 1 | delete; re-expressed as output tests (`String(1e21)`, `String(1e-7)`, …) in R2 |
| `abi_drift_tests.rs::abi_ts_matches_rust_abi` | 1 | delete (R4) |
| `abi_drift_tests.rs` layout tests (3) | 3 | keep (`TYPE_PROMISE` layout row updated in R4) |
| `crates/jsc-core/tests/common/mod.rs` | — | rewrite on the WASI runner: `assert_output` = captured stdout; `assert_uncaught` = stderr + exit 1; `assert_rejected` = compile error, trap or non-zero exit; `assert_unhandled_rejection(src, expected)` = stderr `Uncaught (in promise) …` + exit 1; `assert_eval` / `assert_number` / `assert_string` / `assert_bool` / `assert_nan` / `assert_str` / `assert_type_str` compile with `print_completion` and compare rendered text (expected numbers formatted by a test-only JS-number formatter in `tests/common`, e.g. Rust `{}` adjusted to JS exponent rules) |
| helper call sites (`assert_string` 892, `assert_number` 743, `assert_bool` 680, `assert_output` 533, `assert_uncaught` 279, `assert_eval` 261, `assert_str` 69, `compile_and_run` 38, `assert_rejected` 35, `assert_nan` 29, `assert_type_str` 10, `compile_and_run_capturing` 5) | ≈ 3,570 | unchanged call sites; the single `assert_number(.., -0.0)` becomes `assert_bool("Object.is(x, -0)")` (text renders `-0` as `0`) |
| direct `jsc_core::run::*` users: `tests/concat_coercion.rs`, `tests/memory.rs`, `tests/opcount.rs`, `tests/unsupported.rs`, `crates/jsc/tests/typescript.rs`, `crates/jsc/src/main.rs` | 6 files | rewrite against the runner API |
| `packages/jsc/tests/run.test.ts` | 52 | rewrite (§4.2) |
| `crates/jsc/tests/**` (295), `crates/jsc-test262` unit tests | — | unchanged (any asserting a trap message verbatim is checked in R1) |
| new: `every_compiled_module_validates` | — | every module the suites compile is validated with `wasmparser` in the test helper (invalid wasm slipped through before) |

---

## 5. Memory management

Precise, non-moving, stop-the-world mark-sweep, run synchronously inside
`__alloc` (the only collection point). Non-moving is required: emitted code
keeps raw `i32` bases in locals and on the operand stack (e.g. the array
literal's store base, `emit/array.rs:119-121`), object-literal tables are
co-allocated inside the object (`emit/object.rs:38-45`, interior
addresses), and intern / props / symbol keys compare by `u32` address
(DESIGN §5.3). Consequently a raw base never needs re-deriving after a
safepoint (DESIGN §5.7 says it does — wrong); its block only needs to be
rooted while the base is live. No write barriers.

### 5.1 Memory layout

```text
[0, SS_SIZE)               shadow stack, grows down from SS_SIZE         (§5.3)
[SS_SIZE, static_end)      null guard word, FN_INFO pointer, static region (DESIGN §4.2 order),
                           WASI_SCRATCH, NUM_SCRATCH, DEC_SCRATCH, Unicode/number tables,
                           page table, free-list heads, static root list
[static_end, heap_start)   padding to a 64 KiB boundary
[heap_start, frontier)     64 KiB GC pages
```

`SS_SIZE = 1 MiB` (16 pages, `abi::SHADOW_STACK_SIZE`); `HEAP_BASE =
SS_SIZE + 8`; `FN_INFO_ADDR = SS_SIZE + 4` (today `abi.rs:310`, inside the
`0..8` null guard). Initial memory pages grow by 16; maximum stays 8192.
A stray read of address `0..8` now reads stack data instead of the zero
guard; every raw ref is tested against `0` before use (M1 reviews the
`props == 0` / `elems == 0` guards).

### 5.2 Header and mark epoch

Header byte 1 (`HEADER_GC_OFFSET`, `abi.rs:94-97`; always `0` today:
`emit/runtime.rs:90-104` zeroes the header, static blobs write `0`,
`emit/sections.rs:596`) becomes an **epoch mark**: global `$__gc_epoch`
cycles `1..=255` (never `0`); a block is marked iff `gc == $__gc_epoch`;
nothing clears marks. A single bit plus clearing would leave stale marks on
co-allocated interior tables and the static region (never swept); with an
epoch a stale mark aliases only after 255 cycles and only on an abandoned
block, which is harmless. Fresh blocks (`gc = 0`) are never marked.

### 5.3 Shadow stack, safepoints, spills

**SP and frames.** Global `$__sp` (initial `SS_SIZE`); live range
`[$__sp, SS_SIZE)` scanned as a flat jsval array; frames have no header or
descriptor. A function with `N > 0` root slots:

```wasm
;; prologue (emitted last, once N is known)
global.get $__sp  local.tee $old  i32.const 8N  i32.sub  local.tee $fp  global.set $__sp
;; clear: N <= 4 -> N x (local.get $fp  i64.const 0  i64.store offset=8k)
;;        N >  4 -> local.get $fp  i32.const 0  i32.const 8N  memory.fill
;; epilogue before every exit
local.get $old  global.set $__sp
```

Every slot always holds a valid jsval (`0` = number `+0` after clearing, a
written-through jsval, or `HEAP | base` for a rooted raw base). Clearing is
mandatory (a stale slot could name a block freed later).

**Overflow (Q10 default):** no explicit check; `fp = $__sp - 8N` wraps below
`0` to an address above the 512 MiB maximum and the first slot store traps
(same failure class as today's native stack overflow). A catchable
`RangeError` costs ≈ 4 ops per frame push plus a static error object.

**Function builder (M1).** A wrapper around `wasm_encoder::Function` buffers
instructions, owns root-slot and depth-indexed temp-slot allocation,
intercepts every `Instruction::Return` and the outermost `End` to insert the
epilogue, and emits the prologue with the final `N`. User returns funnel
through `emit/function.rs::emit_throw_return` (`:173`), `emit_return_transfer`
(`:244`) and the generator state machine (`emit/generator.rs:499,644,657`);
helpers and builtins contain ≈ 240 further `Return`s — the builder makes the
audit mechanical. Exceptions unwind by `br` (SP unchanged) or by
`emit_throw_return` (covered); a trap kills the instance.

**Safepoints:** every `call $__alloc`; every `call` / `call_indirect` whose
callee may reach `__alloc` — all user functions, builtins, `call_indirect`s,
and helpers outside a **non-allocating leaf set** computed from a static
helper call graph (each helper emitter declares its callees). Expected
leaves: `__str_eq`, `__str_cmp`, `__str_hash`, `__intern_find`,
`__index_value`, `__prop_find`, `__same_value_zero`, `__map_find`,
`__box_f64`, `__protos_indexless`, `__private_find`, `__json_ws`,
`__str_exotic_index`, plus the new `__ryu_d2d`, `__umul128`, libm helpers
and Unicode lookups. WASI imports never call back, so they are not
safepoints.

**Spill set** (all IR locals are `i64` jsvals, `emit/function.rs:45-56,128-139`):

| class | where | rooted how |
|---|---|---|
| IR locals (`let`/`const`/`var`, temps, `%penv`, iterator records) | `ir.rs:2347-2365,2470-2478` | write-through on every `local.set`/`tee` (+3 ops) |
| parameters `a0..a_{K-1}`, `env` | locals `0..K+1` (DESIGN §4.2) | stored in the prologue iff read after a safepoint; `argc` never |
| `this` snapshot | DESIGN §4.3 | write-through |
| emitter `i64` scratch | `scratch+0..+2, +4..+6, +13..+20, +22, +23` (`emit/function.rs:15-44`) | write-through; **`Date` scratch `+24..+29` declared raw, never rooted** |
| emitter `i32` bases / keys | `+3`, `+7`, `+8`, `+11`, `+12`, `+21` (`emit/object.rs:31`, `emit/generator.rs:43`) | write-through of `HEAP | base` (+6); `+9`, `+10` are integers |
| builtin `Frame` locals | `emit/builtins/mod.rs:127-151` | `i64` write-through; `i32` only via `frame.base(k)`; `f64` never |
| hand-written helper locals | each `Function::new` | declared jsval / base / raw; **no default** (Q11: the builder rejects an undeclared local) |
| operand-stack temporaries | call args (`emit/expr.rs:808`), binary operands, `dst` under element expressions (`emit/array.rs:162-167`, `emit/object.rs:76-86`) | temp slot: `local.set $t; local.get $fp; local.get $t; i64.store off; local.get $t` (+4; +7 for `i32`) |

Keys: `__to_key(v, 1)` may return an interned block reachable only from the
weak intern table, so any `i32` key held across a safepoint is rooted as a
base.

**Spill elision (provable, per function):** (1) a body with no safepoint
gets no frame (single-threaded, no collection can start); (2) **NoRoot**
locals — every definition is a number / boolean / `null` / `undefined` /
`HOLE` / static-block pointer: numeric operator results (no BigInt),
comparisons, `!`, `in`, `instanceof`, `delete`, `typeof`, `a + b` with both
operands NoRoot and neither possibly a string, reads of NoRoot locals;
fixpoint; captured (boxed) locals are never NoRoot. The same predicate
elides temp slots (`n - 1` in `fib`). Helpers shrink-wrap (frame pushed only
on the slow path, e.g. `__array_push` growth); the builder enforces that a
`Return` after a push restores SP and one before does not.

### 5.4 Roots and per-TYPE tracing table

**Roots:**

| root | treatment |
|---|---|
| shadow stack `[$__sp, SS_SIZE)` | jsvals |
| `$exc_value` (`emit/sections.rs:496`), `$cf_value` (`:503`; loop-target codes are denormal numbers), `$__this`, `$__new_target`, `$__spread_args` (`:507-513,529-547`) | jsvals |
| module cells incl. tagged-template sites (`emit/sections.rs:551-557`, `emit/function.rs:60-77`, `codegen/expr.rs:1085-1095`) | jsvals (`TYPE_CELL`; template objects live forever, as `GetTemplateObject` requires) |
| `$__json_data/_pos/_len` (`emit/sections.rs:515-520`) | not roots (interior address valid during `__json_parse`, whose `s` is rooted, `emit/json_parse.rs:785-803`) |
| intern table (`$__intern_base/_cap/_count`, `emit/props.rs:505-540`) | **weak** (§5.6); the table block itself marked via `$__intern_base - 8` |
| static region | never swept; the compiler emits a **static root list** `[count: u32][addr: u32 × count]` of every static object-family block (realm intrinsics, static builtin-method and user top-level `TYPE_FUNCTION` blocks); each cycle traces their fields without marking them. Static strings / symbols / accessors are immutable and skipped. The `Symbol.for` registry (`emit/realm.rs:66-69`) is in the list (strong, per spec). |
| promise engine (R4) | `$__job_head`, `$__job_tail`, `$__rejected_head` (raw refs) |
| `$__out_buf` (R1) | raw ref |
| weak collections (M3) | `$__kept_objects` (strong array), `$__fin_jobs` (raw ref, traced as a registry list) |
| host-held handles | **none after R4.** Today `PromiseEngine` (`run/promise.rs:36-98`) holds jsvals and bases (queue `callback`/`value`/`result`/`async_gen`, reactions, `rejected`, `handled`) and both hosts hold fresh `__new_string`/`__new_array` bases while filling them — invisible to a guest collector. **M2 must not land before R4.** |
| `$__intrinsics_dirty`, `$exc_pending`, `$cf_kind`, `$__heap_ptr` | integers |

**Tracing.** Marking a block: static → not marked (traced only from the
static list); `gc == epoch` → stop; else write the epoch and push on the
mark stack. A **jsval slot** is followed iff `bits & 0xFFFF_0000_0000_0000 ==
0x7FFD_0000_0000_0000` and the low 32 bits lie in the heap or static region;
a **raw ref** is a `u32` base, `0` = none.

| type | kind | pointer fields (offset from base) | variable part |
|---|---|---|---|
| `0` `TYPE_STRING` | string | none (**leaf**) | data `8 + (len << two_byte)` |
| `1` `TYPE_ARRAY` | array handle | raw `props @4`, jsval `proto @8`, raw `elems @20` | — |
| `2` `TYPE_OBJECT` class `0` ordinary / `1` error / `2` arguments / `6` global | object | prefix only (an inline table at `+16` is reached through `props`) | — |
| `2` class `3`/`4`/`5`/`7` wrappers | | prefix + jsval `@16` | — |
| `2` class `8` iterator kinds `0..8` | | prefix + jsval `@16` (`@24` u32, `@28` u8 integers) | — |
| `2` class `8` kind `16` (`Iterator.from` wrapper) | | prefix + jsvals `@16`, `@32`, `@40` | — |
| `2` class `8` kinds `17..21` (helpers) | | prefix + jsvals `@16`, `@32`, `@40`, `@56`, `@64`, `@72` (`@48` f64) | — |
| `3` `TYPE_FUNCTION` | function / closure / bound / settler | prefix + jsval `env @24` | — |
| `4` `TYPE_ENV` | env, generator frame, async-gen request record, promise reaction/job record (R4) | jsval slots `@8 + 8k`, `k < count @4` | `count` slots |
| `5` `TYPE_CELL` | cell | jsval `@4` (unaligned; `HOLE` in TDZ) | — |
| `6` `TYPE_MAP` / `7` `TYPE_SET` | handle | prefix + jsval `entries @24` (backing array; tombstones `HOLE`/`undefined`) | — |
| `8` `TYPE_PROMISE` | promise (40 B, §3.4) | prefix + raw `reactions @20` + jsval `value @24` + raw `rejected_next @36` | — |
| `9` `TYPE_GENERATOR` | generator | prefix + jsval `env @32` + raw `queue @40`, `queue_tail @44` | — |
| `10` `TYPE_DATE` | Date | prefix (`ms @16` f64) | — |
| `11` `TYPE_ACCESSOR` | accessor | jsvals `getter @8`, `setter @16` | — |
| `12` `TYPE_PROPS` | property table | per entry `k < count @4` at `16 + 16k`: raw key `@+0` (`0` = tombstone), jsval `@+8` | `cap` entries |
| `13` `TYPE_ELEMS` | element store | jsval slots `@8 + 8i` for **all** `i < cap @4` | `cap` slots |
| `14` `TYPE_SYMBOL` | symbol | jsval `description @8` | — |
| `15` `TYPE_WEAKMAP` *new (M3)* | WeakMap | prefix + raw `entries @20` → `TYPE_ELEMS` of pairs, **ephemeron** (§6.2); `@16` collector scratch | — |
| `16` `TYPE_WEAKSET` *new (M3)* | WeakSet | as `TYPE_WEAKMAP`, keys only | — |
| `17` `TYPE_WEAKREF` *new (M3)* | WeakRef | prefix; jsval `target @24` **weak**; `@16` weak-list link | — |
| `18` `TYPE_FINREG` *new (M3)* | FinalizationRegistry | prefix + jsval `cleanup @24` (strong) + raw `cells @20` → triples `[target weak, held strong, token weak]`; `@16` link; `@32` flags; raw `job_next @36` | — |
| `19` `TYPE_RAW` *new (M1)* | internal raw buffer | none (**leaf**) | `len @4` bytes from `@8` |
| `0xFF` `TYPE_FREE` *new (M2)* | free slot | never traced (`next_free @4`) | — |

- `TYPE_ELEMS` traces `0..cap`, not `0..len`: `pop` does not clear the
  vacated slot (`emit/array.rs:344-377`), so tracing by `len` could dangle
  across cycles; tracing `cap` can only retain. M2 also writes `HOLE` on
  `pop` / `shift` / `length` truncation.
- `TYPE_PROPS` traces `0..count` (entries past `count` are zero).
- Inline tables (`emit/object.rs:38-45,58-75`; errors `ir.rs:2498-2510`) are
  traced through `props`, freed only with the object; after growth the
  abandoned inline table is dead space.
- Partially initialized blocks are reachable (generator frame cells
  `emit/generator.rs:51-83`, `__new_array` writes `len` first
  `emit/runtime.rs:343-360`); safe because every unwritten field is `0`
  (§5.7 zeroing).
- Mark stack: `u32` bases in a large span taken directly from the page pool
  (never `__alloc`), doubled on overflow; if pages run out, set an overflow
  flag and finish with linear rescans of all pages until no new mark.

### 5.5 Pre-existing GC blockers (fixed in M1)

| # | blocker | fix |
|---|---|---|
| F1 | headerless `__alloc` blocks: the intern growth copy (`emit/props.rs:525-526`, `cap·8` bytes, slot 0 overlapping the header) and the `Math.sumPrecise` accumulator (`emit/builtins/number.rs:446-447`, 576 bytes, held in an `i32` across user-code iterator steps) | both become `TYPE_RAW` blocks (data at `+8`); `$__intern_base` points at `block + 8`; the accumulator base is a rooted base local |
| F2 | async-generator request record `next` stored as a zero-extended raw `u32` in a `TYPE_ENV` slot (`emit/builtins/generator.rs:505-519`, `I64ExtendI32U` at `:517`), invisible to env tracing | store `HEAP | record` (`I64Or` `HEAP_BITS`); readers already `I32WrapI64` (`:395`); the only raw-`u32`-into-`i64` store in `emit/` |

### 5.6 Allocator and collection policy

- **Page table**: one byte per wasm page (8 KiB static): `0` not heap,
  `1..=19` small page of class `c`, `0xFE` large head, `0xFD` large tail,
  `0xFC` free page.
- **Small page**: 16-byte header (`class`, `bump`, `live`, reserved), slots of
  `size(c)` from `page + 16`. Classes (19): `16..256` by 16, then `512`,
  `1024`, `2048`; index `size <= 256 → (size + 15) >> 4`, else
  `17 + ceil_log2(size) - 9`. Constant-size sites call a per-class entry
  with the precomputed class.
- **Large span**: `k` pages for requests `> 2048`, 16-byte pre-header
  (`pages`, `next_large`), block at `page + 16`.
- **Free slot**: `[0xFF][0][0][0][next_free @4]`, per-class lists headed in a
  static `u32[19]`. Allocation from a free list zeroes the whole slot
  (`memory.fill`); bump / fresh pages are already zero; reused free pages are
  zeroed. Callers keep writing only the fields they set
  (`emit/value.rs:10-25`).
- **Sweep: eager** (Q4): walk every small page to `bump`, free unmarked
  non-free slots, rebuild lists page by page, return empty pages to the pool;
  free unmarked large spans.
- **`$__heap_ptr`** becomes the page frontier (end of the highest page ever
  obtained); `memory.grow` only when the frontier needs pages (doubling as
  today, `emit/runtime.rs:55-87`).
- **Intern table (weak):** after marking, slots holding an unmarked heap
  string become tombstone `1`; `__intern_slot`/`__intern_find` probe past
  `1`; insertion does not reuse tombstones; `$__intern_count` keeps counting
  them; growth (`emit/props.rs:516-540`) re-inserts only slots `> 1`.

**When to collect** (globals `$__gc_debt`, `$__gc_live`, `$__gc_epoch`,
`$__gc_busy`):

| step in `__alloc(size)` | rule |
|---|---|
| 1 budget | `debt -= size(c)`; `debt < 0` and not busy → collect, then `debt = max(GC_MIN_BUDGET, live)` (heap ≈ 2 × live; `GC_MIN_BUDGET = 4 MiB`, Q13) |
| 2 take | class free list → current page bump → free-page pool → grow |
| 3 grow | `memory.grow` (doubling) |
| 4 collect-before-OOM | grow failed and no collection for this request → collect, retry 2 |
| 5 OOM | §5.8 |

A collection never runs user code (weak cleanup only enqueues), never calls
`__alloc`, is not re-entrant. Collection order: mark roots → drain mark stack
→ ephemeron fixpoint → clear WeakMaps/WeakSets → clear WeakRefs → process
registries → tombstone the intern table → sweep.

Test-only compile option `gc_stress` (budget `0`: collect at every
allocation; not exposed by the CLI, Q20).

### 5.7 `__alloc` growth, 512 MiB cap, fast path

The 512 MiB / 8192-page maximum stays. Fast path at constant-size sites: pop
the class list head (≈ 6 ops), zero (4), decrement and test the debt (5);
else bump in the current page. Today's fast path is ≈ 25 instructions
(`emit/runtime.rs:30-104`); the new one ≈ 20-30.

### 5.8 Out of memory: catchable `RangeError` (M2)

- **Error object.** A static preallocated `CLASS_ERROR` block
  `%OutOfMemoryError%` (proto `%RangeError.prototype%`, own non-enumerable
  `message = "out of memory"`, `stack = "RangeError: out of memory"`); every
  OOM throws the same object (Q15).
- **Reserve.** `GC_RESERVE = 256 KiB` of pages held back. On OOM `__alloc`
  sets `$exc_value = %OutOfMemoryError%`, `$exc_pending = 1`, and returns a
  zeroed block from the reserve for requests `<= 2048` (so the caller's
  unconditional field writes stay in bounds and its next exception check
  unwinds), or `0` for larger variable-size requests. The variable-size
  allocators (`emit/runtime.rs::emit_alloc_str`, `emit_new_elems`, props
  growth `emit/props.rs:812-825`, intern growth, runtime-count `TYPE_ENV`,
  `TYPE_RAW`) check `0` and return a valid dummy (empty string / no store /
  no growth) with the exception pending.
- **Propagation.** Allocation becomes a throwing operation: in user code
  every call of an allocating helper is followed by
  `emit/function.rs::emit_propagate_if_pending`; the pure allocators
  (`__concat`, `__new_array`, `__str_index`, `__index_string`,
  `__array_reserve`, `__array_push`) gain the check. M2 audits that no
  dummy value reaches user-visible state before a check.
- **Reserve exhausted while OOM is pending** → write the static
  `Uncaught RangeError: out of memory\n` line to fd 2 and `proc_exit(1)`
  (the R1 OOM path, §2.3). The next successful collection re-reserves.
- **Contradiction resolved:** the R-T2 note kept a sticky `HEAP_PTR_OOM` +
  `unreachable` "reported by the run stage" for this case; after R1 no host
  inspects guest globals, so the guest reports and exits itself. The
  `__heap_ptr` export, `HEAP_PTR_OOM` and `oom_guard` are deleted in R1.

### 5.9 Cost model

Per executed event (fuel units, estimated from the sequences above):

| event | ops |
|---|---|
| frame push + pop, `N <= 4` (stores) | 6 + 3N + 2 |
| frame push + pop, `N > 4` (`memory.fill`) | 12 |
| write-through `i64` / `i32` base | 3 / 6 |
| operand temp root `i64` / `i32` | 4 / 7 |
| allocation fast path vs today | −5 … +5 (+4 on free-list reuse) |
| propagate check after a pure allocator | 2 |
| a collection | ≈ 8 per live jsval slot + ≈ 6 per swept slot |

No corpus row reaches `GC_MIN_BUDGET` (largest `push_1e5` peaks near
2 MiB), so the rows carry no collection cost. M1+M2 estimates (unmeasured;
fuel at `c34cd36`, `crates/jsc-core/tests/opcount/snapshot.tsv`), excluding
the R-stage cost of guest formatting (§7):

| row | fuel today | dominant new costs | est. Δ |
|---|---|---|---|
| `fib` | 3,601,020 | 21,891 calls × ≈ 21 (2 slots) | +460 k (+13 %) |
| `push_1e5` | 20,749,986 | `i` NoRoot; `__array_push` shrink-wrapped; 1e5 × check 2 | +0.2 M (+1 %) |
| `array_hofs` | 22,907 | frame-free closures; rooted HOF bases | +1.5 k (+7 %) |
| `class_methods` | 58,552 | ≈ 45 calls × ≈ 20 | +1.5 k (+3 %) |
| `console_log` | 1,503 | 11 prints, no user functions | +50 (+3 %) |
| `for_in` | 19,683 | 20 iterations, rooted `keys` | +0.3 k (+2 %) |
| `for_of` | 157,813 | 100 SM steps, 120 pushes, 20 `Map.set` | +5 k (+3 %) |
| `generators` | 163,734 | 211 `next()` × ≈ 15 | +5 k (+3 %) |
| `json` | 30,256 | ≈ 20 `__json_value` frames | +1 k (+3 %) |
| `map_set` | 45,517 | 40 `set`/`add`, 27 concats | +1.5 k (+3 %) |
| `math` | 54,375 | NoRoot locals, no calls | ≈ 0 |
| `object_reads` | 18,607 | NoRoot locals | ≈ 0 |
| `spread` | 112,920 | 100 SM steps, 300 appends | +4 k (+4 %) |
| `string_methods` | 45,060 | 50 allocating calls, rooted `out` | +1 k (+2 %) |

**Code size:** per framed function prologue 6 + clear ≤ 12 + 2 per return,
+3 per rooted write, +4 per temp root: `user_ops` +5-15 % per row (`fib`
346 → ≈ 380); helpers and builtins (`Builtin::ALL` 142 entries + DESIGN §6.2
helpers) +6-10 k instructions, ≈ +4-8 % of a module's code section.

**No-GC fast path verdict: rejected program-wide.** "Never collect, never
spill" needs a static bound on total allocation (no allocation in loops,
recursion, `call_indirect`, callbacks, microtasks; constant sizes everywhere
— builtins allocate data-dependently: `repeat`, `join`, `JSON.parse`,
`Array(n)`, number formatting). Only trivial programs qualify and it would
be a second source of truth. **Adopted instead:** the per-function elisions
of §5.3 (frame-free safepoint-free functions, NoRoot locals, shrink-wrapped
helpers), which keep `math`, `object_reads`, `push_1e5` at ≈ 0 %.

---

## 6. Weak collections

### 6.1 Surface (ECMA-262 §24.3, §24.4, §26.1, §26.2)

New intrinsics appended to the realm registry (`emit/realm.rs`):
`%WeakMap%` + prototype (`delete`, `get`, `has`, `set`, `@@toStringTag`),
`%WeakSet%` + prototype (`add`, `delete`, `has`, `@@toStringTag`),
`%WeakRef%` + prototype (`deref`, `@@toStringTag`),
`%FinalizationRegistry%` + prototype (`register`, `unregister`,
`@@toStringTag`); global entries for the four constructors. Constructors are
`CONSTRUCTOR | SELF_ALLOC` (DESIGN §5.5), throw `TypeError` without `new`;
`WeakMap`/`WeakSet` take an iterable through the `Map`/`Set` adder protocol
(`emit/builtins/map_set.rs::emit_ctor`, `call_adder`). `cleanupSome` is not
added (not standard).

**`CanBeHeldWeakly(v)`**: an object-family block (the eight kinds + the four
new) or a `TYPE_SYMBOL` without `SYMBOL_FLAG_REGISTERED` (`abi.rs:623`);
well-known (static) symbols qualify and are never collected. Otherwise
`TypeError` ("Invalid value used as weak map key" / "… in weak set" /
"WeakRef: target must be an object" / "FinalizationRegistry.register:
invalid target"). This implements `symbols-as-weakmap-keys`.

### 6.2 WeakMap / WeakSet: ephemerons

Layout (32 B): prefix, `weak_next: u32 @16` (collector scratch),
`entries: u32 @20` (raw ref to a `TYPE_ELEMS` of `(key, value)` pairs),
`live: u32 @24`, pad. Lookup by identity (bit equality); `delete`
tombstones (`HOLE` key, `undefined` value); `set` reuses the first tombstone
or appends (growth like `__array_reserve`). Not iterable, so the collector
compacts freely (Q16: linear pairs).

Marking: (1) tracing the handle marks its prefix and the `entries` block,
not the pairs, and links the handle into the cycle's `weak_maps` list;
(2) after the mark stack drains, the **ephemeron fixpoint**: repeat { for
each pair whose key is marked (or static) and whose value is an unmarked
heap pointer, mark the value; drain } until a pass marks nothing;
(3) clearing: pairs with an unmarked heap key are removed, the store is
compacted in place, the tail rewritten `HOLE`/`undefined`, `live` updated.

### 6.3 WeakRef and KeepDuringJob

Layout (32 B): prefix, `weak_next @16`, pad, `target: i64 @24`. Tracing marks
the prefix only and links into `weak_refs`; after the fixpoint an unmarked
heap target becomes `undefined`. `new WeakRef(t)` and a successful
`deref()` perform `AddToKeptObjects(t)`: push onto `$__kept_objects` (an
internal strong `TYPE_ARRAY`). **`ClearKeptObjects`** runs at each microtask
checkpoint of `__drain()` (§3.4: queue empty — after the post-`main` drain
and after each top-level-`await` drain): length `0`, `HOLE` into vacated
slots (Q17). Clearing after every job would also conform but makes `deref()`
unstable within one checkpoint.

### 6.4 FinalizationRegistry

Layout (40 B): prefix, `weak_next @16`, `cells: u32 @20` (`TYPE_ELEMS` of
triples), `cleanup: i64 @24` (strong), `flags: u32 @32` (`1` = job queued),
`job_next: u32 @36`.

- `register(target, held, token)`: `CanBeHeldWeakly(target)` else
  `TypeError`; `SameValue(target, held)` → `TypeError`; `token` `undefined`
  or `CanBeHeldWeakly`; append the triple. The triple's values are rooted
  across the cells-store growth (a safepoint).
- `unregister(token)`: `CanBeHeldWeakly(token)` else `TypeError`; remove every
  cell with that token; return whether any was removed.
- Tracing marks `cleanup`, the `cells` block and every cell's `held`, not
  `target` / `token`; links into `weak_regs`.
- After WeakRef clearing: a cell with an unmarked heap `target` gets
  `target = HOLE` (empty); an unmarked heap `token` becomes `undefined`. If
  any cell emptied and `flags & 1 == 0`, set the flag and push the registry
  on the intrusive list `$__fin_jobs` via `job_next`
  (`HostEnqueueFinalizationRegistryCleanupJob`; no allocation during a
  collection).
- **Scheduling (host-agnostic):** at the `__drain()` checkpoint, after
  `ClearKeptObjects`, pop registries from `$__fin_jobs`, clear the flag, run
  `CleanupFinalizationRegistry` (remove each empty cell, call
  `cleanup(held)`); a throw is reported like an uncaught exception in a job
  (Q22). Then drain microtasks again; repeat until both queues are empty. No
  timers, no host.

### 6.5 test262 impact and feature flags

Read-only counts over `/srv/work/jsc/.tmp/test262/test` vs
`tests/test262/baseline.txt` (28,037 passing at `c34cd36`):

| directory | files | passing today | eligible today (failing: no constructor) | skipped only by weak flags | skipped by other flags |
|---|---|---|---|---|---|
| `built-ins/WeakMap` | 141 | 0 | 82 | 51 | 8 |
| `built-ins/WeakSet` | 85 | 0 | 65 | 15 | 5 |
| `built-ins/WeakRef` | 29 | 0 | 0 | 22 | 7 |
| `built-ins/FinalizationRegistry` | 47 | 0 | 0 | 40 | 7 |

Delete from `tests/test262/unsupported-features.txt`: `WeakMap`, `WeakSet`,
`WeakRef`, `FinalizationRegistry`, `symbols-as-weakmap-keys` (and the
comment line above the latter). That makes 163 tests eligible (128 above +
`built-ins/Map` 13, `Set` 7, `language/statements` 7, `built-ins/Object` 4,
`language/expressions` 3, `staging/sm` 1). **Upper bound for M3 ≈ +310**
(82 + 65 + 163, ≈ +0.9 %), plus whatever 51 untagged files mentioning weak
globals gain. All depend on the API surface; test262 cannot force a
collection without `host-gc-required` (15 tests), which **stays** (Q18).

---

## 7. Stages and L3 breakdown

### 7.1 Stage list

Rules for every stage: the emitter change, its runner/test changes and the
deletion of every host / shim path it supersedes land together; no stage
stubs a semantic (an import is removed only when the guest replacement is
complete); each stage ends with an opcount re-baseline commit (reason per
row) and the CI gates green in GitHub Actions (L1 runs them; no local test
runs). Test names below are written first (failing) by the stage's first
L3.

| stage | goal | main files | named tests (failing first) | interim state (main green) | est. LOC | test262 Δ | risks |
|---|---|---|---|---|---|---|---|
| **R1** WASI shell + guest output | `_start` command shape; imports `fd_write`, `proc_exit` (+ `clock_time_get`, `random_get` declared now so the import section changes once); guest renderer (§3.1), output buffer + UTF-8 writer, `__report`, `_start` epilogue (§2.4); OOM static line + `proc_exit` (§2.3); `print_completion` option; WASI runner, CLI, test helpers, opcount, `wasi.ts` + `index.ts`; `jsc-wasm` `compile(src, opts)`. Delete `print`, `main` export, `__heap_ptr`, `HEAP_PTR_OOM`, `oom_guard`, `__alloc`/`__new_array` exports, `format_value`/`value.ts` formatting | `emit/sections.rs`, `emit/mod.rs`, `emit/function.rs`, `emit/expr.rs` (console), new `emit/render.rs`, `emit/runtime.rs` (`__alloc` OOM), `abi.rs`, `run/harness.rs`, `run/value.rs`, `crates/jsc/src/main.rs`, `tests/common/mod.rs`, `tests/opcount.rs`, `packages/jsc/src/{index,wasi,shim,value}.ts`, `crates/jsc-wasm/src/lib.rs`, `deny.toml` | `module_imports_only_wasi_and_env_remainder`, `start_is_only_entry_export`, `render_parity_every_kind` (table §3.1, one assertion per row), `render_cycles_ref_numbering`, `render_lone_surrogate_as_fffd`, `uncaught_goes_to_fd2_exit1`, `unhandled_rejection_after_drain_exit1`, `oom_static_line_exit1`, `print_completion_matches_cli_p`, `every_compiled_module_validates`, vitest `links_wasi_polyfill` | modules import 4 WASI + 6 `env` (`__convert`, `__method`, `__promise`, `__then`, `__now`, `__parse_int`) until R2; exports `_start`, `memory` + the host-needed `__new_string`, `__new_promise`, `__promise_self_error`, `__invoke1`, `__async_step`, `__exc_*`; temporary `__promise` op `8` DRAIN returns the first unhandled promise (or `0`) so the guest epilogue renders it — deleted in R4 | +2,600-3,300 / −1,600 | 0 | renderer parity across ≈ 3,570 assertions; helper rewrite touches every suite |
| **R2** numbers, clock, random | §3.2, §3.3, §3.5, §3.6; delete `__convert`, `__parse_int`, `__now`, `__method` 17/40/41/42, `MATH_FN_RANDOM`, the matching `host.rs` / `value.rs::format_number` / shim code | new `emit/dtoa.rs`, `emit/strtod.rs`, `emit/convert.rs`, `emit/numeric.rs`, `emit/builtins/number.rs`, `emit/object.rs` (clock), `emit/json_parse.rs`, `emit/runtime.rs` (`emit_json_str`, `emit_loose_eq`), `abi.rs`, `run/host.rs`, `shim.ts` | `number_to_string_shortest_roundtrip` (sample incl. `5e-324`, `1.7976931348623157e308`, `0.1+0.2`, `1e21`, `1e-7`, `123e-20`), `exponential_thresholds_output`, `to_fixed_exact_ties`, `to_precision_and_exponential_exact`, `radix_to_string_matches_v8`, `string_to_number_slow_path` (≥ 20 digits, `2.2250738585072011e-308`), `parse_int_power_of_two_radix_exact`, `parse_float_prefix`, `date_now_from_wasi_clock`, `math_random_in_unit_interval` | `env` = `__method` (0,1,4,5,22,23,43), `__promise`, `__then` | +1,700-2,400 / −700 | 0 … +5 | dtoa/strtod bugs are silent wrong digits → large differential sample vs Rust in tests |
| **R3** Unicode, libm, search | §3.7, §3.8, §3.9; `tools/unicode-tables` + generated `emit/unicode_data.rs`; delete `__method` entirely, `host.rs`, `value.rs` (last users), `icu_normalizer` from `jsc-core` | new `emit/libm.rs`, `emit/unicode.rs`, `emit/unicode_data.rs`, `tools/unicode-tables/**`, `emit/builtins/string.rs`, `emit/numeric.rs`, `emit/builtins/array.rs`, `emit/expr.rs` (`emit_proto_guard`), `Cargo.toml`s, `ci.yml` (`--check` step), `shim.ts`, `value.ts` | `case_map_special_casing_and_final_sigma`, `case_map_lone_surrogate_passthrough`, `normalize_all_forms_conformance_sample` (lines from `NormalizationTest.txt` embedded as a fixture), `normalize_invalid_form_range_error`, `libm_matches_std_sample` (≥ 1,000 inputs per function, ≤ 1 ulp, exact on special values), `unicode_tables_within_budget` (data ≤ 96 KiB), `index_of_fast_path_guest_scan`, `locale_compare_code_unit_order` | `env` = `__promise`, `__then` only | +4,000-5,400 / −1,900 (+ ≈ 74 KiB tables) | 0 (± libm last-ulp) | module size; Unicode version skew std vs icu; libm port volume |
| **R4** guest promises + final deletion | §3.4; delete `__promise`, `__then`, the `env` module, `run/promise.rs`, `shim.ts`, `abi.ts`, `value.ts`, remaining exports; final runners; `web.yml` `node:wasi` smoke step | new `emit/promise_engine.rs`, `emit/expr.rs`, `emit/runtime.rs` (settlers, `__invoke1`, `__async_step`), `emit/builtins/{promise,generator,async_iter,array}.rs`, `abi.rs` (`TYPE_PROMISE` 40 B, flags), `run/**`, `packages/jsc/**`, `abi_drift_tests.rs`, `.github/workflows/web.yml` | `module_imports_exactly_four_wasi`, `exports_exactly_start_and_memory`, `microtask_order_matches_spec` (then/await interleavings), `thenable_assimilated_per_spec` (`await` / `Promise.resolve` / `resolve(t)`, throwing `then` getter, once-only resolvers), `unhandled_rejection_creation_order`, `late_catch_counts_as_handled`, `promise_self_resolution_type_error`, `top_level_await_drains`, `async_generator_queue_order`, `long_then_chain_no_stack_growth` (1e5 links), `runs_under_node_wasi` (CI smoke) | final module shape | +1,100-1,400 / −2,900 | 0 | job ordering regressions (test262 async suite is the net) |
| **M1** shadow stack (inert) | §5.1-§5.3, F1/F2 (§5.5), `TYPE_RAW`, function builder, NoRoot analysis, root declarations everywhere | `emit/mod.rs`, new `emit/builder.rs`, new `emit/roots.rs`, `emit/function.rs`, `emit/expr.rs`, `emit/object.rs`, `emit/array.rs`, `emit/generator.rs`, `emit/stmt.rs`, `emit/builtins/*.rs`, `emit/runtime.rs`, `emit/props.rs`, `emit/props/descriptor.rs`, `emit/json_parse.rs`, `emit/convert.rs`, `emit/numeric.rs`, `emit/sections.rs`, `abi.rs` | `shadow_sp_restored_after_return`, `shadow_sp_restored_after_uncaught_throw`, `shadow_sp_restored_after_caught_throw`, `leaf_function_has_no_frame`, `noroot_loop_counter_not_spilled`, `frame_slots_cleared_on_entry`, `async_gen_request_next_is_heap_jsval` (F2), `intern_growth_block_is_type_raw` (F1), `builder_rejects_undeclared_local`, `deep_recursion_traps` | always on, no collector: semantics unchanged | 1,500-2,200 | 0 | a missed root is silent until M2 (mitigated by `gc_stress` in M2 and the no-default declaration rule) |
| **M2** collector | §5.4, §5.6-§5.8: `__alloc` rewrite, pages, classes, free lists, mark/trace/sweep in `emit/gc.rs`, intern tombstones, static root list, `%OutOfMemoryError%`, reserve, OOM propagation, `HOLE` on pop/shift, `gc_stress` | `emit/runtime.rs`, new `emit/gc.rs`, `emit/props.rs`, `emit/realm.rs`, `emit/sections.rs`, `emit/array.rs`, `abi.rs`, allocator call sites | `gc_stress_run_suite` (every run test under `gc_stress`, same output), `gc_reclaims_churn` (≈ 2 GiB allocated, small live set), `gc_oom_is_catchable_range_error`, `gc_keeps_operand_stack_temps`, `gc_keeps_partially_built_literals`, `gc_generator_frame_survives`, `gc_async_gen_queue_survives`, `gc_promise_reactions_survive`, `gc_static_proto_heap_props_survive`, `gc_intern_tombstones_probe`, `gc_map_iterator_survives`, `gc_free_list_reuse_zeroes`; `push_fuel_is_linear` holds | collection enabled; any test262 change is a root bug | 1,800-2,600 | 0 | missed roots in helpers (CI shards test262 under `gc_stress` once); OOM-propagation audit; pauses on large heaps |
| **M3** weak collections | §6; renderer forms; drop five feature flags | new `emit/builtins/weak.rs`, `emit/realm.rs`, `emit/gc.rs`, `emit/promise_engine.rs` (checkpoint hooks), `emit/render.rs`, `abi.rs`, `tests/test262/unsupported-features.txt` | `weakmap_basic_api`, `weakmap_symbol_keys`, `weakmap_entry_dropped_after_key_unreachable` (`gc_stress`), `ephemeron_chain_fixpoint`, `weakref_cleared_after_checkpoint`, `weakref_kept_during_job`, `finreg_cleanup_runs_after_drain`, `finreg_unregister_token`, `finreg_target_equals_held_throws` | one merge; before it the four globals stay absent (runtime `ReferenceError`, as today) | 1,300-1,800 | up to +310 | cleanup-job ordering vs microtasks; collection inside `register` |

### 7.2 L3 breakdown (per stage, in merge order)

| stage | L3s |
|---|---|
| R1 | R1-T1 WASI runner + `print_completion` + test helpers + `every_compiled_module_validates` · R1-T2 guest renderer + writer + `_start` epilogue (on top of T1; T1 alone is never merged to main because the old `print` path and the new helpers must switch together) · R1-T3 TS `wasi.ts` / `index.ts` / vitest rewrite, `jsc-wasm` option, `App.tsx` (parallel to T2) · R1-T4 opcount re-baseline + DESIGN.md update. L1 merges R1 only after T1-T4 are all in the L2 branch. |
| R2 | R2-T1 dtoa + exact decimal + radix · R2-T2 strtod + parseInt/parseFloat · R2-T3 clock + random + deletions + re-baseline (T1, T2 parallel) |
| R3 | R3-T1 unicode-tables tool + case mapping · R3-T2 normalization · R3-T3 libm port · R3-T4 search/localeCompare + `host.rs`/`value.rs`/`icu_normalizer` deletion + re-baseline (T1-T3 parallel; T2 after T1's tool lands) |
| R4 | R4-T1 promise engine + layout · R4-T2 async drivers / generators / `Array.fromAsync` on the engine · R4-T3 final deletions, runners, `web.yml` smoke, re-baseline |
| M1 | M1-T1 builder + roots analysis + `TYPE_RAW`/F1/F2 + layout move · M1-T2 declaration sweep over builtins · M1-T3 declaration sweep over helpers + re-baseline |
| M2 | M2-T1 allocator + pages + sweep · M2-T2 mark/trace + static root list + intern weak · M2-T3 OOM reserve + propagation audit + `gc_stress` suite |
| M3 | M3-T1 WeakMap/WeakSet · M3-T2 WeakRef/FinalizationRegistry + checkpoint · M3-T3 flags + renderer + DESIGN |

### 7.3 Dependency DAG and parallelism

```text
R1 ──► R2 ──┐
   └─► R3 ──┴─► R4 ──┐
M1 ─────────────────┴─► M2 ──► M3
```

- R2 and R3 can run as **parallel L2s** after R1 (disjoint main files; shared
  hot files `abi.rs`, `emit/sections.rs`, `emit/numeric.rs`, `shim.ts`;
  merge order R2 → R3, R3 rebases).
- M1 has no semantic dependency and can run as a **parallel L2** from R1's
  merge on (Q21 default). Hot files shared with every R stage:
  `emit/mod.rs`, `emit/sections.rs`, `emit/runtime.rs`,
  `emit/builtins/*.rs`, `emit/realm.rs`, `abi.rs`, `DESIGN.md`. Rule: M1
  merges between two R-stage merges, never interleaved within one; the
  branch merged second rebases and adds root declarations to new code (the
  builder rejects undeclared locals, so missing ones fail to compile).
- **M2 hard-depends on R4** (host promise engine and host-allocating ops
  hold roots invisible to the guest, §5.4) **and on M1**.
- M3 depends on M2 and R4's `__drain()` checkpoint.
- Unhandled-rejection order: R4 keeps creation order via `seq` (§3.4);
  required before M2 because free-list reuse breaks base order.

### 7.4 Fuel expectations of the R stages (unmeasured)

Formatting, parsing and the promise engine become metered: every printed
number costs ≈ 200-400 fuel (Ryu + layout), every printed piece ≈ 50-150
(render + UTF-8), every `console.log` one `fd_write`. Rows dominated by
output (`console_log`, 1,503) may grow several-fold; compute-heavy rows
(`fib`, `push_1e5`, `math`) change by < 1 %; promise-heavy programs gain the
engine's per-job cost (≈ 60-120 per job). Each R stage re-baselines once.

### 7.5 Expected test262

| stage | Δ | why |
|---|---|---|
| R1 | 0 | stdout/stderr text and exit codes unchanged (§2.3) |
| R2 | 0 … +5 | same correctly rounded algorithms; power-of-two `parseInt` becomes exact |
| R3 | 0 | same Unicode data; libm last-ulp changes only affect tests that check exact transcendental bits (test262 checks special values) |
| R4 | > 0 | thenable assimilation (the 14 `Array.fromAsync` failures of DESIGN §13 and the `Promise` / `await` thenable tests); any other change is a job-order bug |
| M1 / M2 | 0 | no semantic change; any change is a bug |
| M3 | ≤ +310 | §6.5 |

---

## 8. Open questions for the user

Each item: **Decision needed**, options, recommended default (used if the
user approves without answering).

| # | question | options | default |
|---|---|---|---|
| Q1 | **Decision needed.** Module target | (a) WASI preview 1 core module; (b) preview 2 component | (a); a component wrapper stays phase 7 |
| Q2 | **Decision needed.** How the ≈ 2,800 completion-value assertions and `jsc -p` read the result | (a) emit option `print_completion` renders it in the guest, helpers compare text; (b) keep a Rust-side decoder for tests only | (a) — (b) would keep a second renderer, contradicting "delete outright" |
| Q3 | **Decision needed.** Nested string quoting in console output | (a) keep today's unquoted strings everywhere (parity); (b) switch to Node (`[ 'a' ]`) | (a) in phase 4; (b) is a separate user-visible change touching many expectations |
| Q4 | **Decision needed.** libm fidelity | (a) hand-port `libm` (musl/msun) to emitter helpers, differential tests vs Rust std; (b) splice a precompiled wasm object (needs function-index relocation) | (a) |
| Q5 | **Decision needed.** Unicode table budget in every module and in `jsc_wasm_bg.wasm` | (a) unconditional tables ≤ 74 KiB target / 96 KiB hard cap, generated by `tools/unicode-tables`; (b) a smaller trie encoding (more code, ≈ 40 KiB) | (a); dropping NFK* or gating on source text is not an option (silently wrong) |
| Q6 | **Decision needed.** Ryu table variant | (a) size-optimized (< 1 KiB); (b) full (≈ 9.9 KiB, fewer multiplies) | (a) |
| Q7 | **Decision needed.** StringToNumber fast path | (a) Clinger + Simple Decimal Conversion, ≈ 1.6 KiB tables; (b) add Eisel-Lemire (+≈ 10.4 KiB) | (a); (b) is phase-6 performance |
| Q8 | **Decided: (b).** `wasmtime-wasi` footprint (may pull `tokio`/`cap-std` and the component-model feature into the CLI and the deny/shear surface) | (a) `wasmtime-wasi` preview-1 linker; (b) four `Linker::func_wrap` imports in the runner (≈ 120 lines, same contract as `wasi.ts`) | (b), chosen by the user; the runner links the four imports itself |
| Q9 | **Decision needed.** Node path of `@cdlab/jsc` | (a) the same in-memory polyfill everywhere, `node:wasi` documented for `jsc build` output; (b) `node:wasi` in `run()` (needs temp files to capture) | (a) |
| Q10 | **Decision needed.** Shadow-stack overflow | (a) trap by address wrap (0 ops); (b) explicit check throwing a catchable `RangeError: Maximum call stack size exceeded` (≈ 4 ops/frame + static error) | (a); revisit in phase 5/6 |
| Q11 | **Decision needed.** Unhandled-rejection report order | (a) creation order via `seq` (today's observable order); (b) rejection order (Node) | (a) |
| Q12 | **Decision needed.** Clock / random errors | (a) `clock_time_get` errno → time `0`; `random_get` errno → clock-seeded `splitmix64`; (b) treat as fatal (`proc_exit(1)`) | (a) |
| Q13 | **Decision needed.** Collection budget | (a) `GC_MIN_BUDGET = 4 MiB`, next budget `max(4 MiB, live)`; (b) other constants | (a) |
| Q14 | **Decision needed.** Renderer depth cap | (a) none (today, `value.rs:174`), cycle guard only; (b) Node's `depth: 2` with `[Object]` | (a) |
| Q15 | **Decision needed.** OOM error identity | (a) one static `%OutOfMemoryError%`; (b) a fresh object when memory allows | (a) |
| Q16 | **Decision needed.** WeakMap storage | (a) linear pairs with in-collection compaction; (b) address-hashed table | (a) |
| Q17 | **Decision needed.** `ClearKeptObjects` timing | (a) each microtask checkpoint (queue empty); (b) after every job | (a) |
| Q18 | **Decision needed.** `$262.gc()` / `host-gc-required` | (a) not in phase 4, flag stays; (b) test-only internal builtin | (a) |
| Q19 | **Decision needed.** Compact Map/Set backings during collection (DESIGN §13 tombstones) | (a) no: a running `forEach` holds a slot index in a wasm local; (b) yes with an iteration counter on the handle | (a) |
| Q20 | **Decision needed.** `gc_stress` | (a) test-only compile option, not on the CLI; (b) CLI flag | (a) |
| Q21 | **Decision needed.** Stage parallelism | (a) M1 as a parallel L2 from R1's merge on, R2 ∥ R3, merge-order rule §7.3; (b) strictly sequential R1 → R2 → R3 → R4 → M1 → M2 → M3 | (a) |
| Q22 | **Decision needed.** A throw inside a `FinalizationRegistry` cleanup callback | (a) reported like an uncaught exception in a job (`Uncaught …`, exit 1); (b) ignored | (a) (throw-or-correct) |
| Q23 | **Decision needed.** Mark representation | (a) epoch byte (§5.2); (b) bit + clearing pass | (a) |
| Q24 | **Decision needed.** Sweep | (a) eager in phase 4; (b) lazy now | (a); lazy is phase-6 work |

Decided without a question (recorded for review): `$262` stays absent; `__alloc` / `__new_array`
dead exports deleted in R1; shim-vs-native divergences (case mapping,
`toString(radix)`, `Math.*`, `Math.random`, lone surrogates in output)
disappear with one guest implementation, so no parity tests are kept; only
the `jsc` CLI promises "trap → exit 1", foreign hosts report traps their own
way.

---

## DESIGN.md changes on implementation

Applied by the implementing stage (not in this task):

| § | change | stage |
|---|---|---|
| §3 | crate/package layout: `run/` = WASI runner only; `packages/jsc/src` = `index.ts` + `wasi.ts`; `tools/unicode-tables` | R1, R3, R4 |
| §4.2 | module layout: shadow stack, static region at `SS_SIZE + 8`, scratch areas, tables, page table, static root list; `_start` replaces `main`; completion value via `print_completion` | R1, M1 |
| §5.1 | header byte 1 = GC epoch, not "reserved mark bit, always 0"; promise flags `HANDLED` / `IN_REJECTED_LIST` | R4, M1 |
| §5.2 | `TYPE_PROMISE` 40 B layout; new kinds `15..19` + `TYPE_FREE`; Map/Set "until a collector exists" → Q19 decision | R4, M1-M3 |
| §5.6 | replaced by pages / size classes / free lists / budget / catchable OOM; "No free", `__heap_ptr` sentinel and `oom_guard` text deleted | R1, M2 |
| §5.7 | rewritten as the implemented collector; delete "raw `i32` bases are re-derived from spilled jsvals after safepoints" (non-moving); "every internal store a headered block" true only after M1 (F1); "promise reactions are keyed by block base" deleted; Map/Set compaction sentence → Q19; "host holds no roots" after R4 | M1, M2 |
| §6.1 | replaced by the four WASI imports; the `__convert` "sees a Date" claim (also stale in `run/harness.rs:180-183`, `shim.ts:686-690`, deleted with those files) disappears with the section | R1-R4 |
| §6.2 | new helpers (renderer, dtoa, strtod, libm, Unicode, promise engine, GC) | each R/M stage |
| §6.7 | promises: guest engine, job records, checkpoint, rejected list | R4 |
| §6.8 | host boundary: deleted (no host reads layouts) | R4 |
| §6.9 | value rendering: guest renderer, same rules | R1 |
| §7.9 | `WeakMap`, `WeakSet`, `WeakRef`, `FinalizationRegistry` rows | M3 |
| §9 | `-p` via `print_completion`; `jsc build` output runs under any WASI host | R1, R4 |
| §10 | feature flags removed; runner unchanged | M3 |
| §11 | `unicode-tables --check`; `node:wasi` smoke step | R1, R3, R4 |
| §13 | delete "Not available: … `WeakMap`/`WeakSet`/`WeakRef`" (M3); delete "Math transcendental functions follow the host libm (Rust) or the JS engine" (R3); delete "imports seven host functions from `env` … implemented three times" (R4); delete "No garbage collection … exhaustion is not catchable. The intern table never shrinks." (M2); keep the Map/Set tombstone bullet (Q19) | R3, R4, M2, M3 |
| §14 | phase 4 done; `WeakSet` listed with `WeakMap`; open decision "WASI preview 1 or preview 2" removed (Q1) | M3 |
