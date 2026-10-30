//! Drift guard for `packages/jsc/src/abi.ts`, the browser shim's hand-ported copy
//! of this module. Every `export const` there must map to the Rust value of the
//! same meaning in `expected()`; an unmapped export, a mapped name missing from
//! the TS file, or a differing value fails the test.

use super::{
    ACCESSOR_GET_OFFSET, ACCESSOR_SET_OFFSET, ARRAY_ELEMS_OFFSET, ARRAY_FLAG_SPARSE,
    ARRAY_LEN_OFFSET, ATTR_ACCESSOR, ATTR_ENUMERABLE, CLASS_ARGUMENTS, CLASS_BOOLEAN, CLASS_ERROR,
    CLASS_NUMBER, CLASS_STRING, ELEMS_CAP_OFFSET, ELEMS_DATA_OFFSET, HEADER_CLASS_OFFSET,
    HEADER_FLAGS_OFFSET, METHOD_OP_NUMBER_TO_STRING, OBJ_PROPS_OFFSET, PROP_ATTRS_OFFSET,
    PROP_ENTRY_SIZE, PROP_KEY_OFFSET, PROP_VALUE_OFFSET, PROPS_COUNT_OFFSET, PROPS_HAS_INDEX_KEYS,
    PROPS_HEADER_SIZE, PROPS_TFLAGS_OFFSET, STR_DATA_OFFSET, STR_FLAG_ARRAY_INDEX,
    STR_FLAG_TWO_BYTE, STR_LEN_OFFSET, WRAPPER_VALUE_OFFSET,
};
use super::{
    ASYNC_STEP_EXPORT_NAME, CONVERT_OP_PARSE_FLOAT, CONVERT_OP_TO_NUMBER, CONVERT_OP_TO_STRING,
    DATE_MS_OFFSET, EXC_PENDING_EXPORT_NAME, EXC_VALUE_EXPORT_NAME, FALSE, GEN_DONE_OFFSET,
    HEADER_SIZE, HEAP, HEAP_PTR_EXPORT_NAME, HEAP_PTR_OOM, HOLE, INVOKE_EXPORT_NAME,
    MEMORY_EXPORT_NAME, MEMORY_MAX_PAGES, METHOD_OP_INCLUDES, METHOD_OP_INDEX_OF,
    METHOD_OP_LOCALE_COMPARE, METHOD_OP_NORMALIZE, METHOD_OP_TO_LOWER, METHOD_OP_TO_UPPER,
    METHOD_RANGE_ERROR, NEW_ARRAY_EXPORT_NAME, NEW_PROMISE_EXPORT_NAME, NEW_STRING_EXPORT_NAME,
    NULL, PRINT_FLAG_END, PRINT_FLAG_NEWLINE, PRINT_FLAG_SEP, PROMISE_FULFILLED,
    PROMISE_OP_ASYNC_SPAWN, PROMISE_OP_AWAIT, PROMISE_OP_FINALLY, PROMISE_OP_NEW,
    PROMISE_OP_REJECT, PROMISE_OP_RESOLVE, PROMISE_OP_SETTLE_FULFILL, PROMISE_OP_SETTLE_REJECT,
    PROMISE_PENDING, PROMISE_REJECTED, PROMISE_SELF_ERROR_EXPORT_NAME, QNAN, TRUE, TYPE_ARRAY,
    TYPE_DATE, TYPE_FUNCTION, TYPE_OBJECT, TYPE_PROMISE, TYPE_STRING, UNDEFINED,
};
use super::{
    CLASS_ITERATOR, CLASS_SYMBOL, PROPS_HAS_SYMBOL_KEYS, SYMBOL_BLOCK_SIZE,
    SYMBOL_DESCRIPTION_OFFSET, SYMBOL_FLAG_REGISTERED, TYPE_SYMBOL,
};
use super::{
    FN_INFO_ADDR, FN_INFO_ENTRY_SIZE, FN_INFO_NAME_OFFSET, FUNC_FLAG_BOUND, FUNC_FLAG_CLASS_CTOR,
    FUNC_FLAG_CONSTRUCTOR, FUNC_FLAG_SELF_ALLOC, FUNC_INDEX_OFFSET, MAP_SIZE_OFFSET,
    OBJ_PREFIX_SIZE, OBJ_PROTO_OFFSET, PROMISE_BLOCK_SIZE, PROMISE_STATE_OFFSET,
    PROMISE_VALUE_OFFSET, TYPE_GENERATOR, TYPE_MAP, TYPE_SET,
};
use super::{
    MATH_FN_ACOS, MATH_FN_ACOSH, MATH_FN_ASIN, MATH_FN_ASINH, MATH_FN_ATAN, MATH_FN_ATAN2,
    MATH_FN_ATANH, MATH_FN_CBRT, MATH_FN_COS, MATH_FN_COSH, MATH_FN_EXP, MATH_FN_EXPM1,
    MATH_FN_LOG, MATH_FN_LOG1P, MATH_FN_LOG2, MATH_FN_LOG10, MATH_FN_RANDOM, MATH_FN_SIN,
    MATH_FN_SINH, MATH_FN_TAN, MATH_FN_TANH, METHOD_OP_MATH, METHOD_OP_NUMBER_TO_EXPONENTIAL,
    METHOD_OP_NUMBER_TO_FIXED, METHOD_OP_NUMBER_TO_PRECISION,
};

