//! Integration tests: object literals, property assignment, and nested value composition via the CLI.

mod common;
use common::jsc;

/// The S11.3 object milestones run inline with `-p` through the built binary.
///
/// Covers the object wave end to end: `.a` read + sum, computed `["a"]` read,
/// a string-literal key, and `typeof {}`.
#[test]
fn object_milestones_via_eval() {
    assert_eq!(jsc(&["-p", "let o={a:1,b:2}; o.a+o.b"]), "3");
    assert_eq!(jsc(&["-p", "let o={a:1}; o[\"a\"]"]), "1");
    assert_eq!(jsc(&["-p", "let o={x:10}; o.x"]), "10");
    assert_eq!(jsc(&["-p", "typeof {}"]), "object");
    assert_eq!(jsc(&["-p", "let o={\"k\":5}; o.k"]), "5");
}

/// S11.4: `o.a = v` / `o["a"] = v` store into an EXISTING key's value slot; the
/// assignment expression evaluates to the stored value, and a missing key is a
/// documented no-op that must not corrupt existing entries.
#[test]
fn object_property_assignment_via_eval() {
    assert_eq!(jsc(&["-p", "let o={x:10}; o.x=42; o.x"]), "42");
    assert_eq!(jsc(&["-p", "let o={a:1}; o[\"a\"]=7; o.a"]), "7");
    assert_eq!(jsc(&["-p", "let o={a:0}; let x=(o.a=9); x"]), "9");
    assert_eq!(jsc(&["-p", "let o={a:1}; o.b=5; o.a"]), "1");
}

/// S11.5: nested arrays and objects compose — arrays and objects store full
/// jsvals as elements/values, so a literal nested inside another composes with
/// no special casing. Covers every nesting direction end to end.
#[test]
fn nested_values_compose_via_eval() {
    // Object holding an array (the S11 milestone), read through both hops.
    assert_eq!(jsc(&["-p", "let o={v:[7,8]}; o.v[1]"]), "8");
    // Array of objects — index then property, on each element.
    assert_eq!(jsc(&["-p", "let a=[{n:1},{n:2}]; a[0].n + a[1].n"]), "3");
    // Object of arrays, nested index past the first slot.
    assert_eq!(jsc(&["-p", "let o={xs:[10,20,30]}; o.xs[2]"]), "30");
    // Deep object nesting — three property hops.
    assert_eq!(jsc(&["-p", "let o={a:{b:{c:5}}}; o.a.b.c"]), "5");
    // Array of arrays — two index hops.
    assert_eq!(jsc(&["-p", "let a=[[1,2],[3,4]]; a[1][0]"]), "3");
}

/// S11.5: mutation reaches through a nesting hop and the write sticks.
///
/// The target slot lives inside a heap value that is itself an element/value of
/// the outer container; the assignment must land in the inner container.
#[test]
fn nested_mutation_via_eval() {
    // Write into an array held by an object.
    assert_eq!(jsc(&["-p", "let o={v:[1,2]}; o.v[0]=9; o.v[0]"]), "9");
    // Write into an object held by an array.
    assert_eq!(jsc(&["-p", "let a=[{n:1}]; a[0].n=7; a[0].n"]), "7");
}

/// S11.5: a `while` loop sums a numeric array nested inside an object.
///
/// Combines nested read (`o.xs[i]`), nested `.length`, and loop control — the
/// integration angle for arrays reached through a property.
#[test]
fn nested_array_while_sum_via_eval() {
    assert_eq!(
        jsc(&[
            "-p",
            "let o={xs:[1,2,3,4]}; let i=0; let s=0; while(i<o.xs.length){ s=s+o.xs[i]; i=i+1 } s"
        ]),
        "10",
    );
}

/// S11.5: a nested result formats recursively without panicking.
///
/// The exact spacing is not the contract here — the point is that the recursive
/// decode/format walks nested heap values and round-trips their shape.
#[test]
fn nested_values_print_recursively() {
    assert_eq!(
        jsc(&["-p", "let a=[{n:1},{n:2}]; a"]),
        "[ { n: 1 }, { n: 2 } ]"
    );
    assert_eq!(jsc(&["-p", "let o={v:[1,2]}; o"]), "{ v: [ 1, 2 ] }");
    assert_eq!(
        jsc(&["-p", "let o={a:{b:{c:5}}}; o"]),
        "{ a: { b: { c: 5 } } }"
    );
    assert_eq!(jsc(&["-p", "[[1,2],[3,4]]"]), "[ [ 1, 2 ], [ 3, 4 ] ]");
}

