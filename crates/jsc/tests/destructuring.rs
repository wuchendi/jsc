//! Integration tests: array and object destructuring in `let`/`const`/`var`
//! declarations, exercised end to end through the built `jsc` binary.

mod common;
use common::jsc;

/// Array destructuring in declarations desugars to indexed
/// reads off the initializer — plain elements, holes, defaults, rest, `const`,
/// and nested patterns.
#[test]
fn array_destructuring_declarations_via_eval() {
    // Plain two-element bind.
    assert_eq!(jsc(&["-p", "let [a,b] = [1,2]; a+b"]), "3");
    // A hole skips its index.
    assert_eq!(jsc(&["-p", "let [,b] = [1,2]; b"]), "2");
    // A default fills a missing (undefined) slot.
    assert_eq!(jsc(&["-p", "let [a=9] = []; a"]), "9");
    // A rest collects the remaining elements into a new array.
    assert_eq!(jsc(&["-p", "let [x, ...r] = [1,2,3]; r.length"]), "2");
    // `const` bindings destructure the same way.
    assert_eq!(jsc(&["-p", "const [a,b]=[3,4]; a+b"]), "7");
    // A nested pattern recurses into a sub-array.
    assert_eq!(jsc(&["-p", "let [[a]] = [[5]]; a"]), "5");
}

/// Object destructuring in declarations desugars to keyed reads
/// off the initializer — shorthand binds, renamed binds, defaults, a rest that
/// collects the remaining own keys into a new object, and nested patterns.
#[test]
fn object_destructuring_declarations_via_eval() {
    // Shorthand two-key bind.
    assert_eq!(jsc(&["-p", "let {x,y} = {x:40,y:2}; x+y"]), "42");
    // A renamed bind (`x: a`) binds the value under the new name.
    assert_eq!(jsc(&["-p", "let {x:a} = {x:5}; a"]), "5");
    // A default fills a missing (undefined) key.
    assert_eq!(jsc(&["-p", "let {z=7} = {}; z"]), "7");
    // A rest collects every remaining own key into a new object.
    assert_eq!(
        jsc(&["-p", "let {a, ...rest} = {a:1,b:2,c:3}; rest.b"]),
        "2"
    );
    // The rest object excludes the destructured keys.
    assert_eq!(
        jsc(&["-p", "let {a, ...rest} = {a:1,b:2,c:3}; rest.a"]),
        "undefined"
    );
    // A nested object pattern recurses into a sub-object.
    assert_eq!(jsc(&["-p", "let {p:{q}} = {p:{q:9}}; q"]), "9");
}

/// Destructuring ASSIGNMENT to member-expression targets (`o.p`, `o[k]`,
/// `a[i]`) in object and array patterns, with defaults, nesting and rest.
#[test]
fn destructuring_assignment_to_member_targets() {
    assert_eq!(
        jsc(&[
            "-e",
            "const o={q:0,a:0,b:0,x:0}; ({a: o.q} = {a: 1}); [o.a, o.b] = [1, 2]; \
             for ({x: o.x} of [{x:5}]); console.log(o.q,o.a,o.b,o.x)",
        ]),
        "1 1 2 5"
    );
    // The RHS is evaluated before any target is written.
    assert_eq!(
        jsc(&[
            "-e",
            "const a=[0,0]; [a[1], a[0]] = [a[0]+1, 2]; console.log(a)"
        ]),
        "[ 2, 1 ]"
    );
    // A default fills an `undefined` source property.
    assert_eq!(
        jsc(&["-e", "const o={q:0}; ({q: o.q = 9} = {}); console.log(o.q)"]),
        "9"
    );
    // Computed keys, `this`-style receivers inside methods, nesting and rest.
    assert_eq!(
        jsc(&[
            "-e",
            "const o={p:0,r:0}; const k='p'; [o[k], ...o.r] = [1, 2, 3]; console.log(o.p, o.r)",
        ]),
        "1 [ 2, 3 ]"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={a:0,b:0}; ({x: [o.a, {y: o.b}]} = {x: [3, {y: 4}]}); console.log(o.a + o.b)",
        ]),
        "7"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={v:0, set(p){ [this.v] = p; }}; o.set([8]); console.log(o.v)",
        ]),
        "8"
    );
    // Identifier and member targets mix in one pattern.
    assert_eq!(
        jsc(&[
            "-e",
            "let x=0; const o={y:0}; ({x, y: o.y} = {x:1, y:2}); console.log(x, o.y)"
        ]),
        "1 2"
    );
}

/// Each member target's reference (object, then key) is evaluated before the
/// value is fetched from the source, left to right.
#[test]
fn destructuring_assignment_member_target_evaluation_order() {
    assert_eq!(
        jsc(&[
            "-e",
            "let log=''; const o={a:0,b:0}; \
             function t(n){ log += n; return o; } function k(n){ log += n; return n; } \
             [t('1')[k('a')], t('2')[k('b')]] = [1, 2]; console.log(log, o.a, o.b)",
        ]),
        "1a2b 1 2"
    );
    // The source property getter runs after the target reference.
    assert_eq!(
        jsc(&[
            "-e",
            "let log=''; const o={a:0}; class S { get a(){ log += 'G'; return 5; } } \
             function t(){ log += 'T'; return o; } ({a: t().a} = new S()); console.log(log, o.a)",
        ]),
        "TG 5"
    );
}

/// A bare member for-of head assigns each element to the member.
#[test]
fn for_of_member_and_pattern_assignment_heads() {
    assert_eq!(
        jsc(&[
            "-e",
            "const o={x:0}; for (o.x of [1,2,3]); console.log(o.x)"
        ]),
        "3"
    );
    assert_eq!(
        jsc(&[
            "-e",
            "const o={a:0,b:0}; let s=0; for ([o.a, o.b] of [[1,2],[3,4]]) s += o.a*o.b; \
             console.log(s, o.a, o.b)",
        ]),
        "14 3 4"
    );
}

/// A destructuring assignment's value is its right-hand side.
#[test]
fn destructuring_assignment_completion_is_rhs() {
    assert_eq!(jsc(&["-p", "const o={a:0}; [o.a] = [4]"]), "[ 4 ]");
}

/// Writing an unknown property on a fixed-shape object still throws.
#[test]
fn destructuring_assignment_unknown_member_throws() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "const o={a:0}; [o.zz] = [1];"])
        .output()
        .expect("failed to spawn jsc binary");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("TypeError"));
}
