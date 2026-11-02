//! Integration tests: the `%Date.prototype%` getters, setters and string
//! forms, `Date.UTC`, `Date.parse` and `new Date(string)` (DESIGN.md §6.4). The local time zone is UTC (`LocalTZA = 0`):
//! every local getter/setter equals its `UTC` twin.

mod common;
use common::{assert_bool, assert_nan, assert_number, assert_string, assert_uncaught};

/// A prefix that makes a program not intrinsics-sealed without writing any
/// prototype, so the call-site fast paths run behind their run-time guards.
const OPEN: &str = "let __open = 'prototype'; ";

/// `2020-01-15T10:20:30.400Z`, a Wednesday.
const T: &str = "1579083630400";

/// Assert `src` evaluates to the number `expected`, both intrinsics-sealed
/// and behind run-time guards.
fn both_number(src: &str, expected: f64) {
    assert_number(src, expected);
    assert_number(&format!("{OPEN}{src}"), expected);
}

/// [`both_number`] for `NaN`.
fn both_nan(src: &str) {
    assert_nan(src);
    assert_nan(&format!("{OPEN}{src}"));
}

/// [`both_number`] for a string.
fn both_string(src: &str, expected: &str) {
    assert_string(src, expected);
    assert_string(&format!("{OPEN}{src}"), expected);
}

/// [`both_number`] for a boolean.
fn both_bool(src: &str, expected: bool) {
    assert_bool(src, expected);
    assert_bool(&format!("{OPEN}{src}"), expected);
}

/// The local setters and their `setUTC*` twins.
const SETTER_PAIRS: [(&str, &str); 7] = [
    ("setMilliseconds", "setUTCMilliseconds"),
    ("setSeconds", "setUTCSeconds"),
    ("setMinutes", "setUTCMinutes"),
    ("setHours", "setUTCHours"),
    ("setDate", "setUTCDate"),
    ("setMonth", "setUTCMonth"),
    ("setFullYear", "setUTCFullYear"),
];

// --- getters ---------------------------------------------------------------------

#[test]
fn getters_read_the_utc_fields() {
    for (getter, expected) in [
        ("getMilliseconds", 400.0),
        ("getUTCFullYear", 2020.0),
        ("getUTCMonth", 0.0),
        ("getUTCDate", 15.0),
        ("getUTCDay", 3.0),
        ("getUTCHours", 10.0),
        ("getUTCMinutes", 20.0),
        ("getUTCSeconds", 30.0),
        ("getUTCMilliseconds", 400.0),
        ("getTimezoneOffset", 0.0),
        ("getYear", 120.0),
    ] {
        both_number(&format!("new Date({T}).{getter}()"), expected);
        // Through the function value too.
        both_number(
            &format!("Date.prototype.{getter}.call(new Date({T}))"),
            expected,
        );
    }
}

#[test]
fn local_getters_equal_utc_getters() {
    for field in [
        "FullYear",
        "Month",
        "Date",
        "Day",
        "Hours",
        "Minutes",
        "Seconds",
        "Milliseconds",
    ] {
        both_bool(
            &format!("const d = new Date({T}); d.get{field}() === d.getUTC{field}()"),
            true,
        );
    }
}

#[test]
fn getters_before_the_epoch() {
    both_number("new Date(-1).getMilliseconds()", 999.0);
    both_number("new Date(-1).getUTCFullYear()", 1969.0);
    both_number("new Date(-1).getUTCMonth()", 11.0);
    both_number("new Date(-1).getUTCDate()", 31.0);
    both_number("new Date(-1).getUTCDay()", 3.0);
    both_number("new Date(-1).getUTCHours()", 23.0);
    both_number("new Date(-1).getYear()", 69.0);
    both_number("new Date(-1).getTimezoneOffset()", 0.0);
}

