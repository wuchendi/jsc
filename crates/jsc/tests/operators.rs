//! Integration tests: `typeof`, literals, strict equality, logical, ternary, and (compound) assignment operators via the CLI.

mod common;
use common::jsc;

/// `typeof` over the four in-scope value types renders the JS type word.
#[test]
fn typeof_renders_type_words() {
    assert_eq!(jsc(&["-e", "typeof 42"]), "number");
    assert_eq!(jsc(&["-e", "typeof true"]), "boolean");
    assert_eq!(jsc(&["-e", "typeof undefined"]), "undefined");
    // `typeof null` is the famous JS quirk: "object".
    assert_eq!(jsc(&["-e", "typeof null"]), "object");
}

/// Boolean, `null`, and `undefined` literals render as their JS source words.
#[test]
fn literals_render_as_js_words() {
    assert_eq!(jsc(&["-e", "true"]), "true");
    assert_eq!(jsc(&["-e", "false"]), "false");
    assert_eq!(jsc(&["-e", "null"]), "null");
    assert_eq!(jsc(&["-e", "undefined"]), "undefined");
}

/// `instanceof Array`/`Function`/`Object` dispatch on the receiver's runtime
/// TYPE BYTE: `Array`/`Function` match their own heap kind, and `Object` matches
/// any heap pointer that is not a primitive string.
#[test]
fn instanceof_runtime_type_byte_builtins() {
    // `Array`: an array literal only.
    assert_eq!(jsc(&["-e", "[] instanceof Array"]), "true");
    assert_eq!(jsc(&["-e", "[1, 2] instanceof Array"]), "true");
    assert_eq!(jsc(&["-e", "({}) instanceof Array"]), "false");
    assert_eq!(jsc(&["-e", "5 instanceof Array"]), "false");
    // `Function`: a function value only.
    assert_eq!(jsc(&["-e", "(function () {}) instanceof Function"]), "true");
    assert_eq!(jsc(&["-e", "(() => 1) instanceof Function"]), "true");
    assert_eq!(jsc(&["-e", "[] instanceof Function"]), "false");
    // `Object`: any heap value except a primitive string.
    assert_eq!(jsc(&["-e", "({}) instanceof Object"]), "true");
    assert_eq!(jsc(&["-e", "[] instanceof Object"]), "true");
    assert_eq!(jsc(&["-e", "(function () {}) instanceof Object"]), "true");
    assert_eq!(jsc(&["-e", "new Error(\"x\") instanceof Object"]), "true");
    // Primitives (including a string) are never `instanceof Object`.
    assert_eq!(jsc(&["-e", "\"s\" instanceof Object"]), "false");
    assert_eq!(jsc(&["-e", "5 instanceof Object"]), "false");
    assert_eq!(jsc(&["-e", "null instanceof Object"]), "false");
    assert_eq!(jsc(&["-e", "undefined instanceof Object"]), "false");
}

/// Strict equality `===` compares by type-then-value across every jsval type.
#[test]
fn strict_equality_across_types() {
    assert_eq!(jsc(&["-e", "1===1"]), "true");
    assert_eq!(jsc(&["-e", "1===2"]), "false");
    assert_eq!(jsc(&["-e", "null===null"]), "true");
    // Different types are never strictly equal.
    assert_eq!(jsc(&["-e", "undefined===null"]), "false");
    assert_eq!(jsc(&["-e", "true===1"]), "false");
}

/// Loose equality `==`/`!=` — Abstract Equality with number/string/boolean
/// coercion, `null`/`undefined` mutual equality, and object identity fallback.
#[test]
fn loose_equality_coerces() {
    // Same type: like strict.
    assert_eq!(jsc(&["-e", "1==1"]), "true");
    assert_eq!(jsc(&["-e", "1==2"]), "false");
    // number <-> string coercion.
    assert_eq!(jsc(&["-e", "1==\"1\""]), "true");
    assert_eq!(jsc(&["-e", "\"1\"==1"]), "true");
    assert_eq!(jsc(&["-e", "\"\"==0"]), "true");
    assert_eq!(jsc(&["-e", "\"abc\"==0"]), "false");
    // boolean coercion to number.
    assert_eq!(jsc(&["-e", "true==1"]), "true");
    assert_eq!(jsc(&["-e", "true==2"]), "false");
    assert_eq!(jsc(&["-e", "false==0"]), "true");
    // null and undefined equal each other and nothing else.
    assert_eq!(jsc(&["-e", "null==undefined"]), "true");
    assert_eq!(jsc(&["-e", "null==0"]), "false");
    assert_eq!(jsc(&["-e", "null==false"]), "false");
    // NaN is never loosely equal to anything.
    assert_eq!(jsc(&["-e", "NaN==NaN"]), "false");
    // `!=` is the negation.
    assert_eq!(jsc(&["-e", "1!=2"]), "true");
    assert_eq!(jsc(&["-e", "1!=\"1\""]), "false");
    // Objects compare by identity (no ToPrimitive coercion — a documented cut).
    assert_eq!(jsc(&["-e", "let o={}; o==o"]), "true");
    assert_eq!(jsc(&["-e", "({})==({})"]), "false");
}

