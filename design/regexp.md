# Phase 5b design: RegExp

**Status: Proposed — awaiting approval.** Nothing is implemented until the
user approves this document and answers (or accepts the defaults of) §10.

Design for the `RegExp` item of DESIGN §14 phase 5 (ECMA-262 §22.2, plus the
String methods of §22.1.3 that reach it). Every claim about current behaviour
cites the tree at `11ca248` (`feat/phase-5b`, = `origin/main` + baseline);
paths are repository-relative, `emit/` = `crates/jsc-core/src/emit/`,
`codegen/` = `crates/jsc-core/src/codegen/`, `abi.rs` =
`crates/jsc-core/src/abi.rs`. test262 counts come from the CI per-test
results of `cd2ce28` (`.tmp/ci/main-cd2ce28/test262-results.tsv`, 29,496 /
33,051 eligible pass) and the pinned checkout `2e0a5676` (`features:`
frontmatter). Proposed names (helpers, constants, files, tests) are normative
unless §10 lists them as a decision.

Contents: 0 Summary · 1 Current state · 2 Engine options · 3 Engine design ·
4 Syntax and early errors · 5 Semantics · 6 The RegExp object and prototype ·
7 String integration · 8 Stages · 9 Risks · 10 Open questions ·
DESIGN.md changes on implementation.

---

## 0. Summary

**Recommendation: option A** — one backtracking engine in the guest runtime
(a pattern parser, a bytecode compiler and a bytecode VM, emitted as helper
functions like `emit/libm.rs` and `emit/unicode.rs`), shared by literals and
`new RegExp`; regular expression literal early errors are reported at compile
time, with a span, by a separate Rust-native parser in `jsc-core` that
implements the same grammar (two implementations, one grammar, one shared
syntax corpus test; §3.6). The code does not disprove A; the cost it reveals
is module size (§2.2): any program that can create a RegExp carries the whole
engine and its Unicode data, so the data is kept out of the unconditional
Unicode segment and the shaker learns one narrow rule so that programs
without RegExp pay nothing.

**Scope:** all of ECMA-262 2025 §22.2 with Annex B off: flags `d g i m s u v
y`, named groups including duplicate named groups, lookbehind, property
escapes, the `v` flag (set operations, properties of strings, `\q{}`),
modifiers, match indices, `RegExp.escape`, the Symbol methods,
`RegExpStringIterator`, species, and the String integration. Out:
`RegExp.prototype.compile` and the Annex B pattern grammar, the legacy static
properties (`RegExp.$1`, `lastMatch`, …).

**Size:** ≈ 12,100–15,400 Rust LOC (emitter, Rust parser, generator, tests;
≈ −340 deleted), ≈ 90–115 KB of guest code and ≈ 67–97 KB of Unicode data,
both only in modules that can create a RegExp (§2.2, §8).

**test262:** ≈ +2,175 (range +1,950 … +2,300) passing; 1,241 tests become
eligible; ≈ 31,670 / 34,292 = 92.4 % at the end (from 89.2 %). Derivation in
§1.3 and §8.3.

**Stages:** four (X1 core engine and the RegExp object, X2 Symbol methods and
String integration, X3 named groups / lookbehind / indices / modifiers, X4
Unicode properties / `v` flag / `RegExp.escape` / closeout), each one L2
merge with at most three parallel L3s.

**Rules applied throughout.** No backward compatibility: the
"regular expression literals are not supported" diagnostic, the
string-pattern `TypeError`s of `search` / `match` / `matchAll`, their tests,
the `regexp-*` / `RegExp.escape` feature lines and the DESIGN §13 bullets are
deleted in the stage that supersedes them, never kept as aliases. Throw or be
correct: a construct a stage cannot yet run is a spanned compile error in a
literal and a `TypeError` naming the construct at run time — never a wrong
match and never a `SyntaxError` for a valid pattern. Tests are written in the
normal test files and executed only in GitHub Actions CI (no local test runs
in this campaign). Workspace lints (no warnings, no `unsafe`, no
`unwrap` / `expect` / `panic` outside tests) hold for every piece: the Rust
parser returns `Result`, the generator reports through `ExitCode`, and the
emitter follows the existing `FnBuilder` patterns.

---

## 1. Current state

### 1.1 Regex literals and `RegExp`

- A regex literal reaches lowering and falls into the unsupported-expression
  arm: `codegen/expr.rs:360` raises `CompileError::spanned(span,
  unsupported_expr_msg(..))`, and `codegen/expr.rs:3206` maps `Lit::Regex` to
  `"regular expression literals are not supported"`. swc lexes the literal
  (body and flags) but nothing validates the pattern; `codegen/early.rs`
  has no regex check.
- `RegExp` is not in the realm (`emit/realm.rs` has no RegExp intrinsic), so
  `RegExp` is a `ReferenceError: RegExp is not defined` at run time.
- `crates/jsc/tests/errors.rs:106-124`
  (`unsupported_construct_renders_caret_diagnostic`) uses `let r = /a/;` as
  its example of an unsupported construct.
- DESIGN §13 lists "Not available: `RegExp` (literals and constructor)"
  (DESIGN.md:2901) and attributes 162 phase-3 failures to `RegExp` (the
  table row at DESIGN.md:2928: Array 28, String 131, Function 3); §14 keeps
  "`RegExp` engine: written for the guest, or an existing engine compiled in"
  as an open decision (DESIGN.md:3114) and the phase-5 bullet
  (DESIGN.md:3088).

### 1.2 String methods today

`emit/builtins/string.rs`:

- `match` / `matchAll` / `replace` / `replaceAll` / `search` / `split` call
  an object argument's `@@match` / … method before `ToString(this)`
  (`emit_dispatch`, `string.rs:727`ff.), and `matchAll` / `replaceAll`
  require an `IsRegExp` argument's `flags` to hold `g`. `IsRegExp` is
  `ToBoolean(Get(v, @@match))` only (`emit_is_regexp`, `string.rs:683-706`:
  "no object has `[[RegExpMatcher]]`").
- `search` / `match` with a primitive pattern treat it as RegExp source
  without an engine: the pattern is scanned for `\^$.*+?()[]{}|`
  (`REGEXP_SYNTAX`, `string.rs:323-325`; the scan in `emit_search`,
  `string.rs:1610-1630`) and any hit throws
  `TypeError: String.prototype.search does not support RegExp patterns yet`
  (`SEARCH_REGEXP` / `MATCH_REGEXP`, `string.rs:205-213`); a hit-free
  pattern is searched as text and `match` builds `[p]` with `index`,
  `input`, `groups: undefined` (`emit_match_result`, `string.rs:831-862`);
  `undefined` becomes the empty pattern (`string.rs:468-473`).
- `matchAll` with a primitive always throws `TypeError:
  String.prototype.matchAll needs RegExp, which is not supported yet`
  (`MATCH_ALL_REGEXP`, `string.rs:215-219`, thrown at `string.rs:405-407`).
