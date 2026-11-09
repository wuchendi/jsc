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
//! offset 0 (see [`TYPE_STRING`] … [`TYPE_CELL`]) followed by the reserved
//! [`HEADER_GC_OFFSET`], [`HEADER_FLAGS_OFFSET`], and [`HEADER_CLASS_OFFSET`]
//! bytes, so the per-type payload begins at [`HEADER_SIZE`].
//! Every block base is [`BLOCK_ALIGN`]-aligned: `__alloc` rounds each request up
//! and static data blobs are padded to it. The single [`HEAP`] tag boxes
//! strings, arrays, objects, and functions alike; the concrete kind is the TYPE
//! BYTE, not the NaN-box tag.

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

/// An array hole: an element slot with no element (a literal elision, a
/// `delete`d index, or a gap left by growth by index). [`UNDEFINED`] with a
/// nonzero payload, which no JS value produces, so it never escapes to JS: an
/// element read that loads it yields `undefined`. The same bit pattern marks
/// a module cell still in its TDZ.
pub(crate) const HOLE: u64 = UNDEFINED | 1;

/// The `typeof` result strings, in `typeof`-index order. A `typeof` result is a
/// real interned HEAP STRING (tag `0b100` is retired), so these are the words
/// `emit_typeof_node` interns and selects between; the decoder reads them back as
/// ordinary strings. Keeping them a single source shared by the emitter and the
/// runtime prevents the index/word tables from drifting apart.
pub(crate) const TYPEOF_STRINGS: [&str; 7] = [
    "number",
    "boolean",
    "undefined",
    "object",
    "string",
    "function",
    "symbol",
];

/// The NaN-boxed heap-pointer header: [`QNAN`] plus tag `0b101` — the SINGLE tag
/// for every heap value. A heap jsval is `HEAP | (ptr as u32)`, carrying
/// the byte offset of the block's base in its low 32 bits.
pub(crate) const HEAP: u64 = 0x7FFD_0000_0000_0000;

/// The size in bytes of every heap block's leading header: the TYPE BYTE
/// at offset 0 plus the `gc`/`flags`/`class` bytes. The per-type payload begins
/// here.
pub(crate) const HEADER_SIZE: u32 = 4;

/// Header byte 1: the `gc` byte (phase 4: bit 0 = mark). Reserved; every block
/// is created with it 0.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const HEADER_GC_OFFSET: u32 = 1;

/// Header byte 2: the per-kind `flags` byte, `0` by default: a string's
/// [`STR_FLAG_INTERNED`] / [`STR_FLAG_ARRAY_INDEX`] / [`STR_FLAG_TWO_BYTE`], an object-family block's
/// [`OBJ_FLAG_NOT_EXTENSIBLE`], a function's [`FUNC_FLAG_LAZY`] /
/// [`FUNC_FLAG_CONSTRUCTOR`] / [`FUNC_FLAG_CLASS_CTOR`], a symbol's
/// [`SYMBOL_FLAG_REGISTERED`].
pub(crate) const HEADER_FLAGS_OFFSET: u32 = 2;

/// Header byte 3: the `TYPE_OBJECT` sub-`class` byte ([`CLASS_ORDINARY`],
/// [`CLASS_ERROR`]); `0` for every other kind.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const HEADER_CLASS_OFFSET: u32 = 3;

/// [`TYPE_OBJECT`] `class` byte: an ordinary object.
pub(crate) const CLASS_ORDINARY: u8 = 0;

/// [`TYPE_OBJECT`] `class` byte: an Error instance (`[[ErrorData]]`). Its kind
/// is its prototype (`%Error.prototype%`, `%TypeError.prototype%`,
/// `%RangeError.prototype%`, static data-segment objects); it owns a
/// non-enumerable `stack` (and `message` when one was given) and inherits
/// `name`.
pub(crate) const CLASS_ERROR: u8 = 1;

/// [`TYPE_OBJECT`] `class` byte: an (unmapped) `arguments` object. It owns
/// the indexed properties `"0".."n-1"` and a non-enumerable `length`.
pub(crate) const CLASS_ARGUMENTS: u8 = 2;

/// [`TYPE_OBJECT`] `class` byte: a Boolean wrapper object, its
/// `[[PrimitiveValue]]` (the boolean jsval) at [`WRAPPER_VALUE_OFFSET`].
pub(crate) const CLASS_BOOLEAN: u8 = 3;

/// [`TYPE_OBJECT`] `class` byte: a Number wrapper object, its
/// `[[PrimitiveValue]]` (the number jsval) at [`WRAPPER_VALUE_OFFSET`].
pub(crate) const CLASS_NUMBER: u8 = 4;

/// [`TYPE_OBJECT`] `class` byte: a String wrapper object, its
/// `[[PrimitiveValue]]` (the string jsval) at [`WRAPPER_VALUE_OFFSET`]. Its
/// `length` and in-range indices are exotic own read-only properties.
pub(crate) const CLASS_STRING: u8 = 5;

/// [`TYPE_OBJECT`] `class` byte: the global object (a static realm block).
pub(crate) const CLASS_GLOBAL: u8 = 6;