/// `typeof x === "<word>"` — the idiomatic JS type guard — compares by content.
/// A `typeof` result is a boxed type-string immediate, not a heap string, so a
/// naive `===` bit-compare would always be `false`; the strict-equality path
/// normalizes it to the interned word first.
#[test]
fn typeof_strict_equality_type_guard() {
    // Each of the six type words matches its own value.
    assert_eq!(jsc(&["-e", "typeof 1 === \"number\""]), "true");
    assert_eq!(jsc(&["-e", "typeof \"x\" === \"string\""]), "true");
    assert_eq!(jsc(&["-e", "typeof true === \"boolean\""]), "true");
    assert_eq!(jsc(&["-e", "typeof undefined === \"undefined\""]), "true");
    assert_eq!(jsc(&["-e", "typeof {} === \"object\""]), "true");
    assert_eq!(jsc(&["-e", "typeof [] === \"object\""]), "true");
    // Mismatches are `false`, not a bogus always-false or always-true.
    assert_eq!(jsc(&["-e", "typeof 1 === \"string\""]), "false");
    assert_eq!(jsc(&["-e", "typeof \"x\" === \"number\""]), "false");
    // The comparison is symmetric (string literal on the left).
    assert_eq!(jsc(&["-e", "\"boolean\" === typeof true"]), "true");
    // `!==` negates correctly.
    assert_eq!(jsc(&["-e", "typeof 1 !== \"string\""]), "true");
    assert_eq!(jsc(&["-e", "typeof 1 !== \"number\""]), "false");
    // `typeof` compared against another `typeof` still works (both normalize).
    assert_eq!(jsc(&["-e", "typeof 1 === typeof 2"]), "true");
    assert_eq!(jsc(&["-e", "typeof 1 === typeof \"s\""]), "false");
    // Used as a real guard driving a ternary.
    assert_eq!(
        jsc(&[
            "-e",
            "let x = 7; typeof x === \"number\" ? \"num\" : \"no\""
        ]),
        "num",
    );
}

/// A ternary driven by a `===` test selects the right branch end to end.
#[test]
fn ternary_selects_branch() {
    assert_eq!(
        jsc(&["-e", "let b = true; let n = b === true ? 42 : 0; n"]),
        "42",
    );
}

/// S12.1 logical operators (`!`, `&&`, `||`, `??`) render correctly end to end,
/// with JS short-circuit + value semantics.
#[test]
fn logical_operators_via_eval() {
    // `!` yields a boolean inverse of truthiness.
    assert_eq!(jsc(&["-e", "!true"]), "false");
    assert_eq!(jsc(&["-e", "!0"]), "true");
    assert_eq!(jsc(&["-e", "!\"\""]), "true");
    assert_eq!(jsc(&["-e", "!\"x\""]), "false");
    // `&&`/`||` yield the operand value (not a coerced boolean).
    assert_eq!(jsc(&["-e", "1 && 2"]), "2");
    assert_eq!(jsc(&["-e", "0 && 2"]), "0");
    assert_eq!(jsc(&["-e", "0 || 3"]), "3");
    assert_eq!(jsc(&["-e", "4 || 5"]), "4");
    // `??` triggers the rhs only on `null`/`undefined`, not on `0`/`""`.
    assert_eq!(jsc(&["-e", "null ?? 5"]), "5");
    assert_eq!(jsc(&["-e", "undefined ?? 5"]), "5");
    assert_eq!(jsc(&["-e", "4 ?? 5"]), "4");
    assert_eq!(jsc(&["-e", "0 ?? 5"]), "0");
    // Short-circuit: the skipped rhs side effect never runs.
    assert_eq!(jsc(&["-e", "let x=0; false && (x = 1); x"]), "0");
    assert_eq!(jsc(&["-e", "let y=0; true || (y = 1); y"]), "0");
}