/// P0.4 optional chaining `?.`: a `?.` link short-circuits the WHOLE chain to
/// `undefined` when its base is null/undefined, and reads through normally
/// otherwise. Member (`?.b`), index (`?.[i]`), and mixed optional/non-optional
/// links are exercised end to end.
#[test]
fn optional_chaining_short_circuits() {
    // A null base short-circuits to `undefined` (milestone 1).
    assert_eq!(
        jsc(&["-p", "let o=null; let r=o?.x; typeof r"]),
        "undefined"
    );
    // A non-null chain reads each link normally (milestone 2).
    assert_eq!(jsc(&["-p", "let o={a:{b:42}}; o?.a?.b"]), "42");
    // The `?.` short-circuit skips every FOLLOWING link (no `.a.b.c` deref on a
    // null base), yielding `undefined` rather than crashing.
    assert_eq!(jsc(&["-p", "let o=null; typeof o?.a.b.c"]), "undefined");
    // A non-optional link after an optional one reads normally when the base is
    // present.
    assert_eq!(jsc(&["-p", "let o={a:{b:{c:7}}}; o?.a.b.c"]), "7");
    // Optional computed index, present and null.
    assert_eq!(jsc(&["-p", "let a=[10,20]; a?.[1]"]), "20");
    assert_eq!(jsc(&["-p", "let a=null; typeof a?.[0]"]), "undefined");
    // `?.length` reads the length when present.
    assert_eq!(jsc(&["-p", "let s=\"abc\"; s?.length"]), "3");
}

/// P2.1 `this` + method calls on object literals: a method `obj.m(args)` binds
/// `this = obj`, method values are functions, and shorthand properties (`{n}`)
/// desugar to `{n: n}`. The nested-`this` case exercises the per-function local
/// snapshot: a method calling another method through `this` must keep its own
/// `this` after the nested call clobbers the `__this` global.
#[test]
fn this_and_method_calls_on_objects() {
    // Milestone 1: a method reads `this`.
    assert_eq!(
        jsc(&["-p", "let o={n:41, inc(){return this.n+1}}; o.inc()"]),
        "42"
    );
    // Milestone 2: a method calls another method via `this` — the caller's `this`
    // survives the nested clobber (41*... here 5*8).
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:2,b:3, sum(){return this.a+this.b}, scaled(){return this.sum()*8}}; o.scaled()"
        ]),
        "40"
    );
    // Milestone 3: shorthand property `{n}` ≡ `{n: n}`.
    assert_eq!(jsc(&["-p", "let n=7; let o={n}; o.n"]), "7");
    // Milestone 4: a method value is a function.
    assert_eq!(
        jsc(&["-p", "let o={inc(){return 1}}; typeof (o.inc)"]),
        "function"
    );
    // A method call whose ARGUMENT is itself a `this`-method call must not leak
    // the receiver into the outer call.
    assert_eq!(
        jsc(&[
            "-p",
            "let o={v:5,id(x){return x},run(){return this.id(this.v)}}; o.run()"
        ]),
        "5"
    );
}

/// P2.1 nested `this` chaining through several method hops: each frame snapshots
/// its own receiver, so `this` stays stable across intervening method calls.
#[test]
fn this_chained_through_method_hops() {
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:10,get2(){return this.a},get3(){return this.get2()+this.a}}; o.get3()"
        ]),
        "20"
    );
    // A method calls a method whose result is combined with a later `this` read.
    assert_eq!(
        jsc(&[
            "-p",
            "let o={n:3,f(){return this.n},g(){let a=this.f(); return a+this.n}}; o.g()"
        ]),
        "6"
    );
}

/// P2.2b regression: a member WRITE with a computed RHS must not clobber the
/// stashed receiver/key. The RHS is emitted with the child scratch base, so
/// before the fix `o.v = 3+4` reused the same low slots that held the receiver
/// and produced garbage; the receiver/key now live in dedicated high slots.
#[test]
fn p2_2b_member_set_computed_rhs_no_clobber() {
    // Arithmetic RHS: the classic reproduction.
    assert_eq!(jsc(&["-p", "let o={v:1}; o.v = 3+4; o.v"]), "7");
    // RHS that READS the same object (member read inside the RHS).
    assert_eq!(jsc(&["-p", "let o={v:1}; o.v = o.v + 5; o.v"]), "6");
    // RHS that is itself an index READ of a DIFFERENT object: get/set use
    // disjoint high slots, so the receiver stash survives.
    assert_eq!(jsc(&["-p", "let o={v:1}; let a=[7]; o.v = a[0]; o.v"]), "7");
    // Literal RHS (uses no scratch) must still work — no regression.
    assert_eq!(jsc(&["-p", "let o={v:1}; o.v = 9; o.v"]), "9");
}

