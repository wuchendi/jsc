//! Emit: lower the crate's numeric [`IrNode`] into a complete WebAssembly module.
//!
//! This is the final backend stage. It walks an [`IrNode`] tree and produces the
//! raw bytes of a valid WebAssembly module using the [`wasm_encoder`] crate.
//!
//! The emitted module is a self-contained WASI preview-1 command: it imports
//! exactly `fd_write`, `clock_time_get`, `random_get` and `proc_exit` from
//! `wasi_snapshot_preview1` and exports exactly `_start: () -> ()` and
//! `memory`. The internal `main: () -> i64` runs the top-level statements and
//! returns the completion value as a NaN-boxed jsval; `_start` wraps it with
//! the drain, the uncaught reports and the optional completion print. An `i64`
//! carrier — not `f64` — keeps the value's bits intact through every wasm op.
//!
//! Any user-defined functions are emitted as additional wasm functions
//! (indices `1..`, signature `(i64, ..) -> i64`), so a call expression lowers
//! to a plain wasm `call`. Recursion works because every function is declared
//! before any body is emitted.
//!
//! Semantics remain number-only in this stage: numbers are stored as their raw
//! `f64` bits reinterpreted into the `i64` carrier, and arithmetic unboxes each
//! operand back to `f64`, computes, and reboxes the `f64` result.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, Ieee64,
    ImportSection, Instruction, MemArg, MemorySection, MemoryType, Module, RefType, TableSection,
    TableType, TypeSection, ValType,
};

use builder::{FnBuilder, LocalClass, Sink, TempClass};
use gc::emit_alloc_const;
use roots::LeafSet;

use crate::abi;
use crate::ir::{
    ArrayHofOp, BinOp, BuiltinCtor, CalleeError, ConvertOp, DateOp, ErrorKind, FnKind,
    GlobalIntrinsic, GlobalRef, InstanceOfKind, IrCase, IrFunction, IrNode, IrProgram, IrStmt,
    IterRecord, JsStr, LogicalOp, MapOp, MathOp, MethodOp, ObjectBuiltinOp, OpSetKind,
    PrivateError, PropKey, ProtoKind, SpreadCallee, SpreadElem,
};

/// The name under which the module's linear memory is exported (see
/// [`abi::MEMORY_EXPORT_NAME`]).
const MEMORY_EXPORT_NAME: &str = abi::MEMORY_EXPORT_NAME;

/// The number of imported functions: the four WASI imports (`fd_write`,
/// `clock_time_get`, `random_get`, `proc_exit`) at indices `0..=3`. Every
/// defined function, funcref-table element and function export index is
/// shifted up by this count.
const IMPORT_FUNC_COUNT: u32 = 4;

/// The function index of the WASI `fd_write` import.
const FD_WRITE_IMPORT: u32 = 0;

/// The function index of the WASI `proc_exit` import.
const PROC_EXIT_IMPORT: u32 = 3;

/// The function index of the WASI `clock_time_get` import.
const CLOCK_TIME_GET_IMPORT: u32 = 1;

/// The function index of the WASI `random_get` import.
const RANDOM_GET_IMPORT: u32 = 2;

/// The wasm global index of the page frontier (`$__heap_ptr`): the end of the
/// highest GC page ever obtained (see [`gc`]).
const HEAP_PTR_GLOBAL: u32 = 0;

/// The wasm global index of the pending-exception flag (`$exc_pending`, i32,
/// init 0). Set to `1` by a `throw` and checked at every throwing call site to
/// unwind an in-flight exception out through the frames.
const EXC_PENDING_GLOBAL: u32 = 1;

/// The wasm global index of the in-flight exception value (`$exc_value`, i64,
/// init 0). A `throw` stashes the thrown jsval here before setting
/// [`EXC_PENDING_GLOBAL`].
const EXC_VALUE_GLOBAL: u32 = 2;

/// The wasm global index of the pending-completion kind (`$cf_kind`, i32, init
/// 0): `0` none, [`CF_RETURN`], [`CF_BREAK`], or [`CF_CONTINUE`]. Set when a
/// `return`/`break`/`continue` must route THROUGH one or more enclosing `finally`
/// clauses before reaching its target; each finalizer's post-body
/// dispatch reads it to resume (or, if the finally completed abruptly, replace)
/// the pending completion. Internal — never exported.
const CF_KIND_GLOBAL: u32 = 3;

/// The wasm global index of the pending `return` value (`$cf_value`, i64, init
/// 0): the jsval a routed `return` carries across the `finally` clauses it must
/// run before returning. A routed `break`/`continue` (which carries no
/// value) reuses this slot for its labeled-loop TARGET: `0` for an
/// unlabeled `break`/`continue` (nearest loop), or the loop id `+ 1` for a
/// labeled one, so a resume after the `finally` re-issues toward the right loop.
/// Only one completion is pending at a time (disambiguated by [`CF_KIND_GLOBAL`]),
/// so the two uses never collide. Internal.
const CF_VALUE_GLOBAL: u32 = 4;

/// The wasm global index of the `this`-binding carrier (`$__this`, i64, init
/// `undefined`). A method call sets it to the receiver just before the
/// `call_indirect`; every function/arrow prologue snapshots it into a local
/// `this` slot; a plain call resets it to `undefined`. See
/// [`abi::THIS_GLOBAL_INDEX`]; it follows the pending-completion globals.
const THIS_GLOBAL: u32 = abi::THIS_GLOBAL_INDEX;

/// The `JSON.parse` recursive-descent cursor globals (all i32, appended after
/// [`THIS_GLOBAL`]): `$__json_data` is the byte offset of the input string's
/// data (`base + STR_DATA_OFFSET`, 8-aligned) with its low bit set for a
/// `TWO_BYTE` input, `$__json_pos` is the current code-unit index into that
/// data, and `$__json_len` is the input's code-unit length. Set once at
/// `__json_parse` entry; the recursive helpers read `$__json_len`/advance
/// `$__json_pos`. A parse runs synchronously to completion, so no re-entrancy
/// disturbs them.
const JSON_DATA_GLOBAL: u32 = 6;
/// See [`JSON_DATA_GLOBAL`].
const JSON_POS_GLOBAL: u32 = 7;
/// See [`JSON_DATA_GLOBAL`].
const JSON_LEN_GLOBAL: u32 = 8;

/// The intern-table globals (all i32, appended after the `JSON.parse` cursor
/// globals): `$__intern_base` is the byte offset of the table's first u32 slot,
/// `$__intern_cap` its (power-of-two) slot count, and `$__intern_count` the
/// occupied slots. Initialized to the compile-time table in the data segment
/// (see [`props::intern_table`]); `__intern` moves the table to the heap when it
/// grows.
const INTERN_BASE_GLOBAL: u32 = 9;
/// See [`INTERN_BASE_GLOBAL`].
const INTERN_CAP_GLOBAL: u32 = 10;
/// See [`INTERN_BASE_GLOBAL`].
const INTERN_COUNT_GLOBAL: u32 = 11;

/// The wasm global index of the `new.target` carrier (`$__new_target`, i64,
/// init `undefined`): `new X()` sets it to `X` just before calling `X` with the
/// construct marker; a function referencing `new.target` snapshots it in its
/// prologue when the marker is present. Internal.
const NEW_TARGET_GLOBAL: u32 = 12;

/// The wasm global index of the spread-argument carrier (`$__spread_args`,
/// i64, init `undefined`): every spread call / construct sets it to its
/// argument array just before the call, so a callee gathering its arguments (a
/// rest parameter or `arguments`) reads the ones beyond the `K` positional
/// slots from it. Plain calls never touch it. Internal.
const SPREAD_ARGS_GLOBAL: u32 = 13;

/// The wasm global index of the intrinsics dirty bits (`$__intrinsics_dirty`,
/// i32, init `0`; DESIGN.md §6.4): bit `i` is set by `__set` / `__define` /
/// `__delete` on the watched intrinsic whose property table carries
/// [`abi::PROPS_WATCHED`] and bit index `i`. Bits are never cleared. Internal.
const DIRTY_GLOBAL: u32 = 14;

/// The output buffer: base, UTF-8 length and capacity (`$__out_buf`, all i32,
/// init 0). Empty between lines; see [`render`].
const OUT_BUF_GLOBAL: u32 = 15;
/// See [`OUT_BUF_GLOBAL`].
const OUT_LEN_GLOBAL: u32 = 16;
/// See [`OUT_BUF_GLOBAL`].
const OUT_CAP_GLOBAL: u32 = 17;

/// The renderer's recursion-path block: base and capacity (i32, init 0).
const PATH_BUF_GLOBAL: u32 = 18;
/// See [`PATH_BUF_GLOBAL`].
const PATH_CAP_GLOBAL: u32 = 19;

/// The renderer's ref table: base, capacity and entry count (i32, init 0).
const REFS_BUF_GLOBAL: u32 = 20;
/// See [`REFS_BUF_GLOBAL`].
const REFS_CAP_GLOBAL: u32 = 21;
/// See [`REFS_BUF_GLOBAL`].
const REFS_LEN_GLOBAL: u32 = 22;

/// The wasm global index of the shadow-stack pointer (`$__sp`, i32, init
/// `SS_SIZE - 8`); see [`builder`].
const SP_GLOBAL: u32 = 23;

/// The `Math.random` xorshift128+ state (`$__rng0`, `$__rng1`, i64, init 0);
/// all-zero until `__random` seeds it on the first call.
const RNG0_GLOBAL: u32 = 24;
/// See [`RNG0_GLOBAL`].
const RNG1_GLOBAL: u32 = 25;

/// The promise engine's job queue: raw `i32` refs to the oldest and newest
/// queued reaction/job record (`$__job_head`, `$__job_tail`, init 0 = empty);
/// see [`promise_engine`].
const JOB_HEAD_GLOBAL: u32 = 26;
/// See [`JOB_HEAD_GLOBAL`].
const JOB_TAIL_GLOBAL: u32 = 27;
/// The promise creation counter (`$__promise_seq`, i32, init 0).
const PROMISE_SEQ_GLOBAL: u32 = 28;
/// The rejected-promise list head (`$__rejected_head`, raw `i32` ref, init 0).
const REJECTED_HEAD_GLOBAL: u32 = 29;

/// The collector's allocation budget (`$__gc_debt`, i32 bytes, init
/// [`abi::GC_MIN_BUDGET`]): every allocation subtracts its slot or span size;
/// see [`gc`].
const GC_DEBT_GLOBAL: u32 = 30;
/// The bytes live after the last sweep (`$__gc_live`, i32, init 0).
const GC_LIVE_GLOBAL: u32 = 31;
/// The mark epoch (`$__gc_epoch`, i32, cycles `1..=255`, never 0, init 1).
const GC_EPOCH_GLOBAL: u32 = 32;
/// Set while a collection runs (`$__gc_busy`, i32, init 0).
const GC_BUSY_GLOBAL: u32 = 33;
/// The number of pages in the free-page pool (`$__gc_free_pages`, i32, init 0).
const GC_FREE_PAGES_GLOBAL: u32 = 34;
/// The head of the large-span list (`$__gc_large`, raw `i32` page address,
/// init 0 = none).
const GC_LARGE_GLOBAL: u32 = 35;
/// The mark stack's base (`$__gc_stack`, i32): [`abi::GC_MARK_STACK`] at the
/// start of every collection, or pages from the pool once it outgrew it.
const GC_STACK_GLOBAL: u32 = 36;
/// The mark stack's first free entry (`$__gc_stack_top`, i32).
const GC_STACK_TOP_GLOBAL: u32 = 37;
/// The mark stack's end (`$__gc_stack_end`, i32).
const GC_STACK_END_GLOBAL: u32 = 38;
/// Set when a mark-stack push found no room (`$__gc_overflow`, i32): the
/// marking pass is repeated with a rescan of every marked block.
const GC_OVERFLOW_GLOBAL: u32 = 39;
/// The out-of-memory reserve (`$__gc_reserve`, raw `i32` page address, init
/// 0 = not held): [`abi::GC_RESERVE_PAGES`] pages kept out of the pool,
/// released by the first out-of-memory and taken again by the next
/// collection (design/runtime-memory.md §5.8).
const GC_RESERVE_GLOBAL: u32 = 40;

