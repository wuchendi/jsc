//! Integration tests: the guest `Number` to string conversions (Ryu shortest
//! digits, exact-decimal `toFixed` / `toExponential` / `toPrecision`, radix
//! `toString`), checked by hand-picked values and by a large differential
//! sample against Rust's own formatting.

mod common;
use common::assert_string;

/// Statements per compiled program of a differential sample.
const BATCH: usize = 400;

/// Compile and run `checks` (`c(actual, expected);` statements) in batches;
/// a mismatch prints the expected and actual text, naming the value.
fn run_checks(checks: &[String]) -> anyhow::Result<()> {
    for batch in checks.chunks(BATCH) {
        let mut src =
            String::from("function c(a, b) { if (a !== b) console.log(b + ' got ' + a); }\n");
        for check in batch {
            src.push_str(check);
            src.push('\n');
        }
        let done = common::exec(&src, false)?;
        anyhow::ensure!(done.code == 0, "batch failed: {}", done.stderr);
        anyhow::ensure!(done.stdout.is_empty(), "mismatches:\n{}", done.stdout);
    }
    Ok(())
}

/// The fixed-seed `xorshift64*` generator of the samples.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A finite double from random bits.
    fn finite(&mut self) -> f64 {
        loop {
            let x = f64::from_bits(self.next());
            if x.is_finite() {
                return x;
            }
        }
    }
}

/// `x` as a JS expression that round-trips.
fn literal(x: f64) -> String {
    format!("({x:e})")
}

/// `(significant digits, n)` of a finite nonzero `|x|`, from Rust's shortest
/// `{:e}`: `|x| = 0.digits * 10^n`.
fn shortest_digits(x: f64) -> (String, i32) {
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp.parse::<i32>().unwrap_or(0) + 1)
}

/// `d.ddd` + `e±k` of `digits` with exponent `e`.
fn exponential(sign: &str, digits: &str, e: i32) -> String {
    let (head, tail) = digits.split_at(1);
    let dot = if tail.is_empty() { "" } else { "." };
    let e_sign = if e < 0 { '-' } else { '+' };
    format!("{sign}{head}{dot}{tail}e{e_sign}{}", e.abs())
}

/// ECMA-262 `Number::toString` of a finite `x`, laid out from Rust's digits.
fn es_to_string(x: f64) -> String {
    if x == 0.0 {
        return "0".to_owned();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let (digits, n) = shortest_digits(x);
    let k = i32::try_from(digits.len()).unwrap_or(0);
    let body = if (1..=21).contains(&n) {
        if n >= k {
            format!("{digits}{}", "0".repeat((n - k).unsigned_abs() as usize))
        } else {
            let (int, frac) = digits.split_at(n.unsigned_abs() as usize);
            format!("{int}.{frac}")
        }
    } else if (-5..=0).contains(&n) {
        format!("0.{}{digits}", "0".repeat(n.unsigned_abs() as usize))
    } else {
        exponential("", &digits, n - 1)
    };
    format!("{sign}{body}")
}

/// The exact decimal expansion of a finite nonzero `|x|`: its significant
/// digits (no leading or trailing zeros) and `n` with `|x| = 0.digits * 10^n`.
fn exact_digits(x: f64) -> (Vec<u8>, i32) {
    let text = format!("{:.1100}", x.abs());
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let int = int.trim_start_matches('0');
    let (mut digits, n): (Vec<u8>, i32) = if int.is_empty() {
        let lead = frac.bytes().take_while(|&b| b == b'0').count();
        (
            frac.bytes().skip(lead).collect(),
            -i32::try_from(lead).unwrap_or(0),
        )
    } else {
        (
            int.bytes().chain(frac.bytes()).collect(),
            i32::try_from(int.len()).unwrap_or(0),
        )
    };
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    (digits, n)
}

/// When exactly `keep` digits of `(digits, n)` end on an exact tie, the
/// digits and `n` the spec picks (the larger `n`, away from zero).
fn tie_up(digits: &[u8], n: i32, keep: usize) -> Option<(String, i32)> {
    if digits.len() != keep + 1 || digits[keep] != b'5' {
        return None;
    }
    let mut out = digits[..keep].to_vec();
    for slot in out.iter_mut().rev() {
        if *slot == b'9' {
            *slot = b'0';
        } else {
            *slot += 1;
            return Some((String::from_utf8_lossy(&out).into_owned(), n));
        }
    }
    out.insert(0, b'1');
    out.truncate(keep.max(1));
    Some((String::from_utf8_lossy(&out).into_owned(), n + 1))
}

/// `(digits, e)` of a finite nonzero `x` rounded to `keep >= 1` significant
/// digits: Rust's exact `{:.*e}`, except for exact ties.
fn significant(x: f64, keep: usize) -> (String, i32) {
    let (exact, n) = exact_digits(x);
    if let Some((digits, n)) = tie_up(&exact, n, keep) {
        return (digits, n - 1);
    }
    let sci = format!("{:.*e}", keep - 1, x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp.parse().unwrap_or(0))
}

/// `Number.prototype.toFixed(f)` of a finite nonzero `x`.
fn expected_fixed(x: f64, f: usize) -> String {
    if x.abs() >= 1e21 {
        return es_to_string(x);
    }
    let (exact, n) = exact_digits(x);
    let keep = i32::try_from(f).unwrap_or(0) + n;
    if let Ok(keep) = usize::try_from(keep)
        && let Some((digits, carried)) = tie_up(&exact, n, keep)
    {
        let sign = if x < 0.0 { "-" } else { "" };
        let mut text = digits;
        // With nothing kept (`0.5` at `f` digits) the carry is the only digit.
        if carried > n && keep > 0 {
            text.push('0');
        }
        if text.len() <= f {
            text = format!("{}{text}", "0".repeat(f + 1 - text.len()));
        }
        let (int, frac) = text.split_at(text.len() - f);
        let dot = if f == 0 { "" } else { "." };
        return format!("{sign}{int}{dot}{frac}");
    }
    format!("{x:.f$}")
}

/// `Number.prototype.toExponential(f)` of a finite nonzero `x`.
fn expected_exponential(x: f64, f: usize) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let (digits, e) = significant(x, f + 1);
    exponential(sign, &digits, e)
}