- `replace` / `replaceAll` / `split` with string patterns are the spec string
  algorithms (`emit_replace`, `string.rs:1828`; `GetSubstitution` for `$$`,
  `$&`, `` $` ``, `$'` in `emit_substitution`, `string.rs:1911`); they stay.
- `includes` / `startsWith` / `endsWith` refuse an `IsRegExp` argument
  (`REGEXP_INCLUDES` …, `string.rs:223-234`); spec behaviour, stays.
- Tests pinning the interim behaviour: `crates/jsc-core/tests/strings.rs:465-467`
  and `:691-693` (`assert_type_error_with(.., "RegExp")`).
- DESIGN §13 (DESIGN.md:2887-2889): "`search` / `match` / `matchAll`
  interpret a string pattern as RegExp source: a pattern with RegExp syntax
  throws a `TypeError`, and `matchAll` (a RegExp string iterator) always
  throws one."

### 1.3 test262 picture (CI results of `cd2ce28`)

The test262 runner skips any `annexB` directory (`crates/jsc-test262/src/walk.rs:7`,
so `annexB/built-ins/RegExp` is never counted) and every test with a feature
in `tests/test262/unsupported-features.txt` (lines 16-18: `regexp-*`,
`RegExp.escape`, `legacy-regexp`). A negative `parse` / `early` test passes on
*any* compile error (`crates/jsc-test262/src/outcome.rs:122-125`), so today's
blanket literal rejection passes 51 of the 52 passing tests under
`language/literals/regexp`.

Per directory ("elsewhere" = tests outside these directories whose result is
the regex-literal diagnostic or a `RegExp` failure):

| bucket | total | pass | fail | compile-error | inelig. (regexp features only) | inelig. (regexp + other) | inelig. (other) |
|---|---|---|---|---|---|---|---|
| built-ins/RegExp | 1,879 | 0 | 303 | 519 | 1,032 | 5 | 20 |
| built-ins/RegExpStringIteratorPrototype | 17 | 0 | 0 | 17 | 0 | 0 | 0 |
| built-ins/String/prototype/match | 51 | 19 | 9 | 19 | 3 | 0 | 1 |
| built-ins/String/prototype/matchAll | 25 | 5 | 6 | 12 | 1 | 0 | 1 |
| built-ins/String/prototype/replace | 55 | 23 | 4 | 24 | 1 | 0 | 3 |
| built-ins/String/prototype/replaceAll | 45 | 33 | 0 | 11 | 0 | 0 | 1 |
| built-ins/String/prototype/search | 43 | 29 | 4 | 7 | 2 | 0 | 1 |
| built-ins/String/prototype/split | 120 | 85 | 14 | 20 | 0 | 0 | 1 |
| language/literals/regexp | 240 | 52 | 23 | 26 | 139 | 0 | 0 |
| elsewhere | 168 | 0 | 105 | 63 | 0 | 0 | 0 |
| **total** | **2,643** | **246** | **468** | **718** | **1,178** | **5** | **28** |

"Other" ineligibility is `Reflect.construct`, `cross-realm` or `noStrict`.
Of the 718 compile errors, 703 are the regex-literal diagnostic (the whole
suite has exactly 703). The 1,178 regexp-only ineligible tests by feature
set: `regexp-unicode-property-escapes` 585, `regexp-modifiers` 230,
`regexp-unicode-property-escapes` + `regexp-v-flag` 96, `regexp-named-groups`
90, `regexp-v-flag` 89, `RegExp.escape` 18, `regexp-lookbehind` 17,
`regexp-match-indices` 15, `regexp-duplicate-named-groups` 12,
`regexp-dotall` 10, match-indices + named-groups 7, dotall + match-indices
4, duplicate-named-groups + match-indices 3, lookbehind + named-groups 1,
dotall + match-indices + v-flag 1. `legacy-regexp` tests all sit under
`annexB` (never collected).

Two harness includes are unusable today because they hold regex literals:
`deepEqual.js` (6 tests, `harness/deepEqual-*.js`) and
`nativeFunctionMatcher.js` (57 tests: 56 under
built-ins/Function/prototype/toString, 1 under harness); the latter also
needs `\p{ID_Start}` / `\p{ID_Continue}` (`u` flag).

**Plan figures corrected.** The plan's "~703 compile errors (literal)" is
exact. "822 failing under built-ins/RegExp" is fail 303 + compile-error 519
(not 822 failures). "~1200 ineligible" is 1,183 tests whose first
unsupported feature is a regexp one: 1,178 become eligible when the regexp
lines are dropped; the other 5 also need `Reflect.construct` or
`cross-realm`.

Of the 1,186 eligible non-passing tests (468 fail + 718 compile-error),
36 also use `eval` (32), the `Function` constructor (2), `Proxy` (1) or
`Reflect` (1) and stay failing for that reason (`eval` and `Function` are
unsupported, §14); 1,150 are blocked by RegExp alone.

---

## 2. Engine options

### 2.1 The options

**(A) Guest engine (recommended).** A pattern parser, a compiler to a compact
bytecode, and a backtracking VM, all emitted by `jsc-core` into the runtime
as helper functions (`emit/regexp/*.rs`, a new registry group like
`Group::Unicode` / `Group::Libm`, `emit/registry.rs:36-64`). Literals and
`new RegExp` use the same guest compiler; a literal site compiles once and
caches its program (§3.5). Literal early errors come from a Rust-native parser
(`crates/jsc-core/src/regexp/`) run during `codegen/early.rs`. The Rust parser
and the guest parser are **two implementations of one grammar** (§4): the
guest runtime is wasm produced instruction by instruction by Rust emitter
code, not compiled Rust, so the compile-time parser cannot be "the same
code". Parity is kept by construction rules and one shared corpus (§3.6).

  Variant A2 (not recommended now): the Rust side also compiles literals to
  bytecode embedded as static data, so literal-only programs skip the guest
  compile. It does not remove the guest compiler from any real module
  (§2.2: `%RegExp%` stays reachable through `"constructor"`), doubles the
  compiler (bytecode, not just accept/reject, must then match), and saves
  only a one-time compile per literal site. It is a phase-6 option once a
  bytecode differential test exists (Q2).

**(B) AOT literals plus an interpreter.** Each literal is compiled by `jsc`
to a dedicated wasm matcher function; dynamic patterns use a runtime
interpreter. Two engines with one semantics: every feature (case folding,
lookbehind, backreferences, `v` sets) is implemented twice, and the
interpreter is still needed in every module that can reach the constructor.
Fastest literal matching; a phase-6 candidate on top of A, not a
replacement.

**(C) An existing Rust engine compiled to wasm** (e.g. `regress`, which
implements ES2018+ syntax and runs over UTF-16). jsc's runtime is not a
linked Rust crate: it is wasm emitted by `wasm_encoder` into one module with
fixed function indices, one memory layout and a collector that owns the heap
(`emit/mod.rs:1636`ff.). Linking a foreign module means either (1) a second
module instantiated by the host — breaking the "imports exactly the four WASI
functions, exports exactly `_start` and `memory`" contract (DESIGN §14 phase
4) and every runner — or (2) merging a relocatable wasm object into the
emitted module: `jsc` would need a wasm linker (function / global / data /
table index relocation, its own stack pointer, its own allocator in a memory
range the collector must not touch), a `wasm32` build of the engine checked
in or built in CI, and string transcoding at each call. Its compiled
patterns would live in the foreign allocator, outside the collector, needing
a finalization hook to be freed; a panic inside it is a trap, not a
catchable exception. Spec coverage would lag (modifiers, duplicate named
groups, `v`-flag strings depend on upstream) and size is not shakeable
(the whole engine with its own Unicode tables, an estimated 250–400 KB).
Using the crate natively in the compiler for literal validation only would
give the compile-time parser for free but still leave two parsers (the
crate's and the guest's), with the crate's grammar not under our control.

### 2.2 Comparison

| criterion | A guest engine | B AOT literals + interpreter | C foreign engine |
|---|---|---|---|
| spec coverage | full §22.2, our schedule (§8) | full, twice | upstream's; gaps not fixable locally |
| module size (program with one literal) | ≈ 90–115 KB code + ≈ 67–97 KB data (§8.1) | A's interpreter + data + 1–3 KB per literal | ≈ 250–400 KB + glue, unshakeable |
| module size (program without RegExp) | 0 with the shake rule of §3.8 | 0 likewise | the whole engine unless gated |
| compile time (`jsc`) | Rust parse of each literal, `O(pattern)` | plus wasm codegen per literal | link step per module |
| match speed | bytecode interpretation (≈ 15–40 wasm ops per input unit on simple classes) | literals 3–10× faster | interpretation plus transcoding per call |
| catastrophic backtracking | explicit heap backtrack stack, capped; overflow → catchable `RangeError` (§3.4); no step budget | same for the interpreter; AOT code needs the same explicit stack | engine-defined; panic = trap |
| maintenance | ≈ 10k LOC in the existing emitter idiom | two backends | foreign toolchain, wasm linker, pinning, supply chain |
| GC interaction | program, captures and stack are heap `TYPE_RAW` blocks traced like any other (§3.7) | same | foreign heap outside the collector |
| early errors | Rust parser, spanned (§4) | same | crate parser or a second one |

**Why A's size is what it is.** Runtime shaking (`emit/shake.rs:1-22`) keeps
every builtin installed under a string key that any kept body looks up. The
console renderer looks up `"constructor"` (`emit/render.rs:1038`), so every
program that prints keeps every constructor — `%RegExp%` included, and with it
the guest compiler — unless shaking learns that a builtin installed only on a
RegExp intrinsic is reachable only when that intrinsic is (§3.8). With that
rule, a program creates RegExps only through a literal, the `RegExp` global,
or the RegExp-creating String paths (`match` / `matchAll` / `search` with a
primitive pattern), each of which keeps the engine; a program with none of
them keeps nothing. Within a program that keeps the engine, the compiler is
always reachable (`/a/.constructor(src)` compiles any pattern), so the
Unicode property data is kept too (Q6 discusses a narrower gate). The data
therefore lives in its own passive data segments, written empty when its
loader is shaken (§3.8), not in the unconditional Unicode segment
(`emit/mod.rs:1657-1658`, budget test `emit/unicode.rs:1081`).

---

## 3. Engine design

### 3.1 Files

| file | content |
|---|---|
| `crates/jsc-core/src/regexp/mod.rs`, `syntax.rs`, `tests.rs` (new) | Rust parser: `parse(pattern: &[u32], flags: &str) -> Result<PatternInfo, SyntaxIssue>`; used by `codegen/early.rs` and the IR lowering |
| `emit/regexp/mod.rs` (new) | `TYPES`, `emit_helpers`, `helper_graph`, bytecode constants, program layout |
| `emit/regexp/parse.rs` (new) | guest parser (pattern string → node buffer, group table) |
| `emit/regexp/compile.rs` (new) | guest compiler (node buffer → program block), class building, case closure |
| `emit/regexp/vm.rs` (new) | `__rx_match`, the backtracking VM |
| `emit/regexp/data.rs` (new) | passive segments and their loaders (fold, properties, strings) |
| `emit/regexp_data.rs` (new, generated) | regexp Unicode tables, written by `tools/unicode-tables` |
| `emit/builtins/regexp.rs` (new) | constructor, `exec`, `test`, `toString`, getters, `RegExp.escape`, `RegExpBuiltinExec` |
| `emit/builtins/regexp_symbols.rs` (new) | `@@match`, `@@matchAll`, `@@search`, `@@replace`, `@@split`, `%RegExpStringIteratorPrototype%.next` |
| `tools/unicode-tables/src/regexp.rs` (new) | fold, property and string-property tables |

