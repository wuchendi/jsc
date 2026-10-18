//! The single source of truth for the compiler's runtime ABI: the value
//! encoding, heap-block layout, and host-import contract shared between the
//! [`crate::emit`] side (which bakes these numbers into the wasm module it
//! generates) and the [`crate::run`] side (which decodes the resulting values,
//! backs the host imports, and reads the exported globals).
//!
//! Every constant has exactly one definition here; each module keeps a locally-typed alias
//! (`i64`/`i32` on the emit side, `u64`/`u8`/`usize` on the run side) that
//! *derives* its value from the canonical constant below, so a change here
//! propagates to both sides and the two can no longer disagree.
//!
//! ## Value encoding — NaN-boxing
//!
//! A jsval is a 64-bit carrier. Any value without the full [`QNAN`] mask set is
//! an ordinary IEEE-754 `f64`; with the mask set, the 3-bit tag in payload bits
//! `[50:48]` discriminates the boxed immediates and the single heap pointer:
//!
//! | tag     | meaning                                             |
//! |---------|-----------------------------------------------------|
//! | `0b000` | the number NaN (canonical NaN = [`QNAN`])            |
//! | `0b001` | boolean — value in bit 0 ([`FALSE`] / [`TRUE`])      |
//! | `0b010` | [`NULL`]                                             |
//! | `0b011` | [`UNDEFINED`]                                        |
//! | `0b100` | unused                                              |
//! | `0b101` | [`HEAP`] pointer (byte offset in the low 32 bits)    |
//! | `0b110` | unused                                              |
//! | `0b111` | unused                                              |
//!
//! Invariant: the number NaN is ALWAYS exactly the [`QNAN`] bits. Every site that
//! reboxes an `f64` as a jsval maps any NaN (sign-bit or payload-carrying, as wasm
//! arithmetic may produce) to [`QNAN`], so NaN tests may compare bits directly.
//!
//! ## Heap-block layout
//!
//! Every heap block opens with a [`HEADER_SIZE`]-byte header: a `TYPE_*` byte at
//! offset 0 (see [`TYPE_STRING`] … [`TYPE_CELL`]) plus 3 padding bytes, so the
//! per-type payload begins 4-byte aligned at [`HEADER_SIZE`]. The single [`HEAP`]
//! tag boxes strings, arrays, objects, and functions alike; the concrete kind is
//! the TYPE BYTE, not the NaN-box tag.

/// The quiet-NaN mask (exponent all ones + quiet bit): a value is a boxed
/// immediate (or the canonical number NaN) iff `bits & QNAN == QNAN`.
pub(crate) const QNAN: u64 = 0x7FF8_0000_0000_0000;

/// The NaN-boxed `false` jsval: [`QNAN`] plus tag `0b001`, value bit clear.
pub(crate) const FALSE: u64 = 0x7FF9_0000_0000_0000;

/// The NaN-boxed `true` jsval: [`QNAN`] plus tag `0b001`, value bit set.
pub(crate) const TRUE: u64 = 0x7FF9_0000_0000_0001;

/// The NaN-boxed `null` jsval: [`QNAN`] plus tag `0b010`.
pub(crate) const NULL: u64 = 0x7FFA_0000_0000_0000;

/// The NaN-boxed `undefined` jsval: [`QNAN`] plus tag `0b011`. Stands in for a
/// bare `return;`, a program with no expression statement, and host-side
/// fallbacks.
pub(crate) const UNDEFINED: u64 = 0x7FFB_0000_0000_0000;

/// The `typeof` result strings, in `typeof`-index order. A `typeof` result is a
/// real interned HEAP STRING (tag `0b100` is retired), so these are the words
/// `emit_typeof_node` interns and selects between; the decoder reads them back as
/// ordinary strings. Keeping them a single source shared by the emitter and the
/// runtime prevents the index/word tables from drifting apart.
pub(crate) const TYPEOF_STRINGS: [&str; 6] = [
    "number",
    "boolean",
    "undefined",
    "object",
    "string",
    "function",
];

/// The NaN-boxed heap-pointer header: [`QNAN`] plus tag `0b101` — the SINGLE tag
/// for every heap value. A heap jsval is `HEAP | (ptr as u32)`, carrying
/// the byte offset of the block's base in its low 32 bits.
pub(crate) const HEAP: u64 = 0x7FFD_0000_0000_0000;

