//! Integration tests: the namespace and constructor statics as builtins
//! (DESIGN.md §6.4): `JSON.parse` revivers, `JSON.stringify` replacers and
//! indentation through every call shape (inline `JSON.x`, a guarded call on
//! an escaping `JSON`, a call through a value), JS arity for every builtin
//! static, variadic builtins called through a value with more than `K`
//! arguments, and spread calls to the statics.

mod common;
use common::{
    assert_bool, assert_eval, assert_nan, assert_number, assert_output, assert_string,
    assert_uncaught,
};

/// `(source, expected string completion)` pairs written against `JSON`;
/// expected values are the output of a spec-conforming engine.
const JSON_CASES: &[(&str, &str)] = &[
    (
        "JSON.stringify(JSON.parse('{\"a\":1,\"b\":[1,2,{\"c\":3}],\"d\":{\"e\":4}}', function (k, v) { return typeof v === 'number' ? v * 10 : v }))",
        "{\"a\":10,\"b\":[10,20,{\"c\":30}],\"d\":{\"e\":40}}",
    ),
    (
        "JSON.stringify(JSON.parse('{\"a\":1,\"b\":2,\"c\":3}', function (k, v) { return k === 'b' ? undefined : v }))",
        "{\"a\":1,\"c\":3}",
    ),
    (
        "JSON.stringify(JSON.parse('[1,2,3]', function (k, v) { return k === '1' ? undefined : v }))",
        "[1,null,3]",
    ),
    (
        "(() => { let p = JSON.parse('[1,2,3]', function (k, v) { return k === '1' ? undefined : v }); return String(1 in p) + p.length })()",
        "false3",
    ),
    (
        "(() => { let log = []; JSON.parse('{\"a\":{\"b\":1},\"c\":[2]}', function (k, v) { log.push(k + ':' + (Array.isArray(this) ? 'arr' : typeof this)); return v }); return log.join(',') })()",
        "b:object,a:object,0:arr,c:object,:object",
    ),
    (
        "String(JSON.parse('{\"x\":1}', function (k, v) { if (k === '') { return this[''] === v } return v }))",
        "true",
    ),
    (
        "JSON.stringify(JSON.parse('5', function (k, v) { return [k, v] }))",
        "[\"\",5]",
    ),
    ("JSON.stringify(JSON.parse('{\"a\":1}', null))", "{\"a\":1}"),
    ("JSON.stringify(JSON.parse('{\"a\":1}', 5))", "{\"a\":1}"),
    (
        "JSON.stringify(JSON.parse('{\"a\":{\"b\":2}}', function (k, v) { if (k === 'a') { v.extra = 1 } return v }))",
        "{\"a\":{\"b\":2,\"extra\":1}}",
    ),
    (
        "JSON.stringify({a:1,b:'x',c:[1,2],d:{e:true}}, function (k, v) { return typeof v === 'number' ? v + 1 : v })",
        "{\"a\":2,\"b\":\"x\",\"c\":[2,3],\"d\":{\"e\":true}}",
    ),
    (
        "JSON.stringify({a:1,b:2}, function (k, v) { return k === 'a' ? undefined : v })",
        "{\"b\":2}",
    ),
    (
        "JSON.stringify([1,2,3], function (k, v) { return k === '1' ? undefined : v })",
        "[1,null,3]",
    ),
    (
        "(() => { let ks = []; JSON.stringify({a:{b:1}, c:[5]}, function (k, v) { ks.push(k + '/' + (Array.isArray(this) ? 'arr' : typeof this)); return v }); return ks.join(',') })()",
        "/object,a/object,b/object,c/object,0/arr",
    ),
    (
        "JSON.stringify({a:1}, function (k, v) { return k === '' ? this[''] === v : v })",
        "true",
    ),
    (
        "JSON.stringify(1, function (k, v) { return k === '' ? {wrapped: v, key: k} : v })",
        "{\"wrapped\":1,\"key\":\"\"}",
    ),
    (
        "JSON.stringify({a:1}, function (k, v) { return k === '' ? [v, function(){}, undefined] : v })",
        "[{\"a\":1},null,null]",
    ),
    (
        "String(JSON.stringify({a:1}, function (k, v) { return undefined }))",
        "undefined",
    ),
    (
        "JSON.stringify({a:1,b:2,c:3}, ['c','a'])",
        "{\"c\":3,\"a\":1}",
    ),
    (
        "JSON.stringify({1:'one',a:'x',2:'two'}, [2,'a',1])",
        "{\"2\":\"two\",\"a\":\"x\",\"1\":\"one\"}",
    ),
    (
        "JSON.stringify({a:1,b:2}, ['a','a','b','a'])",
        "{\"a\":1,\"b\":2}",
    ),
    (
        "JSON.stringify({a:{a:1,b:2},b:3}, ['a'])",
        "{\"a\":{\"a\":1}}",
    ),
    (
        "JSON.stringify({a:1,b:2}, [new String('b'), new Number(1), {}, true, null])",
        "{\"b\":2}",
    ),
    ("JSON.stringify([{a:1,b:2}], ['b'])", "[{\"b\":2}]"),
    ("JSON.stringify({a:1,b:2}, [])", "{}"),
    ("JSON.stringify({a:1,b:2}, ['zz', 'b'])", "{\"b\":2}"),
    (
        "JSON.stringify({a:1,b:[1,2],c:{},d:[]}, null, 2)",
        "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {},\n  \"d\": []\n}",
    ),
    (
        "JSON.stringify([1,[2,{x:3}]], null, '--')",
        "[\n--1,\n--[\n----2,\n----{\n------\"x\": 3\n----}\n--]\n]",
    ),
    (
        "JSON.stringify({a:1}, null, 20)",
        "{\n          \"a\": 1\n}",
    ),
    (
        "JSON.stringify({a:1}, null, '0123456789abc')",
        "{\n0123456789\"a\": 1\n}",
    ),
    ("JSON.stringify({a:1}, null, 0)", "{\"a\":1}"),
    ("JSON.stringify({a:1}, null, -3)", "{\"a\":1}"),
    ("JSON.stringify({a:1}, null, 2.9)", "{\n  \"a\": 1\n}"),
    ("JSON.stringify({a:1}, null, '')", "{\"a\":1}"),
    (
        "JSON.stringify({a:1}, null, new Number(3))",
        "{\n   \"a\": 1\n}",
    ),
    (
        "JSON.stringify({a:1}, null, new String('\\t'))",
        "{\n\t\"a\": 1\n}",
    ),
    ("JSON.stringify({a:1}, null, true)", "{\"a\":1}"),
    (
        "JSON.stringify({\"a:{\":\"[1,2]\"}, null, 1)",
        "{\n \"a:{\": \"[1,2]\"\n}",
    ),
    (
        "JSON.stringify({a:\"x\\\"y,{z\"}, null, 1)",
        "{\n \"a\": \"x\\\"y,{z\"\n}",
    ),
    ("JSON.stringify(3, null, 4)", "3"),
    (
        "JSON.stringify({a:[{}]}, ['a'], 1)",
        "{\n \"a\": [\n  {}\n ]\n}",
    ),
    (
        "JSON.stringify({a:1,b:2}, function (k, v) { return k === 'b' ? [v] : v }, '\\t')",
        "{\n\t\"a\": 1,\n\t\"b\": [\n\t\t2\n\t]\n}",
    ),
    ("JSON.stringify(new String('a'))", "\"a\""),
    (
        "JSON.stringify([new Number(3), new Boolean(false), new String('s')])",
        "[3,false,\"s\"]",
    ),
    (
        "JSON.stringify({n: new Number(-0), b: new Boolean(true)})",
        "{\"n\":0,\"b\":true}",
    ),
    ("String(JSON.stringify())", "undefined"),
    (
        "JSON.stringify({a:1}, undefined, undefined, 7)",
        "{\"a\":1}",
    ),
    (
        "JSON.stringify({a:[1]}, undefined, 1)",
        "{\n \"a\": [\n  1\n ]\n}",
    ),
];

