//! Emit: lower the crate's numeric [`IrNode`] into a complete WebAssembly module.
//!
//! This is the final backend stage. It walks an [`IrNode`] tree and produces the
//! raw bytes of a valid WebAssembly module using the [`wasm_encoder`] crate.
//!
//! The emitted module exports exactly one function under the name **`"main"`**
//! with the signature `() -> i64` (no parameters, a single `i64` result). The
//! `i64` result is a NaN-boxed jsval (see [`crate::run`] for the layout); using
//! an `i64` carrier — not `f64` — guarantees the value's bits survive every
//! wasm op and the wasmtime return boundary without NaN canonicalization. The
//! run/CLI stages depend on this exact export name and signature. `main`
//! declares one `i64` local per top-level variable; the program's value is the
//! value of its last expression statement, which is left on the operand stack.
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

use crate::abi;
use crate::ir::{
    ArrayHofOp, BinOp, ConvertOp, DateOp, ErrorKind, FnKind, InstanceOfKind, IrCase, IrFunction,
    IrNode, IrProgram, IrStmt, LogicalOp, MapOp, MathOp, MethodOp, ObjectBuiltinOp, OpSetKind,
    PropKey, ReceiverOp, SpreadCallee, SpreadElem,
};

/// The name of the entry function exported from the emitted module.
const EXPORT_NAME: &str = "main";

/// The name under which the module's linear memory is exported (see
/// [`abi::MEMORY_EXPORT_NAME`]). [`crate::run`] reads it back to resolve heap
/// pointers.
const MEMORY_EXPORT_NAME: &str = abi::MEMORY_EXPORT_NAME;

/// The module and field name of the host `print` import backing `console.log`.
/// [`crate::run`] supplies this function through a `wasmtime::Linker`.
const PRINT_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const PRINT_IMPORT_NAME: &str = abi::PRINT_IMPORT_NAME;

/// The module and field name of the host `__convert` import backing the
/// `String`/`Number`/`parseFloat` conversions. It receives the
/// argument jsval plus a `CONVERT_OP_*` flag and returns the converted jsval,
/// reusing the run stage's `format_value`/parse helpers (and allocating a heap
/// string in guest memory for `String`).
const CONVERT_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const CONVERT_IMPORT_NAME: &str = abi::CONVERT_IMPORT_NAME;

/// `__convert` op flags; see [`abi`].
const CONVERT_OP_TO_STRING: i32 = abi::CONVERT_OP_TO_STRING;
const CONVERT_OP_TO_NUMBER: i32 = abi::CONVERT_OP_TO_NUMBER;
const CONVERT_OP_PARSE_FLOAT: i32 = abi::CONVERT_OP_PARSE_FLOAT;

/// The module and field name of the host `__method` import backing the
/// non-callback array/string methods. It receives the receiver jsval, a
/// `METHOD_OP_*` flag, and two argument jsval slots (padded with `undefined` when
/// the method takes fewer), branches on the receiver's runtime TYPE BYTE for the
/// overloaded `indexOf`/`slice`, and returns the result jsval (allocating a heap
/// string/array in guest memory when it builds one).
const METHOD_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const METHOD_IMPORT_NAME: &str = abi::METHOD_IMPORT_NAME;

/// The module and field names of the host promise imports: `__promise`
/// backs the single-value primitives (`Promise.resolve`/`reject`, top-level
/// `await`, `p.finally`) and `__then` backs `p.then`/`p.catch`. [`crate::run`]
/// supplies both through a `wasmtime::Linker`, driving the host-side promise
/// engine (reactions + microtask queue).
const PROMISE_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const PROMISE_IMPORT_NAME: &str = abi::PROMISE_IMPORT_NAME;
const THEN_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const THEN_IMPORT_NAME: &str = abi::THEN_IMPORT_NAME;
const NOW_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const NOW_IMPORT_NAME: &str = abi::NOW_IMPORT_NAME;
const PARSE_INT_IMPORT_MODULE: &str = abi::IMPORT_MODULE;
const PARSE_INT_IMPORT_NAME: &str = abi::PARSE_INT_IMPORT_NAME;

/// `__promise` op flags; see [`abi`].
const PROMISE_OP_RESOLVE: i32 = abi::PROMISE_OP_RESOLVE;
const PROMISE_OP_REJECT: i32 = abi::PROMISE_OP_REJECT;
const PROMISE_OP_AWAIT: i32 = abi::PROMISE_OP_AWAIT;
const PROMISE_OP_FINALLY: i32 = abi::PROMISE_OP_FINALLY;
const PROMISE_OP_NEW: i32 = abi::PROMISE_OP_NEW;
const PROMISE_OP_SETTLE_FULFILL: i32 = abi::PROMISE_OP_SETTLE_FULFILL;
const PROMISE_OP_SETTLE_REJECT: i32 = abi::PROMISE_OP_SETTLE_REJECT;
const PROMISE_OP_ASYNC_SPAWN: i32 = abi::PROMISE_OP_ASYNC_SPAWN;

/// The export name of the `__invoke1` callback trampoline the host
/// promise engine calls re-entrantly to run a reaction's guest callback.
const INVOKE_EXPORT_NAME: &str = abi::INVOKE_EXPORT_NAME;

/// The export name of the `__async_step` generator-step trampoline the
/// host async engine calls to advance an `async function` body's state machine.
const ASYNC_STEP_EXPORT_NAME: &str = abi::ASYNC_STEP_EXPORT_NAME;

/// The export names of the bump allocator and the guest constructors the
/// hosts allocate through (see [`abi`]).
const ALLOC_EXPORT_NAME: &str = abi::ALLOC_EXPORT_NAME;
const NEW_STRING_EXPORT_NAME: &str = abi::NEW_STRING_EXPORT_NAME;
const NEW_ARRAY_EXPORT_NAME: &str = abi::NEW_ARRAY_EXPORT_NAME;
const NEW_PROMISE_EXPORT_NAME: &str = abi::NEW_PROMISE_EXPORT_NAME;