/// The size in bytes of every heap block's leading header: the TYPE BYTE
/// at offset 0 plus 3 padding bytes. The per-type payload begins here.
pub(crate) const HEADER_SIZE: u32 = 4;

/// The number of trailing (non-positional) parameters every user function carries
/// under the uniform ABI, appended after the `K = max_arity` positional argument
/// slots: the closure **environment** slot, then the **argument count**.
/// The shared wasm signature is therefore `(a0..a_{K-1}, env, argc) -> i64`
/// (all `i64`), so a single `call_indirect` type still serves every function. The
/// env slot is `undefined` for a non-capturing callee; `argc` is the actual
/// source argument count passed as a number jsval, letting a trailing rest
/// parameter collect exactly `[arg_p, .., arg_{argc-1}]` rather than the
/// `undefined`-padded surplus.
pub(crate) const ABI_TRAILING_PARAMS: u32 = 2;

/// TYPE BYTE (offset 0) of a heap string block:
/// `[type=0][pad×3][len:u32][utf8]`. Maps to `typeof` "string".
pub(crate) const TYPE_STRING: u8 = 0;

/// TYPE BYTE of a heap array block:
/// `[type=1][pad×3][len:u32][elem:i64 × len]`. Maps to `typeof` "object".
pub(crate) const TYPE_ARRAY: u8 = 1;

/// TYPE BYTE of a heap object block:
/// `[type=2][pad×3][n:u32][(key_ptr:i64, val:i64) × n]`. Maps to `typeof`
/// "object".
pub(crate) const TYPE_OBJECT: u8 = 2;

/// TYPE BYTE of a first-class function value:
/// `[type=3][pad×3][func_idx:u32][env:i64]`. Maps to `typeof` "function".
pub(crate) const TYPE_FUNCTION: u8 = 3;

/// TYPE BYTE of a closure **environment** block (internal):
/// `[type=4][pad×3][count:u32][cell_ptr:i64 × count]`. Never a user value.
pub(crate) const TYPE_ENV: u8 = 4;

/// TYPE BYTE of a **boxed cell** (internal): `[type=5][pad×3][value:i64]`
/// holding a captured binding so closures share mutations by reference. Never a
/// user value.
pub(crate) const TYPE_CELL: u8 = 5;

/// TYPE BYTE of a `Map` handle block:
/// `[type=6][pad×3][size:u32][entries:i64]`. `size` is the logical entry count
/// (backing `.size`); `entries` is a heap jsval pointing at a GROWABLE array that
/// stores the `(key, value)` pairs flattened (key at even element slots, value at
/// the following odd slot). Growth reallocs that backing array and rewrites the
/// `entries` slot IN PLACE, so the handle jsval never moves. Maps to `typeof`
/// "object".
pub(crate) const TYPE_MAP: u8 = 6;

/// TYPE BYTE of a `Set` handle block: identical layout to [`TYPE_MAP`]
/// (`[type=7][pad×3][size:u32][entries:i64]`). A Set reuses the same flattened
/// `(key, value)` backing with the value slot always `undefined`, so all the
/// lookup/insert/delete machinery is shared; only the type byte (and thus the
/// render label) differs. Maps to `typeof` "object".
pub(crate) const TYPE_SET: u8 = 7;

/// TYPE BYTE of a `Promise` block: `[type=8][pad×3][state:i32][value:i64]`.
/// `state` is one of [`PROMISE_PENDING`] / [`PROMISE_FULFILLED`] /
/// [`PROMISE_REJECTED`]; `value` is the settled fulfillment/rejection jsval
/// (`undefined` while pending). The block is the RENDERABLE mirror of a promise's
/// settlement — the host promise engine (reactions + microtask queue) owns the
/// dynamic scheduling state, but writes `state`/`value` back here so [`crate::run`]
/// can decode a promise from a bare memory snapshot. Maps to `typeof` "object".
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_PROMISE: u8 = 8;

/// [`TYPE_PROMISE`] `state`: the promise is unsettled.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_PENDING: i32 = 0;

/// [`TYPE_PROMISE`] `state`: the promise fulfilled with its `value`.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_FULFILLED: i32 = 1;

