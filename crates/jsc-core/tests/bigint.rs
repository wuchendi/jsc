//! Integration tests: the `BigInt` value (DESIGN.md §6.10, stage B2a) —
//! literals, `typeof`, truthiness, rendering, string conversion, strict and
//! loose equality, `SameValueZero` (`Map` / `Set` / `includes`), property keys,
//! `JSON.stringify`, and the `ToNumber` `TypeError`.

mod common;
use common::{
    assert_bool, assert_eval, assert_output, assert_same_under_gc_stress, assert_string,
    assert_uncaught,
};

/// `2^64`.
const TWO_64: &str = "18446744073709551616";
/// `2^200`.
const TWO_200: &str = "1606938044258990275541962092341162602522202993782792835301376";

// --- literals ----------------------------------------------------------------

#[test]
fn literals_in_every_radix() {
    assert_eval("1n", "1n");
    assert_eval("0n", "0n");
    assert_eval("0x10n", "16n");
    assert_eval("0o17n", "15n");
    assert_eval("0b101n", "5n");
    assert_eval("1_000n", "1000n");
    assert_eval("-12n", "-12n");
    assert_eval("-(-7n)", "7n");
}

#[test]
fn negative_zero_is_zero() {
    assert_eval("-0n", "0n");
    assert_bool("-0n === 0n", true);
    assert_string("String(-0n)", "0");
}

#[test]
fn multi_limb_literals() {
    assert_eval(&format!("{TWO_64}n"), &format!("{TWO_64}n"));
    assert_eval(
        "0xffffffffffffffffffffffffn",
        "79228162514264337593543950335n",
    );
    assert_eval(&format!("-{TWO_200}n"), &format!("-{TWO_200}n"));
    let hex = format!("0x1{}n", "0".repeat(50));
    assert_string(&format!("String({hex})"), TWO_200);
    assert_string(
        "String(10000000000000000000000000000000000000000n)",
        "10000000000000000000000000000000000000000",
    );
}

// --- typeof and truthiness ---------------------------------------------------

#[test]
fn typeof_is_bigint() {
    assert_string("typeof 1n", "bigint");
    assert_string("typeof 0n", "bigint");
    assert_string(&format!("typeof -{TWO_64}n"), "bigint");
    assert_string("typeof Object(1n)", "object");
}

#[test]
fn zero_is_falsy() {
    assert_string("0n ? 'y' : 'n'", "n");
    assert_string("-0n ? 'y' : 'n'", "n");
    assert_string("1n ? 'y' : 'n'", "y");
    assert_string("-1n ? 'y' : 'n'", "y");
    assert_bool("!0n", true);
    assert_bool(&format!("!!{TWO_64}n"), true);
    assert_string("0n || 'x'", "x");
    assert_string("let r = 'n'; if (2n) r = 'y'; r", "y");
    assert_bool("Boolean(0n)", false);
    assert_bool("!!Object(0n)", true);
}

// --- rendering ---------------------------------------------------------------

#[test]
fn console_renders_the_n_suffix() {
    assert_output("console.log(1n, -2n)", "1n -2n\n");
    assert_output("console.log([1n, 2n])", "[ 1n, 2n ]\n");
    assert_output("console.log({ a: 1n })", "{ a: 1n }\n");
    assert_output("console.log(new Map([[1n, 1]]))", "Map(1) { 1n => 1 }\n");
    assert_output("console.log(new Set([2n]))", "Set(1) { 2n }\n");
    assert_output(
        &format!("console.log({TWO_200}n)"),
        &format!("{TWO_200}n\n"),
    );
    assert_output("console.log(Object(1n))", "[BigInt: 1n]\n");
}

// --- ToString ----------------------------------------------------------------

#[test]
fn string_conversion_has_no_suffix() {
    assert_string("'' + 1n", "1");
    assert_string("1n + ''", "1");
    assert_string("'x' + -5n", "x-5");
    assert_string("`${-5n}|${0n}`", "-5|0");
    assert_string("[1n, 2n].join()", "1,2");
    assert_string(&format!("String({TWO_64}n)"), TWO_64);
    assert_string(&format!("`${{-{TWO_200}n}}`"), &format!("-{TWO_200}"));
}

// --- equality ----------------------------------------------------------------

#[test]
fn strict_equality_is_by_value() {
    assert_bool("1n === 1n", true);
    assert_bool("1n !== 1n", false);
    assert_bool("1n === -1n", false);
    assert_bool("1n === 1", false);
    assert_bool("1n === '1'", false);
    assert_bool(&format!("{TWO_64}n === {TWO_64}n"), true);
    assert_bool("let a = 5n, b = 5n; a === b", true);
    assert_string(
        "let r = ''; switch (2n) { case 2: r = 'num'; break; case 2n: r = 'big' } r",
        "big",
    );
}

#[test]
fn same_value_and_same_value_zero() {
    assert_bool("Object.is(1n, 1n)", true);
    assert_bool("Object.is(0n, -0n)", true);
    assert_bool("Object.is(1n, 1)", false);
    assert_bool("[1n].includes(1n)", true);
    assert_bool("[1].includes(1n)", false);
    assert_string("String([0, 1n, 2n].indexOf(2n))", "2");
}

