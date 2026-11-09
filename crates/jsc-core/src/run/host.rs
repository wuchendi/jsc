//! Host-import handlers backing the `__method` env import. Everything
//! here runs host-side against a `wasmtime::Caller`, decoding the guest's linear
//! memory and (for the methods that build a fresh value) allocating a new heap
//! block through the guest's exported constructors.

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use wasmtime::Caller;

use crate::abi;

use abi::{HEAP, QNAN, TYPE_ARRAY, TYPE_STRING};

use super::harness::reentry_error;
use super::value::{Value, array_elements, decode, string_units, to_js_string};

const STR_DATA_OFFSET: usize = abi::STR_DATA_OFFSET as usize;

/// The `__method` op flags (see [`abi`]): the array element searches, the
/// `Math` op, and the string case mappings,
/// `localeCompare` and `normalize`, dispatched host-side.
const METHOD_OP_INDEX_OF: i32 = abi::METHOD_OP_INDEX_OF;
const METHOD_OP_INCLUDES: i32 = abi::METHOD_OP_INCLUDES;
const METHOD_OP_TO_UPPER: i32 = abi::METHOD_OP_TO_UPPER;
const METHOD_OP_TO_LOWER: i32 = abi::METHOD_OP_TO_LOWER;
const METHOD_OP_MATH: i32 = abi::METHOD_OP_MATH;
const METHOD_OP_LOCALE_COMPARE: i32 = abi::METHOD_OP_LOCALE_COMPARE;
const METHOD_OP_NORMALIZE: i32 = abi::METHOD_OP_NORMALIZE;

/// The `__method` result the calling builtin turns into a `RangeError`; see
/// [`abi::METHOD_RANGE_ERROR`].
const RANGE_ERROR: u64 = abi::METHOD_RANGE_ERROR;

/// The NaN-boxed `true`/`false` jsval bits; see [`abi::TRUE`] / [`abi::FALSE`].
const TRUE_BITS: u64 = abi::TRUE;
const FALSE_BITS: u64 = abi::FALSE;

/// The linear-memory cap in bytes. A constructor request at or past it can
/// only run out of memory, so lengths are clamped to it before the call (which
/// keeps the guest's `i32` size arithmetic from wrapping).
// 512 MiB fits `usize` on every supported target.
#[allow(clippy::cast_possible_truncation)]
const MEMORY_MAX_BYTES: usize = abi::MEMORY_MAX_PAGES as usize * 0x1_0000;

/// Allocate a heap STRING block holding the code units `units` in canonical
/// form (one-byte Latin-1 when every unit is `<= 0xFF`, else UTF-16LE) through
/// the guest `__new_string(len, two_byte)` export and return its base offset,
/// or `None` if the export is missing or the allocation/write fails. The guest
/// writes the header and the width flag; the host only fills the payload. A
/// failed guest call (a `proc_exit` from the out-of-memory path, or a trap) is
/// left in `failed` for the import to return.
fn alloc_units<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    failed: &mut Option<wasmtime::Error>,
    units: &[u16],
) -> Option<u64> {
    let units = &units[..units.len().min(MEMORY_MAX_BYTES)];
    let two_byte = units.iter().any(|&unit| unit > 0xFF);
    let ctor = caller
        .get_export(abi::NEW_STRING_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_func)?
        .typed::<(i32, i32), i32>(&*caller)
        .ok()?;
    let len = i32::try_from(units.len()).ok()?;
    let base = match ctor.call(&mut *caller, (len, i32::from(two_byte))) {
        Ok(base) => base,
        Err(err) => {
            *failed = Some(reentry_error(err));
            return None;
        }
    };
    let base = usize::try_from(base).ok()?;
    let data: Vec<u8> = if two_byte {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    } else {
        units.iter().map(|unit| unit.to_le_bytes()[0]).collect()
    };
    let mem = caller
        .get_export(abi::MEMORY_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_memory)?;
    mem.write(&mut *caller, base + STR_DATA_OFFSET, &data)
        .ok()?;
    Some(base as u64)
}