/// [`TYPE_PROMISE`] `state`: the promise rejected with its `value` as the reason.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_REJECTED: i32 = 2;

/// TYPE BYTE of a `Generator` handle block:
/// `[type=9][pad×3][sm_idx:u32][state:i32][done:i32][env:i64]`. `sm_idx` is the
/// funcref-table index of the generator's **state-machine** function; `state` is
/// the resume point (`0` at creation, bumped at each `yield`); `done` is `0`
/// until the body returns/falls off, then `1`; `env` is the persistent
/// **frame** — a `TYPE_ENV` block of boxed cells holding every generator local so
/// they survive across a `yield` (the SM reaches them through the ordinary
/// closure `EnvLoad`/cell path). A `function*` compiles to TWO ordinary
/// uniform-ABI functions: the CONSTRUCTOR (the name `g` binds to) that allocates
/// this block and returns it without running the body, and the STATE MACHINE
/// (`sm_idx`) that `.next(v)` drives via `call_indirect`, resuming at `state`.
/// Maps to `typeof` "object".
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_GENERATOR: u8 = 9;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the `sm_idx` funcref
/// index (u32), `state` (i32), `done` (i32), and `env` frame jsval (i64). The
/// payload begins at [`HEADER_SIZE`]; the i64 `env` lands 8-byte aligned at 16.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const GEN_SM_IDX_OFFSET: u32 = HEADER_SIZE;
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const GEN_STATE_OFFSET: u32 = HEADER_SIZE + 4;
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const GEN_DONE_OFFSET: u32 = HEADER_SIZE + 8;
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const GEN_ENV_OFFSET: u32 = HEADER_SIZE + 12;

/// TYPE BYTE of a `Date` block: `[type=10][pad×3][pad×4][ms:f64]`. The
/// single f64 payload is the timestamp in milliseconds since the Unix epoch — the
/// same value `Date.now()` / `d.getTime()` yield. `new Date(ms)` stores its
/// numeric argument; `new Date()` stores the host clock (the `__now` import). The
/// f64 lands 8-byte aligned at [`DATE_MS_OFFSET`]. Maps to `typeof` "object".
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_DATE: u8 = 10;

/// Byte offset (from a [`TYPE_DATE`] block base) of the `ms` timestamp (f64). The
/// payload begins at [`HEADER_SIZE`]; the f64 lands 8-byte aligned at 8.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const DATE_MS_OFFSET: u32 = HEADER_SIZE + 4;

/// TYPE BYTE of an **accessor** block (P2b instance getters/setters):
/// `[type=11][pad×3][pad×4][getter:i64][setter:i64]`. It holds a class instance
/// accessor property's getter and setter FUNCTION VALUES (the `undefined` bits
/// when a half is absent). The box is stored under the property name on the
/// class's shared `@@prototype`, so a member READ that resolves (through the
/// prototype chain) to this box invokes the getter with `this = receiver`, and a
/// member WRITE invokes the setter. Never a program-visible value — it is always
/// consumed by the get/set path, so `decode`'s catch-all renders it `undefined`.
/// The two i64 payloads land 8-byte aligned at [`ACCESSOR_GET_OFFSET`] /
/// [`ACCESSOR_SET_OFFSET`]. Maps to no `typeof` (never observed directly).
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_ACCESSOR: u8 = 11;

/// Byte offset (from a [`TYPE_ACCESSOR`] block base) of the GETTER function jsval.
/// The payload begins at [`HEADER_SIZE`]; the first i64 lands 8-byte aligned at 8.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const ACCESSOR_GET_OFFSET: u32 = HEADER_SIZE + 4;

/// Byte offset (from a [`TYPE_ACCESSOR`] block base) of the SETTER function jsval,
/// the second 8-byte-aligned i64 right after [`ACCESSOR_GET_OFFSET`].
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const ACCESSOR_SET_OFFSET: u32 = HEADER_SIZE + 12;

/// A compile-time-only interned SENTINEL key present in the string table iff the
/// program lowers at least one instance accessor ([`crate::ir::IrNode::MakeAccessor`]).
/// The emitter gates the accessor get/set invocation paths on its presence —
/// exactly as it gates the `__obj_get` prototype fallback on [`CLASS_PROTOTYPE_KEY`] —
/// so an accessor-free program keeps byte-identical member-read/write emission. It
/// is never read at runtime (the `@@` prefix keeps it off any JS-visible surface).
pub(crate) const ACCESSOR_MARKER_KEY: &str = "@@accessor";

