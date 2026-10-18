//! Integration tests: first-class function values and arrows via the CLI.

mod common;
use common::jsc;

/// S13.2 — first-class function values run end to end through the built binary:
/// a function bound to a variable and called indirectly, a function passed as an
/// argument, and `typeof` over a function value.
#[test]
fn function_values_via_eval() {
    assert_eq!(
        jsc(&["-p", "function add(a,b){return a+b} let f = add; f(40,2)"]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function apply(f,x){return f(x)} function dbl(n){return n*2} apply(dbl,21)"
        ]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "function id(x){return x} let g=id; typeof g"]),
        "function"
    );
}

/// S13.4 — arrow functions run end to end through the built binary: multi-param
/// and single-param expression bodies, a block body with an explicit `return`,
/// an arrow passed straight as a callback, and `typeof` over an arrow.
#[test]
fn arrow_functions_via_eval() {
    assert_eq!(jsc(&["-p", "let add = (a,b) => a+b; add(40,2)"]), "42");
    assert_eq!(jsc(&["-p", "let sq = x => x*x; sq(7)"]), "49");
    assert_eq!(
        jsc(&["-p", "let f = (n) => { let t = n*2; return t+1 }; f(20)"]),
        "41"
    );
    assert_eq!(
        jsc(&["-p", "function apply(f,x){return f(x)} apply(x=>x*3, 14)"]),
        "42"
    );
    assert_eq!(jsc(&["-p", "typeof (x=>x)"]), "function");
}

/// S13.7 — the S13 acceptance milestones as one consolidated end-to-end gate
/// through the built binary: a function bound to a variable and called
/// indirectly, a function passed as an argument, arrow functions (multi- and
/// single-param), a returned closure capturing a parameter, a stateful counter
/// closure mutating a shared cell across calls, and member-target compound
/// assignment on an array element and an object property.
#[test]
fn s13_acceptance_milestones() {
    // First-class function values: bind-and-call, pass-as-argument.
    assert_eq!(
        jsc(&["-p", "function add(a,b){return a+b} let f=add; f(40,2)"]),
        "42"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function apply(f,x){return f(x)} function dbl(n){return n*2} apply(dbl,21)"
        ]),
        "42"
    );
    // Arrow functions: multi-param and single-param expression bodies.
    assert_eq!(jsc(&["-p", "let add=(a,b)=>a+b; add(40,2)"]), "42");
    assert_eq!(jsc(&["-p", "let sq=x=>x*x; sq(7)"]), "49");
    // Closures: returned arrow capturing a parameter by reference.
    assert_eq!(
        jsc(&[
            "-p",
            "function adder(x){ return y=>x+y } let a5=adder(5); a5(37)"
        ]),
        "42"
    );
    // Closures: stateful counter mutating one shared cell across three calls.
    assert_eq!(
        jsc(&[
            "-p",
            "function counter(){ let c=0; return function(){ c=c+1; return c } } \
             let n=counter(); n();n();n()"
        ]),
        "3"
    );
    // Member-target compound assignment: array element and object property.
    assert_eq!(jsc(&["-p", "let a=[10,20]; a[0]+=5; a[0]"]), "15");
    assert_eq!(jsc(&["-p", "let o={x:1}; o.x+=41; o.x"]), "42");
}

/// P1.3 — default parameters: a missing or explicitly-`undefined` argument
/// triggers the default, which is evaluated per call and may reference earlier
/// (already-bound) parameters. An explicit non-undefined argument is kept.
#[test]
fn default_parameters() {
    // Literal default used when the argument is omitted, kept when supplied.
    assert_eq!(jsc(&["-p", "function f(a, b=2){ return a+b } f(40)"]), "42");
    assert_eq!(
        jsc(&["-p", "function f(a, b=2){ return a+b } f(40, 100)"]),
        "140"
    );
    // A default referencing an earlier parameter.
    assert_eq!(jsc(&["-p", "function g(a, b=a+1){ return b } g(41)"]), "42");
    // Explicit `undefined` triggers the default, like an omitted argument.
    assert_eq!(
        jsc(&["-p", "function f(a, b=2){ return a+b } f(40, undefined)"]),
        "42"
    );
}