/// Dispatch a `__method` host-import call: snapshot linear memory, read
/// the receiver's raw jsval bits, and perform the requested method, returning
/// the result jsval bits (or `None` to yield `undefined` — a type-mismatched
/// receiver or a failed allocation).
///
/// `IndexOf`/`Includes` take an array receiver; the string ops a string receiver, their arguments already
/// coerced by the guest builtins. `normalize` with an unknown form returns
/// [`RANGE_ERROR`], which the builtin throws as a `RangeError`.
#[allow(clippy::too_many_lines)] // one flat dispatch over the op flags
pub(super) fn method_dispatch<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    failed: &mut Option<wasmtime::Error>,
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
            _ => None,
        },
        METHOD_OP_INCLUDES => match type_byte {
            Some(TYPE_ARRAY) => Some(bool_bits(array_includes(&mem, ptr?, arg0.cast_unsigned()))),
            _ => None,
        },
        METHOD_OP_MATH => {
            let number = |bits: u64| match decode(bits, &mem) {
                Value::Number(n) => n,
                _ => f64::NAN,
            };
            let n = math_fn(arg1, number(recv), number(arg0.cast_unsigned()));
            Some(if n.is_nan() { QNAN } else { n.to_bits() })
        }
        METHOD_OP_TO_UPPER | METHOD_OP_TO_LOWER => match type_byte {
            Some(TYPE_STRING) => {
                let units = string_units(&mem, ptr?)?;
                let mapped = map_case(&units, op == METHOD_OP_TO_UPPER);
                alloc_units(caller, failed, &mapped).map(|base| HEAP | base)
            }
            _ => None,
        },
        METHOD_OP_LOCALE_COMPARE => match type_byte {
            Some(TYPE_STRING) => {
                // Code-unit order over the lossless units; the guest coerced
                // the argument to a string.
                let this = string_units(&mem, ptr?)?;
                let that = string_units(&mem, heap_ptr(arg0.cast_unsigned())?)?;
                Some(f64::from(this.cmp(&that) as i8).to_bits())
            }
            _ => None,
        },
        METHOD_OP_NORMALIZE => match type_byte {
            Some(TYPE_STRING) => {
                let form = match decode(arg0.cast_unsigned(), &mem) {
                    Value::Undefined => "NFC".to_owned(),
                    other => to_js_string(&other),
                };
                if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                    return Some(RANGE_ERROR);
                }
                let units = normalize_units(&string_units(&mem, ptr?)?, &form);
                alloc_units(caller, failed, &units).map(|base| HEAP | base)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The full Unicode default case mapping of the code units `units` (upper when
/// `upper`): each maximal run of valid code points maps through std
/// (`SpecialCasing`: `ß` -> `SS`; `Final_Sigma` in the run), a lone surrogate
/// passes through as its unit.
fn map_case(units: &[u16], upper: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u16>| {
        let mapped = if upper {
            run.to_uppercase()
        } else {
            run.to_lowercase()
        };
        out.extend(mapped.encode_utf16());
        run.clear();
    };
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(c) => run.push(c),
            Err(lone) => {
                flush(&mut run, &mut out);
                out.push(lone.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// `units` in the Unicode normalization `form` (`NFC`, `NFD`, `NFKC` or
/// `NFKD`). A lone surrogate never combines, so it passes through unchanged
/// and the well-formed runs between lone surrogates normalize on their own.
fn normalize_units(units: &[u16], form: &str) -> Vec<u16> {
    let normalize = |run: &str, out: &mut Vec<u16>| {
        let normalized = match form {
            "NFD" => DecomposingNormalizerBorrowed::new_nfd().normalize(run),
            "NFKC" => ComposingNormalizerBorrowed::new_nfkc().normalize(run),
            "NFKD" => DecomposingNormalizerBorrowed::new_nfkd().normalize(run),
            _ => ComposingNormalizerBorrowed::new_nfc().normalize(run),
        };
        out.extend(normalized.encode_utf16());
    };
    let mut out = Vec::with_capacity(units.len());
    let mut run = String::new();
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(ch) => run.push(ch),
            Err(lone) => {
                normalize(&run, &mut out);
                run.clear();
                out.push(lone.unpaired_surrogate());
            }
        }
    }
    normalize(&run, &mut out);
    out
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

/// The host `Math` function `code` (an [`abi`] `MATH_FN_*`) of `x` (and `y`
/// for `atan2`): the platform libm, which is correctly rounded or within an ulp.
fn math_fn(code: i64, x: f64, y: f64) -> f64 {
    match code {
        abi::MATH_FN_CBRT => x.cbrt(),
        abi::MATH_FN_LOG => x.ln(),
        abi::MATH_FN_LOG2 => x.log2(),
        abi::MATH_FN_LOG10 => x.log10(),
        abi::MATH_FN_LOG1P => x.ln_1p(),
        abi::MATH_FN_EXP => x.exp(),
        abi::MATH_FN_EXPM1 => x.exp_m1(),
        abi::MATH_FN_SIN => x.sin(),
        abi::MATH_FN_COS => x.cos(),
        abi::MATH_FN_TAN => x.tan(),
        abi::MATH_FN_ASIN => x.asin(),
        abi::MATH_FN_ACOS => x.acos(),
        abi::MATH_FN_ATAN => x.atan(),
        abi::MATH_FN_ATAN2 => x.atan2(y),
        abi::MATH_FN_SINH => x.sinh(),
        abi::MATH_FN_COSH => x.cosh(),
        abi::MATH_FN_TANH => x.tanh(),
        abi::MATH_FN_ASINH => x.asinh(),
        abi::MATH_FN_ACOSH => x.acosh(),
        abi::MATH_FN_ATANH => x.atanh(),
        _ => f64::NAN,
    }
}

/// The NaN-boxed jsval bits of a boolean (`abi::TRUE` / `abi::FALSE`).
fn bool_bits(b: bool) -> u64 {
    if b { TRUE_BITS } else { FALSE_BITS }
}