/// P3.2: `Object.keys` / `Object.values` / `Object.entries` build a fresh
/// growable array from the object's runtime key table, in insertion order, via
/// the in-wasm `__obj_keys`/`__obj_values`/`__obj_entries` helpers.
#[test]
fn object_keys_values_entries_via_eval() {
    // `Object.keys` — count and insertion-order element (a heap-string key).
    assert_eq!(jsc(&["-p", "Object.keys({a:1,b:2}).length"]), "2");
    assert_eq!(jsc(&["-p", "Object.keys({a:1,b:2})[1]"]), "b");
    // `Object.values` — the value at each slot, summed across two calls.
    assert_eq!(
        jsc(&[
            "-p",
            "let o={x:40,y:2}; Object.values(o)[0]+Object.values(o)[1]"
        ]),
        "42"
    );
    // `Object.entries` — an array of `[key, value]` two-element arrays.
    assert_eq!(jsc(&["-p", "Object.entries({a:1})[0][0]"]), "a");
    assert_eq!(jsc(&["-p", "Object.entries({a:1})[0][1]"]), "1");
}

/// P3.2: the enumerators skip the reserved hidden `@@`-prefixed keys, so a class
/// instance (which carries a `@@prototype` link) enumerates only its own fields.
#[test]
fn object_keys_skip_reserved_keys() {
    assert_eq!(
        jsc(&[
            "-p",
            "class A { constructor(){ this.x=1; this.y=2 } } let a=new A(); Object.keys(a).length"
        ]),
        "2"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "class A { constructor(){ this.x=1; this.y=2 } } let a=new A(); Object.keys(a)[0]"
        ]),
        "x"
    );
    // An empty object enumerates nothing.
    assert_eq!(jsc(&["-p", "Object.keys({}).length"]), "0");
}

/// P3.2: `Object` is a syntactic built-in gated on `resolve("Object") == None`, so
/// a user binding shadows it; an unknown static and a wrong arity are rejected.
#[test]
fn object_builtin_shadowing_and_rejections() {
    use std::process::Command;
    // A user `let Object = ..` shadows the built-in (no method dispatch).
    assert_eq!(jsc(&["-p", "let Object=5; Object"]), "5");
    // Run `jsc` expecting compile-error exit 2 and return its stderr for inspection.
    let jsc_stderr = |src: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
            .args(["-e", src])
            .output()
            .expect("failed to spawn jsc binary");
        assert!(
            output.status.code() == Some(2),
            "expected {src:?} to fail to compile"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    // An out-of-scope static is a spanned compile error.
    assert!(jsc_stderr("Object.assign({},{})").contains("unsupported Object method"));
    // A wrong argument count is a spanned compile error.
    assert!(jsc_stderr("Object.keys()").contains("expects 1 argument"));
}

/// P3.3: the `key in obj` operator is an OWN-key membership test, lowered to the
/// in-wasm `__obj_has` linear scan (byte-content key compare, no prototype walk).
#[test]
fn in_operator_own_key_membership() {
    // A present key yields `true`; an absent one `false`.
    assert_eq!(jsc(&["-p", "'a' in {a:1}"]), "true");
    assert_eq!(jsc(&["-p", "'z' in {a:1}"]), "false");
    // Membership among several keys, and the empty object.
    assert_eq!(jsc(&["-p", "let o={a:1,b:2,c:3}; 'b' in o"]), "true");
    assert_eq!(jsc(&["-p", "'a' in {}"]), "false");
}

/// P3.3: `delete obj.k` removes an own property (compacting it out of the runtime
/// key table) and always evaluates to `true`; a later read yields `undefined`,
/// `in` yields `false`, and every enumerator skips the removed key.
#[test]
fn delete_removes_own_property() {
    // `delete` yields `true`.
    assert_eq!(jsc(&["-p", "let o={a:1}; delete o.a"]), "true");
    // After delete: `in` is false, the read is `undefined`, `typeof` "undefined".
    assert_eq!(jsc(&["-p", "let o={a:1}; delete o.a; 'a' in o"]), "false");
    assert_eq!(
        jsc(&["-p", "let o={a:1}; delete o.a; typeof o.a"]),
        "undefined"
    );
    // The computed form `delete o['k']` works too.
    assert_eq!(
        jsc(&["-p", "let o={a:1}; delete o['a']; 'a' in o"]),
        "false"
    );
    // Deleting a middle key leaves the surviving keys readable and enumerable.
    assert_eq!(
        jsc(&["-p", "let o={a:1,b:2,c:3}; delete o.b; o.a+o.c"]),
        "4"
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:1,b:2,c:3}; delete o.b; Object.keys(o).join(',')"
        ]),
        "a,c"
    );
    // for-in (P3.1) skips the deleted key.
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:1,b:2}; delete o.a; let s=''; for (let k in o) s=s+k; s"
        ]),
        "b"
    );
    // Deleting an absent key is a harmless no-op that still yields `true`.
    assert_eq!(jsc(&["-p", "let o={a:1}; delete o.z; 'a' in o"]), "true");
}