/// Each [`JSON_CASES`] entry yields its expected string through the inline
/// sealed `JSON.x` call, through a guarded call on an escaping `JSON`, and
/// through a call on a `JSON` value.
#[test]
fn json_reviver_replacer_space_in_every_call_shape() {
    for (src, expected) in JSON_CASES {
        assert_string(src, expected);
        assert_string(&format!("typeof JSON; {src}"), expected);
        let via_value = src.replace("JSON.", "J.");
        assert_string(&format!("const J = JSON; {via_value}"), expected);
    }
}

/// Wrap `body` so the script yields `"<error name>"` for the error it throws
/// (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name }} r")
}

/// `src` written against namespace `ns` in each call shape: inline, guarded
/// (an escaping `ns`), and through a `ns` value.
fn shapes(ns: &str, src: &str) -> [String; 3] {
    [
        src.to_owned(),
        format!("typeof {ns}; {src}"),
        format!("const N = {ns}; {}", src.replace(&format!("{ns}."), "N.")),
    ]
}

#[test]
fn json_stringify_cycle_free_reentrancy() {
    // A replacer that itself stringifies runs a nested, independent
    // serialization (its own replacer state and indentation).
    assert_string(
        "JSON.stringify({a:{b:1}}, function (k, v) { return k === 'a' ? JSON.stringify(v, null, 1) : v }, 2)",
        "{\n  \"a\": \"{\\n \\\"b\\\": 1\\n}\"\n}",
    );
}