/// The wasm global index of module cell `0` (`IrNode::ModuleCell`); module cell
/// `n` is global `MODULE_CELL_GLOBAL_BASE + n` (i64 cell pointers, appended after
/// the renderer, RNG, promise-engine and collector globals and set in `main`'s
/// prologue).
const MODULE_CELL_GLOBAL_BASE: u32 = 41;

/// An array hole (see [`abi::HOLE`]): the element-slot bit pattern of a
/// missing element. Every element read maps it to `undefined`.
const HOLE_BITS: i64 = abi::HOLE.cast_signed();

/// The TDZ hole a module cell holds until its binding is initialized
/// (`IrNode::IsHole`): the [`HOLE_BITS`] pattern, which no JS value produces.
const TDZ_HOLE_BITS: i64 = HOLE_BITS;

/// [`CF_KIND_GLOBAL`] value for a pending `return` (payload in
/// [`CF_VALUE_GLOBAL`]).
const CF_RETURN: i32 = 1;

/// [`CF_KIND_GLOBAL`] value for a pending `break`.
const CF_BREAK: i32 = 2;

/// [`CF_KIND_GLOBAL`] value for a pending `continue`.
const CF_CONTINUE: i32 = 3;

/// The base heap offset: the first byte of static string-literal data, after
/// the null guard, the function-table word, the fixed WASI area and the
/// [`libm`] block. Literal blobs are packed contiguously from here; the GC
/// heap starts at the first page boundary after the static region (see
/// [`emit`] and [`gc`]).
const HEAP_BASE: i32 = abi::STATIC_AREA_END.cast_signed() + libm::STATIC_SIZE;

/// The alignment of every heap block base (see [`abi::BLOCK_ALIGN`]).
const BLOCK_ALIGN: i32 = abi::BLOCK_ALIGN.cast_signed();

/// The NaN-boxed heap-pointer header (see [`abi::HEAP`]): the SINGLE tag for
/// every heap value. A heap jsval is `HEAP_BITS | (ptr as u32)`, carrying
/// the block base's byte offset in its low 32 bits; the block's concrete kind is
/// the TYPE BYTE at offset `0` (see [`HEADER_SIZE`] and the `TYPE_*` constants).
const HEAP_BITS: i64 = abi::HEAP.cast_signed();

/// The size in bytes of every heap block's leading header (see
/// [`abi::HEADER_SIZE`]). Byte `0` is the TYPE BYTE; bytes `1..4` are reserved
/// padding so the per-type payload begins 4-byte aligned at [`HEADER_SIZE`].
const HEADER_SIZE: i32 = abi::HEADER_SIZE.cast_signed();

/// The count of trailing (env, argc) parameters appended after the `K` positional
/// argument slots in the uniform function ABI (see
/// [`abi::ABI_TRAILING_PARAMS`]). The shared signature is
/// `(a0..a_{K-1}, env, argc) -> i64`.
const ABI_TRAILING_PARAMS: u32 = abi::ABI_TRAILING_PARAMS;

/// Type byte for a heap string block: `[type=0][pad×3][len:u32][utf8]` (payload
/// after the [`HEADER_SIZE`] header). Maps to `typeof` "string".
const TYPE_STRING: i32 = abi::TYPE_STRING as i32;
/// String block field offsets: the u32 `len` (a load/store `MemArg` offset) and
/// the UTF-8 data (an `i32` addend). See [`abi::STR_LEN_OFFSET`].
const STR_LEN_OFFSET: u64 = abi::STR_LEN_OFFSET as u64;
const STR_DATA_OFFSET: i32 = abi::STR_DATA_OFFSET.cast_signed();

/// Type byte, field offsets, and size of an array handle (`length` and the
/// base of its element store). Maps to `typeof` "object". See
/// [`abi::TYPE_ARRAY`] for the full layout.
const TYPE_ARRAY: i32 = abi::TYPE_ARRAY as i32;
const ARRAY_LEN_OFFSET: u64 = abi::ARRAY_LEN_OFFSET as u64;
const ARRAY_ELEMS_OFFSET: u64 = abi::ARRAY_ELEMS_OFFSET as u64;
const ARRAY_BLOCK_SIZE: i32 = abi::ARRAY_BLOCK_SIZE.cast_signed();
/// Array header `flags` bit: the array is sparse (see [`abi::ARRAY_FLAG_SPARSE`]).
const ARRAY_SPARSE: i32 = abi::ARRAY_FLAG_SPARSE as i32;
/// Array header `flags` bit: `length` is read-only (see
/// [`abi::ARRAY_FLAG_LENGTH_READONLY`]).
const ARRAY_LENGTH_READONLY: i32 = abi::ARRAY_FLAG_LENGTH_READONLY as i32;
/// Array header `flags` bit: dictionary mode (see [`abi::ARRAY_FLAG_DICTIONARY`]).
const ARRAY_DICTIONARY: i32 = abi::ARRAY_FLAG_DICTIONARY as i32;

/// Type byte and field offsets of an array element store (internal): `cap`,
/// then element `i` at `ELEMS_DATA_OFFSET + 8 * i`. See [`abi::TYPE_ELEMS`].
const TYPE_ELEMS: i32 = abi::TYPE_ELEMS as i32;
const ELEMS_CAP_OFFSET: u64 = abi::ELEMS_CAP_OFFSET as u64;
const ELEMS_DATA_OFFSET: u64 = abi::ELEMS_DATA_OFFSET as u64;

/// Type byte for an ordinary object block: the object-family prefix alone, its
/// own properties in the `TYPE_PROPS` table at `props`. Maps to `typeof`
/// "object". See [`abi::TYPE_OBJECT`].
const TYPE_OBJECT: i32 = abi::TYPE_OBJECT as i32;

/// Byte offset of the object-family prefix's `proto` jsval slot (see
/// [`abi::OBJ_PROTO_OFFSET`]); arrays, Map/Set, promises and dates are created
/// with their realm prototype, generators with `%GeneratorPrototype%`.
const OBJ_PROTO_OFFSET: u64 = abi::OBJ_PROTO_OFFSET as u64;

/// Type byte, field offsets, and size of a first-class function value block
/// (the funcref-table index and the closure environment). Maps to `typeof`
/// "function". See [`abi::TYPE_FUNCTION`] for the full layout.
const TYPE_FUNCTION: i32 = abi::TYPE_FUNCTION as i32;
const FUNC_INDEX_OFFSET: u64 = abi::FUNC_INDEX_OFFSET as u64;
const FUNC_ENV_OFFSET: u64 = abi::FUNC_ENV_OFFSET as u64;
const FUNC_BLOCK_SIZE: i32 = abi::FUNC_BLOCK_SIZE.cast_signed();

/// Type byte, field offsets, and size of a `Generator` handle block. See
/// [`abi::TYPE_GENERATOR`] for the full layout.
const TYPE_GENERATOR: i32 = abi::TYPE_GENERATOR as i32;
const GEN_SM_IDX_OFFSET: u64 = abi::GEN_SM_IDX_OFFSET as u64;
const GEN_STATE_OFFSET: u64 = abi::GEN_STATE_OFFSET as u64;
const GEN_DONE_OFFSET: u64 = abi::GEN_DONE_OFFSET as u64;
const GEN_ENV_OFFSET: u64 = abi::GEN_ENV_OFFSET as u64;
const GEN_BLOCK_SIZE: i32 = abi::GEN_BLOCK_SIZE.cast_signed();

/// Type byte for a closure **environment** block (internal): a block laid
/// out as `[type=4][pad×3][count:u32][cell_ptr:i64 × count]` holding the boxed
/// cells a closure captured. Never surfaces as a user value.
const TYPE_ENV: i32 = abi::TYPE_ENV as i32;

/// Type byte for a **boxed cell** (internal): a one-slot block
/// `[type=5][pad×3][value:i64]` holding a captured binding's value. Reads/writes
/// of a captured variable go through the cell so closures share mutations by
/// reference. Never surfaces as a user value.
const TYPE_CELL: i32 = abi::TYPE_CELL as i32;

/// The byte size of a boxed-cell heap block: the [`HEADER_SIZE`] header plus one
/// i64 value slot.
const CELL_BLOCK_SIZE: i32 = HEADER_SIZE + 8;

/// Type byte for a `Map` handle block — the logical entry count and a heap jsval
/// pointing at the growable backing. Maps to `typeof` "object". See
/// [`abi::TYPE_MAP`].
const TYPE_MAP: i32 = abi::TYPE_MAP as i32;

/// Type byte for a `Set` handle block: the same layout as [`TYPE_MAP`], with
/// the backing's value slots always `undefined`. Maps to `typeof` "object". See
/// [`abi::TYPE_SET`].
const TYPE_SET: i32 = abi::TYPE_SET as i32;

/// Field offsets and size of a Map/Set handle block (see [`abi::TYPE_MAP`]).
const MAP_SIZE_OFFSET: u64 = abi::MAP_SIZE_OFFSET as u64;
const MAP_ENTRIES_OFFSET: u64 = abi::MAP_ENTRIES_OFFSET as u64;
const MAP_BLOCK_SIZE: i32 = abi::MAP_BLOCK_SIZE.cast_signed();

/// The settled-value offset of a `Promise` block (see [`abi::TYPE_PROMISE`]).
const PROMISE_VALUE_OFFSET: u64 = abi::PROMISE_VALUE_OFFSET as u64;

/// Type byte, payload offset, and size of a `Date` block. Maps to `typeof`
/// "object". See [`abi::TYPE_DATE`].
const TYPE_DATE: i32 = abi::TYPE_DATE as i32;
const DATE_MS_OFFSET: u64 = abi::DATE_MS_OFFSET as u64;
const DATE_BLOCK_SIZE: i32 = abi::DATE_BLOCK_SIZE.cast_signed();

/// The TYPE BYTE of a Symbol primitive block. Maps to `typeof` "symbol". See
/// [`abi::TYPE_SYMBOL`] for the full layout.
const TYPE_SYMBOL: i32 = abi::TYPE_SYMBOL as i32;
const SYMBOL_DESCRIPTION_OFFSET: u64 = abi::SYMBOL_DESCRIPTION_OFFSET as u64;

