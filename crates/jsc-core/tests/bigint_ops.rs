//! Integration tests: the `BigInt` operators (DESIGN.md §6.10.4–6.10.6, stage
//! B2b) — arithmetic, bitwise and shift kernels on multi-limb values, the
//! mixed-type `TypeError`s and the `RangeError`s, `++` / `--` and the compound
//! assignments on every reference kind, and the exact relational comparisons
//! against numbers and strings. A `gc_stress` module repeats the multi-limb
//! loops with a collection at every allocation.

mod common;
use common::{
    assert_bool, assert_eval, assert_number, assert_output, assert_string, assert_uncaught,
};

/// Assert each `(source, expected rendering)` pair.
fn table(cases: &[(&str, &str)]) {
    for (src, expected) in cases {
        assert_eval(src, expected);
    }
}

/// Assert `src` throws an `error` (constructor name) with `message`.
fn throws(src: &str, error: &str, message: &str) {
    assert_string(
        &format!(
            "try {{ {src}; 'none' }} catch (e) {{ e instanceof {error} ? e.message : 'other' }}"
        ),
        message,
    );
}

/// The `TypeError` of an operator mixing a `BigInt` and a number.
const MIX: &str = "Cannot mix BigInt and other types, use explicit conversions";
#[test]
fn addition_and_subtraction_carry_and_borrow() {
    table(&[
        ("(2n ** 64n - 1n) + 1n", "18446744073709551616n"),
        ("2n ** 64n - 1n", "18446744073709551615n"),
        ("0n - 2n ** 64n", "-18446744073709551616n"),
        (
            "2n ** 128n - 1n",
            "340282366920938463463374607431768211455n",
        ),
        ("-(2n ** 128n) + 2n ** 128n", "0n"),
        ("-5n + 3n", "-2n"),
        ("5n + -8n", "-3n"),
        ("-(2n ** 96n) - 1n", "-79228162514264337593543950337n"),
        ("(2n ** 96n) - (2n ** 96n - 1n)", "1n"),
        (
            "0xffffffffffffffffffffffffn + 1n",
            "79228162514264337593543950336n",
        ),
    ]);
}

#[test]
fn multiplication_across_limbs() {
    table(&[
        (
            "(2n ** 64n + 1n) * (2n ** 64n - 1n)",
            "340282366920938463463374607431768211455n",
        ),
        (
            "-(2n ** 100n) * 3n ** 50n",
            "-910043815000214977332758527534256632492715260325658624n",
        ),
        ("0n * -(2n ** 70n)", "0n"),
        ("-7n * -6n", "42n"),
        ("0xffffffffn * 0xffffffffn", "18446744065119617025n"),
        (
            "(2n ** 200n + 12345n) * (2n ** 64n - 1n)",
            "29642774844752946026827234117965114134895153982062821791806663567832912767209415n",
        ),
    ]);
}