Shared files get small, additive edits only (§9): `abi.rs` (one `CLASS_*`
constant and offsets), `emit/realm.rs` (three intrinsics appended),
`emit/gc.rs` (one tracing arm), `emit/render.rs` (one form),
`emit/registry.rs` (one group), `emit/shake.rs` (one rule), `emit/sections.rs`
(the data count section and passive segments), `codegen/early.rs`,
`codegen/expr.rs`, `ir.rs`, `emit/builtins/string.rs`.

### 3.2 Helpers

| helper | signature | role |
|---|---|---|
| `__rx_compile(src: i32, flags: i32) -> i32` | string bases → program base, or `0` with a `SyntaxError` pending | parse, validate, compile (§3.3) |
| `__rx_flags(flags: i32) -> i32` | string base → flag bits, `-1` invalid | §4.3 |
| `__rx_literal(site: i32, src: i32, flags: i32) -> i64` | literal evaluation: a fresh RegExp object on the site's cached program (§3.5) | |
| `__rx_alloc(proto: i64) -> i64` | `RegExpAlloc`: object with `lastIndex` own property, program `0` | |
| `__rx_init(r: i64, src: i64, flags: i64) -> i64` | `RegExpInitialize` | |
| `__rx_match(prog: i32, s: i32, at: i32, caps: i32) -> i32` | run the program on string `s` from unit index `at`; capture slots into the `TYPE_RAW` block `caps`; `1` match / `0` failure | §3.4 |
| `__rx_exec(r: i64, s: i64) -> i64` | `RegExpBuiltinExec` (§5.2): result array or `null` | |
| `__rx_exec_generic(r: i64, s: i64) -> i64` | `RegExpExec` (user `exec` honoured, §6.3) | |
| `__rx_canon(cp: i32, mode: i32) -> i32` | `Canonicalize` (§5.6) | |
| `__rx_class_has(prog: i32, class: i32, cp: i32) -> i32` | binary search of a class's ranges | |
| `__rx_load_*() -> i32` | copy one passive segment to a heap block on first use and return its base (§3.8) | |

### 3.3 Parser and compiler

The guest parser reads the pattern string (Latin-1 or UTF-16) as code
points in `u` / `v` mode and as code units otherwise, and builds a flat node
buffer in a `TYPE_RAW` block: 16-byte nodes (`kind: u8`, `flags: u8`,
`a: u16`, `b: u32`, `c: u32`, `next: u32`), children linked by index. A
first pass counts capturing groups and collects group names (needed before
compilation: forward `\k<name>` references, duplicate-name checks, decimal
escapes beyond the group count). Errors leave `SyntaxError: Invalid regular
expression: /<source>/<flags>: <reason>` pending (the same reason strings as
the Rust parser, §4.4). Recursion over nesting depth uses an explicit stack in
the node buffer, never wasm recursion (a pattern of 10^5 nested groups must
not exhaust the host stack; DESIGN §13 already lists unguarded runtime
recursion).

The compiler walks the node buffer and writes the **program block**
(`TYPE_RAW`):

| offset | field |
|---|---|
| 8 | `bits: u32` — matching flags `i m s u v` and derived bits (`unicode_mode`, `ignore_case_unicode`) |
| 12 | `ncap: u32` — capturing groups + 1 |
| 16 | `nregs: u32` — loop counters and progress registers |
| 20 | `names: u32` — offset of the group-name table (`0` = none): per name, the name string base and the list of group indices sharing it (duplicate named groups) |
| 24 | `code: u32`, `code_len: u32` — the instruction stream (u32 words) |
| 32 | `classes: u32` — class pool: per class, `count: u32` then sorted, merged `(lo, hi)` u32 pairs, canonicalized under `i` (§5.6) |
| 36 | `strings: u32` — string-set pool for `v`-mode classes with strings (§5.9) |
| 40 | table data: group names (units), classes, strings, code |

The program is immutable after compilation and referenced only through
RegExp objects and literal-site caches. Group names are stored as their code
units inside the program, so the block holds no heap references and is a
plain `TYPE_RAW` leaf for the collector (§3.7).

**Instruction set** (one opcode word, operands in following words;
`dir` is a per-instruction direction bit: forward, or backward inside a
lookbehind):

| opcode | operands | meaning |
|---|---|---|
| `CHAR` | `c` | match one unit / code point equal to `c` (canonicalized under `i`) |
| `STR` | `len`, units… | a literal run (after canonicalization) |
| `ANY` | — | any character but a line terminator; `ANY_ALL` under `s` |
| `CLASS` / `NCLASS` | `class` | class membership / negation |
| `CLASS_STR` | `class` | `v`-mode class with strings: longest string first, then the code-point part (§5.9) |
| `BOL` / `EOL` | — | `^` / `$` (with `m`: also after / before a line terminator) |
| `WORDB` / `NWORDB` | — | `\b` / `\B` (`i` + `u`/`v`: `ſ` U+017F and `K` U+212A are word characters) |
| `SPLIT` / `SPLIT_LAZY` | `alt` | push a backtrack entry for `alt`, continue / the reverse |
| `JMP` | `to` | |
| `SAVE` | `slot` | write the position to capture slot `slot` (trailed) |
| `CLEAR` | `lo`, `hi` | reset capture slots `lo..hi` to `-1` (trailed); emitted at each quantifier iteration (`RepeatMatcher` step 4) |
| `BACKREF` | `group` / `name` | match the text of a capture (or the participating one of a duplicate name), canonicalized under `i`; an unset capture matches empty |
| `LOOK` | `kind`, `end` | lookahead / lookbehind, positive / negative: run the body as a sub-match from the current position, then cut the backtrack stack to the mark (lookarounds are atomic) |
| `REG_SET` / `REG_INC_LT` / `REG_GE` | `reg`, `n` | counted loops for `{n,m}` (counters trailed) |
| `PROGRESS` | `reg` | the empty-check: fail when an iteration of a `min = 0` loop consumed nothing (`RepeatMatcher` step 2.b) |
| `STAR` / `STAR_LAZY` | `atom`, `min`, `max` | fast loop over a single-character atom (`CHAR` / `ANY` / `CLASS`) without captures: one backtrack entry holding the count, decremented on backtrack |
| `MOD` | `bits` | set the `i` / `m` / `s` bits for a modifiers group (§5.10); restored by a trailed entry |
| `MATCH` | — | success |

Quantifier bounds are parsed as numbers up to 2^53 − 1 (test262
`quantifier-integer-limit.js`) and stored saturated at `0xFFFF_FFFF` = ∞:
strings never exceed `MAX_STRING_LENGTH` units, so a larger minimum can never
be met and a larger maximum is unbounded in effect.

### 3.4 VM and backtracking

`__rx_match` is a loop over `pc` with locals `pos`, `width` (string width),
`dir`, `bits`. State outside locals lives in three `TYPE_RAW` blocks held by
the caller (`__rx_exec`): the capture slots `caps` (`2 · ncap` i32), the
registers, and the **backtrack stack** — 12-byte entries `(pc | kind, pos,
aux)` — with an undo trail for capture, register and modifier writes (each
backtrack entry records the trail height). Backtracking pops an entry,
undoes the trail to its height and resumes. Nothing recurses on the wasm
stack: alternation, quantifiers and lookarounds are all entries.

