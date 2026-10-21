//! Host-import handlers backing the `__convert`/`__method` env imports. Everything
//! here runs host-side against a `wasmtime::Caller`, decoding the guest's linear
//! memory and (for the methods that build a fresh value) allocating a new heap
//! block through the guest's exported constructors.

use wasmtime::{AsContextMut, Caller, Global};

use crate::abi;

use super::value::{array_elements, array_layout};
use super::{HEADER_SIZE, HEAP, QNAN, TYPE_ARRAY, TYPE_STRING, Value, decode, to_js_string};

/// The `__method` op flags (see [`abi`]): the non-callback array/string
/// methods, dispatched host-side. `IndexOf`/`Slice` exist on both arrays and
/// strings, so the handler branches on the receiver's runtime TYPE BYTE.
const METHOD_OP_INDEX_OF: i32 = abi::METHOD_OP_INDEX_OF;
const METHOD_OP_INCLUDES: i32 = abi::METHOD_OP_INCLUDES;
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

/// The NaN-boxed `true`/`false` jsval bits; see [`abi::TRUE`] / [`abi::FALSE`].
const TRUE_BITS: u64 = abi::TRUE;
const FALSE_BITS: u64 = abi::FALSE;

/// The out-of-memory heap-pointer sentinel; see [`abi::HEAP_PTR_OOM`].
const HEAP_PTR_OOM: i32 = abi::HEAP_PTR_OOM;

/// The linear-memory cap in bytes. A constructor request at or past it can
/// only run out of memory, so lengths are clamped to it before the call (which
/// keeps the guest's `i32` size arithmetic from wrapping).
// 512 MiB fits `usize` on every supported target.
#[allow(clippy::cast_possible_truncation)]
const MEMORY_MAX_BYTES: usize = abi::MEMORY_MAX_PAGES as usize * 0x1_0000;

/// Call the guest constructor export `name(len) -> base` (`__new_string` /
/// `__new_array`) and return the new block's base offset, or `None` when the
/// export is missing or the call traps — an out-of-memory trap leaves the
/// [`abi::HEAP_PTR_OOM`] sentinel for the run stage to report.
fn guest_new<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    name: &str,
    len: usize,
) -> Option<usize> {
    let ctor = caller
        .get_export(name)
        .and_then(wasmtime::Extern::into_func)?
        .typed::<i32, i32>(&*caller)
        .ok()?;
    let len = i32::try_from(len).ok()?;
    let base = ctor.call(&mut *caller, len).ok()?;
    usize::try_from(base).ok()
}

/// Allocate a heap STRING block holding `text` through the guest
/// `__new_string` export and return its base offset, or `None` if the export is
/// missing or the allocation/write fails. The guest writes the header; the host
/// only fills the UTF-8 payload.
pub(super) fn alloc_string<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    text: &str,
) -> Option<u64> {
    let len = text.len().min(MEMORY_MAX_BYTES);
    let base = guest_new(caller, abi::NEW_STRING_EXPORT_NAME, len)?;
    let mem = caller
        .get_export(abi::MEMORY_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_memory)?;
    mem.write(&mut *caller, base + HEADER_SIZE + 4, text.as_bytes())
        .ok()?;
    Some(base as u64)
}

/// Allocate a heap ARRAY holding the jsval bits `elems` through the guest
/// `__new_array` export and return its handle base offset, or `None` if the
/// export is missing or the allocation/write fails. The guest builds the handle
/// and its element store; the host only fills the elements.
fn alloc_array<W: Send + 'static>(caller: &mut Caller<'_, W>, elems: &[u64]) -> Option<u64> {
    let len = elems.len().min(MEMORY_MAX_BYTES / 8);
    let base = guest_new(caller, abi::NEW_ARRAY_EXPORT_NAME, len)?;
    let mem = caller
        .get_export(abi::MEMORY_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_memory)?;
    let (_, data) = array_layout(mem.data(&*caller), base)?;
    let bytes: Vec<u8> = elems[..len]
        .iter()
        .flat_map(|bits| bits.to_le_bytes())
        .collect();
    mem.write(&mut *caller, data, &bytes).ok()?;
    Some(base as u64)
}

