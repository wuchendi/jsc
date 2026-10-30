//! Integration tests: the `String`/`Number`/`Boolean` prototypes (DESIGN.md §6.4)
//! and wrapper objects (DESIGN.md §5.2 classes 3..5, §6.3): `new String(x)`,
//! `Object(primitive)`, `this` coercion of the prototype methods, and the
//! String wrapper's exotic own `length` / index properties.

mod common;
use common::{assert_bool, assert_eval, assert_number, assert_output, assert_string};

/// Wrap `body` so the script yields `"<error name>|<message>"` for the error
/// it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name+\"|\"+e.message }} r")
}

// --- prototypes ----------------------------------------------------------------

#[test]
fn prototype_methods_are_function_values() {
    for name in [
        "indexOf",
        "includes",
        "slice",
        "toUpperCase",
        "toLowerCase",
        "charAt",
        "trim",
        "trimStart",
        "trimEnd",
        "repeat",
        "startsWith",
        "endsWith",
        "padStart",
        "padEnd",
        "substring",
        "split",
        "toString",
        "valueOf",
    ] {
        assert_string(&format!("typeof String.prototype.{name}"), "function");
        assert_bool(&format!("\"a\".{name} === String.prototype.{name}"), true);
        assert_string(&format!("String.prototype.{name}.name"), name);
    }
    for name in ["toString", "valueOf"] {
        assert_bool(&format!("(1).{name} === Number.prototype.{name}"), true);
        assert_bool(&format!("true.{name} === Boolean.prototype.{name}"), true);
    }
}

#[test]
fn prototype_method_lengths() {
    for (name, length) in [
        ("indexOf", 1.0),
        ("slice", 2.0),
        ("toUpperCase", 0.0),
        ("charAt", 1.0),
        ("repeat", 1.0),
        ("padStart", 1.0),
        ("substring", 2.0),
        ("split", 2.0),
        ("toString", 0.0),
    ] {
        assert_number(&format!("String.prototype.{name}.length"), length);
    }
    assert_number("Number.prototype.toString.length", 1.0);
    assert_number("Number.prototype.valueOf.length", 0.0);
    assert_number("Boolean.prototype.toString.length", 0.0);
}

#[test]
fn primitive_property_reads_resolve_through_prototypes() {
    assert_eval("\"abc\".foo", "undefined");
    assert_bool("(1).toString === Number.prototype.toString", true);
    assert_bool("true.valueOf()", true);
    assert_number("(5).valueOf()", 5.0);
    assert_string("(5).toString()", "5");
    assert_string("false.toString()", "false");
    assert_string("\"abc\".toString()", "abc");
    assert_string("\"abc\".valueOf()", "abc");
    assert_bool("\"abc\".constructor === String", true);
    assert_bool("(1).constructor === Number", true);
}

#[test]
fn methods_on_primitives_receive_the_primitive_this() {
    assert_string(
        "String.prototype.kind = function () { return typeof this }; \"x\".kind()",
        "string",
    );
    assert_string(
        "Number.prototype.twice = function () { return this * 2 }; String((4).twice())",
        "8",
    );
    assert_string(
        "Boolean.prototype.neg = function () { return !this }; String(true.neg())",
        "false",
    );
}

#[test]
fn number_to_string_radix() {
    assert_string("(255).toString(16)", "ff");
    assert_string("(255).toString(2)", "11111111");
    assert_string("(-255).toString(36)", "-73");
    assert_string("(0.5).toString(2)", "0.1");
    assert_string("(10).toString(10)", "10");
    assert_string("(10).toString()", "10");
    assert_string("(10).toString(undefined)", "10");
    assert_string("NaN.toString(16)", "NaN");
    assert_string("(-Infinity).toString(2)", "-Infinity");
    assert_string(
        &caught("(1).toString(1)"),
        "RangeError|toString() radix must be between 2 and 36",
    );
    assert_string(
        &caught("(1).toString(37)"),
        "RangeError|toString() radix must be between 2 and 36",
    );
}

