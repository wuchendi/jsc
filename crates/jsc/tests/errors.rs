//! Integration tests: compile-time diagnostics and the `throw`/`try`/`catch` exception surface via the CLI.
#![allow(clippy::expect_used)]

use std::process::Command;
mod common;
use common::jsc;

/// Reassigning a `const` binding is a compile error: the CLI exits
/// 2 and reports the constant on stderr.
#[test]
fn const_reassignment_fails_at_compile_time() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "const c = 1; c = 2; c"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.code() == Some(2),
        "reassigning a const must make jsc exit 2"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("constant"),
        "stderr should explain the const violation, got: {stderr}"
    );
}

/// Reassigning a `const` declared with a different name is likewise a compile
/// error end to end: the CLI exits 2 and names the constant.
#[test]
fn const_reassignment_of_other_binding_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "const x = 1; x = 2; x"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.code() == Some(2),
        "reassigning a const must make jsc exit 2"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("constant"),
        "stderr should explain the const violation, got: {stderr}"
    );
}

/// The diagnostic renderer points a caret at the offending source: a spanned
/// `CompileError` (const-reassignment) prints a rustc/swc-style block on stderr
/// with `<eval>:LINE:COL` and a `^` caret, and the CLI exits 2.
#[test]
fn const_reassignment_renders_caret_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "const c = 1; c = 2"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: cannot assign to constant binding: c"),
        "stderr should carry the message, got: {stderr}"
    );
    assert!(
        stderr.contains("--> <eval>:1:14"),
        "stderr should locate the offending name, got: {stderr}"
    );
    assert!(
        stderr.contains(":1:"),
        "stderr should carry a 1-based line, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "stderr should draw a caret under the span, got: {stderr}"
    );
}

/// The already-spanned unsupported-statement path also renders a caret block
/// (proving the renderer is not special-cased to const-reassignment).
#[test]
fn unsupported_statement_renders_caret_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "debugger"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--> <eval>:1:"), "got: {stderr}");
    assert!(stderr.contains('^'), "got: {stderr}");
}

/// An *unknown identifier* is resolved against the global object at run
/// time (DESIGN.md §6.4): `NaN2 + 1` compiles and throws an uncaught
/// `ReferenceError` (exit 1).
#[test]
fn undeclared_identifier_throws_reference_error_at_run_time() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "NaN2 + 1"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(1), "must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Uncaught ReferenceError: NaN2 is not defined"),
        "got: {stderr}"
    );
}

/// A codegen-rejected *unsupported construct* (a `RegExp` literal)
/// carries the offending node's span: the diagnostic points a caret at
/// the construct.
#[test]
fn unsupported_construct_renders_caret_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "let r = /a/;"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(":1:"),
        "stderr should carry a 1-based location, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "stderr should draw a caret under the construct, got: {stderr}"
    );
}

/// An unsupported `for-of` head (an assignment-target binding, not a
/// `const`/`let` declaration) is rejected with a clean human message and a caret
/// block — crucially, the diagnostic must NOT leak the raw swc AST debug form
/// (`ForOfStmt { .. }`, `span:`, or braces from a `{:?}` dump). The body is
/// written brace-free (`... ) x`) so the source-echo line cannot itself
/// introduce a `{`/`}`, keeping the "no AST-debug braces" check meaningful.
#[test]
fn for_of_renders_clean_message_without_ast_debug() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "for (x of [1,2]) x"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("for-of requires a `const`/`let` loop variable"),
        "stderr should carry the clean message, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "stderr should draw a caret under the loop, got: {stderr}"
    );
    for debris in ['{', '}'] {
        assert!(
            !stderr.contains(debris),
            "stderr must not leak AST-debug braces, got: {stderr}"
        );
    }
    assert!(
        !stderr.contains("ForOfStmt"),
        "stderr must not leak the AST node name, got: {stderr}"
    );
    assert!(
        !stderr.contains("span:"),
        "stderr must not leak the raw span debug, got: {stderr}"
    );
}

