//! Integration tests: the `BigInt` builtins and conversions (DESIGN.md
//! §6.10.7, stage B3) — the `BigInt` function and its coercions,
//! `BigInt.asIntN` / `BigInt.asUintN`, the `%BigInt.prototype%` methods and
//! `[@@toStringTag]`, wrapper objects, `Number(bigint)` and the builtins that
//! convert a `BigInt` on their way (`Math`, `isNaN`, `Number.is*`, `sort`,
//! `Object.is`, `parseInt` / `parseFloat`). Operators are B2b's: every value
//! here is a literal or a builtin result.

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_same_under_gc_stress, assert_string,
    assert_uncaught,
};

/// `2^64`.
const TWO_64: &str = "18446744073709551616";
/// `2^200`.
const TWO_200: &str = "1606938044258990275541962092341162602522202993782792835301376";
/// `2^200 - 12345`.
const TWO_200_LESS: &str = "1606938044258990275541962092341162602522202993782792835289031";

/// The name and message of the error `src` throws, as `"<name>: <message>"`
/// (`"none"` when it completes).
fn thrown(src: &str) -> String {
    format!("try {{ {src}; 'none' }} catch (e) {{ e.name + ': ' + e.message }}")
}

/// Assert `src` throws an error of `kind` with any message.
fn assert_throws(src: &str, kind: &str) {
    assert_bool(
        &format!("try {{ {src}; false }} catch (e) {{ e instanceof {kind} }}"),
        true,
    );
}

/// The digits of `value` in `radix` (lowercase), the reference for
/// `toString(radix)`.
fn radix_digits(mut value: u128, radix: u32) -> String {
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(digits[(value % u128::from(radix)) as usize]);
        value /= u128::from(radix);
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

// --- the BigInt function -----------------------------------------------------

#[test]
fn bigint_of_numbers() {
    assert_eval("BigInt(1)", "1n");
    assert_eval("BigInt(-7)", "-7n");
    assert_eval("BigInt(-0)", "0n");
    assert_eval("BigInt(9007199254740991)", "9007199254740991n");
    assert_eval("BigInt(2 ** 64)", &format!("{TWO_64}n"));
    assert_eval("BigInt(1e21)", "1000000000000000000000n");
    assert_eval("BigInt(-1e21)", "-1000000000000000000000n");
    assert_eval(
        "BigInt(1e300)",
        "1000000000000000052504760255204420248704468581108159154915854115511802457988908195786371375080447864043704443832883878176942523235360430575644792184786706982848387200926575803737830233794788090059368953234970799945081119038967640880074652742780142494579258788820056842838115669472196386865459400540160n",
    );
}

#[test]
fn bigint_of_non_integers_is_a_range_error() {
    assert_string(
        &thrown("BigInt(1.5)"),
        "RangeError: The number 1.5 cannot be converted to a BigInt because it is not an integer",
    );
    assert_string(
        &thrown("BigInt(NaN)"),
        "RangeError: The number NaN cannot be converted to a BigInt because it is not an integer",
    );
    for src in [
        "BigInt(Infinity)",
        "BigInt(-Infinity)",
        "BigInt(0.1)",
        "BigInt(-2.5)",
        "BigInt({ valueOf() { return 0.5 } })",
    ] {
        assert_throws(src, "RangeError");
    }
    assert_uncaught(
        "BigInt(1.5)",
        "RangeError: The number 1.5 cannot be converted to a BigInt because it is not an integer",
    );
}

#[test]
fn bigint_of_booleans_strings_and_objects() {
    assert_eval("BigInt(true)", "1n");
    assert_eval("BigInt(false)", "0n");
    assert_eval("BigInt('0x1F')", "31n");
    assert_eval("BigInt('0X1f')", "31n");
    assert_eval("BigInt('0o17')", "15n");
    assert_eval("BigInt('0b101')", "5n");
    assert_eval("BigInt(' 12 ')", "12n");
    assert_eval("BigInt('\\n-7\\t')", "-7n");
    assert_eval("BigInt('+5')", "5n");
    assert_eval("BigInt('')", "0n");
    assert_eval("BigInt('   ')", "0n");
    assert_eval(&format!("BigInt('{TWO_200}')"), &format!("{TWO_200}n"));
    assert_eval(&format!("BigInt('-{TWO_64}')"), &format!("-{TWO_64}n"));
    assert_eval("BigInt(Object(3n))", "3n");
    assert_eval("BigInt(5n)", "5n");
    assert_eval("BigInt({ valueOf() { return 4 } })", "4n");
    assert_eval("BigInt({ valueOf() { return '6' } })", "6n");
    assert_eval(
        "BigInt({ toString() { return '8' }, valueOf: undefined })",
        "8n",
    );
}

#[test]
fn bigint_of_unparsable_strings_is_a_syntax_error() {
    assert_string(
        &thrown("BigInt('1n')"),
        "SyntaxError: Cannot convert 1n to a BigInt",
    );
    for src in [
        "BigInt('1.5')",
        "BigInt('0x')",
        "BigInt('-0x1')",
        "BigInt('1e3')",
        "BigInt('abc')",
        "BigInt('Infinity')",
        "BigInt('1_000')",
        "BigInt('0b2')",
        // The hint is `number`: `Symbol.toPrimitive` returns "number".
        "BigInt({ [Symbol.toPrimitive](hint) { return hint } })",
    ] {
        assert_throws(src, "SyntaxError");
    }
}

#[test]
fn bigint_of_undefined_null_and_symbols_is_a_type_error() {
    assert_string(
        &thrown("BigInt()"),
        "TypeError: Cannot convert undefined to a BigInt",
    );
    assert_string(
        &thrown("BigInt(undefined)"),
        "TypeError: Cannot convert undefined to a BigInt",
    );
    assert_string(
        &thrown("BigInt(null)"),
        "TypeError: Cannot convert null to a BigInt",
    );
    assert_throws("BigInt(Symbol())", "TypeError");
}

#[test]
fn new_bigint_is_a_type_error_before_converting() {
    assert_string(
        &thrown("new BigInt(1)"),
        "TypeError: BigInt is not a constructor",
    );
    assert_string(
        &thrown("new BigInt({ valueOf() { throw new RangeError('x') } })"),
        "TypeError: BigInt is not a constructor",
    );
    assert_uncaught("new BigInt()", "TypeError: BigInt is not a constructor");
}

#[test]
fn bigint_function_properties() {
    assert_string("typeof BigInt", "function");
    assert_number("BigInt.length", 1.0);
    assert_string("BigInt.name", "BigInt");
    assert_bool("globalThis.BigInt === BigInt", true);
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(globalThis, 'BigInt'); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "true false true",
    );
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(BigInt, 'prototype'); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "false false false",
    );
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(BigInt, 'length'); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "false false true",
    );
    assert_bool("BigInt.prototype.constructor === BigInt", true);
    assert_bool("Object.getPrototypeOf(BigInt) === Function.prototype", true);
    assert_bool(
        "Object.getPrototypeOf(BigInt.prototype) === Object.prototype",
        true,
    );
    assert_bool(
        "Object.getPrototypeOf(Object(1n)) === BigInt.prototype",
        true,
    );
    assert_bool("(1n).constructor === BigInt", true);
}