#[test]
fn value_of_and_to_string_reject_wrong_receivers() {
    assert_string(
        &caught("String.prototype.valueOf.call(1)"),
        "TypeError|String.prototype.valueOf requires that 'this' be a String",
    );
    assert_string(
        &caught("String.prototype.toString.call({})"),
        "TypeError|String.prototype.toString requires that 'this' be a String",
    );
    assert_string(
        &caught("Number.prototype.valueOf.call(\"1\")"),
        "TypeError|Number.prototype.valueOf requires that 'this' be a Number",
    );
    assert_string(
        &caught("Number.prototype.toString.call(true)"),
        "TypeError|Number.prototype.toString requires that 'this' be a Number",
    );
    assert_string(
        &caught("Boolean.prototype.valueOf.call(0)"),
        "TypeError|Boolean.prototype.valueOf requires that 'this' be a Boolean",
    );
    assert_string(
        &caught("Boolean.prototype.toString.call(new Number(1))"),
        "TypeError|Boolean.prototype.toString requires that 'this' be a Boolean",
    );
}

#[test]
fn generic_string_methods_coerce_this() {
    assert_string("String.prototype.toUpperCase.call(12)", "12");
    assert_string("String.prototype.slice.call(12345, 1, 3)", "23");
    assert_number("String.prototype.indexOf.call(true, \"u\")", 2.0);
    assert_string(
        "String.prototype.trim.call({ toString() { return \"  o \" } })",
        "o",
    );
    assert_string(
        &caught("String.prototype.trim.call(null)"),
        "TypeError|String.prototype.trim called on null or undefined",
    );
    assert_string(
        &caught("String.prototype.indexOf.call(undefined, \"a\")"),
        "TypeError|String.prototype.indexOf called on null or undefined",
    );
}

// --- wrappers --------------------------------------------------------------------

#[test]
fn wrapper_construction_and_typeof() {
    for (ctor, arg) in [("String", "\"ab\""), ("Number", "1"), ("Boolean", "true")] {
        assert_string(&format!("typeof new {ctor}({arg})"), "object");
        assert_bool(&format!("new {ctor}({arg}) instanceof {ctor}"), true);
        assert_bool(&format!("new {ctor}({arg}) instanceof Object"), true);
        assert_string(&format!("typeof {ctor}({arg})"), &ctor.to_lowercase());
    }
    assert_string("typeof Object(\"s\")", "object");
    assert_bool("Object(\"s\") instanceof String", true);
    assert_bool("Object(1) instanceof Number", true);
    assert_bool("Object(false) instanceof Boolean", true);
    assert_bool("let o = {}; Object(o) === o", true);
    assert_string("typeof Object(null)", "object");
    assert_string("typeof Object(undefined)", "object");
    assert_string("typeof Object()", "object");
}

#[test]
fn wrapper_default_values() {
    assert_string("new String().valueOf()", "");
    assert_number("new Number().valueOf()", 0.0);
    assert_bool("new Boolean().valueOf()", false);
    assert_string("new String(12).valueOf()", "12");
    assert_number("new Number(\"7\").valueOf()", 7.0);
    assert_bool("new Boolean(\"\").valueOf()", false);
}

#[test]
fn wrappers_convert_through_value_of() {
    assert_number("new Number(2) + 3", 5.0);
    assert_string("new String(\"a\") + \"b\"", "ab");
    assert_string("`${new Number(4)}`", "4");
    assert_number("+new String(\"8\")", 8.0);
    assert_bool("new Number(1) == 1", true);
    assert_bool("new String(\"a\") == \"a\"", true);
    assert_bool("new String(\"a\") === \"a\"", false);
    assert_string("String(new Boolean(false))", "false");
    assert_number("Number(new Number(9))", 9.0);
}

#[test]
fn wrappers_are_truthy() {
    assert_string("new Boolean(false) ? \"yes\" : \"no\"", "yes");
    assert_string("new Number(0) ? \"yes\" : \"no\"", "yes");
    assert_string("new String(\"\") ? \"yes\" : \"no\"", "yes");
    assert_bool("!new Boolean(false)", false);
}

#[test]
fn wrapper_methods_and_own_properties() {
    assert_string("new String(\"abc\").toUpperCase()", "ABC");
    assert_string("new Number(255).toString(16)", "ff");
    assert_string("new Boolean(true).toString()", "true");
    assert_number("let w = new Number(1); w.x = 2; w.x + w", 3.0);
    assert_string(
        "Object.prototype.toString.call(new String(\"a\"))",
        "[object String]",
    );
    assert_string(
        "Object.prototype.toString.call(Object(1))",
        "[object Number]",
    );
    assert_string(
        "Object.prototype.toString.call(Object(true))",
        "[object Boolean]",
    );
}