/// Whether the heap-pointer global holds the out-of-memory sentinel: some guest
/// or host allocation could not grow linear memory.
pub(super) fn heap_exhausted(mut store: impl AsContextMut, global: Global) -> bool {
    global.get(&mut store).i32() == Some(HEAP_PTR_OOM)
}

/// Dispatch a `__method` host-import call: snapshot linear memory, read
/// the receiver's raw jsval bits, and perform the requested array/string method,
/// returning the result jsval bits (or `None` to yield `undefined` — a
/// type-mismatched receiver or a failed allocation).
///
/// `IndexOf`/`Slice` are overloaded on the receiver's runtime TYPE BYTE (array
/// element vs string byte semantics); the other ops accept a single receiver
/// kind. String byte offsets are used throughout (the ASCII subset), so multibyte
/// UTF-8 receivers are a documented limitation.
pub(super) fn method_dispatch<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    recv: u64,
    op: i32,
    arg0: i64,
    arg1: i64,
) -> Option<u64> {
    // Snapshot memory once so every decode/raw read below is a plain slice lookup;
    // any allocation happens afterwards through the guest constructors (which
    // re-borrow the memory mutably).
    let mem: Vec<u8> = match caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
    {
        Some(m) => m.data(&*caller).to_vec(),
        None => Vec::new(),
    };
    let ptr = heap_ptr(recv);
    let type_byte = ptr.and_then(|p| mem.get(p).copied());
    match op {
        METHOD_OP_INDEX_OF => match type_byte {
            Some(TYPE_ARRAY) => Some(array_index_of(&mem, ptr?, arg0.cast_unsigned())),
            Some(TYPE_STRING) => Some(string_index_of(&mem, ptr?, arg0.cast_unsigned())),
            _ => None,
        },
        METHOD_OP_INCLUDES => match type_byte {
            Some(TYPE_ARRAY) => Some(bool_bits(array_includes(&mem, ptr?, arg0.cast_unsigned()))),
            // `str.includes(sub)` — substring containment (an empty `sub` => true).
            Some(TYPE_STRING) => {
                let hay = string_bytes(&mem, ptr?)?;
                let needle = arg_bytes(arg0, &mem);
                Some(bool_bits(byte_contains(&hay, &needle)))
            }
            _ => None,
        },
        METHOD_OP_SLICE => match type_byte {
            Some(TYPE_ARRAY) => array_slice(caller, &mem, ptr?, arg0, arg1),
            Some(TYPE_STRING) => string_slice(caller, &mem, ptr?, arg0, arg1),
            _ => None,
        },
        METHOD_OP_TO_UPPER | METHOD_OP_TO_LOWER => match type_byte {
            Some(TYPE_STRING) => {
                let bytes = string_bytes(&mem, ptr?)?;
                let mapped: Vec<u8> = bytes
                    .iter()
                    .map(|b| {
                        if op == METHOD_OP_TO_UPPER {
                            b.to_ascii_uppercase()
                        } else {
                            b.to_ascii_lowercase()
                        }
                    })
                    .collect();
                let text = String::from_utf8_lossy(&mapped);
                alloc_string(caller, &text).map(|base| HEAP | base)
            }
            _ => None,
        },
        METHOD_OP_CHAR_AT => match type_byte {
            Some(TYPE_STRING) => {
                let bytes = string_bytes(&mem, ptr?)?;
                let i = trunc_to_int(value_to_number(&decode(arg0.cast_unsigned(), &mem)));
                // Out-of-range (a negative index fails `try_from`) => "".
                let text = match usize::try_from(i).ok().filter(|&u| u < bytes.len()) {
                    Some(u) => String::from_utf8_lossy(&bytes[u..=u]).into_owned(),
                    None => String::new(),
                };
                alloc_string(caller, &text).map(|base| HEAP | base)
            }
            _ => None,
        },
        METHOD_OP_TRIM | METHOD_OP_TRIM_START | METHOD_OP_TRIM_END => match type_byte {
            Some(TYPE_STRING) => string_trim(caller, &mem, ptr?, op),
            _ => None,
        },
        METHOD_OP_REPEAT => match type_byte {
            Some(TYPE_STRING) => string_repeat(caller, &mem, ptr?, arg0),
            _ => None,
        },
        METHOD_OP_STARTS_WITH | METHOD_OP_ENDS_WITH => match type_byte {
            Some(TYPE_STRING) => string_starts_ends(&mem, ptr?, arg0, op),
            _ => None,
        },
        METHOD_OP_PAD_START | METHOD_OP_PAD_END => match type_byte {
            Some(TYPE_STRING) => string_pad(caller, &mem, ptr?, arg0, arg1, op),
            _ => None,
        },
        METHOD_OP_SUBSTRING => match type_byte {
            Some(TYPE_STRING) => string_substring(caller, &mem, ptr?, arg0, arg1),
            _ => None,
        },
        METHOD_OP_SPLIT => match type_byte {
            Some(TYPE_STRING) => string_split(caller, &mem, ptr?, arg0.cast_unsigned()),
            _ => None,
        },
        _ => None,
    }
}