/// A parsed `export const` value.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Int(i128),
    Str(String),
    Strs(Vec<String>),
}

fn int(v: impl Into<i128>) -> Val {
    Val::Int(v.into())
}

fn s(v: &str) -> Val {
    Val::Str(v.to_owned())
}

/// TS export name -> the Rust value it must equal.
#[allow(clippy::too_many_lines)] // one flat table of name/value pairs
fn expected() -> Vec<(&'static str, Val)> {
    vec![
        ("QNAN", int(QNAN)),
        ("FALSE", int(FALSE)),
        ("TRUE", int(TRUE)),
        ("NULL", int(NULL)),
        ("UNDEFINED", int(UNDEFINED)),
        ("HOLE", int(HOLE)),
        ("HEAP", int(HEAP)),
        ("HEADER_SIZE", int(HEADER_SIZE)),
        ("TYPE_STRING", int(TYPE_STRING)),
        ("TYPE_ARRAY", int(TYPE_ARRAY)),
        ("TYPE_OBJECT", int(TYPE_OBJECT)),
        ("TYPE_FUNCTION", int(TYPE_FUNCTION)),
        ("TYPE_MAP", int(TYPE_MAP)),
        ("TYPE_SET", int(TYPE_SET)),
        ("TYPE_PROMISE", int(TYPE_PROMISE)),
        ("TYPE_GENERATOR", int(TYPE_GENERATOR)),
        ("TYPE_DATE", int(TYPE_DATE)),
        ("TYPE_SYMBOL", int(TYPE_SYMBOL)),
        ("SYMBOL_DESCRIPTION_OFFSET", int(SYMBOL_DESCRIPTION_OFFSET)),
        ("SYMBOL_BLOCK_SIZE", int(SYMBOL_BLOCK_SIZE)),
        ("SYMBOL_FLAG_REGISTERED", int(SYMBOL_FLAG_REGISTERED)),
        ("HEADER_FLAGS_OFFSET", int(HEADER_FLAGS_OFFSET)),
        ("HEADER_CLASS_OFFSET", int(HEADER_CLASS_OFFSET)),
        ("CLASS_ERROR", int(CLASS_ERROR)),
        ("CLASS_ARGUMENTS", int(CLASS_ARGUMENTS)),
        ("CLASS_BOOLEAN", int(CLASS_BOOLEAN)),
        ("CLASS_NUMBER", int(CLASS_NUMBER)),
        ("CLASS_STRING", int(CLASS_STRING)),
        ("CLASS_SYMBOL", int(CLASS_SYMBOL)),
        ("CLASS_ITERATOR", int(CLASS_ITERATOR)),
        ("WRAPPER_VALUE_OFFSET", int(WRAPPER_VALUE_OFFSET)),
        ("STR_FLAG_ARRAY_INDEX", int(STR_FLAG_ARRAY_INDEX)),
        ("STR_FLAG_TWO_BYTE", int(STR_FLAG_TWO_BYTE)),
        ("STR_LEN_OFFSET", int(STR_LEN_OFFSET)),
        ("STR_DATA_OFFSET", int(STR_DATA_OFFSET)),
        ("ARRAY_FLAG_SPARSE", int(ARRAY_FLAG_SPARSE)),
        ("ELEMS_CAP_OFFSET", int(ELEMS_CAP_OFFSET)),
        ("OBJ_PROPS_OFFSET", int(OBJ_PROPS_OFFSET)),
        ("OBJ_PREFIX_SIZE", int(OBJ_PREFIX_SIZE)),
        ("OBJ_PROTO_OFFSET", int(OBJ_PROTO_OFFSET)),
        ("ARRAY_LEN_OFFSET", int(ARRAY_LEN_OFFSET)),
        ("ARRAY_ELEMS_OFFSET", int(ARRAY_ELEMS_OFFSET)),
        ("ELEMS_DATA_OFFSET", int(ELEMS_DATA_OFFSET)),
        ("FUNC_INDEX_OFFSET", int(FUNC_INDEX_OFFSET)),
        ("FUNC_FLAG_CONSTRUCTOR", int(FUNC_FLAG_CONSTRUCTOR)),
        ("FUNC_FLAG_CLASS_CTOR", int(FUNC_FLAG_CLASS_CTOR)),
        ("FUNC_FLAG_SELF_ALLOC", int(FUNC_FLAG_SELF_ALLOC)),
        ("FUNC_FLAG_BOUND", int(FUNC_FLAG_BOUND)),
        ("FN_INFO_ADDR", int(FN_INFO_ADDR)),
        ("FN_INFO_ENTRY_SIZE", int(FN_INFO_ENTRY_SIZE)),
        ("FN_INFO_NAME_OFFSET", int(FN_INFO_NAME_OFFSET)),
        ("PROPS_TFLAGS_OFFSET", int(PROPS_TFLAGS_OFFSET)),
        ("PROPS_COUNT_OFFSET", int(PROPS_COUNT_OFFSET)),
        ("PROPS_HEADER_SIZE", int(PROPS_HEADER_SIZE)),
        ("PROP_ENTRY_SIZE", int(PROP_ENTRY_SIZE)),
        ("PROP_KEY_OFFSET", int(PROP_KEY_OFFSET)),
        ("PROP_ATTRS_OFFSET", int(PROP_ATTRS_OFFSET)),
        ("PROP_VALUE_OFFSET", int(PROP_VALUE_OFFSET)),
        ("PROPS_HAS_INDEX_KEYS", int(PROPS_HAS_INDEX_KEYS)),
        ("PROPS_HAS_SYMBOL_KEYS", int(PROPS_HAS_SYMBOL_KEYS)),
        ("ATTR_ENUMERABLE", int(ATTR_ENUMERABLE)),
        ("ATTR_ACCESSOR", int(ATTR_ACCESSOR)),
        ("ACCESSOR_GET_OFFSET", int(ACCESSOR_GET_OFFSET)),
        ("ACCESSOR_SET_OFFSET", int(ACCESSOR_SET_OFFSET)),
        ("PROMISE_PENDING", int(PROMISE_PENDING)),
        ("PROMISE_FULFILLED", int(PROMISE_FULFILLED)),
        ("PROMISE_REJECTED", int(PROMISE_REJECTED)),
        ("MAP_SIZE_OFFSET", int(MAP_SIZE_OFFSET)),
        ("PROMISE_STATE_OFFSET", int(PROMISE_STATE_OFFSET)),
        ("PROMISE_VALUE_OFFSET", int(PROMISE_VALUE_OFFSET)),
        ("PROMISE_BLOCK_SIZE", int(PROMISE_BLOCK_SIZE)),
        ("GEN_DONE_OFFSET", int(GEN_DONE_OFFSET)),
        ("DATE_MS_OFFSET", int(DATE_MS_OFFSET)),
        ("MEMORY_EXPORT_NAME", s(MEMORY_EXPORT_NAME)),
        ("HEAP_PTR_EXPORT_NAME", s(HEAP_PTR_EXPORT_NAME)),
        ("MEMORY_MAX_PAGES", int(MEMORY_MAX_PAGES)),
        ("HEAP_PTR_OOM", int(HEAP_PTR_OOM)),
        ("NEW_STRING_EXPORT_NAME", s(NEW_STRING_EXPORT_NAME)),
        ("NEW_ARRAY_EXPORT_NAME", s(NEW_ARRAY_EXPORT_NAME)),
        ("NEW_PROMISE_EXPORT_NAME", s(NEW_PROMISE_EXPORT_NAME)),
        (
            "PROMISE_SELF_ERROR_EXPORT_NAME",
            s(PROMISE_SELF_ERROR_EXPORT_NAME),
        ),
        ("EXC_PENDING_EXPORT_NAME", s(EXC_PENDING_EXPORT_NAME)),
        ("EXC_VALUE_EXPORT_NAME", s(EXC_VALUE_EXPORT_NAME)),
        ("INVOKE_EXPORT_NAME", s(INVOKE_EXPORT_NAME)),
        ("ASYNC_STEP_EXPORT_NAME", s(ASYNC_STEP_EXPORT_NAME)),
        ("PRINT_FLAG_SEP", int(PRINT_FLAG_SEP)),
        ("PRINT_FLAG_END", int(PRINT_FLAG_END)),
        ("PRINT_FLAG_NEWLINE", int(PRINT_FLAG_NEWLINE)),
        ("CONVERT_OP_TO_STRING", int(CONVERT_OP_TO_STRING)),
        ("CONVERT_OP_TO_NUMBER", int(CONVERT_OP_TO_NUMBER)),
        ("CONVERT_OP_PARSE_FLOAT", int(CONVERT_OP_PARSE_FLOAT)),
        ("METHOD_OP_INDEX_OF", int(METHOD_OP_INDEX_OF)),
        ("METHOD_OP_INCLUDES", int(METHOD_OP_INCLUDES)),
        ("METHOD_OP_TO_UPPER", int(METHOD_OP_TO_UPPER)),
        ("METHOD_OP_TO_LOWER", int(METHOD_OP_TO_LOWER)),
        (
            "METHOD_OP_NUMBER_TO_STRING",
            int(METHOD_OP_NUMBER_TO_STRING),
        ),
        ("METHOD_OP_LOCALE_COMPARE", int(METHOD_OP_LOCALE_COMPARE)),
        ("METHOD_OP_NORMALIZE", int(METHOD_OP_NORMALIZE)),
        ("METHOD_RANGE_ERROR", int(METHOD_RANGE_ERROR)),
        ("METHOD_OP_NUMBER_TO_FIXED", int(METHOD_OP_NUMBER_TO_FIXED)),
        (
            "METHOD_OP_NUMBER_TO_EXPONENTIAL",
            int(METHOD_OP_NUMBER_TO_EXPONENTIAL),
        ),
        (
            "METHOD_OP_NUMBER_TO_PRECISION",
            int(METHOD_OP_NUMBER_TO_PRECISION),
        ),
        ("METHOD_OP_MATH", int(METHOD_OP_MATH)),
        ("MATH_FN_CBRT", int(MATH_FN_CBRT)),
        ("MATH_FN_LOG", int(MATH_FN_LOG)),
        ("MATH_FN_LOG2", int(MATH_FN_LOG2)),
        ("MATH_FN_LOG10", int(MATH_FN_LOG10)),
        ("MATH_FN_LOG1P", int(MATH_FN_LOG1P)),
        ("MATH_FN_EXP", int(MATH_FN_EXP)),
        ("MATH_FN_EXPM1", int(MATH_FN_EXPM1)),
        ("MATH_FN_SIN", int(MATH_FN_SIN)),
        ("MATH_FN_COS", int(MATH_FN_COS)),
        ("MATH_FN_TAN", int(MATH_FN_TAN)),
        ("MATH_FN_ASIN", int(MATH_FN_ASIN)),
        ("MATH_FN_ACOS", int(MATH_FN_ACOS)),
        ("MATH_FN_ATAN", int(MATH_FN_ATAN)),
        ("MATH_FN_ATAN2", int(MATH_FN_ATAN2)),
        ("MATH_FN_SINH", int(MATH_FN_SINH)),
        ("MATH_FN_COSH", int(MATH_FN_COSH)),
        ("MATH_FN_TANH", int(MATH_FN_TANH)),
        ("MATH_FN_ASINH", int(MATH_FN_ASINH)),
        ("MATH_FN_ACOSH", int(MATH_FN_ACOSH)),
        ("MATH_FN_ATANH", int(MATH_FN_ATANH)),
        ("MATH_FN_RANDOM", int(MATH_FN_RANDOM)),
        ("PROMISE_OP_RESOLVE", int(PROMISE_OP_RESOLVE)),
        ("PROMISE_OP_REJECT", int(PROMISE_OP_REJECT)),
        ("PROMISE_OP_AWAIT", int(PROMISE_OP_AWAIT)),
        ("PROMISE_OP_FINALLY", int(PROMISE_OP_FINALLY)),
        ("PROMISE_OP_NEW", int(PROMISE_OP_NEW)),
        ("PROMISE_OP_SETTLE_FULFILL", int(PROMISE_OP_SETTLE_FULFILL)),
        ("PROMISE_OP_SETTLE_REJECT", int(PROMISE_OP_SETTLE_REJECT)),
        ("PROMISE_OP_ASYNC_SPAWN", int(PROMISE_OP_ASYNC_SPAWN)),
        // TS-only bigint normalization masks: the Rust side uses the native widths.
        ("U64_MASK", int(u64::MAX)),
        ("U32_MASK", int(u32::MAX)),
    ]
}