/// `%Symbol.prototype%` (DESIGN.md §7.9): where a property read
/// on a symbol primitive continues and the prototype of its `ToObject`
/// wrapper.
const SYMBOL_PROTOTYPE: Intrinsic = Intrinsic::Prototype(BuiltinCtor::Symbol);

/// The TYPE BYTE of an instance-accessor block. See [`abi::TYPE_ACCESSOR`].
const TYPE_ACCESSOR: i32 = abi::TYPE_ACCESSOR as i32;
const ACCESSOR_GET_OFFSET: u64 = abi::ACCESSOR_GET_OFFSET as u64;
const ACCESSOR_SET_OFFSET: u64 = abi::ACCESSOR_SET_OFFSET as u64;

/// The byte size of a `TYPE_ACCESSOR` heap block. See
/// [`abi::ACCESSOR_BLOCK_SIZE`].
const ACCESSOR_BLOCK_SIZE: i32 = abi::ACCESSOR_BLOCK_SIZE.cast_signed();

/// The byte length of a `toISOString()` result with a four-digit year
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`); an expanded `±YYYYYY` year adds three.
const ISO_STRING_LEN: i32 = 24;

/// The `toString()` of an invalid (`NaN`) Date (interned as a data-segment
/// literal, like the `typeof` words).
const INVALID_DATE: &str = "Invalid Date";

/// The NaN-boxed `undefined` jsval as an `i64` carrier (see [`abi::UNDEFINED`]).
/// Stands in for the value of a bare `return;` and of a program with no
/// expression statement.
const UNDEFINED_BITS: i64 = abi::UNDEFINED.cast_signed();

/// The NaN-boxed `false` jsval (see [`abi::FALSE`]).
const FALSE_BITS: i64 = abi::FALSE.cast_signed();

/// The NaN-boxed `true` jsval (see [`abi::TRUE`]).
const TRUE_BITS: i64 = abi::TRUE.cast_signed();

/// The `.message` and `.stack` of the `TypeError` a guarded call site throws
/// when its callee is not callable: a [`CalleeError`]'s texts, or
/// [`EXPRESSION_NOT_A_FUNCTION`]. Both strings are interned by
/// [`collect_node`] at the site.
type CalleeErrorText<'a> = [&'a str; 2];

/// The [`CalleeErrorText`] of an IR [`CalleeError`].
fn callee_error_text(error: &CalleeError) -> CalleeErrorText<'_> {
    [&error.message, &error.stack]
}

/// The not-a-function `TypeError` text of a guarded call whose callee has no
/// source name (an array callback, a promise executor, a `super` call, a
/// generator resume).
/// The `TypeError` of resuming a generator that is already running.
const GENERATOR_RUNNING: CalleeErrorText<'static> = [
    "Generator is already running",
    "TypeError: Generator is already running",
];

const EXPRESSION_NOT_A_FUNCTION: CalleeErrorText<'static> = [
    "expression is not a function",
    "TypeError: expression is not a function",
];

/// The `TypeError` text of an `instanceof` whose right-hand side is not
/// callable.
const INSTANCEOF_NOT_CALLABLE: CalleeErrorText<'static> = [
    "Right-hand side of 'instanceof' is not callable",
    "TypeError: Right-hand side of 'instanceof' is not callable",
];

/// The `TypeError` text of an `instanceof` whose right-hand side's
/// `prototype` is not an object.
const INSTANCEOF_NON_OBJECT_PROTOTYPE: CalleeErrorText<'static> = [
    "Function has non-object prototype in instanceof check",
    "TypeError: Function has non-object prototype in instanceof check",
];

/// The `TypeError` a guarded call site throws (shape of `new TypeError(m)`,
/// see [`IrNode::error_object`]). Emitted on the throw path via [`emit_node`];
/// its strings are seeded by [`collect_node`].
fn callee_type_error([message, stack]: CalleeErrorText<'_>) -> IrNode {
    IrNode::error_object(
        ErrorKind::TypeError,
        Some(IrNode::Str(message.into())),
        IrNode::Str(stack.into()),
    )
}

/// The own key every Error instance carries besides `message` (the realm
/// interns the rest of the error strings).
const STACK_KEY: &str = "stack";

/// The message fragments `__nullish_throw` splices into the V8-style text of the
/// `TypeError` raised by a member read/write on a `null`/`undefined` receiver:
/// `Cannot read properties of <null|undefined> (reading '<key>')` (or the
/// `set`/`setting` form for a write), with `.stack` prefixed `TypeError: `.
const NULLISH_READ_PREFIX: &str = "Cannot read properties of ";
/// See [`NULLISH_READ_PREFIX`].
const NULLISH_SET_PREFIX: &str = "Cannot set properties of ";
/// See [`NULLISH_READ_PREFIX`].
const NULLISH_READ_MID: &str = " (reading '";
/// See [`NULLISH_READ_PREFIX`].
const NULLISH_SET_MID: &str = " (setting '";
/// See [`NULLISH_READ_PREFIX`].
const NULLISH_SUFFIX: &str = "')";
/// See [`NULLISH_READ_PREFIX`].
const TYPE_ERROR_STACK_PREFIX: &str = "TypeError: ";

/// The computed key `recv["length"]` that a string/array index read resolves to
/// the length header. Seeded with [`NULLISH_ERROR_STRS`], since every guarded
/// index read site seeds those.
const INDEX_LENGTH_KEY: &str = "length";

/// The keys `__fn_materialize` defines on a function object, plus the
/// anonymous function's empty `name`. Seeded whenever the program has
/// functions.
const FN_KEYS: [&str; 4] = ["name", "prototype", "constructor", ""];

/// The static strings `__nullish_throw` needs — the receiver words and the
/// message fragments. Seeded by [`collect_node`] at every
/// guarded member read/write/method-call site so their `[len][utf8]` blobs
/// exist when the guard throws.
const NULLISH_ERROR_STRS: [&str; 9] = [
    INDEX_LENGTH_KEY,
    "null",
    "undefined",
    NULLISH_READ_PREFIX,
    NULLISH_SET_PREFIX,
    NULLISH_READ_MID,
    NULLISH_SET_MID,
    NULLISH_SUFFIX,
    TYPE_ERROR_STACK_PREFIX,
];

/// The `(prefix, suffix)` fragments `__prop_throw` wraps around `String(key)`
/// for each keyed `TypeError` mode, indexed by mode.
const PROP_KEYED_MSGS: [(i32, &str, &str); 25] = [
    (
        PROP_THROW_READ_ONLY,
        "Cannot assign to read only property '",
        "' of object",
    ),
    (
        PROP_THROW_NOT_EXTENSIBLE,
        "Cannot add property ",
        ", object is not extensible",
    ),
    (
        PROP_THROW_GETTER_ONLY,
        "Cannot set property ",
        " of object which has only a getter",
    ),
    (PROP_THROW_DELETE, "Cannot delete property '", "' of object"),
    (
        PROP_THROW_EXTENDS,
        "Class extends value ",
        " is not a constructor or null",
    ),
    (
        PROP_THROW_EXTENDS_PROTO,
        "Class extends value does not have valid prototype property ",
        "",
    ),
    (
        PROP_THROW_TO_OBJECT,
        "Cannot convert undefined or null to object",
        "",
    ),
    (
        PROP_THROW_IN,
        "Cannot use 'in' operator to search for a key in ",
        "",
    ),
    (PROP_THROW_REDEFINE, "Cannot redefine property: ", ""),
    (
        PROP_THROW_DEFINE_NOT_EXTENSIBLE,
        "Cannot define property ",
        ", object is not extensible",
    ),
    (
        PROP_THROW_DESC_NOT_OBJECT,
        "Property description must be an object: ",
        "",
    ),
    (PROP_THROW_GETTER, "Getter must be a function: ", ""),
    (PROP_THROW_SETTER, "Setter must be a function: ", ""),
    (
        PROP_THROW_DESC_MIXED,
        "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
        "",
    ),
    (
        PROP_THROW_PROTO_TYPE,
        "Object prototype may only be an Object or null: ",
        "",
    ),
    (
        PROP_THROW_PROTO_NOT_EXTENSIBLE,
        "#<Object> is not extensible",
        "",
    ),
    (PROP_THROW_PROTO_CYCLE, "Cyclic __proto__ value", ""),
    (
        PROP_THROW_PROTO_IMMUTABLE,
        "Immutable prototype object 'Object.prototype' cannot have their prototype set",
        "",
    ),
    (
        PROP_THROW_PRIVATE_INIT,
        "Cannot initialize ",
        " twice on the same object",
    ),
    (
        PROP_THROW_PRIVATE_READ,
        "Cannot read private member ",
        " from an object whose class did not declare it",
    ),
    (
        PROP_THROW_PRIVATE_WRITE,
        "Cannot write private member ",
        " to an object whose class did not declare it",
    ),
    (
        PROP_THROW_PRIVATE_NO_GETTER,
        "'",
        "' was defined without a getter",
    ),
    (
        PROP_THROW_PRIVATE_NO_SETTER,
        "'",
        "' was defined without a setter",
    ),
    (
        PROP_THROW_PRIVATE_METHOD_WRITE,
        "Private method is not writable",
        "",
    ),
    (
        PROP_THROW_PRIVATE_IN,
        "Cannot use 'in' operator to search for '",
        "",
    ),
];
/// The `.message` and `.stack` of the `RangeError` `__array_set_length` raises
/// for a `length` write whose value is not a valid array length.
const ARRAY_LENGTH_MSG: &str = "Invalid array length";
/// See [`ARRAY_LENGTH_MSG`].
const ARRAY_LENGTH_STACK: &str = "RangeError: Invalid array length";

/// The message fragments `__prop_throw` splices into the strict-mode
/// `TypeError` raised by a member store on a primitive receiver:
/// `Cannot create property '<key>' on <number|string|boolean> '<String(recv)>'`.
const PRIM_CREATE_PREFIX: &str = "Cannot create property '";
/// See [`PRIM_CREATE_PREFIX`].
const PRIM_CREATE_MID: &str = "' on ";
/// See [`PRIM_CREATE_PREFIX`].
const PRIM_CREATE_QUOTE: &str = " '";
/// See [`PRIM_CREATE_PREFIX`].
const PRIM_CREATE_SUFFIX: &str = "'";

/// `__prop_throw`'s `mode` argument: any store on a primitive receiver.
const PROP_THROW_PRIMITIVE: i32 = 2;
/// `__prop_throw`'s `mode` argument: an assignment to a non-writable property.
const PROP_THROW_READ_ONLY: i32 = 3;
/// `__prop_throw`'s `mode` argument: adding a property to a non-extensible
/// object.
const PROP_THROW_NOT_EXTENSIBLE: i32 = 4;
/// `__prop_throw`'s `mode` argument: an assignment to an accessor without a
/// setter.
const PROP_THROW_GETTER_ONLY: i32 = 5;
/// `__prop_throw`'s `mode` argument: deleting a non-configurable property.
const PROP_THROW_DELETE: i32 = 6;
/// `__prop_throw`'s `mode` argument: a class `extends` value (the `key`) that
/// is neither a constructor nor `null`.
const PROP_THROW_EXTENDS: i32 = 7;
/// `__prop_throw`'s `mode` argument: a class parent whose `prototype` (the
/// `key`) is neither an object nor `null`.
const PROP_THROW_EXTENDS_PROTO: i32 = 8;
/// `__prop_throw`'s `mode` argument: an `in` whose right-hand side (the
/// `key`) is not an object.
const PROP_THROW_IN: i32 = 9;
/// `__prop_throw`'s `mode` argument: `ToObject` of `null`/`undefined` (the
/// `key` is the empty string).
const PROP_THROW_TO_OBJECT: i32 = 10;
/// `__prop_throw`'s `mode` argument: a rejected redefinition of a
/// non-configurable property (`ValidateAndApplyPropertyDescriptor`).
const PROP_THROW_REDEFINE: i32 = 11;
/// `__prop_throw`'s `mode` argument: defining a new property on a
/// non-extensible object.
const PROP_THROW_DEFINE_NOT_EXTENSIBLE: i32 = 12;
/// `__prop_throw`'s `mode` argument: `ToPropertyDescriptor` of a non-object
/// (the `key`).
const PROP_THROW_DESC_NOT_OBJECT: i32 = 13;
/// `__prop_throw`'s `mode` argument: a descriptor `get` (the `key`) that is
/// neither callable nor `undefined`.
const PROP_THROW_GETTER: i32 = 14;
/// `__prop_throw`'s `mode` argument: a descriptor `set` (the `key`) that is
/// neither callable nor `undefined`.
const PROP_THROW_SETTER: i32 = 15;
/// `__prop_throw`'s `mode` argument: a descriptor with both accessor and
/// data fields (the `key` is the empty string).
const PROP_THROW_DESC_MIXED: i32 = 16;
/// `__prop_throw`'s `mode` argument: a prototype (the `key`) that is neither
/// an object nor `null`.
const PROP_THROW_PROTO_TYPE: i32 = 17;
/// `__prop_throw`'s `mode` argument: `[[SetPrototypeOf]]` of a non-extensible
/// object (the `key` is the empty string).
const PROP_THROW_PROTO_NOT_EXTENSIBLE: i32 = 18;
/// `__prop_throw`'s `mode` argument: `[[SetPrototypeOf]]` that would close a
/// prototype cycle (the `key` is the empty string).
const PROP_THROW_PROTO_CYCLE: i32 = 19;
/// `__prop_throw`'s `mode` argument: `[[SetPrototypeOf]]` of the immutable
/// `%Object.prototype%` (the `key` is the empty string).
const PROP_THROW_PROTO_IMMUTABLE: i32 = 20;
/// `__prop_throw`'s `mode` argument: a second `PrivateFieldAdd` of the private
/// name (the `key` is its `"#x"` source name, DESIGN.md §7.6).
const PROP_THROW_PRIVATE_INIT: i32 = 21;
/// `__prop_throw`'s `mode` argument: a private read of an object lacking the
/// private name (the `key` is its `"#x"` source name).
const PROP_THROW_PRIVATE_READ: i32 = 22;
/// `__prop_throw`'s `mode` argument: a private write to an object lacking the
/// private name (the `key` is its `"#x"` source name).
const PROP_THROW_PRIVATE_WRITE: i32 = 23;
/// `__prop_throw`'s `mode` argument: a private read of an accessor without a
/// getter (the `key` is its `"#x"` source name).
const PROP_THROW_PRIVATE_NO_GETTER: i32 = 24;
/// `__prop_throw`'s `mode` argument: a private write of an accessor without a
/// setter (the `key` is its `"#x"` source name).
const PROP_THROW_PRIVATE_NO_SETTER: i32 = 25;
/// `__prop_throw`'s `mode` argument: a write of a private method (the `key`
/// is the empty string).
const PROP_THROW_PRIVATE_METHOD_WRITE: i32 = 26;
/// `__prop_throw`'s `mode` argument: `#x in o` on a non-object `o` (the `key`
/// is `"#x' in " + String(o)`, built by `__private_in`).
const PROP_THROW_PRIVATE_IN: i32 = 27;
/// The fragment `__private_in` splices between `#x` and `String(o)`.
const PRIVATE_IN_MID: &str = "' in ";