// --- asIntN / asUintN --------------------------------------------------------

#[test]
fn as_int_n_and_as_uint_n_edge_widths() {
    let check = |src: &str, expected: &str| assert_eval(src, expected);
    check(
        "BigInt.asIntN(64, 9223372036854775808n)",
        "-9223372036854775808n",
    );
    check(
        "BigInt.asIntN(64, 9223372036854775807n)",
        "9223372036854775807n",
    );
    check("BigInt.asUintN(64, -1n)", "18446744073709551615n");
    check("BigInt.asIntN(64, 18446744073709551615n)", "-1n");
    check(&format!("BigInt.asUintN(64, -{TWO_64}n)"), "0n");
    check("BigInt.asIntN(0, 5n)", "0n");
    check("BigInt.asUintN(0, -5n)", "0n");
    check("BigInt.asIntN(1, 1n)", "-1n");
    check("BigInt.asIntN(1, 2n)", "0n");
    check("BigInt.asIntN(1, -1n)", "-1n");
    check("BigInt.asUintN(1, -1n)", "1n");
    check("BigInt.asUintN(1, 3n)", "1n");
    check("BigInt.asIntN(32, 0xffffffffn)", "-1n");
    check("BigInt.asIntN(33, 0xffffffffn)", "4294967295n");
    check("BigInt.asUintN(32, -0x80000000n)", "2147483648n");
    check(
        &format!("BigInt.asIntN(65, {TWO_64}n)"),
        &format!("-{TWO_64}n"),
    );
    check(
        "BigInt.asIntN(65, -18446744073709551617n)",
        "18446744073709551615n",
    );
    check("BigInt.asUintN(65, -1n)", "36893488147419103231n");
    check(
        "BigInt.asUintN(200, -1n)",
        "1606938044258990275541962092341162602522202993782792835301375n",
    );
    check("BigInt.asIntN(200, -1n)", "-1n");
    check(&format!("BigInt.asIntN(100, {TWO_200_LESS}n)"), "-12345n");
    check(
        &format!("BigInt.asUintN(100, {TWO_200_LESS}n)"),
        "1267650600228229401496703193031n",
    );
    check(
        &format!("BigInt.asUintN(300, {TWO_200}n)"),
        &format!("{TWO_200}n"),
    );
    check("BigInt.asUintN(8, 0n)", "0n");
    check("BigInt.asUintN(9007199254740991, 5n)", "5n");
    check("BigInt.asIntN(9007199254740991, -5n)", "-5n");
}

