//! Integration tests: S8..S12 feature regression under the S13 jsval ABI.

mod common;
use common::{assert_eval, assert_rejected};

#[test]
fn s8_s12_regression_end_to_end() {
    assert_eval("40 + 2", "42");
    assert_eval(
        "function fib(n){ if(n<2) return n; return fib(n-1)+fib(n-2) } fib(10)",
        "55",
    );
    assert_eval("let s=0; for(let i=0;i<5;i=i+1){ s=s+i } s", "10");
    assert_eval("let i=0; let s=0; while(i<10){ s=s+i; i=i+1 } s", "45");
    assert_eval(
        "let x=0; switch(2){ case 1: x=1; break; case 2: x=2; break } x",
        "2",
    );
    assert_eval("let x=0; { let x=1; } if(true){ let y=1; x=y } x", "1");
    assert_eval("typeof 42", "number");
    assert_eval("null === null", "true");
    assert_eval("\"foo\" + \"bar\"", "foobar");
    assert_eval("\"abc\"[1]", "b");
    assert_eval("\"foo\".length", "3");
    assert_eval("let a=[1,2,3]; a[0]+a[2]", "4");
    assert_eval("let o={a:1,b:2}; o.a+o.b", "3");
    assert_eval("let o={v:[7,8]}; o.v[1]", "8");
    assert_eval("!true", "false");
    assert_eval("0 && 1", "0");
    assert_eval("null ?? 4", "4");
    assert_eval("let x=5; x+=3; x", "8");
    assert_eval("let x=10; x%=3; x", "1");
    // `const` reassignment is a compile error.
    assert_rejected("const x = 1; x = 2");
}