/// The wasm global index of the `this`-binding carrier (`__this`, i64, initial
/// value `undefined`). A method call `obj.m(args)` sets it to the receiver
/// immediately before the `call_indirect`; every function/arrow prologue
/// snapshots it into a function-local `this` slot at entry; and a plain
/// (non-method) call resets it to `undefined`. It follows the
/// completion-routing globals (indices `0..=4`), so it is index `5`. Internal —
/// never exported, since the run stage never reads it.
pub(crate) const THIS_GLOBAL_INDEX: u32 = 5;

/// Reserved hidden property key naming a class's shared PROTOTYPE object.
/// A class binding stores its single prototype object under this key; every
/// instance built by `new` stores that SAME prototype pointer under this key, so
/// a method lookup can resolve `obj.method` by falling back to the prototype when the own
/// property is absent. The `@@` prefix cannot begin a JS identifier, so ordinary
/// `obj.name` source access never collides with it. Interned like any static
/// string key and matched by byte content through `__obj_get`/`__obj_set`.
pub(crate) const CLASS_PROTOTYPE_KEY: &str = "@@prototype";

/// Reserved hidden property key naming a class's CONSTRUCTOR function value.
/// The class binding stores its lowered constructor — an ordinary
/// uniform-ABI function value — under this key; `new A(args)` reads it back and
/// runs it with `this` bound to the fresh instance. Same interning/`@@`-prefix
/// rationale as [`CLASS_PROTOTYPE_KEY`].
pub(crate) const CLASS_CONSTRUCTOR_KEY: &str = "@@constructor";

/// Reserved hidden property key marking a builtin **Error** object's kind
/// (E-series). `new Error`/`new TypeError`/`new RangeError` store one of
/// [`ERROR_KIND_ERROR`] / [`ERROR_KIND_TYPE`] / [`ERROR_KIND_RANGE`] under this
/// key as a number jsval. The `@@` prefix hides it from `for-in` /
/// `Object.keys` / `JSON.stringify` (same mechanism as [`CLASS_PROTOTYPE_KEY`]),
/// yet `__obj_get` reads it back so `instanceof` and the value renderer can
/// recognize an error and its kind. The stored value is never `undefined`, so
/// "the object is an error" is testable as "the loaded marker is not
/// `undefined`", and the concrete kind by strict-equality against the constants
/// below.
pub(crate) const ERROR_KIND_KEY: &str = "@@errorkind";

/// [`ERROR_KIND_KEY`] value identifying a base `Error`.
pub(crate) const ERROR_KIND_ERROR: f64 = 0.0;

/// [`ERROR_KIND_KEY`] value identifying a `TypeError` (which is also an `Error`).
pub(crate) const ERROR_KIND_TYPE: f64 = 1.0;

/// [`ERROR_KIND_KEY`] value identifying a `RangeError` (which is also an `Error`).
pub(crate) const ERROR_KIND_RANGE: f64 = 2.0;

/// The module name of every host env import (`print`/`__convert`/`__method`).
pub(crate) const IMPORT_MODULE: &str = "env";

/// The field name of the host `print` import backing `console.log`.
pub(crate) const PRINT_IMPORT_NAME: &str = "print";

/// The field name of the host `__convert` import backing the
/// `String`/`Number`/`parseInt`/`parseFloat` conversions.
pub(crate) const CONVERT_IMPORT_NAME: &str = "__convert";

/// The field name of the host `__method` import backing the non-callback
/// array/string methods.
pub(crate) const METHOD_IMPORT_NAME: &str = "__method";

/// The field name of the host `__promise` import backing the promise
/// primitives that take at most a single value operand: `Promise.resolve(v)`,
/// `Promise.reject(v)`, top-level `await p`, and `p.finally(fn)`. It receives a
/// `PROMISE_OP_*` flag plus two jsval slots (`a`, `b`) and returns a jsval (a
/// promise for resolve/reject/finally, or the awaited value for await).
pub(crate) const PROMISE_IMPORT_NAME: &str = "__promise";