#[test]
fn division_and_remainder() {
    table(&[
        ("7n / 2n", "3n"),
        ("-7n / 2n", "-3n"),
        ("7n / -2n", "-3n"),
        ("-7n / -2n", "3n"),
        ("7n % 2n", "1n"),
        ("-7n % 2n", "-1n"),
        ("7n % -2n", "1n"),
        ("-7n % -2n", "-1n"),
        ("3n / 5n", "0n"),
        ("-3n % 5n", "-3n"),
        ("(2n ** 70n + 6n) / 7n", "168655945816773043347n"),
        ("-(2n ** 70n + 6n) % 7n", "-1n"),
        ("(2n ** 70n + 6n) / -7n", "-168655945816773043347n"),
        (
            "3n ** 300n / ((2n ** 32n - 1n) * 2n ** 64n + 7n)",
            "1727813377797507216693850974871531819146151421168848954728043851441854592479553340836385588545035350949528362498881n",
        ),
        (
            "3n ** 300n % ((2n ** 32n - 1n) * 2n ** 64n + 7n)",
            "37754857365275493995929241514n",
        ),
        (
            "-(3n ** 300n) / ((2n ** 32n - 1n) * 2n ** 64n + 7n)",
            "-1727813377797507216693850974871531819146151421168848954728043851441854592479553340836385588545035350949528362498881n",
        ),
        (
            "-(3n ** 300n) % ((2n ** 32n - 1n) * 2n ** 64n + 7n)",
            "-37754857365275493995929241514n",
        ),
        (
            "0x7fffffff800000010000000000000000n / 0x800000000000000000000003n",
            "4294967295n",
        ),
        (
            "0x7fffffff800000010000000000000000n % 0x800000000000000000000003n",
            "18446744060824649731n",
        ),
        (
            "0x800000008000000000000001n / 0x10000000100000001n",
            "2147483647n",
        ),
        (
            "0x800000008000000000000001n % 0x10000000100000001n",
            "18446744075857035266n",
        ),
        ("(2n ** 64n) / (2n ** 64n)", "1n"),
        ("(2n ** 64n) % (2n ** 64n)", "0n"),
        ("(2n ** 128n) / (2n ** 64n - 1n)", "18446744073709551617n"),
        ("(2n ** 128n) % (2n ** 64n - 1n)", "1n"),
    ]);
}

#[test]
fn exponentiation() {
    table(&[
        ("2n ** 64n", "18446744073709551616n"),
        ("(-2n) ** 3n", "-8n"),
        ("0n ** 0n", "1n"),
        ("5n ** 0n", "1n"),
        ("0n ** 5n", "0n"),
        ("(-1n) ** 1000001n", "-1n"),
        ("(-1n) ** 1000000n", "1n"),
        ("1n ** (2n ** 100n)", "1n"),
        (
            "3n ** 100n",
            "515377520732011331036461129765621272702107522001n",
        ),
        ("(-3n) ** 41n", "-36472996377170786403n"),
    ]);
}

#[test]
fn bitwise_two_complement() {
    table(&[
        ("5n & 3n", "1n"),
        ("-5n & 3n", "3n"),
        ("-5n | 3n", "-5n"),
        ("-5n ^ 3n", "-8n"),
        ("-(2n ** 64n) & (2n ** 64n - 1n)", "0n"),
        ("-1n & 2n ** 70n", "1180591620717411303424n"),
        ("-(2n ** 32n) | 1n", "-4294967295n"),
        ("(-(2n ** 64n) - 1n) ^ 2n ** 64n", "-1n"),
        ("-(2n ** 40n) & -(2n ** 35n)", "-1099511627776n"),
        ("-(2n ** 40n) | -(2n ** 35n)", "-34359738368n"),
        (
            "(2n ** 64n + 5n) ^ -(2n ** 96n)",
            "-79228162495817593519834398715n",
        ),
        ("0n | -7n", "-7n"),
        ("-7n & 0n", "0n"),
        ("~5n", "-6n"),
        ("~-1n", "0n"),
        ("~0n", "-1n"),
        ("~(2n ** 64n)", "-18446744073709551617n"),
        ("~-(2n ** 64n)", "18446744073709551615n"),
    ]);
}

#[test]
fn shifts() {
    table(&[
        ("1n << 64n", "18446744073709551616n"),
        ("(2n ** 64n + 1n) >> 1n", "9223372036854775808n"),
        ("-1n >> 100n", "-1n"),
        ("-5n >> 1n", "-3n"),
        ("-(2n ** 64n) >> 64n", "-1n"),
        ("(-(2n ** 64n) - 1n) >> 64n", "-2n"),
        ("0xffffffffn << 31n", "9223372034707292160n"),
        ("(-(2n ** 95n) - 1n) >> 33n", "-4611686018427387905n"),
        ("-(2n ** 95n) >> 33n", "-4611686018427387904n"),
        ("5n << -1n", "2n"),
        ("-5n << -1n", "-3n"),
        ("1n >> -3n", "8n"),
        ("1n >> (2n ** 100n)", "0n"),
        ("-3n >> (2n ** 100n)", "-1n"),
        ("0n << (2n ** 40n)", "0n"),
        ("7n << 0n", "7n"),
        ("(2n ** 64n - 1n) << 32n", "79228162514264337589248983040n"),
        ("(2n ** 96n + 2n ** 40n) >> 40n", "72057594037927937n"),
    ]);
}

