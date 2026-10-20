use super::{
    TYPE_ARRAY, TYPE_FUNCTION, TYPE_OBJECT, TYPE_STRING, Value, compile_and_run, decode,
    format_number, format_value,
};

/// Assert a compiled program yields `Value::Number(expected)` (epsilon
/// compare, since `clippy::float_cmp` forbids `==` on `f64`).
fn assert_number(src: &str, expected: f64) {
    let value = compile_and_run(src).expect("pipeline should succeed");
    assert!(
        matches!(value, Value::Number(n) if (n - expected).abs() < 1e-9),
        "expected number {expected}, got {value:?} (from {src:?})"
    );
}

#[test]
fn addition_mvp() {
    assert_number("40 + 2", 42.0);
}

#[test]
fn renders_integer_result_without_decimal() {
    // The whole numeric pipeline is f64; `40 + 2` computes 42.0 and must
    // render as "42" (no ".0"), matching JS number formatting.
    let value = compile_and_run("40 + 2").expect("pipeline should succeed");
    assert_eq!(format_value(&value), "42");
}

#[test]
fn renders_non_integer_result() {
    let value = compile_and_run("1 / 2").expect("pipeline should succeed");
    assert_eq!(format_value(&value), "0.5");
}

#[test]
fn locals() {
    // A declaration plus a reference to the local.
    assert_number("let x = 40; x + 2", 42.0);
}

#[test]
fn assignment_updates_local() {
    assert_number("let x = 1; x = 40; x + 2", 42.0);
}

#[test]
fn multiplication() {
    assert_number("6 * 7", 42.0);
}

#[test]
fn parenthesized_precedence() {
    assert_number("(1 + 2) * 3", 9.0);
}

#[test]
fn operator_precedence() {
    assert_number("10 - 4 / 2", 8.0);
}

#[test]
fn unary_negation() {
    assert_number("-5", -5.0);
}

#[test]
fn while_loop() {
    // Accumulate 0..10 with a `while` loop and comparison.
    assert_number(
        "let i=0; let s=0; while(i<10){ s = s + i; i = i + 1 } s",
        45.0,
    );
}

#[test]
fn if_else_selects_branch() {
    assert_number("let x = 0; if (1 < 2) { x = 10 } else { x = 20 } x", 10.0);
}

#[test]
fn function_call() {
    assert_number("function add(a, b) { return a + b } add(40, 2)", 42.0);
}

#[test]
fn recursive_fib() {
    // Recursion through a two-branch `return`.
    assert_number(
        "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)",
        55.0,
    );
}

#[test]
fn function_with_local_and_forward_call() {
    // A function may use its own local and call another declared later.
    assert_number(
        "function outer(x){ let y = inner(x); return y + 1 } function inner(x){ return x * 2 } outer(20)",
        41.0,
    );
}

#[test]
fn comparison_yields_boolean_number() {
    // === / !== decode to real booleans.
    assert_eq!(
        compile_and_run("3 === 3").expect("pipeline should succeed"),
        Value::Bool(true)
    );
    assert_eq!(
        compile_and_run("3 !== 3").expect("pipeline should succeed"),
        Value::Bool(false)
    );
}

/// Assert a compiled program yields `Value::Bool(expected)`.
fn assert_bool(src: &str, expected: bool) {
    let value = compile_and_run(src).expect("pipeline should succeed");
    assert_eq!(value, Value::Bool(expected), "from {src:?}");
}

#[test]
fn strict_equality_numbers() {
    assert_bool("1 === 1", true);
    assert_bool("1 === 2", false);
    assert_bool("1 !== 2", true);
}

#[test]
fn strict_equality_boxed_types() {
    assert_bool("null === null", true);
    assert_bool("undefined === null", false);
    // Different types differ in tag, so they are never strictly equal.
    assert_bool("true === 1", false);
}

#[test]
fn relational_operators_yield_booleans() {
    assert_bool("1 < 2", true);
    assert_bool("2 < 1", false);
    assert_bool("2 > 1", true);
    assert_bool("2 <= 2", true);
    assert_bool("2 >= 3", false);
}

#[test]
fn ternary_selects_branch_by_truthiness() {
    // A boolean `===` result drives the ternary.
    assert_number("let b = true; let n = b === true ? 42 : 0; n", 42.0);
    assert_number("false ? 1 : 2", 2.0);
}

#[test]
fn truthy_boolean_true_selects_then() {
    // A boolean condition uses JS truthiness, not number-only.
    assert_number("let r = 0; if (true) { r = 1 } else { r = 2 } r", 1.0);
}

#[test]
fn falsy_boolean_false_selects_else() {
    assert_number("let r = 0; if (false) { r = 1 } else { r = 2 } r", 2.0);
}

#[test]
fn falsy_zero_selects_else() {
    assert_number(
        "let r = 0; let x = 0; if (x) { r = 1 } else { r = 2 } r",
        2.0,
    );
}

#[test]
fn falsy_null_selects_else() {
    assert_number("let r = 0; if (null) { r = 1 } else { r = 2 } r", 2.0);
}

#[test]
fn falsy_undefined_selects_else() {
    assert_number("let r = 0; if (undefined) { r = 1 } else { r = 2 } r", 2.0);
}

#[test]
fn truthy_number_one_selects_then() {
    assert_number("let r = 0; if (1) { r = 1 } else { r = 2 } r", 1.0);
}

#[test]
fn falsy_nan_selects_else() {
    // NaN (here `0 / 0`) is falsy.
    assert_number("let r = 0; if (0 / 0) { r = 1 } else { r = 2 } r", 2.0);
}

#[test]
fn while_over_boolean_condition() {
    // A boolean-typed loop condition drives the loop: `b` is truthy for the
    // first pass, then set false to exit, so the body runs exactly once.
    assert_number(
        "let b = true; let n = 0; while (b) { n = n + 1; b = false } n",
        1.0,
    );
}