/// The linear-memory byte offset of a heap jsval (single heap tag `0b101`), or
/// `None` for any non-heap value. Mirrors `emit::HEAP_BITS` / the heap block layout.
fn heap_ptr(bits: u64) -> Option<usize> {
    if bits & QNAN == QNAN && (bits >> 48) & 0b111 == 0b101 {
        usize::try_from(bits & 0xFFFF_FFFF).ok()
    } else {
        None
    }
}

/// The u32 `len` header of a string block at `ptr + HEADER_SIZE`.
fn block_len(mem: &[u8], ptr: usize) -> Option<usize> {
    let at = ptr.checked_add(HEADER_SIZE)?;
    let bytes = mem.get(at..at + 4)?;
    usize::try_from(u32::from_le_bytes(bytes.try_into().ok()?)).ok()
}

/// The UTF-8 payload bytes of a heap STRING block (`[len:u32][utf8]` at
/// `ptr + HEADER_SIZE`), or `None` when the block is truncated.
fn string_bytes(mem: &[u8], ptr: usize) -> Option<Vec<u8>> {
    let len = block_len(mem, ptr)?;
    let start = ptr.checked_add(HEADER_SIZE + 4)?;
    Some(mem.get(start..start + len)?.to_vec())
}

/// Truncate a JS number toward zero to a signed integer, saturating out-of-range
/// magnitudes (the `f64 as i64` cast is saturating and well-defined). Used to turn
/// a slice/`charAt` endpoint into an integer index before clamping.
#[allow(clippy::cast_possible_truncation)]
fn trunc_to_int(n: f64) -> i64 {
    n.trunc() as i64
}

/// Box a non-negative sequence index as a number jsval's raw bits. The index is
/// small (a sequence length), so the lossless `u32 -> f64` conversion suffices.
fn index_bits(i: usize) -> u64 {
    f64::from(u32::try_from(i).unwrap_or(u32::MAX)).to_bits()
}

/// The number jsval bits for a "not found" result (`-1`).
fn not_found_bits() -> u64 {
    (-1.0_f64).to_bits()
}

/// JS strict equality (`===`) between two raw jsval bits: numbers compare by
/// `f64` value (so `NaN !== NaN`, `+0 === -0`), strings by byte content, and
/// everything else (booleans, `null`, `undefined`, heap references) by identity —
/// i.e. equal raw bits. Mismatched types are never equal (their bits differ).
#[allow(clippy::float_cmp)]
fn strict_eq_bits(a: u64, b: u64, mem: &[u8]) -> bool {
    match (decode(a, mem), decode(b, mem)) {
        // Exact `===` number equality (NOT an approximate compare): the same
        // semantics wasm's `f64.eq` gives the compiled `===` operator.
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        _ => a == b,
    }
}