#[test]
fn json_reviver_exception_propagates() {
    assert_string(
        &caught("JSON.parse('[1]', function () { throw new RangeError('x') })"),
        "RangeError",
    );
    assert_string(
        &caught("JSON.stringify([1], function () { throw new RangeError('x') })"),
        "RangeError",
    );
}

#[test]
fn math_arity_follows_js() {
    for (src, expected) in [
        ("Math.floor(1.5, 2)", 1.0),
        ("Math.sqrt(4, 9)", 2.0),
        ("Math.max()", f64::NEG_INFINITY),
        ("Math.min()", f64::INFINITY),
        ("Math.pow(2, 3, 4)", 8.0),
    ] {
        for program in shapes("Math", src) {
            assert_number(&program, expected);
        }
    }
    for src in ["Math.floor()", "Math.pow(2)", "Math.abs()"] {
        for program in shapes("Math", src) {
            assert_nan(&program);
        }
    }
}

#[test]
fn extra_arguments_are_evaluated_in_order() {
    let prelude = "let s = ''; const t = (c, v) => { s += c; return v };";
    for program in shapes("Math", "Math.floor(t('a', 1.5), t('b', 2)) + s") {
        assert_string(&format!("{prelude} {program}"), "1ab");
    }
    for program in shapes("Math", "Math.pow(t('a', 2), t('b', 3), t('c', 0)) + s") {
        assert_string(&format!("{prelude} {program}"), "8abc");
    }
    assert_string(
        &format!("{prelude} Number(t('a', '1'), t('b', 2)) + s"),
        "1ab",
    );
    assert_string(&format!("{prelude} Date.now(t('a', 1)); s"), "a");
    assert_string(
        &format!("{prelude} JSON.parse(t('a', '1'), t('b', 2), t('c', 3)) + s"),
        "1abc",
    );
}

#[test]
fn object_statics_arity_follows_js() {
    for src in [
        "Object.keys()",
        "Object.values(null)",
        "Object.entries(undefined)",
    ] {
        for program in shapes("Object", src) {
            assert_string(&caught(&program), "TypeError");
        }
    }
    for program in shapes("Object", "Object.keys({ a: 1, b: 2 }, 5).join()") {
        assert_string(&program, "a,b");
    }
    assert_uncaught(
        "Object.keys()",
        "TypeError: Cannot convert undefined or null to object",
    );
}

#[test]
fn json_promise_date_console_arity_follows_js() {
    for program in shapes("JSON", "typeof JSON.stringify()") {
        assert_string(&program, "undefined");
    }
    for program in shapes("JSON", "JSON.parse()") {
        assert_string(&caught(&program), "SyntaxError");
    }
    for program in shapes("JSON", "JSON.parse('[1]', undefined, 3).length") {
        assert_number(&program, 1.0);
    }
    for program in shapes("Date", "typeof Date.now(1, 2)") {
        assert_string(&program, "number");
    }
    for program in shapes("Promise", "Promise.resolve(1, 2) instanceof Promise") {
        assert_bool(&program, true);
    }
    for program in shapes("console", "console.log()") {
        assert_output(&program, "\n");
    }
}

#[test]
fn conversion_arity_follows_js() {
    for (src, expected) in [
        ("String()", ""),
        ("String(1, 2)", "1"),
        ("typeof Number()", "number"),
        ("String(Number())", "0"),
        ("String(Number('3', 4))", "3"),
        ("String(Boolean())", "false"),
        ("String(Boolean(0, 1))", "false"),
        ("String(parseInt('11', 2, 9))", "3"),
        ("String(parseFloat('1.5', 2))", "1.5"),
    ] {
        assert_string(src, expected);
    }
    assert_nan("parseInt()");
    assert_nan("parseFloat()");
}