#[test]
fn getters_of_an_invalid_date_are_nan() {
    for getter in [
        "getMilliseconds",
        "getTimezoneOffset",
        "getUTCFullYear",
        "getUTCMonth",
        "getUTCDate",
        "getUTCDay",
        "getUTCHours",
        "getUTCMinutes",
        "getUTCSeconds",
        "getUTCMilliseconds",
        "getYear",
    ] {
        both_nan(&format!("new Date(NaN).{getter}()"));
    }
}

// --- function values -------------------------------------------------------------

#[test]
fn methods_have_names_and_lengths() {
    let getters = [
        "getMilliseconds",
        "getTimezoneOffset",
        "getUTCFullYear",
        "getUTCMonth",
        "getUTCDate",
        "getUTCDay",
        "getUTCHours",
        "getUTCMinutes",
        "getUTCSeconds",
        "getUTCMilliseconds",
        "getYear",
    ];
    let setters = [
        ("setMilliseconds", 1.0),
        ("setSeconds", 2.0),
        ("setMinutes", 3.0),
        ("setHours", 4.0),
        ("setDate", 1.0),
        ("setMonth", 2.0),
        ("setFullYear", 3.0),
        ("setUTCMilliseconds", 1.0),
        ("setUTCSeconds", 2.0),
        ("setUTCMinutes", 3.0),
        ("setUTCHours", 4.0),
        ("setUTCDate", 1.0),
        ("setUTCMonth", 2.0),
        ("setUTCFullYear", 3.0),
        ("setTime", 1.0),
        ("setYear", 1.0),
    ];
    for (name, length) in getters.map(|g| (g, 0.0)).into_iter().chain(setters) {
        both_string(&format!("typeof Date.prototype.{name}"), "function");
        both_string(&format!("Date.prototype.{name}.name"), name);
        both_number(&format!("Date.prototype.{name}.length"), length);
        both_bool(
            &format!(
                "const d = Object.getOwnPropertyDescriptor(Date.prototype, '{name}'); \
                 d.writable && !d.enumerable && d.configurable"
            ),
            true,
        );
    }
    for (local, utc) in SETTER_PAIRS {
        both_bool(
            &format!("Date.prototype.{local} === Date.prototype.{utc}"),
            false,
        );
    }
}

#[test]
fn date_utc_is_a_static() {
    both_string("typeof Date.UTC", "function");
    both_string("Date.UTC.name", "UTC");
    both_number("Date.UTC.length", 7.0);
    both_bool("Object.keys(Date).includes('UTC')", false);
}

#[test]
fn methods_on_a_non_date_throw() {
    for name in ["getUTCHours", "getTimezoneOffset", "getYear"] {
        assert_uncaught(
            &format!("Date.prototype.{name}.call({{}})"),
            "TypeError: this is not a Date object.",
        );
    }
    for (local, utc) in SETTER_PAIRS {
        for name in [local, utc] {
            assert_uncaught(
                &format!("Date.prototype.{name}.call({{}}, 1)"),
                "TypeError: this is not a Date object.",
            );
        }
    }
    assert_uncaught(
        "Date.prototype.setTime.call(5, 1)",
        "TypeError: this is not a Date object.",
    );
    assert_uncaught(
        "Date.prototype.setYear.call(Date.prototype, 1)",
        "TypeError: this is not a Date object.",
    );
    // `thisTimeValue` throws before any argument is converted.
    both_number(
        "let n = 0; const v = { valueOf() { n++; return 1 } }; \
         try { Date.prototype.setHours.call({}, v, v) } catch (e) {} n",
        0.0,
    );
}

// --- setters ---------------------------------------------------------------------