/// [`TYPE_OBJECT`] `class` byte: a Symbol wrapper object (created only by
/// `ToObject`), its `[[SymbolData]]` (the [`TYPE_SYMBOL`] jsval) at
/// [`WRAPPER_VALUE_OFFSET`].
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const CLASS_SYMBOL: u8 = 7;

/// [`TYPE_OBJECT`] `class` byte: a builtin iterator object (an Array, String,
/// Map or Set iterator; DESIGN.md §5.2). Its 32-byte block
/// holds the iterated value, the next index and the iterator kind past the
/// object-family prefix; no host reads the payload.
pub(crate) const CLASS_ITERATOR: u8 = 8;

/// Offset of a wrapper object's `[[PrimitiveValue]]` jsval (an `i64`) from
/// its base, right after the object-family prefix.
pub(crate) const WRAPPER_VALUE_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte size of a wrapper object block.
pub(crate) const WRAPPER_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 8;

/// The alignment in bytes of every heap block base: `__alloc` rounds every
/// request up to a multiple of it, and static data blobs are padded to it, so
/// an `i64` slot at a block offset ≡ 0 (mod 8) is naturally aligned.
pub(crate) const BLOCK_ALIGN: u32 = 8;

/// Byte offset of the object-family prefix's `props: u32` slot (right after the
/// header): the base of the block's [`TYPE_PROPS`] own-property table, `0` = no
/// own named properties. Shared by every object-family kind.
pub(crate) const OBJ_PROPS_OFFSET: u32 = HEADER_SIZE;

/// Object-family header `flags` bit (byte [`HEADER_FLAGS_OFFSET`]): the object
/// is not extensible, so adding an own property throws a `TypeError`.
pub(crate) const OBJ_FLAG_NOT_EXTENSIBLE: u8 = 1;

/// Byte offset of the object-family prefix's `proto: i64` jsval slot (a heap
/// pointer or [`NULL`]), 8-byte aligned. Ordinary objects are created with
/// `%Object.prototype%`, functions with `%Function.prototype%`, generators with
/// `%GeneratorPrototype%`, and the other kinds with their realm prototype
/// (static realm-image blocks).
pub(crate) const OBJ_PROTO_OFFSET: u32 = 8;

/// The byte size of the shared object-family prefix — header, `props`, and
/// `proto` — so every kind payload begins 8-byte aligned here.
pub(crate) const OBJ_PREFIX_SIZE: u32 = 16;

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

/// TYPE BYTE of an array handle: the object-family prefix, then
/// `[len:u32 @16][elems:u32 @20]` ([`ARRAY_BLOCK_SIZE`] bytes). `elems` is the
/// base of the [`TYPE_ELEMS`] store holding the elements (`0` while the
/// capacity is 0). Growth replaces the store and rewrites `elems`, so the handle
/// never moves. `props` holds the named properties, and on an
/// [`ARRAY_FLAG_SPARSE`] array also the elements past the store. Maps to
/// `typeof` "object".
pub(crate) const TYPE_ARRAY: u8 = 1;

/// Byte offset (from a [`TYPE_ARRAY`] handle base) of the u32 `length`.
pub(crate) const ARRAY_LEN_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte offset (from a [`TYPE_ARRAY`] handle base) of the u32 base of its
/// [`TYPE_ELEMS`] store, `0` when the capacity is 0.
pub(crate) const ARRAY_ELEMS_OFFSET: u32 = OBJ_PREFIX_SIZE + 4;

/// The byte size of a [`TYPE_ARRAY`] handle.
pub(crate) const ARRAY_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 8;

/// [`TYPE_ARRAY`] header `flags` bit: the array is sparse. Its store keeps
/// elements `0..cap` (holes past `length`); every element at or past `cap` is
/// an array-index key in its `props` table, so `length` may exceed the store.
/// Set by a store or `length` write past the dense-growth bound; never cleared.
pub(crate) const ARRAY_FLAG_SPARSE: u8 = 4;

/// [`TYPE_ARRAY`] header `flags` bit: the array is being joined (`join`,
/// `Array.prototype.toString`). Set for the duration of the guest join so a
/// cyclic element joins as the empty string; cleared on every exit.
pub(crate) const ARRAY_FLAG_JOINING: u8 = 8;

/// [`TYPE_ARRAY`] header `flags` bit: the array's `length` is non-writable
/// (`defineProperty(a, "length", {writable: false})`, `freeze`), so every
/// `length` write and every store that would grow the array throws. Only set
/// on an [`ARRAY_FLAG_DICTIONARY`] array; never cleared.
pub(crate) const ARRAY_FLAG_LENGTH_READONLY: u8 = 16;

/// [`TYPE_ARRAY`] header `flags` bit: the array is in dictionary mode — it is
/// [`ARRAY_FLAG_SPARSE`] with no store (`elems = 0`), so every element is an
/// array-index key of its `props` table carrying its own attributes. Set before
/// an element takes non-default attributes, before the array becomes
/// non-extensible, and before its `length` becomes read-only; never cleared.
/// The builtin fast paths skip such an array.
pub(crate) const ARRAY_FLAG_DICTIONARY: u8 = 32;

