//! Integration tests: array and object destructuring in `let`/`const`/`var`
//! declarations (P1.1/P1.2), exercised end to end through the built `jsc` binary.

mod common;
use common::jsc;

/// P1.1 milestones: array destructuring in declarations desugars to indexed
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

/// P1.2 milestones: object destructuring in declarations desugars to keyed reads
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
