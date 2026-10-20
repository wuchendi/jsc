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

/// `a[i] = v` past the length grows the array densely (within
/// `max(2 * cap, len + 1024)`), filling the gap with holes; compound and
/// update forms read `undefined` first. A gap beyond the dense bound throws
/// until sparse arrays land, leaving the array untouched.
#[test]
fn array_grows_by_index_assignment() {
    assert_eq!(jsc(&["-p", "let a = []; a[0] = 1; a"]), "[ 1 ]");
    assert_eq!(jsc(&["-p", "let a = [1, 2]; a[2] = 3; a"]), "[ 1, 2, 3 ]");
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1]; a[3] = 4; console.log(a, a.length, a[2])"
        ]),
        "[ 1, <2 empty items>, 4 ] 4 undefined"
    );
    assert_eq!(
        jsc(&["-p", "let a = [1, 2, 3]; a.pop(); a[2] = 9; a"]),
        "[ 1, 2, 9 ]"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let a = []; for (let i = 0; i < 100; i++) a[i] = i; a.length + a[99]"
        ]),
        "199"
    );
    assert_eq!(jsc(&["-p", "let a = []; a[1023] = 1; a.length"]), "1024");
    // A canonical index string grows like the number.
    assert_eq!(
        jsc(&["-p", "let a = [1]; a[\"2\"] = 5; a"]),
        "[ 1, <1 empty item>, 5 ]"
    );
    // Compound and update forms past the end: `undefined` -> NaN, then grow.
    assert_eq!(
        jsc(&["-p", "let a = [1]; a[2] += 1; a"]),
        "[ 1, <1 empty item>, NaN ]"
    );
    assert_eq!(
        jsc(&["-p", "let a = []; a[1]++; a"]),
        "[ <1 empty item>, NaN ]"
    );
    // Every alias sees the growth (the handle never moves).
    assert_eq!(
        jsc(&["-p", "let a = [1]; let b = a; a[4] = 5; b.length"]),
        "5"
    );
    // Beyond the dense bound: TypeError, nothing written.
    let sparse = "true|Cannot grow array sparsely by index assignment yet";
    assert_eq!(caught("let a = []; a[1024] = 1"), sparse);
    assert_eq!(caught("let a = [1, 2]; a[5000] = 3"), sparse);
    assert_eq!(
        jsc(&["-p", "let a = [1, 2]; try { a[5000] = 3 } catch (e) {} a"]),
        "[ 1, 2 ]"
    );
    // In-bounds stores (including compound/update forms) are unchanged.
    assert_eq!(jsc(&["-p", "let a=[1,2]; a[1]+=5; a[0]++; a"]), "[ 2, 7 ]");
    assert_eq!(jsc(&["-p", "let a=[1,2]; let i=1; a[i]=4; a[1]"]), "4");
}

/// DF9: on an array, number keys that are array indices and canonical index
/// strings take the element path; every other key is an ordinary property in
/// the array's own table.
#[test]
fn array_key_routing() {
    assert_eq!(jsc(&["-p", "[7, 8][\"1\"]"]), "8");
    assert_eq!(jsc(&["-p", "let k = \"1\"; [7, 8][k]"]), "8");
    assert_eq!(jsc(&["-p", "let k = \"\" + 1; [7, 8][k]"]), "8");
    assert_eq!(jsc(&["-p", "let a = [5]; a.foo"]), "undefined");
    assert_eq!(jsc(&["-p", "let a = [5]; a.foo = 1; a.foo"]), "1");
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [5]; a.foo = 1; console.log(a[0], a.length, a[\"foo\"])"
        ]),
        "5 1 1"
    );
    // Non-index keys never alias an element.
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1, 2]; a[\"01\"] = 7; a[-1] = 8; a[1.5] = 9; a[NaN] = 10; \
             console.log(a[\"01\"], a[-1], a[1.5], a[NaN], a[1], a.length)"
        ]),
        "7 8 9 10 2 2"
    );
    assert_eq!(jsc(&["-p", "let a = [1]; a[\"01\"]"]), "undefined");
    // Named properties render after the elements, Node style.
    assert_eq!(
        jsc(&["-p", "let a = [1, 2]; a.foo = 1; a"]),
        "[ 1, 2, foo: 1 ]"
    );
    assert_eq!(jsc(&["-p", "let a = []; a.foo = 1; a"]), "[ foo: 1 ]");
    // `length` writes are not supported yet.
    assert_eq!(
        caught("let a = [1]; a[\"length\"] = 3"),
        "true|Cannot set array property 'length' yet"
    );
}

