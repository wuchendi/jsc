//! Integration tests: dictionary objects — property tables, key interning, and
//! the generic get/set/has/delete/define/own-keys algorithms (design 2.3-2.6).
//! Expected texts are Node's `console.log` output.
#![allow(clippy::expect_used)]

mod common;
use common::{assert_output, assert_uncaught};

/// DF1: reserved keys never leak. A class instance enumerates only its own
/// fields through `for-in`, `Object.keys`, rest, `JSON.stringify`, and the
/// renderer; a user key starting with `@` is an ordinary key.
#[test]
fn reserved_keys_do_not_leak() {
    assert_output(
        "class A { constructor(){ this.x = 1 } m(){} } let a = new A(); for (const k in a) console.log(k)",
        "x\n",
    );
    assert_output(
        "class A { constructor(){ this.x = 1 } } let a = new A(); let {...r} = a; \
         console.log(Object.keys(a), r, JSON.stringify(a), a)",
        "[ x ] { x: 1 } {\"x\":1} A { x: 1 }\n",
    );
    assert_output(
        "let e = new RangeError('r'); console.log(Object.keys(e).length, JSON.stringify(Object.values(new Error())))",
        "0 []\n",
    );
    assert_output(
        "console.log(Object.keys({'@x': 1, b: 2}), {'@x': 1})",
        "[ @x, b ] { @x: 1 }\n",
    );
}

/// DF3: a member read on a number/boolean never touches memory.
#[test]
fn primitive_member_reads_are_undefined() {
    assert_output(
        "let n = 5; console.log(n.x, true.y)",
        "undefined undefined\n",
    );
}

/// DF5: `in` walks the prototype chain, and own keys list integer keys first,
/// ascending.
#[test]
fn in_walks_prototypes_and_keys_are_integer_first() {
    assert_output(
        "class A { m(){} } class B extends A {} console.log('m' in new A(), 'm' in new B(), 'z' in new B())",
        "true true false\n",
    );
    assert_output(
        "console.log(Object.keys({b:1, 2:1, a:1, 1:1}))",
        "[ 1, 2, b, a ]\n",
    );
    assert_output(
        "let o = {b: 1}; o[10] = 2; o[9] = 3; o.a = 4; console.log(o, JSON.stringify(o))",
        "{ 9: 3, 10: 2, b: 1, a: 4 } {\"9\":3,\"10\":2,\"b\":1,\"a\":4}\n",
    );
}

/// Dynamic add/delete: tombstones are skipped, a re-added key goes to the end,
/// and growth keeps insertion order.
#[test]
fn dynamic_properties_add_delete_and_grow() {
    assert_output(
        "let o = {}; o.x = 1; o.y = 2; delete o.x; o.z = 3; console.log(o)",
        "{ y: 2, z: 3 }\n",
    );
    assert_output(
        "let o = {a: 1, b: 2}; delete o.a; o.a = 3; console.log(o, 'a' in o, delete o.q)",
        "{ b: 2, a: 3 } true true\n",
    );
    assert_output(
        "let o = {}; for (let i = 0; i < 100; i++) o['k' + i] = i; \
         let ks = Object.keys(o); let ok = true; \
         for (let i = 0; i < 100; i++) if (ks[i] !== 'k' + i || o[ks[i]] !== i) ok = false; \
         console.log(ks.length, ok)",
        "100 true\n",
    );
    // Deleting half then adding reuses the table without losing order.
    assert_output(
        "let o = {a:1, b:2, c:3, d:4}; delete o.a; delete o.c; o.e = 5; o.f = 6; o.g = 7; console.log(o)",
        "{ b: 2, d: 4, e: 5, f: 6, g: 7 }\n",
    );
}

/// Interning: a runtime-built string finds the literal key, and number and
/// numeric-string keys name the same property.
#[test]
fn computed_keys_are_interned() {
    assert_output(
        "let k = 'a' + 'b'; let o = { ab: 1 }; console.log(o[k], k in o)",
        "1 true\n",
    );
    assert_output(
        "let o = {}; o[1] = 'x'; console.log(o['1'], o[1], Object.keys(o)); o['1'] = 'y'; console.log(o)",
        "x x [ 1 ]\n{ 1: y }\n",
    );
    assert_output(
        "let o = {}; o[1.5] = 'a'; o[-1] = 'b'; o[true] = 'c'; console.log(o['1.5'], o['-1'], o.true, Object.keys(o))",
        "a b c [ 1.5, -1, true ]\n",
    );
    assert_output(
        "let o = {}; let k = 'never' + 'seen'; console.log(o[k], k in o)",
        "undefined false\n",
    );
}