/// `Number.prototype.toPrecision(p)` of a finite nonzero `x`.
fn expected_precision(x: f64, p: usize) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let (digits, e) = significant(x, p);
    let width = i32::try_from(p).unwrap_or(0);
    if e < -6 || e >= width {
        return exponential(sign, &digits, e);
    }
    if e == width - 1 {
        return format!("{sign}{digits}");
    }
    if e >= 0 {
        let (int, frac) = digits.split_at(e.unsigned_abs() as usize + 1);
        return format!("{sign}{int}.{frac}");
    }
    let zeros = "0".repeat((-(e + 1)).unsigned_abs() as usize);
    format!("{sign}0.{zeros}{digits}")
}

/// The sample of doubles of [`number_to_string_shortest_roundtrip`].
fn shortest_sample() -> Vec<f64> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut sample = Vec::new();
    for _ in 0..10_000 {
        sample.push(rng.finite());
    }
    for _ in 0..2_000 {
        let bits = rng.next() & ((1 << 52) - 1);
        sample.push(f64::from_bits(bits));
    }
    let mut around = |x: f64| {
        sample.push(x);
        sample.push(x.next_up());
        sample.push(x.next_down());
    };
    for e in -1074..=1023 {
        around(2f64.powi(e));
    }
    for e in -323..=308 {
        around(format!("1e{e}").parse().unwrap_or(1.0));
    }
    let two53 = 2f64.powi(53);
    for k in 0..100u32 {
        around(two53 + f64::from(k));
        around(two53 - f64::from(k));
        around(1e21 + f64::from(k) * 131_072.0);
    }
    for _ in 0..3_000 {
        let n = rng.next() % 1_000_000;
        let k = rng.next() % 12;
        let x: f64 = format!("{n}e-{k}").parse().unwrap_or(0.0);
        sample.push(x);
    }
    sample.retain(|x| x.is_finite());
    sample
}