#[test]
fn division_and_exponent_range_errors() {
    throws("1n / 0n", "RangeError", "Division by zero");
    throws("(2n ** 64n) % 0n", "RangeError", "Division by zero");
    throws("let z = 0n; 5n % z", "RangeError", "Division by zero");
    throws("2n ** -1n", "RangeError", "Exponent must be non-negative");
    throws(
        "0n ** -(2n ** 64n)",
        "RangeError",
        "Exponent must be non-negative",
    );
    throws(
        "2n ** 10000000n",
        "RangeError",
        "Maximum BigInt size exceeded",
    );
    throws(
        "2n ** (2n ** 32n)",
        "RangeError",
        "Maximum BigInt size exceeded",
    );
    throws(
        "1n << (2n ** 32n)",
        "RangeError",
        "Maximum BigInt size exceeded",
    );
    throws(
        "1n >> -(2n ** 32n)",
        "RangeError",
        "Maximum BigInt size exceeded",
    );
    assert_uncaught("1n / 0n", "RangeError: Division by zero");
}

#[test]
fn mixing_with_numbers_throws() {
    for src in [
        "1n + 1",
        "1 + 1n",
        "1n - 1",
        "1 * 1n",
        "1n / 1",
        "1n % 1",
        "1n ** 1",
        "2 ** 1n",
        "1n & 1",
        "1 | 1n",
        "1n ^ 1",
        "1n << 1",
        "1 >> 1n",
        "1n >>> 1",
        "1n + undefined",
        "null - 1n",
        "true * 1n",
        "let x = 1n; x += 1",
        "let o = { v: 1n }; o.v *= 2",
        "let a = [1]; a[0] -= 1n",
    ] {
        throws(src, "TypeError", MIX);
    }
    assert_uncaught("1n + 1", &format!("TypeError: {MIX}"));
}

#[test]
fn unsigned_shift_and_unary_plus_throw() {
    throws(
        "1n >>> 1n",
        "TypeError",
        "BigInts have no unsigned right shift, use >> instead",
    );
    throws(
        "let x = 4n; x >>>= 1n",
        "TypeError",
        "BigInts have no unsigned right shift, use >> instead",
    );
    throws(
        "+1n",
        "TypeError",
        "Cannot convert a BigInt value to a number",
    );
    throws(
        "let x = 1n; +x",
        "TypeError",
        "Cannot convert a BigInt value to a number",
    );
    throws(
        "Math.floor(3n - 1n)",
        "TypeError",
        "Cannot convert a BigInt value to a number",
    );
}

#[test]
fn string_concatenation_and_objects() {
    assert_string("'1' + 1n", "11");
    assert_string("2n + '3'", "23");
    assert_eval("({ valueOf() { return 3n } }) * 2n", "6n");
    assert_eval("-({ valueOf() { return 3n } })", "-3n");
    assert_eval("~({ valueOf() { return 3n } })", "-4n");
    // ToNumeric runs left then right before the mixing check.
    assert_string(
        "let log = ''; \
         try { ({ valueOf() { log += 'a'; return 1n } }) - ({ valueOf() { log += 'b'; return 1 } }) } \
         catch (e) { log += e instanceof TypeError ? '!' : '?' } log",
        "ab!",
    );
}