/// Computed and numeric literal keys round-trip through `Object.keys/entries`
/// and rendering; a duplicate key overwrites in place.
#[test]
fn computed_and_numeric_literal_keys() {
    assert_output(
        "let k = 'c'; let o = {[k]: 1, 2.5: 'b', 1: 'a', [k + k]: 2}; \
         console.log(Object.keys(o), Object.entries(o), o)",
        "[ 1, c, 2.5, cc ] [ [ 1, a ], [ c, 1 ], [ 2.5, b ], [ cc, 2 ] ] { 1: a, c: 1, 2.5: b, cc: 2 }\n",
    );
    assert_output(
        "let o = {a: 1, b: 2, a: 3}; console.log(o)",
        "{ a: 3, b: 2 }\n",
    );
    assert_output(
        "let o = {m(){ return this.v }, v: 4, ['n' + 1]: () => 5}; console.log(o.m(), o.n1())",
        "4 5\n",
    );
}

/// `for-in` lists own keys, then inherited enumerable keys not shadowed, from
/// a snapshot taken at loop entry; strings and arrays list their indices.
#[test]
fn for_in_walks_the_prototype_chain() {
    assert_output(
        "class A { constructor(){ this.a = 1 } } class B extends A { constructor(){ super(); this.b = 2 } } \
         for (const k in new B()) console.log(k)",
        "a\nb\n",
    );
    assert_output(
        "let o = {x: 1, y: 2}; for (const k in o) { o.z = 3; console.log(k) } console.log(o.z)",
        "x\ny\n3\n",
    );
    assert_output("for (const k in 'ab') console.log(k)", "0\n1\n");
    assert_output("for (const k in [7, 8]) console.log(k)", "0\n1\n");
}

/// Function values carry their own properties.
#[test]
fn functions_take_properties() {
    assert_output(
        "let g = () => 1; g.tag = 'x'; console.log(g.tag, 'tag' in g, g())",
        "x true 1\n",
    );
}

/// An absent method is a runtime `TypeError`; an added one is callable.
#[test]
fn absent_methods_fail_at_run_time() {
    assert_uncaught("let o = {}; o.foo()", "TypeError: o.foo is not a function");
    assert_output("let o = {}; o.f = () => 1; console.log(o.f())", "1\n");
}

/// Object rest copies own enumerable properties except the destructured keys.
#[test]
fn object_rest_skips_excluded_keys() {
    assert_output(
        "let {a, ...r} = {a: 1, 2: 'two', b: 3}; console.log(a, r)",
        "1 { 2: two, b: 3 }\n",
    );
}

/// `JSON.parse` builds ordinary dictionary objects (a duplicate key keeps its
/// first position with the last value) that take new properties.
#[test]
fn json_parse_builds_dictionary_objects() {
    assert_output(
        "let o = JSON.parse('{\"b\": 1, \"a\": {\"c\": 2}, \"b\": 3}'); o.d = 4; console.log(o, o.a.c)",
        "{ b: 3, a: { c: 2 }, d: 4 } 2\n",
    );
}

/// DF2: `.length` dispatches on the receiver kind — strings and arrays read
/// their length header, functions their materialized `length`, objects their
/// own or inherited `length` property, and primitives without one `undefined`.
#[test]
fn length_dispatches_on_receiver_kind() {
    assert_output(
        "console.log((5).length, ({}).length, ({length: 3}).length, 'abc'.length, [1,2].length, (function(a,b){}).length)",
        "undefined undefined 3 3 2 2\n",
    );
    assert_output(
        "class A { get length() { return 7 } } let f = (a, b, c) => 0; \
         console.log(new A().length, f.length, true.length, Object.keys([]).length)",
        "7 3 undefined 0\n",
    );
    assert_uncaught(
        "let n = null; n.length",
        "TypeError: Cannot read properties of null (reading 'length')",
    );
}