/// `array.indexOf(x)`: the number bits of the first index whose element `=== x`,
/// or `-1` when absent. A hole is no element, so it never matches.
fn array_index_of(mem: &[u8], ptr: usize, needle: u64) -> u64 {
    let (_, present) = array_elements(mem, ptr).unwrap_or_default();
    present
        .into_iter()
        .find(|&(_, elem)| strict_eq_bits(elem, needle, mem))
        .map_or_else(not_found_bits, |(i, _)| index_bits(i))
}

/// `array.includes(x)`: whether some element is `SameValueZero` to `x` — `===`,
/// except that `NaN` matches `NaN`; a hole reads as `undefined`.
fn array_includes(mem: &[u8], ptr: usize, needle: u64) -> bool {
    let (len, present) = array_elements(mem, ptr).unwrap_or_default();
    (needle == abi::UNDEFINED && present.len() < len)
        || present.into_iter().any(|(_, elem)| {
            strict_eq_bits(elem, needle, mem)
                || matches!(
                    (decode(elem, mem), decode(needle, mem)),
                    (Value::Number(x), Value::Number(y)) if x.is_nan() && y.is_nan()
                )
        })
}

/// `string.indexOf(sub)`: the number bits of the first BYTE index where `sub`
/// occurs (an empty `sub` => `0`), or `-1` when absent / `sub` is not a string.
fn string_index_of(mem: &[u8], ptr: usize, sub: u64) -> u64 {
    let Some(hay) = string_bytes(mem, ptr) else {
        return not_found_bits();
    };
    let Value::Str(needle) = decode(sub, mem) else {
        return not_found_bits();
    };
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return index_bits(0);
    }
    match hay.windows(needle.len()).position(|w| w == needle) {
        Some(i) => index_bits(i),
        None => not_found_bits(),
    }
}

/// Resolve one JS slice endpoint against a sequence `len`: `undefined` uses
/// `default`, otherwise the argument is truncated to an integer, offset from the
/// end when negative, and clamped to `0..=len`. Computed entirely in the integer
/// domain so no lossy float/int casts are needed.
fn slice_index(bits: u64, mem: &[u8], default: usize, len: usize) -> usize {
    let decoded = decode(bits, mem);
    if matches!(decoded, Value::Undefined) {
        return default;
    }
    let n = trunc_to_int(value_to_number(&decoded));
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    // Negative offsets count from the end (clamped at 0); positives clamp to len.
    let idx = if n < 0 {
        (len_i + n).max(0)
    } else {
        n.min(len_i)
    };
    usize::try_from(idx).unwrap_or(0)
}

/// `array.slice(a, b)`: a NEW heap array of elements `[a, b)` (JS negative/clamp
/// semantics; `b` omitted => to end). Element jsval bits are copied verbatim, so
/// heap-reference elements stay shared and holes stay holes. Returns the result's heap jsval bits.
fn array_slice<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    a: i64,
    b: i64,
) -> Option<u64> {
    let (len, present) = array_elements(mem, ptr)?;
    let start = slice_index(a.cast_unsigned(), mem, 0, len);
    let end = slice_index(b.cast_unsigned(), mem, len, len).max(start);
    let mut elems = vec![abi::HOLE; end - start];
    for (i, bits) in present {
        if (start..end).contains(&i) {
            elems[i - start] = bits;
        }
    }
    alloc_array(caller, &elems).map(|base| HEAP | base)
}

/// `string.slice(a, b)`: a NEW substring `[a, b)` by BYTE index (JS negative/clamp
/// semantics; `b` omitted => to end). Returns the result's heap jsval bits.
fn string_slice<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    a: i64,
    b: i64,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let len = bytes.len();
    let start = slice_index(a.cast_unsigned(), mem, 0, len);
    let end = slice_index(b.cast_unsigned(), mem, len, len).max(start);
    let text = String::from_utf8_lossy(&bytes[start..end]);
    alloc_string(caller, &text).map(|base| HEAP | base)
}

/// `string.trim()` / `trimStart()` / `trimEnd()`: a NEW heap string with leading
/// and/or trailing ASCII whitespace removed (selected by `op`). Returns the
/// result's heap jsval bits.
fn string_trim<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    op: i32,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let trim_start = op != METHOD_OP_TRIM_END;
    let trim_end = op != METHOD_OP_TRIM_START;
    let text = String::from_utf8_lossy(trim_bytes(&bytes, trim_start, trim_end));
    alloc_string(caller, &text).map(|base| HEAP | base)
}