#[test]
fn setters_replace_their_fields() {
    for (call, expected) in [
        ("setMilliseconds(5)", 1_579_083_630_005.0),
        ("setSeconds(45)", 1_579_083_645_400.0),
        ("setMinutes(7, 8, 9)", 1_579_082_828_009.0),
        ("setHours(23, 5, 6, 7)", 1_579_129_506_007.0),
        ("setDate(31)", 1_580_466_030_400.0),
        ("setDate(32)", 1_580_552_430_400.0),
        ("setMonth(5, 10)", 1_591_784_430_400.0),
        ("setFullYear(2021, 1, 28)", 1_614_507_630_400.0),
    ] {
        // The local setter, its UTC twin, the stored value, and the setter
        // called through its function value.
        both_number(&format!("new Date({T}).{call}"), expected);
        let utc = call.replacen("set", "setUTC", 1);
        both_number(&format!("new Date({T}).{utc}"), expected);
        both_number(
            &format!("const d = new Date({T}); d.{call}; d.getTime()"),
            expected,
        );
        let (name, args) = call.split_once('(').unwrap_or((call, ")"));
        both_number(
            &format!("Date.prototype.{name}.call(new Date({T}), {args}"),
            expected,
        );
    }
}

#[test]
fn absent_optional_arguments_keep_their_fields() {
    both_number(&format!("new Date({T}).setHours(0)"), 1_579_047_630_400.0);
    both_number(&format!("new Date({T}).setMinutes(0)"), 1_579_082_430_400.0);
    both_number(&format!("new Date({T}).setMonth(1)"), 1_581_762_030_400.0);
    both_number(
        &format!("new Date({T}).setFullYear(2019)"),
        1_547_547_630_400.0,
    );
    // An explicit `undefined` is present: `ToNumber(undefined)` is `NaN`.
    both_nan(&format!("new Date({T}).setMinutes(1, undefined)"));
    both_nan(&format!("new Date({T}).setFullYear(2020, undefined)"));
    // A spread call's arguments.
    both_number("new Date(0).setHours(...[1, 2, 3, 4])", 3_723_004.0);
}

#[test]
fn setters_normalise_out_of_range_fields() {
    both_number("new Date(0).setUTCMonth(-1)", -2_678_400_000.0);
    both_number("new Date(0).setUTCMonth(12)", 31_536_000_000.0);
    both_number("new Date(0).setUTCHours(25)", 90_000_000.0);
    both_number("new Date(0).setUTCMilliseconds(-1)", -1.0);
    // Fractional fields are truncated.
    both_number("new Date(0).setUTCDate(1.9)", 0.0);
    both_number("new Date(0).setUTCSeconds(-1.5)", -1000.0);
    // A non-finite field makes the date invalid.
    both_nan("new Date(0).setUTCHours(Infinity)");
    both_nan("const d = new Date(0); d.setUTCDate(NaN); d.getTime()");
}

#[test]
fn setters_time_clip() {
    both_number("new Date(0).setTime(8.64e15)", 8.64e15);
    both_nan("new Date(0).setTime(8.64e15 + 1)");
    both_nan("new Date(8.64e15).setMilliseconds(1)");
    both_nan("const d = new Date(8.64e15); d.setUTCSeconds(1); d.getTime()");
    both_number("new Date(-8.64e15).setUTCMilliseconds(0)", -8.64e15);
    both_bool("1 / new Date(5).setTime(-0.5) === Infinity", true);
}

#[test]
fn set_time_converts_and_clips() {
    both_number("new Date(0).setTime(5)", 5.0);
    both_number("new Date(0).setTime('7')", 7.0);
    both_number("new Date(0).setTime(1.9)", 1.0);
    both_number("new Date(NaN).setTime(3)", 3.0);
    both_nan("new Date(0).setTime()");
    both_nan("const d = new Date(0); d.setTime(); d.getTime()");
    both_number("new Date(0).setTime({ valueOf() { return 9 } })", 9.0);
}

#[test]
fn setters_on_an_invalid_date_return_nan() {
    for (local, utc) in SETTER_PAIRS {
        for name in [local, utc] {
            if name.ends_with("FullYear") {
                continue;
            }
            both_nan(&format!("new Date(NaN).{name}(1)"));
            both_nan(&format!(
                "const d = new Date(NaN); d.{name}(1); d.getTime()"
            ));
        }
    }
}