/// The static strings `__prop_throw` and `__array_set_length` need — the
/// message fragments (plus every [`PROP_KEYED_MSGS`] fragment). Seeded by
/// [`collect_node`] at every member store and `delete` site
/// so their `[len][utf8]` blobs exist when they throw.
const PROP_ERROR_STRS: [&str; 12] = [
    PRIVATE_IN_MID,
    ARRAY_LENGTH_MSG,
    ARRAY_LENGTH_STACK,
    TYPE_ERROR_STACK_PREFIX,
    PRIM_CREATE_PREFIX,
    PRIM_CREATE_MID,
    PRIM_CREATE_QUOTE,
    PRIM_CREATE_SUFFIX,
    "number",
    "symbol",
    "string",
    "boolean",
];

/// The `JSON.parse` `SyntaxError` `.message` and `.stack` text. A single
/// generic message covers every malformed / unsupported-input case (a `\u`
/// escape is out of v1 scope; see `json_parse`).
const JSON_ERROR_MSG: &str = "JSON.parse: invalid or unsupported JSON input";
/// See [`JSON_ERROR_MSG`].
const JSON_ERROR_STACK: &str = "SyntaxError: JSON.parse: invalid or unsupported JSON input";

/// The static strings the `JSON.parse` syntax-error object embeds, interned
/// by [`collect_node`] wherever a `JsonParse` node exists so their
/// `[len][utf8]` blobs are in the data segment when `__json_throw` builds the
/// object.
const JSON_PARSE_ERROR_STRS: [&str; 2] = [JSON_ERROR_MSG, JSON_ERROR_STACK];

/// Build the `SyntaxError` `__json_throw` raises on malformed JSON, the same
/// shape as [`callee_type_error`]. Emitted via [`emit_node`]; its strings are
/// seeded by [`JSON_PARSE_ERROR_STRS`].
fn json_syntax_error() -> IrNode {
    IrNode::error_object(
        ErrorKind::SyntaxError,
        Some(IrNode::Str(JSON_ERROR_MSG.into())),
        IrNode::Str(JSON_ERROR_STACK.into()),
    )
}

/// The NaN-boxed `null` jsval (see [`abi::NULL`]).
const NULL_BITS: i64 = abi::NULL.cast_signed();

/// The literal token strings the in-wasm `JSON.stringify` serializer concatenates:
/// the three literal keywords plus the structural punctuation. They are
/// interned like any static string (seeded into the string table when a program
/// contains a `JsonStringify` node, see `collect_node`) so `__json_rep` can
/// splice their `[len][utf8]` blocks with `__concat`; [`JsonTokens`] resolves
/// the keywords `__json_str` returns to their compile-time byte offsets.
const JSON_TOKEN_STRS: [&str; 9] = ["null", "true", "false", "[", "]", "{", "}", ",", ":"];

/// The compile-time byte offsets of the keyword [`JSON_TOKEN_STRS`] blocks in
/// linear memory, baked into the `__json_str` serializer. Resolved from the
/// string-offset map; a missing token (only possible when the program has no
/// `JsonStringify` node, so `__json_str` is dead code) falls back to
/// [`HEAP_BASE`] to keep the emitted helper valid.
#[derive(Clone, Copy)]
struct JsonTokens {
    null: i32,
    true_ptr: i32,
    false_ptr: i32,
}

impl JsonTokens {
    fn resolve(offsets: &StrOffsets) -> Self {
        let at = |s: &str| offsets.get(s).copied().unwrap_or(HEAP_BASE);
        Self {
            null: at("null"),
            true_ptr: at("true"),
            false_ptr: at("false"),
        }
    }
}

/// The quiet-NaN mask (see [`abi::QNAN`]): a value is a boxed non-number (or the
/// canonical number NaN) iff `bits & QNAN == QNAN`. Used by `typeof` to tell a
/// number from a boxed immediate.
const QNAN_BITS: i64 = abi::QNAN.cast_signed();

/// The wasm function indices of the runtime string helpers, appended as the LAST
/// functions of the module (after `main` and every user function) so
/// user-function indices `1..=n` stay stable. Threaded together through the
/// emitters so a lowered expression can call whichever helper it needs.
/// Per-function exception-unwinding state threaded through emission.
///
/// A `throw` (and every guarded call-site propagation check) transfers control
/// to the nearest enclosing `try`'s catch-dispatch `block` by a wasm `br`; with
/// no enclosing `try` it falls back to the function-boundary `return`.
/// Because a `br`'s immediate is *relative* to the current control-nesting
/// depth, we track that depth explicitly and record, for each active handler,
/// the depth captured just inside its dispatch block — the `br` immediate at a
/// throw site is then simply `depth - handler_depth`.
///
/// This state is reset at the start of each function body (handlers never span
/// a call frame — a callee's throw unwinds to its own function boundary, and the
/// caller's post-call check re-raises it), so one instance is reused across all
/// functions in a module.
#[derive(Default)]
struct Unwind {
    /// The current wasm control-nesting depth within the function body: `0` at
    /// the top level, `+1` for each enclosing `block`/`loop`/`if` frame that
    /// wraps a possible throw or throwing-call site. Only the constructs that can
    /// wrap such a site update this (statement `if`/`while`/`switch`/`try` and
    /// the conditionally/repeatedly evaluated `ternary`/`&&`/`||`/HOF-loop
    /// bodies); value-computation blocks (e.g. `emit_cond`) open and close with
    /// no throw site inside, so they need not be counted.
    depth: u32,
    /// Active `try` catch-dispatch landing pads, innermost last. Each entry is
    /// the [`depth`](Self::depth) recorded just inside that `try`'s dispatch
    /// `block`, so a throw at the current `depth` reaches it with the relative
    /// `br` immediate `depth - entry`.
    handlers: Vec<u32>,
    /// Active `try` FINALLY landing pads, innermost last — a subset of the
    /// exceptional edges that carries a `finally`. Each entry is the
    /// [`depth`](Self::depth) captured just inside that `try`'s outer
    /// `$finally_target` block, so a `return`/`break`/`continue` that must run the
    /// finally reaches it with the relative `br` immediate `depth - entry`. Unlike
    /// [`handlers`](Self::handlers) this excludes plain catch-dispatch blocks (a
    /// `return` does not run a `catch`), so it names exactly the finalizers a
    /// non-throw abrupt completion must traverse.
    finalizers: Vec<u32>,
    /// Active labeled loops enclosing the point being emitted, innermost last.
    /// Each records the loop's `break`/`continue` landing blocks as
    /// absolute [`depth`](Self::depth)s, so a labeled `break`/`continue` reaches
    /// its target with the relative `br` immediate `depth - entry` — exactly the
    /// [`handlers`](Self::handlers)/[`finalizers`](Self::finalizers) scheme.
    labels: Vec<LabelTarget>,
}

/// A labeled loop's (or labeled statement's) abrupt-exit landing blocks, keyed
/// by the label id an [`IrStmt::Break`]/[`IrStmt::Continue`] carries. Depths are absolute (see
/// [`Unwind::depth`]); the `br` immediate at a use site is `depth - entry`.
struct LabelTarget {
    /// The label id matching [`crate::ir::IrStmt::While`]'s (or
    /// [`crate::ir::IrStmt::Labeled`]'s) `label`.
    id: u32,
    /// Depth just inside the loop's `break` block `B` (a `break LABEL` target).
    break_entry: u32,
    /// Depth just inside the loop's `continue` block `C` (a `continue LABEL`
    /// target).
    continue_entry: u32,
}