/// The field name of the host `__then` import backing `p.then(onF, onR)`
/// / `p.catch(onR)`. It receives the promise plus the two reaction handlers
/// (`undefined` for a missing handler) and returns the chained result promise.
pub(crate) const THEN_IMPORT_NAME: &str = "__then";

/// The field name of the host `__now` import backing
/// `Date.now()` / `new Date()`. It takes no arguments and returns the current
/// wall-clock time as an f64 count of milliseconds since the Unix epoch.
pub(crate) const NOW_IMPORT_NAME: &str = "__now";

/// `__promise` op flag: `Promise.resolve(a)` — a promise already fulfilled
/// with `a` (or `a` itself when it is already a promise).
pub(crate) const PROMISE_OP_RESOLVE: i32 = 0;

/// `__promise` op flag: `Promise.reject(a)` — a promise already rejected
/// with reason `a`.
pub(crate) const PROMISE_OP_REJECT: i32 = 1;

/// `__promise` op flag: top-level `await a` — drive the microtask queue
/// until `a` settles, then return its value (or unwind its rejection through the
/// exception globals).
pub(crate) const PROMISE_OP_AWAIT: i32 = 2;

/// `__promise` op flag: `a.finally(b)` — run `b` on settlement, then pass
/// `a`'s original settlement through to the returned chained promise.
pub(crate) const PROMISE_OP_FINALLY: i32 = 3;

/// `__promise` op flag: allocate a fresh PENDING promise (the backing for
/// `new Promise(executor)`). Both value operands are ignored.
pub(crate) const PROMISE_OP_NEW: i32 = 4;

/// `__promise` op flag: fulfill promise `a` with value `b` — the `resolve`
/// half of a `new Promise` executor's settler pair. A no-op if `a` is already
/// settled (settle-once).
pub(crate) const PROMISE_OP_SETTLE_FULFILL: i32 = 5;

/// `__promise` op flag: reject promise `a` with reason `b` — the `reject`
/// half of a `new Promise` executor's settler pair (and the path a throwing
/// executor takes). A no-op if `a` is already settled.
pub(crate) const PROMISE_OP_SETTLE_REJECT: i32 = 6;

/// `__promise` op flag: spawn an `async function` body. Operand `a` is a
/// freshly built generator handle (the async body compiled as a state machine,
/// `await` == `yield`); the host allocates the result promise, drives the
/// generator's first step synchronously, and returns the result promise. Each
/// suspend (`await`) registers a continuation that resumes the generator when the
/// awaited value settles; completion fulfills (or, on a throw / awaited rejection,
/// rejects) the result promise.
pub(crate) const PROMISE_OP_ASYNC_SPAWN: i32 = 7;

/// The name under which the module exports the `__invoke1(fn: i64, arg: i64) ->
/// i64` callback trampoline. The host promise engine calls it re-entrantly
/// to run a `.then`/`.catch`/`.finally` reaction's guest callback with the settled
/// value, receiving the callback's result (or, on a throw, an ignored result plus
/// a set `__exc_pending` global).
pub(crate) const INVOKE_EXPORT_NAME: &str = "__invoke1";

/// The name under which the module exports the `__async_step(gen: i64, sent: i64)
/// -> i64` generator single-step trampoline. The host async driver calls
/// it re-entrantly to advance an `async function` body's state machine by one
/// suspend: it `call_indirect`s the state machine with `(sent, gen)`, returns the
/// yielded/returned value, and leaves the handle's `done` flag (and any thrown
/// exception via `__exc_pending`) for the host to read.
pub(crate) const ASYNC_STEP_EXPORT_NAME: &str = "__async_step";

/// The name under which the module exports its linear memory; [`crate::run`]
/// reads it back to resolve heap pointers.
pub(crate) const MEMORY_EXPORT_NAME: &str = "memory";

/// The name under which the bump-allocator heap-pointer global is exported so
/// the host `__convert`/`__method` imports can allocate in guest memory.
pub(crate) const HEAP_PTR_EXPORT_NAME: &str = "__heap_ptr";

/// The linear-memory cap in 64 KiB wasm pages (512 MiB), declared as the
/// module's memory `maximum`: a guest `__alloc` or host allocation that cannot
/// grow memory within it is out of memory.
pub(crate) const MEMORY_MAX_PAGES: u64 = 8192;