#[test]
fn number_to_string_shortest_roundtrip() -> anyhow::Result<()> {
    assert_string("String(5e-324)", "5e-324");
    assert_string("String(1.7976931348623157e308)", "1.7976931348623157e+308");
    assert_string("String(0.1 + 0.2)", "0.30000000000000004");
    assert_string("String(1e21)", "1e+21");
    assert_string("String(1e-7)", "1e-7");
    assert_string("String(123e-20)", "1.23e-18");
    assert_string("String(0)", "0");
    assert_string("String(-0)", "0");
    assert_string("String(0 / 0)", "NaN");
    assert_string("String(1 / 0)", "Infinity");
    assert_string("String(-1 / 0)", "-Infinity");
    assert_string("String(true) + String(false)", "truefalse");
    assert_string("String(null) + String(undefined)", "nullundefined");

    let sample = shortest_sample();
    assert!(sample.len() >= 20_000, "sample of {}", sample.len());
    let checks: Vec<String> = sample
        .iter()
        .map(|&x| format!("c(String({}), {:?});", literal(x), es_to_string(x)))
        .collect();
    run_checks(&checks)
}

#[test]
fn exponential_thresholds_output() {
    assert_string("String(1e20)", "100000000000000000000");
    assert_string("String(123456789012345680000)", "123456789012345680000");
    assert_string("String(1e21)", "1e+21");
    assert_string("String(-1e21)", "-1e+21");
    assert_string("String(1.5e21)", "1.5e+21");
    assert_string("String(1e-6)", "0.000001");
    assert_string("String(1.5e-6)", "0.0000015");
    assert_string("String(1e-7)", "1e-7");
    assert_string("String(-1e-7)", "-1e-7");
    assert_string("String(1.5e-7)", "1.5e-7");
    assert_string("String(1e100)", "1e+100");
    assert_string("String(1e-100)", "1e-100");
    assert_string("'' + 1234.5", "1234.5");
    assert_string("'' + 0.001", "0.001");
    assert_string(
        "JSON.stringify([1e21, 1.5, -0, 0 / 0])",
        "[1e+21,1.5,0,null]",
    );
}

#[test]
fn to_fixed_exact_ties() -> anyhow::Result<()> {
    assert_string("(0.5).toFixed(0)", "1");
    assert_string("(1.5).toFixed(0)", "2");
    assert_string("(2.5).toFixed(0)", "3");
    assert_string("(-2.5).toFixed(0)", "-3");
    assert_string("(1.25).toFixed(1)", "1.3");
    assert_string("(1.45).toFixed(1)", "1.4");
    assert_string("(0.05).toFixed(1)", "0.1");
    assert_string("(1.005).toFixed(2)", "1.00");
    assert_string("(0).toFixed(2)", "0.00");
    assert_string("(-0).toFixed(2)", "0.00");
    assert_string("(-0.0000001).toFixed(2)", "-0.00");
    assert_string("(0.000001).toFixed(7)", "0.0000010");
    assert_string("(1e21).toFixed(2)", "1e+21");
    assert_string("(123.456).toFixed(0)", "123");
    assert_string("(0.004).toFixed(1)", "0.0");

    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let mut checks = Vec::new();
    for f in [0usize, 1, 2, 3, 5, 10, 20, 100] {
        for _ in 0..500 {
            let x = rng.finite();
            if x != 0.0 {
                checks.push(format!(
                    "c({}.toFixed({f}), {:?});",
                    literal(x),
                    expected_fixed(x, f)
                ));
            }
        }
    }
    // Dyadic rationals: exact ties at some digit counts.
    for numerator in 1..=512u32 {
        let x = f64::from(numerator) / 256.0;
        for f in 0..=8usize {
            checks.push(format!(
                "c({}.toFixed({f}), {:?});",
                literal(x),
                expected_fixed(x, f)
            ));
        }
    }
    run_checks(&checks)
}

