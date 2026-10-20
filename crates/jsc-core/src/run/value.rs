//! Decode a raw jsval into a [`Value`] and render a [`Value`] as user-facing
//! text. Pure (no `wasmtime`): the NaN-box decoder and the Node-style formatter.

use super::{
    DATE_MS_OFFSET, ERROR_KIND_KEY, HEADER_SIZE, MAP_SIZE_OFFSET, PROMISE_FULFILLED,
    PROMISE_REJECTED, PROMISE_STATE_OFFSET, PROMISE_VALUE_OFFSET, QNAN, TYPE_ARRAY, TYPE_DATE,
    TYPE_FUNCTION, TYPE_GENERATOR, TYPE_MAP, TYPE_OBJECT, TYPE_PROMISE, TYPE_SET, TYPE_STRING,
};

/// A decoded jsval: the in-scope runtime value types for the pipeline.
///
/// Not `Copy`: the `Str` variant owns a heap [`String`], so decoded values are
/// cloned rather than copied.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A JavaScript number (`f64`).
    Number(f64),
    /// A boolean (`true`/`false`).
    Bool(bool),
    /// The `null` value.
    Null,
    /// The `undefined` value.
    Undefined,
    /// A heap string, read from linear memory via a pointer jsval. A
    /// `typeof` result is an ordinary heap string too (there is no immediate
    /// encoding), so it decodes here.
    Str(String),
    /// A heap array, read from linear memory via an array-pointer jsval.
    /// Elements are decoded recursively, so arrays nest.
    Array(Vec<Value>),
    /// A heap object, read from linear memory via an object-pointer jsval: its
    /// own enumerable properties in own-key order (integer keys ascending,
    /// then the other keys in insertion order), values decoded recursively, so
    /// objects nest.
    Object(Vec<(String, Value)>),
    /// A branded Error object (one carrying the non-enumerable
    /// `@@errorkind` brand): its `name` and `message` strings. Renders as its
    /// single-line stack `"<name>: <message>"`.
    Error {
        /// The `name` property (`"Error"` when absent or not a string).
        name: String,
        /// The `message` property (empty when absent or not a string).
        message: String,
    },
    /// An accessor property's value as rendered inside an object: which
    /// halves (getter / setter) it has. Renders as `[Getter]`, `[Setter]`, or
    /// `[Getter/Setter]`.
    Accessor {
        /// Whether the property has a getter.
        getter: bool,
        /// Whether the property has a setter.
        setter: bool,
    },
    /// A first-class function value, read from linear memory via a
    /// function-pointer jsval (type byte `3`). The backing block holds a
    /// funcref-table index and an environment slot; a returned bare function
    /// value only needs to decode without panicking (it renders as
    /// `[Function (anonymous)]`).
    Function,
    /// A `Map` handle (type byte `6`). Only its logical entry count (`size`)
    /// is decoded — the backing entries are not walked — so it renders as `Map(N)`.
    Map(usize),
    /// A `Set` handle (type byte `7`). Like [`Value::Map`], only its `size`
    /// is decoded; it renders as `Set(N)`.
    Set(usize),
    /// A `Promise` (type byte `8`). `state` is the settlement kind
    /// (`0` pending / `1` fulfilled / `2` rejected) and `value` its settled
    /// fulfillment/rejection value (`undefined` while pending), decoded
    /// recursively. Renders Node-console style: `Promise { <pending> }`,
    /// `Promise { 42 }`, or `Promise { <rejected> reason }`.
    Promise {
        /// The settlement kind (`0`/`1`/`2`).
        state: i32,
        /// The settled value (meaningful when fulfilled/rejected).
        value: Box<Value>,
    },
    /// A `Generator` handle (type byte `9`). Only the handle is decoded —
    /// its state machine and frame are not walked — so it renders as
    /// `Object [Generator]`, matching Node's `console.log` of a generator object.
    Generator,
    /// A `Date` (type byte `10`). The f64 payload is the millisecond
    /// timestamp since the Unix epoch. Renders as an ISO 8601 UTC string
    /// (`1970-01-01T00:00:00.000Z`), or `Invalid Date` when the timestamp is not
    /// finite — matching Node's `console.log` of a `Date`.
    Date(f64),
    /// A back-reference to a container still being decoded on the current path
    /// (a cycle). Carries the cycle target's 1-based reference number and
    /// renders Node-console style as `[Circular *N]`.
    Circular(usize),
    /// A container that some [`Value::Circular`] inside it points back to:
    /// the reference number and the decoded container. Renders as
    /// `<ref *N> ` followed by the container.
    Ref(usize, Box<Value>),
}

