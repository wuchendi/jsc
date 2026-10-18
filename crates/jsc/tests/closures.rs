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