/// Parse an integer literal: decimal or `0x` hex, `_` separators, optional `n`.
fn parse_int(lit: &str) -> Option<i128> {
    let digits: String = lit
        .strip_suffix('n')
        .unwrap_or(lit)
        .chars()
        .filter(|&c| c != '_')
        .collect();
    match digits.strip_prefix("0x") {
        Some(hex) => i128::from_str_radix(hex, 16).ok(),
        None => digits.parse().ok(),
    }
}

/// Parse a `'...'` string literal (no escapes in abi.ts).
fn parse_str(lit: &str) -> Option<String> {
    lit.strip_prefix('\'')?
        .strip_suffix('\'')
        .map(str::to_owned)
}

/// Evaluate `term (+ term)*`, each term an integer literal or an earlier export.
fn parse_sum(expr: &str, seen: &[(String, Val)]) -> Result<Val, String> {
    let mut sum = 0i128;
    for term in expr.split('+').map(str::trim) {
        let value = match parse_int(term) {
            Some(v) => v,
            None => match seen.iter().find(|(n, _)| n == term) {
                Some((_, Val::Int(v))) => *v,
                _ => return Err(format!("unsupported term `{term}` in `{expr}`")),
            },
        };
        sum += value;
    }
    Ok(Val::Int(sum))
}

