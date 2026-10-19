//! Integration tests: array literals, element assignment, and array methods via the CLI.

mod common;
use common::jsc;

/// Array basics run inline with `-p` through the built binary.
///
/// Covers arrays end to end: element read + sum, `.length`, `typeof`,
/// a `while`-driven sum over `.length`, and the empty array.
#[test]
fn array_basics_via_eval() {
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a[0]+a[2]"]), "4");
    assert_eq!(jsc(&["-p", "[1,2,3].length"]), "3");
    assert_eq!(jsc(&["-p", "typeof []"]), "object");
    assert_eq!(
        jsc(&[
            "-p",
            "let a=[1,2,3,4]; let i=0; let s=0; while(i<a.length){ s=s+a[i]; i=i+1 } s"
        ]),
        "10",
    );
    assert_eq!(jsc(&["-p", "let a=[]; a.length"]), "0");
}

/// `arr[i] = v` stores into an existing slot and the assignment
/// expression evaluates to the stored value.
#[test]
fn array_element_assignment_via_eval() {
    assert_eq!(jsc(&["-p", "let a=[10,20]; a[1]=5; a[0]+a[1]"]), "15");
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a[0]=9; a[0]"]), "9");
    assert_eq!(jsc(&["-p", "let a=[0]; let x=(a[0]=7); x"]), "7");
}

/// Wrap `body` so the script yields `"<instanceof TypeError>|<message>"` for the
/// error it throws (or `"none"` when it completes normally).
fn caught(body: &str) -> String {
    let src = format!(
        "let r=\"none\"; try {{ {body} }} catch(e) {{ r=(e instanceof TypeError)+\"|\"+e.message }} r"
    );
    jsc(&["-p", &src])
}

/// Arrays do not grow by index assignment yet: an integer index at or past the
/// length throws, any other key (non-number, negative, fractional, NaN) is an
/// absent property and throws the add-property error. Nothing is written and
/// in-bounds stores are unaffected.
#[test]
fn array_out_of_bounds_and_non_index_writes_throw() {
    let grow = "true|Cannot grow array by index assignment yet";
    assert_eq!(caught("let a = [1, 2]; a[2] = 3"), grow);
    assert_eq!(caught("let a = [1, 2]; a[100] = 3"), grow);
    assert_eq!(caught("let a = []; a[0] = 1"), grow);
    assert_eq!(caught("let a = [1, 2, 3]; a.pop(); a[2] = 9"), grow);
    let add = |key: &str| {
        format!("true|Cannot add property '{key}': dynamic properties are not supported yet")
    };
    assert_eq!(caught("let a = [1]; a.foo = 1"), add("foo"));
    assert_eq!(caught("let a = [1]; a[\"foo\"] = 1"), add("foo"));
    assert_eq!(caught("let a = [1]; a[-1] = 1"), add("-1"));
    assert_eq!(caught("let a = [1, 2]; a[0.5] = 1"), add("0.5"));
    assert_eq!(caught("let a = [1]; a[NaN] = 1"), add("NaN"));
    // A rejected write leaves every element and the length untouched.
    assert_eq!(
        jsc(&[
            "-p",
            "let a=[1,2]; try { a[2]=3 } catch(e) {} try { a[-1]=7 } catch(e) {} a"
        ]),
        "[ 1, 2 ]"
    );
    // In-bounds stores (including via compound/update forms) are unchanged.
    assert_eq!(jsc(&["-p", "let a=[1,2]; a[1]+=5; a[0]++; a"]), "[ 2, 7 ]");
    assert_eq!(jsc(&["-p", "let a=[1,2]; let i=1; a[i]=4; a[1]"]), "4");
}

/// `Array.prototype.push`/`pop` grow and shrink a bound array end-to-end
/// through the CLI: the binding observes the new length and elements after growth,
/// and `pop` returns the removed element while decrementing the length in place.
#[test]
fn array_push_pop_via_eval() {
    assert_eq!(jsc(&["-p", "let a=[1,2]; a.push(3); a.length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2]; a.push(3); a[2]"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a.pop()"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2,3]; a.pop(); a.length"]), "2");
    assert_eq!(jsc(&["-p", "let a=[]; a.push(7); a[0]"]), "7");
    assert_eq!(jsc(&["-p", "let a=[]; a.push(7); a.length"]), "1");
    // push returns the new length.
    assert_eq!(jsc(&["-p", "let a=[1]; a.push(2)"]), "2");
    // pop on an empty array is undefined.
    assert_eq!(jsc(&["-p", "let a=[]; a.pop()"]), "undefined");
    // A grown array prints Node-console style with the appended element.
    assert_eq!(jsc(&["-p", "let a=[1,2]; a.push(3); a"]), "[ 1, 2, 3 ]");
}

/// Array higher-order methods `map`/`filter`/`reduce`/`forEach` end-to-end
/// through the CLI: callbacks invoke per element via the `call_indirect` ABI,
/// closures capturing an outer binding mutate it, and results chain.
#[test]
fn array_higher_order_via_eval() {
    assert_eq!(jsc(&["-p", "[1,2,3].map(x => x*2)[2]"]), "6");
    assert_eq!(jsc(&["-p", "[1,2,3,4].filter(x => x > 2).length"]), "2");
    assert_eq!(jsc(&["-p", "[1,2,3,4].reduce((a,b) => a+b, 0)"]), "10");
    assert_eq!(
        jsc(&["-p", "let s=0; [1,2,3,4].forEach(x => { s = s + x }); s"]),
        "10",
    );
    assert_eq!(jsc(&["-p", "[1,2,3].map(x => x+1).length"]), "3");
    // The transformed array prints Node-console style.
    assert_eq!(jsc(&["-p", "[1,2,3].map(x => x*2)"]), "[ 2, 4, 6 ]");
    // Chaining: map result feeds filter.
    assert_eq!(
        jsc(&["-p", "[1,2,3,4].map(x => x*2).filter(x => x > 4).length"]),
        "2",
    );
    // Bound-variable receiver, closure-capturing callback.
    assert_eq!(jsc(&["-p", "let k=10; [1,2,3].map(x => x + k)[0]"]), "11");
    // forEach evaluates to undefined.
    assert_eq!(jsc(&["-p", "[1,2,3].forEach(x => x)"]), "undefined");
}