#[test]
fn string_wrapper_exotic_own_properties() {
    assert_number("new String(\"abc\").length", 3.0);
    assert_string("new String(\"abc\")[1]", "b");
    assert_eval("new String(\"abc\")[3]", "undefined");
    assert_bool("0 in new String(\"ab\")", true);
    assert_bool("\"length\" in new String(\"ab\")", true);
    assert_bool("2 in new String(\"ab\")", false);
    assert_bool("new String(\"ab\").hasOwnProperty(\"1\")", true);
    assert_bool("new String(\"ab\").hasOwnProperty(\"length\")", true);
    assert_string("Object.keys(new String(\"ab\")).join()", "0,1");
    assert_string(
        "let w = new String(\"ab\"); w.x = 1; Object.keys(w).join()",
        "0,1,x",
    );
    assert_string(
        "let w = new String(\"ab\"); w.x = 1; Object.values(w).join()",
        "a,b,1",
    );
    assert_string(
        "let ks = []; for (let k in new String(\"xyz\")) ks.push(k); ks.join()",
        "0,1,2",
    );
    // `length` and the indices count code units.
    assert_number("new String(\"é😀\").length", 3.0);
    assert_bool("new String(\"é😀\")[0] === \"é\"", true);
    assert_bool("new String(\"é😀\")[2] === \"\\uDE00\"", true);
    assert_eval("new String(\"é😀\")[3]", "undefined");
    assert_bool("2 in new String(\"é😀\")", true);
    assert_string("Object.keys(new String(\"é😀\")).join()", "0,1,2");
    assert_string(
        "let ks = []; for (let k in new String(\"é😀\")) ks.push(k); ks.join()",
        "0,1,2",
    );
}

#[test]
fn string_wrapper_exotic_properties_are_read_only() {
    assert_string(
        &caught("let w = new String(\"ab\"); w[0] = \"z\""),
        "TypeError|Cannot assign to read only property '0' of object",
    );
    assert_string(
        &caught("let w = new String(\"ab\"); w.length = 5"),
        "TypeError|Cannot assign to read only property 'length' of object",
    );
    assert_string(
        &caught("let w = new String(\"ab\"); delete w[0]"),
        "TypeError|Cannot delete property '0' of object",
    );
    assert_string("let w = new String(\"ab\"); w[5] = \"q\"; w[5]", "q");
}

#[test]
fn array_methods_to_object_wrap_primitives() {
    assert_number("Array.prototype.indexOf.call(\"abc\", \"c\")", 2.0);
    assert_string("Array.prototype.join.call(\"abc\", \"-\")", "a-b-c");
    assert_number("Array.prototype.map.call(\"ab\", (c) => c).length", 2.0);
    assert_number("Array.prototype.slice.call(5).length", 0.0);
}

#[test]
fn wrapper_rendering() {
    assert_output(
        "console.log(new String(\"abc\"), new Number(1), new Boolean(true))",
        "[String: 'abc'] [Number: 1] [Boolean: true]\n",
    );
    assert_output("console.log([Object(\"x\")])", "[ [String: 'x'] ]\n");
    assert_eval("new Number(-0)", "[Number: -0]");
}

// --- Number.prototype formatting ---------------------------------------------

/// Wrap `body` so the script yields the name of the error it throws.
fn caught_name(body: &str) -> String {
    format!("let r=\"none\"; try {{ {body} }} catch(e) {{ r=e.name }} r")
}

#[test]
fn number_to_string_radix_and_receivers() {
    for (src, expected) in [
        ("(0.1).toString()", "0.1"),
        ("(1e21).toString()", "1e+21"),
        ("(123.456).toString(10)", "123.456"),
        ("(-0).toString(2)", "0"),
        ("(255).toString(16.9)", "ff"),
        ("(255).toString('16')", "ff"),
        ("(0.5).toString(36)", "0.i"),
        ("(-255.5).toString(16)", "-ff.8"),
        ("(1e21).toString(36)", "5v1j4f4ds7c000"),
        ("new Number(10).toString(2)", "1010"),
        ("Number.prototype.toString.call(Object(7), 8)", "7"),
        ("Number.prototype.toString()", "0"),
        ("Number.prototype.toString.length", "1"),
        ("Number.prototype.toLocaleString.name", "toLocaleString"),
        ("Number.prototype.toLocaleString.length", "0"),
        ("(12.5).toLocaleString()", "12.5"),
    ] {
        assert_eval(src, expected);
    }
    for (src, expected) in [
        ("(1).toString(0)", "RangeError"),
        ("(1).toString(NaN)", "RangeError"),
        ("(1).toString(Infinity)", "RangeError"),
        ("Number.prototype.toString.call('1')", "TypeError"),
        ("Number.prototype.toString.call({})", "TypeError"),
        (
            "Number.prototype.toLocaleString.call(Object('1'))",
            "TypeError",
        ),
    ] {
        assert_string(&caught_name(src), expected);
    }
}