#[test]
fn unary_minus_and_not_on_values() {
    table(&[
        ("let x = 5n; -x", "-5n"),
        ("let x = 5n; -(x - x)", "0n"),
        ("let x = -(2n ** 64n); -x", "18446744073709551616n"),
        ("let x = 5n; ~x", "-6n"),
        ("let x = -1n; ~x", "0n"),
        ("let x = 2n ** 64n; ~~x === x", "true"),
    ]);
    // Number paths are unchanged.
    assert_number("let x = 5; -x", -5.0);
    assert_eval("let x = 0; Object.is(-x, -0)", "true");
    assert_number("let x = 5; ~x", -6.0);
    assert_number("let x = '3'; -x", -3.0);
    assert_number("7 % 3", 1.0);
    assert_number("-7 % 3", -1.0);
    assert_number("2 ** 10", 1024.0);
    assert_number("let b = 2; b ** -1", 0.5);
    assert_number("let a = 6; a / 4", 1.5);
    assert_number("let a = -1; a >>> 28", 15.0);
    assert_number("let a = '5'; a << 2", 20.0);
}

#[test]
fn increment_and_decrement_on_every_reference_kind() {
    table(&[
        // Plain locals: prefix yields the new value, postfix the old one.
        ("let x = 1n; ++x", "2n"),
        ("let x = 1n; x++", "1n"),
        ("let x = 1n; x++; x", "2n"),
        ("let x = 0n; --x", "-1n"),
        ("let x = -1n; ++x", "0n"),
        ("let x = 2n ** 64n - 1n; ++x", "18446744073709551616n"),
        ("let x = 2n ** 64n; --x", "18446744073709551615n"),
        ("let x = -(2n ** 64n); x--; x", "-18446744073709551617n"),
        ("let x = -(2n ** 64n) - 1n; ++x", "-18446744073709551616n"),
        ("let x = 1n; for (let i = 0; i < 10; i++) x++; x", "11n"),
        // Captured cells.
        ("let x = 1n; const f = () => ++x; f(); f()", "3n"),
        (
            "let x = 1n; const f = () => x--; f(); [f(), x]",
            "[ 0n, -1n ]",
        ),
        // Globals.
        ("globalThis.g = 2n ** 64n; g++; g", "18446744073709551617n"),
        ("globalThis.g = 5n; [g--, g]", "[ 5n, 4n ]"),
        // Members and elements.
        ("const o = { v: 1n }; [o.v++, o.v]", "[ 1n, 2n ]"),
        ("const o = { v: 1n }; [--o.v, o.v]", "[ 0n, 0n ]"),
        ("const a = [2n ** 32n - 1n]; ++a[0]", "4294967296n"),
        ("const a = [0n]; [a[0]--, a[0]]", "[ 0n, -1n ]"),
        (
            "const o = { v: { valueOf() { return 9n } } }; [o.v++, o.v]",
            "[ 9n, 10n ]",
        ),
        // Private fields and super properties.
        (
            "class C { #v = 1n; f() { this.#v++; return ++this.#v } } new C().f()",
            "3n",
        ),
        (
            "class A { } A.prototype.v = 7n; \
             class B extends A { f() { return [super.v++, this.v, --super.v] } } new B().f()",
            "[ 7n, 8n, 6n ]",
        ),
        // Generator slots.
        (
            "function* g() { let x = 1n; yield x++; yield ++x; yield x-- } [...g()]",
            "[ 1n, 3n, 3n ]",
        ),
    ]);
    // Number updates keep their results.
    assert_number("let x = '4'; x++; x", 5.0);
    assert_number("let x = '4'; x++", 4.0);
    assert_number("const o = { v: '4' }; o.v++", 4.0);
    assert_number("let x = 1; const f = () => x++; f(); x", 2.0);
}