#[test]
fn loose_equality_with_numbers() {
    assert_bool("1n == 1", true);
    assert_bool("1 == 1n", true);
    assert_bool("-1n == -1", true);
    assert_bool("1n == 1.5", false);
    assert_bool("1n == NaN", false);
    assert_bool("1n == Infinity", false);
    assert_bool("-1n == -Infinity", false);
    assert_bool("0n == -0", true);
    assert_bool(&format!("{TWO_64}n == {TWO_64}"), true);
    assert_bool("18446744073709551617n == 18446744073709551616", false);
    assert_bool("9007199254740993n == 9007199254740992", false);
    assert_bool(&format!("{TWO_200}n == {TWO_200}"), true);
    assert_bool(
        &format!("{TWO_200}n == 3213876088517980551083924184682325205044405987565585670602752"),
        false,
    );
    assert_bool("1n != 2", true);
}

#[test]
fn loose_equality_with_strings_booleans_and_objects() {
    assert_bool("1n == '1'", true);
    assert_bool("'1' == 1n", true);
    assert_bool("-2n == '-2'", true);
    assert_bool("1n == ' 1 '", true);
    assert_bool("16n == '0x10'", true);
    assert_bool("0n == ''", true);
    assert_bool("1n == '1.0'", false);
    assert_bool("1n == '1n'", false);
    assert_bool("1n == 'x'", false);
    assert_bool(&format!("{TWO_64}n == '{TWO_64}'"), true);
    assert_bool("1n == true", true);
    assert_bool("0n == false", true);
    assert_bool("2n == true", false);
    assert_bool("0n == null", false);
    assert_bool("undefined == 0n", false);
    assert_bool("({ valueOf() { return 2n } }) == 2", true);
    assert_bool("1n == Symbol()", false);
}

#[test]
fn map_and_set_keys() {
    assert_string("new Map([[1n, 'a']]).get(1n)", "a");
    assert_bool("new Map([[1n, 'a']]).get(1) === undefined", true);
    assert_string("String(new Set([1n, 1, 1n, -1n]).size)", "3");
    assert_string(
        &format!("String(new Set([{TWO_64}n, {TWO_64}n, -{TWO_64}n]).size)"),
        "2",
    );
    // Past the hash-index threshold (DESIGN.md §5.2) too.
    assert_bool(
        "let s = new Set([1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n, 9n, 10n, 11n, 12n]); \
         s.has(7n) && !s.has(7) && s.size === 12",
        true,
    );
}

// --- property keys -----------------------------------------------------------

#[test]
fn computed_keys_use_the_decimal_string() {
    assert_string("let o = {}; o[1n] = 'x'; o['1'] + o[1]", "xx");
    assert_string("String([10, 20][1n])", "20");
    assert_bool("1n in { 1: 0 }", true);
    assert_bool("let o = { 2: 1 }; delete o[2n]; !('2' in o)", true);
    assert_string(
        &format!("let o = {{}}; o[{TWO_64}n] = 'big'; Object.keys(o).join()"),
        TWO_64,
    );
}

#[test]
fn primitive_receiver() {
    assert_bool("(1n).x === undefined", true);
    assert_bool(
        "Object.getPrototypeOf(Object(1n)) === Object.getPrototypeOf(Object(2n))",
        true,
    );
    assert_bool(
        "try { (1n).x = 1; false } catch (e) { e instanceof TypeError }",
        true,
    );
    assert_string("Object.keys(Object(1n)).length + ''", "0");
}

// --- JSON and ToNumber -------------------------------------------------------

#[test]
fn json_stringify_throws() {
    let message = "Do not know how to serialize a BigInt";
    for src in [
        "JSON.stringify(1n)",
        "JSON.stringify({ a: [1n] })",
        "JSON.stringify(Object(1n))",
    ] {
        assert_string(
            &format!("try {{ {src}; 'none' }} catch (e) {{ e instanceof TypeError && e.message }}"),
            message,
        );
    }
    assert_string(
        "JSON.stringify({ a: 1n }, (k, v) => typeof v === 'bigint' ? String(v) : v)",
        "{\"a\":\"1\"}",
    );
}

#[test]
fn to_number_throws() {
    let message = "Cannot convert a BigInt value to a number";
    for src in ["+1n", "Math.abs(1n)", "isNaN(1n)"] {
        assert_string(
            &format!("try {{ {src}; 'none' }} catch (e) {{ e instanceof TypeError && e.message }}"),
            message,
        );
    }
    assert_uncaught("+1n", &format!("TypeError: {message}"));
}

// --- under gc_stress ---------------------------------------------------------

#[test]
fn multi_limb_conversions_survive_collections() {
    // 20 × (61 digits of 2^200 + 21 characters of -2^64).
    assert_same_under_gc_stress(
        &format!(
            "let parts = []; \
             for (let i = 0; i < 20; i++) parts.push(String({TWO_200}n) + `${{-{TWO_64}n}}`); \
             let m = new Map([[{TWO_200}n, 'k']]); \
             console.log(parts.join('').length, m.get({TWO_200}n), {TWO_200}n == '{TWO_200}'); \
             console.log(-{TWO_200}n)"
        ),
        &format!("1640 k true\n-{TWO_200}n\n"),
    );
}