/// Cycle-guard state for one [`decode`] call: the heap blocks on the current
/// recursion path and the cycle targets numbered so far (in discovery order,
/// matching Node's `*1`, `*2`, ... numbering).
struct Guard {
    path: Vec<usize>,
    refs: Vec<usize>,
}

impl Guard {
    /// The reference number of cycle target `ptr`, if one was assigned.
    fn ref_of(&self, ptr: usize) -> Option<usize> {
        self.refs.iter().position(|&p| p == ptr).map(|i| i + 1)
    }
}

/// Decode raw jsval `bits` into a [`Value`] per the NaN-box layout.
///
/// `memory` is the linear-memory snapshot from [`run`]; it is consulted only for
/// heap pointer jsvals (tag `0b101`). Pass `&[]` when decoding an immediate value.
///
/// [`run`]: super::run
#[must_use]
pub fn decode(bits: u64, memory: &[u8]) -> Value {
    decode_in(
        bits,
        memory,
        &mut Guard {
            path: Vec::new(),
            refs: Vec::new(),
        },
    )
}

/// [`decode`] under the cycle guard `guard`.
fn decode_in(bits: u64, memory: &[u8], guard: &mut Guard) -> Value {
    // Any value without the full QNAN mask set is an ordinary number (this also
    // covers infinities, whose exponent is all ones but quiet bit is clear).
    if bits & QNAN != QNAN {
        return Value::Number(f64::from_bits(bits));
    }
    // QNAN is set: the 3-bit tag in payload bits [50:48] discriminates.
    match (bits >> 48) & 0b111 {
        0b001 => Value::Bool(bits & 1 != 0),
        0b010 => Value::Null,
        0b011 => Value::Undefined,
        // Single heap tag: read the block's TYPE BYTE to pick the arm.
        0b101 => decode_heap(bits, memory, guard),
        // Tags 0b100, 0b110 and 0b111 (unused), and 0b000
        // (the canonical number NaN). Any such pattern decodes as a NaN number,
        // keeping decode total without a panic.
        _ => Value::Number(f64::from_bits(bits)),
    }
}

/// Resolve a heap pointer jsval (single tag `0b101`) into a [`Value`] by reading
/// the TYPE BYTE at the block base (the low 32 bits of `bits`) and dispatching to
/// the string / array / object / function decoder. A function block
/// decodes to [`Value::Function`]; an unknown or out-of-range type byte decodes
/// as `undefined`, keeping decode total.
///
/// Containers (array / object / promise) are cycle-guarded: a block already on
/// the recursion path decodes as [`Value::Circular`], and the block it points
/// back to is wrapped in [`Value::Ref`] once decoded.
fn decode_heap(bits: u64, memory: &[u8], guard: &mut Guard) -> Value {
    let ptr = (bits & 0xFFFF_FFFF) as usize;
    let ty = memory.get(ptr).copied();
    if matches!(ty, Some(TYPE_ARRAY | TYPE_OBJECT | TYPE_PROMISE)) {
        if guard.path.contains(&ptr) {
            let id = guard.ref_of(ptr).unwrap_or_else(|| {
                guard.refs.push(ptr);
                guard.refs.len()
            });
            return Value::Circular(id);
        }
        guard.path.push(ptr);
        let value = match ty {
            Some(TYPE_ARRAY) => decode_array(ptr, memory, guard),
            Some(TYPE_OBJECT) => decode_object(ptr, memory, guard),
            _ => decode_promise(ptr, memory, guard),
        };
        guard.path.pop();
        return match guard.ref_of(ptr) {
            Some(id) => Value::Ref(id, Box::new(value)),
            None => value,
        };
    }
    match ty {
        Some(TYPE_STRING) => decode_string(ptr, memory),
        Some(TYPE_FUNCTION) => Value::Function,
        Some(TYPE_MAP) => Value::Map(decode_size(ptr, memory)),
        Some(TYPE_SET) => Value::Set(decode_size(ptr, memory)),
        Some(TYPE_GENERATOR) => Value::Generator,
        Some(TYPE_DATE) => decode_date(ptr, memory),
        _ => Value::Undefined,
    }
}