/// P1.6 — destructuring patterns as function parameters: an array pattern and an
/// object pattern bind their elements/properties, composing with nested defaults,
/// renames, nested patterns, a whole-pattern default, and arrows.
#[test]
fn parameter_destructuring() {
    // Array-pattern and object-pattern parameters (the core milestones).
    assert_eq!(
        jsc(&["-p", "function f([a,b]){ return a+b } f([40,2])"]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "function g({x,y}){ return x*y } g({x:6,y:7})"]),
        "42"
    );
    // A nested default inside an array-pattern parameter fills a missing element.
    assert_eq!(
        jsc(&["-p", "function d([a,b=5]){ return a+b } d([40])"]),
        "45"
    );
    // Object-pattern rename plus a shorthand default.
    assert_eq!(
        jsc(&["-p", "function g({x:m,y=10}){ return m+y } g({x:32})"]),
        "42"
    );
    // A nested object pattern binds a deep property.
    assert_eq!(
        jsc(&["-p", "function h({p:{q}}){ return q } h({p:{q:42}})"]),
        "42"
    );
    // A whole-pattern (parameter-level) default applies when the argument is
    // omitted, and is ignored when supplied.
    assert_eq!(
        jsc(&["-p", "function f([a,b]=[40,2]){ return a+b } f()"]),
        "42"
    );
    assert_eq!(
        jsc(&["-p", "function f([a,b]=[1,2]){ return a+b } f([40,2])"]),
        "42"
    );
    // A destructuring parameter on an arrow.
    assert_eq!(jsc(&["-p", "let f=([a,b])=>a+b; f([40,2])"]), "42");
}

/// P1.4 — a trailing rest parameter gathers every argument from its position
/// onward into a REAL array: its `.length` reflects the actual argument count
/// (0 when none are passed), it composes with preceding fixed parameters, and it
/// is a normal array on which `.map`/indexing work.
#[test]
fn rest_parameters() {
    // The rest binding collects all arguments; its length is the actual count.
    assert_eq!(
        jsc(&["-p", "function f(...xs){ return xs.length } f(1,2,3)"]),
        "3"
    );
    assert_eq!(
        jsc(&["-p", "function f(...xs){ return xs.length } f()"]),
        "0"
    );
    // A leading fixed parameter, then the rest of the arguments.
    assert_eq!(
        jsc(&[
            "-p",
            "function h(a, ...rest){ return a + rest.length } h(10,1,1)"
        ]),
        "12"
    );
    // The rest binding is a normal array: higher-order methods and indexing work.
    assert_eq!(
        jsc(&[
            "-p",
            "function m(...xs){ return xs.map(x=>x*2)[1] } m(1,2,3)"
        ]),
        "4"
    );
    // A rest arrow, and a rest function called indirectly through a variable.
    assert_eq!(
        jsc(&["-p", "let f = (...xs) => xs.length; f(1,2,3,4,5)"]),
        "5"
    );
    assert_eq!(
        jsc(&["-p", "function f(...xs){return xs.length} let g=f; g(1,2)"]),
        "2"
    );
}

/// P1.5 — spread `...` in call arguments through the CLI: the arguments are
/// materialized into a runtime array and spread into the uniform ABI (P1.4), so
/// a direct, indirect, or rest-parameter callee receives the expanded values.
#[test]
fn p1_5_call_spread_via_eval() {
    assert_eq!(
        jsc(&["-p", "function add(a,b,c){return a+b+c} add(...[10,20,12])"]),
        "42"
    );
    // Plain and spread arguments mixed at one call site.
    assert_eq!(
        jsc(&["-p", "function add(a,b,c){return a+b+c} add(10,...[20,12])"]),
        "42"
    );
    // A rest-parameter callee collects the spread via the dynamic argc. The
    // uniform ABI passes K positional slots, so K (widened here by the 4-arg
    // `sum(1,2,3,4)` call) must cover the spread's runtime length.
    assert_eq!(
        jsc(&[
            "-p",
            "function sum(...xs){let t=0;for(const x of xs)t=t+x;return t} \
             sum(1,2,3,4); sum(...[1,2,3,4])"
        ]),
        "10"
    );
    // Indirect spread call through a bound function value.
    assert_eq!(jsc(&["-p", "let f=(a,b)=>a*b; f(...[6,7])"]), "42");
    // A spread whose source is a bound array variable.
    assert_eq!(
        jsc(&[
            "-p",
            "function add(a,b,c){return a+b+c} let xs=[10,20,12]; add(...xs)"
        ]),
        "42"
    );
}
