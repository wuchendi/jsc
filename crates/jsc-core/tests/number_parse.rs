//! Integration tests: the guest `StringToNumber`, `parseFloat` and `parseInt`
//! (design/runtime-memory.md §3.3), checked bit-for-bit against Rust's
//! correctly rounded `str::parse::<f64>`.
#![allow(clippy::expect_used)]

use std::fmt::Write as _;

mod common;
use common::{assert_bool, exec};

/// A JS expression for exactly `x`: `{:e}` round-trips, and the three values
/// without a literal form are spelled out.
fn literal(x: f64) -> String {
    if x.is_nan() {
        "NaN".to_owned()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else if x == 0.0 && x.is_sign_negative() {
        "-0".to_owned()
    } else {
        format!("{x:e}")
    }
}

/// Assert the JS expression `expr` is exactly `expected` (`-0` and `NaN`
/// included).
fn assert_same(expr: &str, expected: f64) {
    let lit = literal(expected);
    assert_bool(&format!("Object.is({expr}, {lit})"), true);
}

/// The Rust oracle for a decimal literal.
fn oracle(text: &str) -> f64 {
    text.parse::<f64>().expect("generated literal parses")
}

#[test]
fn string_to_number_slow_path() {
    for text in [
        "2.2250738585072011e-308",
        "2.2250738585072012e-308",
        "2.2250738585072014e-308",
        "1.00000000000000011102230246251565404236316680908203125",
        "1.00000000000000011102230246251565404236316680908203126",
        "9007199254740993.000000000000000000001",
        "123456789012345678901234567890",
        "0.000000000000000000000000000001234567890123456789012345",
        "1.7976931348623158079372897140530341507993413271003782693617e308",
        "1.7976931348623158079372897140530341507993413271003782693618e308",
        "4.9406564584124654417656879286822137236505980261432476442558e-324",
        "2.4703282292062327208507391753933e-324",
        "2.4703282292062327208507391753934e-324",
    ] {
        assert_same(&format!("Number(\"{text}\")"), oracle(text));
        assert_same(&format!("Number(\"  -{text}\\n\")"), -oracle(text));
    }
    assert_same(
        "JSON.parse(\"2.2250738585072011e-308\")",
        oracle("2.2250738585072011e-308"),
    );
    // A digit run past the 768-digit buffer still breaks the tie upward.
    let tail = format!(
        "1.00000000000000011102230246251565404236316680908203125{}1",
        "0".repeat(800)
    );
    assert_same(&format!("Number(\"{tail}\")"), oracle(&tail));
}

#[test]
fn string_to_number_grammar() {
    for (expr, expected) in [
        ("Number(\"\")", 0.0),
        ("Number(\" \\t\\n\")", 0.0),
        ("Number(\"-0\")", -0.0),
        ("Number(\"+5\")", 5.0),
        ("Number(\"5.\")", 5.0),
        ("Number(\".5\")", 0.5),
        ("Number(\"1E2\")", 100.0),
        ("Number(\"Infinity\")", f64::INFINITY),
        ("Number(\"-Infinity\")", f64::NEG_INFINITY),
        ("Number(\"0x1F\")", 31.0),
        ("Number(\"0o17\")", 15.0),
        ("Number(\"0B101\")", 5.0),
        ("Number(\"\\u20001.5\\uFEFF\")", 1.5),
        ("Number(\"1e400\")", f64::INFINITY),
        ("Number(\"1e-400\")", 0.0),
        ("Number(undefined)", f64::NAN),
        ("Number(null)", 0.0),
        ("Number(true)", 1.0),
        ("Number(false)", 0.0),
        ("Number(\"1e\")", f64::NAN),
        ("Number(\".\")", f64::NAN),
        ("Number(\"-\")", f64::NAN),
        ("Number(\"infinity\")", f64::NAN),
        ("Number(\"Infinityx\")", f64::NAN),
        ("Number(\"-0x10\")", f64::NAN),
        ("Number(\"0x\")", f64::NAN),
        ("Number(\"0b2\")", f64::NAN),
        ("Number(\"1 2\")", f64::NAN),
        ("Number(\"1\\uD83D\")", f64::NAN),
        ("Number(\"\\uD83D1\")", f64::NAN),
    ] {
        assert_same(expr, expected);
    }
    assert_bool("1 == \"1.0\"", true);
    assert_bool("\"0x10\" == 16", true);
}

#[test]
fn parse_int_power_of_two_radix_exact() {
    let two_53 = 9_007_199_254_740_992.0;
    for (expr, expected) in [
        // 2^53 + 1 ties to even; 2^53 + 3 ties up to 2^53 + 4.
        ("parseInt(\"20000000000001\", 16)", two_53),
        ("parseInt(\"20000000000003\", 16)", two_53 + 4.0),
        ("parseInt(\"-20000000000001\", 16)", -two_53),
        ("parseInt(\"0x20000000000001\")", two_53),
        ("Number(\"0x20000000000001\")", two_53),
        ("Number(\"0x20000000000003\")", two_53 + 4.0),
        ("parseInt(\"400000000000000002\", 8)", two_53 + 2.0),
        // 2^63 + 1: far below the 53-bit window.
        ("parseInt(\"8000000000001\", 32)", 2.0_f64.powi(63)),
    ] {
        assert_same(expr, expected);
    }
    let tie_even = format!("1{}1", "0".repeat(52));
    let tie_up = format!("1{}11", "0".repeat(51));
    assert_same(&format!("parseInt(\"{tie_even}\", 2)"), two_53);
    assert_same(&format!("parseInt(\"{tie_up}\", 2)"), two_53 + 4.0);
    assert_same(&format!("Number(\"0b{tie_even}\")"), two_53);
    // A set bit far below the window turns a tie into a round-up.
    let above = 9_007_199_254_740_994.0 * 2.0_f64.powi(56);
    assert_same("parseInt(\"2000000000000100000000000001\", 16)", above);
    assert_same(
        &format!("parseInt(\"1{}\", 2)", "0".repeat(1024)),
        f64::INFINITY,
    );
    assert_same(
        &format!("parseInt(\"1{}\", 2)", "0".repeat(1023)),
        2.0_f64.powi(1023),
    );
}

#[test]
fn parse_int_general() {
    for (expr, expected) in [
        ("parseInt(\"42px\")", 42.0),
        ("parseInt(\"  -17\")", -17.0),
        ("parseInt(\"-0\")", -0.0),
        ("parseInt(\"0x1f\")", 31.0),
        ("parseInt(\"0X1F\", 16)", 31.0),
        ("parseInt(\"0x1f\", 10)", 0.0),
        ("parseInt(\"z\", 36)", 35.0),
        ("parseInt(\"Zz\", 36)", 1295.0),
        ("parseInt(\"12\", 3)", 5.0),
        ("parseInt(\"12\", 37)", f64::NAN),
        ("parseInt(\"12\", 1)", f64::NAN),
        ("parseInt(\"12\", 4294967306)", 12.0),
        ("parseInt(\"12\", { valueOf() { return 8; } })", 10.0),
        ("parseInt(\"\")", f64::NAN),
        ("parseInt(\"0x\")", f64::NAN),
        ("parseInt(\"-\")", f64::NAN),
        ("parseInt(\"\\u00A0\\u20007\")", 7.0),
        ("parseInt(\"\\uD83D7\")", f64::NAN),
        ("parseInt(null, 36)", 1_112_745.0),
    ] {
        assert_same(expr, expected);
    }
    let long = "123456789012345678901234567890";
    assert_same(&format!("parseInt(\"{long}\")"), oracle(long));
}

#[test]
fn parse_float_prefix() {
    for (expr, expected) in [
        ("parseFloat(\"3.25abc\")", 3.25),
        ("parseFloat(\"  -.5e1x\")", -5.0),
        ("parseFloat(\"1e\")", 1.0),
        ("parseFloat(\"1e+\")", 1.0),
        ("parseFloat(\"1.2.3\")", 1.2),
        ("parseFloat(\"5.\")", 5.0),
        ("parseFloat(\"-0\")", -0.0),
        ("parseFloat(\"0x10\")", 0.0),
        ("parseFloat(\"Infinityx\")", f64::INFINITY),
        ("parseFloat(\"-Infinity\")", f64::NEG_INFINITY),
        ("parseFloat(\"\\u00A0 7\")", 7.0),
        ("parseFloat(\"\\uFEFF1\")", 1.0),
        ("parseFloat(\"\")", f64::NAN),
        ("parseFloat(\".\")", f64::NAN),
        ("parseFloat(\"-\")", f64::NAN),
        ("parseFloat(\"e5\")", f64::NAN),
        ("parseFloat(\"\\uD83D5\")", f64::NAN),
        ("parseFloat(\"infinity\")", f64::NAN),
    ] {
        assert_same(expr, expected);
    }
}

/// A fixed-seed xorshift64* generator.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-enough value in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let span = u64::try_from(hi - lo + 1).expect("non-empty range");
        lo + i64::try_from(self.next_u64() % span).expect("span fits i64")
    }

    fn digit(&mut self) -> char {
        char::from(b'0' + u8::try_from(self.next_u64() % 10).expect("single digit"))
    }

    fn digits(&mut self, count: usize) -> String {
        (0..count).map(|_| self.digit()).collect()
    }

    fn finite(&mut self) -> f64 {
        loop {
            let x = f64::from_bits(self.next_u64() & 0x7FFF_FFFF_FFFF_FFFF);
            if x.is_finite() {
                return x;
            }
        }
    }
}