- **Growth and limit.** The stack starts at 256 entries and doubles through
  `__alloc` (a safepoint, §3.7). Past `REGEXP_STACK_LIMIT` = 4 Mi entries
  (48 MiB) the match throws `RangeError: Maximum call stack size exceeded`
  (the message V8 gives for backtrack-stack exhaustion); an allocation
  failure below the limit is the usual catchable out-of-memory `RangeError`
  (DESIGN §5.6). Both are catchable and leave `lastIndex` untouched (the
  spec's `Set(lastIndex)` steps are not reached).
- **No step budget.** Exponential patterns (`/(a*)*b/` on a long `a` run) run
  as long as the spec requires, as in V8; the `jsc` runner and test262 kill
  on timeout. A budget would make a valid program throw (Q3).
- **Input reads.** In `u` / `v` mode a forward read combines a lead and
  trail surrogate at `pos` (`emit/unicode.rs::push_pair_cp` pattern) and
  advances by 2; a backward read combines the pair ending at `pos`. Lone
  surrogates are code points of their own. Outside `u` / `v` every unit is a
  character.
- **Metering.** The VM is ordinary emitted code, so wasmtime fuel and the
  opcount gate meter it; a new opcount program `regexp.js` pins its cost.

### 3.5 Literal evaluation

A literal is lowered to `IrNode::RegExp { site, source, flags }` (`ir.rs`).
Each evaluation creates a new object (ES5+), as `RegExpCreate` requires, but
compiles only once: the site owns a module cell (`program.module_cells`, a
collector root, DESIGN §5.7) holding its program base boxed as a heap jsval;
`__rx_literal` compiles into it on the first evaluation, then
`__rx_alloc(%RegExp.prototype%)` plus `[[OriginalSource]]` / `[[OriginalFlags]]`
from the static strings and the cached program. Because the Rust parser
already accepted the literal, `__rx_compile` cannot fail on it; a failure
would be a parity bug and surfaces as the pending `SyntaxError` (not a trap),
caught by the corpus test (§3.6).

### 3.6 Two parsers, one grammar: parity

| rule | how |
|---|---|
| one grammar text | §4 of this document is the grammar both implement; each production is one function in both parsers, named after the production (`parse_class_set_operand`, …) |
| one reason table | `regexp/syntax.rs` owns `SyntaxIssue` and its message strings; the emitter takes the guest's static strings from the same table, so messages cannot drift |
| one corpus | `crates/jsc-core/tests/regexp_syntax_corpus.txt`: lines `verdict<TAB>flags<TAB>pattern` (`ok` / reason id), ≥ 600 lines covering every early error of §4.2 and every production, in all modes; `regexp_syntax.rs` checks each line both as a literal (compile error or not, with the reason) and through `new RegExp(p, f)` (`SyntaxError` with the same reason, or success) |
| test262 | the paired literal / constructor tests (language/literals/regexp vs built-ins/RegExp) |

### 3.7 Object layout and GC

A RegExp instance is a `TYPE_OBJECT` block of a new class `CLASS_REGEXP`
(the next free class byte; `9` at `11ca248`, `abi.rs:150` ends at
`CLASS_ITERATOR = 8`), 40 bytes:

| offset | field |
|---|---|
| 0 | header (class byte `CLASS_REGEXP`) |
| 4 | `props` → own table; entry 0 is always `lastIndex` (writable, non-enumerable, non-configurable), co-allocated like an object literal's table |
| 8 | `proto` |
| 16 | `source: i64` — `[[OriginalSource]]` string (`undefined` before `RegExpInitialize`) |
| 24 | `flags: i64` — `[[OriginalFlags]]` string |
| 32 | `program: u32` — raw ref to the program block; `0` = not initialized (`[[RegExpMatcher]]` absent) |
| 36 | `bits: u32` — all eight flag bits, for the getters and `RegExpBuiltinExec` |

A `TYPE_OBJECT` class, not a new type byte, keeps the edit to the type list
of `abi.rs` and the object-family tests out of the way of L2-T / L2-B, which
add kinds. Tracing (`emit/gc.rs`, the class arm of `__gc_trace`): jsvals
`@16`, `@24`, raw `@32`. The program block is a `TYPE_RAW` leaf (group names
are units inside it; `groups` objects intern them when built, §5.4).

**Roots during a match.** `__rx_exec` holds the RegExp object, the input
string, the program, `caps`, the registers and the backtrack stack as
`ROOT_BASE` / jsval locals (DESIGN §5.7 builder rules); `__rx_match` is a
leaf except on the backtrack-stack growth path, which pushes its frame only
there (the `__array_push` cold-path pattern). The match runs no user code;
every user-visible step (`lastIndex` reads and writes, `exec` lookups,
replacer calls) happens between matches, after the capture results are
copied out.

### 3.8 Shaking and data segments

- **Holder rule** (`emit/shake.rs`): a builtin installed only on
  `%RegExp%`, `%RegExp.prototype%` or `%RegExpStringIteratorPrototype%` is
  kept by a key lookup only when its holder is reachable — the global key
  `RegExp` is looked up, a kept body names the holder (literal lowering,
  `RegExpCreate`), or the program is left whole. Every other builtin keeps
  today's rule; generalizing the holder rule is phase-6 work. Named test
  `shake::programs_without_regexp_drop_the_engine`.
- **Passive data.** The regexp tables (§5.6–§5.9) are passive data segments,
  one per group (fold; properties; properties of strings; names). Each has a
  loader `__rx_load_<group>()` that, on first call, allocates a `TYPE_RAW`
  block, `memory.init`s the segment into it, stores the base in a global
  listed among the collector's raw roots (DESIGN §5.7 root table), and
  `data.drop`s the segment. The data section is written after shaking
  (`emit/mod.rs:1795-1811`), so a segment whose loader body was shaken is
  emitted with zero bytes; the segment count never changes, so the data
  count section (new; required by `memory.init`) is fixed. No layout address
  depends on the regexp tables.

---

## 4. Syntax and early errors

### 4.1 Grammar (ES2025 §22.2.1, Annex B off)

`ParsePattern(P, u, v)` parses with `[+UnicodeMode]` when `u` or `v`,
`[+UnicodeSetsMode]` when `v`, and `[+NamedCaptureGroups]` always (Annex B
off: `\k` is always a group reference). Annex B's `ExtendedAtom`,
`ExtendedPatternCharacter`, `QuantifiableAssertion`, octal and legacy class
escapes, `\c` inside classes and `InvalidBracedQuantifier` tolerance are not
implemented (Q4): spec B.1.2 is normative only for web browsers, and the
test262 runner never collects `annexB/`.

| production | non-`u` (`[~UnicodeMode]`) | `u` (`[+UnicodeMode]`) | `v` (`[+UnicodeSetsMode]`) |
|---|---|---|---|
| characters | code units | code points; `\uLEAD\uTRAIL` combine; `\u{…}` | as `u` |
| `PatternCharacter` | not a `SyntaxCharacter` (`^ $ \ . * + ? ( ) [ ] { }` and the vertical bar): a bare `]`, `{`, `}` is an error | same | same |
| `IdentityEscape` | `SourceCharacter` but not `UnicodeIDContinue` | `SyntaxCharacter` or `/` | as `u` |
| `\k<name>` | group reference | same | same |
| `\p{…}` / `\P{…}` | error (`p` is `IDContinue`) | property escape (§5.8) | property escape incl. properties of strings (`\p` only) |
| `\c` | `\c` + ASCII letter | same | same |
| `\0` | not followed by a decimal digit | same | same |
| `DecimalEscape` `\N` | ≤ group count (else error) | same | same |
| quantifiers | after an `Atom` only; lookarounds not quantifiable | same | same |
| class | `[…]` with ranges | ranges; a class escape in a range is an error | `ClassSetExpression`: union, `&&` intersection, `--` subtraction, nested `[…]`, `\q{…}` string alternatives |
| groups | `(…)`, `(?:…)`, `(?<name>…)`, `(?=` `(?!` `(?<=` `(?<!`, `(?i-ms:…)` | same | same |

Group names: `RegExpIdentifierName` with `\u` escapes and surrogate pairs
(escaped or literal) in every mode. In `v` mode a `ClassSetCharacter` excludes
`ClassSetSyntaxCharacter` (`( ) [ ] { } / - \ |`) and the
`ClassSetReservedDoublePunctuator`s (`&& !! ## $$ %% ** ++ ,, .. :: ;; << ==
>> ?? @@ ^^ `` ~~`), which must be escaped (`ClassSetReservedPunctuator`).

### 4.2 Early errors (§22.2.1.1)

Both parsers report: unbalanced `(` / `)` / `[`; nothing to repeat;
`{n,m}` with `n > m`; a class range with a class escape endpoint (in `u` /
`v`) or out of order; an invalid escape for the mode; a decimal escape above
the group count; `\k` without a valid `<name>` or naming no group; a
duplicate group name unless the two groups can never both participate
(`MightBothParticipate`, ES2025 duplicate named groups); an unknown
property name or value, a property of strings under `\P`, in a negated class
or in `u` mode; a negated `v` class that `MayContainStrings`; mixed
operators in one `ClassSetExpression` level; modifiers: a flag other than
`i m s`, a repeated flag, a flag both added and removed, `(?-:`; an invalid
group name; a lone `{` / `}` / `]`.

### 4.3 Flags

`__rx_flags` / the Rust parser: each of `d g i m s u v y` at most once, `u`
and `v` together, any other character → `SyntaxError: Invalid flags
supplied to RegExp constructor '<flags>'` at run time, and an early error in
a literal. A literal's flags must be plain identifier characters: an escape
(`/a/g`) is an early error (test262 `early-err-flags-unicode-escape.js`;
swc keeps the raw flag text). Bits: `d=1 g=2 i=4 m=8 s=16 u=32 v=64 y=128`;
the `flags` getter order is `dgimsuvy` (§6.4).

### 4.4 Where errors surface

| source | when | form |
|---|---|---|
| regex literal | compile time: `codegen/early.rs` visits `Lit::Regex`, decodes the body to code points and calls `regexp::syntax::parse` | `CompileError::spanned(literal span, "Invalid regular expression: /src/flags: reason")`, exit 2 |
| `new RegExp(p, f)`, `RegExp(p, f)`, `RegExpCreate` (String `match` / `matchAll` / `search`) | run time, `__rx_compile` / `__rx_flags` | `SyntaxError` with the same text |
| a construct of a later stage (§8) | interim only | literal: spanned compile error `"… is not supported yet"`; run time: `TypeError` naming the construct |

The literal diagnostic replaces `"regular expression literals are not
supported"` (`codegen/expr.rs:3206`), which is deleted with its match arm in
X1. The 51 negative literal tests that pass today keep passing because the
parser rejects exactly the invalid literals (and they all are).

---

## 5. Semantics

### 5.1 Flags

`d` → `hasIndices` (§5.11); `g` / `y` → `RegExpBuiltinExec` loop and
`lastIndex` (§5.2); `i` → `Canonicalize` (§5.6); `m` → `BOL` / `EOL` at line
terminators (LF, CR, U+2028, U+2029); `s` → `ANY_ALL`; `u` → code-point
mode, simple case folding, `\p`; `v` → `u` plus set notation (§5.9). Only
`i m s u v` change the program; `d g y` are read by `RegExpBuiltinExec`, so
`@@split`'s `new C(rx, flags + "y")` reuses the program when the
`[[OriginalSource]]` and the program-relevant bits are unchanged (a clone,
§6.5).

### 5.2 `RegExpBuiltinExec` and `lastIndex`

Spec order (§22.2.7.2): `lastIndex = ToLength(Get(R, "lastIndex"))` always
(observable `valueOf`); if neither `g` nor `y`, match from 0; if
`lastIndex > length`, on `g`/`y` `Set(R, "lastIndex", 0, true)` (throws when
non-writable) and return `null`; loop: `__rx_match` at `lastIndex`; on
failure, sticky → reset and `null`, else advance by one unit (the spec's
`AdvanceStringIndex(S, lastIndex, fullUnicode)` with code-point steps in
`u` / `v` mode) and retry until past the end (then reset and `null`). On
success, in `u` / `v` mode `e` stays a code-unit index; `g`/`y` →
`Set(R, "lastIndex", e, true)`. The result: `A = ArrayCreate(n + 1)`, then
`index`, `input`, element `0`, the captures (`undefined` when unset), then
`groups` (a null-prototype object or `undefined`), then `indices` under `d`
— in the spec's property creation order.

### 5.3 Captures and backreferences

Captures are `(start, end)` unit slots; quantified groups clear their inner
captures each iteration (`CLEAR`). `\N` / `\k<name>` compare the captured
text: forward reads in normal direction, backward (ending at `pos`) inside a
lookbehind; under `i` each pair of characters is compared after
`Canonicalize`. A reference to a group that has not participated (or is
later in the pattern, a forward reference) matches empty.

### 5.4 Named groups, duplicate named groups

The name table maps each name to its group indices. `groups` is created per
match with a null prototype and one property per **distinct** name in
first-appearance order; for a duplicated name the value is the participating
group's text (at most one can participate) or `undefined`. `\k<name>` with
duplicates compiles to a `BACKREF` over the list (first participating
group). Names are stored as units in the program and interned when `groups`
objects are built.

### 5.5 Lookahead and lookbehind

`LOOK` pushes a mark, runs the body (direction forward for `(?=` `(?!`,
backward for `(?<=` `(?<!`), and on body success cuts the backtrack stack to
the mark (atomic). Negative lookarounds succeed on body failure and
restore captures (the trail). A lookbehind body is compiled right to left:
sequence order reversed, `SAVE` end before start, `STR` units compared
backward, every character read backward. Captures inside a positive
lookaround keep their values after it; inside a negative one they are
`undefined`.

### 5.6 Case folding: `Canonicalize`

- `i` + `u`/`v`: simple case folding (`CaseFolding.txt` statuses `C` + `S`).
  New table `FOLD` (run-encoded like `CASE_UPPER`, `(start: u24, len: u8,
  delta: i32, stride: u8)`, ≈ 200 runs, ≈ 1.9 KB), in the regexp fold
  segment.
- `i` without `u`/`v`: `toUppercase` of the one-unit string; when the result
  is not exactly one unit, or maps a non-ASCII unit to ASCII, keep `ch`. This
  reuses the existing tables unchanged: `CASE_UPPER` (single-scalar runs)
  and `SPECIAL_UPPER` (multi-unit results → keep `ch`) through
  `__case_simple` (`emit/unicode.rs:122`, `:333`) — no new data.
- Classes under `i` are stored canonicalized: the compiler maps every range
  through `Canonicalize` (range-wise over the fold or case runs) and merges,
  and the VM canonicalizes the input character before the class test. So a
  class matches `ch` iff some member `m` has `Canonicalize(m) =
  Canonicalize(ch)`, which is the spec's `CharacterSetMatcher`. `v`-mode
  complement under `i` follows the ES2024 `MaybeSimpleCaseFolding` rules
  (fold the operands first, complement after).
- Data source: the fold table comes from ICU4X `icu_casemap` at the same
  Unicode version as the property data (§5.8, Q6); `SPECIAL_UPPER` /
  `CASE_UPPER` stay Rust-std Unicode 17.0.0.

### 5.7 Character class escapes

`\d \D \w \W \s \S` are fixed classes (`\s` = WhiteSpace + LineTerminator,
the `trim` table already in `string.rs`); `\w` under `i` + `u`/`v` adds
U+017F and U+212A (§22.2.2.9.3 `WordCharacters`).

### 5.8 Property escapes `\p{…}` / `\P{…}`

Supported names exactly as the spec tables require: `General_Category` (and
`gc`) values with aliases, bare `gc` values, `Script` / `sc`,
`Script_Extensions` / `scx`, and the 53 binary properties of
`table-binary-unicode-properties` (`ASCII`, `Any`, `Assigned` derived, the
rest from data); in `v` mode also the seven properties of strings. The
compiler resolves a name through the names table, decodes the property's
ranges into the class (the data is read only at compile time; matching
binary-searches the class pool), and applies `\P` / class negation /
case closure as for any class. Loose matching is not allowed (exact names,
case-sensitive), per spec.

**Data and size** (generated by `tools/unicode-tables/src/regexp.rs`; counts
are estimates from Unicode 15–17 data, ±20 %, pinned by a budget test):

| table | content | encoding | est. bytes |
|---|---|---|---|
| `GC` | ≈ 4,200 runs covering U+0000–U+10FFFF | `(len: varint, gc: u8)` | ≈ 13,000 |
| `SC` | ≈ 2,300 runs | `(len: varint, sc: u8)` | ≈ 7,000 |
| `SCX` | ≈ 600 ranges whose extensions differ from `sc` + a set pool | `(delta, len: varint, set: u8)` + pool | ≈ 2,500 |
| `BINARY` | 50 data-backed properties, ≈ 11,500 ranges total | per property, an inversion list of `(gap, len)` varints | ≈ 30,000 |
| `NAMES` | property names and aliases, `gc` / `sc` values and aliases | sorted, length-prefixed | ≈ 5,000 |
| `FOLD` | §5.6 | runs | ≈ 1,900 |
| `EMOJI_STRINGS` | the seven properties of strings: ≈ 11,300 code points in multi-code-point strings (Basic_Emoji 1,406 strings, Emoji_Keycap_Sequence 12, RGI_Emoji_Flag_Sequence 258, RGI_Emoji_Modifier_Sequence 655, RGI_Emoji_Tag_Sequence 3, RGI_Emoji_ZWJ_Sequence ≈ 1,400–1,630; RGI_Emoji is their union) | single code points as ranges, strings as `(len: u8, cps: varint…)` | ≈ 20,000–35,000 |
| **total** | | | **≈ 80,000–95,000** (≈ 65,000 without strings) |

Variable-length codes are fine because only the compiler reads the tables,
once per pattern. Budget: `regexp_tables_within_budget` (≤ 112 KiB).
Source: ICU4X `icu_properties` for code-point properties and `Basic_Emoji`,
`icu_casemap` for folding, and the Unicode emoji files
(`emoji-sequences.txt`, `emoji-zwj-sequences.txt`, ≈ 170 KB of text,
vendored under `tools/unicode-tables/data/`) for the other properties of
strings, which ICU4X does not expose. Version: Unicode 17.0.0 — the version
test262's generated property tests check (`Unicode v17.0.0` in all 441
files at the top of `property-escapes/generated/`), the version of the existing case tables (Rust std), and
ICU4X 2.3.0's data (ICU 78); `icu_normalizer` stays 2.4.0 / Unicode 18.0.0
(Q6). `unicode-tables --check` in CI covers the new file.