/// TYPE BYTE of an ordinary object block: the object-family prefix alone
/// ([`OBJ_PREFIX_SIZE`] bytes, `class` byte 0) — its own properties live in the
/// [`TYPE_PROPS`] table at `props`, its prototype in `proto`. Maps to `typeof`
/// "object".
pub(crate) const TYPE_OBJECT: u8 = 2;

/// TYPE BYTE of a first-class function value: the object-family prefix, then
/// `[func_idx:u32 @16][reserved:u32 @20][env:i64 @24]` ([`FUNC_BLOCK_SIZE`]
/// bytes). Maps to `typeof` "function".
pub(crate) const TYPE_FUNCTION: u8 = 3;

/// Byte offset (from a [`TYPE_FUNCTION`] block base) of the u32 funcref-table
/// index. The following u32 at `+20` is reserved (always 0).
pub(crate) const FUNC_INDEX_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte offset (from a [`TYPE_FUNCTION`] block base) of the i64 closure
/// environment jsval (`undefined` for a non-capturing function).
pub(crate) const FUNC_ENV_OFFSET: u32 = OBJ_PREFIX_SIZE + 8;

/// The byte size of a [`TYPE_FUNCTION`] block.
pub(crate) const FUNC_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 16;

/// [`TYPE_FUNCTION`] header `flags` bit: `length`, `name`, and (for a
/// [`FUNC_FLAG_CONSTRUCTOR`]) `prototype` are not yet defined. The first
/// property operation on the function calls `__fn_materialize`, which defines
/// them from the function's [`FN_INFO_ENTRY_SIZE`]-byte info entry and clears
/// the bit.
pub(crate) const FUNC_FLAG_LAZY: u8 = 4;

/// [`TYPE_FUNCTION`] header `flags` bit: the function is a constructor (a
/// `function` declaration/expression or a class), so materialization gives it
/// a `prototype` object.
pub(crate) const FUNC_FLAG_CONSTRUCTOR: u8 = 8;

/// [`TYPE_FUNCTION`] header `flags` bit: the function is a class constructor.
/// Its `prototype` is installed by the class definition, not materialized.
pub(crate) const FUNC_FLAG_CLASS_CTOR: u8 = 16;

/// [`TYPE_FUNCTION`] header `flags` bit: the constructor allocates its own
/// `this` (a derived class: `super(..)` constructs it), so `new` skips the
/// allocation.
pub(crate) const FUNC_FLAG_SELF_ALLOC: u8 = 32;

/// [`TYPE_FUNCTION`] header `flags` bit: a bound function exotic object
/// (`Function.prototype.bind`). Its `func_idx` is the bound-call trampoline
/// and its `env` an array `[target, boundThis, ...boundArgs]`; its own
/// `length` and `name` are defined at creation (it is never `LAZY`).
pub(crate) const FUNC_FLAG_BOUND: u8 = 64;

/// [`TYPE_FUNCTION`] header `flags` bit: a generator function (`function*`
/// or a generator method). Not a constructor; materialization gives it a
/// `prototype` object inheriting `%GeneratorPrototype%` with no
/// `constructor`.
pub(crate) const FUNC_FLAG_GENERATOR: u8 = 128;

/// [`TYPE_FUNCTION`] header `flags` bit, with [`FUNC_FLAG_GENERATOR`]: an
/// async generator function, whose `prototype` inherits
/// `%AsyncGeneratorPrototype%` instead.
pub(crate) const FUNC_FLAG_ASYNC: u8 = 2;

/// Size in bytes of the shadow-stack region `[0, SHADOW_STACK_SIZE)`. The stack
/// body is `[SHADOW_FP_BIAS, SHADOW_STACK_SIZE)`, growing down from its end
/// (`$__sp` starts at `SHADOW_STACK_SIZE - SHADOW_FP_BIAS`). See DESIGN.md §4.2.
pub(crate) const SHADOW_STACK_SIZE: u32 = 1 << 20;

/// Frame-pointer bias: slot `k` is at `$fp + SHADOW_FP_BIAS + 8k`, so addresses
/// `0..8` never hold stack data and a raw ref of `0` always reads zero.
pub(crate) const SHADOW_FP_BIAS: u32 = 8;

/// Linear-memory address of the u32 holding the base of the static function
/// info table (inside the null guard below the first static string): entry
/// `func_idx` is [`FN_INFO_ENTRY_SIZE`] bytes at `base + FN_INFO_ENTRY_SIZE *
/// func_idx`, holding the function's interned `name` key at
/// [`FN_INFO_NAME_OFFSET`] and its `length` at [`FN_INFO_LENGTH_OFFSET`]
/// (`__fn_info`). `__fn_materialize` and the host renderers read it.
pub(crate) const FN_INFO_ADDR: u32 = SHADOW_STACK_SIZE + 4;

/// The byte size of one function info entry (see [`FN_INFO_ADDR`]).
pub(crate) const FN_INFO_ENTRY_SIZE: u32 = 8;

/// Byte offset (within a function info entry) of the u32 interned `name` key.
pub(crate) const FN_INFO_NAME_OFFSET: u32 = 0;

/// Byte offset (within a function info entry) of the u32 `length`.
pub(crate) const FN_INFO_LENGTH_OFFSET: u32 = 4;

/// TYPE BYTE of a closure **environment** block (internal):
/// `[type=4][pad×3][count:u32][cell_ptr:i64 × count]`. Never a user value.
pub(crate) const TYPE_ENV: u8 = 4;