#[test]
fn set_full_year_on_an_invalid_date_starts_from_zero() {
    both_number("new Date(NaN).setFullYear(1950)", -631_152_000_000.0);
    both_number(
        "new Date(NaN).setUTCFullYear(2024, 1, 29)",
        1_709_164_800_000.0,
    );
    both_number(
        "const d = new Date(NaN); d.setFullYear(2000); d.getTime()",
        946_684_800_000.0,
    );
    // Two-digit years are not mapped by setFullYear.
    both_number("new Date(0).setUTCFullYear(50)", -60_589_296_000_000.0);
}

#[test]
fn set_year_maps_two_digit_years() {
    both_number(&format!("new Date({T}).setYear(95)"), 790_165_230_400.0);
    both_number(&format!("new Date({T}).setYear(95.7)"), 790_165_230_400.0);
    both_number(&format!("new Date({T}).setYear(2000)"), 947_931_630_400.0);
    both_number("new Date(NaN).setYear(2000)", 946_684_800_000.0);
    both_number("new Date(NaN).setYear(0)", -2_208_988_800_000.0);
    both_nan("new Date(0).setYear(NaN)");
    both_nan("new Date(0).setYear()");
    both_number(
        &format!("const d = new Date({T}); d.setYear(95); d.getYear()"),
        95.0,
    );
}

// --- argument coercion order -----------------------------------------------------

#[test]
fn arguments_convert_in_order_even_on_an_invalid_date() {
    let probe = "const log = []; \
                 const v = (n, r) => ({ valueOf() { log.push(n); return r } });";
    both_string(
        &format!(
            "{probe} const r = new Date(NaN).setHours(v('h', 1), v('m', 2), v('s', 3), v('ms', 4)); \
             log.join() + ':' + isNaN(r)"
        ),
        "h,m,s,ms:true",
    );
    both_string(
        &format!(
            "{probe} const r = new Date(NaN).setUTCMonth(v('m', 1), v('d', 2)); \
             log.join() + ':' + isNaN(r)"
        ),
        "m,d:true",
    );
    both_string(
        &format!(
            "{probe} const r = new Date(0).setFullYear(v('y', 2000), v('m', 0), v('d', 1)); \
             log.join() + ':' + r"
        ),
        "y,m,d:946684800000",
    );
    // A throwing conversion stops the later ones and leaves the date as is.
    both_string(
        "const log = []; const d = new Date(0); \
         try { d.setMinutes({ valueOf() { log.push('a'); throw 1 } }, \
                            { valueOf() { log.push('b'); return 0 } }) } catch (e) {} \
         log.join() + ':' + d.getTime()",
        "a:0",
    );
}

#[test]
fn the_time_value_is_read_before_the_conversions() {
    // A valid date: the fields come from the time value before `valueOf`
    // changed it.
    both_number(
        "const d = new Date(0); \
         d.setMilliseconds({ valueOf() { d.setTime(86400000); return 5 } })",
        5.0,
    );
    both_number(
        "const d = new Date(0); \
         d.setUTCHours(1, { valueOf() { d.setTime(86400000); return 0 } }); d.getTime()",
        3_600_000.0,
    );
    // An invalid date stays untouched: the setter returns NaN without storing.
    both_number(
        "const d = new Date(NaN); \
         d.setMilliseconds({ valueOf() { d.setTime(7); return 1 } }); d.getTime()",
        7.0,
    );
    // setFullYear starts from +0 instead.
    both_number(
        "const d = new Date(NaN); \
         d.setFullYear({ valueOf() { d.setTime(7); return 1970 } })",
        0.0,
    );
}

// --- Date.UTC --------------------------------------------------------------------