/// `__method` op flags; see [`abi`].
const METHOD_OP_INDEX_OF: i32 = abi::METHOD_OP_INDEX_OF;
const METHOD_OP_INCLUDES: i32 = abi::METHOD_OP_INCLUDES;
const METHOD_OP_JOIN: i32 = abi::METHOD_OP_JOIN;
const METHOD_OP_SLICE: i32 = abi::METHOD_OP_SLICE;
const METHOD_OP_TO_UPPER: i32 = abi::METHOD_OP_TO_UPPER;
const METHOD_OP_TO_LOWER: i32 = abi::METHOD_OP_TO_LOWER;
const METHOD_OP_CHAR_AT: i32 = abi::METHOD_OP_CHAR_AT;
const METHOD_OP_TRIM: i32 = abi::METHOD_OP_TRIM;
const METHOD_OP_TRIM_START: i32 = abi::METHOD_OP_TRIM_START;
const METHOD_OP_TRIM_END: i32 = abi::METHOD_OP_TRIM_END;
const METHOD_OP_REPEAT: i32 = abi::METHOD_OP_REPEAT;
const METHOD_OP_STARTS_WITH: i32 = abi::METHOD_OP_STARTS_WITH;
const METHOD_OP_ENDS_WITH: i32 = abi::METHOD_OP_ENDS_WITH;
const METHOD_OP_PAD_START: i32 = abi::METHOD_OP_PAD_START;
const METHOD_OP_PAD_END: i32 = abi::METHOD_OP_PAD_END;
const METHOD_OP_SUBSTRING: i32 = abi::METHOD_OP_SUBSTRING;
const METHOD_OP_SPLIT: i32 = abi::METHOD_OP_SPLIT;

/// The name under which the bump-allocator heap pointer global is exported so the
/// run stage can detect the out-of-memory sentinel.
const HEAP_PTR_EXPORT_NAME: &str = abi::HEAP_PTR_EXPORT_NAME;

/// The name under which the pending-exception flag global (`$exc_pending`, i32) is
/// exported so the run stage can detect an uncaught exception after `main` returns.
/// `0` = no exception in flight, `1` = an exception is propagating.
const EXC_PENDING_EXPORT_NAME: &str = abi::EXC_PENDING_EXPORT_NAME;

/// The name under which the in-flight exception value global (`$exc_value`, i64) is
/// exported so the run stage can render an uncaught exception. Holds the
/// thrown jsval bits; only meaningful while `$exc_pending` is `1`.
const EXC_VALUE_EXPORT_NAME: &str = abi::EXC_VALUE_EXPORT_NAME;

/// The number of imported functions the module declares: the host `print`
/// function occupies function index `0`, the host `__convert` function
/// index `1`, and the host `__method` function index `2`, so
/// every DEFINED function — `main`, the user functions, and the runtime helpers —
/// plus the funcref-table element indices and the exported `main` index all shift
/// up by this count.
///
/// Two more imports — `__promise` (index 3) and `__then` (index 4) — back the
/// host promise engine, and `__now` (index 5) is the host wall clock backing
/// `Date.now()` / `new Date()`, and `__parse_int` (index 6) backs
/// `parseInt(s, radix)`, so the count is `7`.
const IMPORT_FUNC_COUNT: u32 = 7;

/// `print` import flags; see [`abi`]: a value plus a trailing space, a
/// value plus a newline, or a newline alone for zero-argument `console.log()`.
const PRINT_FLAG_SEP: i32 = abi::PRINT_FLAG_SEP;
const PRINT_FLAG_END: i32 = abi::PRINT_FLAG_END;
const PRINT_FLAG_NEWLINE: i32 = abi::PRINT_FLAG_NEWLINE;

/// The wasm global index of the bump-allocator heap pointer (`__heap_ptr`).
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
/// [`THIS_GLOBAL`]): `$__json_data` is the byte offset of the input string's UTF-8
/// payload (`base + HEADER_SIZE + 4`), `$__json_pos` is the current byte offset
/// into that payload, and `$__json_len` is the input's byte length. Set once at
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

/// The wasm global index of module cell `0` (`IrNode::ModuleCell`); module cell
/// `n` is global `MODULE_CELL_GLOBAL_BASE + n` (i64 cell pointers, appended after
/// the intern-table globals and set in `main`'s prologue).
const MODULE_CELL_GLOBAL_BASE: u32 = 12;

/// The TDZ hole a module cell holds until its binding is initialized
/// (`IrNode::IsHole`): an `undefined`-tagged NaN box with a nonzero payload, which
/// no JS value ever produces.
const TDZ_HOLE_BITS: i64 = abi::UNDEFINED.cast_signed() | 1;

/// [`CF_KIND_GLOBAL`] value for a pending `return` (payload in
/// [`CF_VALUE_GLOBAL`]).
const CF_RETURN: i32 = 1;

/// [`CF_KIND_GLOBAL`] value for a pending `break`.
const CF_BREAK: i32 = 2;

/// [`CF_KIND_GLOBAL`] value for a pending `continue`.
const CF_CONTINUE: i32 = 3;

/// The base heap offset: the first byte of static string-literal data (offset 0
/// is reserved as a null guard). Literal blobs are packed contiguously from here;
/// the bump allocator (`__alloc`) then starts at the first free offset *after*
/// them (see [`emit`]).
const HEAP_BASE: i32 = 8;

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

/// Type byte for a heap array block: `[type=1][pad×3][len:u32][elem:i64 × len]`
/// (payload after the [`HEADER_SIZE`] header). Maps to `typeof` "object".
const TYPE_ARRAY: i32 = abi::TYPE_ARRAY as i32;

/// Type byte for a heap object block:
/// `[type=2][pad×3][n:u32][(key_ptr:i64, val:i64) × n]` (payload after the
/// [`HEADER_SIZE`] header). Maps to `typeof` "object".
const TYPE_OBJECT: i32 = abi::TYPE_OBJECT as i32;