/// TYPE BYTE of a **boxed cell** (internal): `[type=5][pad×3][value:i64]`
/// holding a captured binding so closures share mutations by reference. Never a
/// user value.
pub(crate) const TYPE_CELL: u8 = 5;

/// TYPE BYTE of a `Map` handle block: the object-family prefix, then
/// `[size:u32 @16][pad:u32][entries:i64 @24]`. `size` is the logical entry count
/// (backing `.size`); `entries` is a heap jsval pointing at a [`TYPE_ARRAY`]
/// that stores the `(key, value)` pairs flattened (key at even element slots,
/// value at the following odd slot) and grows in place like any array. Maps to
/// `typeof` "object".
pub(crate) const TYPE_MAP: u8 = 6;

/// TYPE BYTE of a `Set` handle block: identical layout to [`TYPE_MAP`]. A Set reuses the same flattened
/// `(key, value)` backing with the value slot always `undefined`, so all the
/// lookup/insert/delete machinery is shared; only the type byte (and thus the
/// render label) differs. Maps to `typeof` "object".
pub(crate) const TYPE_SET: u8 = 7;

/// Byte offset (from a [`TYPE_MAP`] / [`TYPE_SET`] block base) of the u32
/// logical entry count.
pub(crate) const MAP_SIZE_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte offset (from a [`TYPE_MAP`] / [`TYPE_SET`] block base) of the i64
/// `entries` backing-array jsval.
pub(crate) const MAP_ENTRIES_OFFSET: u32 = OBJ_PREFIX_SIZE + 8;

/// The byte size of a [`TYPE_MAP`] / [`TYPE_SET`] handle block.
pub(crate) const MAP_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 16;

/// TYPE BYTE of a `Promise` block: the object-family prefix, then
/// `[state:i32 @16][pad:u32][value:i64 @24]`.
/// `state` is one of [`PROMISE_PENDING`] / [`PROMISE_FULFILLED`] /
/// [`PROMISE_REJECTED`]; `value` is the settled fulfillment/rejection jsval
/// (`undefined` while pending). The block is the RENDERABLE mirror of a promise's
/// settlement — the host promise engine (reactions + microtask queue) owns the
/// dynamic scheduling state, but writes `state`/`value` back here so [`crate::run`]
/// can decode a promise from a bare memory snapshot. Maps to `typeof` "object".
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_PROMISE: u8 = 8;

/// Byte offset (from a [`TYPE_PROMISE`] block base) of the i32 `state`.
pub(crate) const PROMISE_STATE_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte offset (from a [`TYPE_PROMISE`] block base) of the i64 settled `value`.
pub(crate) const PROMISE_VALUE_OFFSET: u32 = OBJ_PREFIX_SIZE + 8;

/// The byte size of a [`TYPE_PROMISE`] block.
pub(crate) const PROMISE_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 16;

/// [`TYPE_PROMISE`] `state`: the promise is unsettled.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_PENDING: i32 = 0;

/// [`TYPE_PROMISE`] `state`: the promise fulfilled with its `value`.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_FULFILLED: i32 = 1;

/// [`TYPE_PROMISE`] `state`: the promise rejected with its `value` as the reason.
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const PROMISE_REJECTED: i32 = 2;

/// TYPE BYTE of a `Generator` handle block: the object-family prefix, then
/// `[sm_idx:u32 @16][state:i32 @20][done:i32 @24][pad:u32][env:i64 @32]`. `sm_idx` is the
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

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the u32 `sm_idx`
/// funcref-table index.
pub(crate) const GEN_SM_IDX_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the i32 resume `state`.
pub(crate) const GEN_STATE_OFFSET: u32 = OBJ_PREFIX_SIZE + 4;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the i32 `done` flag.
pub(crate) const GEN_DONE_OFFSET: u32 = OBJ_PREFIX_SIZE + 8;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the u32 `limits`
/// bits ([`GEN_RUNNING`] .. [`GEN_DELEGATED_RESULT`], the [`GEN_RESUME_MODE_MASK`]
/// field), guest-only.
pub(crate) const GEN_LIMITS_OFFSET: u32 = OBJ_PREFIX_SIZE + 12;

/// Generator `limits` field: the resumption mode of a resume entered with an
/// exception pending — [`GEN_RESUME_MODE_THROW`] (the pending value is
/// thrown at the suspended `yield`) or [`GEN_RESUME_MODE_RETURN`] (it is the
/// value of a return completion injected there). Written by `throw()` /
/// `return()` (and the async generator request serving) before the resume,
/// consumed by the resumed `yield`; a resume without a pending exception is
/// `next(v)` whatever the field holds.
pub(crate) const GEN_RESUME_MODE_MASK: u32 = 0x300;

/// The shift of the [`GEN_RESUME_MODE_MASK`] field.
pub(crate) const GEN_RESUME_MODE_SHIFT: u32 = 8;

/// [`GEN_RESUME_MODE_MASK`] value: `next(v)`.
pub(crate) const GEN_RESUME_MODE_NEXT: u32 = 0;