#[test]
fn date_utc_computes_a_time_value() {
    both_number(
        "Date.UTC(2020, 0, 15, 10, 20, 30, 400)",
        1_579_083_630_400.0,
    );
    both_number("Date.UTC(2020)", 1_577_836_800_000.0);
    both_number("Date.UTC(99)", 915_148_800_000.0);
    both_number("Date.UTC(1970, 0, 1, 0, 0, 0, 0.9)", 0.0);
    both_number("Date.UTC(1970, 12)", 31_536_000_000.0);
    both_number(
        "Date.UTC(...[2020, 0, 15, 10, 20, 30, 400])",
        1_579_083_630_400.0,
    );
    both_number("Date.UTC('2020', '0')", 1_577_836_800_000.0);
    both_nan("Date.UTC()");
    both_nan("Date.UTC(2020, NaN)");
    both_nan("Date.UTC(275760, 8, 13, 0, 0, 0, 1)");
    both_number("Date.UTC(275760, 8, 13)", 8.64e15);
    both_bool("new Date(Date.UTC(2020, 1, 29)).getUTCDate() === 29", true);
}

#[test]
fn date_utc_converts_every_argument_in_order() {
    both_string(
        "const log = []; \
         const v = (n) => ({ valueOf() { log.push(n); return 1 } }); \
         Date.UTC(v(0), v(1), v(2), v(3), v(4), v(5), v(6)); log.join()",
        "0,1,2,3,4,5,6",
    );
    both_string(
        "const log = []; \
         const v = (n, r) => ({ valueOf() { log.push(n); return r } }); \
         const r = Date.UTC(v('y', NaN), v('m', 0)); log.join() + ':' + isNaN(r)",
        "y,m:true",
    );
}

// --- string forms ----------------------------------------------------------------

/// The string forms and their text for [`T`].
const STRING_FORMS: [(&str, &str); 7] = [
    (
        "toString",
        "Wed Jan 15 2020 10:20:30 GMT+0000 (Coordinated Universal Time)",
    ),
    ("toDateString", "Wed Jan 15 2020"),
    (
        "toTimeString",
        "10:20:30 GMT+0000 (Coordinated Universal Time)",
    ),
    ("toUTCString", "Wed, 15 Jan 2020 10:20:30 GMT"),
    (
        "toLocaleString",
        "Wed Jan 15 2020 10:20:30 GMT+0000 (Coordinated Universal Time)",
    ),
    ("toLocaleDateString", "Wed Jan 15 2020"),
    (
        "toLocaleTimeString",
        "10:20:30 GMT+0000 (Coordinated Universal Time)",
    ),
];

#[test]
fn string_forms_render_the_utc_fields() {
    for (method, expected) in STRING_FORMS {
        both_string(&format!("new Date({T}).{method}()"), expected);
        both_string(
            &format!("Date.prototype.{method}.call(new Date({T}))"),
            expected,
        );
    }
    both_string(
        "new Date(-1).toUTCString()",
        "Wed, 31 Dec 1969 23:59:59 GMT",
    );
    both_string("new Date(-1).toDateString()", "Wed Dec 31 1969");
}

#[test]
fn string_forms_of_an_invalid_date_are_invalid_date() {
    for (method, _) in STRING_FORMS {
        both_string(&format!("new Date(NaN).{method}()"), "Invalid Date");
        both_string(&format!("new Date(8.64e15 + 1).{method}()"), "Invalid Date");
    }
    both_string("new Date(NaN).toGMTString()", "Invalid Date");
}

#[test]
fn string_forms_pad_and_sign_the_year() {
    // Year 0, year -1 (four digits after the sign), and the time value range ends.
    both_string(
        "new Date(-62167219200000).toDateString()",
        "Sat Jan 01 0000",
    );
    both_string(
        "new Date(-62198755200000).toUTCString()",
        "Fri, 01 Jan -0001 00:00:00 GMT",
    );
    both_string(
        "new Date(-62198755200000).toString()",
        "Fri Jan 01 -0001 00:00:00 GMT+0000 (Coordinated Universal Time)",
    );
    both_string("new Date(8.64e15).toDateString()", "Sat Sep 13 275760");
    both_string(
        "new Date(-8.64e15).toUTCString()",
        "Tue, 20 Apr -271821 00:00:00 GMT",
    );
    both_string(
        "new Date(-8.64e15).toTimeString()",
        "00:00:00 GMT+0000 (Coordinated Universal Time)",
    );
}