### 5.9 The `v` flag

`ClassSetExpression` evaluation at compile time over (code-point ranges,
string set) pairs: union, intersection, subtraction, nested classes, `\q{}`
strings, properties of strings. The result's code points form a normal class;
its strings (each ≥ 2 code points, or the empty string) go to the string
pool sorted by length, longest first. `CLASS_STR` tries each string (pushing a
backtrack entry per alternative, as the spec's alternation of
`CompileToCharSet` strings longest-first), then the code-point part. Under
`i` strings are canonicalized code point by code point. Negation of a class
that may contain strings is an early error (§4.2), so `NCLASS` never has
strings.

### 5.10 Modifiers `(?ims-ims:…)`

Compiled as `MOD bits` at group entry and a trailed restore at exit; the
modified bits are the `i m s` bits the VM reads (`CHAR` / `CLASS` /
`BACKREF` canonicalization, `BOL` / `EOL`, `ANY`). Classes and literals inside
an `i`-modified group are compiled canonicalized for that group's `i` value,
so the compiler tracks the bits per node.

### 5.11 `hasIndices`

With `d`, `RegExpBuiltinExec` builds `indices`: an array of `[start, end]`
pairs (or `undefined`) per capture, plus `indices.groups` (null-prototype,
same names as `groups`) or `undefined`, created after `groups` (§22.2.7.8
`MakeMatchIndicesIndexPairArray`).

### 5.12 `RegExp.escape`

`RegExp.escape(S)` (§22.2.5.1): a non-string throws `TypeError`; a leading
decimal digit or ASCII letter becomes `\xHH`; then per code point
(`EncodeForRegExpEscape`): a `SyntaxCharacter` or `/` gets a backslash; the
`ControlEscape` characters become `\t \n \v \f \r`; the other
punctuators (comma, `-`, `=`, `<`, `>`, `#`, `&`, `!`, `%`, `:`, `;`, `@`,
`~`, `'`, backtick, `"`), white space, line terminators and lone surrogates
become `\xHH` / `\uHHHH`; everything else is copied. A guest builtin over
code units.

---

## 6. The RegExp object and prototype

### 6.1 Realm

