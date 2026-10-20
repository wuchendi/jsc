//! Integration tests: cross-feature regression and acceptance gates via the CLI.
#![allow(clippy::expect_used)]

use std::process::Command;
mod common;
use common::{jsc, jsc_raw_stdout};

/// The core non-string programs hold alongside the string layer.
///
/// Numbers, `while`-sum, recursive `fib`, `typeof` over non-string types,
/// `null === null`, and a boolean-driven ternary.
#[test]
fn regression_survives_strings() {
    assert_eq!(jsc(&["-p", "40 + 2"]), "42");
    assert_eq!(jsc(&["-p", "let x = 40; x + 2"]), "42");
    assert_eq!(
        jsc(&["-p", "let i=0; let s=0; while(i<10){ s=s+i; i=i+1 } s"]),
        "45",
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55",
    );
    assert_eq!(jsc(&["-p", "typeof 42"]), "number");
    assert_eq!(jsc(&["-p", "typeof true"]), "boolean");
    assert_eq!(jsc(&["-p", "typeof null"]), "object");
    assert_eq!(jsc(&["-p", "null === null"]), "true");
    assert_eq!(jsc(&["-p", "let b=true; b === true ? 42 : 0"]), "42");
}

/// String programs dispatch correctly alongside arrays.
///
/// `s[i]` and `.length` share syntax with arrays but must route to the string
/// path at runtime; concatenation and strict equality behave as for strings.
#[test]
fn regression_survives_arrays() {
    assert_eq!(jsc(&["-p", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-p", r#""foo".length"#]), "3");
    assert_eq!(jsc(&["-p", r#""foo" + "bar""#]), "foobar");
    assert_eq!(jsc(&["-p", "40+2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55",
    );
    assert_eq!(jsc(&["-p", "typeof 42"]), "number");
    assert_eq!(jsc(&["-p", "null===null"]), "true");
}

/// Arrays, strings, and numbers still dispatch correctly after objects landed.
///
/// `o.a` / `o["a"]` share syntax with array/string indexing but must keep routing
/// to the object path at runtime; every earlier heap type stays intact.
#[test]
fn regression_survives_objects() {
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a[0]+a[2]"]), "4");
    assert_eq!(jsc(&["-p", "[1,2,3].length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[10,20]; a[1]=5; a[0]+a[1]"]), "15");
    assert_eq!(jsc(&["-p", "typeof []"]), "object");
    assert_eq!(
        jsc(&[
            "-p",
            "let a=[1,2,3,4]; let i=0; let s=0; while(i<a.length){ s=s+a[i]; i=i+1 } s"
        ]),
        "10",
    );
    assert_eq!(jsc(&["-p", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-p", r#""foo".length"#]), "3");
    assert_eq!(jsc(&["-p", r#""foo" + "bar""#]), "foobar");
    assert_eq!(jsc(&["-p", "40+2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55",
    );
    assert_eq!(jsc(&["-p", "typeof 42"]), "number");
    assert_eq!(jsc(&["-p", "null===null"]), "true");
}

/// The base language holds alongside nested values.
///
/// One assertion per feature area — arrays, objects (read/assign/computed),
/// strings, numbers, `fib`, `typeof`, and strict equality — so a nesting change
/// that regresses any base behavior is caught here.
#[test]
fn regression_survives_nesting() {
    // Objects: read, computed read, property assign.
    assert_eq!(jsc(&["-p", "let o={a:1,b:2}; o.a+o.b"]), "3");
    assert_eq!(jsc(&["-p", "let o={a:1}; o[\"a\"]"]), "1");
    assert_eq!(jsc(&["-p", "let o={x:10}; o.x=42; o.x"]), "42");
    // Arrays: length, element assign.
    assert_eq!(jsc(&["-p", "[1,2,3].length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[10,20]; a[1]=5; a[0]+a[1]"]), "15");
    // Heap typeof.
    assert_eq!(jsc(&["-p", "typeof []"]), "object");
    assert_eq!(jsc(&["-p", "typeof {}"]), "object");
    // Strings.
    assert_eq!(jsc(&["-p", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-p", r#""foo" + "bar""#]), "foobar");
    // Numbers, recursion, strict equality.
    assert_eq!(jsc(&["-p", "40+2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55",
    );
    assert_eq!(jsc(&["-p", "null===null"]), "true");
}

/// A regression sampling across the language: numbers, recursion, `for`-sum,
/// `switch`, block scope, `typeof`, strict equality, logical/coalescing
/// operators, compound assignment, and the string / array / object basics all
/// render their expected values.
#[test]
fn language_regression_sampling() {
    // Numbers and recursion.
    assert_eq!(jsc(&["-p", "40 + 2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55"
    );
    // Statement forms: `for`-sum, `switch` match, block scoping.
    assert_eq!(
        jsc(&["-p", "let s=0; for(let i=0;i<5;i=i+1){ s=s+i } s"]),
        "10"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let x=2; let r=0; switch(x){ case 1: r=1; break; case 2: r=42; break } r"
        ]),
        "42"
    );
    assert_eq!(jsc(&["-p", "let x=1; { let x=2; } x"]), "1");
    // Operators: `typeof`, strict equality, logical / coalescing, compound assign.
    assert_eq!(jsc(&["-p", "typeof 42"]), "number");
    assert_eq!(jsc(&["-p", "null === null"]), "true");
    assert_eq!(jsc(&["-p", "!true"]), "false");
    assert_eq!(jsc(&["-p", "0 && 1"]), "0");
    assert_eq!(jsc(&["-p", "null ?? 4"]), "4");
    assert_eq!(jsc(&["-p", "let x=5; x+=3; x"]), "8");
    assert_eq!(jsc(&["-p", "let x=10; x%=3; x"]), "1");
    // Heap basics: strings, arrays, objects (including nested read).
    assert_eq!(jsc(&["-p", r#""foo"+"bar""#]), "foobar");
    assert_eq!(jsc(&["-p", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-p", r#""foo".length"#]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a[0]+a[2]"]), "4");
    assert_eq!(jsc(&["-p", "let o={a:1,b:2}; o.a+o.b"]), "3");
    assert_eq!(jsc(&["-p", "let o={v:[7,8]}; o.v[1]"]), "8");
}

/// The built-in library matrix as one consolidated end-to-end gate through the
/// built binary. The `console.log` cases assert **stdout** byte-for-byte; the
/// value cases assert the decoded final value. Covers every built-in group:
/// `console.log`, `Math`, conversions, array growth (`push`/`pop`), the higher-order
/// callback methods (`map`/`filter`/`reduce`/`forEach`), and the array/string method
/// surface.
#[test]
fn builtins_acceptance() {
    // console.log — printed output, independent of the final-expression value.
    assert_eq!(jsc_raw_stdout(&["-e", "console.log(40 + 2)"]), "42\n");
    assert_eq!(jsc_raw_stdout(&["-e", r#"console.log("hi")"#]), "hi\n");
    assert_eq!(jsc_raw_stdout(&["-e", "console.log(1, 2, 3)"]), "1 2 3\n");
    // Math — floor / max / sqrt / abs / min.
    assert_eq!(jsc(&["-p", "Math.floor(3.7)"]), "3");
    assert_eq!(jsc(&["-p", "Math.max(1, 42)"]), "42");
    assert_eq!(jsc(&["-p", "Math.sqrt(1764)"]), "42");
    assert_eq!(jsc(&["-p", "Math.abs(-42)"]), "42");
    assert_eq!(jsc(&["-p", "Math.min(5, 2, 9)"]), "2");
    // Conversions — String / Number / parseInt / Boolean.
    assert_eq!(jsc(&["-p", "String(42)"]), "42");
    assert_eq!(jsc(&["-p", "typeof String(42)"]), "string");
    assert_eq!(jsc(&["-p", r#"Number("42")"#]), "42");
    assert_eq!(jsc(&["-p", r#"parseInt("42px")"#]), "42");
    assert_eq!(jsc(&["-p", "Boolean(0)"]), "false");
    // Array growth — push observed through the binding's length and element.
    assert_eq!(jsc(&["-p", "let a=[1,2]; a.push(3); a.length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2]; a.push(3); a[2]"]), "3");
    // Higher-order — map / filter / reduce / forEach through the callback ABI.
    assert_eq!(jsc(&["-p", "[1,2,3].map(x => x*2)[2]"]), "6");
    assert_eq!(jsc(&["-p", "[1,2,3,4].filter(x => x > 2).length"]), "2");
    assert_eq!(jsc(&["-p", "[1,2,3,4].reduce((a,b) => a+b, 0)"]), "10");
    assert_eq!(
        jsc(&["-p", "let s=0; [1,2,3,4].forEach(x => { s = s + x }); s"]),
        "10",
    );
    // Array/string methods — indexOf / join / toUpperCase / slice.
    assert_eq!(jsc(&["-p", "[10,20,30].indexOf(20)"]), "1");
    assert_eq!(jsc(&["-p", r#"[1,2,3].join("-")"#]), "1-2-3");
    assert_eq!(jsc(&["-p", r#""abc".toUpperCase()"#]), "ABC");
    assert_eq!(jsc(&["-p", r#""HELLO".slice(1,3)"#]), "EL");
}

/// The core language regression list holds through the built binary alongside
/// the built-in library: numbers, recursion, closures, statement forms, heap
/// basics, and the operator/typeof/strict-equality surface all render
/// result-for-result as expected.
#[test]
fn core_language_regression() {
    // Numbers and recursion.
    assert_eq!(jsc(&["-p", "40 + 2"]), "42");
    assert_eq!(
        jsc(&[
            "-p",
            "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)"
        ]),
        "55"
    );
    // Closures — stateful counter and a returned adder.
    assert_eq!(
        jsc(&[
            "-p",
            "function counter(){ let c=0; return function(){ c=c+1; return c } } \
             let n=counter(); n();n();n()"
        ]),
        "3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function adder(x){ return y=>x+y } let a5=adder(5); a5(37)"
        ]),
        "42"
    );
    // Statement forms — for-sum, switch match, block scope.
    assert_eq!(
        jsc(&["-p", "let s=0; for(let i=0;i<5;i=i+1){ s=s+i } s"]),
        "10"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let x=2; let r=0; switch(x){ case 1: r=1; break; case 2: r=42; break } r"
        ]),
        "42"
    );
    assert_eq!(jsc(&["-p", "let x=1; { let x=2; } x"]), "1");
    // Strings, arrays, objects.
    assert_eq!(jsc(&["-p", r#""foo" + "bar""#]), "foobar");
    assert_eq!(jsc(&["-p", r#""abc"[1]"#]), "b");
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a[0]+a[2]"]), "4");
    assert_eq!(jsc(&["-p", "let o={a:1,b:2}; o.a+o.b"]), "3");
    // typeof, strict equality.
    assert_eq!(jsc(&["-p", "typeof 42"]), "number");
    assert_eq!(jsc(&["-p", "typeof (x=>x)"]), "function");
    assert_eq!(jsc(&["-p", "null === null"]), "true");
    // const reassignment is a compile error: exit 2, names the constant.
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

/// What a nested-`yield`/`await` shape does today.
enum Nested {
    /// Rejected at compile time (exit 2) — never a panic.
    CompileError,
    /// Compiles and runs, printing this `console.log` output.
    Prints(&'static str),
}

/// A `yield`/`await` nested inside an expression shape either compiles to a
/// clean compile error or runs correctly — it never panics the compiler. Each
/// body uses `@` for the suspend keyword with operand `7`, and the driver sends
/// `7` back, so a supported shape observes `7` at the suspend point in both the
/// `function*` and the `async function` variant.
#[test]
fn nested_yield_and_await_never_panic() {
    use Nested::{CompileError, Prints};
    let cases: &[(&str, Nested)] = &[
        (
            "let o = {x:0}; o.x = (@ 7) ? 2 : 3; console.log(o.x);",
            CompileError,
        ),
        (
            "let o = {x:0}; let k = \"x\"; o[k] = @ 7; console.log(o.x);",
            CompileError,
        ),
        (
            "let a = [1,2]; let i = 1; a[i] += @ 7; console.log(a[1]);",
            CompileError,
        ),
        (
            "let o = {x:{y:0}}; o.x.y = @ 7; console.log(o.x.y);",
            CompileError,
        ),
        (
            "let o = {f: function(v){ return v + 1; }}; console.log(o.f(@ 7));",
            CompileError,
        ),
        ("let p = {v: @ 7}; console.log(p.v);", CompileError),
        ("let a = [@ 7, 1]; console.log(a[0]);", CompileError),
        ("console.log(`v=${@ 7}`);", CompileError),
        (
            "let c = true; let r = c ? (@ 7) : 0; console.log(r);",
            CompileError,
        ),
        ("let r = true && (@ 7); console.log(r);", CompileError),
        ("let r = false || (@ 7); console.log(r);", CompileError),
        ("let r = null ?? (@ 7); console.log(r);", CompileError),
        // Already-supported statement shapes keep working.
        ("let r = 0; r = @ 7; console.log(r);", Prints("7\n")),
        ("@ 7; console.log(1);", Prints("1\n")),
        (
            "let r = 0; if (true) { r = @ 7; } console.log(r);",
            Prints("7\n"),
        ),
    ];
    for (body, expected) in cases {
        let generator = format!(
            "function* g(){{ {} }} let it = g(); it.next(); it.next(7); it.next(7);",
            body.replace('@', "yield")
        );
        let async_fn = format!(
            "async function f(){{ {} }} f();",
            body.replace('@', "await")
        );
        for src in [generator, async_fn] {
            let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
                .args(["-e", &src])
                .output()
                .expect("failed to spawn jsc binary");
            let stderr = String::from_utf8_lossy(&output.stderr);
            match expected {
                CompileError => assert!(
                    output.status.code() == Some(2),
                    "`{src}` should be a compile error (exit 2), got {:?}: {stderr}",
                    output.status
                ),
                Prints(out) => {
                    assert!(
                        output.status.success(),
                        "`{src}` should run, got {:?}: {stderr}",
                        output.status
                    );
                    assert_eq!(String::from_utf8_lossy(&output.stdout), *out, "for `{src}`");
                }
            }
        }
    }
}