/// [`GEN_RESUME_MODE_MASK`] value: `throw(e)`.
pub(crate) const GEN_RESUME_MODE_THROW: u32 = 1;

/// [`GEN_RESUME_MODE_MASK`] value: `return(v)`.
pub(crate) const GEN_RESUME_MODE_RETURN: u32 = 2;

/// Generator `limits` bit: the state machine is running (set around each
/// resume), so a `next()` / `return()` / `throw()` from its own body throws.
pub(crate) const GEN_RUNNING: u32 = 4;

/// Generator `limits` bit: an async generator (`async function*`), driven
/// by `%AsyncGeneratorPrototype%` through its request queue; never a sync
/// iterable.
pub(crate) const GEN_ASYNC: u32 = 8;

/// Async generator `limits` bit: the state machine's last suspend was an
/// `await` (the driver clears it before each step), not a `yield`.
pub(crate) const GEN_AWAIT: u32 = 16;

/// Async generator `limits` bit: the body is executing or awaiting (an
/// `await` or a `return()` value), so a queued request waits.
pub(crate) const GEN_BUSY: u32 = 32;

/// Generator `limits` bit, set at construction: the body has a `yield*`, so
/// a resume may pass the delegate's result object through
/// ([`GEN_DELEGATED_RESULT`]); such a handle never takes the direct
/// state-machine iteration fast path.
pub(crate) const GEN_DELEGATES: u32 = 64;

/// Generator `limits` bit: the state machine's last suspend was a `yield*`
/// step returning the delegate's result object itself (not a value to
/// wrap), which the resumer returns as-is and clears the bit.
pub(crate) const GEN_DELEGATED_RESULT: u32 = 128;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the i64 `env` frame
/// jsval, 8-byte aligned past the `limits` word.
pub(crate) const GEN_ENV_OFFSET: u32 = OBJ_PREFIX_SIZE + 16;

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of an async
/// generator's u32 request-queue head (`0` when empty), guest-only; the
/// tail follows at `+ 4`.
pub(crate) const GEN_QUEUE_OFFSET: u32 = OBJ_PREFIX_SIZE + 24;

/// The byte size of a [`TYPE_GENERATOR`] block.
pub(crate) const GEN_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 32;

/// TYPE BYTE of a `Date` block: the object-family prefix, then `[ms:f64 @16]`. The
/// single f64 payload is the timestamp in milliseconds since the Unix epoch — the
/// same value `Date.now()` / `d.getTime()` yield. `new Date(ms)` stores its
/// numeric argument; `new Date()` stores the WASI clock (the guest `__now`).
/// Maps to `typeof` "object".
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const TYPE_DATE: u8 = 10;

/// Byte offset (from a [`TYPE_DATE`] block base) of the f64 `ms` timestamp.
pub(crate) const DATE_MS_OFFSET: u32 = OBJ_PREFIX_SIZE;

/// The byte size of a [`TYPE_DATE`] block.
pub(crate) const DATE_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 8;

/// TYPE BYTE of an **accessor** block (class and object-literal getters/setters):
/// `[type=11][pad×3][pad×4][getter:i64][setter:i64]`. It holds an accessor
/// property's getter and setter FUNCTION VALUES (the `undefined` bits
/// when a half is absent). It is the value of an [`ATTR_ACCESSOR`] property
/// table entry, so `__get` invokes the getter with `this = receiver` and
/// `__set` the setter. Never a program-visible value; the host decoders render
/// the property as `[Getter]` / `[Setter]` / `[Getter/Setter]`.
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

/// The byte size of a [`TYPE_ACCESSOR`] block: the header, 4 padding bytes and
/// the two getter/setter jsvals.
pub(crate) const ACCESSOR_BLOCK_SIZE: u32 = HEADER_SIZE + 4 + 8 + 8;

/// TYPE BYTE of an own-property table (internal, never a user value), the
/// block an object-family `props` slot points at:
/// `[type=12][pad][tflags:u8][watch bit:u8][count:u32 @4][cap:u32 @8][live:u32 @12]`
/// followed by `cap` [`PROP_ENTRY_SIZE`]-byte entries
/// `[key:u32][attrs:u32][value:i64]` from [`PROPS_HEADER_SIZE`]. `key` is an
/// interned string block base (`0` = a deleted entry's tombstone), `count` the
/// slots used (tombstones included) and `live` the non-tombstone entries.
/// Insertion order is slot order; growth copies the live entries into a fresh
/// table and rewrites the owner's `props`, so the owner handle never moves.
pub(crate) const TYPE_PROPS: u8 = 12;

/// Byte offset (from a [`TYPE_PROPS`] block base) of the `tflags` byte
/// ([`PROPS_HAS_INDEX_KEYS`], [`PROPS_HAS_NON_ENUM`], [`PROPS_WATCHED`]).
pub(crate) const PROPS_TFLAGS_OFFSET: u32 = 2;

/// Byte offset (from a [`TYPE_PROPS`] block base) of the u32 `count`.
pub(crate) const PROPS_COUNT_OFFSET: u32 = 4;

/// Byte offset (from a [`TYPE_PROPS`] block base) of the u32 `cap`.
pub(crate) const PROPS_CAP_OFFSET: u32 = 8;

