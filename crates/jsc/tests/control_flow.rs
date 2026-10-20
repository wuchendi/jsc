//! Integration tests: `for` / `switch` / block-scoping statement forms via the CLI.

mod common;
use common::jsc;

/// C-style `for (init; cond; update) body` end to end via the CLI.
#[test]
fn c_style_for_via_eval() {
    // Brace body, `for`-`let` binding.
    assert_eq!(
        jsc(&["-p", "let s=0; for (let i=0;i<5;i=i+1) { s = s + i } s"]),
        "10"
    );
    // Single-statement (no-brace) body.
    assert_eq!(
        jsc(&["-p", "let s=0; for (let i=0;i<5;i=i+1) s = s + i; s"]),
        "10"
    );
    // Empty init and empty update sections.
    assert_eq!(
        jsc(&["-p", "let i=0; let s=0; for(;i<3;){ s=s+1; i=i+1 } s"]),
        "3"
    );
}

/// `switch`/`case`/`default`/`break` with JS fall-through, end to end.
#[test]
fn switch_case_via_eval() {
    // Matching case with `break`.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=2; let r=0; switch(x){ case 1: r=10; break; case 2: r=42; break; default: r=99 } r"
        ]),
        "42"
    );
    // `default` runs when no case matches.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=9; let r=0; switch(x){ case 1: r=10; break; default: r=99 } r"
        ]),
        "99"
    );
    // Empty case falls through to the next case body.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=1; let r=0; switch(x){ case 1: case 2: r=42; break; default: r=0 } r"
        ]),
        "42"
    );
    // A case without `break` falls through into the following case body.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=1; let r=0; switch(x){ case 1: r=1; case 2: r=r+40; break; default: } r"
        ]),
        "41"
    );
    // `default` in the middle falls through into the following case (source order).
    assert_eq!(
        jsc(&[
            "-p",
            "let x=5; let r=0; switch(x){ case 1: r=1; break; default: r=r+3; case 2: r=r+4; break } r"
        ]),
        "7"
    );
    // No match and no `default`: the switch is a no-op.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=7; let r=0; switch(x){ case 1: r=1; break; case 2: r=2; break } r"
        ]),
        "0"
    );
    // `break` nested inside an `if` still exits the switch.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=1; let r=0; switch(x){ case 1: if(x>0){ r=42; break } r=5; break; default: r=9 } r"
        ]),
        "42"
    );
    // Strict-equality matching on a string discriminant.
    assert_eq!(
        jsc(&[
            "-p",
            "let x=\"b\"; let r=0; switch(x){ case \"a\": r=1; break; case \"b\": r=2; break } r"
        ]),
        "2"
    );
}