#[test]
fn compound_assignments() {
    table(&[
        ("let x = 2n ** 64n; x += 1n; x", "18446744073709551617n"),
        ("let x = 10n; x -= 11n; x", "-1n"),
        ("let x = 3n; x *= -(2n ** 40n); x", "-3298534883328n"),
        ("let x = -9n; x /= 2n; x", "-4n"),
        ("let x = -9n; x %= 2n; x", "-1n"),
        ("let x = 3n; x **= 4n; x", "81n"),
        ("let x = 12n; x &= 10n; x", "8n"),
        ("let x = 12n; x |= 3n; x", "15n"),
        ("let x = 12n; x ^= -1n; x", "-13n"),
        ("let x = 1n; x <<= 70n; x", "1180591620717411303424n"),
        ("let x = -(2n ** 70n); x >>= 69n; x", "-2n"),
        (
            "const o = { v: 2n }; o.v **= 65n; o.v",
            "36893488147419103232n",
        ),
        ("const a = [7n]; a[0] %= 4n; a[0]", "3n"),
        ("let x = 3n; const f = () => (x **= 2n); f(); f()", "81n"),
        ("globalThis.h = 6n; h <<= 2n; h", "24n"),
        (
            "class C { #v = 2n; f() { this.#v **= 3n; return this.#v } } new C().f()",
            "8n",
        ),
        ("let x = 0n; x ||= 5n; x", "5n"),
        ("let x = 2n; x &&= x * 3n; x", "6n"),
        ("let x = null; x ??= 4n; x", "4n"),
        (
            "const o = { v: 0n }; o.v ||= 2n ** 64n; o.v",
            "18446744073709551616n",
        ),
    ]);
}

#[test]
fn relational_comparisons_are_exact() {
    for (src, expected) in [
        ("1n < 2n", true),
        ("-(2n ** 64n) < -(2n ** 63n)", true),
        ("2n ** 64n > 2n ** 64n - 1n", true),
        ("2n ** 64n < 2 ** 64", false),
        ("2n ** 64n <= 2 ** 64", true),
        ("2n ** 64n >= 2 ** 64", true),
        ("2n ** 64n + 1n > 2 ** 64", true),
        ("1n < 1.5", true),
        ("2n > 1.5", true),
        ("-1n < -0.5", true),
        ("-1n > -1.5", true),
        ("9007199254740993n > 9007199254740992", true),
        ("9007199254740992 < 9007199254740993n", true),
        ("9007199254740992n >= 9007199254740992", true),
        ("1n < Infinity", true),
        ("2n ** 2000n < Infinity", true),
        ("-(2n ** 2000n) > -Infinity", true),
        ("Infinity > 2n ** 2000n", true),
        ("1n < NaN", false),
        ("1n > NaN", false),
        ("1n <= NaN", false),
        ("NaN >= 1n", false),
        ("1n < '2'", true),
        ("'10' > 9n", true),
        ("'0x10' >= 16n", true),
        ("' 12 ' <= 12n", true),
        ("1n < 'x'", false),
        ("1n >= 'x'", false),
        ("'1.5' >= 1n", false),
        ("'1.5' <= 1n", false),
        ("1n < true", false),
        ("1n <= true", true),
        ("0n > null", false),
        ("0n >= null", true),
        ("1n < undefined", false),
        ("({ valueOf() { return 3n } }) > 2n", true),
        ("[5n] > 4n", true),
    ] {
        assert_bool(src, expected);
    }
    throws(
        "1n < Symbol()",
        "TypeError",
        "Cannot convert a Symbol value to a number",
    );
}

#[test]
fn equality_of_computed_values() {
    assert_bool("(2n ** 64n) / 2n === 2n ** 63n", true);
    assert_bool("-(1n - 1n) === 0n", true);
    assert_bool("Object.is((-5n) % 5n, 0n)", true);
    assert_bool("new Map([[2n ** 70n, 1]]).has(2n ** 69n * 2n)", true);
    assert_string("typeof (1n - 1n)", "bigint");
}

/// The multi-limb loops of this file, compiled with a collection at every
/// allocation, so each kernel runs while its operands and partial results
/// are the only references.
mod gc_stress {
    use super::common::assert_same_under_gc_stress;

    #[test]
    fn factorial_of_200() {
        assert_same_under_gc_stress(
            "let f = 1n; for (let i = 1n; i <= 200n; i++) f *= i; \
             console.log(String(f).length, f % 1000000007n)",
            "375 722479105n\n",
        );
    }

    #[test]
    fn fibonacci_accumulator() {
        assert_same_under_gc_stress(
            "let a = 0n, b = 1n; for (let i = 0; i < 300; i++) { const t = a + b; a = b; b = t } \
             console.log(a)",
            "222232244629420445529739893461909967206666939096499764990979600n\n",
        );
    }