/// Byte offset of the object-family prefix's `proto` jsval slot (see
/// [`abi::OBJ_PROTO_OFFSET`]); every non-`TYPE_OBJECT`/`TYPE_ARRAY`
/// object-family block is created with it `null`.
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

/// Field offsets and size of a `Promise` block (see [`abi::TYPE_PROMISE`]).
const PROMISE_STATE_OFFSET: u64 = abi::PROMISE_STATE_OFFSET as u64;
const PROMISE_VALUE_OFFSET: u64 = abi::PROMISE_VALUE_OFFSET as u64;
const PROMISE_BLOCK_SIZE: i32 = abi::PROMISE_BLOCK_SIZE.cast_signed();

/// Type byte, payload offset, and size of a `Date` block. Maps to `typeof`
/// "object". See [`abi::TYPE_DATE`].
const TYPE_DATE: i32 = abi::TYPE_DATE as i32;
const DATE_MS_OFFSET: u64 = abi::DATE_MS_OFFSET as u64;
const DATE_BLOCK_SIZE: i32 = abi::DATE_BLOCK_SIZE.cast_signed();

/// The TYPE BYTE of an instance-accessor block. See [`abi::TYPE_ACCESSOR`].
const TYPE_ACCESSOR: i32 = abi::TYPE_ACCESSOR as i32;
const ACCESSOR_GET_OFFSET: u64 = abi::ACCESSOR_GET_OFFSET as u64;
const ACCESSOR_SET_OFFSET: u64 = abi::ACCESSOR_SET_OFFSET as u64;

/// The byte size of a `TYPE_ACCESSOR` heap block: the [`HEADER_SIZE`] header, 4
/// padding bytes for 8-byte alignment, and the two 8-byte getter/setter jsval
/// slots.
const ACCESSOR_BLOCK_SIZE: i32 = HEADER_SIZE + 4 + 8 + 8;

/// The fixed byte length of a `toISOString()` result (`YYYY-MM-DDTHH:MM:SS.mmmZ`)
/// — always 24 for a 4-digit year, the in-scope range.
const ISO_STRING_LEN: i32 = 24;

/// The string a `Date` field method yields for a non-finite (`NaN`/`Infinity`)
/// timestamp: `toISOString()` returns this instead of an ISO string (interned as a
/// data-segment literal, like the `typeof` words).
const INVALID_DATE: &str = "Invalid Date";

/// The NaN-boxed `undefined` jsval as an `i64` carrier (see [`abi::UNDEFINED`]).
/// Stands in for the value of a bare `return;` and of a program with no
/// expression statement.
const UNDEFINED_BITS: i64 = abi::UNDEFINED.cast_signed();

/// The NaN-boxed `false` jsval (see [`abi::FALSE`]).
const FALSE_BITS: i64 = abi::FALSE.cast_signed();

/// The NaN-boxed `true` jsval (see [`abi::TRUE`]).
const TRUE_BITS: i64 = abi::TRUE.cast_signed();

/// The message thrown by the runtime callee-is-a-function guard in
/// [`emit_method_call_user`] when a method call's resolved member is NOT a
/// function value. This is the dynamic-shape complement of the compile-time
/// unknown-method diagnostics (static-primitive and static-object-shape receivers):
/// a `new`-instance, reassigned binding, computed key, or call-result receiver
/// whose member reads `undefined` (or a non-function) can only be caught at
/// runtime, so instead of dispatching that value as a bogus funcref index
/// (undefined behaviour — a wrong result or a cryptic wasm trap) the guard
/// raises a clean exception — a real `TypeError` object, so
/// `catch (e) { e instanceof TypeError }` holds — surfaced when uncaught as
/// `Uncaught TypeError: called value is not a function`. This is the
/// error's `.message` (no `TypeError:` prefix); the prefixed first line lives in
/// [`NOT_A_FUNCTION_STACK`]. The object's static strings are interned into the
/// data segment by [`collect_node`] at every method-call site.
const NOT_A_FUNCTION_MSG: &str = "called value is not a function";

/// The internal `TypeError`'s single-line `.stack` (its `name: message` first
/// line); see [`NOT_A_FUNCTION_MSG`] and [`internal_type_error`].
const NOT_A_FUNCTION_STACK: &str = "TypeError: called value is not a function";

/// The static strings the internal-`TypeError` object (see [`internal_type_error`])
/// embeds — its `.message` and `.stack`. They must be interned wherever the
/// runtime callee guard can fire so their `[len][utf8]` blobs exist in the data
/// segment; [`collect_node`] seeds them (with [`ERROR_STRS`]) at each guarded
/// call.
const INTERNAL_TYPE_ERROR_STRS: [&str; 2] = [NOT_A_FUNCTION_MSG, NOT_A_FUNCTION_STACK];

/// Build the `TypeError` the runtime callee guard throws, identical in shape
/// to `new TypeError("called value is not a function")`. Emitted at the guard
/// site via [`emit_node`]; its strings are seeded by [`INTERNAL_TYPE_ERROR_STRS`].
fn internal_type_error() -> IrNode {
    IrNode::error_object(
        ErrorKind::TypeError,
        None,
        Some(IrNode::Str(NOT_A_FUNCTION_MSG.to_owned())),
        IrNode::Str(NOT_A_FUNCTION_STACK.to_owned()),
    )
}

/// The static strings every error needs: the own keys of an Error instance and
/// the keys and values of the static error prototypes (see
/// [`emit_error_protos`]). Seeded by [`collect_node`] wherever an error can be
/// created or observed; the prototypes are emitted iff all are interned.
const ERROR_STRS: [&str; 7] = [
    "name",
    "message",
    "stack",
    "",
    "Error",
    "TypeError",
    "RangeError",
];

/// The byte size of one static error prototype block: the object-family
/// prefix, then its `TYPE_PROPS` table holding `name` and `message`.
const ERROR_PROTO_SIZE: i32 =
    (abi::OBJ_PREFIX_SIZE + abi::PROPS_HEADER_SIZE + 2 * abi::PROP_ENTRY_SIZE).cast_signed();

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
const FN_KEYS: [&str; 6] = [
    "name",
    "prototype",
    "constructor",
    "caller",
    "arguments",
    "",
];

