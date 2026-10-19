//! Integration tests: closures via the CLI.

mod common;
use common::jsc;

/// Closures run end to end through the built binary: a returned arrow
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

/// A top-level `function` declaration whose body reads (and writes) an
/// enclosing top-level `let`/`const` closes over it, running end to end through
/// the built binary. A non-capturing declaration also runs, and a
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

/// A closure created in a C-style `for (let ...)` loop captures that
/// iteration's binding, not one cell shared by every iteration. The update runs
/// on the next iteration's copy, so a body mutation is still seen by it.
#[test]
fn for_let_closures_capture_per_iteration_binding() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<3;i++) fs.push(() => i); \
             fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()"
        ]),
        "0,1,2"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<3;i++) { fs.push(() => i); i++; } \
             fs[0]()+\",\"+fs[1]()+\",\"+fs.length"
        ]),
        "1,3,2"
    );
}

/// A setter closure writes only its own iteration's copy: called after the loop
/// it leaves the other iterations' values alone, and called inside the body it
/// feeds that iteration's update.
#[test]
fn for_let_setter_closure_mutates_its_iteration_only() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; let gs=[]; \
             for (let i=0;i<3;i++) { fs.push(() => i); gs.push(v => { i = v; }); } \
             gs[0](100); fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()"
        ]),
        "100,1,2"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<3;i++) { let s = v => { i = v; }; s(i+1); fs.push(() => i); } \
             fs[0]()+\",\"+fs[1]()+\",\"+fs.length"
        ]),
        "1,3,2"
    );
}

/// Nested `for (let ...)` loops each copy their own binding per iteration.
#[test]
fn nested_for_let_closures_are_distinct() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<2;i++) for (let j=0;j<2;j++) fs.push(() => i*10+j); \
             fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()+\",\"+fs[3]()"
        ]),
        "0,1,10,11"
    );
}

/// `continue` still advances to a fresh copy, and `break` leaves the last
/// closure holding the break iteration's value.
#[test]
fn for_let_closures_across_continue_and_break() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<4;i++) { fs.push(() => i); if (i<2) continue; } \
             fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()+\",\"+fs[3]()"
        ]),
        "0,1,2,3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0;i<6;i++) { if (i%2) continue; fs.push(() => i); if (i==4) break; } \
             fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()+\",\"+fs.length"
        ]),
        "0,2,4,3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let last; for (let i=0;;i++) { last = () => i; if (i==2) break; } last()"
        ]),
        "2"
    );
}

/// A captured `const` loop binding and multiple `let` declarators.
#[test]
fn for_const_and_multi_declarator_closures() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (const x = 5;;) { fs.push(() => x); break; } fs[0]()"
        ]),
        "5"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (let i=0, j=10; i<3; i++) { fs.push(() => i+\":\"+j); j--; } \
             fs[0]()+\",\"+fs[2]()"
        ]),
        "0:9,2:7"
    );
}

/// `for-of` and `for-in` already bind a fresh binding per iteration.
#[test]
fn for_of_and_for_in_closures_capture_per_iteration_binding() {
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (const x of [1,2,3]) fs.push(() => x); \
             fs[0]()+\",\"+fs[1]()+\",\"+fs[2]()"
        ]),
        "1,2,3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let fs=[]; for (const k in {a:1,b:2}) fs.push(() => k); fs[0]()+\",\"+fs[1]()"
        ]),
        "a,b"
    );
}
