//! Integration tests: `Math.*` methods and constants.
#![allow(clippy::expect_used)]

mod common;
use common::{
    assert_bool, assert_eval, assert_nan, assert_number, assert_output, assert_uncaught, eval_text,
};

/// Each acceptance bullet: the primary result of every recognized `Math.*` method.
#[test]
fn math_methods() {
    assert_number("Math.floor(3.7)", 3.0);
    assert_number("Math.max(1, 42)", 42.0);
    assert_number("Math.sqrt(1764)", 42.0);
    assert_number("Math.abs(-42)", 42.0);
    assert_number("Math.min(5, 2, 9)", 2.0);
    assert_number("Math.ceil(3.2)", 4.0);
    assert_number("Math.round(2.5)", 3.0);
    assert_number("Math.trunc(-3.7)", -3.0);
    assert_number("Math.pow(2, 10)", 1024.0);
}

/// `Math.round` uses JS `floor(x + 0.5)` semantics, NOT wasm banker's rounding:
/// `round(2.5) == 3` and `round(-2.5) == -2`.
#[test]
fn math_round_js_semantics() {
    assert_number("Math.round(2.5)", 3.0);
    assert_number("Math.round(-2.5)", -2.0);
    assert_number("Math.round(0.4)", 0.0);
    assert_bool("Object.is(Math.round(-0.5), -0)", true);
}

/// `Math.PI` / `Math.E` are constant property READS (no call), lowering to their
/// `f64` literals; `typeof Math.PI` is `"number"`.
#[test]
fn math_constants() {
    assert_number("Math.PI", std::f64::consts::PI);
    assert_number("Math.E", std::f64::consts::E);
    assert_eq!(eval_text("typeof Math.PI"), "number");
    // Usable in arithmetic like any number.
    assert_number("Math.PI * 2", std::f64::consts::PI * 2.0);
}

/// `min`/`max` are variadic: empty args yield the identity infinity, and any NaN
/// argument propagates to NaN (matching wasm `f64.min`/`f64.max`).
#[test]
fn math_min_max_edges() {
    assert_number("Math.min()", f64::INFINITY);
    assert_number("Math.max()", f64::NEG_INFINITY);
    assert_number("Math.min(7)", 7.0);
    // A NaN operand (`0/0`) propagates through the fold.
    assert_nan("Math.min(0 / 0, 1)");
    assert_nan("Math.max(1, 0 / 0)");
}

/// N5: empty-arg `min`/`max` RENDER as the identity infinities (`Infinity` /
/// `-Infinity`) via the shared number-rendering path, the `Infinity` identifier
/// is usable as an argument, and the non-empty folds stay unchanged.
#[test]
fn math_min_max_empty_arg_identity() {
    assert_eval("Math.max()", "-Infinity");
    assert_eval("Math.min()", "Infinity");
    assert_eval("Math.max(1, Infinity)", "Infinity");
    // Regression: the ordinary variadic folds are untouched.
    assert_eval("Math.max(1, 2, 3)", "3");
    assert_eval("Math.min(1, 2, 3)", "1");
    // Rendered identically when routed through `console.log`.
    assert_output(
        "console.log(Math.max(), Math.min())",
        "-Infinity Infinity\n",
    );
}

/// `Math.pow` is exact for integer exponents (including zero and negatives) and
/// composes with itself.
#[test]
fn math_pow_integer_exponents() {
    assert_number("Math.pow(2, 10)", 1024.0);
    assert_number("Math.pow(5, 0)", 1.0);
    assert_number("Math.pow(2, -2)", 0.25);
    assert_number("Math.pow(3, 3)", 27.0);
    assert_number("Math.pow(Math.pow(2, 3), 2)", 64.0);
}

/// Huge-magnitude integer exponents terminate (square-and-multiply over a halving
/// counter, not one multiply per unit) and saturate to the exact IEEE result.
#[test]
fn math_pow_large_exponents_terminate() {
    assert_eval("2 ** -2147483648", "0");
    assert_eval("1 ** -2147483648", "1");
    assert_eval("(-1) ** 2147483648", "1");
    assert_eval("(-1) ** 2147483647", "-1");
    assert_eval("0.5 ** -2147483648", "Infinity");
    assert_eval("2 ** 2147483648", "Infinity");
    assert_eval("Math.pow(2, -2147483648)", "0");
    assert_eval("Math.pow(-2, 1e308)", "Infinity");
    assert_eval("Math.pow(0.5, 1e308)", "0");
    assert_eval("Math.pow(2, 1023) === Infinity", "false");
    assert_eval("2 ** 1024", "Infinity");
    assert_eval("2 ** -1074 > 0", "true");
    assert_eval("1.5 ** 3", "3.375");
    assert_eval("4 ** 2.5", "32");
}