    #[test]
    fn bitwise_and_steps_across_limbs() {
        assert_same_under_gc_stress(
            "let x = -(2n ** 100n) + 12345n, y = 2n ** 96n - 1n, acc = 0n; \
             for (let i = 0; i < 40; i++) { acc ^= (x | y) & (x ^ (y << 3n)); x--; y++; acc += 1n; } \
             let z = 2n ** 64n; z--; let w = -(2n ** 64n); w--; \
             console.log(acc, z, w, x < y, -x > y * 2n)",
            "79228162514264337593543950384n 18446744073709551615n -18446744073709551617n true true\n",
        );
    }

    #[test]
    fn divmod_round_trips() {
        assert_same_under_gc_stress(
            "let x = 3n ** 200n, ok = true; \
             for (let d = 2n ** 31n + 11n; d < 2n ** 200n; d = d * d + 1n) { \
               const q = x / d, r = x % d; \
               ok = ok && q * d + r === x && r < d && ((-x) / d === -q) && ((-x) % d === -r); \
             } \
             console.log(ok, (x >> 100n) << 100n === x - (x & ((1n << 100n) - 1n)), ~(-x) === x - 1n)",
            "true true true\n",
        );
    }

    /// The `BigInt` gate opened by a `globalThis` read (DESIGN.md §6.10.9).
    #[test]
    fn gate_opened_by_a_global_object_alias() {
        assert_same_under_gc_stress(
            "const g = globalThis; const B = g['Big' + 'Int']; const y = B(7); \
             console.log(y * y, -y, y ** 2n, y > 6, y + 1n)",
            "49n -7n 49n true 8n\n",
        );
    }
}

/// The `BigInt` gate opens on any `globalThis` read (DESIGN.md §6.10.9): an
/// escaping `globalThis` reads the global object as a global lookup, not
/// the sealed intrinsic, and a `BigInt` found through it must not take the
/// Number-only operator code.
#[test]
fn gate_opened_by_a_global_object_lookup() {
    assert_output(
        "let k = \"Big\" + \"Int\"; let x = globalThis[k](5); console.log(x * x, x > 2)",
        "25n true\n",
    );
    assert_output(
        "const g = globalThis; const B = g[\"Big\" + \"Int\"]; const y = B(7); \
         console.log(y * y, -y, y ** 2n, y > 6, y + 1n)",
        "49n -7n 49n true 8n\n",
    );
}

/// The compile-time gate (DESIGN.md §6.10.9): a `BigInt` reached only through
/// the global object (no literal, no `BigInt` name) still goes through every
/// `BigInt` operator path, and a Number-only program keeps its results.
#[test]
fn gate_opened_by_the_global_object() {
    let big = "const k = 'Big' + 'Int'; const B = globalThis[k]; let x = B(5);";
    table(&[
        (&format!("{big} x++; x"), "6n"),
        (
            &format!("{big} [-x, ~x, x * x, x ** 2n, x - 1n]"),
            "[ -5n, -6n, 25n, 25n, 4n ]",
        ),
        (&format!("{big} const o = {{ v: x }}; o.v--; o.v"), "4n"),
        (
            &format!("{big} [x < 6, x > '4', typeof x, !!x, x === B(5)]"),
            "[ true, true, bigint, true, true ]",
        ),
        (&format!("{big} Number(x) + 1"), "6"),
    ]);
    throws(&format!("{big} x + 1"), "TypeError", MIX);
    table(&[
        (
            "let a = 1; a++; const o = { n: 1 }; o.n--; [-a, ~a, a ** 3, a % 3, o.n, a > '1']",
            "[ -2, -3, 8, 2, 0, true ]",
        ),
        (
            "let s = '4'; s++; let t = '4'; t--; [s, t, -'3', ~'1', '2' ** 3, Number('7')]",
            "[ 5, 3, -3, -2, 8, 7 ]",
        ),
    ]);
}