/// Line-based parse of every `export const NAME = <literal>` in `src`.
fn parse_exports(src: &str) -> Result<Vec<(String, Val)>, String> {
    let mut out: Vec<(String, Val)> = Vec::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let Some(decl) = line.strip_prefix("export const ") else {
            continue;
        };
        let (name, rhs) = decl
            .split_once(" = ")
            .ok_or_else(|| format!("malformed export: `{line}`"))?;
        let rhs = rhs.trim();
        let value = if rhs == "[" {
            let mut items = Vec::new();
            loop {
                let item = lines
                    .next()
                    .ok_or_else(|| format!("unterminated array `{name}`"))?
                    .trim();
                if item.starts_with(']') {
                    break;
                }
                let lit = item.trim_end_matches(',');
                items.push(
                    parse_str(lit).ok_or_else(|| format!("bad array item `{lit}` in `{name}`"))?,
                );
            }
            Val::Strs(items)
        } else if let Some(text) = parse_str(rhs) {
            Val::Str(text)
        } else {
            parse_sum(rhs, &out)?
        };
        out.push((name.to_owned(), value));
    }
    Ok(out)
}

#[test]
fn abi_ts_matches_rust_abi() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/jsc/src/abi.ts");
    let src = std::fs::read_to_string(path).unwrap();
    let exports = parse_exports(&src).unwrap();
    assert!(!exports.is_empty(), "parsed zero constants from {path}");

    let expected = expected();
    let mut errors = Vec::new();
    for (name, value) in &exports {
        match expected.iter().find(|(n, _)| n == name) {
            None => errors.push(format!(
                "abi.ts exports `{name}` with no Rust mapping in abi_drift_tests.rs"
            )),
            Some((_, want)) if want != value => {
                errors.push(format!(
                    "abi.ts `{name}` = {value:?}, Rust abi.rs = {want:?}"
                ));
            }
            Some(_) => {}
        }
    }
    for (name, _) in &expected {
        if !exports.iter().any(|(n, _)| n == name) {
            errors.push(format!("mapped `{name}` is missing from abi.ts"));
        }
    }
    assert!(
        errors.is_empty(),
        "abi.ts drifted from abi.rs:\n{}",
        errors.join("\n")
    );
}