/// An swc *parser* error (an unterminated `(`) is routed through the same
/// spanned-diagnostic path as codegen rejections: the CLI renders a caret block
/// with a `<eval>:LINE:COL` location, not the old `failed to parse source: ...`.
#[test]
fn parse_error_renders_caret_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "let x = ("])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(":1:"),
        "stderr should carry a 1-based location, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "stderr should draw a caret under the parse error, got: {stderr}"
    );
    assert!(
        !stderr.contains("failed to parse source"),
        "the old locationless message must be gone, got: {stderr}"
    );
}

/// A *recoverable* swc parse error (a TS annotation in plain JS) is not
/// silently accepted: the CLI exits 2 with a caret diagnostic and prints
/// nothing on stdout.
#[test]
fn recoverable_parse_error_renders_caret_diagnostic() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-p", "let x: number = 1; x"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(output.status.code() == Some(2), "must exit 2");
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--> <eval>:1:"),
        "stderr should carry a 1-based location, got: {stderr}"
    );
    assert!(
        stderr.contains('^'),
        "stderr should draw a caret under the parse error, got: {stderr}"
    );
}

/// Consolidated diagnostics contract (§9): a table of distinct compile-error
/// classes — a `const` reassignment, a `for-of` loop,
/// and an swc parse error — each renders the rustc/swc-style block, so every
/// rendered stderr must carry BOTH a `:1:` line:col locator AND a `^` caret.
/// This spans parse rejections and every codegen-rejection path in one place.
#[test]
fn compile_errors_render_locator_and_caret() {
    // (source, a human label for the failure message)
    let cases = [
        ("const c = 1; c = 2", "const reassignment"),
        ("for (x of [1,2]) x", "for-of loop"),
        ("for (let i=0;i<3;i=i+1) break nope", "unknown break label"),
        ("foo: { break foo }", "label on non-loop"),
        ("let x = (", "parse error"),
    ];
    for (src, what) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-e", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert!(
            output.status.code() == Some(2),
            "{what} (`{src}`) must make jsc exit 2"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(":1:"),
            "{what} (`{src}`) stderr should carry a 1-based line:col locator, got: {stderr}"
        );
        assert!(
            stderr.contains('^'),
            "{what} (`{src}`) stderr should draw a caret under the span, got: {stderr}"
        );
    }
}

/// The exception system (`throw`/`try`/`catch`/`finally`/`Error`) driven
/// through the real CLI on the `-p` path, one behaviour per assertion.
#[test]
fn exceptions_via_eval() {
    // try/catch binds the thrown number and, separately, the thrown string.
    assert_eq!(
        jsc(&["-p", "let r = 0; try { throw 42 } catch (e) { r = e } r"]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            r#"let r = 0; try { throw "boom" } catch (e) { r = e } r"#
        ]),
        "boom"
    );
    // No-throw path: the catch is skipped and the try's own value flows out.
    assert_eq!(
        jsc(&["-p", "let r = 42; try { r = r } catch (e) { r = -1 } r"]),
        "42"
    );
    // finally on the normal path runs (x ends 42) ...
    assert_eq!(
        jsc(&["-p", "let x = 40; try { x = 40 } finally { x = x + 2 } x"]),
        "42"
    );
    // ... and finally still runs after a caught throw (x ends 42).
    assert_eq!(
        jsc(&[
            "-p",
            "let x = 0; try { throw 1 } catch (e) { x = 40 } finally { x = x + 2 } x"
        ]),
        "42"
    );
    // Propagation across functions: f throws, g calls f, an outer try catches.
    assert_eq!(
        jsc(&[
            "-p",
            "function f(){ throw 42 } function g(){ f() } \
             let r = 0; try { g() } catch (e) { r = e } r"
        ]),
        "42"
    );
    // new Error(msg): `.message` reads the argument back; a thrown Error is caught.
    assert_eq!(
        jsc(&[
            "-p",
            r#"let r = 0; try { throw new Error("bad") } catch (e) { r = e.message } r"#
        ]),
        "bad"
    );
    // An Error reuses the OBJECT heap representation, so `typeof` is "object".
    assert_eq!(jsc(&["-p", r#"typeof (new Error("x"))"#]), "object");
    // Nested try + rethrow: inner catch transforms the value, outer catch binds 42.
    assert_eq!(
        jsc(&[
            "-p",
            "let r = 0; try { try { throw 1 } catch (e) { throw e + 41 } } \
             catch (e) { r = e } r"
        ]),
        "42"
    );
}

/// An UNCAUGHT `throw` (no enclosing handler) must fail the process: the
/// CLI exits 1 and reports the unwound value as `Uncaught <value>` on
/// stderr, never printing a normal result to stdout.
#[test]
fn uncaught_throw_exits_one() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "throw 42"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.code() == Some(1),
        "an uncaught throw must make jsc exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Uncaught 42"),
        "stderr should report the uncaught value, got: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "no normal result should be printed for an uncaught throw"
    );
}

