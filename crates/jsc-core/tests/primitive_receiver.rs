//! Integration tests: property reads, writes, updates and method calls on
//! primitive (number/string/boolean) and nullish receivers. No path may treat a
//! non-heap jsval as a pointer: writes throw the strict-mode `TypeError`, reads
//! of an unknown property yield `undefined`.

mod common;
use common::{assert_eval, assert_str, assert_uncaught};

/// Wrap `body` so the script yields `"<instanceof TypeError>|<message>"` for the
/// error it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    format!(
        "let r=\"none\"; try {{ {body} }} catch(e) {{ r=(e instanceof TypeError)+\"|\"+e.message }} r"
    )
}

/// Assert `body` throws a `TypeError` with exactly `message`.
fn assert_type_error(body: &str, message: &str) {
    assert_str(&caught(body), &format!("true|{message}"));
}

/// The receivers under test: a declaration binding `v`, and its rendered
/// `typeof` / `String(v)` pair for the create-property message.
const PRIMITIVES: [(&str, &str, &str); 4] = [
    ("let v=5;", "number", "5"),
    ("let v=1.5;", "number", "1.5"),
    ("let v=\"abc\";", "string", "abc"),
    ("let v=true;", "boolean", "true"),
];

#[test]
fn primitive_static_write_throws() {
    for (decl, kind, text) in PRIMITIVES {
        assert_type_error(
            &format!("{decl} v.x=1;"),
            &format!("Cannot create property 'x' on {kind} '{text}'"),
        );
    }
}

#[test]
fn primitive_computed_write_throws() {
    for (decl, kind, text) in PRIMITIVES {
        assert_type_error(
            &format!("{decl} v[\"x\"]=1;"),
            &format!("Cannot create property 'x' on {kind} '{text}'"),
        );
        assert_type_error(
            &format!("{decl} let k=\"y\"; v[k]=1;"),
            &format!("Cannot create property 'y' on {kind} '{text}'"),
        );
        assert_type_error(
            &format!("{decl} v[0]=1;"),
            &format!("Cannot create property '0' on {kind} '{text}'"),
        );
    }
}

#[test]
fn primitive_compound_and_update_throw() {
    for (decl, kind, text) in PRIMITIVES {
        let msg = format!("Cannot create property 'x' on {kind} '{text}'");
        for op in ["v.x+=1;", "v.x-=1;", "v.x++;", "--v.x;", "v[\"x\"]*=2;"] {
            assert_type_error(&format!("{decl} {op}"), &msg);
        }
    }
}

#[test]
fn literal_primitive_write_throws() {
    assert_type_error("(5).x=1;", "Cannot create property 'x' on number '5'");
    assert_type_error("\"abc\".x=1;", "Cannot create property 'x' on string 'abc'");
    assert_type_error(
        "false.x=1;",
        "Cannot create property 'x' on boolean 'false'",
    );
}

#[test]
fn primitive_write_uncaught_message() {
    assert_uncaught(
        "let n=5; n.x=1",
        "TypeError: Cannot create property 'x' on number '5'",
    );
}

#[test]
fn primitive_unknown_reads_are_undefined() {
    for (decl, _, _) in PRIMITIVES {
        assert_eval(&format!("{decl} v.x"), "undefined");
        assert_eval(&format!("{decl} v[\"x\"]"), "undefined");
        assert_eval(&format!("{decl} let k=\"zz\"; v[k]"), "undefined");
    }
    assert_eval("let n=5; n[0]", "undefined");
    assert_eval("let b=true; b[0]", "undefined");
    assert_eval("let n=5; n.length", "undefined");
    assert_eval("let b=false; b.length", "undefined");
}

#[test]
fn primitive_unknown_method_call_throws() {
    for (decl, _, _) in PRIMITIVES {
        assert_type_error(&format!("{decl} v.foo();"), "v.foo is not a function");
        assert_type_error(
            &format!("{decl} let k=\"foo\"; v[k]();"),
            "expression is not a function",
        );
    }
}

#[test]
fn literal_primitive_method_calls_resolve_at_run_time() {
    assert_type_error("\"abc\".foo();", "expression is not a function");
    assert_type_error("(5).foo();", "expression is not a function");
    assert_type_error("true.foo();", "expression is not a function");
    assert_str("(5).toString()", "5");
    assert_str("true.toString()", "true");
    assert_str("\"abc\".valueOf()", "abc");
    assert_str("let n = 7; n.toString(2)", "111");
}

#[test]
fn nullish_receivers_throw() {
    for (decl, word) in [("let v=undefined;", "undefined"), ("let v=null;", "null")] {
        assert_type_error(
            &format!("{decl} v.x=1;"),
            &format!("Cannot set properties of {word} (setting 'x')"),
        );
        assert_type_error(
            &format!("{decl} v[\"x\"]=1;"),
            &format!("Cannot set properties of {word} (setting 'x')"),
        );
        assert_type_error(
            &format!("{decl} v.x;"),
            &format!("Cannot read properties of {word} (reading 'x')"),
        );
        assert_type_error(
            &format!("{decl} v.x+=1;"),
            &format!("Cannot read properties of {word} (reading 'x')"),
        );
        assert_type_error(
            &format!("{decl} v.x++;"),
            &format!("Cannot read properties of {word} (reading 'x')"),
        );
        assert_type_error(
            &format!("{decl} v.foo();"),
            &format!("Cannot read properties of {word} (reading 'foo')"),
        );
    }
}

#[test]
fn string_unknown_property_is_undefined() {
    assert_eval("\"abc\".foo === undefined", "true");
    assert_eval("\"abc\".foo", "undefined");
    assert_eval("let s=\"abc\"; s.foo", "undefined");
    assert_eval("let s=\"abc\"; let k=\"foo\"; s[k]", "undefined");
    assert_eval("\"abc\"[\"\"]", "undefined");
    assert_eval("\"abc\"[\"01\"]", "undefined");
    assert_eval("\"abc\"[1.5]", "undefined");
    assert_eval("\"abc\"[NaN]", "undefined");
}

#[test]
fn string_known_properties_still_work() {
    assert_eval("\"abc\".length", "3");
    assert_eval("\"abc\"[\"length\"]", "3");
    assert_str("\"abc\"[1]", "b");
    assert_str("\"abc\"[\"1\"]", "b");
    assert_str("let s=\"abc\"; let i=2; s[i]", "c");
    assert_eval("\"abc\"[5]", "undefined");
    assert_eval("\"abc\"[-1]", "undefined");
    assert_str("\"abc\".toUpperCase()", "ABC");
    assert_str("let s=\"abc\"; s.slice(1)", "bc");
}

#[test]
fn array_string_keys() {
    assert_eval("let a=[1,2]; a.foo", "undefined");
    assert_eval("let a=[1,2]; a[\"1\"]", "2");
    assert_eval("let a=[1,2]; a[\"length\"]", "2");
}