/// `string.repeat(n)`: a NEW heap string of the receiver repeated `n` times. A
/// negative/NaN (=> `0`) count yields `""` (JS throws a `RangeError` — a documented
/// limitation); an overflowing total size yields `undefined`.
fn string_repeat<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    arg0: i64,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let count = trunc_to_int(value_to_number(&decode(arg0.cast_unsigned(), mem)));
    let n = usize::try_from(count).unwrap_or(0);
    let total = bytes.len().checked_mul(n)?;
    let mut out = Vec::with_capacity(total);
    for _ in 0..n {
        out.extend_from_slice(&bytes);
    }
    let text = String::from_utf8_lossy(&out);
    alloc_string(caller, &text).map(|base| HEAP | base)
}

/// `string.startsWith(sub)` / `endsWith(sub)`: a boolean prefix/suffix test (`sub`
/// coerced to its string form). Returns the boolean jsval bits.
fn string_starts_ends(mem: &[u8], ptr: usize, arg0: i64, op: i32) -> Option<u64> {
    let hay = string_bytes(mem, ptr)?;
    let needle = arg_bytes(arg0, mem);
    let hit = if op == METHOD_OP_STARTS_WITH {
        hay.starts_with(&needle)
    } else {
        hay.ends_with(&needle)
    };
    Some(bool_bits(hit))
}

/// `string.padStart(len, pad)` / `padEnd(len, pad)`: a NEW heap string padded to
/// BYTE length `len` with `pad` (default `" "`) on the selected side. Returns the
/// result's heap jsval bits.
fn string_pad<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    arg0: i64,
    arg1: i64,
    op: i32,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let target = trunc_to_int(value_to_number(&decode(arg0.cast_unsigned(), mem)));
    // The pad string defaults to a single space when omitted.
    let pad = match decode(arg1.cast_unsigned(), mem) {
        Value::Undefined => " ".to_owned(),
        other => to_js_string(&other),
    };
    let text = pad_bytes(&bytes, target, pad.as_bytes(), op == METHOD_OP_PAD_START);
    alloc_string(caller, &text).map(|base| HEAP | base)
}

/// `string.substring(a, b)`: a NEW substring `[a, b)` by BYTE index (endpoints
/// clamped to `0..=len` and swapped when `a > b`; `b` omitted => to end). Returns
/// the result's heap jsval bits.
fn string_substring<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    arg0: i64,
    arg1: i64,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let len = bytes.len();
    let a = substring_index(arg0.cast_unsigned(), mem, 0, len);
    let b = substring_index(arg1.cast_unsigned(), mem, len, len);
    let (lo, hi) = (a.min(b), a.max(b));
    let text = String::from_utf8_lossy(&bytes[lo..hi]);
    alloc_string(caller, &text).map(|base| HEAP | base)
}

/// The NaN-boxed jsval bits of a boolean (`abi::TRUE` / `abi::FALSE`).
fn bool_bits(b: bool) -> u64 {
    if b { TRUE_BITS } else { FALSE_BITS }
}

/// Coerce a method argument's raw jsval bits to its JS string form as bytes: a
/// string yields its payload, everything else its JS `ToString` text (so
/// `"5".startsWith(5)` matches on `"5"`). Used by the string search/split methods.
fn arg_bytes(bits: i64, mem: &[u8]) -> Vec<u8> {
    to_js_string(&decode(bits.cast_unsigned(), mem)).into_bytes()
}

/// Whether `hay` contains the contiguous byte sequence `needle` (an empty
/// `needle` is always contained — JS `"".includes` / `str.includes("")` are true).
fn byte_contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// Slice `bytes` with leading and/or trailing ASCII whitespace removed. `is_ascii_whitespace`
/// matches the same subset as the existing numeric parsers (space/`\t`/`\n`/`\r`/`\x0C`);
/// non-ASCII whitespace is out of scope (a documented limitation).
fn trim_bytes(bytes: &[u8], trim_start: bool, trim_end: bool) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    if trim_start {
        while start < end && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
    }
    if trim_end {
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
    }
    &bytes[start..end]
}