/// P3.3: `delete` requires a member reference; a bare `delete x` is rejected.
#[test]
fn delete_non_member_is_rejected() {
    use std::process::Command;
    let output = Command::new(env!("CARGO_BIN_EXE_jsc"))
        .args(["-e", "let x=1; delete x"])
        .output()
        .expect("failed to spawn jsc binary");
    assert!(
        output.status.code() == Some(2),
        "expected `delete x` to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("`delete` requires a property reference")
    );
}

/// P3.4: the `JSON.stringify` milestones — a number, a quoted string, an array,
/// and an object — serialized entirely in-wasm.
#[test]
fn json_stringify_milestones() {
    assert_eq!(jsc(&["-p", "JSON.stringify(42)"]), "42");
    // The result is the 4-char string `"hi"` (quote, h, i, quote), printed raw.
    assert_eq!(jsc(&["-p", "JSON.stringify(\"hi\")"]), "\"hi\"");
    assert_eq!(jsc(&["-p", "JSON.stringify([1,2,3])"]), "[1,2,3]");
    assert_eq!(
        jsc(&["-p", "JSON.stringify({a:1,b:2})"]),
        "{\"a\":1,\"b\":2}"
    );
    // The output is a genuine heap string, not a special-cased render.
    assert_eq!(jsc(&["-p", "typeof JSON.stringify(42)"]), "string");
}

/// P3.4: primitive serialization — booleans, `null`, floats, and non-finite
/// numbers (which JSON renders as `null`).
#[test]
fn json_stringify_primitives() {
    assert_eq!(jsc(&["-p", "JSON.stringify(true)"]), "true");
    assert_eq!(jsc(&["-p", "JSON.stringify(false)"]), "false");
    assert_eq!(jsc(&["-p", "JSON.stringify(null)"]), "null");
    assert_eq!(jsc(&["-p", "JSON.stringify(1.5)"]), "1.5");
    assert_eq!(jsc(&["-p", "JSON.stringify(-7)"]), "-7");
    // NaN / Infinity are not representable in JSON — they stringify to `null`.
    assert_eq!(jsc(&["-p", "JSON.stringify(1/0)"]), "null");
    assert_eq!(jsc(&["-p", "JSON.stringify(0/0)"]), "null");
}

/// P3.4: string escaping — `"`, `\`, and the whitespace control characters get
/// their JSON backslash escapes; a bare control byte becomes `\u00XX`.
#[test]
fn json_stringify_escapes() {
    assert_eq!(jsc(&["-p", "JSON.stringify(\"a\\\"b\")"]), "\"a\\\"b\"");
    assert_eq!(jsc(&["-p", "JSON.stringify(\"a\\\\b\")"]), "\"a\\\\b\"");
    assert_eq!(jsc(&["-p", "JSON.stringify(\"l1\\nl2\")"]), "\"l1\\nl2\"");
    assert_eq!(jsc(&["-p", "JSON.stringify(\"t\\tx\")"]), "\"t\\tx\"");
    // A non-named control byte (0x01) expands to a `\u00XX` escape.
    assert_eq!(jsc(&["-p", "JSON.stringify(\"\\u0001\")"]), "\"\\u0001\"");
}

/// P3.4: nested containers recurse, and `undefined`/function values are handled
/// per JS — omitted as object values, rendered as `null` as array elements.
#[test]
fn json_stringify_nested_and_holes() {
    assert_eq!(
        jsc(&["-p", "JSON.stringify([1,[2,3],{a:4}])"]),
        "[1,[2,3],{\"a\":4}]"
    );
    assert_eq!(
        jsc(&["-p", "JSON.stringify({o:{a:1},arr:[true,null]})"]),
        "{\"o\":{\"a\":1},\"arr\":[true,null]}"
    );
    assert_eq!(
        jsc(&["-p", "JSON.stringify([1,undefined,3])"]),
        "[1,null,3]"
    );
    assert_eq!(
        jsc(&["-p", "JSON.stringify({a:1,b:undefined,c:3})"]),
        "{\"a\":1,\"c\":3}"
    );
    // A function value is likewise omitted as an object value and `null` in arrays.
    assert_eq!(
        jsc(&["-p", "function f(){} JSON.stringify({a:1,b:f})"]),
        "{\"a\":1}"
    );
    assert_eq!(jsc(&["-p", "JSON.stringify([])"]), "[]");
    assert_eq!(jsc(&["-p", "JSON.stringify({})"]), "{}");
    // `JSON.stringify(undefined)` is the value `undefined`, not a string.
    assert_eq!(
        jsc(&["-p", "typeof JSON.stringify(undefined)"]),
        "undefined"
    );
}