#[test]
fn boolean_true_literal() {
    let value = compile_and_run("true").expect("pipeline should succeed");
    assert_eq!(value, Value::Bool(true));
    assert_eq!(format_value(&value), "true");
}

#[test]
fn boolean_false_literal() {
    let value = compile_and_run("false").expect("pipeline should succeed");
    assert_eq!(value, Value::Bool(false));
    assert_eq!(format_value(&value), "false");
}

#[test]
fn null_literal() {
    let value = compile_and_run("null").expect("pipeline should succeed");
    assert_eq!(value, Value::Null);
    assert_eq!(format_value(&value), "null");
}

#[test]
fn undefined_literal() {
    let value = compile_and_run("undefined").expect("pipeline should succeed");
    assert_eq!(value, Value::Undefined);
    assert_eq!(format_value(&value), "undefined");
}

#[test]
fn boolean_survives_local_roundtrip() {
    // A boolean stored in a local and read back keeps its type and value.
    let value = compile_and_run("let b = true; b").expect("pipeline should succeed");
    assert_eq!(value, Value::Bool(true));
    assert_eq!(format_value(&value), "true");
}

#[test]
fn declaration_only_program_is_undefined() {
    // No expression statement: the program's value is `undefined`.
    let value = compile_and_run("let x = 42;").expect("pipeline should succeed");
    assert_eq!(value, Value::Undefined);
    assert_eq!(format_value(&value), "undefined");
}

#[test]
fn number_boxing_roundtrips() {
    // Boxing a number is `to_bits`; decode round-trips it back unchanged.
    for &x in &[0.0_f64, 42.0, -5.0, 0.5, 1e300, f64::INFINITY, f64::MIN] {
        assert!(
            matches!(decode(x.to_bits(), &[]), Value::Number(n) if n.to_bits() == x.to_bits()),
            "roundtrip {x}",
        );
    }
}

#[test]
fn decode_boxed_constants() {
    assert_eq!(decode(0x7FF9_0000_0000_0000, &[]), Value::Bool(false));
    assert_eq!(decode(0x7FF9_0000_0000_0001, &[]), Value::Bool(true));
    assert_eq!(decode(0x7FFA_0000_0000_0000, &[]), Value::Null);
    assert_eq!(decode(0x7FFB_0000_0000_0000, &[]), Value::Undefined);
}

#[test]
fn decode_canonical_nan_is_number() {
    assert!(
        matches!(decode(0x7FF8_0000_0000_0000, &[]), Value::Number(n) if n.is_nan()),
        "canonical NaN should decode as a NaN number",
    );
}

#[test]
fn decode_retired_typeof_tag_is_nan_number() {
    // Tag 0b100 is unused (`typeof` yields a real heap string), so it decodes as a
    // NaN number (decode stays total, like the other unused tags).
    assert!(matches!(decode(0x7FFC_0000_0000_0001, &[]), Value::Number(n) if n.is_nan()));
}

#[test]
fn decode_pointer_reads_string_from_memory() {
    // A hand-built heap string block: "hi" laid out as
    // [type=0][pad×3][len=2 LE][b'h', b'i'] at base offset 8.
    let mut memory = vec![0u8; 8];
    memory.extend_from_slice(&[TYPE_STRING, 0, 0, 0]); // header
    memory.extend_from_slice(&2u32.to_le_bytes());
    memory.extend_from_slice(b"hi");
    // Heap jsval: tag 0b101 header (HEAP_BITS) with block base offset 8.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(decode(bits, &memory), Value::Str("hi".to_owned()));
}

#[test]
fn decode_out_of_range_pointer_is_undefined() {
    // A heap pointer past the end of memory must not panic — its type byte
    // can't be read, so decode stays total by yielding `undefined`.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x270F;
    assert_eq!(decode(bits, &[]), Value::Undefined);
}

#[test]
fn decode_reads_type_byte_to_pick_the_arm() {
    // The single heap tag dispatches on the block's TYPE BYTE. The same
    // base offset decodes as a string, array, or object purely by that byte.
    let base = 8usize;
    let bits = 0x7FFD_0000_0000_0000_u64 | base as u64;
    // type=array: a handle `[42.0]` with its element store right after it.
    let mut mem = vec![0u8; base];
    push_array(&mut mem, &[42.0_f64.to_bits()]);
    assert_eq!(
        decode(bits, &mem),
        Value::Array(vec![Value::Number(42.0)], Vec::new())
    );
    // Flip only the type byte to string: [type=0][pad×3][len=1]['x'] would need
    // a 1-byte payload, so rebuild the block accordingly.
    let mut mem = vec![0u8; base];
    mem.extend_from_slice(&[TYPE_STRING, 0, 0, 0]);
    mem.extend_from_slice(&1u32.to_le_bytes());
    mem.push(b'x');
    assert_eq!(decode(bits, &mem), Value::Str("x".to_owned()));
}

#[test]
fn typeof_number() {
    let value = compile_and_run("typeof 42").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("number".to_owned()));
    assert_eq!(format_value(&value), "number");
}

#[test]
fn typeof_boolean() {
    let value = compile_and_run("typeof true").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("boolean".to_owned()));
    assert_eq!(format_value(&value), "boolean");
}

#[test]
fn typeof_undefined() {
    let value = compile_and_run("typeof undefined").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("undefined".to_owned()));
    assert_eq!(format_value(&value), "undefined");
}

#[test]
fn typeof_null() {
    // `typeof null` is the historical JS quirk: it reports "object".
    let value = compile_and_run("typeof null").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("object".to_owned()));
    assert_eq!(format_value(&value), "object");
}

