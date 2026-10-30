//! Host-import handlers backing the `__convert`/`__method` env imports. Everything
//! here runs host-side against a `wasmtime::Caller`, decoding the guest's linear
//! memory and (for the methods that build a fresh value) allocating a new heap
//! block through the guest's exported constructors.

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use wasmtime::{AsContextMut, Caller, Global};

use crate::abi;

use super::value::{array_elements, string_units};
use super::{HEAP, QNAN, STR_DATA_OFFSET, TYPE_ARRAY, TYPE_STRING, Value, decode, to_js_string};

/// The `__method` op flags (see [`abi`]): the array element searches, the
/// number formatting and `Math` ops, and the string case mappings,
/// `localeCompare` and `normalize`, dispatched host-side.
const METHOD_OP_INDEX_OF: i32 = abi::METHOD_OP_INDEX_OF;
const METHOD_OP_INCLUDES: i32 = abi::METHOD_OP_INCLUDES;
const METHOD_OP_TO_UPPER: i32 = abi::METHOD_OP_TO_UPPER;
const METHOD_OP_TO_LOWER: i32 = abi::METHOD_OP_TO_LOWER;
const METHOD_OP_NUMBER_TO_STRING: i32 = abi::METHOD_OP_NUMBER_TO_STRING;
const METHOD_OP_NUMBER_TO_FIXED: i32 = abi::METHOD_OP_NUMBER_TO_FIXED;
const METHOD_OP_NUMBER_TO_EXPONENTIAL: i32 = abi::METHOD_OP_NUMBER_TO_EXPONENTIAL;
const METHOD_OP_NUMBER_TO_PRECISION: i32 = abi::METHOD_OP_NUMBER_TO_PRECISION;
const METHOD_OP_MATH: i32 = abi::METHOD_OP_MATH;
const METHOD_OP_LOCALE_COMPARE: i32 = abi::METHOD_OP_LOCALE_COMPARE;
const METHOD_OP_NORMALIZE: i32 = abi::METHOD_OP_NORMALIZE;

/// The `__method` result the calling builtin turns into a `RangeError`; see
/// [`abi::METHOD_RANGE_ERROR`].
const RANGE_ERROR: u64 = abi::METHOD_RANGE_ERROR;

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

/// Allocate a heap STRING block holding `text` (as UTF-16 code units, see
/// [`alloc_units`]) and return its base offset.
pub(super) fn alloc_string<W: Send + 'static>(
    caller: &mut Caller<'_, W>,
    text: &str,
) -> Option<u64> {
    alloc_units(caller, &text.encode_utf16().collect::<Vec<u16>>())
}