Three intrinsics appended to the image order (DESIGN §6.4):
`%RegExp%` (constructor, `length` 2, `get [@@species]`, `escape`),
`%RegExp.prototype%` (an **ordinary object**, not a RegExp instance: §22.2.6),
and `%RegExpStringIteratorPrototype%` (`next`, `@@toStringTag`
`"RegExp String Iterator"`, inheriting `%IteratorPrototype%`). The global
object gets `RegExp`.

### 6.2 Constructor (§22.2.4.1)

`patternIsRegExp = IsRegExp(pattern)`; called without `new`: when
`patternIsRegExp` and `flags` is `undefined` and `pattern.constructor ===
%RegExp%`, return `pattern`. A pattern with `[[RegExpMatcher]]` gives `P =
[[OriginalSource]]` and `F = flags ?? [[OriginalFlags]]`; a non-RegExp
`IsRegExp` object gives `P = Get(pattern, "source")`, `F = flags ??
Get(pattern, "flags")`. Then `RegExpAlloc(newTarget)`
(`GetPrototypeFromConstructor`, `lastIndex` defined) and `RegExpInitialize`
(`ToString` P and F — `undefined` → `""` — `__rx_flags`, `__rx_compile`,
store, then `Set(obj, "lastIndex", 0, true)`). `IsRegExp` becomes
`@@match`-or-`[[RegExpMatcher]]` (`CLASS_REGEXP` with `program ≠ 0`), which
also changes `emit_is_regexp` (`string.rs:683-706`).

### 6.3 `exec`, `test`, `RegExpExec`

`exec` requires `[[RegExpMatcher]]` (else `TypeError`) and runs
`RegExpBuiltinExec`. `RegExpExec(R, S)` (used by `test` and the Symbol
methods): `exec = Get(R, "exec")`; callable → call, result must be an object
or `null` (else `TypeError`); otherwise require `[[RegExpMatcher]]` and use
the builtin. Fast path: when `exec` is the intact `%RegExp.prototype.exec%`
(the existing intrinsic-guard mechanism, DESIGN §6.4 "fast paths") call
`__rx_exec` directly — the lookup itself is always performed, because it is
observable.

### 6.4 Accessors, `toString`, `compile`

- `flags` getter (generic, any object): reads `hasIndices`, `global`,
  `ignoreCase`, `multiline`, `dotAll`, `unicode`, `unicodeSets`, `sticky` in
  that order and builds `"dgimsuvy"`-ordered text.
- Flag getters: on a RegExp instance, the bit; on `%RegExp.prototype%`
  itself `undefined`; any other `this` → `TypeError`.
- `source`: `EscapeRegExpPattern([[OriginalSource]])` — `/` → `\/`, line
  terminators → `\n` `\r` ` ` ` `, the empty pattern → `(?:)`;
  on `%RegExp.prototype%` `"(?:)"`; otherwise `TypeError`.
- `toString`: generic, `"/" + ToString(Get(R, "source")) + "/" +
  ToString(Get(R, "flags"))`.
- `RegExp.prototype.compile` (Annex B §B.2.4): **not provided** (Q4). Its
  absence is allowed for non-web hosts and is a `TypeError: … is not a
  function` at the call, never a wrong result.
- Legacy statics (`RegExp.$1`–`$9`, `input`, `lastMatch`, `lastParen`,
  `leftContext`, `rightContext`, the `legacy-regexp` proposal): **not
  provided** (Q5); their tests are under `annexB` and never collected.

### 6.5 Symbol methods (§22.2.6)

All are generic over `this` (any object) and go through `RegExpExec`:

- `@@match`: non-`g` → `RegExpExec`; `g` → `lastIndex = 0`, collect each
  match's `ToString(result[0])`, an empty match advances `lastIndex` by
  `AdvanceStringIndex`, `null` when none. `fullUnicode` = flags contain
  `u` or `v` (read from `ToString(Get(rx, "flags"))`).
- `@@matchAll`: `C = SpeciesConstructor(R, %RegExp%)`, `flags =
  ToString(Get(R, "flags"))`, `matcher = Construct(C, «R, flags»)`, copy
  `lastIndex`, return a `RegExpStringIterator` (`CLASS_ITERATOR` kind `22`:
  `[[IteratingRegExp]]` `@16`, `[[IteratedString]]` `@32`, bits
  `global` / `unicode` / `done` in `@28`; traced like the other iterator
  kinds ≥ 16, DESIGN §5.7).
- `@@search`: save `lastIndex`, set 0 (`SameValue` checks), `RegExpExec`,
  restore, return `index` or `-1`.
- `@@replace`: collect results (all for `g`), then for each: `position`
  clamped, captures `ToString`ed, `namedCaptures = Get(result, "groups")`;
  a callable replacer gets `(matched, …captures, position, S[, groups])`;
  else `GetSubstitution` with `$$ $& $\` $' $n $nn $<name>` (§7.2). Results
  are assembled in position order, skipping overlaps.
- `@@split`: `C = SpeciesConstructor(rx, %RegExp%)`, `newFlags = flags +
  "y"` if missing, `splitter = Construct(C, «rx, newFlags»)`; the sticky
  matcher is retried at each position `q`; `lim = ToUint32(limit)`
  (`undefined` → 2^32 − 1). When `C` is `%RegExp%`, `Construct` takes the
  clone path (§5.1: same program, new flag bits), so no recompile.
- `get RegExp[@@species]` returns `this`.

### 6.6 Rendering (DESIGN §6.9)

A `CLASS_REGEXP` object renders as Node does: `/source/flags` from
`[[OriginalSource]]` (escaped as `source`) and `[[OriginalFlags]]`, then own
enumerable properties in braces if any (`/a/g { x: 1 }`). An uninitialized
instance (`program = 0`) exists only inside the constructor, between
`RegExpAlloc` and `RegExpInitialize`, and never reaches the renderer
(`Object.create(RegExp.prototype)` is an ordinary object and renders as
one). Match results are
ordinary arrays with `index`, `input`, `groups` (and `indices`) own
properties, which the existing array rule already prints
(`[ ll, index: 2, input: hello, groups: undefined ]`, strings unquoted as
everywhere, DESIGN §6.9); a null-prototype `groups` object prints in the
existing null-prototype form. `RegExp String Iterator` objects render
`Object [RegExp String Iterator] {}`.

---

## 7. String integration

### 7.1 Dispatch

`match` / `matchAll` / `replace` / `replaceAll` / `search` / `split` keep
`emit_dispatch` for object arguments. The primitive paths become the spec
algorithms:

| method | primitive (or `undefined`) argument |
|---|---|
| `match(r)` | `rx = RegExpCreate(r, undefined)`, `Invoke(rx, @@match, «S»)` |
| `matchAll(r)` | `rx = RegExpCreate(r, "g")`, `Invoke(rx, @@matchAll, «S»)` |
| `search(r)` | `rx = RegExpCreate(r, undefined)`, `Invoke(rx, @@search, «S»)` |
| `replace` / `replaceAll` / `split` | unchanged string algorithms (spec) |

`Invoke` performs the observable `Get(rx, @@match)` on the fresh object, so
a user-patched `RegExp.prototype[Symbol.match]` is honoured; the intact case
takes the guarded fast path to the builtin.

### 7.2 Deleted and shared code (X2)

Deleted outright: `REGEXP_SYNTAX` and the scan arm of `emit_search`
(`string.rs:323-325`, `:1610-1630`), `SEARCH_REGEXP` / `MATCH_REGEXP` /
`MATCH_ALL_REGEXP` (`string.rs:205-219`) and the `MatchAll` throw
(`:405-407`), `emit_match_result` (`:831-862`) and the empty-pattern special
case (`:468-473`; `RegExpCreate(undefined)` yields `(?:)` by itself), their
entries in the static string list (`string.rs:333-335`), the tests at
`strings.rs:465-467` and `:691-693` (rewritten to real results), and the
DESIGN §13 bullet. `emit_substitution` (`string.rs:1911`) becomes the one
`GetSubstitution` shared by the string `replace` / `replaceAll` and
`@@replace`, extended with `$n` / `$nn` and `$<name>` (taken literally when
there are no captures / `namedCaptures` is `undefined`, which is always the
case on the string path). `includes` / `startsWith` / `endsWith` keep their
`IsRegExp` refusal.

---

## 8. Stages

### 8.1 Stage list

Rules for every stage: the emitter change, its tests and the deletion of
every path it supersedes land together; an interim stage rejects constructs
of later stages per §4.4 (literal: spanned "not supported yet"; run time:
`TypeError`), never a wrong result; each stage ends with an opcount
re-baseline commit and the CI gates green in GitHub Actions (L1 runs them;
no local test runs). Test names are written first (failing) by the stage's
first L3; files: `crates/jsc-core/tests/regexp.rs` (new),
`regexp_syntax.rs` + `regexp_syntax_corpus.txt` (new),
`regexp_unicode.rs` (new), existing files as named.