/// The spec's `Number::exponentiate` special cases: `NaN` and infinite exponents,
/// `±1` with an infinite exponent, and signed zero / infinity bases under a
/// non-integer exponent.
#[test]
fn math_pow_special_values() {
    assert_eval("2 ** NaN", "NaN");
    assert_eval("NaN ** 0", "1");
    assert_eval("NaN ** -0", "1");
    assert_eval("NaN ** 1", "NaN");
    assert_eval("1 ** Infinity", "NaN");
    assert_eval("(-1) ** -Infinity", "NaN");
    assert_eval("2 ** Infinity", "Infinity");
    assert_eval("2 ** -Infinity", "0");
    assert_eval("0.5 ** Infinity", "0");
    assert_eval("(-0.5) ** -Infinity", "Infinity");
    assert_eval("(-8) ** (1 / 3)", "NaN");
    assert_eval("1 / ((-0) ** 0.5)", "Infinity");
    assert_eval("(-0) ** -0.5", "Infinity");
    assert_eval("1 / ((-0) ** 3)", "-Infinity");
    assert_eval("(-Infinity) ** 0.5", "Infinity");
    assert_eval("(-Infinity) ** 3", "-Infinity");
    assert_eval("1 / ((-Infinity) ** -3)", "-Infinity");
    assert_eval("1 / ((-Infinity) ** -0.5)", "Infinity");
}

/// `Math` is not a real binding, so a local named `Math` shadows the built-in: the
/// member call then resolves to the user object's function value.
#[test]
fn math_is_shadowable() {
    assert_number(
        "let Math = { floor: function (x) { return 99 } }; Math.floor(3)",
        99.0,
    );
}

/// An unknown `Math.<name>` is an ordinary property read (`undefined`), so
/// calling it throws a catchable `TypeError` at run time.
#[test]
fn math_unknown_method_is_a_type_error() {
    assert_uncaught("Math.nope(2)", "TypeError: Math.nope is not a function");
}

/// `Math.*` results flow through `console.log` like any number.
#[test]
fn math_through_console_log() {
    assert_output("console.log(Math.max(1, 42), Math.floor(3.7))", "42 3\n");
}

/// `1 / x`, so a signed zero renders as `Infinity` / `-Infinity`.
fn sign_of(expr: &str) -> String {
    format!("1 / ({expr})")
}

/// The transcendental and remaining ES2025 `Math` functions: primary results,
/// `ToNumber` of the argument, and the NaN / signed-zero / infinity cases.
#[test]
fn math_transcendentals() {
    for (src, expected) in [
        ("Math.log(Math.E)", "1"),
        ("Math.log(1)", "0"),
        ("Math.log(0)", "-Infinity"),
        ("Math.log(-1)", "NaN"),
        ("Math.log2(8)", "3"),
        ("Math.log10(1000)", "3"),
        ("Math.log1p(0)", "0"),
        ("Math.log1p(-1)", "-Infinity"),
        ("Math.exp(0)", "1"),
        ("Math.exp(-Infinity)", "0"),
        ("Math.expm1(0)", "0"),
        ("Math.expm1(-Infinity)", "-1"),
        ("Math.cbrt(27)", "3"),
        ("Math.cbrt(-8)", "-2"),
        ("Math.sin(0)", "0"),
        ("Math.cos(0)", "1"),
        ("Math.tan(0)", "0"),
        ("Math.asin(1) * 2 === Math.PI", "true"),
        ("Math.acos(1)", "0"),
        ("Math.atan(Infinity) * 2 === Math.PI", "true"),
        ("Math.atan2(1, 1) * 4 === Math.PI", "true"),
        ("Math.atan2(0, -1) === Math.PI", "true"),
        ("Math.sinh(0)", "0"),
        ("Math.cosh(0)", "1"),
        ("Math.tanh(Infinity)", "1"),
        ("Math.asinh(0)", "0"),
        ("Math.acosh(1)", "0"),
        ("Math.atanh(1)", "Infinity"),
        ("Math.sin('0')", "0"),
        ("Math.exp({ valueOf() { return 0 } })", "1"),
        ("Math.cos(NaN)", "NaN"),
        ("Math.sin(Infinity)", "NaN"),
        ("Math.acosh(0.5)", "NaN"),
        ("Math.cosh(-Infinity)", "Infinity"),
    ] {
        assert_eval(src, expected);
    }
    for expr in [
        "Math.sin(-0)",
        "Math.tan(-0)",
        "Math.asin(-0)",
        "Math.atan(-0)",
        "Math.sinh(-0)",
        "Math.tanh(-0)",
        "Math.asinh(-0)",
        "Math.atanh(-0)",
        "Math.cbrt(-0)",
        "Math.expm1(-0)",
        "Math.log1p(-0)",
        "Math.atan2(-0, 0)",
    ] {
        assert_eval(&sign_of(expr), "-Infinity");
    }
}

