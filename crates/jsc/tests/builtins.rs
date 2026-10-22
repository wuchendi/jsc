//! Integration tests: `console.log` / `Math` / conversion built-ins via the CLI.

mod common;
use common::{jsc, jsc_raw_stdout};

/// `console.log` writes to the process's real stdout DURING execution.
///
/// The printed lines are single-space separated with one trailing newline per
/// call, and each argument is formatted exactly as `-p` formats a completion
/// value (numbers bare, strings unquoted, Node-style arrays). `-e` prints only
/// the logged lines; `-p` prints the completion value AFTER them.
#[test]
fn console_log_streams_to_stdout() {
    // Single number argument.
    assert_eq!(jsc_raw_stdout(&["-e", "console.log(40 + 2)"]), "42\n");
    // Unquoted string.
    assert_eq!(jsc_raw_stdout(&["-e", r#"console.log("hi")"#]), "hi\n");
    // Multiple arguments, space-separated, one trailing newline.
    assert_eq!(jsc_raw_stdout(&["-e", "console.log(1, 2, 3)"]), "1 2 3\n");
    // Node-style array formatting.
    assert_eq!(
        jsc_raw_stdout(&["-e", "console.log([1, 2, 3])"]),
        "[ 1, 2, 3 ]\n"
    );
    // Under `-p`, logged lines precede the independent completion value (99).
    assert_eq!(
        jsc_raw_stdout(&["-p", "console.log(1); console.log(2); 99"]),
        "1\n2\n99\n"
    );
}

/// `Math.*` methods and `Math.PI`/`Math.E` compute end-to-end through the
/// CLI, formatting their numeric result exactly like any other final value.
#[test]
fn math_methods_via_eval() {
    assert_eq!(jsc(&["-p", "Math.floor(3.7)"]), "3");
    assert_eq!(jsc(&["-p", "Math.ceil(3.2)"]), "4");
    assert_eq!(jsc(&["-p", "Math.round(2.5)"]), "3");
    assert_eq!(jsc(&["-p", "Math.trunc(-3.7)"]), "-3");
    assert_eq!(jsc(&["-p", "Math.abs(-42)"]), "42");
    assert_eq!(jsc(&["-p", "Math.sqrt(1764)"]), "42");
    assert_eq!(jsc(&["-p", "Math.min(5, 2, 9)"]), "2");
    assert_eq!(jsc(&["-p", "Math.max(1, 42)"]), "42");
    assert_eq!(jsc(&["-p", "Math.pow(2, 10)"]), "1024");
    assert_eq!(jsc(&["-p", "Math.PI"]), "3.141592653589793");
    assert_eq!(jsc(&["-p", "Math.E"]), "2.718281828459045");
    assert_eq!(jsc(&["-p", "typeof Math.PI"]), "number");
}

/// `String`/`Number`/`Boolean`/`parseInt`/`parseFloat` conversions compute
/// end-to-end through the CLI, formatting their result exactly like any other
/// final value.
#[test]
fn conversions_via_eval() {
    // String(x): number form matches `format_number`, and the result is a string.
    assert_eq!(jsc(&["-p", "String(42)"]), "42");
    assert_eq!(jsc(&["-p", "String(3.5)"]), "3.5");
    assert_eq!(jsc(&["-p", "typeof String(42)"]), "string");
    assert_eq!(jsc(&["-p", r#"String(42) + "px""#]), "42px");
    // Number(x): string parse, boolean/null/undefined coercion.
    assert_eq!(jsc(&["-p", r#"Number("42")"#]), "42");
    assert_eq!(jsc(&["-p", r#"Number("  3.5 ")"#]), "3.5");
    assert_eq!(jsc(&["-p", r#"Number("")"#]), "0");
    assert_eq!(jsc(&["-p", r#"Number("x")"#]), "NaN");
    // Boolean(x): JS truthiness.
    assert_eq!(jsc(&["-p", "Boolean(0)"]), "false");
    assert_eq!(jsc(&["-p", r#"Boolean("")"#]), "false");
    assert_eq!(jsc(&["-p", r#"Boolean("x")"#]), "true");
    assert_eq!(jsc(&["-p", "Boolean(1)"]), "true");
    // parseInt / parseFloat: leading numeric prefix.
    assert_eq!(jsc(&["-p", r#"parseInt("42px")"#]), "42");
    assert_eq!(jsc(&["-p", r#"parseInt("abc")"#]), "NaN");
    assert_eq!(jsc(&["-p", r#"parseInt("ff", 16)"#]), "255");
    assert_eq!(jsc(&["-p", r#"parseInt("0xff")"#]), "255");
    assert_eq!(jsc(&["-p", r#"parseInt("101", 2)"#]), "5");
    assert_eq!(jsc(&["-p", r#"parseInt("12", 37)"#]), "NaN");
    assert_eq!(jsc(&["-p", r#"parseFloat("3.14xyz")"#]), "3.14");
    // Shadowable by a user binding of the same name.
    assert_eq!(
        jsc(&["-p", "let String = function (x) { return 99 }; String(1)"]),
        "99",
    );
}

/// `new Date(ms)` from an explicit timestamp renders as its ISO 8601
/// UTC string, `.getTime()` reads the millisecond payload back, and pre-epoch
/// timestamps and `Number`-coerced arguments are handled.
#[test]
fn date_from_timestamp_renders_iso() {
    assert_eq!(jsc(&["-p", "new Date(0)"]), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        jsc(&["-p", "new Date(1710000000000)"]),
        "2024-03-09T16:00:00.000Z",
    );
    // Pre-epoch (negative) timestamps split correctly.
    assert_eq!(
        jsc(&["-p", "new Date(-86400000)"]),
        "1969-12-31T00:00:00.000Z",
    );
    // `.getTime()` reads the payload back; the argument is `Number`-coerced.
    assert_eq!(jsc(&["-p", "new Date(1500).getTime()"]), "1500");
    assert_eq!(jsc(&["-p", "new Date(true).getTime()"]), "1");
    // Nested rendering (inside an array) uses the same ISO form.
    assert_eq!(
        jsc(&["-p", "[new Date(1000)]"]),
        "[ 1970-01-01T00:00:01.000Z ]",
    );
    // `getTime` is a `Date.prototype` method: an object without one throws.
    assert_eq!(
        jsc(&["-p", "try { ({}).getTime() } catch (e) { e.message }"]),
        "expression is not a function",
    );
}

/// `Date.now()` and `new Date()` read the host wall clock, and both a
/// `Date` value and `Date.now()` carry the expected runtime type. The clock is
/// non-deterministic, so these assert structural properties, not exact values.
#[test]
fn date_now_reads_host_clock() {
    assert_eq!(jsc(&["-p", "typeof Date.now()"]), "number");
    // A clock well past 2023-11 (1_700_000_000_000 ms) confirms a real timestamp.
    assert_eq!(jsc(&["-p", "Date.now() > 1700000000000"]), "true");
    assert_eq!(jsc(&["-p", "typeof new Date()"]), "object");
    assert_eq!(jsc(&["-p", "new Date() instanceof Date"]), "true");
    // `new Date()` and `Date.now()` share the same clock, in order.
    assert_eq!(
        jsc(&["-p", "let a = Date.now(); new Date().getTime() >= a"]),
        "true",
    );
    // `instanceof Date` is exact: only a Date value matches.
    assert_eq!(jsc(&["-p", "new Date(0) instanceof Date"]), "true");
    assert_eq!(jsc(&["-p", "({}) instanceof Date"]), "false");
    assert_eq!(jsc(&["-p", "5 instanceof Date"]), "false");
}

/// The guest-side `Date` field methods computed from the stored ms:
/// `valueOf`, the UTC calendar getters (`getFullYear`/`getMonth` 0-based/`getDate`/
/// `getDay` 0=Sunday), the UTC time getters, and `toISOString`.
#[test]
fn date_field_methods() {
    // valueOf() == getTime() == the stored ms.
    assert_eq!(jsc(&["-p", "new Date(1000).valueOf()"]), "1000");
    // Epoch: 1970-01-01T00:00:00.000Z, a Thursday (getDay 4).
    assert_eq!(jsc(&["-p", "new Date(0).getFullYear()"]), "1970");
    assert_eq!(jsc(&["-p", "new Date(0).getMonth()"]), "0");
    assert_eq!(jsc(&["-p", "new Date(0).getDate()"]), "1");
    assert_eq!(jsc(&["-p", "new Date(0).getDay()"]), "4");
    assert_eq!(jsc(&["-p", "new Date(0).getHours()"]), "0");
    assert_eq!(jsc(&["-p", "new Date(0).getMinutes()"]), "0");
    assert_eq!(jsc(&["-p", "new Date(0).getSeconds()"]), "0");
    assert_eq!(
        jsc(&["-p", "new Date(0).toISOString()"]),
        "1970-01-01T00:00:00.000Z",
    );
    // 2021-01-01T00:00:00Z (a Friday, getDay 5).
    assert_eq!(
        jsc(&["-p", "new Date(1609459200000).getFullYear()"]),
        "2021"
    );
    assert_eq!(jsc(&["-p", "new Date(1609459200000).getMonth()"]), "0");
    assert_eq!(jsc(&["-p", "new Date(1609459200000).getDate()"]), "1");
    assert_eq!(jsc(&["-p", "new Date(1609459200000).getDay()"]), "5");
    // A timestamp exercising every field: 2024-03-09T19:25:45.678Z.
    assert_eq!(
        jsc(&["-p", "new Date(1710012345678).toISOString()"]),
        "2024-03-09T19:25:45.678Z",
    );
    assert_eq!(jsc(&["-p", "new Date(1710012345678).getHours()"]), "19");
    assert_eq!(jsc(&["-p", "new Date(1710012345678).getMinutes()"]), "25");
    assert_eq!(jsc(&["-p", "new Date(1710012345678).getSeconds()"]), "45");
    // Pre-epoch (negative) timestamps split correctly (1969-12-31, a Wednesday).
    assert_eq!(
        jsc(&["-p", "new Date(-86400000).toISOString()"]),
        "1969-12-31T00:00:00.000Z",
    );
    assert_eq!(jsc(&["-p", "new Date(-86400000).getDay()"]), "3");
    assert_eq!(jsc(&["-p", "new Date(-1).getFullYear()"]), "1969");
    // toISOString() is an ordinary heap string (concatenable, has .length).
    assert_eq!(jsc(&["-p", "new Date(0).toISOString().length"]), "24");
    assert_eq!(
        jsc(&["-p", r#""[" + new Date(0).toISOString() + "]""#]),
        "[1970-01-01T00:00:00.000Z]",
    );
    // A non-finite timestamp: numeric fields are NaN, toISOString is "Invalid Date".
    assert_eq!(jsc(&["-p", "new Date(NaN).getFullYear()"]), "NaN");
    assert_eq!(jsc(&["-p", "new Date(NaN).toISOString()"]), "Invalid Date");
}

/// The `Date` field-method names work as ordinary user object
/// methods on a non-Date receiver (the runtime TYPE-BYTE dispatch falls back to a
/// plain method call), so claiming the names does not shadow user methods.
#[test]
fn date_method_names_fall_back_to_user_methods() {
    assert_eq!(
        jsc(&[
            "-p",
            "let o = { getFullYear: function () { return 42 } }; o.getFullYear()"
        ]),
        "42",
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let o = { valueOf: function () { return 7 } }; o.valueOf()"
        ]),
        "7",
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let o = { toISOString: function () { return \"x\" } }; o.toISOString()",
        ]),
        "x",
    );
}

/// The `Date` builtins are shadowable.
#[test]
fn date_shadowing_and_arity() {
    // A user `let Date = ..` shadows the builtin constructor.
    assert_eq!(
        jsc(&["-p", "let Date = function () { return 7 }; Date()"]),
        "7",
    );
}

/// Object-family values render through their payload offsets (Map/Set `size`,
/// Date `ms`, Promise `state`/`value`, generator and function blocks). The
/// browser shim asserts the same text in `packages/jsc/tests/run.test.ts`.
#[test]
fn object_family_values_render_through_payload_offsets() {
    let src = "const m = new Map(); m.set(1, 'a'); m.set(2, 'b'); m.delete(2); \
               const s = new Set(); s.add(1); s.add(2); \
               function* gen() { yield 1 } function f(x) { return x } \
               console.log(m, s, m.size, s.size, m.get(1), new Date(5), f, gen(), \
               Promise.resolve(4), Promise.reject(1).catch(() => 0)); \
               [m, s, new Date(0), Promise.resolve(f)]";
    assert_eq!(
        jsc_raw_stdout(&["-e", src]),
        "Map(1) Set(2) 1 2 a 1970-01-01T00:00:00.005Z [Function: f] \
         Object [Generator] Promise { 4 } Promise { <pending> }\n"
    );
    assert_eq!(
        jsc(&["-p", src]).lines().last(),
        Some("[ Map(1), Set(2), 1970-01-01T00:00:00.000Z, Promise { [Function: f] } ]")
    );
}

/// The realm's prototype methods are reached through ordinary property lookup
/// (`%Object.prototype%`, `%Function.prototype%`, `%Error.prototype%`), and
/// an own property shadows them.
#[test]
fn prototype_methods_via_eval() {
    assert_eq!(jsc(&["-p", r#"({ a: 1 }).hasOwnProperty("a")"#]), "true");
    assert_eq!(jsc(&["-p", "({}).toString.call([])"]), "[object Array]");
    assert_eq!(
        jsc(&[
            "-p",
            "function f(a) { return this.k + a } f.call({ k: 1 }, 2)"
        ]),
        "3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function f(a, b) { return a * b } f.apply(null, [6, 7])"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", r#"new ReferenceError("r").toString()"#]),
        "ReferenceError: r"
    );
    assert_eq!(
        jsc(&[
            "-p",
            r#"const o = { toString() { return "own" } }; o.toString()"#
        ]),
        "own"
    );
    assert_eq!(jsc_raw_stdout(&["-e", "console.log({})"]), "{}\n");
}