/// The static strings `__nullish_throw` needs — the receiver words and the
/// message fragments. Seeded (with [`ERROR_STRS`]) by [`collect_node`] at every
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
const PROP_KEYED_MSGS: [(i32, &str, &str); 5] = [
    (
        PROP_THROW_ARRAY_KEY,
        "Cannot set named property '",
        "' on an array yet",
    ),
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
];
/// The `TypeError` message `__prop_throw` raises for an array index store at or
/// past the current length.
const ARRAY_GROW_MSG: &str = "Cannot grow array by index assignment yet";

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

/// `__prop_throw`'s `mode` argument: a named (non-index) property store on an
/// array (S2 owns array properties).
const PROP_THROW_ARRAY_KEY: i32 = 0;
/// `__prop_throw`'s `mode` argument: an array index store at/past the length.
const PROP_THROW_GROW: i32 = 1;
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

/// The static strings `__prop_throw` needs — the message fragments (plus every
/// [`PROP_KEYED_MSGS`] fragment). Seeded (with [`ERROR_STRS`]) by
/// [`collect_node`] at every member store and `delete` site so their
/// `[len][utf8]` blobs exist when it throws.
const PROP_ERROR_STRS: [&str; 9] = [
    ARRAY_GROW_MSG,
    TYPE_ERROR_STACK_PREFIX,
    PRIM_CREATE_PREFIX,
    PRIM_CREATE_MID,
    PRIM_CREATE_QUOTE,
    PRIM_CREATE_SUFFIX,
    "number",
    "string",
    "boolean",
];

/// The `JSON.parse` syntax-error `.name`, `.message`, and `.stack` text. jsc has
/// no `%SyntaxError.prototype%` yet, so the thrown object is an `Error` (`e
/// instanceof Error` holds) with an own `.name` of `SyntaxError`, rendering as
/// `SyntaxError: …`. A single generic message covers every malformed /
/// unsupported-input case (a `\u` escape is out of v1 scope; see `json_parse`).
const JSON_ERROR_NAME: &str = "SyntaxError";
/// See [`JSON_ERROR_NAME`].
const JSON_ERROR_MSG: &str = "JSON.parse: invalid or unsupported JSON input";
/// See [`JSON_ERROR_NAME`].
const JSON_ERROR_STACK: &str = "SyntaxError: JSON.parse: invalid or unsupported JSON input";

/// The static strings the `JSON.parse` syntax-error object embeds, interned
/// (with [`ERROR_STRS`]) by [`collect_node`] wherever a `JsonParse` node exists
/// so their `[len][utf8]` blobs are in the data segment when `__json_throw`
/// builds the object.
const JSON_PARSE_ERROR_STRS: [&str; 3] = [JSON_ERROR_NAME, JSON_ERROR_MSG, JSON_ERROR_STACK];

/// Build the syntax-error object `__json_throw` raises on malformed JSON, the
/// same shape as [`internal_type_error`]. Emitted via [`emit_node`]; its
/// strings are seeded by [`JSON_PARSE_ERROR_STRS`].
fn json_syntax_error() -> IrNode {
    IrNode::error_object(
        ErrorKind::Error,
        Some(JSON_ERROR_NAME),
        Some(IrNode::Str(JSON_ERROR_MSG.to_owned())),
        IrNode::Str(JSON_ERROR_STACK.to_owned()),
    )
}

/// The NaN-boxed `null` jsval (see [`abi::NULL`]).
const NULL_BITS: i64 = abi::NULL.cast_signed();

/// The literal token strings the in-wasm `JSON.stringify` serializer concatenates:
/// the three literal keywords plus the structural punctuation. They are
/// interned like any static string (seeded into the string table when a program
/// contains a `JsonStringify` node, see `collect_node`) so the `__json_str` helper
/// can splice their `[len][utf8]` blocks with `__concat`. The layout mirrors
/// [`JsonTokens`], which resolves each to its compile-time byte offset.
const JSON_TOKEN_STRS: [&str; 9] = ["null", "true", "false", "[", "]", "{", "}", ",", ":"];

/// The compile-time byte offsets of the [`JSON_TOKEN_STRS`] blocks in linear
/// memory, baked into the `__json_str` serializer so it can splice each
/// token with `__concat`. Resolved from the string-offset map; a missing token
/// (only possible when the program has no `JsonStringify` node, so `__json_str` is
/// dead code) falls back to [`HEAP_BASE`] to keep the emitted helper valid.
#[derive(Clone, Copy)]
struct JsonTokens {
    null: i32,
    true_ptr: i32,
    false_ptr: i32,
    lbrack: i32,
    rbrack: i32,
    lbrace: i32,
    rbrace: i32,
    comma: i32,
    colon: i32,
}