/// `for (const v of ITER) body` over arrays and strings, end to end.
#[test]
fn for_of_via_eval() {
    // Iterate array elements, accumulating their sum.
    assert_eq!(
        jsc(&["-p", "let s=0; for (const v of [10,20,30]) { s = s + v } s"]),
        "60"
    );
    // Iterate a string's characters, rebuilding it.
    assert_eq!(
        jsc(&["-p", r#"let r=""; for (const c of "abc") { r = r + c } r"#]),
        "abc"
    );
    // `break` exits the loop early.
    assert_eq!(
        jsc(&[
            "-p",
            "let s=0; for (const v of [1,2,3,4]) { if (v===3) break; s=s+v } s"
        ]),
        "3"
    );
    // `continue` skips an iteration but still advances the index.
    assert_eq!(
        jsc(&[
            "-p",
            "let s=0; for (const v of [1,2,3,4]) { if (v===2) continue; s=s+v } s"
        ]),
        "8"
    );
    // Single-statement (no-brace) body, and a `let` binding.
    assert_eq!(
        jsc(&["-p", "let s=0; for (let v of [1,2,3]) s = s + v; s"]),
        "6"
    );
    // The loop binding is scoped to the loop and does not leak past it.
    assert_eq!(
        jsc(&[
            "-p",
            "let last=0; for (const v of [7,8,9]) { last = v } last"
        ]),
        "9"
    );
}

/// Lexical block scoping (`{}` pushes a scope; `for`-let block-scoped),
/// end to end via the CLI. An inner `let` shadows an outer one and does not leak.
#[test]
fn block_scoping_via_eval() {
    // Inner `let x` shadows the outer; the outer binding survives the block.
    assert_eq!(jsc(&["-p", "let x=1; { let x=2; } x"]), "1");
    // The inner block sees its own binding while it is in scope.
    assert_eq!(jsc(&["-p", "let x=1; let r=0; { let x=2; r=x } r"]), "2");
    // Sibling blocks reuse a name for distinct bindings.
    assert_eq!(
        jsc(&["-p", "let r=0; { let x=1; r=r+x } { let x=2; r=r+x } r"]),
        "3"
    );
    // A `for`-let binding is scoped to the loop and does not leak past it.
    assert_eq!(
        jsc(&["-p", "let s=0; for (let i=0;i<3;i=i+1) { s = s + i } s"]),
        "3"
    );
}

/// `do { .. } while (cond)` end to end via the CLI. The body always runs
/// once before the condition is tested, and a `continue` jumps to the condition.
#[test]
fn do_while_via_eval() {
    // Body runs while the condition holds, accumulating across iterations.
    assert_eq!(
        jsc(&["-p", "let i=0,s=0; do { s=s+i; i=i+1 } while (i<3); s"]),
        "3"
    );
    // The body runs at least once even when the condition is immediately false.
    assert_eq!(jsc(&["-p", "let n=0; do { n=n+1 } while (false); n"]), "1");
    // `continue` in a `do`-`while` jumps to the condition test (re-evaluated),
    // still terminating the loop.
    assert_eq!(
        jsc(&[
            "-p",
            "let i=0,s=0; do { i=i+1; if (i===2) continue; s=s+i } while (i<4); s"
        ]),
        "8"
    );
    // `break` exits the `do`-`while` immediately.
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0; do { n=n+1; if (n===2) break } while (true); n"
        ]),
        "2"
    );
}

/// Labeled `break`/`continue` end to end via the CLI. A label names its
/// immediately-nested loop; `break LABEL`/`continue LABEL` from inside a nested
/// loop target that OUTER loop.
#[test]
fn labeled_break_continue_via_eval() {
    // `break LABEL` from an inner loop exits the labeled outer loop entirely.
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0; outer: for(let i=0;i<3;i=i+1){ for(let j=0;j<3;j=j+1){ if(j===1) break outer; n=n+1 } } n"
        ]),
        "1"
    );
    // `continue LABEL` skips to the labeled outer loop's next iteration.
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0; outer: for(let i=0;i<3;i=i+1){ for(let j=0;j<3;j=j+1){ if(j===1) continue outer; n=n+1 } } n"
        ]),
        "3"
    );
    // A labeled `break` targeting the loop it sits directly in behaves like a
    // plain `break`.
    assert_eq!(
        jsc(&[
            "-p",
            "let s=0; L: for(let i=0;i<10;i=i+1){ if(i===5) break L; s=s+1 } s"
        ]),
        "5"
    );
    // A label may name a `do`-`while`, and `break LABEL` exits it.
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0; L: do { n=n+1; if(n===3) break L } while(true); n"
        ]),
        "3"
    );
    // `break LABEL` still runs an intervening `finally` on the way out.
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0,f=0; outer: for(let i=0;i<3;i=i+1){ for(let j=0;j<3;j=j+1){ try { if(j===1) break outer; n=n+1 } finally { f=f+1 } } } n*10+f"
        ]),
        "12"
    );
}

/// A `yield` inside a `for-of` body and inside a `switch` case, end to end.
#[test]
fn generator_yield_in_for_of_and_switch_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "function* g(xs){ for (const x of xs) { yield x * 2 } } [...g([1,2,3])]"
        ]),
        "[ 2, 4, 6 ]"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "function* g(x){ switch (x) { case 1: yield \"a\"; case 2: yield \"b\"; break; default: yield \"c\" } } [...g(1)].join(\"\")"
        ]),
        "ab"
    );
}

/// A `yield` inside `try`/`catch`/`finally`, end to end.
#[test]
fn generator_yield_in_try_catch_finally_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "function* g(){ try { yield 1; throw new Error(\"x\") } catch (e) { yield e.message } finally { yield \"f\" } } [...g()].join()"
        ]),
        "1,x,f"
    );
}