/// Byte offset (from a [`TYPE_PROPS`] block base) of the u32 `live`.
pub(crate) const PROPS_LIVE_OFFSET: u32 = 12;

/// The byte size of a [`TYPE_PROPS`] table header; entry `k` starts at
/// `PROPS_HEADER_SIZE + PROP_ENTRY_SIZE * k`.
pub(crate) const PROPS_HEADER_SIZE: u32 = 16;

/// The byte size of one [`TYPE_PROPS`] entry.
pub(crate) const PROP_ENTRY_SIZE: u32 = 16;

/// Byte offset (within a [`TYPE_PROPS`] entry) of the u32 interned key.
pub(crate) const PROP_KEY_OFFSET: u32 = 0;

/// Byte offset (within a [`TYPE_PROPS`] entry) of the u32 `attrs`.
pub(crate) const PROP_ATTRS_OFFSET: u32 = 4;

/// Byte offset (within a [`TYPE_PROPS`] entry) of the i64 value (a data value,
/// or a [`TYPE_ACCESSOR`] pointer when [`ATTR_ACCESSOR`] is set).
pub(crate) const PROP_VALUE_OFFSET: u32 = 8;

/// [`TYPE_PROPS`] `tflags` bit: some key is a canonical array index (its
/// string carries [`STR_FLAG_ARRAY_INDEX`]), so own-key enumeration must list
/// the integer keys first, ascending.
pub(crate) const PROPS_HAS_INDEX_KEYS: u8 = 1;

/// [`TYPE_PROPS`] `tflags` bit: some entry lacks [`ATTR_ENUMERABLE`].
pub(crate) const PROPS_HAS_NON_ENUM: u8 = 2;

/// [`TYPE_PROPS`] `tflags` bit: the table belongs to a watched intrinsic
/// (DESIGN.md §6.4); its byte [`PROPS_WATCH_BIT_OFFSET`] holds the intrinsic's
/// dirty-bit index. A rebuilt table keeps both.
pub(crate) const PROPS_WATCHED: u8 = 4;

/// [`TYPE_PROPS`] `tflags` bit: some key is a [`TYPE_SYMBOL`] block, so string
/// key enumeration must skip it and symbol enumeration has entries to list.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const PROPS_HAS_SYMBOL_KEYS: u8 = 8;

/// Byte offset (from a [`TYPE_PROPS`] block base) of a watched table's u8
/// dirty-bit index.
pub(crate) const PROPS_WATCH_BIT_OFFSET: u32 = 3;

/// TYPE BYTE of an array element store (internal, never a user value), the
/// block a [`TYPE_ARRAY`] handle's `elems` slot points at:
/// `[type=13][pad×3][cap:u32 @4]` followed by `cap` element jsvals from
/// [`ELEMS_DATA_OFFSET`] (`8 + 8 * cap` bytes). Slots at and past the owner's
/// `length` are unused.
pub(crate) const TYPE_ELEMS: u8 = 13;

/// Byte offset (from a [`TYPE_ELEMS`] block base) of the u32 `cap`.
pub(crate) const ELEMS_CAP_OFFSET: u32 = HEADER_SIZE;

/// Byte offset (from a [`TYPE_ELEMS`] block base) of element 0; element `i`
/// is the jsval at `ELEMS_DATA_OFFSET + 8 * i`.
pub(crate) const ELEMS_DATA_OFFSET: u32 = 8;

/// TYPE BYTE of a Symbol primitive: a heap block under the single [`HEAP`]
/// tag, so its jsval's low 32 bits double as a property key (a `u32` block
/// pointer, like an interned string's). `typeof` is `"symbol"`. Layout:
/// `[type=14][gc][flags][class=0][reserved:u32 = 0][description:i64]`, the
/// description (a string jsval or `undefined`) at
/// [`SYMBOL_DESCRIPTION_OFFSET`], [`SYMBOL_BLOCK_SIZE`] bytes. The flags are
/// [`SYMBOL_FLAG_REGISTERED`] and [`SYMBOL_FLAG_PRIVATE`]; bits `1` and `2`
/// stay clear so a symbol key
/// never looks like an interned string ([`STR_FLAG_INTERNED`]) or an array
/// index ([`STR_FLAG_ARRAY_INDEX`]), and the reserved word at offset 4 reads
/// as "no own properties" if a symbol is mistaken for an object-family block.
/// The description is immutable; identity is the block address. Well-known
/// symbols are static blocks in the realm image.
pub(crate) const TYPE_SYMBOL: u8 = 14;

/// TYPE BYTE of an internal raw buffer: `[type=19][pad×3][len:u32][data]`,
/// a collector leaf whose `len` bytes from offset `8` hold no references (the
/// grown intern table, the `Math.sumPrecise` accumulator, the console render
/// output, path and ref buffers). Never a user value.
pub(crate) const TYPE_RAW: u8 = 19;

/// Byte offset (from a [`TYPE_SYMBOL`] block base) of the `description` jsval
/// (an `i64`: a string jsval or `undefined`).
#[cfg_attr(not(feature = "native-run"), allow(dead_code))]
pub(crate) const SYMBOL_DESCRIPTION_OFFSET: u32 = 8;

/// Byte size of a [`TYPE_SYMBOL`] block.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SYMBOL_BLOCK_SIZE: u32 = 16;