/// `sign`, `clz32`, `fround`, `imul`, `hypot` and `random`.
#[test]
fn math_integer_and_misc() {
    for (src, expected) in [
        ("Math.sign(-3)", "-1"),
        ("Math.sign(5)", "1"),
        ("Math.sign(NaN)", "NaN"),
        ("Math.sign('-2')", "-1"),
        ("Math.clz32(1)", "31"),
        ("Math.clz32(0)", "32"),
        ("Math.clz32(-1)", "0"),
        ("Math.clz32(2 ** 32)", "32"),
        ("Math.clz32(NaN)", "32"),
        ("Math.clz32(0.5)", "32"),
        ("Math.fround(5.5)", "5.5"),
        ("Math.fround(5.05)", "5.050000190734863"),
        ("Math.fround(2 ** 128)", "Infinity"),
        ("Math.fround(NaN)", "NaN"),
        ("Math.imul(2, 4)", "8"),
        ("Math.imul(-1, 8)", "-8"),
        ("Math.imul(0xffffffff, 5)", "-5"),
        ("Math.imul(0xfffffffe, 5)", "-10"),
        ("Math.imul('3', { valueOf() { return 4 } })", "12"),
        ("Math.hypot()", "0"),
        ("Math.hypot(3, 4)", "5"),
        ("Math.hypot(-3)", "3"),
        ("Math.hypot(NaN, 1)", "NaN"),
        ("Math.hypot(NaN, Infinity)", "Infinity"),
        ("Math.hypot(-Infinity, NaN)", "Infinity"),
        ("Math.hypot(1e200, 1e200) > 1e200", "true"),
        ("Math.hypot(1, 2, 2)", "3"),
        ("Math.hypot(...[1, 2, 2])", "3"),
        ("Math.hypot.length", "2"),
        (
            "Math.max.length + Math.atan2.length + Math.imul.length",
            "6",
        ),
        ("Math.sin.name", "sin"),
        ("typeof Math.random()", "number"),
        (
            "let ok = true; for (let i = 0; i < 100; i++) { const r = Math.random(); if (!(r >= 0 && r < 1)) ok = false } ok",
            "true",
        ),
        ("Math.random() !== Math.random()", "true"),
    ] {
        assert_eval(src, expected);
    }
    for expr in ["Math.sign(-0)", "Math.fround(-0)", "Math.imul(-0, 1) * -1"] {
        assert_eval(&sign_of(expr), "-Infinity");
    }
    assert_eval(&sign_of("Math.hypot(-0)"), "Infinity");
    assert_eval(&sign_of("Math.hypot(-0, -0)"), "Infinity");
}

/// Every argument of a multi-argument `Math` function is converted with
/// `ToNumber` in order, before the `Infinity` / `NaN` rules apply.
#[test]
fn math_converts_every_argument_in_order() {
    let prelude = "let s = ''; const v = (c, n) => ({ valueOf() { s += c; return n } });";
    for src in [
        "Math.hypot(v('a', Infinity), v('b', NaN), v('c', 1)); s",
        "Math.atan2(v('a', 1), v('b', 2)); s",
        "Math.imul(v('a', 1), v('b', 2)); s",
        "Math.hypot(...[v('a', NaN), v('b', 1), v('c', 2)]); s",
    ] {
        let expected = if src.contains("'c'") { "abc" } else { "ab" };
        assert_eval(&format!("{prelude} {src}"), expected);
    }
}

/// `Math.round` rounds half up without the `x + 0.5` error, keeps `-0` for
/// `[-0.5, -0]`, and leaves large integers alone.
#[test]
fn math_round_edges() {
    assert_eval("Math.round(0.49999999999999994)", "0");
    assert_eval("Math.round(4503599627370497)", "4503599627370497");
    assert_eval("Math.round(-4503599627370497)", "-4503599627370497");
    assert_eval(&sign_of("Math.round(-0.4)"), "-Infinity");
    assert_eval(&sign_of("Math.round(-0.5)"), "-Infinity");
    assert_eval(&sign_of("Math.round(-0)"), "-Infinity");
    assert_eval("Math.round(-0.6)", "-1");
    assert_eval("Math.round(Infinity)", "Infinity");
}