/// Non-callback array/string methods end-to-end through the CLI:
/// `indexOf`/`includes`/`join`/`slice` on arrays and
/// `toUpperCase`/`toLowerCase`/`indexOf`/`slice`/`charAt` on strings, with the
/// overloaded `indexOf`/`slice` dispatched on the receiver's runtime type.
#[test]
fn array_and_string_methods_via_eval() {
    assert_eq!(jsc(&["-p", "[10,20,30].indexOf(20)"]), "1");
    assert_eq!(jsc(&["-p", "[10,20,30].indexOf(99)"]), "-1");
    assert_eq!(jsc(&["-p", "[1,2,3].includes(2)"]), "true");
    assert_eq!(jsc(&["-p", "[1,2,3].includes(9)"]), "false");
    assert_eq!(jsc(&["-p", "[1,2,3].join(\"-\")"]), "1-2-3");
    assert_eq!(jsc(&["-p", "[1,2,3].join()"]), "1,2,3");
    assert_eq!(jsc(&["-p", "[1,2,3,4].slice(1,3)"]), "[ 2, 3 ]");
    assert_eq!(jsc(&["-p", "[1,2,3,4].slice(1,3).length"]), "2");
    assert_eq!(jsc(&["-p", "\"abc\".toUpperCase()"]), "ABC");
    assert_eq!(jsc(&["-p", "\"ABC\".toLowerCase()"]), "abc");
    assert_eq!(jsc(&["-p", "\"HELLO\".slice(1,3)"]), "EL");
    assert_eq!(jsc(&["-p", "\"hello\".indexOf(\"ll\")"]), "2");
    assert_eq!(jsc(&["-p", "\"hello\".indexOf(\"z\")"]), "-1");
    assert_eq!(jsc(&["-p", "\"abc\".charAt(1)"]), "b");
    // Chaining across array + string methods.
    assert_eq!(jsc(&["-p", "[1,2,3,4].slice(1,3).join(\"-\")"]), "2-3");
}

/// Spread `...` in array literals through the CLI: a spread source's
/// elements are appended at runtime, interleaved with plain elements, so the
/// result length and indexing are only known after the build.
#[test]
fn array_spread_via_eval() {
    assert_eq!(jsc(&["-p", "let a=[1,2]; [...a,3].length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2],b=[3]; [...a,...b][2]"]), "3");
    assert_eq!(jsc(&["-p", "[0,...[1,2],9][3]"]), "9");
    // A leading/trailing plain element around a spread, and a bare spread.
    assert_eq!(jsc(&["-p", "[0,...[1,2],9].length"]), "4");
    assert_eq!(jsc(&["-p", "let a=[5,6,7]; [...a].length"]), "3");
    assert_eq!(jsc(&["-p", "let a=[1,2],b=[3,4]; [...a,...b].length"]), "4");
    // Spread feeds an ordinary array operation.
    assert_eq!(jsc(&["-p", "[0,...[1,2],9][0]"]), "0");
    assert_eq!(jsc(&["-p", "let a=[1,2]; [...a,3,...a].length"]), "5");
    // Math.max over a spread argument (variadic built-in).
    assert_eq!(jsc(&["-p", "Math.max(...[1,9,3])"]), "9");
    assert_eq!(jsc(&["-p", "Math.min(...[4,2,8])"]), "2");
    assert_eq!(jsc(&["-p", "Math.max(1,...[9,3],7)"]), "9");
}

/// Spreading a STRING into an array literal explodes it into its characters
/// (one-byte heap strings) rather than reading the string's UTF-8 bytes as
/// jsvals. The character walk is byte-indexed, matching
/// for-of / indexing over a string.
#[test]
fn string_spread_explodes_chars() {
    assert_eq!(jsc(&["-p", "[...\"abc\"].length"]), "3");
    assert_eq!(jsc(&["-p", "[...\"abc\"][0]"]), "a");
    assert_eq!(jsc(&["-p", "[...\"abc\"][2]"]), "c");
    assert_eq!(jsc(&["-p", "[...\"\"].length"]), "0");
    // Interleaved with plain elements and an array spread.
    assert_eq!(jsc(&["-p", "let a=[1,2]; [0,...a,...\"xy\"].length"]), "5");
    assert_eq!(jsc(&["-p", "[0,...\"xy\",9][2]"]), "y");
    // A spread string feeds an ordinary array operation.
    assert_eq!(jsc(&["-p", "[...\"hi\"].join(\"-\")"]), "h-i");
}

/// Regression: an element WRITE with a computed RHS, and a computed-index
/// READ, must not clobber the stashed receiver. Both the RHS and the index
/// sub-expression are emitted with the child scratch base; the receiver/key
/// live in dedicated high slots so they survive the child evaluation.
#[test]
fn index_computed_rhs_and_index_no_clobber() {
    // Computed RHS on an element store.
    assert_eq!(jsc(&["-p", "let a=[0,0]; a[1]=3+4; a[1]"]), "7");
    // Computed index on a READ.
    assert_eq!(jsc(&["-p", "let a=[10,20]; let i=0; a[i+1]"]), "20");
}