impl Unwind {
    /// Reset to the top of a fresh function body: no active handlers, depth `0`.
    fn reset(&mut self) {
        self.depth = 0;
        self.handlers.clear();
        self.finalizers.clear();
        self.labels.clear();
    }
}

#[derive(Clone, Copy)]
struct Helpers<'a> {
    /// Per-function exception-unwinding state: the active `try` handler
    /// landing pads and the current control-nesting depth. A shared `&RefCell`
    /// so the `Copy` `Helpers` threaded through emission can still mutate it in
    /// place (push/pop a handler, bump/restore the depth) without re-plumbing a
    /// `&mut` through every helper.
    unwind: &'a RefCell<Unwind>,
    /// The first of the [`dtoa::HELPER_COUNT`] consecutive `Number` to string
    /// helpers (see [`dtoa::emit_helpers`]).
    dtoa: u32,
    /// `__now() -> f64`, the WASI clock in ms since the Unix epoch (backing
    /// `Date`); `__random` follows it (see [`Self::random`]).
    now: u32,
    /// `__num_unit`, the first of the [`strtod`] number parsers; the others
    /// follow it (see the accessors).
    strtod: u32,
    /// `__gen_to_array(gen: i64) -> i64`: drive a generator to
    /// exhaustion via `call_indirect`, collecting its yielded values into a fresh
    /// array — the drain backing `[...gen]` / `f(...gen)` spread. Emitted just
    /// before `invoke1`.
    gen_to_array: u32,
    /// `__nullish_throw(recv: i64, key: i64, set: i32)`: build the V8-style
    /// `TypeError` for a member read (`set == 0`) or write (`set == 1`) on a
    /// `null`/`undefined` receiver and leave it in flight (`$exc_pending` set).
    /// The out-of-line half of the nullish-receiver guard (see
    /// [`object::emit_nullish_guard`]).
    nullish_throw: u32,
    /// `__prop_throw(recv: i64, key: i64, mode: i32)`: build the `TypeError` for
    /// a member store or delete the receiver cannot take — any store on a
    /// primitive `recv` ([`PROP_THROW_PRIMITIVE`]), or a keyed mode of
    /// [`PROP_KEYED_MSGS`] — and leave it in flight (`$exc_pending` set).
    prop_throw: u32,
    /// `__box_f64(x: f64) -> i64`: rebox an `f64` as a number jsval, mapping
    /// every NaN to the canonical [`abi::QNAN`] bits (see [`value::emit_box_f64`]).
    box_f64: u32,
    /// `__to_number(v: i64) -> i64`: the JS `ToNumber` of any jsval (an object
    /// through `__to_primitive`; see [`value::emit_to_number_helper`]).
    /// Call sites skip it for number operands (see [`value::emit_coerce_number`]).
    to_number: u32,
    /// `__invoke1(fn: i64, arg: i64) -> i64`: the callback trampoline
    /// `__run_job` calls to run a reaction's callback.
    invoke1: u32,
    /// `__async_step(gen: i64, sent: i64) -> i64`: the generator
    /// single-step trampoline `__async_drive` calls to advance an
    /// `async function` body's state machine by one suspend. Appended right after
    /// `invoke1` (index `invoke1 + 1`).
    async_step: u32,
    /// The funcref-TABLE slots of the two `new Promise` settler functions:
    /// `resolve` (fulfills the bound promise) and `reject`. A `new Promise`
    /// executor receives function values pointing at these slots with the target
    /// promise in their env slot. They follow the `n` user-function slots, so
    /// `settle_resolve_slot` is also the user function count.
    settle_resolve_slot: u32,
    settle_reject_slot: u32,
    /// `new_string(len: i32, two_byte: i32) -> i32`: the string block
    /// constructor, followed by `__new_array` (`new_string + 1`), `__has_own`
    /// (`new_string + 2`), and the builtin bodies ([`Self::builtin`]), appended
    /// after the settlers so every other index stays stable.
    new_string: u32,
    /// `__alloc(size) -> i32`: allocate a zeroed block of `size` bytes (a small
    /// slot or a large span, see [`gc`]), returning its base. Constant-size
    /// sites call the per-class entry instead (`gc::emit_alloc_const`).
    alloc: u32,
    /// `__concat(a, b) -> i32`: allocate and build the concatenation of two heap
    /// strings.
    concat: u32,
    /// `__str_eq(a, b) -> i32`: `1` iff two heap strings have equal byte content.
    str_eq: u32,
    /// `__str_cmp(a, b) -> i32`: lexicographic byte compare, `-1` / `0` / `1`.
    str_cmp: u32,
    /// `__str_index(ptr, i) -> i32`: allocate a fresh one-char heap string
    /// holding the byte at `ptr + 4 + i`.
    str_index: u32,
    /// `__str_hash(s) -> i32`: the FNV-1a hash of a string's bytes (the
    /// intern-table hash).
    str_hash: u32,
    /// `__intern_slot(s) -> addr`: the intern-table slot for `s`'s content.
    intern_slot: u32,
    /// `__intern_find(s) -> key`: the interned block for `s`'s content, `0` on a
    /// miss.
    intern_find: u32,
    /// `__intern(s) -> key`: the interned block for `s`'s content, inserting `s`.
    intern: u32,
    /// `__index_value(s) -> i32`: a canonical array-index string's value, else `-1`.
    index_value: u32,
    /// `__index_string(n) -> s`: a fresh string of the unsigned `n`'s digits,
    /// followed by `__str_exotic_index` ([`Self::str_exotic_index`]).
    index_string: u32,
    /// `__to_key(v, insert) -> key`: `ToPropertyKey` of a jsval as an interned key
    /// (`insert` interns a miss; otherwise a miss is `0`).
    to_key: u32,
    /// `__prop_find(o, key) -> entry`: an own property-table entry address, or `0`.
    prop_find: u32,
    /// `__define(o, key, v, attrs)`: define an own property (overwrite or append).
    define: u32,
    /// `__get(recv, key) -> i64`: `OrdinaryGet` through the `proto` chain.
    get: u32,
    /// `__set(recv, key, v)`: `OrdinarySet` (strict) on an object-family receiver.
    set: u32,
    /// `__has(key, obj) -> i32`: `HasProperty` through the `proto` chain (`in`).
    has: u32,
    /// `__delete(obj, key)`: tombstone an own property (`delete`).
    delete: u32,
    /// `__own_keys(obj, mode) -> array`: own string keys / values / entries in
    /// own-key order (`Object.keys/values/entries`, `JSON.stringify`).
    own_keys: u32,
    /// `__for_in_keys(obj) -> array`: the `for-in` key snapshot.
    for_in_keys: u32,
    /// `__copy_data(dst, src, excl) -> dst`: `CopyDataProperties` (object
    /// spread and object rest).
    copy_data: u32,
    /// `__array_push(base, value) -> base`: append `value` to the array handle
    /// in place (amortized growth through [`Self::array_reserve`]) and return
    /// the same handle base.
    array_push: u32,
    /// `__array_concat(dst, src) -> dst`: append every element of array `src`
    /// to array `dst` in place and return `dst`. Used to append a spread
    /// source's elements when building a spread array literal or spread call
    /// argument list.
    array_concat: u32,
    /// `__json_escape(str_ptr) -> str_base`: bump-allocate a fresh heap
    /// string that is the JSON-quoted + escaped form of the source string (leading
    /// and trailing `"`, with `"`/`\`/control bytes escaped). Called by
    /// [`Self::json_str`] for string values and object keys.
    json_escape: u32,
    /// `__json_str(v) -> str_base`: the recursive `JSON.stringify` serializer
    /// — returns the heap-string base offset of `v`'s JSON form, composing pieces
    /// with `__concat`, delegating string escaping to [`Self::json_escape`] and
    /// finite-number formatting to the guest number formatter, and calling
    /// itself for nested array/object elements. Backs [`crate::ir::IrNode::JsonStringify`].
    json_str: u32,
    /// `__same_value_zero(a: i64, b: i64) -> i32`: `SameValueZero` over two
    /// jsvals (two heap strings compare by byte content, two numbers as `f64` or
    /// identical bits — so `NaN` matches `NaN` and `+0` matches `-0` — everything
    /// else by identity bits). The key equality of the `__map_*` linear scans.
    same_value_zero: u32,
    /// `__array_search(recv: i64, needle: i64, includes: i32) -> i64` (see
    /// [`builtins::emit_array_search`]): the `indexOf`/`includes` fast path.
    array_search: u32,
    /// `__map_find(handle: i32, key: i64) -> i32`: the byte address of the
    /// matching key slot in a Map/Set's backing array (a `__same_value_zero` linear scan over
    /// the even element slots, skipping `HOLE` tombstones), or `-1` on a miss. Shared by get/has/set/delete.
    map_find: u32,
    /// `__map_set(handle: i32, key: i64, val: i64)`: update the existing
    /// entry for `key`, or append `(key, val)` to the growable backing (via
    /// `__array_push`), rewriting the handle's `entries`/`size` in place. Backs
    /// `map.set` and (with an `undefined` value) `set.add`.
    map_set: u32,
    /// `__map_get(handle: i32, key: i64) -> i64`: the value stored under
    /// `key` (`__map_find` + load), or the `undefined` bits on a miss.
    map_get: u32,
    /// `__map_has(handle: i32, key: i64) -> i32`: `1` iff `__map_find` hits.
    map_has: u32,
    /// `__map_delete(handle: i32, key: i64) -> i32`: tombstone the matching
    /// entry in place (`HOLE` key, `undefined` value), drop `size` by one and
    /// return `1`, or `0` on a miss.
    map_delete: u32,
    /// `__loose_eq(a: i64, b: i64) -> i32`: loose `==` (Abstract Equality with
    /// number/string/boolean coercion; `null`/`undefined` mutually equal; objects
    /// by identity). Backs [`crate::ir::BinOp::LooseEq`]/`LooseNe`.
    loose_eq: u32,
    /// `__str_to_array(s: i64) -> i64`: build a fresh array of a string's
    /// characters (one-byte heap strings). Backs string spread `[...str]`.
    str_to_array: u32,
    /// The in-wasm `JSON.parse` recursive-descent helpers (backing
    /// [`crate::ir::IrNode::JsonParse`]). `__json_ws` skips whitespace;
    /// `__json_throw` raises a syntax error; `__json_string`/`__json_number`
    /// parse the scalar tokens; `__json_value` dispatches on the current byte;
    /// `__json_array`/`__json_object` parse the composites (an object defines
    /// each member through `__define`); and
    /// `__json_parse` is the entry that seeds the cursor globals and enforces no
    /// trailing content.
    json_ws: u32,
    json_throw: u32,
    json_string: u32,
    json_number: u32,
    json_value: u32,
    json_array: u32,
    json_object: u32,
    json_parse: u32,
    /// The wasm type index of the uniform function signature
    /// `(a0..a_{K-1}, env, argc) -> i64`, used as the `call_indirect` type.
    fn_type: u32,
    /// The fixed positional-argument arity `K` of the uniform ABI: how many
    /// argument slots every call site pushes (padding with `undefined` or
    /// truncating extras).
    max_args: u32,
    /// Whether some user function gathers its arguments past the `K`
    /// positional slots (a rest parameter or `arguments`, which snapshot
    /// [`IrNode::SpreadArgsGlobal`]). When `false`, only a builtin callee can
    /// observe more than `K` arguments, so a call passing more of them
    /// publishes the spread argument array only for a builtin callee.
    gathers_arguments: bool,
    /// Whether the program builds any accessor ([`IrNode::MakeAccessor`]).
    /// When `false`, `__get` / `__set` omit the getter/setter invocation and
    /// member accesses skip the post-call exception check.
    has_accessors: bool,
    /// Whether the program tests an intrinsics dirty bit (DESIGN.md §6.4). When
    /// `false`, `__set` / `__define` / `__delete` do not set the bits.
    dirty_tracking: bool,
    /// Base address of the Unicode tables (see [`unicode`]).
    unicode: i32,
    /// Base address of the `__fn_info` table (see [`FnLayout`]). The static
    /// declaration blocks follow it ([`decl_func_base`]). `__fn_materialize`
    /// is the helper right after `__define` (`define + 1`).
    fn_info: i32,
}

