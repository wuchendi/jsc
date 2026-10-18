//! Integration tests: closures via the CLI.

mod common;
use common::jsc;

/// S13.4 — closures run end to end through the built binary: a returned arrow
/// that captures a parameter by reference, a returned function that increments a
/// shared captured cell across calls, and two closures from one factory holding
/// independent cells.
#[test]
fn closures_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "function adder(x){ return y => x + y } let a5 = adder(5); a5(37)"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function counter(){ let c=0; return function(){ c=c+1; return c } } \
             let n=counter(); n(); n(); n()"
        ]),
        "3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function adder(x){ return y => x + y } let a=adder(5); let b=adder(100); a(1)+b(1)"
        ]),
        "107"
    );
}

/// G2 — a top-level `function` declaration whose body reads (and writes) an
/// enclosing top-level `let`/`const` closes over it, running end to end through
/// the built binary. A non-capturing declaration still runs unchanged, and a
/// class method reads an enclosing top-level binding the same way.
#[test]
fn function_declaration_captures_enclosing_binding_via_eval() {
    // Read a `let` and a `const`.
    assert_eq!(
        jsc(&["-p", "let n=41; function f(){ return n+1 } f()"]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "const k=42; function f(){ return k } f()"]),
        "42"
    );
    // Read AND write a shared cell across calls.
    assert_eq!(
        jsc(&[
            "-p",
            "let s=0; function add(x){ s = s + x } add(40); add(2); s"
        ]),
        "42"
    );
    // A nested `function` inside a top-level `function` still captures locally.
    assert_eq!(
        jsc(&[
            "-p",
            "function outer(){ let c=0; function inc(){ c=c+1; return c } inc(); return inc() } \
             outer()"
        ]),
        "2"
    );
    // A non-capturing declaration keeps working (direct call, hoisted).
    assert_eq!(
        jsc(&[
            "-p",
            "function g(){ return 7 } function f(){ return g() } f()"
        ]),
        "7"
    );
    // A class method reads an enclosing top-level binding.
    assert_eq!(
        jsc(&[
            "-p",
            "let base=40; class C { add(x){ return base + x } } new C().add(2)"
        ]),
        "42"
    );
}

/// A top-level `function` that references top-level bindings is callable from any
/// call site — top-level code, sibling top-level functions, nested arrows and
/// callbacks — and usable as a first-class value from them, sharing the bindings'
/// mutations in both directions.
#[test]
fn top_level_function_capturing_top_level_bindings_callable_anywhere() {
    // Called from an arrow bound at top level.
    assert_eq!(
        jsc(&[
            "-p",
            "let k = 5; function a() { return k } let f = () => a(); f()"
        ]),
        "5"
    );
    // Called from a sibling top-level function.
    assert_eq!(
        jsc(&[
            "-p",
            "let k = 5; function a() { return k } function b() { return a() } b()"
        ]),
        "5"
    );
    // Writes through a chain of top-level functions are visible at top level.
    assert_eq!(
        jsc(&[
            "-p",
            "let n = 0; function inc() { n++ } function run() { inc(); inc() } run(); n"
        ]),
        "2"
    );
    // A top-level class referenced from a function called inside a callback.
    assert_eq!(
        jsc(&[
            "-p",
            "class P { constructor(v){ this.v=v } } function mk(v){ return new P(v) } \
             [1,2].map(x => mk(x).v).join()"
        ]),
        "1,2"
    );
    // Passed as a value from another top-level function.
    assert_eq!(
        jsc(&[
            "-p",
            "let k=3; function g(){return k} function h(f){return f()} function w(){ return h(g) } w()"
        ]),
        "3"
    );
    // Recursion plus capture, passed as a callback.
    assert_eq!(
        jsc(&[
            "-p",
            "const base = 1; function fact(n){ return n <= base ? 1 : n * fact(n-1) } \
             [5].map(fact)[0]"
        ]),
        "120"
    );
}

/// A throw from a capturing top-level function invoked inside a callback
/// propagates as an uncaught exception (exit 1) instead of being lost.
#[test]
fn top_level_function_capturing_class_throws_from_callback() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args([
            "-e",
            "class E {} function a(x) { if (x !== 2) { throw new E(); } } \
             [1].forEach(() => a(1)); console.log(\"nothrow\");",
        ])
        .output()
        .unwrap_or_else(|e| unreachable!("failed to spawn jsc: {e}"));
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("nothrow"),
        "{output:?}"
    );
}