/// [`TYPE_SYMBOL`] header `flags` bit (byte [`HEADER_FLAGS_OFFSET`]): the
/// symbol is in the `Symbol.for` registry (its description is the registry
/// key), so `Symbol.keyFor` returns the description.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SYMBOL_FLAG_REGISTERED: u8 = 4;

/// [`TYPE_SYMBOL`] header `flags` bit: the symbol is a private name (`#x`,
/// DESIGN.md §7.6), never registered and never yielded by an expression. A
/// property keyed by one is invisible to every key walk, and `__define` adds
/// it to a non-extensible object.
pub(crate) const SYMBOL_FLAG_PRIVATE: u8 = 8;

/// Property `attrs` bit: a data property's value may be reassigned (ignored
/// for an accessor).
pub(crate) const ATTR_WRITABLE: u32 = 1;

/// Property `attrs` bit: the key is listed by own-key enumeration.
pub(crate) const ATTR_ENUMERABLE: u32 = 2;

/// Property `attrs` bit: the property may be deleted or redefined.
pub(crate) const ATTR_CONFIGURABLE: u32 = 4;

/// Property `attrs` bit: the value is a [`TYPE_ACCESSOR`] getter/setter pair.
pub(crate) const ATTR_ACCESSOR: u32 = 8;

/// The `attrs` of a data property created by assignment or a literal:
/// writable, enumerable, and configurable.
pub(crate) const ATTRS_DEFAULT: u32 = ATTR_WRITABLE | ATTR_ENUMERABLE | ATTR_CONFIGURABLE;

/// String header `flags` bit (byte [`HEADER_FLAGS_OFFSET`]): the block is the
/// canonical interned block for its content, so it may be used as a property
/// key by pointer.
pub(crate) const STR_FLAG_INTERNED: u8 = 1;

/// String header `flags` bit: the content is a canonical array index
/// (`"0"` or a nonzero digit followed by digits, value `< 2^32 - 1`). Set at
/// intern time.
pub(crate) const STR_FLAG_ARRAY_INDEX: u8 = 2;

/// String header `flags` bit: the data is UTF-16LE code units (`2 * len`
/// bytes); absent, it is Latin-1 bytes (`len` bytes). Canonical: set iff some
/// unit is above `0xFF`. It shares bit `4` with [`SYMBOL_FLAG_REGISTERED`],
/// which cannot collide: string paths read it only on a [`TYPE_STRING`]
/// block, and a symbol key never reaches them (unlike `1` / `2`, which key
/// paths test on any key block).
pub(crate) const STR_FLAG_TWO_BYTE: u8 = 4;

/// Byte offset of a [`TYPE_STRING`] block's `len: u32`, the length in UTF-16
/// code units (the JS `.length`). String block = header | `len: u32` @4 |
/// data @8 (Latin-1 bytes, or UTF-16LE units with [`STR_FLAG_TWO_BYTE`]).
pub(crate) const STR_LEN_OFFSET: u32 = 4;

/// Byte offset of a [`TYPE_STRING`] block's data (see [`STR_LEN_OFFSET`]).
pub(crate) const STR_DATA_OFFSET: u32 = 8;

/// The wasm global index of the `this`-binding carrier (`__this`, i64, initial
/// value `undefined`). A method call `obj.m(args)` sets it to the receiver
/// immediately before the `call_indirect`; every function/arrow prologue
/// snapshots it into a function-local `this` slot at entry; and a plain
/// (non-method) call resets it to `undefined`. It follows the
/// completion-routing globals (indices `0..=4`), so it is index `5`. Internal —
/// never exported, since the run stage never reads it.
pub(crate) const THIS_GLOBAL_INDEX: u32 = 5;

/// The module name of the host env imports not yet replaced by guest code.
pub(crate) const IMPORT_MODULE: &str = "env";

/// The import module of the four WASI preview-1 functions.
pub(crate) const WASI_MODULE: &str = "wasi_snapshot_preview1";

/// The WASI `fd_write` import.
pub(crate) const WASI_FD_WRITE_NAME: &str = "fd_write";

/// The WASI `clock_time_get` import.
pub(crate) const WASI_CLOCK_TIME_GET_NAME: &str = "clock_time_get";

/// The WASI `random_get` import.
pub(crate) const WASI_RANDOM_GET_NAME: &str = "random_get";

/// The WASI `proc_exit` import.
pub(crate) const WASI_PROC_EXIT_NAME: &str = "proc_exit";

/// The command-module entry point export.
pub(crate) const START_EXPORT_NAME: &str = "_start";

/// Address of the static WASI scratch area, 40 bytes: one iovec, `nwritten`,
/// the 8-byte clock result and the 16-byte random seed buffer.
pub(crate) const WASI_SCRATCH: u32 = SHADOW_STACK_SIZE + 8;

/// Address of the `nwritten` word of the WASI scratch area.
pub(crate) const WASI_NWRITTEN: u32 = WASI_SCRATCH + 8;

/// Address of the 8-byte `clock_time_get` result in the WASI scratch area.
pub(crate) const WASI_CLOCK: u32 = WASI_SCRATCH + 16;