impl Helpers<'_> {
    /// `__num_unit(data: i32, i: i32) -> i32`: a code unit behind a tagged pointer.
    fn num_unit(self) -> u32 {
        self.strtod
    }

    /// `__str_ws(c: i32) -> i32`: whether a unit is a `StrWhiteSpaceChar`.
    fn str_ws(self) -> u32 {
        self.strtod + 1
    }

    /// `__dec_parse(data: i32, start: i32, end: i32) -> f64`: the longest `StrDecimalLiteral` prefix, correctly rounded.
    fn dec_parse(self) -> u32 {
        self.strtod + 2
    }

    /// `__dec_slow() -> i64`: Simple Decimal Conversion of the digit buffer.
    fn dec_slow(self) -> u32 {
        self.strtod + 3
    }

    /// `__dec_round() -> i64`: the digit buffer's integer part, rounded.
    fn dec_round(self) -> u32 {
        self.strtod + 4
    }

    /// `__dec_left(shift: i32)`: multiply the digit buffer by `2^shift`.
    fn dec_left(self) -> u32 {
        self.strtod + 5
    }

    /// `__dec_right(shift: i32)`: divide the digit buffer by `2^shift`.
    fn dec_right(self) -> u32 {
        self.strtod + 6
    }

    /// `__digit_span(data, start, end, radix) -> i32`: the end of a digit run.
    fn digit_span(self) -> u32 {
        self.strtod + 7
    }

    /// `__radix_pow2(data, start, end, bits) -> f64`: exact power-of-two radix.
    fn radix_pow2(self) -> u32 {
        self.strtod + 8
    }

    /// `__radix_fold(data, start, end, radix) -> f64`: `f64` digit accumulation.
    fn radix_fold(self) -> u32 {
        self.strtod + 9
    }

    /// `__string_to_number(s: i32) -> f64`: `StringToNumber`.
    fn string_to_number(self) -> u32 {
        self.strtod + 10
    }

    /// `__to_number_prim(v: i64) -> i64`: `ToNumber` of a non-symbol primitive.
    fn to_number_prim(self) -> u32 {
        self.strtod + 11
    }

    /// `__parse_float(v: i64) -> i64`: `parseFloat` of a string jsval.
    fn parse_float(self) -> u32 {
        self.strtod + 12
    }

    /// `__random() -> f64`, the `Math.random` generator (right after `__now`).
    fn random(self) -> u32 {
        self.now + 1
    }

    /// `__parse_int(v: i64, radix: i32) -> i64`: `parseInt` of a string jsval.
    fn parse_int(self) -> u32 {
        self.strtod + 13
    }

    /// Base address of the realm image (see [`realm`]): right after the
    /// static declaration blocks.
    fn realm(self) -> i32 {
        decl_func_base(self.settle_resolve_slot, self)
    }

    /// `__has_own(key, obj) -> i32`: whether `obj` has the own property `key`
    /// (see [`props::emit_has_own`]), right after `__new_array`.
    fn has_own(self) -> u32 {
        self.new_string + 2
    }

    /// The wasm function index of `builtin`'s body; the bodies follow
    /// `__has_own` in [`Builtin::ALL`] order.
    fn builtin(self, builtin: Builtin) -> u32 {
        self.new_string + 3 + builtin.index()
    }

    /// `__umul128(a, b) -> (low, high)`.
    fn umul128(self) -> u32 {
        self.dtoa
    }

    /// `__ryu_pow5(i, inv) -> (low, high)`.
    fn ryu_pow5(self) -> u32 {
        self.dtoa + 1
    }

    /// `__ryu_mul_shift(m, low, high, j) -> i64`.
    fn mul_shift(self) -> u32 {
        self.dtoa + 2
    }

    /// `__ryu_mp5(value, p) -> i32`.
    fn multiple_of_pow5(self) -> u32 {
        self.dtoa + 3
    }

    /// `__ryu_d2d(bits) -> (digits, exp10)`.
    fn ryu_d2d(self) -> u32 {
        self.dtoa + 4
    }

    /// `__scratch_str(ptr, len) -> i32`.
    fn scratch_str(self) -> u32 {
        self.dtoa + 5
    }

    /// `__num_to_str(bits) -> i32`.
    fn num_to_str(self) -> u32 {
        self.dtoa + 6
    }

    /// `__dec_expand(bits) -> (ptr, count, point)`.
    fn dec_expand(self) -> u32 {
        self.dtoa + 7
    }

    /// `__num_format(bits, mode, f) -> i32`.
    fn num_format(self) -> u32 {
        self.dtoa + 8
    }

    /// `__num_radix(bits, radix) -> i32`.
    fn num_radix(self) -> u32 {
        self.dtoa + 9
    }

    /// `__prim_str(v) -> i32`.
    fn prim_str(self) -> u32 {
        self.dtoa + 10
    }

    /// `__msg_str(v) -> i32`.
    fn msg_str(self) -> u32 {
        self.dtoa + 11
    }

    /// `__to_primitive(v, hint) -> i64` (see [`convert::emit_to_primitive`]):
    /// the first of the conversion helpers, which follow the builtin bodies.
    fn to_primitive(self) -> u32 {
        self.builtin(Builtin::ALL[Builtin::ALL.len() - 1]) + 1
    }

    /// `__to_string(v) -> i32` (see [`convert::emit_to_string`]).
    fn to_str(self) -> u32 {
        self.to_primitive() + 1
    }

    /// `__add(a, b) -> i64` (see [`convert::emit_add_helper`]).
    fn add_slow(self) -> u32 {
        self.to_primitive() + 2
    }

    /// `__compare(a, b, op) -> i32` (see [`convert::emit_compare`]).
    fn compare(self) -> u32 {
        self.to_primitive() + 3
    }

    /// `__array_join(recv, sep) -> i64` (see [`convert::emit_array_join`]).
    fn array_join(self) -> u32 {
        self.to_primitive() + 4
    }

    /// `__to_object(v) -> i64` (see [`builtins::emit_to_object`]), right
    /// after `__array_join`.
    fn to_object(self) -> u32 {
        self.to_primitive() + 5
    }

    /// `__json_rep(holder, key, value, replacer, list, stack) -> i64` (see
    /// [`builtins::emit_json_rep`]), right after `__to_object`.
    fn json_rep(self) -> u32 {
        self.to_primitive() + 6
    }

    /// `__json_indent(s, gap) -> i32` (see [`builtins::emit_json_indent`]).
    fn json_indent(self) -> u32 {
        self.to_primitive() + 7
    }

    /// `__json_revive(holder, key, reviver) -> i64` (see
    /// [`builtins::emit_json_revive`]).
    fn json_revive(self) -> u32 {
        self.to_primitive() + 8
    }

    /// `__define_own(o, key, desc, value, get, set) -> i32` (see
    /// [`props::emit_define_own`]), right after `__json_revive`.
    fn define_own(self) -> u32 {
        self.to_primitive() + 9
    }

    /// `__to_desc(attrs, list)` (see [`props::emit_to_desc`]).
    fn to_desc(self) -> u32 {
        self.to_primitive() + 10
    }

    /// `__own_desc(obj, key) -> i64` (see [`props::emit_own_desc`]).
    fn own_desc(self) -> u32 {
        self.to_primitive() + 11
    }

    /// `__set_integrity(obj, level)` (see [`props::emit_set_integrity`]).
    fn set_integrity(self) -> u32 {
        self.to_primitive() + 12
    }

    /// `__test_integrity(obj, level) -> i32` (see
    /// [`props::emit_test_integrity`]).
    fn test_integrity(self) -> u32 {
        self.to_primitive() + 13
    }

    /// `__own_enum(obj, key) -> i32` (see [`props::emit_own_enum`]).
    fn own_enum(self) -> u32 {
        self.to_primitive() + 14
    }

    /// `__set_proto(o, proto) -> i32` (see [`props::emit_set_proto`]).
    fn set_proto(self) -> u32 {
        self.to_primitive() + 15
    }

    /// The self-resolution `TypeError` constructor `() -> i64` (see
    /// [`builtins::emit_promise_self_error`]).
    fn promise_self_error(self) -> u32 {
        self.to_primitive() + 16
    }

    /// `__symbol_get(v, sym) -> i64` (see [`props::emit_symbol_get`]).
    fn symbol_get(self) -> u32 {
        self.to_primitive() + 17
    }

    /// `__super_get(home, key, recv) -> i64` (see [`props::emit_super_get`]).
    fn super_get(self) -> u32 {
        self.to_primitive() + 18
    }

    /// `__super_set(home, key, v, recv)` (see [`props::emit_super_set`]).
    fn super_set(self) -> u32 {
        self.to_primitive() + 19
    }

    /// `__private_find(o, key) -> i32` (see [`props::emit_private_find`]).
    fn private_find(self) -> u32 {
        self.to_primitive() + 20
    }

    /// `__private_add(o, key, v, attrs, name)` (see
    /// [`props::emit_private_add`]).
    fn private_add(self) -> u32 {
        self.to_primitive() + 21
    }

    /// `__private_get(o, key, name) -> i64` (see [`props::emit_private_get`]).
    fn private_get(self) -> u32 {
        self.to_primitive() + 22
    }

    /// `__private_set(o, key, v, name) -> i64` (see
    /// [`props::emit_private_set`]).
    fn private_set(self) -> u32 {
        self.to_primitive() + 23
    }

    /// `__private_brand(o, v, brand, name, mode) -> i64` (see
    /// [`props::emit_private_brand`]).
    fn private_brand(self) -> u32 {
        self.to_primitive() + 24
    }

    /// `__private_in(o, brand, name) -> i64` (see [`props::emit_private_in`]).
    fn private_in(self) -> u32 {
        self.to_primitive() + 25
    }

    /// The funcref-table slot of `builtin`'s body: the builtins follow the
    /// user functions and the two settlers.
    fn builtin_slot(self, builtin: Builtin) -> u32 {
        self.settle_resolve_slot + 2 + builtin.index()
    }

    /// `__new_array(len) -> i32`: the array constructor, right after
    /// `new_string`.
    fn new_array(self) -> u32 {
        self.new_string + 1
    }

    /// `__str_exotic_index(o, key) -> i32`: the `String` wrapper own index
    /// `key` names on `o`, else `-1`; right after `__index_string`.
    fn str_exotic_index(self) -> u32 {
        self.index_string + 1
    }

    /// `__array_reserve(base, need)`: grow the array handle's element store to
    /// hold at least `need` elements (`cap' = max(need, 4, 2 * cap)`), copying
    /// the `len` live slots and repointing `elems`; the handle never moves. It
    /// is the helper right before `__array_push`.
    fn array_reserve(self) -> u32 {
        self.array_push - 1
    }

    /// `__array_set(recv, key, v)`: the slow path of a member store on an
    /// array — growth by index, DF9 key routing, named properties (see
    /// [`props::emit_array_set`]); may leave an exception pending. It is the
    /// helper right after `__array_concat`.
    fn array_set(self) -> u32 {
        self.array_concat + 1
    }

    /// `__array_set_length(arr, v)`: the `length` write on the array handle
    /// `arr` (see [`props::emit_array_set_length`]); may leave a `RangeError`
    /// pending. It is the helper right after `__array_set`.
    fn array_set_length(self) -> u32 {
        self.array_concat + 2
    }

    /// `__sparse_get(arr, i) -> i64`: element `i` (or [`HOLE_BITS`]) of a
    /// `SPARSE` array (see [`props::emit_sparse_get`]). It is the helper right
    /// after `__array_set_length`.
    fn sparse_get(self) -> u32 {
        self.array_concat + 3
    }

    /// `__sparse_set(arr, i, v)`: store element `i` of a `SPARSE` array,
    /// raising `length` past `i` (see [`props::emit_sparse_set`]). It is the
    /// helper right after `__sparse_get`.
    fn sparse_set(self) -> u32 {
        self.array_concat + 4
    }

    /// `__protos_indexless(o) -> i32`: whether no prototype of `o` can hold an
    /// integer-index property (see [`props::emit_protos_indexless`]). It is
    /// the helper right after `__sparse_set`.
    fn protos_indexless(self) -> u32 {
        self.array_concat + 5
    }

    /// `__proto_elem(o, i) -> i64`: element `i` that `o` does not own, read
    /// through its prototype chain ([`HOLE_BITS`] when absent; see
    /// [`props::emit_proto_elem`]). It is the helper right after
    /// `__protos_indexless`.
    fn proto_elem(self) -> u32 {
        self.array_concat + 6
    }

    /// `__proto_set(o, i, v) -> i32`: the inherited half of a store to an
    /// element `o` does not own (see [`props::emit_proto_set`]). It is the
    /// helper right after `__proto_elem`.
    fn proto_set(self) -> u32 {
        self.array_concat + 7
    }

    /// `__array_define(recv, key, v)`: `__array_set` without the prototype
    /// chain, for `CreateDataPropertyOrThrow` (see [`props::emit_array_set`]).
    /// It is the helper right after `__proto_set`.
    fn array_define(self) -> u32 {
        self.array_concat + 8
    }

    /// `__sparse_define(o, i, v)`: the `SPARSE` arm of `__array_define` (see
    /// [`props::emit_sparse_set`]), right after `__array_define`.
    fn sparse_define(self) -> u32 {
        self.array_concat + 9
    }
}

