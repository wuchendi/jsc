//! Integration tests: namespace members as ordinary runtime properties (G1c
//! deleted), the guarded fast paths of design 3.2 and the intrinsics dirty
//! bits of design 3.3.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string, assert_uncaught};

#[test]
fn unknown_namespace_members_are_runtime_reads() {
    assert_uncaught("Math.foo()", "TypeError: Math.foo is not a function");
    assert_bool(
        "let ok = false; try { Math.foo(); } catch (e) { ok = e instanceof TypeError && e.message === 'Math.foo is not a function'; } ok",
        true,
    );
    assert_bool("Math.foo === undefined", true);
    assert_uncaught("JSON.foo(1)", "TypeError: JSON.foo is not a function");
    assert_uncaught(
        "Promise.all([])",
        "TypeError: Promise.all is not a function",
    );
    assert_uncaught(
        "console.warn(1)",
        "TypeError: console.warn is not a function",
    );
    // Phase 3 statics are absent until then: a catchable TypeError.
    assert_bool(
        "let ok = false; try { Object.freeze({}); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
    // Inherited members of a namespace or constructor are reachable.
    assert_string("Math.toString()", "[object Object]");
    assert_bool("Object.hasOwnProperty('keys')", true);
}

#[test]
fn array_literal_receivers_dispatch_at_runtime() {
    assert_string("[1].toString()", "1");
    assert_string("[1, 2].toString()", "1,2");
    assert_uncaught("[1, 2].flat()", "TypeError: expression is not a function");
}

#[test]
fn namespace_overrides_are_observed() {
    assert_number("Math.max = () => 42; Math.max(1, 2)", 42.0);
    assert_number("const m = Math; m.max = () => 42; Math.max(1, 2)", 42.0);
    assert_number(
        "function f(n) { n.max = () => 42; } f(Math); Math.max(1, 2)",
        42.0,
    );
    assert_string(
        "JSON.stringify = () => 'mine'; JSON.stringify({ a: 1 })",
        "mine",
    );
    assert_number("Object.keys = () => 7; Object.keys({})", 7.0);
    assert_output(
        "const c = console; c.log = (x) => 0; console.log('hidden'); c.log === console.log",
        "",
    );
}

#[test]
fn escaping_namespaces_keep_builtin_semantics() {
    // The namespace escapes (or the program is open), so each call is guarded.
    assert_number("const m = Math; Math.max(1, 5, 3) + m.floor(1.5)", 6.0);
    assert_number("globalThis; Math.max(1, 5, 3)", 5.0);
    assert_string("const j = JSON; JSON.stringify([1, 2])", "[1,2]");
    assert_number("const o = Object; Object.keys({ a: 1, b: 2 }).length", 2.0);
    assert_output("const c = console; console.log('x', 1)", "x 1\n");
    // A write to another member keeps the builtin fast path correct.
    assert_number("const m = Math; m.extra = 1; Math.max(1, 2) + m.extra", 3.0);
    // A reassigned namespace is read from the global object.
    assert_number("Math = { max() { return 7; } }; Math.max(1, 2)", 7.0);
}

#[test]
fn prototype_writes_are_observed() {
    assert_number("Object.prototype.x = 1; ({}).x", 1.0);
    assert_number("Map.prototype.x = 2; new Map().x", 2.0);
    // A write to an unrelated member keeps the array fast paths correct.
    assert_number(
        "Array.prototype.foo = 1; const a = [1]; a.push(2); a.length + a.foo + a.pop()",
        5.0,
    );
}

#[test]
fn array_push_override_is_observed() {
    assert_string(
        r#"Array.prototype.push = function () { return "user"; }; [].push(1)"#,
        "user",
    );
    assert_number(
        "Array.prototype.pop = function () { return this.length + 40; }; [1, 2].pop()",
        42.0,
    );
    // An override receives the receiver and arguments.
    assert_number(
        "Array.prototype.push = function (x) { return this.length * 10 + x; }; [1, 2].push(3)",
        23.0,
    );
}

#[test]
fn computed_caller_key_throws() {
    assert_bool(
        r#"function f() {} let ok = false; try { f["cal" + "ler"]; } catch (e) { ok = e instanceof TypeError; } ok"#,
        true,
    );
    assert_bool(
        r#"function f() {} let k = "argu"; k += "ments"; let ok = false; try { f[k] = 1; } catch (e) { ok = e instanceof TypeError; } ok"#,
        true,
    );
}

#[test]
fn every_watched_write_path_sets_its_dirty_bit() {
    // An own overwrite (`__set`), an added property (`__set` -> `__define`)
    // and a delete (`__delete`) on a watched intrinsic each reach the guard.
    assert_number("const m = Math; m.max = () => 42; Math.max(1, 2)", 42.0);
    assert_number(
        "Array.prototype.pop = function () { return 7; }; [1].pop()",
        7.0,
    );
    assert_uncaught(
        "const j = JSON; delete j.stringify; JSON.stringify(1)",
        "TypeError: JSON.stringify is not a function",
    );
    // A user `push` on `%Object.prototype%` is shadowed by the builtin
    // `Array.prototype.push`, so the array fast path stays.
    assert_number(
        "Object.prototype.push = function () { return 99; }; const a = []; a.push(5); a[0]",
        5.0,
    );
}
