//! Integration tests: the official test262 `sta.js` + `assert.js` (milestone
//! M1) and `propertyHelper.js` compile and run unmodified, with passing and
//! failing assertions.
//!
//! The harness is read from the checkout fetched by
//! `scripts/test262-fetch.sh` (`.tmp/test262/harness`); nothing is vendored.
//! When the checkout is absent the tests print a notice and return.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp/test262/harness")
}

/// `"use strict";`, the official `sta.js` + `assert.js` and `includes`, then
/// `body`; `None` when the test262 checkout has not been fetched.
fn program(includes: &[&str], body: &str) -> Option<String> {
    let dir = harness_dir();
    if !dir.join("assert.js").is_file() {
        eprintln!(
            "skipped: {} not found; run `bash scripts/test262-fetch.sh`",
            dir.display()
        );
        return None;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read harness file");
    let mut src = format!(
        "\"use strict\";\n{}\n{}\n",
        read("sta.js"),
        read("assert.js")
    );
    for include in includes {
        src.push_str(&read(include));
        src.push('\n');
    }
    src.push_str(body);
    src.push('\n');
    Some(src)
}

fn run(name: &str, src: &str) -> Output {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, src).expect("write temp source");
    Command::new(env!("CARGO_BIN_EXE_jsc"))
        .arg(&path)
        .output()
        .expect("failed to spawn jsc binary")
}

