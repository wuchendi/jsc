//! Run: execute an emitted WebAssembly module with `wasmtime`.
//!
//! This is the final stage of the pipeline. It loads the module bytes produced
//! by [`crate::emit::emit`] into a [`wasmtime`] engine, instantiates it with no
//! imports, and calls the exported `main` function (signature `() -> i64`),
//! returning the resulting jsval bits as a [`u64`].
//!
//! [`compile_and_run`] wires the whole compiler together: JavaScript source text
//! in, a decoded [`Value`] out.
//!
//! The module is split by concern: [`harness`] runs the emitted module and wires
//! the host imports, [`host`] implements those import handlers, and [`value`]
//! decodes and formats jsvals.
//!
//! # jsval NaN-box layout
//!
//! A jsval is 64 bits carried through wasm as an `i64` (so the bits survive
//! every op and the wasmtime return boundary without NaN canonicalization).
//! Numbers are stored as raw `f64` bits; every other in-scope value sets the
//! quiet-NaN mask plus a 3-bit tag in payload bits `[50:48]`:
//!
//! ```text
//! QNAN mask = 0x7FF8_0000_0000_0000  (exponent all ones + quiet bit)
//! NUMBER    iff (bits & QNAN) != QNAN, OR bits == QNAN (canonical number NaN)
//! tag 0b000 -> the number NaN (canonical NaN 0x7FF8_0000_0000_0000)
//! tag 0b001 -> boolean (value in bit 0):  FALSE=0x7FF9.., TRUE=0x7FF9..1
//! tag 0b010 -> null       = 0x7FFA_0000_0000_0000
//! tag 0b011 -> undefined  = 0x7FFB_0000_0000_0000
//! tag 0b100 -> unused (`typeof` yields a real interned heap string)
//! tag 0b101 -> heap pointer: low 32 bits are the byte offset of a heap BLOCK
//!              base. This is the SINGLE heap tag for every heap kind; the
//!              concrete kind is a TYPE BYTE at offset 0 of the block (tags
//!              0b110/0b111 are unused). See the block-header layout below.
//! ```
//!
//! # Heap block header
//!
//! Every heap block begins with a `HEADER_SIZE`-byte header: byte `0` is the TYPE
//! BYTE (0 string, 1 array, 2 object, 3 function), bytes `1..4` are padding, and
//! the per-type payload begins at offset
//! `HEADER_SIZE`:
//!
//! ```text
//! type 0 string : [type][pad×3][len:u32 LE][utf8]
//! type 1 array  : [type][pad×3][len:u32 LE][elem:i64 × len]
//! type 2 object : [type][pad×3][n:u32 LE][(key_ptr:i64, val:i64) × n]
//! ```
//!
//! The immediate types (number/boolean/null/undefined/typeof-string) need no
//! memory. A heap pointer resolves against the module's exported `"memory"`;
//! [`run`] snapshots those bytes so [`decode`] can read them.

mod harness;
mod host;
mod promise;
mod value;

#[cfg(test)]
mod tests;

pub use harness::{compile_and_run, compile_and_run_capturing, run, run_capturing, run_metered};
pub use value::{Value, decode, format_number, format_value};

use crate::abi;

/// The quiet-NaN mask (see [`abi::QNAN`]): exponent bits all ones plus the quiet
/// bit set.
const QNAN: u64 = abi::QNAN;

/// The NaN-boxed heap-pointer header (single heap tag `0b101`, see [`abi::HEAP`]):
/// a heap jsval is `HEAP | (ptr as u32)`. Used by the `__convert` host import
/// to box a freshly allocated heap string.
const HEAP: u64 = abi::HEAP;

/// The size in bytes of every heap block's leading header (see
/// [`abi::HEADER_SIZE`]): the TYPE BYTE at offset 0 plus 3 padding bytes. The
/// per-type payload begins here.
const HEADER_SIZE: usize = abi::HEADER_SIZE as usize;

/// Heap block TYPE BYTE values (offset 0 of the block); see [`abi`].
const TYPE_STRING: u8 = abi::TYPE_STRING;
const TYPE_ARRAY: u8 = abi::TYPE_ARRAY;
const TYPE_OBJECT: u8 = abi::TYPE_OBJECT;
const TYPE_FUNCTION: u8 = abi::TYPE_FUNCTION;
const TYPE_MAP: u8 = abi::TYPE_MAP;
const TYPE_SET: u8 = abi::TYPE_SET;
const TYPE_PROMISE: u8 = abi::TYPE_PROMISE;
const TYPE_GENERATOR: u8 = abi::TYPE_GENERATOR;
const TYPE_DATE: u8 = abi::TYPE_DATE;

/// Byte offset of a `Date` block's f64 `ms` payload; see [`abi`].
const DATE_MS_OFFSET: usize = abi::DATE_MS_OFFSET as usize;

/// Promise `state` header values; see [`abi`].
const PROMISE_PENDING: i32 = abi::PROMISE_PENDING;
const PROMISE_FULFILLED: i32 = abi::PROMISE_FULFILLED;
const PROMISE_REJECTED: i32 = abi::PROMISE_REJECTED;

/// Reserved hidden key marking a branded Error object's kind; see
/// [`abi::ERROR_KIND_KEY`]. The renderer uses its presence to print an error as
/// its single-line stack rather than a plain `{ ... }` object.
const ERROR_KIND_KEY: &str = abi::ERROR_KIND_KEY;