/// The address of the static function block of top-level declaration
/// `table_index`: the blocks follow the `__fn_info` table's entries for the
/// user functions, the two settlers, and the builtins (`settle_resolve_slot`
/// is the user function count).
fn decl_func_base(table_index: u32, helpers: Helpers<'_>) -> i32 {
    let info_entries = helpers.settle_resolve_slot.cast_signed() + 2 + builtin_count();
    helpers.fn_info
        + abi::FN_INFO_ENTRY_SIZE.cast_signed() * info_entries
        + abi::FUNC_BLOCK_SIZE.cast_signed() * table_index.cast_signed()
}

/// The static root list's addresses (design/runtime-memory.md §5.4): the
/// `fn_count` static declaration blocks from `blocks`, then the realm image's
/// intrinsic and builtin-method blocks ([`realm::static_objects`], the
/// `Symbol.for` registry among them).
fn static_roots(blocks: i32, fn_count: i32, realm: i32) -> Vec<u32> {
    let size = abi::FUNC_BLOCK_SIZE.cast_signed();
    (0..fn_count)
        .map(|i| (blocks + size * i).cast_unsigned())
        .chain(realm::static_objects(realm))
        .collect()
}

/// The number of builtin bodies (funcref-table slots after the settlers).
fn builtin_count() -> i32 {
    i32::try_from(Builtin::ALL.len()).unwrap_or(0)
}

/// Where the function data lives in the data segment: the `__fn_info` table
/// (one [`abi::FN_INFO_ENTRY_SIZE`]-byte entry per funcref-table index:
/// user functions, settlers, builtins), after it one static
/// [`abi::FUNC_BLOCK_SIZE`]-byte function block per user function (the object
/// of a top-level declaration, [`IrNode::DeclFunc`]), then the realm image.
#[derive(Clone, Copy)]
struct FnLayout {
    /// Base address of the info table.
    info: i32,
    /// Base address of the static function blocks.
    blocks: i32,
    /// Base address of the realm image.
    realm: i32,
    /// Base address of the Unicode table segment ([`unicode::data`]).
    unicode: i32,
}

mod array;
mod builder;
mod builtins;
mod convert;
mod dtoa;
mod expr;
mod function;
mod gc;
mod generator;
mod json_parse;
mod libm;
mod numeric;
mod object;
mod promise_engine;
mod props;
mod realm;
mod render;
mod roots;
mod runtime;
mod sections;
mod stmt;
mod strtod;
#[cfg(test)]
mod tests;
mod unicode;
mod unicode_data;
mod value;

#[allow(clippy::wildcard_imports)]
use array::*;
#[allow(clippy::wildcard_imports)]
use convert::*;
#[allow(clippy::wildcard_imports)]
use expr::*;
#[allow(clippy::wildcard_imports)]
use function::*;
#[allow(clippy::wildcard_imports)]
use json_parse::*;
#[allow(clippy::wildcard_imports)]
use numeric::*;
#[allow(clippy::wildcard_imports)]
use object::*;
#[allow(clippy::wildcard_imports)]
use props::*;
use realm::{
    AsyncGenReaction, AsyncGenRequest, Builtin, Intrinsic, ProtocolOp, ReflectOp, StringOp,
    WellKnown, intrinsic_bits,
};
#[allow(clippy::wildcard_imports)]
use runtime::*;
#[allow(clippy::wildcard_imports)]
use sections::*;
#[allow(clippy::wildcard_imports)]
use stmt::*;
#[allow(clippy::wildcard_imports)]
use value::*;

/// Options of a compilation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompileOptions {
    /// Render the completion value to fd 1 after the final drain.
    pub print_completion: bool,
    /// Test only (not exposed by the CLI): an allocation budget of `0`, so
    /// every allocation runs a full collection (design/runtime-memory.md
    /// §5.6, Q20).
    pub gc_stress: bool,
}