/// An UNCAUGHT `TypeError` from a property write fails the process: exit 1
/// with the message on stderr (a store on a primitive throws).
#[test]
fn uncaught_property_write_error_exits_one() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args([
            "-e",
            "let s = \"x\"; s.foo = 1; console.log(\"unreachable\")",
        ])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.code() == Some(1),
        "an uncaught property-write error must make jsc exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Uncaught TypeError: Cannot create property 'foo' on string 'x'"),
        "stderr should report the TypeError, got: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "nothing after the failed write should run"
    );
}

/// A property-write `TypeError` (a store on a primitive) unwinds through
/// `finally` and nested function frames to the nearest handler.
#[test]
fn property_write_error_unwinds_through_finally_and_calls() {
    assert_eq!(
        jsc(&[
            "-p",
            "let log=\"\"; function f(o){ try { o[\"foo\"] = 1; log+=\"no\" } finally { log+=\"f\" } } \
             try { f(\"x\") } catch(e) { log+=\"|\"+e.name } log"
        ]),
        "f|TypeError"
    );
}

/// Out-of-scope class forms are rejected at compile time with a
/// non-zero exit and an explanatory, spanned message. Every other member form,
/// computed names included, is supported (see the classes suites); private
/// members stay out of scope.
#[test]
fn unsupported_class_forms_error() {
    let cases = [("class A{ #p = 1 } 1", "private")];
    for (src, needle) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-e", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert!(
            output.status.code() == Some(2),
            "`{src}` must make jsc exit 2"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(needle),
            "stderr for `{src}` should mention `{needle}`, got: {stderr}"
        );
    }
}

/// An unhandled promise rejection fails the process after the microtask queue
/// drains: exit 1 with `Uncaught (in promise) <value>` on stderr.
#[test]
fn unhandled_rejection_exits_one() {
    let cases = [
        (
            "Promise.reject(new Error(\"x\"))",
            "Uncaught (in promise) Error: x",
        ),
        (
            "async function f() { throw new TypeError(\"t\") } f()",
            "Uncaught (in promise) TypeError: t",
        ),
        ("Promise.reject(1)", "Uncaught (in promise) 1"),
    ];
    for (src, needle) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-e", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert_eq!(
            output.status.code(),
            Some(1),
            "`{src}` must make jsc exit 1"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(needle),
            "stderr for `{src}` should contain `{needle}`, got: {stderr}"
        );
    }
}

/// A handled rejection exits 0 with nothing on stderr.
#[test]
fn handled_rejection_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "Promise.reject(1).catch(() => {})"])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(output.status.code(), Some(0), "a caught rejection exits 0");
    assert!(output.stderr.is_empty(), "stderr must be empty");
}