/// Resolve a heap `Date` block at base `ptr` into a [`Value::Date`] by reading its
/// f64 `ms` payload. A truncated block reads as `Date(NaN)` (rendered
/// `Invalid Date`), keeping decode total.
fn decode_date(ptr: usize, memory: &[u8]) -> Value {
    let start = ptr + DATE_MS_OFFSET;
    let ms = memory
        .get(start..start + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map_or(f64::NAN, f64::from_le_bytes);
    Value::Date(ms)
}

/// Resolve a heap promise block at base `ptr` into a [`Value::Promise`]:
/// the i32 `state` at `ptr + PROMISE_STATE_OFFSET` and the i64 settled `value`
/// at `ptr + PROMISE_VALUE_OFFSET` (decoded recursively). Defensive: a truncated block
/// reads as pending with an `undefined` value.
fn decode_promise(ptr: usize, memory: &[u8], guard: &mut Guard) -> Value {
    let state_at = ptr + PROMISE_STATE_OFFSET;
    let state = memory
        .get(state_at..state_at + 4)
        .map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap_or([0; 4])));
    let val_at = ptr + PROMISE_VALUE_OFFSET;
    let value = memory
        .get(val_at..val_at + 8)
        .map_or(Value::Undefined, |b| {
            decode_in(
                u64::from_le_bytes(b.try_into().unwrap_or([0; 8])),
                memory,
                guard,
            )
        });
    Value::Promise {
        state,
        value: Box::new(value),
    }
}

/// Read the u32 `size` field of a Map/Set handle block at base `ptr` (at
/// `ptr + MAP_SIZE_OFFSET`), the logical entry count. Defensive: an out-of-range
/// header yields `0`, so a malformed pointer never panics.
fn decode_size(ptr: usize, memory: &[u8]) -> usize {
    let at = ptr + MAP_SIZE_OFFSET;
    memory.get(at..at + 4).map_or(0, |b| {
        u32::from_le_bytes(b.try_into().unwrap_or([0; 4])) as usize
    })
}

/// Resolve a heap string block at base `ptr` into a [`Value::Str`] by reading
/// `[len:u32 LE][utf8]` at `ptr + HEADER_SIZE` in `memory`.
///
/// Defensive: any out-of-range slice yields an empty string and invalid UTF-8 is
/// replaced lossily, so a malformed pointer never panics (decode stays total).
fn decode_string(ptr: usize, memory: &[u8]) -> Value {
    let len_at = ptr + HEADER_SIZE;
    let Some(len_bytes) = memory.get(len_at..len_at + 4) else {
        return Value::Str(String::new());
    };
    let len = u32::from_le_bytes(len_bytes.try_into().unwrap_or([0; 4])) as usize;
    let start = len_at + 4;
    let Some(str_bytes) = memory.get(start..start + len) else {
        return Value::Str(String::new());
    };
    Value::Str(String::from_utf8_lossy(str_bytes).into_owned())
}

/// Resolve a heap array block at base `ptr` into a [`Value::Array`] by reading the
/// u32 `len` header at `ptr + HEADER_SIZE`, then decoding `len` consecutive i64
/// element jsvals at `ptr + HEADER_SIZE + 4 + 8*k` (each recursively). Nested
/// arrays and strings resolve through the same `memory`.
///
/// Defensive: an out-of-range header yields an empty array and a truncated
/// element run stops early, so a malformed pointer never panics.
fn decode_array(ptr: usize, memory: &[u8], guard: &mut Guard) -> Value {
    let len_at = ptr + HEADER_SIZE;
    let Some(len_bytes) = memory.get(len_at..len_at + 4) else {
        return Value::Array(Vec::new());
    };
    let len = u32::from_le_bytes(len_bytes.try_into().unwrap_or([0; 4])) as usize;
    let mut elems = Vec::with_capacity(len);
    for k in 0..len {
        let start = len_at + 4 + 8 * k;
        let Some(elem_bytes) = memory.get(start..start + 8) else {
            break;
        };
        let elem_bits = u64::from_le_bytes(elem_bytes.try_into().unwrap_or([0; 8]));
        elems.push(decode_in(elem_bits, memory, guard));
    }
    Value::Array(elems)
}