| stage | content | files (new / changed) | named tests | est. Rust LOC | est. wasm | test262 Δ |
|---|---|---|---|---|---|---|
| **X1** core engine + RegExp object | Rust parser for the **full** grammar (§4; later-stage constructs rejected as "not supported yet"), literal early errors; guest parser / compiler / VM for non-`u`, `u`, flags `g i m s u y` (`d`, `v`, named groups, lookbehind, `\p`, modifiers → interim errors); `FOLD` table; `CLASS_REGEXP`, `%RegExp%`, `%RegExp.prototype%`, constructor, species getter, `exec`, `test`, `toString`, `source`, `flags` and flag getters; literal lowering + site cache; GC arm; renderer form; holder shake rule; passive segments + data count; delete the literal diagnostic | new `regexp/{mod,syntax,tests}.rs`, `emit/regexp/{mod,parse,compile,vm,data}.rs`, `emit/regexp_data.rs`, `emit/builtins/regexp.rs`, `tools/unicode-tables/src/regexp.rs`; changed `codegen/{early,expr}.rs`, `ir.rs`, `abi.rs`, `emit/{realm,gc,render,registry,shake,sections,mod}.rs`, `tools/unicode-tables/{Cargo.toml,src/main.rs}`, `crates/jsc/tests/errors.rs` (caret test switches to `debugger;`), `tests/test262/unsupported-features.txt` (`regexp-*` becomes the explicit list of the other regexp features, so `regexp-dotall` alone is dropped), `tests/opcount/regexp.js` | `regexp_syntax::corpus_literal_and_constructor_agree`, `regexp_syntax::literal_error_is_spanned`, `regexp_syntax::invalid_flags`, `regexp::exec_result_shape`, `regexp::last_index_global_sticky`, `regexp::last_index_tolength_observable`, `regexp::alternation_and_quantifier_backtracking`, `regexp::empty_check_terminates`, `regexp::captures_reset_per_iteration`, `regexp::backreference_forward_and_case_insensitive`, `regexp::lookahead_atomic`, `regexp::canonicalize_non_unicode_keeps_non_ascii`, `regexp::unicode_mode_surrogate_pairs`, `regexp::simple_case_folding_unicode`, `regexp::dotall_and_multiline`, `regexp::quantifier_bound_saturates`, `regexp::backtrack_stack_overflow_is_range_error`, `regexp::literal_site_compiles_once`, `regexp::constructor_from_regexp_and_regexp_like`, `regexp::prototype_getters_on_prototype_object`, `console::regexp_renders_source_and_flags`, `gc::regexp_program_survives_collection`, `shake::programs_without_regexp_drop_the_engine`, `regexp/tests.rs` unit tests per production | 6,000–7,500 (−40) | ≈ 55–70 KB code + ≈ 2 KB data | ≈ +730 |
| **X2** Symbol methods + String integration | `RegExpExec` with user `exec`; `@@match`, `@@matchAll`, `@@search`, `@@replace` (shared `GetSubstitution`), `@@split` (species, sticky, clone path); `%RegExpStringIteratorPrototype%`; `IsRegExp` with `[[RegExpMatcher]]`; String `match` / `matchAll` / `search` via `RegExpCreate`; deletions of §7.2 | new `emit/builtins/regexp_symbols.rs`; changed `emit/builtins/string.rs`, `emit/builtins/regexp.rs`, `emit/realm.rs`, `emit/gc.rs` (iterator kind 22), `emit/render.rs`, `crates/jsc-core/tests/{strings,well_known}.rs` | `regexp::match_global_advances_on_empty`, `regexp::match_all_iterator_protocol`, `regexp::search_restores_last_index`, `regexp::replace_substitutions_all_forms`, `regexp::replace_function_receives_groups`, `regexp::split_species_and_limit`, `regexp::user_exec_is_called`, `regexp::exec_returning_primitive_throws`, `strings::search_and_match_with_string_patterns` (rewritten: `"a.b".search(".")` is `0`, `"aa".matchAll("a")` yields two matches), `well_known::is_regexp_uses_regexp_matcher` | 1,700–2,200 (−300) | ≈ 15–20 KB | ≈ +320 |
| **X3** named groups, lookbehind, indices, modifiers | guest parse/compile of `(?<name>…)`, `\k<name>`, duplicate named groups, `groups` objects, `$<name>`; backward compilation and `LOOK` behind; `d` / `hasIndices` / `indices`; modifiers (`MOD`); drop the interim errors for these | changed `emit/regexp/{parse,compile,vm}.rs`, `emit/builtins/regexp.rs`, `emit/builtins/regexp_symbols.rs`, `regexp/syntax.rs` (remove "not supported yet" for these), `tests/test262/unsupported-features.txt` (drop `regexp-named-groups`, `regexp-duplicate-named-groups`, `regexp-lookbehind`, `regexp-match-indices`, `regexp-modifiers`) | `regexp::named_groups_object_null_proto`, `regexp::duplicate_named_groups_participating`, `regexp::named_backreference_forward`, `regexp::lookbehind_backward_captures`, `regexp::backreference_inside_lookbehind`, `regexp::negative_lookbehind_clears_captures`, `regexp::has_indices_pairs_and_groups`, `regexp::modifiers_scope_ignore_case`, `regexp::modifiers_restore_after_group` | 1,600–2,100 | ≈ 8–12 KB | ≈ +340 |
| **X4** properties, `v` flag, `RegExp.escape`, closeout | generator: `GC`, `SC`, `SCX`, `BINARY`, `NAMES`, `EMOJI_STRINGS` (+ vendored emoji files); `\p` / `\P` resolution; `v` set operations, nested classes, `\q{}`, properties of strings, `CLASS_STR`; `RegExp.escape`; remove every interim error; drop `regexp-*` and `RegExp.escape` from `unsupported-features.txt`; DESIGN.md (§ list below), opcount re-baseline | changed `tools/unicode-tables/**`, `emit/regexp_data.rs`, `emit/regexp/{compile,data,parse,vm}.rs`, `regexp/syntax.rs`, `emit/builtins/regexp.rs`, `emit/realm.rs`, `tests/test262/unsupported-features.txt`, `DESIGN.md` | `regexp_unicode::property_general_category_and_aliases`, `regexp_unicode::property_script_extensions`, `regexp_unicode::binary_property_sample` (one assertion per spec binary property), `regexp_unicode::property_of_strings_longest_first`, `regexp_unicode::v_flag_set_operations`, `regexp_unicode::v_flag_negated_strings_early_error`, `regexp_unicode::v_flag_case_folding_complement`, `regexp::escape_all_rules`, `regexp_tables_within_budget` (unit, `emit/regexp/data.rs`) | 2,800–3,600 | ≈ 10–15 KB code + ≈ 65–95 KB data | ≈ +785 |

Totals: ≈ 12,100–15,400 Rust LOC (≈ 4,500 of it tests and the corpus),
≈ −340 deleted; ≈ 90–115 KB code + ≈ 67–97 KB data in modules that keep the
engine, 0 elsewhere.

### 8.2 L3 breakdown (at most three parallel L3s per stage)

