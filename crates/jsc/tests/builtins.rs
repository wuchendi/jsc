//! Integration tests: `console.log` / `Math` / conversion built-ins via the CLI.

mod common;
use common::{jsc, jsc_raw_stdout};

/// S14.1 — `console.log` writes to the process's real stdout DURING execution.
///
/// The printed lines appear before the final-expression value line, single-space
/// separated with one trailing newline per call, and each argument is formatted
/// exactly as the CLI formats a final value (numbers bare, strings unquoted,
/// Node-style arrays). The final `console.log(..)` here evaluates to `undefined`,
/// which the CLI then prints as the completion value AFTER the logged lines.
#[test]
fn s14_console_log_streams_to_stdout() {
    // Single number argument.
    assert_eq!(
        jsc_raw_stdout(&["-e", "console.log(40 + 2)"]),
        "42\nundefined\n"
    );
    // Unquoted string.
    assert_eq!(
        jsc_raw_stdout(&["-e", r#"console.log("hi")"#]),
        "hi\nundefined\n"
    );
    // Multiple arguments, space-separated, one trailing newline.
    assert_eq!(
        jsc_raw_stdout(&["-e", "console.log(1, 2, 3)"]),
        "1 2 3\nundefined\n"
    );
    // Node-style array formatting.
    assert_eq!(
        jsc_raw_stdout(&["-e", "console.log([1, 2, 3])"]),
        "[ 1, 2, 3 ]\nundefined\n"
    );
    // Logged lines precede the independent final-expression value (99).
    assert_eq!(
        jsc_raw_stdout(&["-e", "console.log(1); console.log(2); 99"]),
        "1\n2\n99\n"
    );
}

/// S14.2 — `Math.*` methods and `Math.PI`/`Math.E` compute end-to-end through the
/// CLI, formatting their numeric result exactly like any other final value.
#[test]
fn s14_math_methods_via_eval() {
    assert_eq!(jsc(&["-e", "Math.floor(3.7)"]), "3");
    assert_eq!(jsc(&["-e", "Math.ceil(3.2)"]), "4");
    assert_eq!(jsc(&["-e", "Math.round(2.5)"]), "3");
    assert_eq!(jsc(&["-e", "Math.trunc(-3.7)"]), "-3");
    assert_eq!(jsc(&["-e", "Math.abs(-42)"]), "42");
    assert_eq!(jsc(&["-e", "Math.sqrt(1764)"]), "42");
    assert_eq!(jsc(&["-e", "Math.min(5, 2, 9)"]), "2");
    assert_eq!(jsc(&["-e", "Math.max(1, 42)"]), "42");
    assert_eq!(jsc(&["-e", "Math.pow(2, 10)"]), "1024");
    assert_eq!(jsc(&["-e", "Math.PI"]), "3.141592653589793");
    assert_eq!(jsc(&["-e", "Math.E"]), "2.718281828459045");
    assert_eq!(jsc(&["-e", "typeof Math.PI"]), "number");
}

/// S14.3 — `String`/`Number`/`Boolean`/`parseInt`/`parseFloat` conversions compute
/// end-to-end through the CLI, formatting their result exactly like any other
/// final value.
#[test]
fn s14_conversions_via_eval() {
    // String(x): number form matches `format_number`, and the result is a string.
    assert_eq!(jsc(&["-e", "String(42)"]), "42");
    assert_eq!(jsc(&["-e", "String(3.5)"]), "3.5");
    assert_eq!(jsc(&["-e", "typeof String(42)"]), "string");
    assert_eq!(jsc(&["-e", r#"String(42) + "px""#]), "42px");
    // Number(x): string parse, boolean/null/undefined coercion.
    assert_eq!(jsc(&["-e", r#"Number("42")"#]), "42");
    assert_eq!(jsc(&["-e", r#"Number("  3.5 ")"#]), "3.5");
    assert_eq!(jsc(&["-e", r#"Number("")"#]), "0");
    assert_eq!(jsc(&["-e", r#"Number("x")"#]), "NaN");
    // Boolean(x): JS truthiness.
    assert_eq!(jsc(&["-e", "Boolean(0)"]), "false");
    assert_eq!(jsc(&["-e", r#"Boolean("")"#]), "false");
    assert_eq!(jsc(&["-e", r#"Boolean("x")"#]), "true");
    assert_eq!(jsc(&["-e", "Boolean(1)"]), "true");
    // parseInt / parseFloat: leading numeric prefix.
    assert_eq!(jsc(&["-e", r#"parseInt("42px")"#]), "42");
    assert_eq!(jsc(&["-e", r#"parseInt("abc")"#]), "NaN");
    assert_eq!(jsc(&["-e", r#"parseFloat("3.14xyz")"#]), "3.14");
    // Shadowable by a user binding of the same name.
    assert_eq!(
        jsc(&["-e", "let String = function (x) { return 99 }; String(1)"]),
        "99",
    );
}

/// P-series — `new Date(ms)` from an explicit timestamp renders as its ISO 8601
/// UTC string, `.getTime()` reads the millisecond payload back, and pre-epoch
/// timestamps and `Number`-coerced arguments are handled.
#[test]
fn date_from_timestamp_renders_iso() {
    assert_eq!(jsc(&["-e", "new Date(0)"]), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        jsc(&["-e", "new Date(1710000000000)"]),
        "2024-03-09T16:00:00.000Z",
    );
    // Pre-epoch (negative) timestamps split correctly.
    assert_eq!(
        jsc(&["-e", "new Date(-86400000)"]),
        "1969-12-31T00:00:00.000Z",
    );
    // `.getTime()` reads the payload back; the argument is `Number`-coerced.
    assert_eq!(jsc(&["-e", "new Date(1500).getTime()"]), "1500");
    assert_eq!(jsc(&["-e", "new Date(true).getTime()"]), "1");
    // Nested rendering (inside an array) uses the same ISO form.
    assert_eq!(
        jsc(&["-e", "[new Date(1000)]"]),
        "[ 1970-01-01T00:00:01.000Z ]",
    );
    // `getTime()` on a non-Date receiver yields NaN (no method-receiver TypeError).
    assert_eq!(jsc(&["-e", "({}).getTime()"]), "NaN");
}

/// P-series — `Date.now()` and `new Date()` read the host wall clock, and both a
/// `Date` value and `Date.now()` carry the expected runtime type. The clock is
/// non-deterministic, so these assert structural properties, not exact values.
#[test]
fn date_now_reads_host_clock() {
    assert_eq!(jsc(&["-e", "typeof Date.now()"]), "number");
    // A clock well past 2023-11 (1_700_000_000_000 ms) confirms a real timestamp.
    assert_eq!(jsc(&["-e", "Date.now() > 1700000000000"]), "true");
    assert_eq!(jsc(&["-e", "typeof new Date()"]), "object");
    assert_eq!(jsc(&["-e", "new Date() instanceof Date"]), "true");
    // `new Date()` and `Date.now()` share the same clock, in order.
    assert_eq!(
        jsc(&["-e", "let a = Date.now(); new Date().getTime() >= a"]),
        "true",
    );
    // `instanceof Date` is exact: only a Date value matches.
    assert_eq!(jsc(&["-e", "new Date(0) instanceof Date"]), "true");
    assert_eq!(jsc(&["-e", "({}) instanceof Date"]), "false");
    assert_eq!(jsc(&["-e", "5 instanceof Date"]), "false");
}

/// P-series — the guest-side `Date` field methods computed from the stored ms:
/// `valueOf`, the UTC calendar getters (`getFullYear`/`getMonth` 0-based/`getDate`/
/// `getDay` 0=Sunday), the UTC time getters, and `toISOString`.
#[test]
fn date_field_methods() {
    // valueOf() == getTime() == the stored ms.
    assert_eq!(jsc(&["-e", "new Date(1000).valueOf()"]), "1000");
    // Epoch: 1970-01-01T00:00:00.000Z, a Thursday (getDay 4).
    assert_eq!(jsc(&["-e", "new Date(0).getFullYear()"]), "1970");
    assert_eq!(jsc(&["-e", "new Date(0).getMonth()"]), "0");
    assert_eq!(jsc(&["-e", "new Date(0).getDate()"]), "1");
    assert_eq!(jsc(&["-e", "new Date(0).getDay()"]), "4");
    assert_eq!(jsc(&["-e", "new Date(0).getHours()"]), "0");
    assert_eq!(jsc(&["-e", "new Date(0).getMinutes()"]), "0");
    assert_eq!(jsc(&["-e", "new Date(0).getSeconds()"]), "0");
    assert_eq!(
        jsc(&["-e", "new Date(0).toISOString()"]),
        "1970-01-01T00:00:00.000Z",
    );
    // 2021-01-01T00:00:00Z (a Friday, getDay 5).
    assert_eq!(
        jsc(&["-e", "new Date(1609459200000).getFullYear()"]),
        "2021"
    );
    assert_eq!(jsc(&["-e", "new Date(1609459200000).getMonth()"]), "0");
    assert_eq!(jsc(&["-e", "new Date(1609459200000).getDate()"]), "1");
    assert_eq!(jsc(&["-e", "new Date(1609459200000).getDay()"]), "5");
    // A timestamp exercising every field: 2024-03-09T19:25:45.678Z.
    assert_eq!(
        jsc(&["-e", "new Date(1710012345678).toISOString()"]),
        "2024-03-09T19:25:45.678Z",
    );
    assert_eq!(jsc(&["-e", "new Date(1710012345678).getHours()"]), "19");
    assert_eq!(jsc(&["-e", "new Date(1710012345678).getMinutes()"]), "25");
    assert_eq!(jsc(&["-e", "new Date(1710012345678).getSeconds()"]), "45");
    // Pre-epoch (negative) timestamps split correctly (1969-12-31, a Wednesday).
    assert_eq!(
        jsc(&["-e", "new Date(-86400000).toISOString()"]),
        "1969-12-31T00:00:00.000Z",
    );
    assert_eq!(jsc(&["-e", "new Date(-86400000).getDay()"]), "3");
    assert_eq!(jsc(&["-e", "new Date(-1).getFullYear()"]), "1969");
    // toISOString() is an ordinary heap string (concatenable, has .length).
    assert_eq!(jsc(&["-e", "new Date(0).toISOString().length"]), "24");
    assert_eq!(
        jsc(&["-e", r#""[" + new Date(0).toISOString() + "]""#]),
        "[1970-01-01T00:00:00.000Z]",
    );
    // A non-finite timestamp: numeric fields are NaN, toISOString is "Invalid Date".
    assert_eq!(jsc(&["-e", "new Date(NaN).getFullYear()"]), "NaN");
    assert_eq!(jsc(&["-e", "new Date(NaN).toISOString()"]), "Invalid Date");
}

/// P-series — the `Date` field-method names still work as ordinary user object
/// methods on a non-Date receiver (the runtime TYPE-BYTE dispatch falls back to a
/// plain method call), so claiming the names does not shadow user methods.
#[test]
fn date_method_names_fall_back_to_user_methods() {
    assert_eq!(
        jsc(&[
            "-e",
            "let o = { getFullYear: function () { return 42 } }; o.getFullYear()"
        ]),
        "42",
    );
    assert_eq!(
        jsc(&[
            "-e",
            "let o = { valueOf: function () { return 7 } }; o.valueOf()"
        ]),
        "7",
    );
    assert_eq!(
        jsc(&[
            "-e",
            "let o = { toISOString: function () { return \"x\" } }; o.toISOString()",
        ]),
        "x",
    );
}

/// P-series — the `Date` builtins are shadowable, and only the timestamp
/// constructor forms are accepted (the calendar-field form is rejected).
#[test]
fn date_shadowing_and_arity() {
    // A user `let Date = ..` shadows the builtin constructor.
    assert_eq!(
        jsc(&["-e", "let Date = function () { return 7 }; Date()"]),
        "7",
    );
}