#[test]
fn to_iso_string_throws_on_an_invalid_date_and_expands_years() {
    assert_uncaught(
        "new Date(NaN).toISOString()",
        "RangeError: Invalid time value",
    );
    both_string(
        "new Date(-62198755200000).toISOString()",
        "-000001-01-01T00:00:00.000Z",
    );
    both_string(
        "new Date(8.64e15).toISOString()",
        "+275760-09-13T00:00:00.000Z",
    );
}

#[test]
fn string_forms_are_methods_with_spec_shape() {
    for (method, _) in STRING_FORMS {
        both_string(&format!("Date.prototype.{method}.name"), method);
        both_number(&format!("Date.prototype.{method}.length"), 0.0);
        both_string(
            &format!(
                "(() => {{ const d = Object.getOwnPropertyDescriptor(Date.prototype, '{method}'); \
                 return `${{d.writable}} ${{d.enumerable}} ${{d.configurable}}`; }})()"
            ),
            "true false true",
        );
        assert_uncaught(
            &format!("Date.prototype.{method}.call({{}})"),
            "TypeError: this is not a Date object.",
        );
    }
}

#[test]
fn to_gmt_string_is_to_utc_string() {
    both_bool(
        "Date.prototype.toGMTString === Date.prototype.toUTCString",
        true,
    );
    both_string("Date.prototype.toGMTString.name", "toUTCString");
    both_string(
        &format!("new Date({T}).toGMTString()"),
        "Wed, 15 Jan 2020 10:20:30 GMT",
    );
    both_bool(
        "Object.getOwnPropertyDescriptor(Date.prototype, 'toGMTString').enumerable",
        false,
    );
}

#[test]
fn date_called_as_a_function_returns_a_string() {
    both_string("typeof Date()", "string");
    both_string("typeof Date(0, 1, 2)", "string");
    both_bool(
        "Date(0).endsWith(' GMT+0000 (Coordinated Universal Time)')",
        true,
    );
}

// --- Date.parse ----------------------------------------------------------------

#[test]
fn date_parse_is_a_static() {
    both_string("typeof Date.parse", "function");
    both_string("Date.parse.name", "parse");
    both_number("Date.parse.length", 1.0);
    both_bool("Object.keys(Date).includes('parse')", false);
}

#[test]
fn date_parse_reads_every_iso_form() {
    for (text, expected) in [
        ("2020", 1_577_836_800_000.0),
        ("2020-02", 1_580_515_200_000.0),
        ("2020-02-29", 1_582_934_400_000.0),
        ("2020T10:20", 1_577_874_000_000.0),
        ("2020-01-15T10:20", 1_579_083_600_000.0),
        ("2020-01-15T10:20:30", 1_579_083_630_000.0),
        ("2020-01-15T10:20:30.400", 1_579_083_630_400.0),
        ("2020-01-15T10:20:30.400Z", 1_579_083_630_400.0),
        ("+002020-01-15T10:20:30.400Z", 1_579_083_630_400.0),
        ("0000", -62_167_219_200_000.0),
        ("+000000-01-01T00:00:00Z", -62_167_219_200_000.0),
        ("-000001-01-01T00:00:00Z", -62_198_755_200_000.0),
        ("1970-01-01", 0.0),
        ("1970-01-01T00:00:00", 0.0),
    ] {
        both_number(&format!("Date.parse('{text}')"), expected);
    }
}

#[test]
fn date_parse_applies_the_utc_offset() {
    both_number(
        "Date.parse('2020-01-15T10:20:30.400+01:00')",
        1_579_080_030_400.0,
    );
    both_number(
        "Date.parse('2020-01-15T10:20:30.400-05:30')",
        1_579_103_430_400.0,
    );
    both_number("Date.parse('2020-01-15T10:20+00:00')", 1_579_083_600_000.0);
    both_number("Date.parse('2020-01-15T10:20-00:00')", 1_579_083_600_000.0);
    // A date-time without an offset is local time, which is UTC.
    both_number(
        "Date.parse('1970-01-01T00:00:00') - new Date(0).getTimezoneOffset() * 60000",
        0.0,
    );
}