/// Read the little-endian u32 at `at`, or `None` when out of range.
fn read_u32(memory: &[u8], at: usize) -> Option<u32> {
    let bytes = memory.get(at..at + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Read the little-endian u64 at `at`, or `None` when out of range.
fn read_u64(memory: &[u8], at: usize) -> Option<u64> {
    let bytes = memory.get(at..at + 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// Resolve a heap object block at base `ptr` by walking its `TYPE_PROPS` table
/// (at the `props` slot): tombstones and non-enumerable entries are skipped,
/// integer keys (strings flagged `ARRAY_INDEX`) come first in ascending order
/// when the table has `HAS_INDEX_KEYS`, then the other keys in slot order. An
/// accessor property decodes as [`Value::Accessor`]; an object carrying the
/// non-enumerable `@@errorkind` brand decodes as [`Value::Error`].
///
/// Defensive: a truncated table stops early, so a malformed pointer never
/// panics.
fn decode_object(ptr: usize, memory: &[u8], guard: &mut Guard) -> Value {
    use crate::abi;
    let props = read_u32(memory, ptr + abi::OBJ_PROPS_OFFSET as usize).unwrap_or(0) as usize;
    if props == 0 {
        return Value::Object(Vec::new());
    }
    let count = read_u32(memory, props + abi::PROPS_COUNT_OFFSET as usize).unwrap_or(0) as usize;
    let tflags = memory
        .get(props + abi::PROPS_TFLAGS_OFFSET as usize)
        .copied()
        .unwrap_or(0);
    let mut is_error = false;
    // (index value for an integer key, key text, attrs, value bits)
    let mut keyed: Vec<(Option<u32>, String, u32, u64)> = Vec::with_capacity(count);
    for k in 0..count {
        let entry = props + abi::PROPS_HEADER_SIZE as usize + abi::PROP_ENTRY_SIZE as usize * k;
        let (Some(key), Some(attrs), Some(bits)) = (
            read_u32(memory, entry + abi::PROP_KEY_OFFSET as usize),
            read_u32(memory, entry + abi::PROP_ATTRS_OFFSET as usize),
            read_u64(memory, entry + abi::PROP_VALUE_OFFSET as usize),
        ) else {
            break;
        };
        if key == 0 {
            continue;
        }
        let Value::Str(text) = decode_string(key as usize, memory) else {
            continue;
        };
        if text == ERROR_KIND_KEY {
            is_error = true;
        }
        if attrs & abi::ATTR_ENUMERABLE == 0 {
            continue;
        }
        let flags = memory
            .get(key as usize + abi::HEADER_FLAGS_OFFSET as usize)
            .copied()
            .unwrap_or(0);
        let index = (tflags & abi::PROPS_HAS_INDEX_KEYS != 0
            && flags & abi::STR_FLAG_ARRAY_INDEX != 0)
            .then(|| text.parse::<u32>().ok())
            .flatten();
        keyed.push((index, text, attrs, bits));
    }
    // A stable sort keeps the non-index keys in slot order after the indices.
    keyed.sort_by_key(|(index, ..)| index.map_or((1, 0), |i| (0, i)));
    let mut entries = Vec::with_capacity(keyed.len());
    for (_, key, attrs, bits) in keyed {
        let value = if attrs & abi::ATTR_ACCESSOR != 0 {
            let pair = (bits & 0xFFFF_FFFF) as usize;
            let half = |offset: u32| {
                read_u64(memory, pair + offset as usize).is_some_and(|b| b != abi::UNDEFINED)
            };
            Value::Accessor {
                getter: half(abi::ACCESSOR_GET_OFFSET),
                setter: half(abi::ACCESSOR_SET_OFFSET),
            }
        } else {
            decode_in(bits, memory, guard)
        };
        entries.push((key, value));
    }
    if is_error {
        let field = |name: &str| {
            entries.iter().find_map(|(key, value)| match value {
                Value::Str(s) if key == name => Some(s.clone()),
                _ => None,
            })
        };
        return Value::Error {
            name: field("name").unwrap_or_else(|| "Error".to_owned()),
            message: field("message").unwrap_or_default(),
        };
    }
    Value::Object(entries)
}

/// Render an `f64` the way JavaScript prints a number.
///
/// Integer-valued finite numbers are printed without a fractional part (`42.0`
/// becomes `"42"`, matching JS and keeping `40 + 2` => `"42"`); every other
/// value falls back to Rust's shortest round-tripping form (`42.5` => `"42.5"`).
#[must_use]
pub fn format_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        }
    } else if value == 0.0 {
        // Both +0 and -0 render as "0" (JS `String(-0)` is "0"); the stored
        // f64 sign is untouched so arithmetic still sees -0 (e.g. 1/-0 => -Infinity).
        "0".to_owned()
    } else {
        let neg = value < 0.0;
        let body = format_finite_positive(value.abs());
        if neg { format!("-{body}") } else { body }
    }
}

/// Render a finite, strictly positive `f64` per the ECMAScript `Number::toString`
/// digit rules (ECMA-262, `Number::toString`).
///
/// The spec picks integers `n`, `k`, `s` with `k >= 1`, `10^(k-1) <= s < 10^k`,
/// and `s * 10^(n-k) == x`, where `k` is minimal — i.e. `s` is the shortest
/// decimal digit string that round-trips. Rust's `{:e}` already emits that
/// shortest mantissa in scientific form (`d.ddd * 10^exp`), so we recover the
/// digit string and derive `n = exp + 1`, then apply the spec's fixed vs.
/// exponential threshold: fixed form for `-6 < n <= 21`, exponential otherwise
/// (i.e. values `>= 1e21` or `0 < x < 1e-6`).
fn format_finite_positive(value: f64) -> String {
    let sci = format!("{value:e}");
    // `{:e}` is always `<mantissa>e<exp>`; the `else` arms are unreachable but
    // keep the function panic-free (Lock 6) by falling back to the raw form.
    let Some((mantissa, exp)) = sci.split_once('e') else {
        return sci;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return sci;
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len();
    // `n` is the position of the decimal point relative to the first digit.
    let n = exp + 1;

    if (1..=21).contains(&n) {
        // `n >= 1` here, so the conversion never loses the value.
        let n = usize::try_from(n).unwrap_or(0);
        if n >= k {
            // Integer: all `k` digits, then `n - k` trailing zeros.
            let zeros = "0".repeat(n - k);
            format!("{digits}{zeros}")
        } else {
            // `0 < n < k`: decimal point after the first `n` digits.
            format!("{}.{}", &digits[..n], &digits[n..])
        }
    } else if (-5..=0).contains(&n) {
        // `-6 < n <= 0`: "0.", then `-n` leading zeros, then the digits.
        let zeros = "0".repeat(usize::try_from(-n).unwrap_or(0));
        format!("0.{zeros}{digits}")
    } else {
        // Exponential: `n > 21` or `n <= -6`.
        let e = n - 1;
        let sign = if e >= 0 { "+" } else { "-" };
        let mantissa = if k == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{mantissa}e{sign}{}", e.abs())
    }
}

/// Render a decoded [`Value`] as user-facing text, the way JavaScript prints it.
#[must_use]
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Number(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_owned(),
        Value::Undefined => "undefined".to_owned(),
        // A string renders as its raw text, with no surrounding quotes (a `typeof`
        // result is an ordinary string, so `console.log(typeof x)` prints the word).
        Value::Str(s) => s.clone(),
        // An array renders Node-console style: `[ 1, 2, 3 ]`, `[]` when empty.
        Value::Array(elems) => {
            if elems.is_empty() {
                "[]".to_owned()
            } else {
                let inner = elems
                    .iter()
                    .map(format_value)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[ {inner} ]")
            }
        }
        // An object renders Node-console style: `{ a: 1, b: 2 }`, `{}` when empty.
        Value::Object(entries) => {
            if entries.is_empty() {
                "{}".to_owned()
            } else {
                let inner = entries
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", format_value(value)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {inner} }}")
            }
        }
        // A branded Error object renders as its single-line stack
        // (`"<name>: <message>"`, or just the name without a message), the way
        // a JS console prints an error's first line.
        Value::Error { name, message } => {
            if message.is_empty() {
                name.clone()
            } else {
                format!("{name}: {message}")
            }
        }
        Value::Accessor { getter, setter } => match (getter, setter) {
            (true, true) => "[Getter/Setter]".to_owned(),
            (false, true) => "[Setter]".to_owned(),
            _ => "[Getter]".to_owned(),
        },
        // A function value renders Node-console style. jsc does not track a
        // function's source name through the value, so every function prints
        // anonymously.
        Value::Function => "[Function (anonymous)]".to_owned(),
        // A Map/Set renders with its entry count (the backing entries are not
        // decoded), e.g. `Map(2)` / `Set(0)`.
        Value::Map(size) => format!("Map({size})"),
        Value::Set(size) => format!("Set({size})"),
        // A promise renders Node-console style by settlement.
        Value::Promise { state, value } => {
            if *state == PROMISE_FULFILLED {
                format!("Promise {{ {} }}", format_value(value))
            } else if *state == PROMISE_REJECTED {
                format!("Promise {{ <rejected> {} }}", format_value(value))
            } else {
                "Promise { <pending> }".to_owned()
            }
        }
        // A generator handle renders Node-console style.
        Value::Generator => "Object [Generator]".to_owned(),
        // A Date renders as its ISO 8601 UTC string.
        Value::Date(ms) => format_date_iso(*ms),
        // Cycle markers render Node-console style.
        Value::Circular(id) => format!("[Circular *{id}]"),
        Value::Ref(id, value) => format!("<ref *{id}> {}", format_value(value)),
    }
}