/// A variadic builtin called through a value sees every argument, beyond the
/// uniform ABI's `K` positional slots.
#[test]
fn value_path_builtins_see_all_arguments() {
    let xs = "const xs = []; for (let i = 0; i < 20; i++) xs.push(i);";
    assert_number(&format!("{xs} const f = Math.max; f(...xs)"), 19.0);
    assert_number(
        &format!("{xs} const f = Math.min; f(...xs.map(x => 20 - x))"),
        1.0,
    );
    assert_number("const f = Math.max; f(1, 2, 3, 4, 5, 6, 7, 8, 9)", 9.0);
    assert_number("const p = Math.pow; p(...[2, 10])", 1024.0);
    assert_output(
        &format!("{xs} const log = console.log; log(...xs)"),
        "0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19\n",
    );
    assert_string(
        "const s = JSON.stringify; s(...[{ a: [1] }, null, 1])",
        "{\n \"a\": [\n  1\n ]\n}",
    );
}

#[test]
fn spread_calls_to_statics() {
    let xs = "const xs = [3, 9, 4];";
    for program in shapes(
        "Math",
        "Math.max(...xs) + Math.min(...xs) + Math.floor(...[1.5, 7])",
    ) {
        assert_number(&format!("{xs} {program}"), 13.0);
    }
    for program in shapes("console", "console.log(...xs)") {
        assert_output(&format!("{xs} {program}"), "3 9 4\n");
    }
    for program in shapes("Object", "Object.keys(...[{ a: 1, b: 2 }]).join()") {
        assert_string(&program, "a,b");
    }
    for program in shapes("JSON", "JSON.stringify(...[{ a: 1 }, ['a'], 1])") {
        assert_string(&program, "{\n \"a\": 1\n}");
    }
    for program in shapes("Promise", "Promise.resolve(...[1]) instanceof Promise") {
        assert_bool(&program, true);
    }
    assert_string("String(...[5, 6])", "5");
    assert_number("Number(...['7'])", 7.0);
}

#[test]
fn function_constructor_is_a_type_error() {
    assert_uncaught(
        "new Function('a', 'return a')",
        "TypeError: the Function constructor is not supported",
    );
    assert_string(&caught("Function('return 1')"), "TypeError");
}

#[test]
fn in_on_a_primitive_is_a_type_error() {
    for src in [
        "'a' in 'abc'",
        "'x' in 1",
        "'x' in undefined",
        "'x' in true",
    ] {
        assert_string(&caught(src), "TypeError");
    }
    assert_bool("'length' in new String('ab')", true);
}

#[test]
fn repeat_past_the_string_length_limit_is_a_range_error() {
    assert_uncaught("'x'.repeat(1e10)", "RangeError: Invalid string length");
    assert_string(&caught("const n = 2 ** 31; 'ab'.repeat(n)"), "RangeError");
    assert_number("'ab'.repeat(3).length", 6.0);
}