#[test]
fn typeof_computed_number_is_number() {
    // The operand can be any expression, not just a literal.
    let value = compile_and_run("typeof (1 + 2)").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("number".to_owned()));
}

#[test]
fn format_value_covers_every_arm() {
    assert_eq!(format_value(&Value::Number(42.0)), "42");
    assert_eq!(format_value(&Value::Bool(true)), "true");
    assert_eq!(format_value(&Value::Bool(false)), "false");
    assert_eq!(format_value(&Value::Null), "null");
    assert_eq!(format_value(&Value::Undefined), "undefined");
    assert_eq!(format_value(&Value::Str("number".to_owned())), "number");
    // A string renders as its raw text, no surrounding quotes.
    assert_eq!(format_value(&Value::Str("foo".into())), "foo");
}

#[test]
fn string_concatenation_allocates_result() {
    // Two heap strings concatenate into a fresh heap string.
    let value = compile_and_run("\"foo\" + \"bar\"").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("foobar".to_owned()));
    assert_eq!(format_value(&value), "foobar");
}

#[test]
fn concatenated_string_length() {
    // `.length` over a concatenation reads the fresh blob's header.
    assert_number("(\"ab\" + \"cd\").length", 4.0);
}

#[test]
fn nested_string_concatenation() {
    // Left-associative: `("a" + "b") + "c"` — the inner result is a heap
    // string that concatenates again.
    let value = compile_and_run("\"a\" + \"b\" + \"c\"").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("abc".to_owned()));
}

#[test]
fn concatenation_of_variables() {
    let value =
        compile_and_run("let a = \"x\"; let b = \"y\"; a + b").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("xy".to_owned()));
}

#[test]
fn numeric_add_still_works_alongside_concat() {
    // The runtime dispatch keeps the numeric path intact.
    assert_number("40 + 2", 42.0);
}

#[test]
fn format_number_still_renders_numbers() {
    assert_eq!(format_number(42.0), "42");
    assert_eq!(format_number(0.5), "0.5");
}

#[test]
fn string_strict_equality_by_content() {
    // `===` / `!==` on heap strings compares byte content, not identity.
    assert_bool("\"a\" === \"a\"", true);
    assert_bool("\"a\" === \"b\"", false);
    assert_bool("\"a\" !== \"b\"", true);
    assert_bool("\"ab\" === \"ab\"", true);
    assert_bool("\"ab\" !== \"ab\"", false);
}

#[test]
fn concat_result_equals_literal_by_content() {
    // A freshly allocated concatenation compares equal to an identical literal
    // even though it lives at a different heap offset.
    assert_bool("(\"a\" + \"b\") === \"ab\"", true);
}

#[test]
fn string_relational_lexicographic() {
    // `< > <= >=` on heap strings compare lexicographically by bytes.
    assert_bool("\"a\" < \"b\"", true);
    assert_bool("\"b\" < \"a\"", false);
    assert_bool("\"abc\" < \"abd\"", true);
    // A proper prefix is "less" than the longer string.
    assert_bool("\"ab\" < \"abc\"", true);
    assert_bool("\"b\" > \"a\"", true);
    assert_bool("\"a\" <= \"a\"", true);
    assert_bool("\"b\" >= \"a\"", true);
}

#[test]
fn empty_string_is_falsy() {
    // `""` (length 0) is falsy; a non-empty string is truthy. The `if`
    // is a statement, so capture the taken branch in a variable and read it.
    assert_number(
        "let s = \"\"; let r = 0; if (s) { r = 1 } else { r = 2 } r",
        2.0,
    );
    assert_number(
        "let s = \"x\"; let r = 0; if (s) { r = 1 } else { r = 2 } r",
        1.0,
    );
}

#[test]
fn string_truthiness_in_ternary() {
    assert_number("\"\" ? 1 : 2", 2.0);
    assert_number("\"x\" ? 1 : 2", 1.0);
}

#[test]
fn array_index_read() {
    // Element reads dispatch on the array tag and sum numerically.
    assert_number("let a=[1,2,3]; a[0]+a[2]", 4.0);
}

#[test]
fn array_length() {
    // `.length` reads the same u32 header strings use.
    assert_number("[1,2,3].length", 3.0);
    assert_number("let a=[]; a.length", 0.0);
}

#[test]
fn typeof_array_is_object() {
    // `typeof []` is "object" (the array tag maps to index 3).
    let value = compile_and_run("typeof []").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("object".to_owned()));
    assert_eq!(format_value(&value), "object");
}

#[test]
fn array_sum_via_while_loop() {
    // Index reads inside a `while`, driven by `.length`.
    assert_number(
        "let a=[1,2,3,4]; let i=0; let s=0; while(i<a.length){ s=s+a[i]; i=i+1 } s",
        10.0,
    );
}

#[test]
fn array_is_always_truthy() {
    // Even the empty array is truthy (its length is never read).
    assert_number("let r=0; if ([]) { r=1 } else { r=2 } r", 1.0);
    assert_number("[1] ? 1 : 2", 1.0);
}

#[test]
fn array_decodes_and_formats() {
    // The whole pipeline yields a `Value::Array`; it renders Node style.
    let value = compile_and_run("[1,2,3]").expect("pipeline should succeed");
    assert_eq!(
        value,
        Value::Array(
            vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)],
            Vec::new()
        )
    );
    assert_eq!(format_value(&value), "[ 1, 2, 3 ]");

    let empty = compile_and_run("[]").expect("pipeline should succeed");
    assert_eq!(empty, Value::Array(Vec::new(), Vec::new()));
    assert_eq!(format_value(&empty), "[]");
}

#[test]
fn object_property_read() {
    // `.a` reads dispatch on the object tag and sum numerically.
    assert_number("let o={a:1,b:2}; o.a+o.b", 3.0);
    assert_number("let o={x:10}; o.x", 10.0);
}