| stage | L3s (file-disjoint where possible; merge order left to right) |
|---|---|
| X1 | **X1-T1** Rust parser (`regexp/**`), `codegen/early.rs` hook, diagnostics, corpus file, `errors.rs` caret switch · **X1-T2** guest parser / compiler / VM, `FOLD` generator, bytecode tests through a test-only `regexp_vm` unit harness in `emit/regexp/` · **X1-T3** object, realm, builtins, literal lowering (`codegen/expr.rs`, `ir.rs`), GC arm, renderer, holder shake rule, passive segments; codes against §3.2 signatures and merges after T2 |
| X2 | **X2-T1** `@@match` / `@@matchAll` / `@@search` + iterator (`regexp_symbols.rs` part 1) · **X2-T2** `@@replace` / `@@split` + `GetSubstitution` sharing (`regexp_symbols.rs` part 2, `string.rs` substitution only) · **X2-T3** String dispatch rewrite + deletions + test rewrites (`string.rs` rest, `strings.rs`, `well_known.rs`) after T1/T2 |
| X3 | **X3-T1** named + duplicate named groups, `groups`, `$<name>` · **X3-T2** lookbehind / backward VM · **X3-T3** modifiers + `hasIndices`; all touch `emit/regexp/compile.rs`, merge order T2 → T1 → T3 |
| X4 | **X4-T1** generator, vendored data, `regexp_data.rs`, loaders, budget test · **X4-T2** `\p` and `v`-flag compile + `CLASS_STR` (after T1's tables land) · **X4-T3** `RegExp.escape`, feature-flag removal, DESIGN.md, §13 / §14, opcount re-baseline |

Dependencies: X1 → X2 → X3 → X4 strictly (X2 needs exec, X3 needs `@@replace`
for `$<name>`, X4 is the closeout). X3 and X4-T1 can overlap (the generator
touches only `tools/` and the generated file).

### 8.3 test262 estimate (derivation)

Pools come from §1.3 with the 36 `eval` / `Function` / `Proxy` / `Reflect`
tests removed. "Core" vs "Symbol" splits by whether the test body calls
`replace` / `match` / `matchAll` / `search` / `split` / `replaceAll` or names
`Symbol.match` etc. Pass rates: 90 % for hand-written suites (the phase-3
families ended at ≈ 88–95 %), 95 % for the data-driven property suites.

| stage | pool | arithmetic | Δ |
|---|---|---|---|
| X1 | built-ins/RegExp core 410 + built-ins/RegExp/prototype non-Symbol 192 + language/literals/regexp core 26 + elsewhere 166 + newly eligible `regexp-dotall` 10 + `deepEqual.js` include 6 = 810 | 810 × 0.90 | ≈ +730 |
| X2 | built-ins/RegExp Symbol-using 13 + literals 1 + prototype `Symbol.*` 200 + RegExpStringIteratorPrototype 17 + String `match` 27 / `matchAll` 18 / `replace` 26 / `replaceAll` 11 / `search` 11 / `split` 32 = 356 | 356 × 0.90 | ≈ +320 |
| X3 | newly eligible: named-groups 90 + duplicate 12 + lookbehind 17 + lookbehind+named 1 + match-indices 15 + indices+named 7 + duplicate+indices 3 + dotall+indices 4 + modifiers 230 = 379 | 379 × 0.90 | ≈ +340 |
| X4 | newly eligible: property escapes 585 + properties+v 96 + v-flag 89 + dotall+indices+v 1 + RegExp.escape 18 = 789; `nativeFunctionMatcher.js` include unlocks 57 (Function.prototype.toString forms unmeasured, ≈ 60 % → 35) | 789 × 0.95 + 35 | ≈ +785 |
| **total** | | | **≈ +2,175** |

Eligible count: 33,051 + 1,178 + 6 + 57 = 34,292; pass ≈ 29,496 + 2,175 =
31,671 (92.4 %). Not reached: the 36 tests also needing `eval` / `Function` /
`Proxy` / `Reflect`, the 5 needing `Reflect.construct` / `cross-realm`, the
28 other-feature tests in these directories, and the hand-written-suite
residue. The X1 figure includes no loss of the 51 accidental literal
negative passes (§4.4); a parser that accepted an invalid literal would turn
one into a failure, which the corpus test and the test262 delta both show.

---

## 9. Risks

| risk | mitigation |
|---|---|
| **Module size.** ≈ 160–210 KB in every module that keeps the engine, including every "left whole" module (any computed member key, DESIGN §13) | holder shake rule (§3.8) keeps it out of programs without RegExp; passive segments; budget test; Q6 narrower data gate; phase 6 can generalize the holder rule |
| **Performance cliffs.** Exponential backtracking hangs like V8; greedy loops over long inputs (test262 property tests match 10^5–10^6 code points) | `STAR` single-entry loops; class tests by binary search; no time budget by design (Q3); `regexp.js` opcount program tracks VM cost |
| **wasm stack depth.** Deep patterns or long inputs exhausting the host stack (smaller under `node:wasi` / browsers) | parser, compiler and VM use explicit heap stacks only; no recursion proportional to pattern or input size |
| **Backtrack memory.** | 4 Mi-entry cap → catchable `RangeError`; growth through `__alloc` with catchable OOM |
| **Parser parity** (Rust vs guest) | §3.6: one grammar section, one reason table, ≥ 600-line corpus checked both ways, test262 pairs |
| **Unicode data growth and version skew** (properties 17.0.0 vs normalization 18.0.0; test262 moving to 18) | data from one pinned ICU4X release; header records versions; `--check` in CI; a test262 bump re-runs the generator (Q6) |
| **Annex B off** surprises real code (`/]/`, `/{/`, `\a`, octal escapes are `SyntaxError`s) | spec-correct and test262-core-correct; diagnostic names the construct; Q4 can add Annex B later as one grammar flag in both parsers |
| **Shared files with L2-P / L2-T / L2-B** (`emit/realm.rs`, `emit/props*`, `abi.rs`, `emit/gc.rs`, `emit/render.rs`, `emit/shake.rs`, `DESIGN.md`) | additive edits only: intrinsics appended at the end of the image order, one `CLASS_*` constant (number chosen at implementation: the next free one), one tracing arm, one renderer arm, one shake rule; no property-algorithm changes (Proxy's trap dispatch in §6.3 is untouched: RegExp objects are ordinary for property access); the RegExp L2 starts after PR A merges (plan: PR B), so it rebases once onto P + T + B |
| **Iterator kind numbering** (`CLASS_ITERATOR` kind `22`) colliding with a kind another L2 adds | chosen at implementation as the next free kind; DESIGN §5.2 lists them |
| **Literal-site cache vs generators** | the cache is a module cell, so suspended generator frames see the same program |

---

## 10. Open questions for the user

Each item: question, options, recommended default (used if the user
approves without answering), impact.

| # | question | options | default | impact |
|---|---|---|---|---|
| Q1 | **Engine** | (A) guest parser + compiler + VM, Rust parser for literal early errors; (B) AOT literal matchers + interpreter; (C) foreign engine linked into the module | (A) | B doubles the engine; C needs a wasm linker and breaks shaking and GC ownership |
| Q2 | Literal compilation | (a) guest compiles every pattern, literal sites cache the program; (b) Rust also emits literal bytecode (A2) | (a); (b) is phase-6 work behind a bytecode differential test | (b) adds a second compiler whose output must match byte for byte |
| Q3 | Backtracking limit | (a) no step budget; backtrack stack capped at 4 Mi entries → `RangeError: Maximum call stack size exceeded`; (b) also a step budget throwing `RangeError` | (a) | (b) makes valid long-running programs throw (spec deviation) |
| Q4 | Annex B pattern grammar and `RegExp.prototype.compile` | (a) both off; (b) Annex B grammar on (web reality), `compile` provided | (a) | (b) adds ≈ 600 LOC across both parsers and is not measured by the runner (`annexB/` skipped) |
| Q5 | Legacy statics (`RegExp.$1`, `lastMatch`, …) | (a) not provided; (b) provide | (a) | tests are all under `annexB`; (b) adds per-exec global state |
| Q6 | Unicode version and data gating | (a) properties and folding at Unicode 17.0.0 (ICU4X 2.3.0, matches test262 and the case tables), data in passive segments kept with the engine; (b) Unicode 18.0.0 (ICU4X 2.4.0, matches normalization) — ≈ 100–250 generated property tests then fail until test262 updates; (c) (a) plus gating the property data on "a literal uses `\p` or a dynamic pattern source is named" | (a) | ≈ +65–95 KB per engine-keeping module; (c) saves it for literal-only programs but a `/x/.constructor(s)` with `\p` would then throw |
| Q7 | `v` flag | (a) in (X4); (b) deferred, `v` stays an interim error | (a) | (b) leaves 186 tests (89 + 96 + 1) ineligible and ≈ 20–35 KB of string data out |
| Q8 | Duplicate named groups and modifiers (ES2025) | (a) in (X3); (b) deferred | (a) | (b) leaves 230 + 15 tests ineligible |
| Q9 | `RegExp.escape` (ES2026) | (a) in (X4); (b) deferred | (a) | 18 tests; ≈ 200 LOC |
| Q10 | Literal early errors | (a) compile time, spanned, from the Rust parser; (b) at run time, at literal evaluation, from the guest parser only | (a) | (b) fails the 51 negative literal tests and every `phase: parse` regexp test, and a typo in a dead branch would go unnoticed |
| Q11 | Size budget for the regexp tables | (a) ≤ 112 KiB, enforced by `regexp_tables_within_budget`; (b) other | (a) | the estimate is 80–95 KB with strings |
| Q12 | When the implementation L2 starts | (a) after PR A (P + T + B) merges, per the plan; (b) now, in parallel, rebasing on each P/T/B merge | (a) | (b) multiplies conflicts on `realm.rs` / `abi.rs` / `gc.rs` / `DESIGN.md` |

Decided without a question (recorded for review): `%RegExp.prototype%` is an
ordinary object (spec); `lastIndex` is a real own data property (observable
through descriptors); literal sites compile once and every evaluation
creates a new object (spec); `@@split` with `C = %RegExp%` clones without
recompiling (unobservable); no `RegExp` legacy features are hinted in error
messages.

---

## DESIGN.md changes on implementation

Applied by the implementing stage (not in this task):

| § | change | stage |
|---|---|---|
| §3 | crate layout: `crates/jsc-core/src/regexp/`, `emit/regexp/`, `emit/regexp_data.rs`, `tools/unicode-tables` regexp tables and `data/` | X1, X4 |
| §4.2 | module layout: data count section, passive regexp segments, loader globals | X1 |
| §5.2 | `CLASS_REGEXP` layout; `CLASS_ITERATOR` kind 22 (RegExp String Iterator) | X1, X2 |
| §5.7 | tracing rows for `CLASS_REGEXP` and iterator kind 22; regexp data globals as raw roots | X1, X2 |
| §6.2 | the regexp helpers (§3.2) | X1–X4 |
| §6.4 | intrinsics `%RegExp%`, `%RegExp.prototype%`, `%RegExpStringIteratorPrototype%`; the holder shake rule | X1, X2 |
| §6.9 | RegExp and RegExp String Iterator forms | X1, X2 |
| §7.9 | new `RegExp` row; `String` row: drop "the RegExp-syntax check of a `search` / `match` pattern runs in the guest", describe `RegExpCreate` paths | X1, X2 |
| §8 | the `Invalid regular expression` diagnostic | X1 |
| §10 | `regexp-*` / `RegExp.escape` removed from the unsupported list (`legacy-regexp` stays) | X1, X3, X4 |
| §13 | delete the `search` / `match` / `matchAll` bullet (X2); drop `RegExp` from "Not available" (X1); delete the `RegExp` row of the phase-3 failure table and recount (X4); add the RegExp limits that remain (no Annex B grammar, no `compile`, no legacy statics, backtrack cap) (X4); Unicode version line for the regexp tables (X4) | X1, X2, X4 |
| §14 | phase 5 `RegExp` done; delete the open decision "`RegExp` engine" | X4 |