/// Literal elisions are holes: they count toward `length` (a single trailing
/// comma does not), render as empty items, and read as `undefined`.
#[test]
fn array_elisions_are_holes() {
    assert_eq!(jsc(&["-p", "[1, , 3]"]), "[ 1, <1 empty item>, 3 ]");
    assert_eq!(jsc(&["-p", "[,]"]), "[ <1 empty item> ]");
    assert_eq!(jsc(&["-p", "[, , 1]"]), "[ <2 empty items>, 1 ]");
    assert_eq!(jsc(&["-p", "[1, , , ]"]), "[ 1, <2 empty items> ]");
    assert_eq!(jsc(&["-p", "[1,].length"]), "1");
    assert_eq!(jsc(&["-p", "[,].length"]), "1");
    assert_eq!(jsc(&["-p", "[1, , 3].length"]), "3");
    assert_eq!(jsc(&["-p", "[1, , 3][1]"]), "undefined");
    assert_eq!(jsc(&["-p", "[1, , 3][\"1\"]"]), "undefined");
    assert_eq!(
        jsc(&["-p", "let a = [1, , 3]; let i = 1; typeof a[i]"]),
        "undefined"
    );
    assert_eq!(jsc(&["-p", "[...[1], , 2]"]), "[ 1, <1 empty item>, 2 ]");
    assert_eq!(jsc(&["-p", "let a = [1, , ]; a.pop()"]), "undefined");
    assert_eq!(jsc(&["-p", "let a = [1, , 3]; a[1] = 2; a"]), "[ 1, 2, 3 ]");
}

/// Holes are absent properties: `in`, `delete`, `Object.keys`, `for-in`, and the
/// skipping higher-order methods do not see them; reads through iteration,
/// `includes`, `join`, spread, and destructuring see `undefined`.
#[test]
fn array_holes_are_absent() {
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1, , 3]; console.log(1 in a, 0 in a, \"2\" in a, 3 in a, \"length\" in a)"
        ]),
        "false true true false true"
    );
    assert_eq!(jsc(&["-p", "let a = [1]; a.foo = 2; \"foo\" in a"]), "true");
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1, 2, 3]; console.log(delete a[1], a, a.length, 1 in a)"
        ]),
        "true [ 1, <1 empty item>, 3 ] 3 false"
    );
    assert_eq!(jsc(&["-p", "let a = [1]; delete a[5]; a"]), "[ 1 ]");
    assert_eq!(
        jsc(&["-p", "let a = [1]; a.foo = 2; delete a.foo; a"]),
        "[ 1 ]"
    );
    assert_eq!(jsc(&["-p", "Object.keys([1, , 3]).join()"]), "0,2");
    assert_eq!(
        jsc(&["-p", "let a = [1]; a.foo = 1; Object.keys(a).join()"]),
        "0,foo"
    );
    assert_eq!(jsc(&["-p", "Object.values([1, , 3])"]), "[ 1, 3 ]");
    assert_eq!(
        jsc(&["-p", "Object.entries([1, , 3]).join(';')"]),
        "0,1;2,3"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let a = [1, , 3]; a.foo = 1; let s = ''; for (const k in a) s += k + ';'; s"
        ]),
        "0;2;foo;"
    );
    assert_eq!(
        jsc(&["-e", "[1, , 3].forEach((x, i) => console.log(x, i))"]),
        "1 0\n3 2"
    );
    assert_eq!(
        jsc(&["-p", "[1, , 3].map(x => x * 2)"]),
        "[ 2, <1 empty item>, 6 ]"
    );
    assert_eq!(jsc(&["-p", "[1, , 3].filter(x => true)"]), "[ 1, 3 ]");
    assert_eq!(
        jsc(&[
            "-p",
            "let n = 0; let s = [1, , 3].reduce((a, x) => { n++; return a + x }, 0); s + ':' + n"
        ]),
        "4:2"
    );
    assert_eq!(jsc(&["-p", "[1, , 3].indexOf(undefined)"]), "-1");
    assert_eq!(jsc(&["-p", "[1, undefined].indexOf(undefined)"]), "1");
    assert_eq!(jsc(&["-p", "[1, , 3].includes(undefined)"]), "true");
    assert_eq!(jsc(&["-p", "[1, , 3].join('-')"]), "1--3");
    assert_eq!(jsc(&["-p", "String([1, , 3])"]), "1,,3");
    assert_eq!(
        jsc(&["-p", "[1, , 3].slice(0)"]),
        "[ 1, <1 empty item>, 3 ]"
    );
    assert_eq!(
        jsc(&["-e", "for (const x of [1, , 3]) console.log(x)"]),
        "1\nundefined\n3"
    );
    assert_eq!(jsc(&["-p", "[...[1, , 3]]"]), "[ 1, undefined, 3 ]");
    assert_eq!(
        jsc(&["-p", "let [x, y, z] = [1, , 3]; typeof y + z"]),
        "undefined3"
    );
    assert_eq!(jsc(&["-p", "JSON.stringify([1, , 3])"]), "[1,null,3]");
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