/// Pad `bytes` to BYTE length `target` with `pad` repeated (and truncated to fit),
/// on the start (`at_start`) or the end. The receiver is returned unchanged when it
/// is already at least `target` bytes or `pad` is empty (JS `padStart`/`padEnd`).
fn pad_bytes(bytes: &[u8], target: i64, pad: &[u8], at_start: bool) -> String {
    let need = usize::try_from(target)
        .unwrap_or(0)
        .saturating_sub(bytes.len());
    if need == 0 || pad.is_empty() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let filler: Vec<u8> = pad.iter().copied().cycle().take(need).collect();
    let mut out = Vec::with_capacity(bytes.len() + need);
    if at_start {
        out.extend_from_slice(&filler);
        out.extend_from_slice(bytes);
    } else {
        out.extend_from_slice(bytes);
        out.extend_from_slice(&filler);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve one `substring` endpoint against a BYTE `len`: `undefined` uses
/// `default`, otherwise the argument is truncated to an integer and clamped to
/// `0..=len` (negatives and NaN become `0` — unlike `slice`, there is no
/// negative-from-end offset).
fn substring_index(bits: u64, mem: &[u8], default: usize, len: usize) -> usize {
    let decoded = decode(bits, mem);
    if matches!(decoded, Value::Undefined) {
        return default;
    }
    let n = trunc_to_int(value_to_number(&decoded));
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    usize::try_from(n.clamp(0, len_i)).unwrap_or(0)
}

/// `string.split(sep)`: a NEW heap array of substrings (each a NEW heap string) by
/// BYTE index. `sep` omitted (`undefined`) => `[str]`; `sep === ""` => one element
/// per byte; otherwise split on each non-overlapping `sep` occurrence (`sep`
/// coerced to its string form). Returns the array's heap jsval bits.
fn string_split<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    mem: &[u8],
    ptr: usize,
    sep_bits: u64,
) -> Option<u64> {
    let bytes = string_bytes(mem, ptr)?;
    let pieces: Vec<Vec<u8>> = match decode(sep_bits, mem) {
        // An omitted separator yields a single-element array of the whole string.
        Value::Undefined => vec![bytes.clone()],
        other => {
            let sep = to_js_string(&other).into_bytes();
            if sep.is_empty() {
                // An empty separator splits into one element per byte.
                bytes.iter().map(|b| vec![*b]).collect()
            } else {
                split_on(&bytes, &sep)
            }
        }
    };
    // Allocate each piece as a heap string first (each is an owned byte vec,
    // independent of guest memory), collecting their jsval bits, then build the
    // backing array from them.
    let mut elems = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        let text = String::from_utf8_lossy(piece);
        elems.push(HEAP | alloc_string(caller, &text)?);
    }
    alloc_array(caller, &elems).map(|base| HEAP | base)
}

