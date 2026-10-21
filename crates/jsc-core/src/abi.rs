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
/// at offset 0 plus the `gc`/`flags`/`class` bytes. The per-type payload begins
/// here.
pub(crate) const HEADER_SIZE: u32 = 4;

/// Header byte 1: the `gc` byte (phase 4: bit 0 = mark). Reserved; every block
/// is created with it 0.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const HEADER_GC_OFFSET: u32 = 1;

/// Header byte 2: the per-kind `flags` byte, `0` by default: a string's
/// [`STR_FLAG_INTERNED`] / [`STR_FLAG_ARRAY_INDEX`], an object-family block's
/// [`OBJ_FLAG_NOT_EXTENSIBLE`], a function's [`FUNC_FLAG_LAZY`] /
/// [`FUNC_FLAG_CONSTRUCTOR`] / [`FUNC_FLAG_CLASS_CTOR`].
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
/// `%Object.prototype%` and functions with `%Function.prototype%` (static
/// realm-image blocks); the other kinds with [`NULL`] until their intrinsic
/// prototypes exist.
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

/// Linear-memory address of the u32 holding the base of the static function
/// info table (inside the null guard below the first static string): entry
/// `func_idx` is [`FN_INFO_ENTRY_SIZE`] bytes at `base + FN_INFO_ENTRY_SIZE *
/// func_idx`, holding the function's interned `name` key at
/// [`FN_INFO_NAME_OFFSET`] and its `length` at [`FN_INFO_LENGTH_OFFSET`]
/// (`__fn_info`). `__fn_materialize` and the host renderers read it.
pub(crate) const FN_INFO_ADDR: u32 = 4;

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

/// Byte offset (from a [`TYPE_GENERATOR`] block base) of the i64 `env` frame
/// jsval, 8-byte aligned past a u32 pad.
pub(crate) const GEN_ENV_OFFSET: u32 = OBJ_PREFIX_SIZE + 16;

/// The byte size of a [`TYPE_GENERATOR`] block.
pub(crate) const GEN_BLOCK_SIZE: u32 = OBJ_PREFIX_SIZE + 24;

/// TYPE BYTE of a `Date` block: the object-family prefix, then `[ms:f64 @16]`. The
/// single f64 payload is the timestamp in milliseconds since the Unix epoch — the
/// same value `Date.now()` / `d.getTime()` yield. `new Date(ms)` stores its
/// numeric argument; `new Date()` stores the host clock (the `__now` import).
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
/// (design 3.3); its byte [`PROPS_WATCH_BIT_OFFSET`] holds the intrinsic's
/// dirty-bit index. A rebuilt table keeps both.
pub(crate) const PROPS_WATCHED: u8 = 4;

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

/// The wasm global index of the `this`-binding carrier (`__this`, i64, initial
/// value `undefined`). A method call `obj.m(args)` sets it to the receiver
/// immediately before the `call_indirect`; every function/arrow prologue
/// snapshots it into a function-local `this` slot at entry; and a plain
/// (non-method) call resets it to `undefined`. It follows the
/// completion-routing globals (indices `0..=4`), so it is index `5`. Internal —
/// never exported, since the run stage never reads it.
pub(crate) const THIS_GLOBAL_INDEX: u32 = 5;

/// The module name of every host env import (`print`/`__convert`/`__method`).
pub(crate) const IMPORT_MODULE: &str = "env";

/// The field name of the host `print` import backing `console.log`.
pub(crate) const PRINT_IMPORT_NAME: &str = "print";

/// The field name of the host `__convert` import backing the
/// `String`/`Number`/`parseFloat` conversions.
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

/// The field name of the host `__parse_int` import backing `parseInt(s, radix)`.
/// It receives the string and radix jsvals (`undefined` for an omitted radix) and
/// returns the parsed number jsval.
pub(crate) const PARSE_INT_IMPORT_NAME: &str = "__parse_int";

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

/// The name under which the module exports the `__alloc(size: i32) -> base: i32`
/// bump allocator. It rounds `size` up to [`BLOCK_ALIGN`], zeroes the block's
/// [`HEADER_SIZE`] header bytes, and returns the 8-aligned base; on exhaustion it
/// stores [`HEAP_PTR_OOM`] and traps.
pub(crate) const ALLOC_EXPORT_NAME: &str = "__alloc";

/// The name under which the module exports the `__new_string(len: i32) -> base:
/// i32` constructor: a [`TYPE_STRING`] block with `len` set and a `len`-byte
/// payload for the caller to fill. The hosts allocate every string through it.
pub(crate) const NEW_STRING_EXPORT_NAME: &str = "__new_string";

/// The name under which the module exports the `__new_array(len: i32) -> base:
/// i32` constructor: a [`TYPE_ARRAY`] handle with `len` set and a `cap = len`
/// [`TYPE_ELEMS`] store (none when `len` is 0) whose every element is
/// `undefined`. The hosts allocate every array through it.
pub(crate) const NEW_ARRAY_EXPORT_NAME: &str = "__new_array";

/// The name under which the module exports the `__new_promise() -> base: i32`
/// constructor: a pending [`TYPE_PROMISE`] block (value `undefined`). The hosts
/// allocate every promise through it.
pub(crate) const NEW_PROMISE_EXPORT_NAME: &str = "__new_promise";

/// The name under which the module exports its linear memory; [`crate::run`]
/// reads it back to resolve heap pointers.
pub(crate) const MEMORY_EXPORT_NAME: &str = "memory";

/// The name under which the bump-allocator heap-pointer global is exported so
/// the run stage can detect the [`HEAP_PTR_OOM`] sentinel (hosts only read it;
/// they allocate through the constructor exports).
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

/// `__convert` op flag: parse a leading float (`parseFloat(x)`).
pub(crate) const CONVERT_OP_PARSE_FLOAT: i32 = 3;

/// `__method` op flag: `indexOf` (overloaded on array/string).
pub(crate) const METHOD_OP_INDEX_OF: i32 = 0;

/// `__method` op flag: `includes`.
pub(crate) const METHOD_OP_INCLUDES: i32 = 1;

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

/// `__method` op flag: `Number.prototype.toString(radix)` (a number receiver,
/// `arg0` an integral radix in `2..=36`).
pub(crate) const METHOD_OP_NUMBER_TO_STRING: i32 = 17;

#[cfg(test)]
#[path = "abi_drift_tests.rs"]
mod drift_tests;