#[test]
fn object_computed_read() {
    // `o["a"]` shares the `Index` dispatch with `o.a`.
    assert_number("let o={a:1}; o[\"a\"]", 1.0);
}

#[test]
fn object_string_literal_key() {
    // A string-literal key `{"k": 5}` reads back the same as an identifier key.
    assert_number("let o={\"k\":5}; o.k", 5.0);
}

#[test]
fn object_missing_key_is_undefined() {
    // Reading an absent key yields `undefined` (the scan falls through).
    let value = compile_and_run("let o={a:1}; o.b").expect("pipeline should succeed");
    assert_eq!(value, Value::Undefined);
}

#[test]
fn typeof_object_is_object() {
    // `typeof {}` is "object" (the object tag maps to index 3).
    let value = compile_and_run("typeof {}").expect("pipeline should succeed");
    assert_eq!(value, Value::Str("object".to_owned()));
    assert_eq!(format_value(&value), "object");
}

#[test]
fn object_is_always_truthy() {
    // Even the empty object is truthy (its contents are never read). The
    // object literals are parenthesized so they parse as expressions, not
    // block statements.
    assert_number("let r=0; if ({}) { r=1 } else { r=2 } r", 1.0);
    assert_number("({a:1}) ? 1 : 2", 1.0);
}

#[test]
fn object_literal_roundtrips_to_heap_object() {
    // A `{a:1}` literal round-trips through the whole pipeline to a
    // heap object with one decoded entry. Bound to a local so the `{..}`
    // parses as an object expression rather than a block statement.
    let value = compile_and_run("let o={a:1}; o").expect("pipeline should succeed");
    assert_eq!(
        value,
        Value::Object(vec![("a".to_owned(), Value::Number(1.0))])
    );
    assert_eq!(format_value(&value), "{ a: 1 }");
}

#[test]
fn object_decodes_and_formats() {
    // A two-entry object decodes in insertion order and renders Node style.
    let value = compile_and_run("let o={a:1,b:2}; o").expect("pipeline should succeed");
    assert_eq!(
        value,
        Value::Object(vec![
            ("a".to_owned(), Value::Number(1.0)),
            ("b".to_owned(), Value::Number(2.0)),
        ])
    );
    assert_eq!(format_value(&value), "{ a: 1, b: 2 }");

    let empty = compile_and_run("let e={}; e").expect("pipeline should succeed");
    assert_eq!(empty, Value::Object(Vec::new()));
    assert_eq!(format_value(&empty), "{}");
}

#[test]
fn decode_object_reads_entries_from_memory() {
    // A hand-built heap object `{a: 7}` (plus a non-enumerable `h` and a
    // tombstone): the key strings "a" at 8 and "h" at 24, the object at 40, its
    // property table at 56:
    //   object: [type=2][0×3][props=56][proto=null]
    //   table : [12][0][tflags=HAS_NON_ENUM][0][count=3][cap=3][live=2]
    //           ["a" | attrs 7 | 7.0] [tombstone] ["h" | attrs 5 | 1.0]
    let null = 0x7FFA_0000_0000_0000_u64;
    let mut memory = vec![0u8; 8];
    for key in *b"ah" {
        memory.extend_from_slice(&[TYPE_STRING, 0, 1, 0]);
        memory.extend_from_slice(&1u32.to_le_bytes());
        memory.push(key);
        memory.resize(memory.len() + 7, 0);
    }
    assert_eq!(memory.len(), 40);
    memory.extend_from_slice(&[TYPE_OBJECT, 0, 0, 0]);
    memory.extend_from_slice(&56u32.to_le_bytes());
    memory.extend_from_slice(&null.to_le_bytes());
    memory.extend_from_slice(&[12, 0, 2, 0]);
    for word in [3u32, 3, 2] {
        memory.extend_from_slice(&word.to_le_bytes());
    }
    for (key, attrs, value) in [(8u32, 7u32, 7.0_f64), (0, 0, 0.0), (24, 5, 1.0)] {
        memory.extend_from_slice(&key.to_le_bytes());
        memory.extend_from_slice(&attrs.to_le_bytes());
        memory.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x28;
    assert_eq!(
        decode(bits, &memory),
        Value::Object(vec![("a".to_owned(), Value::Number(7.0))])
    );
}

/// Append a hand-built array handle holding `elems` at the end of `memory`
/// (`[type=1][…][props=0][proto=null][len][elems]`), followed by its
/// `TYPE_ELEMS` store (`[type=13][…][cap][elem × cap]`).
fn push_array(memory: &mut Vec<u8>, elems: &[u64]) {
    let base = memory.len();
    let len = u32::try_from(elems.len()).expect("len");
    let store = u32::try_from(base + 24).expect("store");
    memory.extend_from_slice(&[TYPE_ARRAY, 0, 0, 0]);
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFA_0000_0000_0000_u64.to_le_bytes());
    memory.extend_from_slice(&len.to_le_bytes());
    memory.extend_from_slice(&store.to_le_bytes());
    memory.extend_from_slice(&[crate::abi::TYPE_ELEMS, 0, 0, 0]);
    memory.extend_from_slice(&len.to_le_bytes());
    for bits in elems {
        memory.extend_from_slice(&bits.to_le_bytes());
    }
}

#[test]
fn decode_array_reads_elements_from_memory() {
    // A hand-built heap array `[1, 2]` at base offset 8: the handle, then its
    // element store.
    let mut memory = vec![0u8; 8];
    push_array(&mut memory, &[1.0_f64.to_bits(), 2.0_f64.to_bits()]);
    // Array jsval: single heap tag 0b101 (HEAP_BITS) with block base offset 8.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(
        decode(bits, &memory),
        Value::Array(vec![Value::Number(1.0), Value::Number(2.0)], Vec::new())
    );
}

