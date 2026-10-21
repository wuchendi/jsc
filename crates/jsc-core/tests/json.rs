//! Integration tests: `JSON.stringify` `toJSON` (including
//! `Date.prototype.toJSON`) and cycle detection, through every call shape
//! (inline `JSON.x`, a guarded call on an escaping `JSON`, a call through a
//! value).

mod common;
use common::{assert_output, assert_string};

/// `src` written against `JSON` in each call shape: inline, guarded (an
/// escaping `JSON`), and through a `JSON` value.
fn shapes(src: &str) -> [String; 3] {
    [
        src.to_owned(),
        format!("typeof JSON; {src}"),
        format!("const J = JSON; {}", src.replace("JSON.", "J.")),
    ]
}

/// Wrap `body` so the script yields `"<error name>: <message>"` for the error
/// it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!(
        "let r = 'none'; try {{ {body} }} catch (e) {{ r = (e instanceof TypeError) + ' ' + e.name + ': ' + e.message }} r"
    )
}

const CYCLE: &str = "true TypeError: Converting circular structure to JSON";

#[test]
fn to_json_is_called_with_its_key() {
    for (src, expected) in [
        ("JSON.stringify({ toJSON() { return 1 } })", "1"),
        ("JSON.stringify({ toJSON(k) { return k === '' } })", "true"),
        ("JSON.stringify({ v: 3, toJSON() { return this.v } })", "3"),
        (
            "JSON.stringify({ a: { toJSON(k) { return 'k=' + k } }, b: [{ toJSON(k) { return typeof k + k } }] })",
            "{\"a\":\"k=a\",\"b\":[\"string0\"]}",
        ),
        (
            "JSON.stringify({ a: { toJSON() {} }, b: 1, c: [{ toJSON() {} }] })",
            "{\"b\":1,\"c\":[null]}",
        ),
        ("String(JSON.stringify({ toJSON() {} }))", "undefined"),
        (
            "(() => { const a = [1]; a.toJSON = function () { return 'arr' }; return JSON.stringify(a) })()",
            "\"arr\"",
        ),
        (
            "JSON.stringify({ a: { toJSON() { return { b: { toJSON() { return [2] } } } } } })",
            "{\"a\":{\"b\":[2]}}",
        ),
        (
            "JSON.stringify({ toJSON: 5, a: 1 })",
            "{\"toJSON\":5,\"a\":1}",
        ),
        (
            "(() => { Object.prototype.toJSON = function () { return 'p' }; return JSON.stringify([1, {}]) })()",
            "\"p\"",
        ),
        (
            "JSON.stringify({ a: { toJSON() { return 5 } } }, function (k, v) { return k === 'a' ? v * 2 : v })",
            "{\"a\":10}",
        ),
        (
            "JSON.stringify([{ toJSON(k) { return k } }], function (k, v) { return k === '0' ? v + '!' : v })",
            "[\"0!\"]",
        ),
        (
            "JSON.stringify({ a: { toJSON() { return { x: 1 } } } }, null, 1)",
            "{\n \"a\": {\n  \"x\": 1\n }\n}",
        ),
    ] {
        for program in shapes(src) {
            assert_string(&program, expected);
        }
    }
    for program in shapes("JSON.stringify({ toJSON() { throw new RangeError('x') } })") {
        assert_string(
            &format!("let r = 'none'; try {{ {program} }} catch (e) {{ r = e.name }} r"),
            "RangeError",
        );
    }
}

#[test]
fn date_serializes_through_date_prototype_to_json() {
    for (src, expected) in [
        (
            "JSON.stringify({ d: new Date(0) })",
            "{\"d\":\"1970-01-01T00:00:00.000Z\"}",
        ),
        ("JSON.stringify(new Date(NaN))", "null"),
        (
            "JSON.stringify([new Date(86400000)])",
            "[\"1970-01-02T00:00:00.000Z\"]",
        ),
    ] {
        for program in shapes(src) {
            assert_string(&program, expected);
        }
    }
    for (src, expected) in [
        (
            "Date.prototype.toJSON.name + Date.prototype.toJSON.length",
            "toJSON1",
        ),
        (
            "Date.prototype.toJSON.call({ toISOString() { return 'iso' } })",
            "iso",
        ),
        (
            "String(Date.prototype.toJSON.call({ valueOf() { return Infinity }, toISOString() { return 'no' } }))",
            "null",
        ),
        (
            "(() => { const d = new Date(0); d.toISOString = function () { return 'own' }; return JSON.stringify(d) })()",
            "\"own\"",
        ),
        (
            "(() => { let r = 'none'; try { Date.prototype.toJSON.call({ toISOString: 1 }) } catch (e) { r = e.name } return r })()",
            "TypeError",
        ),
    ] {
        assert_string(src, expected);
    }
}

#[test]
fn cycles_throw_a_catchable_type_error() {
    for body in [
        "const o = {}; o.o = o; JSON.stringify(o)",
        "const o = { l: [] }; o.l.push({ x: o }); JSON.stringify(o)",
        "const a = [1]; a.push([a]); JSON.stringify(a)",
        "const o = { a: 1 }; o.self = o; JSON.stringify(o, function (k, v) { return v }, 2)",
        "const o = { a: 1 }; o.self = o; JSON.stringify(o, ['a', 'self'])",
        "const o = {}; o.o = { toJSON() { return o } }; JSON.stringify(o)",
    ] {
        for program in shapes(body) {
            assert_string(&caught(&program), CYCLE);
        }
    }
}

#[test]
fn shared_references_are_not_cycles() {
    for (src, expected) in [
        (
            "(() => { const x = { n: 1 }; return JSON.stringify({ a: x, b: x }) })()",
            "{\"a\":{\"n\":1},\"b\":{\"n\":1}}",
        ),
        (
            "(() => { const x = [1]; return JSON.stringify([x, [x], x]) })()",
            "[[1],[[1]],[1]]",
        ),
        (
            "(() => { const o = {}; o.o = o; try { JSON.stringify(o) } catch (e) {} return JSON.stringify({ a: [1] }) })()",
            "{\"a\":[1]}",
        ),
        (
            "JSON.stringify({ a: 1 }, function (k, v) { return k === 'a' ? JSON.stringify(this) : v })",
            "{\"a\":\"{\\\"a\\\":1}\"}",
        ),
        ("JSON.stringify([[[[1]]]])", "[[[[1]]]]"),
    ] {
        for program in shapes(src) {
            assert_string(&program, expected);
        }
    }
}

#[test]
fn task_repros() {
    assert_output("console.log(Math.max(...[\"3\", \"10\"]));", "10\n");
    assert_output(
        "console.log(Math.floor({ valueOf() { return 2.5; } }));",
        "2\n",
    );
    assert_output(
        "console.log(JSON.stringify({ toJSON() { return 1; } }));",
        "1\n",
    );
    assert_output(
        "console.log(JSON.stringify({ d: new Date(0) }));",
        "{\"d\":\"1970-01-01T00:00:00.000Z\"}\n",
    );
    assert_output(
        "const o = {}; o.o = o; try { JSON.stringify(o); } catch (e) { console.log(e instanceof TypeError, e.message); }",
        "true Converting circular structure to JSON\n",
    );
}