#[test]
fn to_precision_and_exponential_exact() -> anyhow::Result<()> {
    assert_string("(1.5).toPrecision(1)", "2");
    assert_string("(2.5).toPrecision(1)", "3");
    assert_string("(123.456).toPrecision(4)", "123.5");
    assert_string("(1e21).toPrecision(3)", "1.00e+21");
    assert_string("(0.00001).toPrecision(1)", "0.00001");
    assert_string("(1e-7).toPrecision(2)", "1.0e-7");
    assert_string("(0).toPrecision(3)", "0.00");
    assert_string("(123.456).toExponential(2)", "1.23e+2");
    assert_string("(0).toExponential(1)", "0.0e+0");
    assert_string("(0).toExponential()", "0e+0");
    assert_string("(1.25).toExponential(1)", "1.3e+0");
    assert_string("(5e-324).toExponential()", "5e-324");
    assert_string("(123456).toExponential()", "1.23456e+5");
    assert_string("(-1.5).toExponential(0)", "-2e+0");
    assert_string("(1e21).toExponential(2)", "1.00e+21");
    assert_string("(1).toExponential(3)", "1.000e+0");
    assert_string("(1.5).toExponential(0)", "2e+0");
    assert_string("(0).toExponential(2)", "0.00e+0");
    assert_string("(1234.5678).toExponential()", "1.2345678e+3");
    assert_string("(-1.5).toExponential(1)", "-1.5e+0");
    assert_string("(9.5).toPrecision(1)", "1e+1");

    let mut rng = Rng(0xFEDC_BA98_7654_3210);
    let mut checks = Vec::new();
    for digits in [1usize, 2, 3, 5, 10, 17, 21, 50, 100] {
        for _ in 0..300 {
            let x = rng.finite();
            if x != 0.0 {
                checks.push(format!(
                    "c({}.toPrecision({digits}), {:?});",
                    literal(x),
                    expected_precision(x, digits)
                ));
                checks.push(format!(
                    "c({}.toExponential({}), {:?});",
                    literal(x),
                    digits - 1,
                    expected_exponential(x, digits - 1)
                ));
            }
        }
    }
    for numerator in 1..=512u32 {
        let x = f64::from(numerator) / 64.0;
        for digits in 1..=6usize {
            checks.push(format!(
                "c({}.toPrecision({digits}), {:?});",
                literal(x),
                expected_precision(x, digits)
            ));
            checks.push(format!(
                "c({}.toExponential({}), {:?});",
                literal(x),
                digits - 1,
                expected_exponential(x, digits - 1)
            ));
        }
    }
    run_checks(&checks)
}

/// V8's `DoubleToRadixCString`, the reference of the guest port.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn radix_reference(x: f64, radix: u32) -> String {
    let digit = |d: u32| char::from_digit(d, radix).unwrap_or('0');
    let base = f64::from(radix);
    let value = x.abs();
    let mut integer = value.floor();
    let mut fraction = value - integer;
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
            if (fraction > 0.5 || ((fraction - 0.5).abs() < f64::MIN_POSITIVE && d & 1 == 1))
                && fraction + delta > 1.0
            {
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

#[test]
fn radix_to_string_matches_v8() -> anyhow::Result<()> {
    assert_string("(255).toString(16)", "ff");
    assert_string("(-255).toString(2)", "-11111111");
    assert_string("(35).toString(36)", "z");
    assert_string("(0.5).toString(2)", "0.1");
    assert_string("(0.25).toString(16)", "0.4");
    assert_string("(0.1).toString(16)", "0.1999999999999a");
    assert_string(
        "(0.1).toString(2)",
        "0.0001100110011001100110011001100110011001100110011001101",
    );
    assert_string("(18446744073709551616).toString(16)", "10000000000000000");
    assert_string("(-0).toString(2)", "0");
    assert_string("(0 / 0).toString(2)", "NaN");
    assert_string("(1 / 0).toString(16)", "Infinity");
    assert_string("(-1 / 0).toString(16)", "-Infinity");
    assert_string("(5e-324).toString(2)", &format!("0.{}1", "0".repeat(1073)));
    assert_string(
        "(9007199254740992).toString(2)",
        &format!("1{}", "0".repeat(53)),
    );

    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let mut checks = Vec::new();
    for radix in [2u32, 3, 7, 8, 16, 32, 36] {
        for _ in 0..400 {
            let x = rng.finite();
            if x != 0.0 {
                checks.push(format!(
                    "c({}.toString({radix}), {:?});",
                    literal(x),
                    radix_reference(x, radix)
                ));
            }
        }
    }
    run_checks(&checks)
}

#[test]
fn message_builders_accept_symbols() {
    assert_string(
        "let m = ''; try { null[Symbol('k')]; } catch (e) { m = e.message; } m",
        "Cannot read properties of null (reading 'Symbol(k)')",
    );
    assert_string(
        "let m = ''; try { null[1.5]; } catch (e) { m = e.message; } m",
        "Cannot read properties of null (reading '1.5')",
    );
}