/// Allocate a heap STRING block holding the code units `units` in canonical
/// form (one-byte Latin-1 when every unit is `<= 0xFF`, else UTF-16LE) through
/// the guest `__new_string(len, two_byte)` export and return its base offset,
/// or `None` if the export is missing or the allocation/write fails. The guest
/// writes the header and the width flag; the host only fills the payload.
fn alloc_units<W: Send + 'static>(caller: &mut Caller<'_, W>, units: &[u16]) -> Option<u64> {
    let units = &units[..units.len().min(MEMORY_MAX_BYTES)];
    let two_byte = units.iter().any(|&unit| unit > 0xFF);
    let ctor = caller
        .get_export(abi::NEW_STRING_EXPORT_NAME)
        .and_then(wasmtime::Extern::into_func)?
        .typed::<(i32, i32), i32>(&*caller)
        .ok()?;
    let len = i32::try_from(units.len()).ok()?;
    let base = ctor.call(&mut *caller, (len, i32::from(two_byte))).ok()?;
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

/// Whether the heap-pointer global holds the out-of-memory sentinel: some guest
/// or host allocation could not grow linear memory.
pub(super) fn heap_exhausted(mut store: impl AsContextMut, global: Global) -> bool {
    global.get(&mut store).i32() == Some(HEAP_PTR_OOM)
}

/// Dispatch a `__method` host-import call: snapshot linear memory, read
/// the receiver's raw jsval bits, and perform the requested method, returning
/// the result jsval bits (or `None` to yield `undefined` — a type-mismatched
/// receiver or a failed allocation).
///
/// `IndexOf`/`Includes` take an array receiver; the number ops a number
/// receiver; the string ops a string receiver, their arguments already
/// coerced by the guest builtins. `normalize` with an unknown form returns
/// [`RANGE_ERROR`], which the builtin throws as a `RangeError`.
#[allow(clippy::too_many_lines)] // one flat dispatch over the op flags
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
            _ => None,
        },
        METHOD_OP_INCLUDES => match type_byte {
            Some(TYPE_ARRAY) => Some(bool_bits(array_includes(&mem, ptr?, arg0.cast_unsigned()))),
            _ => None,
        },
        METHOD_OP_NUMBER_TO_STRING => {
            let Value::Number(x) = decode(recv, &mem) else {
                return None;
            };
            let radix = value_to_number(&decode(arg0.cast_unsigned(), &mem));
            let text = number_to_radix_string(x, radix_digits(radix));
            alloc_string(caller, &text).map(|base| HEAP | base)
        }
        METHOD_OP_NUMBER_TO_FIXED
        | METHOD_OP_NUMBER_TO_EXPONENTIAL
        | METHOD_OP_NUMBER_TO_PRECISION => {
            let Value::Number(x) = decode(recv, &mem) else {
                return None;
            };
            let digits = match decode(arg0.cast_unsigned(), &mem) {
                Value::Number(d) => Some(trunc_to_int(d)),
                _ => None,
            };
            let text = match (op, digits) {
                (METHOD_OP_NUMBER_TO_FIXED, Some(f)) => number_to_fixed(x, f),
                (METHOD_OP_NUMBER_TO_PRECISION, Some(p)) => number_to_precision(x, p),
                (METHOD_OP_NUMBER_TO_EXPONENTIAL, f) => number_to_exponential(x, f),
                _ => return None,
            };
            alloc_string(caller, &text).map(|base| HEAP | base)
        }
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
                let text = string_text(&mem, ptr?)?;
                // The full Unicode default case mappings (`ß` -> `SS`, final sigma).
                let mapped = if op == METHOD_OP_TO_UPPER {
                    text.to_uppercase()
                } else {
                    text.to_lowercase()
                };
                alloc_string(caller, &mapped).map(|base| HEAP | base)
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
                alloc_units(caller, &units).map(|base| HEAP | base)
            }
            _ => None,
        },
        _ => None,
    }
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

/// The radix `Number.prototype.toString` validated (an integral `2..=36`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn radix_digits(radix: f64) -> u32 {
    radix.clamp(2.0, 36.0) as u32
}

/// `Number.prototype.toString(radix)` for a radix other than 10: the
/// shortest digit string that reads back as `x` (V8's `DoubleToRadixCString`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]
pub(super) fn number_to_radix_string(x: f64, radix: u32) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if x == 0.0 {
        return "0".to_owned();
    }
    let digit = |d: u32| char::from_digit(d, radix).unwrap_or('0');
    let base = f64::from(radix);
    let value = x.abs();
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // The distance to the next double, halved: digits stop once the
    // remaining fraction is below it.
    let next_up = f64::from_bits(value.to_bits() + 1);
    let mut delta = (0.5 * (next_up - value)).max(f64::from_bits(1));
    let mut fraction_digits: Vec<u32> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= base;
            delta *= base;
            let d = fraction as u32;
            fraction_digits.push(d);
            fraction -= f64::from(d);
            // Round to even.
            if (fraction > 0.5 || (fraction == 0.5 && d & 1 == 1)) && fraction + delta > 1.0 {
                // Carry into the already written digits.
                loop {
                    let Some(last) = fraction_digits.pop() else {
                        integer += 1.0;
                        break;
                    };
                    if last + 1 < radix {
                        fraction_digits.push(last + 1);
                        break;
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer digits; past 2^53 the unrepresented low digits are zeros.
    let mut int_digits: Vec<char> = Vec::new();
    while integer / base >= 9_007_199_254_740_992.0 {
        integer /= base;
        int_digits.push('0');
    }
    loop {
        let remainder = integer % base;
        int_digits.push(digit(remainder as u32));
        integer = (integer - remainder) / base;
        if integer <= 0.0 {
            break;
        }
    }
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    out.extend(int_digits.iter().rev());
    if !fraction_digits.is_empty() {
        out.push('.');
        out.extend(fraction_digits.into_iter().map(digit));
    }
    out
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
        abi::MATH_FN_RANDOM => random(),
        _ => f64::NAN,
    }
}

/// The `xorshift64*` state of [`random`]; `0` until first seeded.
static RANDOM_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `Math.random()`: a `xorshift64*` step over a process-wide state seeded from
/// the std per-process hash keys, scaled to 53 bits in `[0, 1)`.
fn random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::Ordering;
    let mut x = RANDOM_STATE.load(Ordering::Relaxed);
    if x == 0 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0x9E37_79B9_7F4A_7C15);
        x = hasher.finish() | 1;
    }
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    RANDOM_STATE.store(x, Ordering::Relaxed);
    let bits = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
    // A 53-bit integer converts to `f64` exactly.
    #[allow(clippy::cast_precision_loss)]
    let unit = bits as f64;
    unit / 9_007_199_254_740_992.0
}