/// Assert a compiled program yields `Value::Str(expected)`.
fn assert_string(src: &str, expected: &str) {
    let value = compile_and_run(src).expect("pipeline should succeed");
    assert_eq!(value, Value::Str(expected.to_owned()), "from {src:?}");
}

// Logical NOT `!` (boolean inverse of truthiness).
#[test]
fn logical_not() {
    assert_bool("!true", false);
    assert_bool("!false", true);
    assert_bool("!0", true);
    assert_bool("!\"\"", true);
    assert_bool("!\"x\"", false);
    assert_bool("!null", true);
    assert_bool("!undefined", true);
    // Arrays/objects are always truthy, so `![]`/`!{}` are false.
    assert_bool("![]", false);
    assert_bool("!({})", false);
    // Double negation collapses to a plain truthiness boolean.
    assert_bool("!!5", true);
}

// `&&` short-circuit with VALUE semantics (yields the operand jsval).
#[test]
fn logical_and() {
    assert_number("1 && 2", 2.0);
    assert_number("0 && 2", 0.0);
    assert_number("4 && 5", 5.0);
    // `a && b` yields `a` (the operand) when `a` is falsy, not a boolean.
    assert_string("1 && \"s\"", "s");
    assert_bool("false && true", false);
}

// `||` short-circuit with VALUE semantics.
#[test]
fn logical_or() {
    assert_number("0 || 3", 3.0);
    assert_number("4 || 5", 4.0);
    assert_number("1 || 2", 1.0);
    assert_string("\"\" || \"x\"", "x");
    assert_string("\"a\" || \"b\"", "a");
}

// `??` nullish coalescing (only `null`/`undefined` trigger the rhs).
#[test]
fn nullish_coalescing() {
    assert_number("null ?? 5", 5.0);
    assert_number("undefined ?? 5", 5.0);
    assert_number("4 ?? 5", 4.0);
    // Falsy-but-not-nullish operands (0, "") do NOT trigger the rhs.
    assert_number("0 ?? 5", 0.0);
    assert_string("\"\" ?? \"x\"", "");
}

// Short-circuit must NOT evaluate the rhs when it short-circuits.
#[test]
fn short_circuit_skips_rhs_side_effect() {
    // `false && (x = 1)` never runs the assignment, so `x` stays 0.
    assert_number("let x = 0; false && (x = 1); x", 0.0);
    // `true || (y = 1)` never runs the assignment, so `y` stays 0.
    assert_number("let y = 0; true || (y = 1); y", 0.0);
    // The rhs DOES run when the operator does not short-circuit.
    assert_number("let z = 0; true && (z = 1); z", 1.0);
    assert_number("let w = 0; false || (w = 1); w", 1.0);
    // `??` short-circuits on a non-nullish left (0 is not nullish).
    assert_number("let v = 0; 0 ?? (v = 1); v", 0.0);
}

// Nested logicals reuse the single temp local correctly.
#[test]
fn nested_logical_operators() {
    assert_number("1 && 2 && 3", 3.0);
    assert_number("0 && 1 && 2", 0.0);
    assert_number("0 || 0 || 7", 7.0);
    assert_number("null ?? undefined ?? 9", 9.0);
    // Mixed precedence: `&&` binds tighter than `||`.
    assert_number("0 || 2 && 3", 3.0);
}

// Compound assignment `+= -= *= /= %=` over a local (read once).
#[test]
fn compound_assignment() {
    assert_number("let x = 40; x += 2; x", 42.0);
    assert_number("let y = 10; y *= 4; y", 40.0);
    assert_number("let z = 10; z -= 3; z", 7.0);
    assert_number("let w = 17; w %= 5; w", 2.0);
    assert_number("let q = 20; q /= 4; q", 5.0);
}

// `+=` reuses the `+` overload, so a string lhs concatenates.
#[test]
fn string_compound_assignment() {
    assert_string("let s = \"foo\"; s += \"bar\"; s", "foobar");
}

// Compound assignment on member targets `a[i] op= e` / `o.k op= e`.
#[test]
fn member_compound_assignment() {
    // Array element target.
    assert_number("let a = [10, 20]; a[0] += 5; a[0]", 15.0);
    assert_number("let a = [3]; a[0] *= 14; a[0]", 42.0);
    // Static-property object target.
    assert_number("let o = {x: 1}; o.x += 41; o.x", 42.0);
    // Computed object-member target.
    assert_number("let o = {n: 100}; o[\"n\"] -= 58; o.n", 42.0);
}

// All five compound operators are supported on member targets.
#[test]
fn member_compound_assignment_all_operators() {
    assert_number("let a = [40]; a[0] += 2; a[0]", 42.0);
    assert_number("let a = [50]; a[0] -= 8; a[0]", 42.0);
    assert_number("let a = [6]; a[0] *= 7; a[0]", 42.0);
    assert_number("let a = [84]; a[0] /= 2; a[0]", 42.0);
    assert_number("let o = {v: 17}; o.v %= 5; o.v", 2.0);
}

// `+=` on a string member reuses the `+` overload (concatenation).
#[test]
fn member_string_compound_assignment() {
    assert_string("let o = {s: \"foo\"}; o.s += \"bar\"; o.s", "foobar");
    assert_string("let a = [\"foo\"]; a[0] += \"baz\"; a[0]", "foobaz");
}

// The index sub-expression is evaluated EXACTLY ONCE. A postfix
// `i++` index bumps `i` each time it runs; if it ran twice (once for the read,
// once for the store) the write would land on `a[1]` and `i` would reach 2.
#[test]
fn member_compound_assignment_evaluates_index_once() {
    // Evaluated once: a[0] = 15, a[1] = 20, i = 1 => 1500 + 200 + 1 = 1701.
    let src = "let a = [10, 20]; let i = 0; a[i++] += 5; a[0] * 100 + a[1] * 10 + i";
    assert_number(src, 1701.0);
}

