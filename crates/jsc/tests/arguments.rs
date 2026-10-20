//! Integration tests: the `arguments` object (an ordinary object of class
//! ARGUMENTS with unmapped, strict-mode semantics) and rest parameters receiving
//! every spread argument, including those beyond the uniform ABI's `K` slots.
#![allow(clippy::expect_used)]

mod common;
use common::{jsc, jsc_raw_stdout};

use std::process::Command;

fn run(src: &str) -> String {
    jsc_raw_stdout(&["-e", src])
}

/// Run `jsc -e src`, assert it is a compile error (exit 2), and return stderr.
fn compile_error(src: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", src])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(
        output.status.code(),
        Some(2),
        "jsc -e {src:?}: stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stderr).expect("stderr should be UTF-8")
}

/// A rest parameter receives every spread element, also beyond the largest
/// arity in the program.
#[test]
fn rest_receives_all_spread_arguments() {
    assert_eq!(
        run("let f = (...xs) => xs.length; console.log(f(...[1,2,3]))"),
        "3\n"
    );
    assert_eq!(
        run("let f = (...xs) => xs.length;
             console.log(f(...[1,2,3,4,5,6,7,8,9,10]))"),
        "10\n"
    );
    assert_eq!(
        run("let f = (...xs) => xs.join('-');
             console.log(f(...[1,2,3,4,5,6,7,8,9,10]))"),
        "1-2-3-4-5-6-7-8-9-10\n"
    );
    assert_eq!(
        run("function g(a, ...r){ return r.length } console.log(g(...[1,2,3,4]))"),
        "3\n"
    );
    assert_eq!(
        run("function g(a, ...r){ return a + ':' + r.join(',') }
             console.log(g(...[1,2,3]))"),
        "1:2,3\n"
    );
    // Spread mixed with positional arguments.
    assert_eq!(
        run("function g(a, ...r){ return r.length } console.log(g(0, ...[1,2], 9))"),
        "3\n"
    );
    assert_eq!(
        run("function g(a, ...r){ return r.join(',') }
             let xs = [1,2,3,4,5,6]; console.log(g(0, ...xs, 9))"),
        "1,2,3,4,5,6,9\n"
    );
    // Indirect call through a binding.
    assert_eq!(
        run("function g(...r){ return r[7] } let h = g;
             console.log(h(...[0,1,2,3,4,5,6,7]))"),
        "7\n"
    );
}

/// Nested spread calls inside the arguments of a spread call each see their
/// own argument array.
#[test]
fn nested_spread_calls() {
    assert_eq!(
        run("let n = (...a) => a.length;
             let s = (...a) => a.join(',');
             console.log(s(...[1,2,3,4,5], n(...[1,2,3,4,5,6,7]), ...[8,9]))"),
        "1,2,3,4,5,7,8,9\n"
    );
    // A spread call inside a default parameter does not disturb the
    // enclosing rest parameter.
    assert_eq!(
        run("let n = (...a) => a.length;
             function f(d = n(...[1,2,3,4,5,6]), ...r){ return d + ':' + r.length }
             console.log(f(undefined, ...[1,2,3,4,5,6,7,8]))"),
        "6:8\n"
    );
}

/// `o.m(...xs)` is a method call binding `this = o`.
#[test]
fn spread_method_calls() {
    assert_eq!(
        run("let o = { m(...a){ return this.t + a.length }, t: 10 };
             console.log(o.m(...[1,2,3]))"),
        "13\n"
    );
    assert_eq!(
        run("let o = { m(a, b){ return this.t + a + b }, t: 10 };
             console.log(o['m'](...[1,2]))"),
        "13\n"
    );
    assert_eq!(
        run(
            "class P { constructor(){ this.k = 2 } sum(...a){ return this.k * a.length } }
             let p = new P(); console.log(p.sum(...[1,2,3,4,5,6,7]))"
        ),
        "14\n"
    );
    // A non-callable member throws a TypeError naming it.
    let out = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "let o = {}; o.m(...[1])"])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("o.m is not a function"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `new C(...xs)` and `super(...args)` forward every argument.
#[test]
fn spread_construct_and_super() {
    assert_eq!(
        run("class C { constructor(...a){ this.n = a.length } }
             console.log(new C(...[1,2,3,4,5]).n)"),
        "5\n"
    );
    assert_eq!(
        run("class A { constructor(...a){ this.s = a.join(',') } }
             class B extends A { constructor(...a){ super(...a, 'x') } }
             console.log(new B(...[1,2,3,4,5,6]).s)"),
        "1,2,3,4,5,6,x\n"
    );
    assert_eq!(
        run("function F(){ this.n = arguments.length }
             console.log(new F(...[1,2,3,4,5,6,7]).n)"),
        "7\n"
    );
}

/// `arguments` holds every actual argument and its count.
#[test]
fn arguments_object_basics() {
    assert_eq!(
        run("function h(){ return arguments.length } console.log(h(1,2,3), h())"),
        "3 0\n"
    );
    assert_eq!(
        run("function h(){ return arguments.length }
             console.log(h(...[1,2,3,4,5,6,7,8]))"),
        "8\n"
    );
    assert_eq!(
        run("function h(){ return arguments[7] }
             console.log(h(...[1,2,3,4,5,6,7,8]), h(1))"),
        "8 undefined\n"
    );
    assert_eq!(
        run("function sum(){ let t = 0;
                for (let i = 0; i < arguments.length; i++) t += arguments[i];
                return t }
             console.log(sum(1,2,3,4))"),
        "10\n"
    );
    // Passing `arguments` along to another function.
    assert_eq!(
        run("function len(a){ return a.length + ':' + a[1] }
             function f(){ return len(arguments) } console.log(f('x','y'))"),
        "2:y\n"
    );
    // Methods and class constructors have `arguments` too.
    assert_eq!(
        run("let o = { m(){ return arguments.length } }; console.log(o.m(1,2))"),
        "2\n"
    );
    assert_eq!(
        run("console.log(new (function(){ this.n = arguments.length })(1,2).n)"),
        "2\n"
    );
}

/// Unmapped semantics: assigning a parameter does not change `arguments`, and
/// writing `arguments` does not change the parameter.
#[test]
fn arguments_is_unmapped() {
    assert_eq!(
        run("function k(a){ a = 9; return arguments[0] } console.log(k(1))"),
        "1\n"
    );
    assert_eq!(
        run("function k(a){ arguments[0] = 9; return a } console.log(k(1))"),
        "1\n"
    );
    // A default parameter does not appear in `arguments`.
    assert_eq!(
        run(
            "function d(a = 5){ return arguments.length + ':' + arguments[0] }
             console.log(d())"
        ),
        "0:undefined\n"
    );
    // A captured parameter still reads its original value through `arguments`.
    assert_eq!(
        run(
            "function c(a){ let g = () => a; a = 3; return arguments[0] + g() }
             console.log(c(1))"
        ),
        "4\n"
    );
}

/// An arrow sees its enclosing function's `arguments`.
#[test]
fn arrows_see_enclosing_arguments() {
    assert_eq!(
        run("function f(){ let g = () => arguments[1]; return g(7) } console.log(f(1, 2))"),
        "2\n"
    );
    assert_eq!(
        run("function f(){ return () => () => arguments.length } console.log(f(1,2,3)()())"),
        "3\n"
    );
}

/// A local binding named `arguments` shadows the object.
#[test]
fn arguments_binding_shadows() {
    assert_eq!(
        run("function f(arguments){ return arguments } console.log(f(4))"),
        "4\n"
    );
    assert_eq!(
        run("function f(){ let arguments = 5; return arguments } console.log(f(1))"),
        "5\n"
    );
}

/// `arguments` renders like Node and is an object.
#[test]
fn arguments_rendering() {
    assert_eq!(
        run("function r(){ return arguments } console.log(r(1,2))"),
        "[Arguments] { '0': 1, '1': 2 }\n"
    );
    assert_eq!(
        run("function r(){ return arguments } console.log(r())"),
        "[Arguments] {}\n"
    );
    assert_eq!(
        run("function r(){ return typeof arguments } console.log(r())"),
        "object\n"
    );
    // `length` is non-enumerable.
    assert_eq!(
        run("function r(){ return Object.keys(arguments).join(',') } console.log(r('a','b'))"),
        "0,1\n"
    );
    assert_eq!(
        jsc(&["-p", "function r(){ return arguments } r(3)"]),
        "[Arguments] { '0': 3 }"
    );
}

/// Top-level `arguments` (including inside a top-level arrow) is a compile
/// error.
#[test]
fn top_level_arguments_rejected() {
    let err = compile_error("console.log(arguments)");
    assert!(err.contains("arguments"), "{err}");
    let err = compile_error("let f = () => arguments; f()");
    assert!(err.contains("arguments"), "{err}");
}

/// A plain call never reads a stale spread argument array left by an earlier
/// spread call, even when its argument count escapes the arity scan.
#[test]
fn plain_call_ignores_stale_spread_arguments() {
    assert_eq!(
        run("let g = (...a) => a.length; g(...[1,2,3,4,5,6,7,8]);
             function f(...r){ return r.length + ':' + arguments.length }
             console.log(f(1), `${f(1,2,3,4,5,6,7,8,9)}` === '8:8')"),
        "1:1 false\n"
    );
}

/// Spread calls read their argument array through the array handle's element
/// store: an array grown by `push` (its store reallocated) spreads into plain,
/// method, `new` and `super(..)` calls, `arguments` and rest parameters, all
/// beyond the `K` slots; the rest array is itself a growable array.
#[test]
fn spread_of_grown_array() {
    let grown = "let a = []; for (let i = 0; i < 20; i++) a.push(i);";
    assert_eq!(
        run(&format!(
            "{grown} function f(...r){{ return r.length }} console.log(f(...a), Math.max(...a))"
        )),
        "20 19\n"
    );
    assert_eq!(
        run(&format!(
            "{grown} const o = {{ m(...r){{ return r[19] + r.length }} }}; console.log(o.m(...a))"
        )),
        "39\n"
    );
    assert_eq!(
        run(&format!(
            "{grown} class C {{ constructor(...r){{ this.n = r.length; this.l = r[19] }} }} \
             const c = new C(...a); console.log(c.n, c.l)"
        )),
        "20 19\n"
    );
    assert_eq!(
        run(&format!(
            "{grown} class B {{ constructor(...r){{ this.n = r.length; this.l = r[19] }} }} \
             class D extends B {{ constructor(){{ super(...a) }} }} const d = new D(); console.log(d.n, d.l)"
        )),
        "20 19\n"
    );
    assert_eq!(
        run(&format!(
            "{grown} function f(){{ return arguments.length + ':' + arguments[19] }} console.log(f(...a))"
        )),
        "20:19\n"
    );
    assert_eq!(
        run(&format!(
            "{grown} function f(...r){{ r.push(99); return r.length + ':' + r[20] }} console.log(f(...a))"
        )),
        "21:99\n"
    );
}

/// A hole in a spread source reaches the callee as `undefined`.
#[test]
fn spread_of_array_with_holes() {
    assert_eq!(
        run("function f(a, b, c){ return [a, b, c] } console.log(f(...[1, , 3]))"),
        "[ 1, undefined, 3 ]\n"
    );
    assert_eq!(
        run("let h = [0]; h[3] = 3; function f(...r){ return r } console.log(f(...h))"),
        "[ 0, undefined, undefined, 3 ]\n"
    );
}