/// Render a millisecond epoch timestamp as an ISO 8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`), matching `Date.prototype.toISOString` and Node's
/// `console.log` of a `Date`. A non-finite timestamp renders as `Invalid Date`.
/// The civil date is derived from the day count with Howard Hinnant's
/// `civil_from_days` algorithm; time-of-day fields come from the millisecond
/// remainder. `div_euclid`/`rem_euclid` keep the split correct for pre-epoch
/// (negative) timestamps.
#[allow(clippy::cast_possible_truncation)] // finite-guarded; `f64 as i64` saturates
fn format_date_iso(ms: f64) -> String {
    if !ms.is_finite() {
        return "Invalid Date".to_owned();
    }
    let total_ms = ms.floor() as i64;
    let days = total_ms.div_euclid(86_400_000);
    let mut tod = total_ms.rem_euclid(86_400_000); // ms into the day, 0..86_400_000
    let millis = tod % 1000;
    tod /= 1000;
    let secs = tod % 60;
    tod /= 60;
    let mins = tod % 60;
    let hours = tod / 60; // 0..23

    // civil_from_days: `days` are days since 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era, [0, 146_096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year (Mar 1 start), [0, 365]
    let mp = (5 * doy + 2) / 153; // month shifted so Mar=0, [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{secs:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::format_number;

    /// `format_number` follows the ECMAScript `Number::toString` fixed vs.
    /// exponential thresholds: exponential for magnitudes `>= 1e21` or
    /// `0 < |x| < 1e-6`, fixed form (shortest round-tripping digits) otherwise.
    #[test]
    fn exponential_thresholds() {
        // Exponential form with lowercase `e`, explicit sign, no leading zeros.
        assert_eq!(format_number(1e21), "1e+21");
        assert_eq!(format_number(1e-7), "1e-7");
        assert_eq!(format_number(1.5e22), "1.5e+22");
        assert_eq!(format_number(-1e21), "-1e+21");
        assert_eq!(format_number(5e-7), "5e-7");
        // Fixed form on either side of the boundary and across the range.
        assert_eq!(format_number(1e-6), "0.000001");
        assert_eq!(format_number(123.0), "123");
        assert_eq!(format_number(100_000.0), "100000");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(4.5), "4.5");
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(-5.0), "-5");
    }
}