impl JsonTokens {
    fn resolve(offsets: &HashMap<String, i32>) -> Self {
        let at = |s: &str| offsets.get(s).copied().unwrap_or(HEAP_BASE);
        Self {
            null: at("null"),
            true_ptr: at("true"),
            false_ptr: at("false"),
            lbrack: at("["),
            rbrack: at("]"),
            lbrace: at("{"),
            rbrace: at("}"),
            comma: at(","),
            colon: at(":"),
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

/// A labeled loop's abrupt-exit landing blocks, keyed by the loop id an
/// [`IrStmt::Break`]/[`IrStmt::Continue`] carries. Depths are absolute (see
/// [`Unwind::depth`]); the `br` immediate at a use site is `depth - entry`.
struct LabelTarget {
    /// The loop id matching [`crate::ir::IrStmt::While`]'s `label`.
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
    /// The host `print` import's function index. Imports occupy the
    /// lowest function indices, so this is always `0`; `console.log` lowers to
    /// `call`s of it.
    print: u32,
    /// The host `__convert` import's function index. Always `1` (the
    /// second import); `String`/`Number`/`parseFloat` lower to `call`s of it.
    convert: u32,
    /// The host `__method` import's function index. Always `2` (the third
    /// import); the non-callback array/string methods lower to `call`s of it.
    method: u32,
    /// The host `__promise` import's function index. Always `3` (the fourth
    /// import); `Promise.resolve`/`reject`, `await`, and `p.finally` lower to
    /// `call`s of it.
    promise: u32,
    /// The host `__then` import's function index. Always `4` (the fifth
    /// import); `p.then`/`p.catch` lower to `call`s of it.
    then: u32,
    /// The host `__now` import's function index (backing `Date`). Always `5` (the
    /// sixth import); `Date.now()` / `new Date()` lower to `call`s of it. The
    /// import returns an f64 (ms since the Unix epoch).
    now: u32,
    /// The host `__parse_int(s: i64, radix: i64) -> i64` import's function
    /// index. Always `6` (the seventh import); `parseInt` lowers to `call`s of it.
    parse_int: u32,
    /// `__gen_to_array(gen: i64) -> i64`: drive a generator to
    /// exhaustion via `call_indirect`, collecting its yielded values into a fresh
    /// array — the drain backing `[...gen]` / `f(...gen)` spread. Emitted just
    /// before `invoke1`; a stub (returns an empty array) when the program declares
    /// no functions (no generator can exist, so it is never called).
    gen_to_array: u32,
    /// `__nullish_throw(recv: i64, key: i64, set: i32)`: build the V8-style
    /// `TypeError` for a member read (`set == 0`) or write (`set == 1`) on a
    /// `null`/`undefined` receiver and leave it in flight (`$exc_pending` set).
    /// The out-of-line half of the nullish-receiver guard (see
    /// [`object::emit_nullish_guard`]).
    nullish_throw: u32,
    /// `__prop_throw(recv: i64, key: i64, mode: i32)`: build the `TypeError` for
    /// a member store the receiver cannot take — an absent property
    /// ([`PROP_THROW_ADD`], message names `String(key)`), an array index at/past
    /// the length ([`PROP_THROW_GROW`]), or any store on a primitive `recv`
    /// ([`PROP_THROW_PRIMITIVE`]) — and leave it in flight (`$exc_pending` set).
    /// The out-of-line half of [`object::emit_index_set_node`]'s miss paths.
    prop_throw: u32,
    /// `__box_f64(x: f64) -> i64`: rebox an `f64` as a number jsval, mapping
    /// every NaN to the canonical [`abi::QNAN`] bits (see [`value::emit_box_f64`]).
    box_f64: u32,
    /// `__to_number(v: i64) -> i64`: the JS `ToNumber` of any jsval (`ToPrimitive` of
    /// an array is its `join()` string; see [`value::emit_to_number_helper`]).
    /// Call sites skip it for number operands (see [`value::emit_coerce_number`]).
    to_number: u32,
    /// `__invoke1(fn: i64, arg: i64) -> i64`: the callback trampoline the
    /// host promise engine calls re-entrantly. Exported under
    /// [`INVOKE_EXPORT_NAME`].
    invoke1: u32,
    /// `__async_step(gen: i64, sent: i64) -> i64`: the generator
    /// single-step trampoline the host async engine calls to advance an
    /// `async function` body's state machine by one suspend. Appended right after
    /// `invoke1` (so it always exists, index `invoke1 + 1`, even when the program
    /// declares no functions — a stub then), exported under
    /// [`ASYNC_STEP_EXPORT_NAME`].
    async_step: u32,
    /// The funcref-TABLE slots of the two `new Promise` settler functions:
    /// `resolve` (fulfills the bound promise) and `reject`. A `new Promise`
    /// executor receives function values pointing at these slots with the target
    /// promise in their env slot. `0` when the program declares no functions (no
    /// table, and `new Promise` — needing an executor function — cannot occur).
    settle_resolve_slot: u32,
    settle_reject_slot: u32,
    /// `__new_string(len: i32) -> i32`: the exported string constructor. The
    /// LAST defined functions are it, `__new_array` (`new_string + 1`), and
    /// `__new_promise` (`new_string + 2`), appended after the settlers so every
    /// other index stays stable.
    new_string: u32,
    /// `__alloc(size) -> i32`: bump-allocate `size` bytes, returning the offset.
    /// Called inline by array literals.
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
    /// `__index_string(n) -> s`: a fresh string of the unsigned `n`'s digits.
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
    /// `__rest(src, excl) -> object`: the object-rest copy.
    rest: u32,
    /// `__array_push(base, value) -> new_base`: bump-allocate a new array
    /// block of `len + 1` element capacity, copy the header + existing elements,
    /// append `value`, bump the u32 length header, and return the new block base.
    array_push: u32,
    /// `__array_concat(dst, src) -> new_base`: bump-allocate a new array
    /// block holding every element of `dst` followed by every element of `src`,
    /// and return the new block base. Used to append a spread source's elements
    /// when building a spread array literal or spread call argument list.
    array_concat: u32,
    /// `__json_escape(str_ptr) -> str_base`: bump-allocate a fresh heap
    /// string that is the JSON-quoted + escaped form of the source string (leading
    /// and trailing `"`, with `"`/`\`/control bytes escaped). Called by
    /// [`Self::json_str`] for string values and object keys.
    json_escape: u32,
    /// `__json_str(v) -> str_base`: the recursive `JSON.stringify` serializer
    /// — returns the heap-string base offset of `v`'s JSON form, composing pieces
    /// with `__concat`, delegating string escaping to [`Self::json_escape`] and
    /// finite-number formatting to the existing `__convert` import, and calling
    /// itself for nested array/object elements. Backs [`crate::ir::IrNode::JsonStringify`].
    json_str: u32,
    /// `__same_value_zero(a: i64, b: i64) -> i32`: `SameValueZero` over two
    /// jsvals (two heap strings compare by byte content, two numbers as `f64` or
    /// identical bits — so `NaN` matches `NaN` and `+0` matches `-0` — everything
    /// else by identity bits). The key equality of the `__map_*` linear scans.
    same_value_zero: u32,
    /// `__map_find(handle: i32, key: i64) -> i32`: the byte address of the
    /// matching key slot in a Map/Set's backing array (a `__same_value_zero` linear scan over
    /// the even element slots), or `-1` on a miss. Shared by get/has/set/delete.
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
    /// `__map_delete(handle: i32, key: i64) -> i32`: compact the matching
    /// entry out of the backing (shift trailing pairs down, drop `size`/`len` by
    /// one entry) and return `1`, or `0` on a miss.
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
    /// The wasm type index of the uniform user-function signature
    /// `(env, a0..a_{K-1}) -> i64`, used as the `call_indirect` type. `0` when
    /// the program declares no functions (no indirect call can occur).
    fn_type: u32,
    /// The fixed positional-argument arity `K` of the uniform ABI: how many
    /// argument slots every call site pushes (padding with `undefined` or
    /// truncating extras). `0` when there are no functions.
    max_args: u32,
    /// Whether the program declares any function (so the funcref table and the
    /// uniform `call_indirect` type exist). When `false`, a `gen.next()` receiver
    /// provably cannot be a generator — [`generator::emit_gen_resume`] then throws
    /// a `TypeError` unconditionally instead of emitting an invalid indirect call.
    has_funcs: bool,
    /// Whether the program builds any accessor ([`IrNode::MakeAccessor`]).
    /// When `false`, `__get` / `__set` omit the getter/setter invocation and
    /// member accesses skip the post-call exception check. Implies `has_funcs`.
    has_accessors: bool,
    /// Base address of the `__fn_info` table (see [`FnLayout`]). The static
    /// declaration blocks follow it ([`decl_func_base`]). `__fn_materialize`
    /// is the helper right after `__define` (`define + 1`).
    fn_info: i32,
}

/// The heap-pointer jsval of `kind`'s static prototype block (see
/// [`emit_error_protos`]). The prototypes directly follow the function data
/// ([`FnLayout::errors`]); every site referencing one seeds [`ERROR_STRS`],
/// so they are present.
fn error_proto_bits(kind: ErrorKind, helpers: Helpers<'_>) -> i64 {
    let base = if helpers.has_funcs {
        decl_func_base(helpers.settle_resolve_slot, helpers)
    } else {
        helpers.fn_info
    };
    let index = ErrorKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
    let addr = base + ERROR_PROTO_SIZE * i32::try_from(index).unwrap_or(0);
    HEAP_BITS | i64::from(addr)
}

/// The address of the static function block of top-level declaration
/// `table_index`: the blocks follow the `__fn_info` table's entries for the
/// user functions and the two settlers (`settle_resolve_slot` is the user
/// function count).
fn decl_func_base(table_index: u32, helpers: Helpers<'_>) -> i32 {
    let info_entries = helpers.settle_resolve_slot.cast_signed() + 2;
    helpers.fn_info
        + abi::FN_INFO_ENTRY_SIZE.cast_signed() * info_entries
        + abi::FUNC_BLOCK_SIZE.cast_signed() * table_index.cast_signed()
}

/// Where the function data lives in the data segment: the `__fn_info` table
/// (one [`abi::FN_INFO_ENTRY_SIZE`]-byte entry per funcref-table index,
/// settlers included) and,
/// after it, one static [`abi::FUNC_BLOCK_SIZE`]-byte function block per index
/// (the object of a top-level declaration, [`IrNode::DeclFunc`]).
#[derive(Clone, Copy)]
struct FnLayout {
    /// Base address of the info table.
    info: i32,
    /// Base address of the static function blocks.
    blocks: i32,
    /// Base address of the static error prototypes, [`ERROR_PROTO_SIZE`] bytes
    /// each in [`ErrorKind::ALL`] order, or `0` when absent.
    errors: i32,
}

mod array;
mod expr;
mod function;
mod generator;
mod json_parse;
mod numeric;
mod object;
mod props;
mod runtime;
mod sections;
mod stmt;
#[cfg(test)]
mod tests;
mod value;

#[allow(clippy::wildcard_imports)]
use array::*;
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
#[allow(clippy::wildcard_imports)]
use runtime::*;
#[allow(clippy::wildcard_imports)]
use sections::*;
#[allow(clippy::wildcard_imports)]
use stmt::*;
#[allow(clippy::wildcard_imports)]
use value::*;

/// Emit a complete WebAssembly module for `program`.
///
/// The returned bytes are a self-contained module exporting a single function
/// `main` with signature `() -> i64`. It declares one `i64` local per program
/// variable, executes each statement in order, and leaves the value of the last
/// expression statement on the stack as the result. Values of any earlier
/// expression statements are dropped. A program with no expression statement
/// (only declarations) has no value, so the `undefined` jsval is pushed as the
/// result to keep the `() -> i64` signature total.
#[allow(clippy::too_many_lines)]
pub fn emit(program: &IrProgram) -> Vec<u8> {
    let mut module = Module::new();

    // Collect every distinct string literal and assign each a compile-time byte
    // offset in linear memory. `heap_ptr_init` is the first free offset after all
    // literal blobs — where the bump allocator begins.
    let (offsets, strings_end) = collect_strings(program);
    // The function info table and the static declaration blocks follow the
    // string blobs, then the intern table; the bump allocator begins after it.
    // (Info entries cover the two `new Promise` settlers, present with functions.)
    let n = program.functions.len();
    let fn_count = i32::try_from(n).unwrap_or(i32::MAX);
    let info_count = if n > 0 { fn_count + 2 } else { 0 };
    // The static error prototypes follow the function blocks, only when every
    // string they and the errors need is interned (seeded wherever an error
    // can be created or observed).
    let blocks = strings_end + abi::FN_INFO_ENTRY_SIZE.cast_signed() * info_count;
    let blocks_end = blocks + abi::FUNC_BLOCK_SIZE.cast_signed() * fn_count;
    let has_errors = ERROR_STRS.iter().all(|s| offsets.contains_key(*s));
    let layout = FnLayout {
        info: strings_end,
        blocks,
        errors: if has_errors { blocks_end } else { 0 },
    };
    let intern_base = if has_errors {
        blocks_end + ERROR_PROTO_SIZE * i32::try_from(ErrorKind::ALL.len()).unwrap_or(0)
    } else {
        blocks_end
    };
    let intern = intern_table(&offsets, intern_base);
    let heap_ptr_init = intern_base + 4 * intern.cap;

    // Type/function sections and the resolved helper indices (see
    // [`build_type_and_function_sections`]).
    let has_funcs = n > 0;
    // Per-function exception-unwinding state, reset at each function body.
    // Owned here so it outlives every `emit_main`/`emit_function` call below.
    let unwind = RefCell::new(Unwind::default());
    let (
        types,
        functions,
        helpers,
        print_type,
        convert_type,
        method_type,
        promise_type,
        then_type,
        now_type,
        parse_int_type,
    ) = build_type_and_function_sections(program, &unwind, layout);
    module.section(&types);

    // Import section: the host `print` function backing `console.log` (index 0),
    // the host `__convert` function backing the conversions (index 1), and the
    // host `__method` function backing the array/string methods (index 2).
    // Imports occupy the lowest function indices, so every defined
    // function/element/export index is shifted by IMPORT_FUNC_COUNT (accounted for
    // in `build_type_and_function_sections` and below). This follows the type
    // section and precedes the function section (wasm section order); the order of
    // the imports here must match their assigned indices in `Helpers`.
    let mut imports = ImportSection::new();
    imports.import(
        PRINT_IMPORT_MODULE,
        PRINT_IMPORT_NAME,
        EntityType::Function(print_type),
    );
    imports.import(
        CONVERT_IMPORT_MODULE,
        CONVERT_IMPORT_NAME,
        EntityType::Function(convert_type),
    );
    imports.import(
        METHOD_IMPORT_MODULE,
        METHOD_IMPORT_NAME,
        EntityType::Function(method_type),
    );
    // The host promise imports (indices 3/4): `__promise(op, a, b) -> i64`
    // and `__then(p, on_f, on_r) -> i64`, backing the host promise engine.
    imports.import(
        PROMISE_IMPORT_MODULE,
        PROMISE_IMPORT_NAME,
        EntityType::Function(promise_type),
    );
    imports.import(
        THEN_IMPORT_MODULE,
        THEN_IMPORT_NAME,
        EntityType::Function(then_type),
    );
    // The host wall-clock import (backing `Date`, index 5): `__now() -> f64`,
    // milliseconds since the Unix epoch, backing `Date.now()` / `new Date()`.
    imports.import(
        NOW_IMPORT_MODULE,
        NOW_IMPORT_NAME,
        EntityType::Function(now_type),
    );
    // The host `parseInt` import (index 6): `__parse_int(s, radix) -> i64`.
    imports.import(
        PARSE_INT_IMPORT_MODULE,
        PARSE_INT_IMPORT_NAME,
        EntityType::Function(parse_int_type),
    );
    module.section(&imports);

    module.section(&functions);

    // Table section: a funcref table listing every user function so a
    // function value can be called through `call_indirect`. Table slot `i` holds
    // wasm function index `i + 1`; the element segment (below, after exports)
    // populates it. Omitted entirely when the program declares no functions.
    if has_funcs {
        // `n` user-function slots plus the two `new Promise` settler slots,
        // which occupy slots `n` and `n + 1`.
        let table_len = (n + 2) as u64;
        let mut tables = TableSection::new();
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table_len,
            maximum: Some(table_len),
            shared: false,
        });
        module.section(&tables);
    }

    // Memory section: one linear memory (min 1 page, capped at
    // `abi::MEMORY_MAX_PAGES`; `__alloc` grows it on demand), exported below.
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: Some(abi::MEMORY_MAX_PAGES),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memories);