// `%` (BinOp::Mod) as a standalone operator, incl. JS sign semantics.
#[test]
fn modulo_operator() {
    assert_number("17 % 5", 2.0);
    assert_number("10 % 3", 1.0);
    // JS `%` takes the sign of the dividend (truncated remainder).
    assert_number("-7 % 3", -1.0);
    assert_number("7 % -3", 1.0);
}

// `++`/`--` in both prefix and postfix forms.
#[test]
fn increment_decrement() {
    // Statement-position update mutates the local.
    assert_number("let x = 41; x++; x", 42.0);
    assert_number("let i = 5; --i", 4.0);
    // Postfix yields the OLD value; prefix yields the NEW value.
    assert_number("let a = 5; let b = a++; b", 5.0);
    assert_number("let a = 5; let b = a++; a", 6.0);
    assert_number("let c = 5; let d = ++c; d", 6.0);
    // Postfix/prefix decrement.
    assert_number("let e = 5; let f = e--; f", 5.0);
    assert_number("let g = 5; let h = --g; h", 4.0);
}

// `switch`/`case`/`default`/`break` with JS fall-through.
#[test]
fn switch_case() {
    // A matching case with `break` selects its body.
    assert_number(
        "let x=2; let r=0; switch(x){ case 1: r=10; break; case 2: r=42; break; default: r=99 } r",
        42.0,
    );
    // `default` runs when no case matches.
    assert_number(
        "let x=9; let r=0; switch(x){ case 1: r=10; break; default: r=99 } r",
        99.0,
    );
    // An empty case falls through to the next case body.
    assert_number(
        "let x=1; let r=0; switch(x){ case 1: case 2: r=42; break; default: r=0 } r",
        42.0,
    );
    // A body without `break` falls through into the next case body.
    assert_number(
        "let x=1; let r=0; switch(x){ case 1: r=1; case 2: r=r+40; break; default: } r",
        41.0,
    );
}

// `default` between cases; fall-through follows source order.
#[test]
fn switch_default_in_middle_falls_through() {
    assert_number(
        "let x=5; let r=0; switch(x){ case 1: r=1; break; default: r=r+3; case 2: r=r+4; break } r",
        7.0,
    );
}

// No match and no `default` leaves the discriminant handling a no-op;
// `break` nested inside an `if` still exits the switch.
#[test]
fn switch_no_match_and_nested_break() {
    assert_number(
        "let x=7; let r=0; switch(x){ case 1: r=1; break; case 2: r=2; break } r",
        0.0,
    );
    assert_number(
        "let x=1; let r=0; switch(x){ case 1: if(x>0){ r=42; break } r=5; break; default: r=9 } r",
        42.0,
    );
}

// Strict-equality (`===`) dispatch works on a string discriminant.
#[test]
fn switch_string_discriminant() {
    assert_number(
        "let x=\"b\"; let r=0; switch(x){ case \"a\": r=1; break; case \"b\": r=2; break } r",
        2.0,
    );
}

// Block scoping for `let`/`const`.

#[test]
fn inner_block_shadow_does_not_leak() {
    // The inner `let x=2` shadows within its block; after the block, `x`
    // resolves back to the outer binding (=> 1).
    assert_number("let x=1; { let x=2; } x", 1.0);
}

#[test]
fn inner_block_sees_its_own_binding() {
    // Inside the block, `x` captures the inner binding (=> 2)...
    assert_number("let x=1; { let x=2; x }", 2.0);
    // ...while the same source read after the block sees the outer one (=> 1).
    assert_number("let x=1; { let x=2; } x", 1.0);
}

#[test]
fn nested_blocks_shadow_independently() {
    // Two levels of shadowing peel back to the outermost binding (=> 1).
    assert_number("let x=1; { let x=2; { let x=3; } } x", 1.0);
    // The innermost read sees its own binding (=> 3).
    assert_number("let x=1; { let x=2; { let x=3; x } }", 3.0);
}

#[test]
fn shadowed_bindings_use_distinct_locals() {
    // Mutating the inner binding must not touch the outer one: after the
    // block, `x` is still the outer value (=> 1).
    assert_number("let x=1; { let x=2; x = 99; } x", 1.0);
}

#[test]
fn for_let_binding_is_block_scoped() {
    // The `for` head binding stays visible (and mutable) inside the loop.
    assert_number("let s=0; for(let i=0;i<3;i=i+1){ s=s+i } s", 3.0);
}

#[test]
fn for_let_binding_not_visible_after_loop() {
    // Referencing the `for`-`let` binding after the loop is a compile error.
    assert!(
        compile_and_run("for(let i=0;i<3;i=i+1){} i").is_err(),
        "for-let binding must not leak past the loop"
    );
}

#[test]
fn block_local_not_visible_after_block() {
    // A `let` declared only inside a block is undeclared after it.
    assert!(
        compile_and_run("{ let y=5; } y").is_err(),
        "block-local binding must not leak out"
    );
}