/// A sample range: uniform, log-uniform positive, or log-uniform of either sign.
#[derive(Clone, Copy)]
enum Spread {
    Uniform(f64, f64),
    Log(f64, f64),
    SignedLog(f64, f64),
}
use Spread::{Log, SignedLog, Uniform};

/// A deterministic `xorshift64*` over `[0, 1)`.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let mantissa = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 12;
        f64::from_bits((0x3ff_u64 << 52) | mantissa) - 1.0
    }

    fn sample(&mut self, spread: Spread) -> f64 {
        match spread {
            Uniform(lo, hi) => lo + (hi - lo) * self.next(),
            Log(lo, hi) => lo * (hi / lo).powf(self.next()),
            SignedLog(lo, hi) => {
                let magnitude = self.sample(Log(lo, hi));
                if self.next() < 0.5 {
                    -magnitude
                } else {
                    magnitude
                }
            }
        }
    }
}

/// At least 1,000 inputs drawn across `spreads`, plus the domain edges.
fn sample_inputs(seed: u64, spreads: &[Spread]) -> Vec<f64> {
    const PER_FUNCTION: usize = 1_200;
    let mut rng = Rng(seed);
    let mut inputs: Vec<f64> = (0..PER_FUNCTION)
        .map(|i| rng.sample(spreads[i % spreads.len()]))
        .collect();
    let edges = [5e-324, f64::MIN_POSITIVE, 1e-300, 0.5, 1.0, 2.0, 709.5];
    inputs.extend(edges);
    inputs.extend(edges.map(|x| -x));
    inputs
}

/// `x` on a line where adjacent doubles differ by one (`-0` and `+0` coincide).
fn ordinal(x: f64) -> i128 {
    let bits = x.to_bits().cast_signed();
    i128::from(if bits < 0 { i64::MIN - bits } else { bits })
}

/// Run one guest expression per input and return its results, in order
/// (`-0` is printed with its sign so results round-trip bit-exactly).
/// `call` renders the JS expression for input index expression `i`.
fn guest_results(literals: &[Vec<f64>], call: &str) -> Vec<f64> {
    let decls: Vec<String> = ["xs", "ys"]
        .iter()
        .zip(literals)
        .map(|(name, values)| {
            let list: Vec<String> = values.iter().map(|x| format!("{x:e}")).collect();
            format!("const {name} = [{}];\n", list.join(","))
        })
        .collect();
    let src = format!(
        "{}let out = \"\"; for (let i = 0; i < xs.length; i++) {{ const v = {call}; out += (Object.is(v, -0) ? \"-0\" : v) + \"\\n\"; }}\n\
         console.log(out);\n",
        decls.concat()
    );
    let done = common::exec(&src, false).expect("sample program runs");
    assert_eq!(done.code, 0, "{call}: {}", done.stderr);
    done.stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.parse().expect("numeric output line"))
        .collect()
}

/// Assert one guest result has the same bits as the `libm` crate's result
/// (any two NaNs are equal).
fn assert_bit_exact(label: &str, input: (f64, f64), guest: f64, reference: f64) {
    let same = if reference.is_nan() {
        guest.is_nan()
    } else {
        guest.to_bits() == reference.to_bits()
    };
    assert!(
        same,
        "{label}{input:?}: guest {guest:e} ({:#018x}), libm {reference:e} ({:#018x})",
        guest.to_bits(),
        reference.to_bits()
    );
}

/// The Rust `std` sanity bound in ulps. `std` is the platform libm, so it may
/// differ from msun by a few ulps; 4096 ulps (about 12 bits of the mantissa)
/// cannot be reached by that difference and only catches gross errors.
const STD_SANITY_ULPS: i128 = 4096;

/// Assert one guest result is roughly the `std` result.
fn assert_near_std(label: &str, input: (f64, f64), guest: f64, native: f64) {
    let near = if native.is_nan() {
        guest.is_nan()
    } else {
        (ordinal(guest) - ordinal(native)).abs() <= STD_SANITY_ULPS
    };
    assert!(near, "{label}{input:?}: guest {guest:e}, std {native:e}");
}