/// `push`/`pop` mutate the array in place: every alias observes the change.
#[test]
fn array_push_pop_mutate_in_place() {
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1]; let b = a; a.push(2); console.log(b.length, b[1])"
        ]),
        "2 2",
    );
    assert_eq!(
        jsc(&[
            "-e",
            "let a = [1, 2]; let b = a; a.pop(); console.log(b.length, b)"
        ]),
        "1 [ 1 ]",
    );
    assert_eq!(
        jsc(&[
            "-e",
            "let o = { xs: [] }; let a = o.xs; a.push(5); console.log(o.xs)"
        ]),
        "[ 5 ]",
    );
}

/// Pushing past several capacity doublings keeps every element and the length;
/// `pop` after growth shrinks in place and a later `push` reuses the slot.
#[test]
fn array_push_growth_keeps_elements() {
    let src = "let a = []; for (let i = 1; i <= 100; i++) a.push(i); \
               let s = 0; for (let i = 0; i < a.length; i++) s += a[i]; \
               console.log(a.length, s, a[0], a[63], a[99]); \
               console.log(a.pop(), a.length); a.push(7); console.log(a.length, a[99]);";
    assert_eq!(jsc(&["-e", src]), "100 5050 1 64 100\n100 99\n100 7");
    let src = "let a = [1, 2, 3]; for (let i = 0; i < 20; i++) a.push(a.length); \
               console.log(a.length, a[2], a[3], a[22]);";
    assert_eq!(jsc(&["-e", src]), "23 3 3 22");
}

/// Builders that grow a fresh array (filter, spread, concat of several
/// sources, generator spread, string split, slice) keep every element.
#[test]
fn array_builders_after_growth() {
    let src = "let a = []; for (let i = 0; i < 50; i++) a.push(i); \
               let f = a.filter(x => x % 2 == 0); \
               let s = [...a, ...f, -1]; \
               function* g() { for (let i = 0; i < 40; i++) yield i; } \
               let gs = [...g()]; \
               console.log(f.length, f[24], s.length, s[50], s[75], gs.length, gs[39]); \
               console.log(\"a,b,c,d,e,f,g\".split(\",\").length, a.slice(10, 45).length);";
    assert_eq!(jsc(&["-e", src]), "25 48 76 0 -1 40 39\n7 35");
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

/// `push`/`pop` accept any receiver expression, evaluated exactly once:
/// captured bindings, module bindings read inside functions, parameters,
/// member reads, call results, and element reads.
#[test]
fn push_pop_on_any_receiver() {
    let cases: [(&str, &str); 12] = [
        (
            "function f(){ let a=[]; const g=()=>a.push(1); g(); g(); return a.length } f()",
            "2",
        ),
        (
            "const a=[]; function f(){ a.push(1) } f(); f(); a.length",
            "2",
        ),
        (
            "function add(xs,v){ xs.push(v) } const a=[]; add(a,1); add(a,2); a",
            "[ 1, 2 ]",
        ),
        (
            "const o={xs:[]}; o.xs.push(1); o.xs.push(2); o.xs",
            "[ 1, 2 ]",
        ),
        (
            "let n=0; const a=[]; function f(){ n++; return a } f().push(7); f().push(8); n+':'+a",
            "2:7,8",
        ),
        (
            "const m=[[1],[2]]; let i=1; m[i].push(3); m",
            "[ [ 1 ], [ 2, 3 ] ]",
        ),
        ("let a=[1]; const b=a; (()=>a.push(2))(); b.length", "2"),
        (
            "function f(){ let a=[1,2]; const g=()=>a.pop(); return g()+':'+a.length } f()",
            "2:1",
        ),
        (
            "const a=[1,2]; function f(){ return a.pop() } f()+':'+a.length",
            "2:1",
        ),
        (
            "function p(xs){ return xs.pop() } const a=[5,6]; p(a)+':'+a",
            "6:5",
        ),
        ("const o={xs:[1,2]}; o.xs.pop()+':'+o.xs.length", "2:1"),
        (
            "let n=0; const a=[1,2]; function f(){ n++; return a } f().pop()+':'+n+':'+a",
            "2:1:1",
        ),
    ];
    for (src, want) in cases {
        assert_eq!(jsc(&["-p", src]), want, "{src}");
    }
}

/// A non-array receiver throws `<recv>.push is not a function` (the method is
/// absent), after the arguments are evaluated; a nullish receiver still throws
/// the member-read `TypeError` first.
#[test]
fn push_pop_on_non_array_receiver_throws() {
    assert_eq!(
        caught("const o={}; o.push(1)"),
        "true|o.push is not a function"
    );
    assert_eq!(
        caught("const o={v:5}; o.v.pop()"),
        "true|o.v.pop is not a function"
    );
    assert_eq!(
        caught("function f(){ return 1 } f().push(2)"),
        "true|expression is not a function"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let n=0; const o={}; try { o.push(n++) } catch(e) {} n"
        ]),
        "1"
    );
    assert_eq!(
        caught("const o={xs:null}; o.xs.push(1)"),
        "true|Cannot read properties of null (reading 'push')"
    );
}