    // Global section: the bump-allocator heap pointer plus the
    // exception-unwinding globals (see [`build_globals`]).
    module.section(&build_globals(heap_ptr_init, &intern, program.module_cells));

    // Export section: `main` as function index IMPORT_FUNC_COUNT (the first
    // DEFINED function, after the imports), memory index 0 as "memory" (function
    // imports do not shift the single memory's index).
    let mut exports = ExportSection::new();
    exports.export(EXPORT_NAME, ExportKind::Func, IMPORT_FUNC_COUNT);
    exports.export(MEMORY_EXPORT_NAME, ExportKind::Memory, 0);
    // The bump-allocator heap pointer global is exported so the run stage can
    // detect the out-of-memory sentinel.
    exports.export(HEAP_PTR_EXPORT_NAME, ExportKind::Global, HEAP_PTR_GLOBAL);
    // The exception-unwinding globals are exported so the run stage can
    // detect and render an uncaught exception after `main` returns.
    exports.export(
        EXC_PENDING_EXPORT_NAME,
        ExportKind::Global,
        EXC_PENDING_GLOBAL,
    );
    exports.export(EXC_VALUE_EXPORT_NAME, ExportKind::Global, EXC_VALUE_GLOBAL);
    // The `__invoke1` callback trampoline is exported so the host promise
    // engine can run a `.then`/`.catch`/`.finally` reaction's guest callback
    // re-entrantly. It is the LAST defined function (appended after every helper).
    exports.export(INVOKE_EXPORT_NAME, ExportKind::Func, helpers.invoke1);
    // The `__async_step` trampoline is exported so the host async engine
    // can drive an `async function` body's state machine one suspend at a time.
    exports.export(ASYNC_STEP_EXPORT_NAME, ExportKind::Func, helpers.async_step);
    // The allocator and the guest constructors: the hosts allocate every
    // string/array/promise block through these, never by hand.
    exports.export(ALLOC_EXPORT_NAME, ExportKind::Func, helpers.alloc);
    exports.export(NEW_STRING_EXPORT_NAME, ExportKind::Func, helpers.new_string);
    exports.export(
        NEW_ARRAY_EXPORT_NAME,
        ExportKind::Func,
        helpers.new_string + 1,
    );
    exports.export(
        NEW_PROMISE_EXPORT_NAME,
        ExportKind::Func,
        helpers.new_string + 2,
    );
    module.section(&exports);

