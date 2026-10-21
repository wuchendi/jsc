//! Integration tests: `OrdinaryToPrimitive` — every object -> primitive
//! conversion site consults the object's own or inherited `valueOf` /
//! `toString` in hint order (number/default: `valueOf` first; string:
//! `toString` first), skipping non-callable and object-returning methods.

mod common;
use common::{
    assert_bool, assert_nan, assert_number, assert_output, assert_string, assert_uncaught,
};

/// An object with only a user `valueOf`, only a user `toString`, and both.
const V: &str = "const v = { valueOf() { return 7; } };";
const T: &str = "const t = { toString() { return '5'; } };";
const B: &str = "const b = { valueOf() { return 2; }, toString() { return 'str'; } };";

fn with(prelude: &str, expr: &str) -> String {
    format!("{V} {T} {B} {prelude} {expr}")
}

#[test]
fn string_conversion_uses_hint_string() {
    assert_string(&with("", "String(v)"), "[object Object]");
    assert_string(&with("", "String(t)"), "5");
    assert_string(&with("", "String(b)"), "str");
    assert_string(&with("", "`${b}|${v}|${t}`"), "str|[object Object]|5");
}

#[test]
fn addition_uses_hint_default() {
    assert_string(&with("", "'' + v"), "7");
    assert_string(&with("", "t + ''"), "5");
    assert_string(&with("", "'' + b"), "2");
    assert_number(&with("", "v + 1"), 8.0);
    assert_string(&with("", "t + 1"), "51");
    assert_number(&with("", "b + b"), 4.0);
    assert_string("[] + {}", "[object Object]");
    assert_string("[1, 2] + [3]", "1,23");
    assert_number("let x = { valueOf() { return 1; } }; x += 2; x", 3.0);
    assert_string("let m = 'a'; m += { toString() { return 'b'; } }; m", "ab");
}

#[test]
fn numeric_operators_use_hint_number() {
    assert_number(&with("", "+v"), 7.0);
    assert_number(&with("", "+t"), 5.0);
    assert_number(&with("", "+b"), 2.0);
    assert_number(&with("", "-v"), -7.0);
    assert_number(&with("", "v - 1"), 6.0);
    assert_number(&with("", "t * 2"), 10.0);
    assert_number(&with("", "v / 7"), 1.0);
    assert_number(&with("", "v % 4"), 3.0);
    assert_number(&with("", "t ** 2"), 25.0);
    assert_number(&with("", "let w = v; w++; w"), 8.0);
    assert_number(&with("", "Number(b)"), 2.0);
    assert_number(&with("", "parseInt(t)"), 5.0);
    assert_number(
        "parseFloat({ toString() { return '2.5x'; }, valueOf() { return 9; } })",
        2.5,
    );
}

#[test]
fn relational_operators_use_hint_number() {
    assert_bool(&with("", "v > 6"), true);
    assert_bool(&with("", "t < 6"), true);
    assert_bool(&with("", "b <= 2"), true);
    assert_bool(&with("", "b >= 3"), false);
    // Both primitives strings: compared as strings.
    assert_bool("[2] < [10]", false);
    assert_bool("({ toString() { return 'b'; } }) > 'a'", true);
}

#[test]
fn loose_equality_converts_the_object_side() {
    assert_bool(&with("", "v == 7"), true);
    assert_bool(&with("", "t == '5'"), true);
    assert_bool(&with("", "b == 2"), true);
    assert_bool(&with("", "b == 'str'"), false);
    assert_bool(&with("", "1 == { valueOf() { return true; } }"), true);
    assert_bool("[1] == 1", true);
    assert_bool("[1, 2] == '1,2'", true);
    // Object vs object stays identity; nullish never converts.
    assert_bool(&with("", "v == { valueOf() { return 7; } }"), false);
    assert_bool(&with("", "v == null"), false);
    assert_bool(&with("", "v != 7"), false);
}

#[test]
fn property_keys_use_hint_string() {
    assert_number(&with("const o = { str: 1, 2: 3, 5: 4 };", "o[b]"), 1.0);
    assert_number(&with("const o = { str: 1, 2: 3, 5: 4 };", "o[t]"), 4.0);
    assert_number(&with("const o = {}; o[b] = 9;", "o.str"), 9.0);
    assert_bool(&with("const o = { str: 1 };", "b in o"), true);
}

#[test]
fn join_converts_object_elements() {
    assert_string(&with("", "[b, t, v].join('-')"), "str-5-[object Object]");
    assert_string(&with("", "String([b, [t]])"), "str,5");
    assert_string(&with("", "'' + [b]"), "str");
    assert_string("[1, null, undefined, , 2].join()", "1,,,,2");
    assert_string("const a = [1]; a.push(a); a.join('-')", "1-");
}

#[test]
fn inherited_defaults_match_spec() {
    assert_string("String({})", "[object Object]");
    assert_string("String([1, 2])", "1,2");
    assert_string("String(new Error('m'))", "Error: m");
    assert_string("String(new TypeError())", "TypeError");
    assert_number("+[]", 0.0);
    assert_number("+[5]", 5.0);
    assert_nan("+{}");
    assert_string("String(new Map())", "[object Map]");
    assert_string("String(Promise.resolve(1))", "[object Promise]");
    assert_string("String(function f(a) { return a; })", "[Function: f]");
    assert_string("Array.prototype.toString.call({})", "[object Object]");
    assert_string("const a = [3, 4]; a.toString()", "3,4");
    // A user toString on a class prototype is inherited.
    assert_string(
        "class P { toString() { return 'P!'; } } `${new P()}` + new P()",
        "P!P!",
    );
    // A user toString on a function wins over the default rendering.
    assert_string(
        "function f() {} f.toString = function () { return 'F'; }; '' + f",
        "F",
    );
}