#[test]
fn number_to_fixed() {
    for (src, expected) in [
        ("(1.005).toFixed(2)", "1.00"),
        ("(1.45).toFixed(1)", "1.4"),
        ("(2.5).toFixed(0)", "3"),
        ("(0.5).toFixed(0)", "1"),
        ("(-1.5).toFixed(0)", "-2"),
        ("(1.25).toFixed(1)", "1.3"),
        ("(123.456).toFixed(2)", "123.46"),
        ("(0.000001).toFixed(3)", "0.000"),
        ("(-0.0000001).toFixed(2)", "-0.00"),
        ("(-0).toFixed(2)", "0.00"),
        ("(42).toFixed()", "42"),
        ("(42).toFixed(NaN)", "42"),
        ("(1.1).toFixed(20)", "1.10000000000000008882"),
        ("(1e21).toFixed(2)", "1e+21"),
        ("(1e20).toFixed(2)", "100000000000000000000.00"),
        ("NaN.toFixed(2)", "NaN"),
        ("(-Infinity).toFixed(1)", "-Infinity"),
        ("(0).toFixed(100).length", "102"),
        ("Number.prototype.toFixed.length", "1"),
        ("new Number(3.14159).toFixed(3)", "3.142"),
    ] {
        assert_eval(src, expected);
    }
    for (src, expected) in [
        ("(1).toFixed(101)", "RangeError"),
        ("(1).toFixed(-1)", "RangeError"),
        ("(1).toFixed(Infinity)", "RangeError"),
        ("NaN.toFixed(Infinity)", "RangeError"),
        ("Number.prototype.toFixed.call('1', 1)", "TypeError"),
    ] {
        assert_string(&caught_name(src), expected);
    }
}

#[test]
fn number_to_exponential() {
    for (src, expected) in [
        ("(123456).toExponential(2)", "1.23e+5"),
        ("(123456).toExponential()", "1.23456e+5"),
        ("(0.00015).toExponential(1)", "1.5e-4"),
        ("(1.25).toExponential(1)", "1.3e+0"),
        ("(-5).toExponential(3)", "-5.000e+0"),
        ("(0).toExponential()", "0e+0"),
        ("(0).toExponential(2)", "0.00e+0"),
        ("(-0).toExponential(1)", "0.0e+0"),
        ("(1e21).toExponential()", "1e+21"),
        ("(5e-324).toExponential()", "5e-324"),
        ("NaN.toExponential(1000)", "NaN"),
        ("Infinity.toExponential(-5)", "Infinity"),
        ("Number.prototype.toExponential.length", "1"),
    ] {
        assert_eval(src, expected);
    }
    for (src, expected) in [
        ("(1).toExponential(101)", "RangeError"),
        ("(1).toExponential(-1)", "RangeError"),
        ("Number.prototype.toExponential.call(true)", "TypeError"),
    ] {
        assert_string(&caught_name(src), expected);
    }
}

#[test]
fn number_to_precision() {
    for (src, expected) in [
        ("(123.456).toPrecision(4)", "123.5"),
        ("(123.456).toPrecision(2)", "1.2e+2"),
        ("(0.000123).toPrecision(2)", "0.00012"),
        ("(0.0000001).toPrecision(1)", "1e-7"),
        ("(123456).toPrecision(6)", "123456"),
        ("(123456).toPrecision(3)", "1.23e+5"),
        ("(1.25).toPrecision(2)", "1.3"),
        ("(0).toPrecision(3)", "0.00"),
        ("(-1.5).toPrecision(1)", "-2"),
        ("(25).toPrecision(1)", "3e+1"),
        ("(1.5).toPrecision()", "1.5"),
        ("NaN.toPrecision(1000)", "NaN"),
        ("Number.prototype.toPrecision.length", "1"),
    ] {
        assert_eval(src, expected);
    }
    for (src, expected) in [
        ("(1).toPrecision(0)", "RangeError"),
        ("(1).toPrecision(101)", "RangeError"),
        ("Number.prototype.toPrecision.call(null, 1)", "TypeError"),
    ] {
        assert_string(&caught_name(src), expected);
    }
}