/// Pins the object-family prefix and per-kind payload layouts to
/// `DESIGN.md` sections 5.1-5.2.
#[test]
fn object_family_layout_matches_design() {
    use super::{
        ARRAY_BLOCK_SIZE, DATE_BLOCK_SIZE, ELEMS_CAP_OFFSET, FUNC_BLOCK_SIZE, FUNC_ENV_OFFSET,
        GEN_BLOCK_SIZE, GEN_ENV_OFFSET, GEN_SM_IDX_OFFSET, GEN_STATE_OFFSET, MAP_BLOCK_SIZE,
        MAP_ENTRIES_OFFSET, OBJ_PROPS_OFFSET, TYPE_ELEMS,
    };
    let actual = [
        ("OBJ_PROPS_OFFSET", OBJ_PROPS_OFFSET),
        ("OBJ_PROTO_OFFSET", OBJ_PROTO_OFFSET),
        ("OBJ_PREFIX_SIZE", OBJ_PREFIX_SIZE),
        ("FUNC_INDEX_OFFSET", FUNC_INDEX_OFFSET),
        ("FUNC_ENV_OFFSET", FUNC_ENV_OFFSET),
        ("FUNC_BLOCK_SIZE", FUNC_BLOCK_SIZE),
        ("MAP_SIZE_OFFSET", MAP_SIZE_OFFSET),
        ("MAP_ENTRIES_OFFSET", MAP_ENTRIES_OFFSET),
        ("MAP_BLOCK_SIZE", MAP_BLOCK_SIZE),
        ("PROMISE_STATE_OFFSET", PROMISE_STATE_OFFSET),
        ("PROMISE_VALUE_OFFSET", PROMISE_VALUE_OFFSET),
        ("PROMISE_BLOCK_SIZE", PROMISE_BLOCK_SIZE),
        ("GEN_SM_IDX_OFFSET", GEN_SM_IDX_OFFSET),
        ("GEN_STATE_OFFSET", GEN_STATE_OFFSET),
        ("GEN_DONE_OFFSET", GEN_DONE_OFFSET),
        ("GEN_ENV_OFFSET", GEN_ENV_OFFSET),
        ("GEN_BLOCK_SIZE", GEN_BLOCK_SIZE),
        ("DATE_MS_OFFSET", DATE_MS_OFFSET),
        ("DATE_BLOCK_SIZE", DATE_BLOCK_SIZE),
        ("ARRAY_LEN_OFFSET", ARRAY_LEN_OFFSET),
        ("ARRAY_ELEMS_OFFSET", ARRAY_ELEMS_OFFSET),
        ("ARRAY_BLOCK_SIZE", ARRAY_BLOCK_SIZE),
        ("ELEMS_CAP_OFFSET", ELEMS_CAP_OFFSET),
        ("ELEMS_DATA_OFFSET", ELEMS_DATA_OFFSET),
    ];
    let design = [
        4, 8, 16, // prefix
        16, 24, 32, // function
        16, 24, 32, // map / set
        16, 24, 32, // promise
        16, 20, 24, 32, 48, // generator
        16, 24, // date
        16, 20, 24, // array handle
        4, 8, // element store
    ];
    for ((name, got), want) in actual.iter().zip(design) {
        assert_eq!(*got, want, "{name}");
    }
    // Every i64/f64 slot is 8-byte aligned; every block size is a BLOCK_ALIGN multiple.
    assert_eq!(TYPE_ELEMS, 13);
    for slot in [
        ELEMS_DATA_OFFSET,
        OBJ_PROTO_OFFSET,
        FUNC_ENV_OFFSET,
        MAP_ENTRIES_OFFSET,
        PROMISE_VALUE_OFFSET,
        GEN_ENV_OFFSET,
        DATE_MS_OFFSET,
    ] {
        assert_eq!(slot % 8, 0);
    }
    for size in [
        ARRAY_BLOCK_SIZE,
        FUNC_BLOCK_SIZE,
        MAP_BLOCK_SIZE,
        PROMISE_BLOCK_SIZE,
        GEN_BLOCK_SIZE,
        DATE_BLOCK_SIZE,
    ] {
        assert_eq!(size % super::BLOCK_ALIGN, 0);
    }
}