/// Address of the 16-byte `random_get` seed buffer in the WASI scratch area.
pub(crate) const WASI_SEED: u32 = WASI_SCRATCH + 24;

/// Address of the static iovec of [`OOM_LINE`], right after the scratch area.
pub(crate) const OOM_IOVEC: u32 = WASI_SCRATCH + 40;

/// Address of the [`OOM_LINE`] bytes, right after its iovec.
pub(crate) const OOM_LINE_ADDR: u32 = OOM_IOVEC + 8;

/// The line `__alloc` writes to fd 2 before `proc_exit(1)`.
pub(crate) const OOM_LINE: &str = "Uncaught RangeError: out of memory\n";

/// Address of the static decimal-conversion scratch, 784 bytes: the
/// `num_digits`, `decimal_point` and `truncated` words, the end index of the
/// last parsed literal, then the 768-digit buffer.
pub(crate) const DEC_SCRATCH: u32 = OOM_LINE_ADDR + 40;

/// Address of the 23 exact `f64` powers of ten (`10^0..=10^22`).
pub(crate) const DEC_POW10: u32 = DEC_SCRATCH + 784;

/// Address of the 65-entry `u16` decimal left-shift table.
pub(crate) const DEC_SHIFT_TABLE: u32 = DEC_POW10 + 184;

/// Address of the packed decimal digits of `5^1..=5^63`.
pub(crate) const DEC_POW5: u32 = DEC_SHIFT_TABLE + 130;

/// Byte length of the digits at [`DEC_POW5`].
pub(crate) const DEC_POW5_LEN: u32 = 1440;

/// Address of the 19-entry table of decimal-conversion shift steps.
pub(crate) const DEC_POWERS: u32 = DEC_POW5 + DEC_POW5_LEN;

/// The first address after the fixed static area, where the string blocks begin.
pub(crate) const STATIC_AREA_END: u32 = RYU_TABLES_ADDR + RYU_TABLES_SIZE;

/// Address of the static `Number` formatting scratch, right after the
/// decimal-conversion tables, 16-aligned.
pub(crate) const NUM_SCRATCH_ADDR: u32 = (DEC_POWERS + 19 + 15) & !15;

/// Size of [`NUM_SCRATCH_ADDR`].
pub(crate) const NUM_SCRATCH_SIZE: u32 = 2048;

/// Address of the static Ryu power-of-5 tables, right after the scratch.
pub(crate) const RYU_TABLES_ADDR: u32 = NUM_SCRATCH_ADDR + NUM_SCRATCH_SIZE;

/// Size of the Ryu tables (828 bytes), rounded up to keep the string blocks
/// aligned.
pub(crate) const RYU_TABLES_SIZE: u32 = 832;

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

/// `__promise` op flag: drain the microtask queue and return the first
/// unhandled rejected promise, or `0` if none. Deleted in R4.
pub(crate) const PROMISE_OP_DRAIN: i32 = 8;

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

/// The name under which the module exports the `__new_string(len: i32,
/// two_byte: i32) -> base: i32` constructor: a [`TYPE_STRING`] block of `len`
/// code units — `len` Latin-1 bytes, or with `two_byte` (`0`/`1`)
/// [`STR_FLAG_TWO_BYTE`] set and `2 * len` bytes of UTF-16LE units — with
/// `len` set and the data at [`STR_DATA_OFFSET`] for the caller to fill. The
/// hosts allocate every string through it, in canonical form (one-byte iff
/// every unit is `<= 0xFF`).
pub(crate) const NEW_STRING_EXPORT_NAME: &str = "__new_string";

/// The name under which the module exports the `__new_promise() -> base: i32`
/// constructor: a pending [`TYPE_PROMISE`] block (value `undefined`). The hosts
/// allocate every promise through it.
pub(crate) const NEW_PROMISE_EXPORT_NAME: &str = "__new_promise";

/// The name under which the module exports the `__promise_self_error() -> i64`
/// constructor: a fresh `TypeError` ("Chaining cycle detected for promise
/// #<Promise>"). The host promise engines reject a promise resolved with
/// itself (Promise Resolve Functions step 7) with it.
pub(crate) const PROMISE_SELF_ERROR_EXPORT_NAME: &str = "__promise_self_error";

/// The name under which the module exports its linear memory; [`crate::run`]
/// reads it back to resolve heap pointers.
pub(crate) const MEMORY_EXPORT_NAME: &str = "memory";

/// The linear-memory cap in 64 KiB wasm pages (512 MiB), declared as the
/// module's memory `maximum`: a guest `__alloc` or host allocation that cannot
/// grow memory within it is out of memory.
pub(crate) const MEMORY_MAX_PAGES: u64 = 8192;

/// The name under which the pending-exception flag global (`$exc_pending`, i32)
/// is exported so the run stage can detect an uncaught exception after `main`
/// returns. `0` = none, `1` = an exception is propagating.
pub(crate) const EXC_PENDING_EXPORT_NAME: &str = "__exc_pending";

/// The name under which the in-flight exception value global (`$exc_value`, i64)
/// is exported so the run stage can render an uncaught exception.
pub(crate) const EXC_VALUE_EXPORT_NAME: &str = "__exc_value";

#[cfg(test)]
#[path = "abi_drift_tests.rs"]
mod drift_tests;