/// One sampled function: its `Math` name, the `libm` reference, the std
/// sanity reference and its inputs.
type Sampled<'a> = (&'a str, fn(f64) -> f64, fn(f64) -> f64, &'a [Spread]);

/// The guest libm is bit-exact with the `libm` crate it was ported from on a
/// deterministic sample of every transcendental, coarsely agrees with Rust
/// `std`, and is exact on the special values.
#[test]
fn libm_matches_std_sample() {
    let trig: &[Spread] = &[
        Uniform(-7.0, 7.0),
        SignedLog(1e-12, 1e6),
        SignedLog(1e7, 1e300),
    ];
    let log: &[Spread] = &[Log(1e-300, 1e300), Uniform(0.5, 2.0)];
    let wide: &[Spread] = &[SignedLog(1e-300, 1e300), Uniform(-30.0, 30.0)];
    let unit: &[Spread] = &[Uniform(-1.0, 1.0), SignedLog(1e-300, 1.0)];
    let exp: &[Spread] = &[Uniform(-700.0, 700.0), SignedLog(1e-300, 1.0)];
    let hyper: &[Spread] = &[Uniform(-30.0, 30.0), Uniform(-700.0, 700.0)];
    let functions: [Sampled<'_>; 19] = [
        ("cbrt", libm::cbrt, f64::cbrt, wide),
        ("log", libm::log, f64::ln, log),
        ("log2", libm::log2, f64::log2, log),
        ("log10", libm::log10, f64::log10, log),
        (
            "log1p",
            libm::log1p,
            f64::ln_1p,
            &[Uniform(-0.99, 10.0), Log(1e-300, 1e300)],
        ),
        ("exp", libm::exp, f64::exp, exp),
        ("expm1", libm::expm1, f64::exp_m1, exp),
        ("sin", libm::sin, f64::sin, trig),
        ("cos", libm::cos, f64::cos, trig),
        ("tan", libm::tan, f64::tan, trig),
        ("asin", libm::asin, f64::asin, unit),
        ("acos", libm::acos, f64::acos, unit),
        ("atan", libm::atan, f64::atan, wide),
        ("sinh", libm::sinh, f64::sinh, hyper),
        ("cosh", libm::cosh, f64::cosh, hyper),
        (
            "tanh",
            libm::tanh,
            f64::tanh,
            &[Uniform(-20.0, 20.0), SignedLog(1e-300, 1.0)],
        ),
        ("asinh", libm::asinh, f64::asinh, wide),
        (
            "acosh",
            libm::acosh,
            f64::acosh,
            &[Log(1.0, 1e300), Uniform(1.0, 10.0)],
        ),
        ("atanh", libm::atanh, f64::atanh, unit),
    ];
    for (seed, (name, reference, native, spreads)) in (1_u64..).zip(functions) {
        let xs = sample_inputs(seed * 0x9E37_79B9, spreads);
        let results = guest_results(std::slice::from_ref(&xs), &format!("Math.{name}(xs[i])"));
        assert_eq!(results.len(), xs.len(), "{name}: result count");
        for (&x, guest) in xs.iter().zip(results) {
            assert_bit_exact(name, (x, f64::NAN), guest, reference(x));
            assert_near_std(name, (x, f64::NAN), guest, native(x));
        }
    }
    let ys = sample_inputs(0xA5A5_0001, wide);
    let xs = sample_inputs(0xA5A5_0002, wide);
    let results = guest_results(&[xs.clone(), ys.clone()], "Math.atan2(ys[i], xs[i])");
    assert_eq!(results.len(), xs.len(), "atan2: result count");
    for ((&x, &y), guest) in xs.iter().zip(&ys).zip(results) {
        assert_bit_exact("atan2", (y, x), guest, libm::atan2(y, x));
        assert_near_std("atan2", (y, x), guest, y.atan2(x));
    }
    assert_libm_special_values();
}