/// `value * multiplier^times`, as base-1e9 limbs (least significant first).
fn scaled(value: u64, multiplier: u32, chunk: u32, times: usize) -> Vec<u32> {
    const BASE: u64 = 1_000_000_000;
    let mut limbs = vec![
        u32::try_from(value % BASE).expect("limb"),
        u32::try_from(value / BASE % BASE).expect("limb"),
        u32::try_from(value / BASE / BASE).expect("limb"),
    ];
    let mut left = times;
    while left > 0 {
        let (factor, used) = if left >= 13 {
            (chunk, 13)
        } else {
            (multiplier.pow(u32::try_from(left).expect("small")), left)
        };
        let mut carry = 0_u64;
        for limb in &mut limbs {
            let next = u64::from(*limb) * u64::from(factor) + carry;
            *limb = u32::try_from(next % BASE).expect("limb");
            carry = next / BASE;
        }
        while carry > 0 {
            limbs.push(u32::try_from(carry % BASE).expect("limb"));
            carry /= BASE;
        }
        left -= used;
    }
    limbs
}

/// The decimal digits of `limbs`, with no leading zeros.
fn limbs_to_string(limbs: &[u32]) -> String {
    let mut text = String::new();
    for limb in limbs.iter().rev() {
        write!(text, "{limb:09}").expect("writing to a String");
    }
    let trimmed = text.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }.to_owned()
}