#[test]
fn date_default_hint_is_string() {
    assert_number("+new Date(0)", 0.0);
    assert_number("new Date(5) - 1", 4.0);
    assert_number("new Date(0).valueOf()", 0.0);
    assert_string("String(new Date(0))", "1970-01-01T00:00:00.000Z");
    assert_string("'' + new Date(0)", "1970-01-01T00:00:00.000Z");
    assert_string("new Date(0) + 1", "1970-01-01T00:00:00.000Z1");
    assert_bool("new Date(0) == '1970-01-01T00:00:00.000Z'", true);
    assert_bool("new Date(0) < new Date(1)", true);
}

#[test]
fn object_returning_methods_are_skipped() {
    assert_number(
        "+{ valueOf() { return {}; }, toString() { return '3'; } }",
        3.0,
    );
    assert_string(
        "String({ toString() { return []; }, valueOf() { return 4; } })",
        "4",
    );
    // A non-callable method is skipped too.
    assert_string("'' + { valueOf: 1, toString() { return 'x'; } }", "x");
}

#[test]
fn neither_primitive_throws_type_error() {
    let msg = "TypeError: Cannot convert object to primitive value";
    let o = "const o = { valueOf() { return {}; }, toString() { return {}; } };";
    assert_uncaught(&format!("{o} String(o)"), msg);
    assert_uncaught(&format!("{o} '' + o"), msg);
    assert_uncaught(&format!("{o} +o"), msg);
    assert_uncaught(&format!("{o} o < 1"), msg);
    assert_uncaught(&format!("{o} o == 1"), msg);
    assert_bool(
        &format!(
            "{o} let ok = false; try {{ o * 2; }} catch (e) {{ ok = e instanceof TypeError; }} ok"
        ),
        true,
    );
}

#[test]
fn throwing_methods_propagate() {
    let o = "const o = { toString() { throw new RangeError('boom'); } };";
    assert_uncaught(&format!("{o} String(o)"), "RangeError: boom");
    assert_uncaught(&format!("{o} `${{o}}`"), "RangeError: boom");
    assert_uncaught(&format!("{o} [o].join()"), "RangeError: boom");
    assert_uncaught(&format!("{o} +o"), "RangeError: boom");
    assert_output(
        &format!(
            "{o} for (const f of [() => '' + o, () => o - 1, () => o < 1, () => o == 1, () => [o].join()]) \
             {{ try {{ f(); console.log('no'); }} catch (e) {{ console.log(e.message); }} }}"
        ),
        "boom\nboom\nboom\nboom\nboom\n",
    );
    // A throw mid-join leaves the array joinable again.
    assert_string(
        &format!("{o} const a = [1, o]; try {{ a.join(); }} catch (e) {{}} a[1] = 2; a.join()"),
        "1,2",
    );
}

#[test]
fn methods_see_the_receiver_as_this() {
    assert_string(
        "const o = { n: 'me', toString() { return this.n; } }; `${o}`",
        "me",
    );
    assert_number(
        "function C(x) { this.x = x; } C.prototype.valueOf = function () { return this.x * 2; }; new C(4) + 1",
        9.0,
    );
}

/// The official test262 `sta.js` harness error: a plain function constructor
/// whose prototype carries a user `toString`.
#[test]
fn official_test262_error_to_string() {
    let sta = "function Test262Error(message) { this.message = message || ''; } \
               Test262Error.prototype.toString = function () { return 'Test262Error: ' + this.message; };";
    assert_string(
        &format!("{sta} String(new Test262Error('m'))"),
        "Test262Error: m",
    );
    assert_string(
        &format!("{sta} '' + new Test262Error('m')"),
        "Test262Error: m",
    );
    assert_string(
        &format!("{sta} let message = 'x'; message += new Test262Error('m'); message"),
        "xTest262Error: m",
    );
}

/// `console.log` still inspects objects rather than converting them.
#[test]
fn console_log_is_not_a_to_string_site() {
    assert_output(
        "console.log({ toString() { return 'no'; } }, [{ valueOf() { return 1; } }].length)",
        "{ toString: [Function: toString] } 1\n",
    );
}

/// A generator object inherits `%Object.prototype%`'s `toString`.
#[test]
fn generator_objects_convert() {
    assert_string("function* g() {} String(g())", "[object Generator]");
    assert_number(
        "function* g() { return 1; } const o = { [g()]: 1 }; o[String(g())]",
        1.0,
    );
}

/// A computed key read and written by one expression is converted once.
#[test]
fn compound_member_key_converts_once() {
    let k = "let n = 0; const k = { toString() { n++; return 'p'; } }; const o = { p: 1 };";
    assert_number(
        &format!("{k} o[k] += 2; o[k] *= 3; o[k]++; ++o[k]; n * 100 + o.p"),
        411.0,
    );
}