/// The `Math.*` calls with an exact expected result: NaN, signed zeros,
/// infinities and domain edges.
const LIBM_SPECIAL_CASES: &[(&str, &str)] = &[
    ("cbrt(-0)", "-0"),
    ("cbrt(Infinity)", "Infinity"),
    ("cbrt(-Infinity)", "-Infinity"),
    ("cbrt(27)", "3"),
    ("cbrt(-8)", "-2"),
    ("cbrt(NaN)", "NaN"),
    ("log(1)", "0"),
    ("log(0)", "-Infinity"),
    ("log(-0)", "-Infinity"),
    ("log(-1)", "NaN"),
    ("log(Infinity)", "Infinity"),
    ("log(-Infinity)", "NaN"),
    ("log(NaN)", "NaN"),
    ("log2(1)", "0"),
    ("log2(8)", "3"),
    ("log2(0)", "-Infinity"),
    ("log2(-1)", "NaN"),
    ("log10(1)", "0"),
    ("log10(0)", "-Infinity"),
    ("log10(-1)", "NaN"),
    ("log1p(-0)", "-0"),
    ("log1p(0)", "0"),
    ("log1p(-1)", "-Infinity"),
    ("log1p(-2)", "NaN"),
    ("log1p(Infinity)", "Infinity"),
    ("exp(0)", "1"),
    ("exp(-0)", "1"),
    ("exp(-Infinity)", "0"),
    ("exp(Infinity)", "Infinity"),
    ("exp(710)", "Infinity"),
    ("exp(-746)", "0"),
    ("exp(NaN)", "NaN"),
    ("expm1(-0)", "-0"),
    ("expm1(0)", "0"),
    ("expm1(-Infinity)", "-1"),
    ("expm1(Infinity)", "Infinity"),
    ("sin(-0)", "-0"),
    ("sin(Infinity)", "NaN"),
    ("sin(-Infinity)", "NaN"),
    ("sin(NaN)", "NaN"),
    ("cos(-0)", "1"),
    ("cos(Infinity)", "NaN"),
    ("tan(-0)", "-0"),
    ("tan(Infinity)", "NaN"),
    ("asin(-0)", "-0"),
    ("asin(1)", "Math.PI / 2"),
    ("asin(-1)", "-Math.PI / 2"),
    ("asin(2)", "NaN"),
    ("acos(1)", "0"),
    ("acos(-1)", "Math.PI"),
    ("acos(-2)", "NaN"),
    ("atan(-0)", "-0"),
    ("atan(Infinity)", "Math.PI / 2"),
    ("atan(-Infinity)", "-Math.PI / 2"),
    ("atan2(0, 0)", "0"),
    ("atan2(-0, 0)", "-0"),
    ("atan2(0, -0)", "Math.PI"),
    ("atan2(-0, -0)", "-Math.PI"),
    ("atan2(1, 0)", "Math.PI / 2"),
    ("atan2(-1, 0)", "-Math.PI / 2"),
    ("atan2(Infinity, Infinity)", "Math.PI / 4"),
    ("atan2(Infinity, -Infinity)", "3 * Math.PI / 4"),
    ("atan2(-Infinity, Infinity)", "-Math.PI / 4"),
    ("atan2(1, Infinity)", "0"),
    ("atan2(-1, Infinity)", "-0"),
    ("atan2(1, -Infinity)", "Math.PI"),
    ("atan2(NaN, 1)", "NaN"),
    ("sinh(-0)", "-0"),
    ("sinh(Infinity)", "Infinity"),
    ("sinh(-Infinity)", "-Infinity"),
    ("cosh(-0)", "1"),
    ("cosh(-Infinity)", "Infinity"),
    ("tanh(-0)", "-0"),
    ("tanh(Infinity)", "1"),
    ("tanh(-Infinity)", "-1"),
    ("tanh(NaN)", "NaN"),
    ("asinh(-0)", "-0"),
    ("asinh(-Infinity)", "-Infinity"),
    ("acosh(1)", "0"),
    ("acosh(0.5)", "NaN"),
    ("acosh(Infinity)", "Infinity"),
    ("acosh(-Infinity)", "NaN"),
    ("atanh(-0)", "-0"),
    ("atanh(1)", "Infinity"),
    ("atanh(-1)", "-Infinity"),
    ("atanh(2)", "NaN"),
];

/// Exact results at NaN, signed zeros, infinities and domain edges.
fn assert_libm_special_values() {
    let cases: Vec<(String, &str)> = LIBM_SPECIAL_CASES
        .iter()
        .map(|(call, want)| (format!("Math.{call}"), *want))
        .collect();
    let checks: Vec<String> = cases
        .iter()
        .map(|(call, want)| format!("Object.is({call}, {want})"))
        .collect();
    let output = eval_text(&format!("[{}].join()", checks.join(",")));
    let verdicts: Vec<&str> = output.split(',').collect();
    assert_eq!(verdicts.len(), cases.len(), "special-value result count");
    for ((call, want), verdict) in cases.iter().zip(verdicts) {
        assert_eq!(verdict, "true", "{call} should be exactly {want}");
    }
}