    // Element section: an active segment filling the funcref table at offset 0
    // with wasm function indices `1..=n` (the user functions), so table slot `i`
    // resolves to function `i + 1` for `call_indirect`. Emitted between the
    // export and code sections (the wasm section order), only when there are
    // functions to reference.
    if has_funcs {
        // Table slot `i` holds DEFINED function index `i + 1 + IMPORT_FUNC_COUNT`
        // (the user functions, shifted past `main` and the imports). The funcref
        // table's slot indices themselves are unaffected by imports, so a
        // `call_indirect` table index needs no adjustment.
        let mut func_indices: Vec<u32> = (1..=u32::try_from(n).unwrap_or(u32::MAX))
            .map(|i| i + IMPORT_FUNC_COUNT)
            .collect();
        // The two `new Promise` settler functions fill the trailing table
        // slots `n`/`n + 1`; they are the last two DEFINED functions, right after
        // the `__invoke1` and `__async_step` trampolines.
        func_indices.push(helpers.async_step + 1);
        func_indices.push(helpers.async_step + 2);
        let mut elements = ElementSection::new();
        elements.active(
            None,
            &ConstExpr::i32_const(0),
            Elements::Functions(Cow::Owned(func_indices)),
        );
        module.section(&elements);
    }

    // Code section: `main` first, then each user function's body, then the
    // helpers last (mirroring the function-index order above).
    let mut code = CodeSection::new();
    code.function(&emit_main(program, &offsets, helpers));
    for func in &program.functions {
        // A generator state machine emits a `switch (gen.state)` dispatch
        // over its segments rather than a straight-line body.
        if func.machine.is_some() {
            code.function(&generator::emit_generator_sm(func, &offsets, helpers));
        } else {
            code.function(&emit_function(func, &offsets, helpers));
        }
    }
    code.function(&emit_alloc());
    code.function(&emit_concat(helpers.alloc));
    code.function(&emit_str_eq());
    code.function(&emit_str_cmp());
    code.function(&emit_str_index(helpers.alloc));
    code.function(&emit_str_hash());
    code.function(&emit_intern_slot(helpers));
    code.function(&emit_intern_find(helpers));
    code.function(&emit_intern(helpers));
    code.function(&emit_index_value());
    code.function(&emit_index_string(helpers));
    code.function(&emit_to_key(helpers));
    code.function(&emit_prop_find(helpers));
    code.function(&emit_define(helpers));
    code.function(&emit_fn_materialize(&offsets, helpers));
    code.function(&emit_get(&offsets, helpers));
    code.function(&emit_set(helpers));
    code.function(&emit_has(&offsets, helpers));
    code.function(&emit_delete(helpers));
    code.function(&emit_own_keys(helpers));
    code.function(&emit_for_in_keys(helpers));
    code.function(&emit_rest(helpers));
    code.function(&emit_array_push(helpers.alloc));
    code.function(&emit_array_concat(helpers.alloc));
    code.function(&emit_json_escape(helpers.alloc));
    code.function(&emit_json_str(
        helpers.json_str,
        helpers.concat,
        helpers.convert,
        helpers.json_escape,
        (helpers.own_keys, helpers.get),
        JsonTokens::resolve(&offsets),
    ));
    code.function(&emit_same_value_zero(helpers.str_eq));
    code.function(&emit_map_find(helpers.same_value_zero));
    code.function(&emit_map_set(helpers.map_find, helpers.array_push));
    code.function(&emit_map_get(helpers.map_find));
    code.function(&emit_map_has(helpers.map_find));
    code.function(&emit_map_delete(helpers.map_find));
    // `__loose_eq` (loose `==`) and `__str_to_array` (string spread) follow the
    // Map/Set helpers.
    code.function(&emit_loose_eq(helpers.str_eq, helpers.convert));
    code.function(&emit_str_to_array(helpers.alloc, helpers.str_index));
    // The in-wasm `JSON.parse` family, in the order their indices were assigned.
    code.function(&emit_json_ws());
    code.function(&emit_json_throw(&offsets, helpers));
    code.function(&emit_json_string(helpers.alloc, helpers));
    code.function(&emit_json_number(helpers.alloc, helpers.convert, helpers));
    code.function(&emit_json_value(helpers));
    code.function(&emit_json_array(helpers.alloc, helpers));
    code.function(&emit_json_object(helpers.alloc, helpers));
    code.function(&emit_json_parse(helpers));
    // `__gen_to_array` — the generator-spread drain — precedes the
    // trampoline so every later index stays stable.
    code.function(&generator::emit_gen_to_array(helpers));
    code.function(&emit_nullish_throw(&offsets, helpers));
    code.function(&emit_prop_throw(&offsets, helpers));
    // The `__invoke1` callback trampoline is appended after every helper so
    // every helper index stays stable; the `__async_step` trampoline
    // follows it (always, so its index is deterministic), then the two
    // `new Promise` settler functions (only when there are functions — the table
    // exists then).
    code.function(&emit_box_f64_helper());
    code.function(&emit_to_number_helper(helpers));
    code.function(&emit_invoke1(helpers));
    code.function(&generator::emit_async_step(helpers));
    if has_funcs {
        code.function(&emit_settler(PROMISE_OP_SETTLE_FULFILL, helpers));
        code.function(&emit_settler(PROMISE_OP_SETTLE_REJECT, helpers));
    }
    code.function(&emit_new_string(helpers.alloc));
    code.function(&emit_new_array(helpers.alloc));
    code.function(&emit_new_promise(helpers.alloc));
    module.section(&code);

    // Data section (after code): the string-literal blobs and the intern table.
    module.section(&emit_data_section(
        &offsets,
        &intern,
        &program.functions,
        layout,
    ));

    module.finish()
}