// Every heap kind shares the one NaN-box pointer tag `0b101`, discriminated
// by a TYPE BYTE in the block header. `typeof` and a full value round-trip must be preserved for all three
// user-visible heap kinds through the single-tag + header layout.
#[test]
fn heap_type_byte_typeof_and_value_roundtrip() {
    // String: typeof "string" and the text round-trips through the header.
    assert_eq!(
        compile_and_run("typeof \"hi\"").expect("pipeline should succeed"),
        Value::Str("string".to_owned())
    );
    assert_eq!(
        compile_and_run("let s = \"hi\"; s").expect("pipeline should succeed"),
        Value::Str("hi".to_owned())
    );
    // Array: typeof "object" and the elements round-trip.
    assert_eq!(
        compile_and_run("typeof [1, 2]").expect("pipeline should succeed"),
        Value::Str("object".to_owned())
    );
    assert_eq!(
        compile_and_run("let a = [7, 8]; a").expect("pipeline should succeed"),
        Value::Array(vec![Value::Number(7.0), Value::Number(8.0)], Vec::new())
    );
    // Object: typeof "object" and the entries round-trip.
    assert_eq!(
        compile_and_run("typeof ({a: 1})").expect("pipeline should succeed"),
        Value::Str("object".to_owned())
    );
    assert_eq!(
        compile_and_run("let o = {a: 1, b: 2}; o").expect("pipeline should succeed"),
        Value::Object(vec![
            ("a".to_owned(), Value::Number(1.0)),
            ("b".to_owned(), Value::Number(2.0)),
        ])
    );
}

// First-class function values: a function declaration used in value
// position becomes a callable heap value (table index + env), called through
// `call_indirect` when the callee is a variable/parameter/returned value.
#[test]
fn first_class_function_value() {
    // Bind a function to a variable and call it indirectly.
    assert_number("function add(a,b){return a+b} let f = add; f(40,2)", 42.0);
}

#[test]
fn function_passed_as_argument() {
    // Pass a function value as an argument and call it inside the callee.
    assert_number(
        "function apply(f,x){return f(x)} function dbl(n){return n*2} apply(dbl,21)",
        42.0,
    );
}

#[test]
fn typeof_function_value_is_function() {
    // `typeof` a function value (bound to a variable, and a bare name) is
    // "function".
    let value = compile_and_run("function id(x){return x} let g=id; typeof g").expect("pipeline");
    assert_eq!(value, Value::Str("function".to_owned()));
    assert_eq!(format_value(&value), "function");
    assert_eq!(
        compile_and_run("function id(x){return x} typeof id").expect("pipeline"),
        Value::Str("function".to_owned()),
    );
}

#[test]
fn bare_function_value_decodes_and_formats() {
    // A returned bare function value decodes to `Value::Function` with its
    // `__fn_info` name and renders Node-console style.
    let value = compile_and_run("function id(x){return x} let g=id; g").expect("pipeline");
    assert_eq!(
        value,
        Value::Function {
            name: "id".to_owned(),
            class: false,
            extends: None,
            props: Vec::new(),
        }
    );
    assert_eq!(format_value(&value), "[Function: id]");
    let arrow = compile_and_run("let g = [() => 1]; g[0]").expect("pipeline");
    assert_eq!(format_value(&arrow), "[Function (anonymous)]");
}

#[test]
fn indirect_call_pads_and_ignores_arity() {
    // Uniform ABI: a missing argument reads back `undefined`; extra arguments
    // are ignored. `zero-arg` and mixed-arity functions share one table type.
    assert_number("function k(){return 7} let m=k; m()", 7.0);
    // Calling a 1-arg function value with no args yields `undefined` for the
    // parameter (returned straight back).
    let value = compile_and_run("function id(x){return x} let g=id; g()").expect("pipeline");
    assert_eq!(value, Value::Undefined);
}

// Arrow functions: an arrow expression is a first-class function
// value (same heap repr and uniform ABI as a `function` declaration), with an
// expression body (implicit return) or a block body.
#[test]
fn arrow_multi_param_expression_body() {
    // Parenthesized multi-parameter arrow with an implicit-return body.
    assert_number("let add = (a,b) => a+b; add(40,2)", 42.0);
}

#[test]
fn arrow_single_unparenthesized_param() {
    // Single parameter without parentheses, expression body.
    assert_number("let sq = x => x*x; sq(7)", 49.0);
}

#[test]
fn arrow_block_body() {
    // Block body with a local declaration and an explicit `return`.
    assert_number("let f = (n) => { let t = n*2; return t+1 }; f(20)", 41.0);
}

#[test]
fn arrow_passed_as_argument() {
    // An arrow passed straight into a higher-order function and called there.
    assert_number("function apply(f,x){return f(x)} apply(x=>x*3, 14)", 42.0);
}

#[test]
fn arrow_zero_params_and_typeof() {
    // A zero-parameter arrow is callable and `typeof` an arrow is "function".
    assert_number("let g = () => 7; g()", 7.0);
    assert_eq!(
        compile_and_run("typeof (x=>x)").expect("pipeline"),
        Value::Str("function".to_owned()),
    );
}

#[test]
fn arrow_wider_arity_fixes_uniform_abi() {
    // An arrow wider than any declaration still calls correctly: the uniform
    // ABI's fixed arity covers every arrow, so mixed-arity calls share one
    // `call_indirect` type.
    assert_number("let f=(a,b,c)=>a+b+c; let g=x=>x; f(1,2,3)+g(36)", 42.0);
}

// Closures: a nested function/arrow captures enclosing bindings by
// reference through a heap environment of boxed cells.
#[test]
fn closure_read_capture() {
    // `adder(5)` returns an arrow that captures `x`; `a5(37)` reads the
    // captured `x` (= 5) from its cell and adds 37.
    assert_number(
        "function adder(x){ return y => x + y } let a5 = adder(5); a5(37)",
        42.0,
    );
}

#[test]
fn closure_mutable_cell() {
    // `counter` returns a function that increments a shared captured `c`;
    // three calls yield 1, 2, 3, so the third returns 3.
    assert_number(
        "function counter(){ let c=0; return function(){ c=c+1; return c } } \
         let n=counter(); n(); n(); n()",
        3.0,
    );
}