/// Pins the `TYPE_PROPS` table layout, attribute bits, and flag bits to
/// `DESIGN.md` sections 5.1-5.3.
#[test]
fn property_table_layout_matches_design() {
    use super::{
        ARRAY_FLAG_DICTIONARY, ARRAY_FLAG_JOINING, ARRAY_FLAG_LENGTH_READONLY, ATTR_CONFIGURABLE,
        ATTR_WRITABLE, ATTRS_DEFAULT, OBJ_FLAG_NOT_EXTENSIBLE, PROPS_CAP_OFFSET,
        PROPS_HAS_NON_ENUM, PROPS_LIVE_OFFSET, STR_FLAG_INTERNED, TYPE_PROPS,
    };
    assert_eq!(TYPE_PROPS, 12);
    assert_eq!(
        [
            PROPS_TFLAGS_OFFSET,
            PROPS_COUNT_OFFSET,
            PROPS_CAP_OFFSET,
            PROPS_LIVE_OFFSET,
            PROPS_HEADER_SIZE
        ],
        [2, 4, 8, 12, 16]
    );
    assert_eq!(
        [
            PROP_ENTRY_SIZE,
            PROP_KEY_OFFSET,
            PROP_ATTRS_OFFSET,
            PROP_VALUE_OFFSET
        ],
        [16, 0, 4, 8]
    );
    assert_eq!(
        [
            ATTR_WRITABLE,
            ATTR_ENUMERABLE,
            ATTR_CONFIGURABLE,
            ATTR_ACCESSOR
        ],
        [1, 2, 4, 8]
    );
    assert_eq!(ATTRS_DEFAULT, 0b0111);
    assert_eq!(
        [
            PROPS_HAS_INDEX_KEYS,
            PROPS_HAS_NON_ENUM,
            super::PROPS_WATCHED,
            PROPS_HAS_SYMBOL_KEYS
        ],
        [1, 2, 4, 8]
    );
    assert_eq!(
        [STR_FLAG_INTERNED, STR_FLAG_ARRAY_INDEX, STR_FLAG_TWO_BYTE],
        [1, 2, 4]
    );
    assert_eq!(OBJ_FLAG_NOT_EXTENSIBLE, 1);
    assert_eq!(
        [
            ARRAY_FLAG_SPARSE,
            ARRAY_FLAG_JOINING,
            ARRAY_FLAG_LENGTH_READONLY,
            ARRAY_FLAG_DICTIONARY
        ],
        [4, 8, 16, 32]
    );
    assert_eq!(HEADER_FLAGS_OFFSET, 2);
}