/// Runs `body` under the harness plus `includes` and returns (exit code,
/// stderr).
fn harness_with(includes: &[&str], name: &str, body: &str) -> Option<(Option<i32>, String)> {
    let out = run(name, &program(includes, body)?);
    Some((
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Runs `body` under `sta.js` + `assert.js` and returns (exit code, stderr).
fn harness(name: &str, body: &str) -> Option<(Option<i32>, String)> {
    harness_with(&[], name, body)
}

#[test]
fn passing_assertions_exit_zero() {
    let Some((code, stderr)) = harness(
        "t262_pass.js",
        "assert(true);
         assert.sameValue(1, 1);
         assert.sameValue(NaN, NaN);
         assert.notSameValue(0, -0);
         assert.throws(TypeError, function () { null.x; });
         assert.throws(Test262Error, function () { throw new Test262Error('x'); });
         assert.compareArray([1, 2, 3], [1, 2, 3]);",
    ) else {
        return;
    };
    assert_eq!(code, Some(0), "{stderr}");
}

#[test]
fn failing_assertions_throw_test262_error() {
    let cases = [
        ("assert(false);", "Expected true but got false"),
        (
            "assert.sameValue(1, 2, 'm');",
            "m Expected SameValue(«1», «2») to be true",
        ),
        (
            "assert.notSameValue(1, 1);",
            "Expected SameValue(«1», «1») to be false",
        ),
        (
            "assert.throws(TypeError, function () { throw new RangeError('r'); });",
            "Expected a TypeError but got a RangeError",
        ),
        (
            "assert.throws(TypeError, function () {});",
            "Expected a TypeError to be thrown but no exception was thrown at all",
        ),
        (
            "assert.compareArray([1, 2], [1, 3]);",
            "Actual [1, 2] and expected [1, 3] should have the same contents.",
        ),
    ];
    for (i, (body, message)) in cases.into_iter().enumerate() {
        let Some((code, stderr)) = harness(&format!("t262_fail_{i}.js"), body) else {
            return;
        };
        assert_eq!(code, Some(1), "{body}: {stderr}");
        assert!(
            stderr.starts_with("Uncaught Test262Error"),
            "{body}: {stderr}"
        );
        assert!(stderr.contains(message), "{body}: {stderr}");
    }
}

#[test]
fn caught_failure_is_a_test262_error() {
    let Some((code, stderr)) = harness(
        "t262_caught.js",
        "var caught;
         try { assert.sameValue(1, 2); } catch (e) { caught = e; }
         if (!(caught instanceof Test262Error)) { throw new Error('not a Test262Error'); }
         if (caught.constructor !== Test262Error) { throw new Error('wrong constructor'); }
         if (String(caught).indexOf('Test262Error: ') !== 0) { throw new Error(String(caught)); }",
    ) else {
        return;
    };
    assert_eq!(code, Some(0), "{stderr}");
}

/// Every `propertyHelper.js` verifier accepts correct descriptors of
/// ordinary objects, arrays, functions, a frozen object, accessors, and
/// builtin methods.
#[test]
fn property_helper_verifiers_pass() {
    let Some((code, stderr)) = harness_with(
        &["propertyHelper.js"],
        "t262_property_helper_pass.js",
        "var o = {a: 1, b: 2};
         verifyProperty(o, 'a', {value: 1, writable: true, enumerable: true, configurable: true});
         verifyProperty(o, 'b', {value: 2, writable: true}, {restore: true});
         verifyProperty(o, 'missing', undefined);
         var arr = [1, 2];
         verifyProperty(arr, 'length', {value: 2, writable: true, enumerable: false, configurable: false});
         verifyProperty(arr, '0', {value: 1, writable: true, enumerable: true, configurable: true});
         function f(x, y) {}
         verifyProperty(f, 'length', {value: 2, writable: false, enumerable: false, configurable: true});
         verifyProperty(f, 'name', {value: 'f', writable: false, enumerable: false, configurable: true});
         verifyProperty(f, 'prototype', {writable: true, enumerable: false, configurable: false});
         var fr = Object.freeze({x: 1});
         verifyProperty(fr, 'x', {value: 1, writable: false, enumerable: true, configurable: false});
         verifyCallableProperty(Array.prototype, 'push', 'push', 1);
         verifyPrimordialCallableProperty(Object, 'keys', 'keys', 1);
         verifyPrimordialProperty(Math, 'PI', {writable: false, enumerable: false, configurable: false});
         verifyAccessorProperty(Map.prototype, 'size', {get: {}});
         verifyPrimordialAccessorProperty(Object.prototype, '__proto__', {get: {}, set: {}});
         var g = function () { return 1; };
         var acc = Object.defineProperty({}, 'p', {get: g, configurable: true});
         verifyAccessorProperty(acc, 'p', {get: g, set: undefined});
         var ro = Object.defineProperty({}, 'k', {value: 3});
         verifyEqualTo(ro, 'k', 3);
         verifyNotWritable(ro, 'k');
         verifyNotEnumerable(ro, 'k');
         verifyNotConfigurable(ro, 'k');
         var rw = {z: 1};
         verifyEqualTo(rw, 'z', 1);
         verifyWritable(rw, 'z');
         verifyEnumerable(rw, 'z');
         verifyConfigurable(rw, 'z');
         verifyNotEnumerable(Array.prototype, 'map');
         verifyWritable(Array.prototype, 'map');",
    ) else {
        return;
    };
    assert_eq!(code, Some(0), "{stderr}");
}

/// A wrong descriptor fails each verifier with its `Test262Error` message.
#[test]
fn property_helper_failures() {
    let cases = [
        (
            "verifyProperty({q: 1}, 'q', {value: 2, writable: false});",
            "q descriptor value should be 2; q value should be 2; q descriptor should not be writable",
        ),
        (
            "verifyProperty([1], 'length', {enumerable: true});",
            "length descriptor should be enumerable",
        ),
        (
            "verifyProperty(Object.freeze({x: 1}), 'x', {configurable: true});",
            "x descriptor should be configurable",
        ),
        (
            "verifyProperty({}, 'x', {value: 1});",
            "x should be an own property",
        ),
        (
            "verifyCallableProperty(Array.prototype, 'push', 'push', 2);",
            "push length descriptor value should be 2",
        ),
        (
            "verifyAccessorProperty(Map.prototype, 'size', {get: {name: 'size'}});",
            "size getter name descriptor value should be size",
        ),
        (
            "verifyNotWritable({w: 1}, 'w');",
            "Expected obj[w] to have writable:false.",
        ),
        (
            "verifyEqualTo({v: 1}, 'v', 2);",
            "Expected obj[v] to equal 2, actually 1",
        ),
    ];
    for (i, (body, message)) in cases.into_iter().enumerate() {
        let Some((code, stderr)) = harness_with(
            &["propertyHelper.js"],
            &format!("t262_property_helper_fail_{i}.js"),
            body,
        ) else {
            return;
        };
        assert_eq!(code, Some(1), "{body}: {stderr}");
        assert!(
            stderr.starts_with("Uncaught Test262Error"),
            "{body}: {stderr}"
        );
        assert!(stderr.contains(message), "{body}: {stderr}");
    }
}