/// The exact decimal expansion of the midpoint above finite positive `x`, as
/// `digits` + `e` + `exp10` (`(2m + 1) * 2^(e - 1)`).
fn exact_halfway(x: f64) -> String {
    let bits = x.to_bits();
    let raw = i32::try_from((bits >> 52) & 0x7FF).expect("11 bits");
    let frac = bits & ((1_u64 << 52) - 1);
    let (m, e) = if raw == 0 {
        (frac, -1074)
    } else {
        (frac | (1_u64 << 52), raw - 1075)
    };
    let odd = 2 * m + 1;
    let e = e - 1;
    if e >= 0 {
        let times = usize::try_from(e).expect("non-negative");
        limbs_to_string(&scaled(odd, 2, 1 << 13, times))
    } else {
        let times = usize::try_from(-e).expect("non-negative");
        let digits = limbs_to_string(&scaled(odd, 5, 1_220_703_125, times));
        format!("{digits}e{e}")
    }
}

/// The ≥ 20,000 decimal inputs of the differential sample.
fn differential_inputs() -> Vec<String> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut inputs: Vec<String> = [
        "4.9e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "2.2250738585072011e-308",
        "2.2250738585072014e-308",
        "1.7976931348623157e308",
        "1.7976931348623158e308",
        "1.7976931348623159e308",
        "1e309",
        "1e-400",
        "0e999999",
        "0.000e-999999",
        "9007199254740993",
        "9007199254740992e22",
        "9007199254740993e22",
        "1e22",
        "1e23",
        "123456789012345678e-22",
    ]
    .iter()
    .map(|text| (*text).to_owned())
    .collect();

    // Shortest forms of random doubles, in both notations.
    for _ in 0..4_000 {
        let x = rng.finite();
        inputs.push(format!("{x:e}"));
        inputs.push(format!("{x}"));
    }
    // 17-to-800-digit mantissas.
    for _ in 0..4_000 {
        let count = usize::try_from(rng.range(17, 800)).expect("positive");
        let digits = rng.digits(count);
        let point = usize::try_from(rng.range(0, 40))
            .expect("positive")
            .min(count);
        let exp = rng.range(-360, 330);
        inputs.push(format!("{}.{}e{exp}", &digits[..point], &digits[point..]));
    }
    // Halfway cases written out exactly, and the same plus a far set bit.
    for _ in 0..3_000 {
        let x = f64::from_bits(rng.next_u64() & 0x7FFF_FFFF_FFFF_FFFF);
        if !x.is_finite() || x == 0.0 {
            continue;
        }
        let half = exact_halfway(x);
        let tail = if let Some((digits, exp)) = half.split_once('e') {
            format!(
                "{digits}{}1e{}",
                "0".repeat(800),
                exp.parse::<i64>().expect("exp") - 801
            )
        } else {
            format!("{half}.{}1", "0".repeat(800))
        };
        inputs.push(half);
        inputs.push(tail);
    }
    // Subnormal and overflow boundaries.
    for _ in 0..1_500 {
        let count = usize::try_from(rng.range(1, 30)).expect("positive");
        let digits = rng.digits(count);
        let exp = if rng.next_u64().is_multiple_of(2) {
            rng.range(-345, -300)
        } else {
            rng.range(280, 320)
        };
        inputs.push(format!("{}.{digits}e{exp}", rng.digit()));
    }
    // Exponents forcing the fast and disguised fast paths.
    for _ in 0..2_000 {
        let mantissa = rng.next_u64() % (1_u64 << 53);
        inputs.push(format!("{mantissa}e{}", rng.range(-22, 45)));
    }
    // Signs on a slice of everything above.
    let signed: Vec<String> = inputs
        .iter()
        .step_by(7)
        .map(|text| format!("-{text}"))
        .collect();
    inputs.extend(signed);
    inputs
}

/// One program comparing `batch`, printing every mismatching input.
fn differential_program(batch: &[String]) -> String {
    let mut src =
        String::from("function t(s, v) { if (!Object.is(Number(s), v)) console.log(s); }\n");
    for text in batch {
        let lit = literal(oracle(text));
        writeln!(src, "t(\"{text}\", {lit});").expect("writing to a String");
    }
    src
}

#[test]
fn number_parse_differential() {
    let inputs = differential_inputs();
    assert!(inputs.len() >= 20_000, "only {} inputs", inputs.len());
    for batch in inputs.chunks(500) {
        let done = exec(&differential_program(batch), false).expect("batch compiles and runs");
        assert_eq!(done.code, 0, "batch failed: {}", done.stderr);
        assert_eq!(
            done.stdout, "",
            "Number(s) differs from str::parse for the listed inputs"
        );
    }
}