/// Emit a complete WebAssembly command module for `program`: exports
/// `_start` and `memory`, imports the four WASI functions. See the module
/// documentation.
#[allow(clippy::too_many_lines)]
pub fn emit(program: &IrProgram, options: CompileOptions) -> Vec<u8> {
    let mut module = Module::new();

    // Collect every distinct string literal and assign each a compile-time byte
    // offset in linear memory.
    let (offsets, strings_end) = collect_strings(program);
    // The function info table (user functions, the two `new Promise`
    // settlers, the builtins), the static declaration blocks, and the realm
    // image follow the string blobs, then the intern table and the Unicode
    // tables; the GC heap begins at the next page boundary.
    let n = program.functions.len();
    let fn_count = i32::try_from(n).unwrap_or(i32::MAX);
    let info_count = fn_count + 2 + builtin_count();
    let blocks = strings_end + abi::FN_INFO_ENTRY_SIZE.cast_signed() * info_count;
    let realm = blocks + abi::FUNC_BLOCK_SIZE.cast_signed() * fn_count;
    let intern_base = realm + realm::image_size();
    let intern = intern_table(&offsets, intern_base);
    // The raw Unicode tables and the collector's static root list end the
    // static region; blocks stay 8-aligned.
    let unicode_base = intern_base + 4 * intern.cap;
    let roots_base = (unicode_base + unicode::data_size() + 3) & -4;
    let static_roots = static_roots(blocks, fn_count, realm);
    let layout = FnLayout {
        info: strings_end,
        blocks,
        realm,
        unicode: unicode_base,
    };
    let heap_start = gc::heap_start(roots_base + gc::root_list_size(static_roots.len()));
    let gc_layout = gc::GcLayout {
        heap_start,
        roots: roots_base,
        module_cells: program.module_cells,
        stress: options.gc_stress,
    };

    // Type/function sections and the resolved helper indices (see
    // [`build_type_and_function_sections`]).
    // Per-function exception-unwinding state, reset at each function body.
    // Owned here so it outlives every `emit_main`/`emit_function` call below.
    let unwind = RefCell::new(Unwind::default());
    let (mut types, mut functions, helpers, import_types) =
        build_type_and_function_sections(program, &unwind, layout);
    libm::register(&mut types, &mut functions);
    promise_engine::register(&mut types, &mut functions);
    gc::register(&mut types, &mut functions);
    module.section(&types);

    // Import section: exactly the four WASI functions (indices 0..=3).
    // Imports occupy the lowest function indices, so every defined
    // function/element/export index is shifted by IMPORT_FUNC_COUNT.
    let mut imports = ImportSection::new();
    let names = [
        (abi::WASI_MODULE, abi::WASI_FD_WRITE_NAME),
        (abi::WASI_MODULE, abi::WASI_CLOCK_TIME_GET_NAME),
        (abi::WASI_MODULE, abi::WASI_RANDOM_GET_NAME),
        (abi::WASI_MODULE, abi::WASI_PROC_EXIT_NAME),
    ];
    for ((module_name, name), ty) in names.into_iter().zip(import_types) {
        imports.import(module_name, name, EntityType::Function(ty));
    }
    module.section(&imports);

    module.section(&functions);

    // Table section: a funcref table listing every user function, the two
    // `new Promise` settlers (slots `n`, `n + 1`) and the builtins, so a
    // function value can be called through `call_indirect`. The element
    // segment (below, after exports) populates it.
    let table_len = (n + 2 + Builtin::ALL.len()) as u64;
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        table64: false,
        minimum: table_len,
        maximum: Some(table_len),
        shared: false,
    });
    module.section(&tables);

    // Memory section: one linear memory (min: the pages below the heap;
    // capped at `abi::MEMORY_MAX_PAGES`; `__gc_take_pages` grows it on
    // demand), exported below.
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: u64::from(heap_start.cast_unsigned() / abi::GC_PAGE_SIZE),
        maximum: Some(abi::MEMORY_MAX_PAGES),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memories);

    // Global section: the page frontier plus the exception-unwinding globals
    // (see [`build_globals`]).
    module.section(&build_globals(heap_start, &intern, gc_layout));

    // Export section: exactly `_start` and memory index 0 as "memory".
    let mut exports = ExportSection::new();
    exports.export(abi::START_EXPORT_NAME, ExportKind::Func, helpers.start());
    exports.export(MEMORY_EXPORT_NAME, ExportKind::Memory, 0);
    module.section(&exports);

    // Element section: an active segment filling the funcref table at offset
    // 0. Table slot `i < n` holds DEFINED function index
    // `i + 1 + IMPORT_FUNC_COUNT` (the user functions, shifted past `main` and
    // the imports); the funcref table's slot indices themselves are unaffected
    // by imports, so a `call_indirect` table index needs no adjustment.
    let mut func_indices: Vec<u32> = (1..=u32::try_from(n).unwrap_or(u32::MAX))
        .map(|i| i + IMPORT_FUNC_COUNT)
        .collect();
    // The two `new Promise` settler functions fill slots `n`/`n + 1`; they are
    // the DEFINED functions right after the `__invoke1` and `__async_step`
    // trampolines. The builtin bodies fill the slots after them.
    func_indices.push(helpers.async_step + 1);
    func_indices.push(helpers.async_step + 2);
    func_indices.extend(Builtin::ALL.map(|b| helpers.builtin(b)));
    let mut elements = ElementSection::new();
    elements.active(
        None,
        &ConstExpr::i32_const(0),
        Elements::Functions(Cow::Owned(func_indices)),
    );
    module.section(&elements);

    // Code section: `main` first, then each user function's body, then the
    // helpers last (mirroring the function-index order above).
    let mut code = CodeSection::new();
    let leaves = roots::leaf_set(helpers);
    code.function(&emit_main(program, &offsets, helpers, &leaves));
    for func in &program.functions {
        // A generator state machine emits a `switch (gen.state)` dispatch
        // over its segments rather than a straight-line body.
        if func.machine.is_some() {
            code.function(&generator::emit_generator_sm(
                func, &offsets, helpers, &leaves,
            ));
        } else {
            code.function(&emit_function(func, &offsets, helpers, &leaves));
        }
    }
    code.function(&emit_alloc(helpers, &leaves, false));
    code.function(&emit_concat(helpers.alloc, &leaves));
    code.function(&emit_str_eq(&leaves));
    code.function(&emit_str_cmp(&leaves));
    code.function(&emit_str_index(helpers.alloc, &leaves));
    code.function(&emit_str_hash(&leaves));
    code.function(&emit_intern_slot(helpers, &leaves));
    code.function(&emit_intern_find(helpers, &leaves));
    code.function(&emit_intern(helpers, &leaves));
    code.function(&emit_index_value(&leaves));
    code.function(&emit_index_string(helpers, &leaves));
    code.function(&emit_str_exotic_index(helpers, &leaves));
    code.function(&emit_to_key(&offsets, helpers, &leaves));
    code.function(&emit_prop_find(helpers, &leaves));
    code.function(&emit_define(helpers, &leaves));
    code.function(&emit_fn_materialize(&offsets, helpers, &leaves));
    code.function(&emit_get(&offsets, helpers, &leaves));
    code.function(&emit_set(helpers, &leaves));
    code.function(&emit_has(&offsets, helpers, &leaves));
    code.function(&emit_delete(&offsets, helpers, &leaves));
    code.function(&emit_own_keys(helpers, &leaves));
    code.function(&emit_for_in_keys(helpers, &leaves));
    code.function(&emit_copy_data(helpers, &leaves));
    code.function(&emit_array_reserve(helpers.alloc, &leaves));
    code.function(&emit_array_push(
        helpers.array_reserve(),
        helpers.sparse_define(),
        &leaves,
    ));
    code.function(&emit_array_concat(
        helpers.array_reserve(),
        helpers.sparse_get(),
        helpers.proto_elem(),
        &leaves,
    ));
    code.function(&emit_array_set(&offsets, helpers, false, &leaves));
    code.function(&emit_array_set_length(&offsets, helpers, &leaves));
    code.function(&emit_sparse_get(helpers, &leaves));
    code.function(&emit_sparse_set(helpers, false, &leaves));
    code.function(&emit_protos_indexless(&leaves));
    code.function(&emit_proto_elem(helpers, &leaves));
    code.function(&emit_proto_set(helpers, &leaves));
    code.function(&emit_array_set(&offsets, helpers, true, &leaves));
    code.function(&emit_sparse_set(helpers, true, &leaves));
    code.function(&emit_json_escape(helpers.alloc, &leaves));
    code.function(&emit_json_str(
        helpers.num_to_str(),
        helpers.json_escape,
        JsonTokens::resolve(&offsets),
        &leaves,
    ));
    code.function(&emit_same_value_zero(helpers.str_eq, &leaves));
    code.function(&builtins::emit_array_search(helpers, &leaves));
    code.function(&emit_map_find(helpers.same_value_zero, &leaves));
    code.function(&emit_map_set(helpers.map_find, helpers.array_push, &leaves));
    code.function(&emit_map_get(helpers.map_find, &leaves));
    code.function(&emit_map_has(helpers.map_find, &leaves));
    code.function(&emit_map_delete(helpers.map_find, &leaves));
    // `__loose_eq` (loose `==`) and `__str_to_array` (string spread) follow the
    // Map/Set helpers.
    code.function(&emit_loose_eq(helpers, &leaves));
    code.function(&emit_str_to_array(
        helpers.new_array(),
        helpers.str_index,
        helpers.alloc,
        &leaves,
    ));
    // The in-wasm `JSON.parse` family, in the order their indices were assigned.
    code.function(&emit_json_ws(&leaves));
    code.function(&emit_json_throw(&offsets, helpers, &leaves));
    code.function(&emit_json_string(helpers.alloc, helpers, &leaves));
    code.function(&emit_json_number(helpers, &leaves));
    code.function(&emit_json_value(helpers, &leaves));
    code.function(&emit_json_array(helpers, &leaves));
    code.function(&emit_json_object(helpers, &leaves));
    code.function(&emit_json_parse(helpers, &leaves));
    // `__gen_to_array` — the generator-spread drain — precedes the
    // trampoline so every later index stays stable.
    code.function(&generator::emit_gen_to_array(helpers, &leaves));
    code.function(&emit_nullish_throw(&offsets, helpers, &leaves));
    code.function(&emit_prop_throw(&offsets, helpers, &leaves));
    // The `__invoke1` callback trampoline is appended after every helper so
    // every helper index stays stable; the `__async_step` trampoline
    // follows it, then the two `new Promise` settler functions.
    code.function(&emit_box_f64_helper(&leaves));
    code.function(&emit_to_number_helper(&offsets, helpers, &leaves));
    for helper in dtoa::emit_helpers(&offsets, helpers, &leaves) {
        code.function(&helper);
    }
    code.function(&strtod::emit_num_unit(&leaves));
    code.function(&strtod::emit_str_ws(&leaves));
    code.function(&strtod::emit_dec_parse(helpers, &leaves));
    code.function(&strtod::emit_dec_slow(helpers, &leaves));
    code.function(&strtod::emit_dec_round(&leaves));
    code.function(&strtod::emit_dec_left(&leaves));
    code.function(&strtod::emit_dec_right(&leaves));
    code.function(&strtod::emit_digit_span(helpers, &leaves));
    code.function(&strtod::emit_radix_pow2(helpers, &leaves));
    code.function(&strtod::emit_radix_fold(helpers, &leaves));
    code.function(&strtod::emit_string_to_number(helpers, &leaves));
    code.function(&strtod::emit_to_number_prim(helpers, &leaves));
    code.function(&strtod::emit_parse_float(helpers, &leaves));
    code.function(&strtod::emit_parse_int(helpers, &leaves));
    code.function(&numeric::emit_now(&leaves));
    code.function(&numeric::emit_random(&leaves));
    code.function(&emit_invoke1(helpers, &leaves));
    code.function(&generator::emit_async_step(helpers, &leaves));
    code.function(&emit_settler(abi::PROMISE_FULFILLED, helpers, &leaves));
    code.function(&emit_settler(abi::PROMISE_REJECTED, helpers, &leaves));
    code.function(&emit_new_string(helpers.alloc, &leaves));
    code.function(&emit_new_array(
        helpers,
        intrinsic_bits(Intrinsic::Prototype(BuiltinCtor::Array), helpers),
        &leaves,
    ));
    // The realm: `__has_own`, then the builtin bodies.
    code.function(&emit_has_own(&offsets, helpers, &leaves));
    for builtin in Builtin::ALL {
        code.function(&builtins::emit_builtin(builtin, &offsets, helpers, &leaves));
    }
    // The conversion helpers follow the builtin bodies.
    code.function(&emit_to_primitive(&offsets, helpers, &leaves));
    code.function(&convert::emit_to_string(&offsets, helpers, &leaves));
    code.function(&emit_add_helper(helpers, &leaves));
    code.function(&emit_compare(helpers, &leaves));
    code.function(&emit_array_join(&offsets, helpers, &leaves));
    code.function(&builtins::emit_to_object(&offsets, helpers, &leaves));
    code.function(&builtins::emit_json_rep(&offsets, helpers, &leaves));
    code.function(&builtins::emit_json_indent(helpers, &leaves));
    code.function(&builtins::emit_json_revive(helpers, &leaves));
    code.function(&emit_define_own(&offsets, helpers, &leaves));
    code.function(&emit_to_desc(&offsets, helpers, &leaves));
    code.function(&emit_own_desc(&offsets, helpers, &leaves));
    code.function(&emit_set_integrity(helpers, &leaves));
    code.function(&emit_test_integrity(&leaves));
    code.function(&emit_own_enum(&offsets, helpers, &leaves));
    code.function(&emit_set_proto(helpers, &leaves));
    code.function(&builtins::emit_promise_self_error(
        &offsets, helpers, &leaves,
    ));
    code.function(&emit_symbol_get(helpers, &leaves));
    code.function(&emit_super_get(&offsets, helpers, &leaves));
    code.function(&emit_super_set(&offsets, helpers, &leaves));
    code.function(&emit_private_find(helpers, &leaves));
    code.function(&emit_private_add(helpers, &leaves));
    code.function(&emit_private_get(helpers, &leaves));
    code.function(&emit_private_set(helpers, &leaves));
    code.function(&emit_private_brand(helpers, &leaves));
    code.function(&emit_private_in(&offsets, helpers, &leaves));
    for helper in render::emit_helpers(&offsets, helpers, &leaves) {
        code.function(&helper);
    }
    code.function(&emit_start(&offsets, helpers, options, &leaves));
    // The families after `_start`, in index order: Unicode, libm, the
    // promise engine, then the allocator and collector.
    for helper in unicode::emit_helpers(helpers, &leaves) {
        code.function(&helper);
    }
    for helper in libm::emit_helpers(helpers, &leaves) {
        code.function(&helper);
    }
    for helper in promise_engine::emit_helpers(&offsets, helpers, &leaves) {
        code.function(&helper);
    }
    for helper in gc::emit_helpers(helpers, &leaves, gc_layout) {
        code.function(&helper);
    }
    module.section(&code);

    // Data section (after code): the string-literal blobs and the intern table.
    let mut data = emit_data_section(&offsets, &intern, &program.functions, layout);
    data.active(
        0,
        &ConstExpr::i32_const(roots_base),
        gc::root_list(&static_roots),
    );
    libm::emit_data(&mut data);
    module.section(&data);

    module.finish()
}