/// Pins the `TYPE_SYMBOL` block layout and the `typeof` word table to
/// `design/symbol-strings.md` sections 2.2 and 2.4.
#[test]
fn symbol_layout_matches_design() {
    use super::{BLOCK_ALIGN, CLASS_GLOBAL, STR_FLAG_INTERNED, TYPE_ELEMS, TYPEOF_STRINGS};
    assert_eq!(TYPE_SYMBOL, TYPE_ELEMS + 1);
    assert_eq!([SYMBOL_DESCRIPTION_OFFSET, SYMBOL_BLOCK_SIZE], [8, 16]);
    assert_eq!(SYMBOL_BLOCK_SIZE % BLOCK_ALIGN, 0);
    // Bits 1 and 2 (the string INTERNED / ARRAY_INDEX bits, tested on any key
    // block) stay clear. Bit 4 is shared with STR_FLAG_TWO_BYTE, which only
    // string paths read, after the TYPE byte says TYPE_STRING.
    assert_eq!(SYMBOL_FLAG_REGISTERED, 4);
    assert_eq!(
        SYMBOL_FLAG_REGISTERED & (STR_FLAG_ARRAY_INDEX | STR_FLAG_INTERNED),
        0
    );
    assert_eq!(SYMBOL_FLAG_REGISTERED, STR_FLAG_TWO_BYTE);
    assert_ne!(TYPE_SYMBOL, TYPE_STRING);
    assert_eq!(CLASS_SYMBOL, CLASS_GLOBAL + 1);
    // Section 5.4: the iterator objects' class byte.
    assert_eq!(CLASS_ITERATOR, CLASS_SYMBOL + 1);
    assert_eq!(TYPEOF_STRINGS[6], "symbol");
}