/// Split `hay` on each non-overlapping occurrence of the non-empty `sep`, keeping
/// empty leading/trailing/adjacent pieces (JS `String.split` semantics: `"a,,b"`
/// => `["a", "", "b"]`, `"".split(",")` => `[""]`).
fn split_on(hay: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= hay.len() {
        if &hay[i..i + sep.len()] == sep {
            out.push(hay[start..i].to_vec());
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(hay[start..].to_vec());
    out
}

/// Coerce a decoded [`Value`] to a JS number (the `Number(x)` subset):
/// number is identity, `true`/`false` are `1`/`0`, `null` is `0`, `undefined` is
/// `NaN`, and a string is full-string parsed via [`str_to_number`]. Array/object/
/// function operands are out of scope and yield `NaN`.
pub(super) fn value_to_number(value: &Value) -> f64 {
    match value {
        Value::Number(n) => *n,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Null => 0.0,
        Value::Str(s) => str_to_number(s),
        // `undefined` and the out-of-scope heap kinds (array/object/function)
        // coerce to `NaN`.
        _ => f64::NAN,
    }
}

/// Parse a whole string as a JS `Number(str)`: leading/trailing ASCII
/// whitespace is trimmed, an empty/whitespace-only string is `0`, and anything the
/// remaining text does not parse as a full number is `NaN`. The grammar is JS
/// `StringToNumber`: an unsigned `0x`/`0o`/`0b` integer literal, a signed
/// `Infinity`, or a signed decimal literal (Rust's `inf`/`nan` spellings are not
/// JS and yield `NaN`).
fn str_to_number(s: &str) -> f64 {
    let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return 0.0;
    }
    let radix = match trimmed.get(..2) {
        Some("0x" | "0X") => Some(16),
        Some("0o" | "0O") => Some(8),
        Some("0b" | "0B") => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        let digits = &trimmed[2..];
        if digits.is_empty() {
            return f64::NAN;
        }
        return digits
            .chars()
            .try_fold(0.0, |acc, c| {
                c.to_digit(radix)
                    .map(|d| acc * f64::from(radix) + f64::from(d))
            })
            .unwrap_or(f64::NAN);
    }
    let unsigned = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    if unsigned == "Infinity" {
        return if trimmed.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    if !unsigned.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `parseInt(s, radix)` on the already-stringified `s` and the already
/// number-coerced `radix`: skip leading JS whitespace, honor an optional `+`/`-`
/// sign, then take `R = ToInt32(radix)`. `R == 0` means 10 unless a `0x`/`0X`
/// prefix selects 16; `R == 16` also strips an optional `0x` prefix; any other
/// `R` outside `2..=36` yields `NaN`. The longest prefix of radix-`R` digits
/// (letters case-insensitive) is the value; no digits yields `NaN`.
pub(super) fn parse_int_str(s: &str, radix: f64) -> f64 {
    let s = s.trim_start_matches(is_js_whitespace);
    let (sign, mut s) = match s.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, s.strip_prefix('+').unwrap_or(s)),
    };
    let r = to_int32(radix);
    let (mut radix, strip_prefix) = if r == 0.0 {
        (10, true)
    } else {
        match (2_u32..=36).find(|&k| f64::from(k).total_cmp(&r).is_eq()) {
            Some(k) => (k, k == 16),
            None => return f64::NAN,
        }
    };
    if strip_prefix && let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        s = rest;
        radix = 16;
    }
    let end = s.find(|c: char| !c.is_digit(radix)).unwrap_or(s.len());
    let digits = &s[..end];
    if digits.is_empty() {
        return f64::NAN;
    }
    // Base 10 goes through the correctly rounded decimal parser; other radices
    // accumulate digit by digit.
    let value = if radix == 10 {
        digits.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        digits.chars().fold(0.0, |acc, c| {
            acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
        })
    };
    sign * value
}

/// JS `StrWhiteSpaceChar`: the `WhiteSpace` and `LineTerminator` code points
/// (Unicode `White_Space` minus U+0085, plus U+FEFF).
fn is_js_whitespace(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// JS `ToInt32` of a number, returned as the (integral) f64 in
/// `-2^31..2^31`: non-finite values map to 0, otherwise the truncated value
/// wraps modulo 2^32.
fn to_int32(n: f64) -> f64 {
    const TWO_32: f64 = 4_294_967_296.0;
    if !n.is_finite() {
        return 0.0;
    }
    let m = n.trunc().rem_euclid(TWO_32);
    if m >= TWO_32 / 2.0 { m - TWO_32 } else { m }
}

/// Parse a leading decimal float from `s` (the `parseFloat` subset): skip
/// leading ASCII whitespace, then take the longest prefix matching an optional
/// sign, integer/fraction digits (at least one digit overall), and an optional
/// `e`/`E` exponent. Yields `NaN` when no such prefix exists.
pub(super) fn parse_float_str(s: &str) -> f64 {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut seen_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        seen_digit = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            seen_digit = true;
        }
    }
    if !seen_digit {
        return f64::NAN;
    }
    // Optional exponent: only consumed when at least one exponent digit follows.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    s[..i].parse::<f64>().unwrap_or(f64::NAN)
}