/// The exact decimal digits of a finite, nonzero `|x|`: `(digits, point)` with
/// `|x| = 0.d1d2.. * 10^point`, no leading or trailing zero digits. Every
/// double has a finite decimal expansion of at most 1074 fractional digits,
/// which `{:.1100}` prints exactly.
fn exact_decimal(x: f64) -> (Vec<u8>, i64) {
    let text = format!("{:.1100}", x.abs());
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let int = int.trim_start_matches('0');
    let (mut digits, point): (Vec<u8>, i64) = if int.is_empty() {
        let lead = frac.bytes().take_while(|&b| b == b'0').count();
        let digits = frac.bytes().skip(lead).map(|b| b - b'0').collect();
        (digits, -i64::try_from(lead).unwrap_or(0))
    } else {
        let digits = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
        (digits, i64::try_from(int.len()).unwrap_or(0))
    };
    while digits.last() == Some(&0) {
        digits.pop();
    }
    (digits, point)
}

/// Round `(digits, point)` to `n` significant digits, ties away from zero (the
/// spec's "pick the larger `n`"): `n` digits (`0` when `n <= 0` rounds to
/// zero) and the possibly carried point.
fn round_digits(digits: &[u8], point: i64, n: i64) -> (Vec<u8>, i64) {
    let Ok(keep) = usize::try_from(n) else {
        return (Vec::new(), point);
    };
    let mut out: Vec<u8> = (0..keep)
        .map(|i| digits.get(i).copied().unwrap_or(0))
        .collect();
    if digits.get(keep).is_some_and(|&d| d >= 5) {
        let mut i = keep;
        loop {
            if i == 0 {
                out.insert(0, 1);
                out.truncate(keep.max(1));
                return (out, point + 1);
            }
            i -= 1;
            if out[i] == 9 {
                out[i] = 0;
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    (out, point)
}

/// Decimal digit values as text.
fn digit_text(digits: &[u8]) -> String {
    digits.iter().map(|d| char::from(b'0' + d)).collect()
}

/// `Number.prototype.toFixed(f)` of a finite `x` (`f` in `0..=100`).
pub(super) fn number_to_fixed(x: f64, f: i64) -> String {
    if x.abs() >= 1e21 {
        return super::value::format_number(x);
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let f_len = usize::try_from(f).unwrap_or(0);
    // `n = round(|x| * 10^f)` as a digit string.
    let mut n = if x == 0.0 {
        "0".to_owned()
    } else {
        let (digits, point) = exact_decimal(x);
        let (rounded, point) = round_digits(&digits, point, point + f);
        let width = usize::try_from(point + f).unwrap_or(0);
        let mut text = digit_text(&rounded);
        while text.len() < width {
            text.push('0');
        }
        if text.is_empty() {
            "0".to_owned()
        } else {
            text
        }
    };
    if f_len == 0 {
        return format!("{sign}{n}");
    }
    if n.len() <= f_len {
        n = format!("{}{n}", "0".repeat(f_len + 1 - n.len()));
    }
    let (int, frac) = n.split_at(n.len() - f_len);
    format!("{sign}{int}.{frac}")
}

/// `d.ddd` + `e±k` of the significant `digits` with exponent `e`.
fn exponential_text(sign: &str, digits: &str, e: i64) -> String {
    let (head, tail) = digits.split_at(1.min(digits.len()));
    let dot = if tail.is_empty() { "" } else { "." };
    let e_sign = if e < 0 { "-" } else { "+" };
    format!("{sign}{head}{dot}{tail}e{e_sign}{}", e.abs())
}

/// `Number.prototype.toExponential(f)` of a finite `x`: `f + 1` significant
/// digits (`f` in `0..=100`), or the shortest round-tripping digits for `None`.
pub(super) fn number_to_exponential(x: f64, f: Option<i64>) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    if x == 0.0 {
        let zeros = "0".repeat(usize::try_from(f.unwrap_or(0)).unwrap_or(0));
        return exponential_text("", &format!("0{zeros}"), 0);
    }
    let (digits, e) = if let Some(f) = f {
        let (digits, point) = exact_decimal(x);
        let (rounded, point) = round_digits(&digits, point, f + 1);
        (digit_text(&rounded), point - 1)
    } else {
        let sci = format!("{:e}", x.abs());
        let (mantissa, e) = sci.split_once('e').unwrap_or((&sci, "0"));
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        (digits, e.parse().unwrap_or(0))
    };
    exponential_text(sign, &digits, e)
}

/// `Number.prototype.toPrecision(p)` of a finite `x` (`p` in `1..=100`).
pub(super) fn number_to_precision(x: f64, p: i64) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let (digits, e) = if x == 0.0 {
        (vec![0; usize::try_from(p).unwrap_or(1)], 0)
    } else {
        let (digits, point) = exact_decimal(x);
        let (rounded, point) = round_digits(&digits, point, p);
        (rounded, point - 1)
    };
    let text = digit_text(&digits);
    if e < -6 || e >= p {
        return exponential_text(sign, &text, e);
    }
    if e == p - 1 {
        return format!("{sign}{text}");
    }
    if e >= 0 {
        let (int, frac) = text.split_at(usize::try_from(e + 1).unwrap_or(0));
        return format!("{sign}{int}.{frac}");
    }
    let zeros = "0".repeat(usize::try_from(-(e + 1)).unwrap_or(0));
    format!("{sign}0.{zeros}{text}")
}

/// The NaN-boxed jsval bits of a boolean (`abi::TRUE` / `abi::FALSE`).
fn bool_bits(b: bool) -> u64 {
    if b { TRUE_BITS } else { FALSE_BITS }
}

/// The text of a heap STRING block, a lone surrogate replaced by U+FFFD.
fn string_text(mem: &[u8], ptr: usize) -> Option<String> {
    Some(String::from_utf16_lossy(&string_units(mem, ptr)?))
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

/// Parse a whole string as a JS `Number(str)`: leading/trailing JS
/// whitespace ([`is_js_whitespace`]) is trimmed, an empty/whitespace-only string is `0`, and anything the
/// remaining text does not parse as a full number is `NaN`. The grammar is JS
/// `StringToNumber`: an unsigned `0x`/`0o`/`0b` integer literal, a signed
/// `Infinity`, or a signed decimal literal (Rust's `inf`/`nan` spellings are not
/// JS and yield `NaN`).
fn str_to_number(s: &str) -> f64 {
    let trimmed = s.trim_matches(is_js_whitespace);
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
/// leading JS whitespace ([`is_js_whitespace`]), then take the longest prefix matching an optional
/// sign, integer/fraction digits (at least one digit overall), and an optional
/// `e`/`E` exponent. Yields `NaN` when no such prefix exists.
pub(super) fn parse_float_str(s: &str) -> f64 {
    let s = s.trim_start_matches(is_js_whitespace);
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