#[test]
fn as_n_converts_bits_with_to_index_then_the_value_with_to_big_int() {
    assert_eval("BigInt.asIntN(undefined, 5n)", "0n");
    assert_eval("BigInt.asIntN('8', 255n)", "-1n");
    assert_eval("BigInt.asIntN(2.9, 3n)", "-1n");
    assert_eval("BigInt.asIntN(-0.5, 3n)", "0n");
    assert_eval("BigInt.asUintN(8, '0x1ff')", "255n");
    assert_eval("BigInt.asUintN(8, true)", "1n");
    assert_eval("BigInt.asUintN(8, Object(257n))", "1n");
    let index = "RangeError: Invalid value: not (convertible to) a safe integer";
    assert_string(&thrown("BigInt.asIntN(-1, 0n)"), index);
    assert_string(&thrown("BigInt.asUintN(9007199254740992, 0n)"), index);
    assert_string(&thrown("BigInt.asUintN(Infinity, 0n)"), index);
    // ToIndex runs before ToBigInt.
    assert_string(&thrown("BigInt.asIntN(-1, Symbol())"), index);
    assert_throws("BigInt.asIntN(1n, 1n)", "TypeError");
    assert_throws("BigInt.asUintN(8, 1)", "TypeError");
    assert_throws("BigInt.asUintN(8)", "TypeError");
    assert_throws("BigInt.asUintN(8, '1.5')", "SyntaxError");
    assert_string(
        "let log = []; \
         try { BigInt.asIntN({ valueOf() { log.push('bits'); return 8 } }, \
                             { valueOf() { log.push('value'); return 1 } }) } \
         catch (e) { log.push(e.name) } log.join()",
        "bits,value,TypeError",
    );
}

#[test]
fn as_uint_n_of_a_negative_value_checks_the_size() {
    assert_string(
        &thrown("BigInt.asUintN(9007199254740991, -1n)"),
        "RangeError: Maximum BigInt size exceeded",
    );
    assert_string(
        &thrown("BigInt.asUintN(524289 * 32, -1n)"),
        "RangeError: Maximum BigInt size exceeded",
    );
}

#[test]
fn as_n_properties() {
    assert_number("BigInt.asIntN.length", 2.0);
    assert_number("BigInt.asUintN.length", 2.0);
    assert_string("BigInt.asIntN.name", "asIntN");
    assert_string("BigInt.asUintN.name", "asUintN");
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(BigInt, 'asIntN'); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "true false true",
    );
}

// --- %BigInt.prototype% ------------------------------------------------------

#[test]
fn to_string_radixes() {
    assert_string("(255n).toString(16)", "ff");
    assert_string("(255n).toString(2)", "11111111");
    assert_string("(-255n).toString(36)", "-73");
    assert_string("(0n).toString(7)", "0");
    assert_string("1n.toString()", "1");
    assert_string("(-12n).toString()", "-12");
    assert_string("(255n).toString(undefined)", "255");
    assert_string("(255n).toString('16')", "ff");
    assert_string("(255n).toString(16.9)", "ff");
    assert_string(&format!("({TWO_200}n).toString()"), TWO_200);
    assert_string(
        &format!("({TWO_200}n).toString(36)"),
        "bnklg118comha6gqury14067gur54n8won6guf4",
    );
    assert_string(
        &format!("({TWO_200}n).toString(16)"),
        &format!("1{}", "0".repeat(50)),
    );
    assert_string(
        &format!("({TWO_64}n).toString(2)"),
        &format!("1{}", "0".repeat(64)),
    );
    assert_string(
        &format!("(-{TWO_200}n).toString(7)"),
        "-141246066533632643213232344050606053061443446006544361632102630555343054",
    );
}