#[test]
fn closure_instances_are_independent() {
    // Two closures from the same factory hold distinct cells: `a` captures
    // x=5 and `b` captures x=100, so `a(1)` is 6 and `b(1)` is 101 — their
    // sum (107) only holds if the two captures do not share a cell.
    assert_number(
        "function adder(x){ return y => x + y } \
         let a=adder(5); let b=adder(100); a(1)+b(1)",
        107.0,
    );
    // Two independent mutable counters do not interfere.
    assert_number(
        "function counter(){ let c=0; return function(){ c=c+1; return c } } \
         let m=counter(); let n=counter(); m(); m(); n(); m()",
        3.0,
    );
}

#[test]
fn closure_captures_top_level_binding() {
    // An arrow at the top level (inside `main`) captures a `main` local by
    // reference through a boxed cell.
    assert_number("let x=5; let f=()=>x; f()", 5.0);
    assert_number(
        "let c=0; let inc=()=>{ c=c+1; return c }; inc(); inc()",
        2.0,
    );
}

#[test]
fn closure_transitive_capture_two_levels() {
    // A binding captured two function levels deep is routed through each
    // intermediate environment: `outer(9)()()` returns the captured `x`.
    assert_number(
        "function outer(x){ return function(){ return function(){ return x } } } outer(9)()()",
        9.0,
    );
}

#[test]
fn closure_nested_function_declaration_captures() {
    // A nested `function` declaration is a first-class value bound in scope,
    // and captures enclosing bindings like an arrow.
    assert_number(
        "function make(){ function inner(){ return 7 } return inner } make()()",
        7.0,
    );
    assert_number(
        "function adder(x){ function add(y){ return x + y } return add } adder(40)(2)",
        42.0,
    );
}

#[test]
fn closure_compound_assign_through_cell() {
    // Compound assignment to a captured binding read-modify-writes its cell,
    // so the accumulated state persists across calls.
    assert_number(
        "function acc(){ let s=0; return d => { s += d; return s } } \
         let f=acc(); f(10); f(5)",
        15.0,
    );
}

#[test]
fn non_capturing_function_is_unaffected() {
    // A function value that captures nothing carries an empty environment and
    // still calls correctly (the closure path costs nothing for it).
    assert_number(
        "function apply(f,x){return f(x)} function dbl(n){return n*2} apply(dbl,21)",
        42.0,
    );
    assert_number("let sq = x => x*x; sq(7)", 49.0);
}

#[test]
fn function_value_decode_reads_type_byte() {
    // A hand-built function block at base offset 8: [type=3][pad×3][props=0]
    // [proto=null][func_idx:u32=0][reserved:u32][env:i64=undefined], with no
    // `__fn_info` table (its address word is 0) and so no name.
    let mut memory = vec![0u8; 8];
    memory.extend_from_slice(&[TYPE_FUNCTION, 0, 0, 0]);
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFA_0000_0000_0000_u64.to_le_bytes());
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFB_0000_0000_0000_u64.to_le_bytes());
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(
        format_value(&decode(bits, &memory)),
        "[Function (anonymous)]"
    );
}

// The type byte, not the (single) NaN-box tag, distinguishes a
// heap string from an array/object: `"a" + "b"` still concatenates, while
// `[1] === [1]` compares by reference (different blocks) and is `false`.
#[test]
fn heap_type_byte_disambiguates_string_from_array_object() {
    assert_string("\"a\" + \"b\"", "ab");
    assert_bool("[1] === [1]", false);
    assert_bool("({}) === ({})", false);
    // Nested: an array of objects and an object holding an array round-trip,
    // exercising the shifted element/entry offsets under the header.
    assert_number("let o = {v: [7, 8]}; o.v[1]", 8.0);
}

// `for-in` over an object's own enumerable string keys, in insertion
// order; the loop binding is each KEY as a string.
#[test]
fn for_in_object_keys() {
    // Concatenating the keys of `{a:1, b:2}` yields "ab" (insertion order).
    assert_string(
        "let ks=\"\"; for (const k in {a:1,b:2}) { ks = ks + k } ks",
        "ab",
    );
}

#[test]
fn for_in_object_value_lookup() {
    // The key binds to a string, so `o[k]` reads each value back: 10 + 32 = 42.
    assert_number(
        "let s=0; let o={x:10,y:32}; for (const k in o) { s = s + o[k] } s",
        42.0,
    );
}

#[test]
fn for_in_empty_object_zero_iterations() {
    // An empty object enumerates no keys: the body never runs.
    assert_number("let n=0; for (const k in {}) { n = n + 1 } n", 0.0);
}

#[test]
fn for_in_array_yields_index_strings() {
    // Over an array the keys are the index strings "0","1","2" (not the elements).
    assert_string(
        "let ks=\"\"; for (const k in [10,20,30]) { ks = ks + k } ks",
        "012",
    );
    // And each index key indexes back into the array (10 + 20 + 30 = 60), after
    // coercing the "0"/"1"/"2" string key to a number.
    assert_number(
        "let s=0; let a=[10,20,30]; for (const k in a) { s = s + a[Number(k)] } s",
        60.0,
    );
}

#[test]
fn for_in_let_and_var_binding_forms() {
    // `let` and `var` heads are accepted alongside `const`.
    assert_string(
        "let ks=\"\"; for (let k in {a:1,b:2}) { ks = ks + k } ks",
        "ab",
    );
    assert_string(
        "let ks=\"\"; for (var k in {a:1,b:2}) { ks = ks + k } ks",
        "ab",
    );
}

#[test]
fn for_in_supports_break_and_continue() {
    // `break`/`continue` target the desugared loop; the index still advances on a
    // `continue` (via the loop's `update` slot). Keys of {a,b,c}: skip "a", stop
    // at "c" -> only "b" is collected.
    assert_string(
        "let ks=\"\"; for (const k in {a:1,b:2,c:3}) { if (k === \"a\") { continue } \
         if (k === \"c\") { break } ks = ks + k } ks",
        "b",
    );
}