/// S12.2 — compound assignment `+= -= *= /= %=` end to end via the CLI.
#[test]
fn compound_assignment_via_eval() {
    assert_eq!(jsc(&["-e", "let x=40; x += 2; x"]), "42");
    assert_eq!(jsc(&["-e", "let y=10; y *= 4; y"]), "40");
    assert_eq!(jsc(&["-e", "let z=10; z -= 3; z"]), "7");
    assert_eq!(jsc(&["-e", "let w=17; w %= 5; w"]), "2");
    assert_eq!(jsc(&["-e", "let q=20; q /= 4; q"]), "5");
    // `+=` concatenates when the lhs is a string.
    assert_eq!(jsc(&["-e", r#"let s="foo"; s += "bar"; s"#]), "foobar");
}

/// S12.2 — `++`/`--` in prefix and postfix forms end to end via the CLI.
#[test]
fn increment_decrement_via_eval() {
    assert_eq!(jsc(&["-e", "let x=41; x++; x"]), "42");
    assert_eq!(jsc(&["-e", "let i=5; --i"]), "4");
    // Postfix yields the old value; prefix yields the new value.
    assert_eq!(jsc(&["-e", "let a=5; let b=a++; b"]), "5");
    assert_eq!(jsc(&["-e", "let c=5; let d=++c; d"]), "6");
    // Standalone `%` operator.
    assert_eq!(jsc(&["-e", "17 % 5"]), "2");
}

/// S13.5 — compound assignment on member targets (`a[i] op= e`, `o.k op= e`,
/// `o[k] op= e`) end to end through the built binary.
#[test]
fn member_compound_assignment_via_eval() {
    // Array element and object-member targets for the milestone cases.
    assert_eq!(jsc(&["-e", "let a=[10,20]; a[0]+=5; a[0]"]), "15");
    assert_eq!(jsc(&["-e", "let o={x:1}; o.x+=41; o.x"]), "42");
    assert_eq!(jsc(&["-e", r#"let o={n:100}; o["n"]-=58; o.n"#]), "42");
    assert_eq!(jsc(&["-e", "let a=[3]; a[0]*=14; a[0]"]), "42");
    // `%=` and `/=` on members, plus string `+=` concatenation.
    assert_eq!(jsc(&["-e", "let o={v:17}; o.v%=5; o.v"]), "2");
    assert_eq!(
        jsc(&["-e", r#"let o={s:"foo"}; o.s+="bar"; o.s"#]),
        "foobar"
    );
    // Base and index evaluated exactly once (postfix index runs once).
    assert_eq!(
        jsc(&[
            "-e",
            "let a=[10,20]; let i=0; a[i++]+=5; a[0]*100+a[1]*10+i"
        ]),
        "1701"
    );
}

/// P0.2 — exponentiation `**` (lowered to the `Math.pow` power op) and its
/// compound form `**=`, end to end through the built binary.
#[test]
fn exponentiation_via_eval() {
    // Integer exponent is exact.
    assert_eq!(jsc(&["-e", "2 ** 10"]), "1024");
    // Fractional exponent: `2 ** 0.5` is `sqrt(2)`.
    assert_eq!(jsc(&["-e", "2 ** 0.5"]), "1.4142135623730951");
    // `**=` reads-modifies-writes like other compound assignments.
    assert_eq!(jsc(&["-e", "let x=3; x **= 2; x"]), "9");
    // `**` is right-associative: `2 ** 3 ** 2` is `2 ** (3 ** 2)` = `2 ** 9`.
    assert_eq!(jsc(&["-e", "2 ** 3 ** 2"]), "512");
    // A negative exponent reciprocates; a negative base with a fractional
    // exponent is `NaN`, matching JS.
    assert_eq!(jsc(&["-e", "2 ** -2"]), "0.25");
    assert_eq!(jsc(&["-e", "(-2) ** 0.5"]), "NaN");
}

/// P0.4 — logical assignment `??=` / `||=` / `&&=`, end to end. Each desugars to
/// a conditional assign reusing the short-circuit `??`/`||`/`&&` semantics, so
/// the RHS is evaluated only when the assign actually happens.
#[test]
fn logical_assignment_via_eval() {
    // `??=` assigns only when the target is null/undefined (milestone 3); an
    // uninitialized `let x;` starts `undefined`.
    assert_eq!(jsc(&["-e", "let x; x ??= 42; x"]), "42");
    assert_eq!(jsc(&["-e", "let x=5; x ??= 9; x"]), "5");
    // `||=` assigns only when the target is falsy (milestones 4 and 5).
    assert_eq!(jsc(&["-e", "let x=1; x ||= 9; x"]), "1");
    assert_eq!(jsc(&["-e", "let x=0; x ||= 9; x"]), "9");
    // `&&=` assigns only when the target is truthy (milestone 6).
    assert_eq!(jsc(&["-e", "let x=5; x &&= 7; x"]), "7");
    assert_eq!(jsc(&["-e", "let x=0; x &&= 7; x"]), "0");
    // The RHS is NOT evaluated when the assign is skipped: a truthy `||=` target
    // leaves the side-effecting right operand (`y=5`) unrun, so `y` stays `0`.
    assert_eq!(jsc(&["-e", "let x=1; let y=0; x ||= (y=5); y"]), "0");
}