#[test]
fn date_parse_accepts_24_00_only_as_midnight() {
    both_number("Date.parse('2020-01-15T24:00')", 1_579_132_800_000.0);
    both_number(
        "Date.parse('2020-01-15T24:00:00.000Z')",
        1_579_132_800_000.0,
    );
    for text in [
        "2020-01-15T24:01",
        "2020-01-15T24:00:01",
        "2020-01-15T24:00:00.001",
    ] {
        both_nan(&format!("Date.parse('{text}')"));
    }
}

#[test]
fn date_parse_rejects_out_of_range_fields() {
    for text in [
        "2020-00",
        "2020-13",
        "2020-01-00",
        "2020-01-32",
        "2020-04-31",
        "2020-02-30",
        "2019-02-29",
        "2100-02-29",
        "2020-01-15T25:00",
        "2020-01-15T10:60",
        "2020-01-15T10:20:60",
        "2020-01-15T10:20+24:00",
        "2020-01-15T10:20+01:60",
    ] {
        both_nan(&format!("Date.parse('{text}')"));
    }
    // 2000 is a leap year.
    both_number("Date.parse('2000-02-29')", 951_782_400_000.0);
}

#[test]
fn date_parse_rejects_minus_zero_as_an_expanded_year() {
    for text in [
        "-000000",
        "-000000-03-31T00:45Z",
        "-000000-03-31T01:45",
        "-000000-03-31T01:45:00+01:00",
    ] {
        both_nan(&format!("Date.parse('{text}')"));
    }
}

#[test]
fn date_parse_time_clips_the_result() {
    both_number("Date.parse('+275760-09-13T00:00:00.000Z')", 8.64e15);
    both_number("Date.parse('-271821-04-20T00:00:00.000Z')", -8.64e15);
    both_nan("Date.parse('+275760-09-13T00:00:00.001Z')");
    both_nan("Date.parse('-271821-04-19T23:59:59.999Z')");
}

#[test]
fn date_parse_reads_the_to_string_and_to_utc_string_forms() {
    for (text, expected) in [
        (
            "Wed Jan 15 2020 10:20:30 GMT+0000 (Coordinated Universal Time)",
            1_579_083_630_000.0,
        ),
        ("Wed Jan 15 2020 10:20:30 GMT+0000", 1_579_083_630_000.0),
        ("Wed Jan 15 2020 10:20:30 GMT+0100", 1_579_080_030_000.0),
        (
            "Wed Jan 15 2020 10:20:30 GMT-0500 (Eastern Standard Time)",
            1_579_101_630_000.0,
        ),
        ("Wed, 15 Jan 2020 10:20:30 GMT", 1_579_083_630_000.0),
        ("Fri, 01 Jan -0001 00:00:00 GMT", -62_198_755_200_000.0),
        (
            "Fri Jan 01 -0001 00:00:00 GMT+0000 (Coordinated Universal Time)",
            -62_198_755_200_000.0,
        ),
    ] {
        both_number(&format!("Date.parse('{text}')"), expected);
    }
}

#[test]
fn date_parse_round_trips_the_string_forms() {
    // `toString` / `toUTCString` drop the milliseconds; `toISOString` keeps them.
    both_bool(
        "const ts = [0, 1579083630400, -1, -62198755200000, -62167219200000, \
         253402300800000, 8.64e15, -8.64e15]; \
         ts.every(t => { \
           const d = new Date(t), s = Math.floor(t / 1000) * 1000; \
           return Date.parse(d.toString()) === s && Date.parse(d.toUTCString()) === s \
             && Date.parse(d.toISOString()) === t; \
         })",
        true,
    );
}