#[test]
fn math_applies_to_number_to_every_argument() {
    let prelude = "const v = (n) => ({ valueOf() { return n } });";
    for (src, expected) in [
        ("Math.floor(v(2.5))", 2.0),
        ("Math.ceil('1.5')", 2.0),
        ("Math.abs(v(-3))", 3.0),
        ("Math.round(v(2.5))", 3.0),
        ("Math.trunc(true)", 1.0),
        ("Math.sqrt(v(16))", 4.0),
        ("Math.pow(v(2), v(5))", 32.0),
        ("Math.max(v(3), v(10))", 10.0),
        ("Math.min(v(3), '1', 7)", 1.0),
        ("Math.max('3', '10')", 10.0),
        ("Math.max(null, -1)", 0.0),
        (
            "Math.min(v(4), v(2), v(8), v(6), v(5), v(9), v(7), v(3))",
            2.0,
        ),
        ("Math.max(...['3', '10'])", 10.0),
        ("Math.min(...[v(5), '2', 9])", 2.0),
        ("Math.floor({ toString() { return '7.5' } })", 7.0),
    ] {
        for program in shapes("Math", src) {
            assert_number(&format!("{prelude} {program}"), expected);
        }
    }
    for src in [
        "Math.max(1, undefined)",
        "Math.floor('x')",
        "Math.min({}, 1)",
    ] {
        for program in shapes("Math", src) {
            assert_nan(&program);
        }
    }
    // Past the uniform ABI's positional slots, through a value.
    let many = (1..=20)
        .map(|i| format!("'{i}'"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_number(&format!("const m = Math.max; m({many})"), 20.0);
    assert_number(
        &format!("{prelude} const m = Math.min; m({many}, v(0))"),
        0.0,
    );
}

#[test]
fn math_converts_after_evaluating_every_argument() {
    // Every argument is evaluated before the first `ToNumber`, which then run
    // left to right; a throwing `valueOf` stops the later conversions.
    let prelude = "let s = ''; const v = (c) => ({ valueOf() { s += c; return 1 } }); const t = (c, x) => { s += c; return x };";
    for src in [
        "Math.max(v('a'), t('b', v('c'))); s",
        "Math.min(t('b', v('a')), v('c')); s",
    ] {
        for program in shapes("Math", src) {
            assert_string(&format!("{prelude} {program}"), "bac");
        }
    }
    for program in shapes(
        "Math",
        "let r = ''; try { Math.max(v('a'), { valueOf() { throw new RangeError('x') } }, v('b')) } catch (e) { r = e.name } r + s",
    ) {
        assert_string(&format!("{prelude} {program}"), "RangeErrora");
    }
    for program in shapes(
        "Math",
        "let r = ''; for (let i = 0; i < 2; i++) { try { Math.min(...[v('a'), { valueOf() { throw new RangeError('x') } }]) } catch (e) { r += e.name } } r + s",
    ) {
        assert_string(&format!("{prelude} {program}"), "RangeErrorRangeErroraa");
    }
}

#[test]
fn number_statics_do_not_coerce() {
    for (src, expected) in [
        ("Number.isInteger(5)", "true"),
        ("Number.isInteger(5.5)", "false"),
        ("Number.isInteger(-0)", "true"),
        ("Number.isInteger(Infinity)", "false"),
        ("Number.isInteger('5')", "false"),
        ("Number.isInteger(new Number(5))", "false"),
        ("Number.isInteger(2 ** 60)", "true"),
        ("Number.isSafeInteger(2 ** 53 - 1)", "true"),
        ("Number.isSafeInteger(2 ** 53)", "false"),
        ("Number.isSafeInteger(-(2 ** 53 - 1))", "true"),
        ("Number.isSafeInteger(1.5)", "false"),
        ("Number.isSafeInteger(NaN)", "false"),
        ("Number.isFinite(1)", "true"),
        ("Number.isFinite('1')", "false"),
        ("Number.isFinite(Infinity)", "false"),
        ("Number.isNaN(NaN)", "true"),
        ("Number.isNaN('NaN')", "false"),
        ("Number.isNaN(undefined)", "false"),
        ("Number.isNaN()", "false"),
        ("Number.isInteger.length + Number.isNaN.length", "2"),
        ("Number.isSafeInteger.name", "isSafeInteger"),
        ("const f = Number.isFinite; f(3)", "true"),
    ] {
        assert_eval(src, expected);
    }
}

#[test]
fn number_parse_functions_are_the_globals() {
    for (src, expected) in [
        ("Number.parseFloat === parseFloat", "true"),
        ("Number.parseInt === parseInt", "true"),
        ("typeof globalThis.parseFloat", "function"),
        ("parseFloat.name + parseFloat.length", "parseFloat1"),
        ("parseInt.name + parseInt.length", "parseInt2"),
        ("Number.parseFloat('3.5abc')", "3.5"),
        ("Number.parseInt('ff', 16)", "255"),
        ("const p = parseInt; p('12px')", "12"),
        (
            "Number.parseFloat({ toString() { return '-1e3' } })",
            "-1000",
        ),
        ("Number.parseInt(' 0x1A ')", "26"),
    ] {
        assert_eval(src, expected);
    }
}

#[test]
fn number_constants() {
    for (src, expected) in [
        ("Number.EPSILON === 2 ** -52", "true"),
        ("Number.MAX_SAFE_INTEGER", "9007199254740991"),
        ("Number.MIN_SAFE_INTEGER", "-9007199254740991"),
        ("Number.MAX_VALUE", "1.7976931348623157e+308"),
        ("Number.MIN_VALUE", "5e-324"),
        ("Number.NaN", "NaN"),
        ("Number.POSITIVE_INFINITY", "Infinity"),
        ("Number.NEGATIVE_INFINITY", "-Infinity"),
        (
            "try { Number.MAX_VALUE = 1 } catch (e) {} Number.MAX_VALUE",
            "1.7976931348623157e+308",
        ),
        (
            "try { delete Number.EPSILON } catch (e) {} Number.EPSILON > 0",
            "true",
        ),
        ("Object.keys(Number).length", "0"),
        ("try { Math.PI = 3 } catch (e) {} Math.PI === 3", "false"),
    ] {
        assert_eval(src, expected);
    }
}