#[test]
fn to_string_every_radix_of_a_multi_limb_value() {
    let value: u128 = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef;
    for radix in 2..=36 {
        assert_string(
            &format!("({value}n).toString({radix})"),
            &radix_digits(value, radix),
        );
        assert_string(
            &format!("(-{value}n).toString({radix})"),
            &format!("-{}", radix_digits(value, radix)),
        );
    }
}

#[test]
fn to_string_radix_out_of_range_is_a_range_error() {
    let message = "RangeError: toString() radix must be between 2 and 36";
    for radix in ["1", "37", "0", "-2", "Infinity", "NaN"] {
        assert_string(&thrown(&format!("(1n).toString({radix})")), message);
    }
    assert_throws("(1n).toString(1n)", "TypeError");
}

#[test]
fn this_big_int_value() {
    assert_string("BigInt.prototype.toString.call(Object(10n), 16)", "a");
    assert_bool("BigInt.prototype.valueOf.call(Object(5n)) === 5n", true);
    assert_bool("BigInt.prototype.valueOf.call(5n) === 5n", true);
    assert_string(
        "BigInt.prototype.toLocaleString.call(Object(1234n))",
        "1234",
    );
    assert_string(
        &thrown("BigInt.prototype.toString.call(1)"),
        "TypeError: BigInt.prototype.toString requires that 'this' be a BigInt",
    );
    assert_string(
        &thrown("BigInt.prototype.valueOf.call('1')"),
        "TypeError: BigInt.prototype.valueOf requires that 'this' be a BigInt",
    );
    assert_string(
        &thrown("BigInt.prototype.toLocaleString.call({})"),
        "TypeError: BigInt.prototype.toLocaleString requires that 'this' be a BigInt",
    );
    for receiver in [
        "BigInt.prototype",
        "undefined",
        "null",
        "Object(1)",
        "Symbol()",
    ] {
        assert_throws(
            &format!("BigInt.prototype.valueOf.call({receiver})"),
            "TypeError",
        );
    }
    // `thisBigIntValue` runs before the radix conversion.
    assert_throws(
        "BigInt.prototype.toString.call(1, { valueOf() { throw new RangeError('r') } })",
        "TypeError",
    );
}

#[test]
fn prototype_methods_and_tag() {
    assert_string("(1234n).toLocaleString()", "1234");
    assert_string(
        &format!("(-{TWO_64}n).toLocaleString()"),
        &format!("-{TWO_64}"),
    );
    assert_bool("(7n).valueOf() === 7n", true);
    assert_number("BigInt.prototype.toString.length", 0.0);
    assert_number("BigInt.prototype.toLocaleString.length", 0.0);
    assert_number("BigInt.prototype.valueOf.length", 0.0);
    assert_string("BigInt.prototype.valueOf.name", "valueOf");
    assert_string("BigInt.prototype[Symbol.toStringTag]", "BigInt");
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(BigInt.prototype, Symbol.toStringTag); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "false false true",
    );
    assert_string(
        "let d = Object.getOwnPropertyDescriptor(BigInt.prototype, 'toString'); \
         `${d.writable} ${d.enumerable} ${d.configurable}`",
        "true false true",
    );
    assert_string("Object.prototype.toString.call(1n)", "[object BigInt]");
    assert_string(
        "Object.prototype.toString.call(Object(1n))",
        "[object BigInt]",
    );
    assert_string("`${{}.toString.call(0n)}`", "[object BigInt]");
}

// --- wrapper objects ---------------------------------------------------------

#[test]
fn wrapper_objects() {
    assert_string("typeof Object(1n)", "object");
    assert_bool("Object(1n) == 1n", true);
    assert_bool("1n == Object(1n)", true);
    assert_bool("Object(1n) === 1n", false);
    assert_bool("Object(2n) == 2", true);
    assert_bool("Object(2n) == '2'", true);
    assert_bool("Object(1n) instanceof BigInt", true);
    assert_bool("1n instanceof BigInt", false);
    assert_string("String(Object(7n))", "7");
    assert_string("`${Object(-7n)}`", "-7");
    assert_number("Number(Object(5n))", 5.0);
    assert_eval("Object(3n).valueOf()", "3n");
    assert_eval("Object(3n)", "[BigInt: 3n]");
    // A user `valueOf` on the prototype is what ToPrimitive of a wrapper calls.
    assert_bool(
        "BigInt.prototype.valueOf = function () { return 9n }; Object(1n) == 9n",
        true,
    );
}