/// The sticky out-of-memory sentinel stored in the heap-pointer global when an
/// allocation cannot grow memory (as `i32`, i.e. `0xFFFF_FFFF`). Every later
/// allocation fails against it, and the run stage reports it as
/// `Uncaught RangeError: out of memory`.
pub(crate) const HEAP_PTR_OOM: i32 = -1;

/// The name under which the pending-exception flag global (`$exc_pending`, i32)
/// is exported so the run stage can detect an uncaught exception after `main`
/// returns. `0` = none, `1` = an exception is propagating.
pub(crate) const EXC_PENDING_EXPORT_NAME: &str = "__exc_pending";

/// The name under which the in-flight exception value global (`$exc_value`, i64)
/// is exported so the run stage can render an uncaught exception.
pub(crate) const EXC_VALUE_EXPORT_NAME: &str = "__exc_value";

/// `print` import flag: format the jsval and follow it with a single
/// space — used for every `console.log` argument except the last.
pub(crate) const PRINT_FLAG_SEP: i32 = 0;

/// `print` import flag: format the jsval and follow it with a newline —
/// used for the last `console.log` argument.
pub(crate) const PRINT_FLAG_END: i32 = 1;

/// `print` import flag: emit only a newline, ignoring the (dummy) jsval
/// operand — used for a zero-argument `console.log()`.
pub(crate) const PRINT_FLAG_NEWLINE: i32 = 2;

/// `__convert` op flag: stringify the argument (`String(x)`).
pub(crate) const CONVERT_OP_TO_STRING: i32 = 0;

/// `__convert` op flag: coerce the argument to a number (`Number(x)`).
pub(crate) const CONVERT_OP_TO_NUMBER: i32 = 1;

/// `__convert` op flag: parse a leading integer (`parseInt(x)`).
pub(crate) const CONVERT_OP_PARSE_INT: i32 = 2;

/// `__convert` op flag: parse a leading float (`parseFloat(x)`).
pub(crate) const CONVERT_OP_PARSE_FLOAT: i32 = 3;

/// `__method` op flag: `indexOf` (overloaded on array/string).
pub(crate) const METHOD_OP_INDEX_OF: i32 = 0;

/// `__method` op flag: `includes`.
pub(crate) const METHOD_OP_INCLUDES: i32 = 1;

/// `__method` op flag: `join`.
pub(crate) const METHOD_OP_JOIN: i32 = 2;

/// `__method` op flag: `slice` (overloaded on array/string).
pub(crate) const METHOD_OP_SLICE: i32 = 3;

/// `__method` op flag: `toUpperCase`.
pub(crate) const METHOD_OP_TO_UPPER: i32 = 4;

/// `__method` op flag: `toLowerCase`.
pub(crate) const METHOD_OP_TO_LOWER: i32 = 5;

/// `__method` op flag: `charAt`.
pub(crate) const METHOD_OP_CHAR_AT: i32 = 6;

/// `__method` op flag: `trim` (string).
pub(crate) const METHOD_OP_TRIM: i32 = 7;

/// `__method` op flag: `trimStart` (string).
pub(crate) const METHOD_OP_TRIM_START: i32 = 8;

/// `__method` op flag: `trimEnd` (string).
pub(crate) const METHOD_OP_TRIM_END: i32 = 9;

/// `__method` op flag: `repeat` (string).
pub(crate) const METHOD_OP_REPEAT: i32 = 10;

/// `__method` op flag: `startsWith` (string).
pub(crate) const METHOD_OP_STARTS_WITH: i32 = 11;

/// `__method` op flag: `endsWith` (string).
pub(crate) const METHOD_OP_ENDS_WITH: i32 = 12;

/// `__method` op flag: `padStart` (string).
pub(crate) const METHOD_OP_PAD_START: i32 = 13;

/// `__method` op flag: `padEnd` (string).
pub(crate) const METHOD_OP_PAD_END: i32 = 14;

/// `__method` op flag: `substring` (string).
pub(crate) const METHOD_OP_SUBSTRING: i32 = 15;

/// `__method` op flag: `split` (string, yields an array).
pub(crate) const METHOD_OP_SPLIT: i32 = 16;