/// P3.4: `JSON` is a syntactic built-in, shadowable like `Object`/`Math`; a
/// user binding named `JSON` wins and is not treated as the built-in.
#[test]
fn json_shadowing() {
    assert_eq!(jsc(&["-p", "let JSON=5; JSON"]), "5");
    assert_eq!(jsc(&["-p", "let JSON={stringify:1}; JSON.stringify"]), "1");
}

/// `JSON.parse` parses scalars, arrays, and objects with RUNTIME-unknown keys via
/// the in-wasm recursive-descent parser (un-defers the P3.4 backlog item).
#[test]
fn json_parse_scalars_and_composites() {
    // Scalars.
    assert_eq!(jsc(&["-p", "JSON.parse(\"42\")"]), "42");
    assert_eq!(jsc(&["-p", "JSON.parse(\"-3.5\")"]), "-3.5");
    assert_eq!(jsc(&["-p", "JSON.parse(\"true\")"]), "true");
    assert_eq!(jsc(&["-p", "JSON.parse(\"null\")"]), "null");
    assert_eq!(jsc(&["-p", "JSON.parse(\"\\\"hi\\\"\")"]), "hi");
    // Arrays (nested, indexed).
    assert_eq!(jsc(&["-p", "JSON.parse(\"[1,2,3]\").length"]), "3");
    assert_eq!(jsc(&["-p", "JSON.parse(\"[1,[2,3],4]\")[1][0]"]), "2");
    assert_eq!(jsc(&["-p", "JSON.parse(\"[]\").length"]), "0");
    // Objects with runtime keys resolved by a literal access.
    assert_eq!(
        jsc(&["-p", "JSON.parse(\"{\\\"a\\\":1,\\\"b\\\":2}\").a"]),
        "1"
    );
    assert_eq!(
        jsc(&["-p", "JSON.parse(\"{\\\"name\\\":\\\"Bob\\\"}\").name"]),
        "Bob"
    );
    assert_eq!(jsc(&["-p", "Object.keys(JSON.parse(\"{}\")).length"]), "0");
    // Insignificant whitespace is skipped.
    assert_eq!(jsc(&["-p", "JSON.parse(\"  [ 1 , 2 ] \")[1]"]), "2");
    // The argument is coerced to a string (like spec ToString).
    assert_eq!(jsc(&["-p", "JSON.parse(42)"]), "42");
    // Round-trips through JSON.stringify.
    assert_eq!(
        jsc(&[
            "-p",
            "let o={a:1,b:[2,3],c:\"x\"}; JSON.stringify(JSON.parse(JSON.stringify(o)))"
        ]),
        "{\"a\":1,\"b\":[2,3],\"c\":\"x\"}",
    );
}

/// Malformed (or unsupported — a `\u` escape) input throws a catchable branded
/// `Error` whose `.name` is `SyntaxError`.
#[test]
fn json_parse_rejects_malformed_input() {
    assert_eq!(
        jsc(&[
            "-p",
            "let r; try{JSON.parse(\"{bad}\")}catch(e){r=e.name} r"
        ]),
        "SyntaxError",
    );
    assert_eq!(
        jsc(&[
            "-p",
            "let r; try{JSON.parse(\"[1,\")}catch(e){r=e instanceof Error} r"
        ]),
        "true",
    );
    // Trailing content, unterminated string, empty input, and `\u` all throw.
    for src in [
        "let r=0; try{JSON.parse(\"1 2\")}catch(e){r=1} r",
        "let r=0; try{JSON.parse(\"\\\"abc\")}catch(e){r=1} r",
        "let r=0; try{JSON.parse(\"\")}catch(e){r=1} r",
        "let r=0; try{JSON.parse(\"\\\"\\\\u0041\\\"\")}catch(e){r=1} r",
    ] {
        assert_eq!(jsc(&["-p", src]), "1", "expected `{src}` to throw");
    }
}