// --- conversions on builtin paths --------------------------------------------

#[test]
fn number_of_a_bigint() {
    assert_number("Number(1n)", 1.0);
    assert_number("Number(-1n)", -1.0);
    assert_number("Number(0n)", 0.0);
    assert_number("Number(9007199254740993n)", 9_007_199_254_740_992.0);
    assert_number("Number(9007199254740995n)", 9_007_199_254_740_996.0);
    assert_number(
        "Number(18446744073709551615n)",
        18_446_744_073_709_551_616.0,
    );
    assert_number(&format!("Number({TWO_200}n)"), 2f64.powi(200));
    let two_1024 = format!("0x1{}n", "0".repeat(256));
    assert_number(&format!("Number({two_1024})"), f64::INFINITY);
    assert_number(&format!("Number(-{two_1024})"), f64::NEG_INFINITY);
    assert_string("typeof new Number(1n)", "object");
    assert_number("new Number(5n).valueOf()", 5.0);
    assert_number("Number.call(undefined, 3n)", 3.0);
    assert_number("let n = Number; n(4n)", 4.0);
    // The non-BigInt paths are unchanged.
    assert_number("Number('12')", 12.0);
    assert_number("Number()", 0.0);
    assert_string("String(Number(undefined))", "NaN");
    assert_throws("Number(Symbol())", "TypeError");
    // Unary plus stays ToNumber.
    assert_throws("+1n", "TypeError");
}

#[test]
fn string_of_a_bigint() {
    assert_string("String(1n)", "1");
    assert_string(&format!("String(-{TWO_200}n)"), &format!("-{TWO_200}"));
    assert_string("new String(12n).valueOf()", "12");
}

#[test]
fn to_number_builtins_throw() {
    let message = "TypeError: Cannot convert a BigInt value to a number";
    for src in [
        "Math.floor(1n)",
        "Math.max(1, 1n)",
        "Math.pow(2n, 1n)",
        "Math.sign(Object(1n))",
        "isNaN(1n)",
        "isFinite(1n)",
        "new Date(0).setFullYear(1n)",
    ] {
        assert_string(&thrown(src), message);
    }
}

#[test]
fn number_predicates_do_not_coerce() {
    for name in ["isNaN", "isFinite", "isInteger", "isSafeInteger"] {
        assert_bool(&format!("Number.{name}(1n)"), false);
        assert_bool(&format!("Number.{name}(0n)"), false);
    }
}

#[test]
fn sort_object_is_and_parsing() {
    assert_eval("[3n, 20n, 1n].sort()", "[ 1n, 20n, 3n ]");
    assert_eval("[-1n, 10n, 2n].sort()", "[ -1n, 10n, 2n ]");
    assert_bool("Object.is(1n, 1n)", true);
    assert_bool("Object.is(0n, -0n)", true);
    assert_bool(&format!("Object.is({TWO_64}n, {TWO_64}n)"), true);
    assert_bool("Object.is(1n, 1)", false);
    assert_bool("Object.is(1n, Object(1n))", false);
    assert_number("parseInt(15n)", 15.0);
    assert_number("parseInt(0x10n, 16)", 22.0);
    assert_number(
        &format!("parseInt({TWO_64}n)"),
        18_446_744_073_709_551_616.0,
    );
    assert_number("parseFloat(12n)", 12.0);
    assert_number("parseFloat(-3n)", -3.0);
}

// --- under gc_stress ---------------------------------------------------------

#[test]
fn radix_conversions_survive_collections() {
    // Every radix of a 200-bit value, parsed back from hex, and the
    // `asUintN` / `asIntN` results, with a collection at every allocation.
    assert_same_under_gc_stress(
        &format!(
            "let v = {TWO_200_LESS}n, total = 0; \
             for (let r = 2; r <= 36; r++) total += v.toString(r).length; \
             let back = BigInt('0x' + v.toString(16)) === v && BigInt(v.toString(10)) === v \
               && BigInt('0o' + v.toString(8)) === v; \
             console.log(total, back); \
             console.log(BigInt.asUintN(256, -{TWO_200_LESS}n)); \
             console.log(BigInt.asIntN(100, v), Number(v) === 2 ** 200); \
             console.log(BigInt(' 0b' + v.toString(2) + ' ') === v)"
        ),
        "2046 true\n\
         115792089237316193816632940749697632311307892324477961517254590225120294350905n\n\
         -12345n true\n\
         true\n",
    );
}
