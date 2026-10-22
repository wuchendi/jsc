//! Integration tests: the official test262 `sta.js` + `assert.js` (milestone
//! M1) compile and run unmodified, with passing and failing assertions.
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

/// `"use strict";`, the official `sta.js` + `assert.js`, then `body`; `None`
/// when the test262 checkout has not been fetched.
fn program(body: &str) -> Option<String> {
    let dir = harness_dir();
    if !dir.join("assert.js").is_file() {
        eprintln!(
            "skipped: {} not found; run `bash scripts/test262-fetch.sh`",
            dir.display()
        );
        return None;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read harness file");
    Some(format!(
        "\"use strict\";\n{}\n{}\n{body}\n",
        read("sta.js"),
        read("assert.js")
    ))
}

fn run(name: &str, src: &str) -> Output {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, src).expect("write temp source");
    Command::new(env!("CARGO_BIN_EXE_jsc"))
        .arg(&path)
        .output()
        .expect("failed to spawn jsc binary")
}

/// Runs `body` under the harness and returns (exit code, stderr).
fn harness(name: &str, body: &str) -> Option<(Option<i32>, String)> {
    let out = run(name, &program(body)?);
    Some((
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
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