#[test]
fn date_parse_of_anything_else_is_nan() {
    for text in [
        "",
        "garbage",
        " 2020",
        "2020 ",
        "20200",
        "2020-1-15",
        "2020-01-15T",
        "2020-01-15T10",
        "2020-01-15Z",
        "2020-01-15T10:20:30.4",
        "2020-01-15T10:20:30.400z",
        "+2020-01-15",
        "Wed Jan 15 2020",
        "Wed Jan 15 2020 10:20:30",
        "Wed Jan 15 2020 10:20:30 GMT",
        "Wed Jan 15 2020 10:20:30 GMT+0160",
        "Wed Jan 15 2020 10:20:30 GMT+2400",
        "Wed Jan 15 2020 10:20:30 GMT+0000x",
        "Wed Jan 15 2020 10:20:30 GMT+0000 (UTC",
        "Wed Foo 15 2020 10:20:30 GMT+0000",
        "Foo Jan 15 2020 10:20:30 GMT+0000",
        "Wed Feb 30 2020 00:00:00 GMT+0000",
        "Fri Jan 01 -0000 00:00:00 GMT+0000",
        "Wed, 15 Jan 20 10:20:30 GMT",
        "Wed, 15 Jan 2020 10:20:30 GMT+0000",
        "2020-01-15T10:20:30Z\\u0100",
    ] {
        both_nan(&format!("Date.parse('{text}')"));
    }
}

#[test]
fn date_parse_converts_its_argument_with_to_string() {
    both_number(
        "Date.parse({ toString() { return '2020' } })",
        1_577_836_800_000.0,
    );
    both_number("Date.parse(2020)", 1_577_836_800_000.0);
    both_nan("Date.parse()");
    both_nan("Date.parse(null)");
    both_bool(
        "try { Date.parse(Symbol()); false } catch (e) { e instanceof TypeError }",
        true,
    );
}

// --- new Date(value) -----------------------------------------------------------

#[test]
fn new_date_parses_a_string() {
    both_number(
        "new Date('2020-01-15T10:20:30.400Z').getTime()",
        1_579_083_630_400.0,
    );
    both_number(
        "new Date('Wed, 15 Jan 2020 10:20:30 GMT').getTime()",
        1_579_083_630_000.0,
    );
    both_nan("new Date('nope').getTime()");
    both_string("String(new Date('2020-13'))", "Invalid Date");
    both_nan("new Date('+275760-09-13T00:00:00.001Z').getTime()");
    // Through the constructor function value too (the builtin body).
    both_number(
        "const D = Date; new D('2020').getTime()",
        1_577_836_800_000.0,
    );
}

#[test]
fn new_date_converts_its_argument_with_to_primitive() {
    both_string(
        "let hint; new Date({ [Symbol.toPrimitive](h) { hint = h; return 1 } }); hint",
        "default",
    );
    both_number(
        "new Date({ [Symbol.toPrimitive]() { return '2016-06-05T18:40:00.000Z' } }).getTime()",
        1_465_152_000_000.0,
    );
    // Hint default tries `valueOf` first; a string result is parsed.
    both_number(
        "new Date({ valueOf() { return 5 }, toString() { return '2020' } }).getTime()",
        5.0,
    );
    both_number(
        "new Date({ valueOf() { return {} }, toString() { return '2020' } }).getTime()",
        1_577_836_800_000.0,
    );
    both_number(
        "new Date(new String('2020')).getTime()",
        1_577_836_800_000.0,
    );
    // A Date argument contributes its time value without `ToPrimitive` (the
    // override is defined: the inherited `Date.prototype[@@toPrimitive]` is
    // read-only, so assigning it throws).
    both_number(
        "const d = new Date(5); \
         Object.defineProperty(d, Symbol.toPrimitive, { value: () => 7, configurable: true }); \
         new Date(d).getTime()",
        5.0,
    );
    both_bool(
        "try { new Date({ valueOf() { throw new RangeError('x') } }); false } \
         catch (e) { e instanceof RangeError }",
        true,
    );
    both_bool(
        "try { new Date(Symbol()); false } catch (e) { e instanceof TypeError }",
        true,
    );
}
