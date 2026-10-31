//! Integration tests: `Array.of` / `Array.from`, the non-mutating
//! `%Array.prototype%` methods (`at`, `find*`, `some`, `every`,
//! `reduceRight`, `lastIndexOf`, `concat`, `flat`, `flatMap`), the mutating
//! ones (`splice`, `sort`, `reverse`, `shift`, `unshift`, `fill`,
//! `copyWithin`), the change-by-copy ones (`with`, `toReversed`, `toSorted`,
//! `toSpliced`) and `ArraySpeciesCreate` by `constructor` (DESIGN.md §6.4,
//! §13): generic over array-likes, holes, subclass receivers.

mod common;
use common::{assert_bool, assert_number, assert_string};

/// A prefix that makes a program not intrinsics-sealed without writing any
/// prototype, so call sites run behind their run-time guards.
const OPEN: &str = "let __open = 'prototype'; ";

/// Assert `src` evaluates to the string `expected`, both intrinsics-sealed
/// and behind run-time guards.
fn both_string(src: &str, expected: &str) {
    assert_string(src, expected);
    assert_string(&format!("{OPEN}{src}"), expected);
}

/// [`both_string`] for a number.
fn both_number(src: &str, expected: f64) {
    assert_number(src, expected);
    assert_number(&format!("{OPEN}{src}"), expected);
}

/// [`both_string`] for a boolean.
fn both_bool(src: &str, expected: bool) {
    assert_bool(src, expected);
    assert_bool(&format!("{OPEN}{src}"), expected);
}

/// Assert `src` throws a `TypeError` (caught and tested in the program).
fn throws_type_error(src: &str) {
    both_bool(
        &format!(
            "let ok = false; try {{ {src}; }} catch (e) {{ ok = e instanceof TypeError; }} ok"
        ),
        true,
    );
}

/// An array-like with two elements and a hole at index 2 of length 4.
const LIKE: &str = "const o = { length: 4, 0: 'a', 1: 'b', 3: 'd' }; ";

/// An Array subclass.
const SUB: &str = "class A extends Array {}; ";

// --- function values ---------------------------------------------------------

#[test]
fn new_methods_have_name_and_length() {
    for (name, length) in [
        ("at", 1.0),
        ("find", 1.0),
        ("findIndex", 1.0),
        ("findLast", 1.0),
        ("findLastIndex", 1.0),
        ("some", 1.0),
        ("every", 1.0),
        ("reduceRight", 1.0),
        ("lastIndexOf", 1.0),
        ("concat", 1.0),
        ("flat", 0.0),
        ("flatMap", 1.0),
    ] {
        both_string(&format!("typeof [].{name}"), "function");
        both_string(&format!("Array.prototype.{name}.name"), name);
        both_number(&format!("Array.prototype.{name}.length"), length);
        both_bool(
            &format!("Object.keys(Array.prototype).indexOf('{name}') === -1"),
            true,
        );
    }
    for (name, length) in [("of", 0.0), ("from", 1.0)] {
        both_string(&format!("typeof Array.{name}"), "function");
        both_string(&format!("Array.{name}.name"), name);
        both_number(&format!("Array.{name}.length"), length);
    }
}

// --- Array.of / Array.from -----------------------------------------------------

/// `Array.isArray` tests the Array exotic brand, not the `proto` chain that
/// `instanceof Array` walks.
#[test]
fn is_array_tests_the_brand_not_the_prototype() {
    both_bool(
        "var C = function () {}; C.prototype = []; Array.isArray(new C())",
        false,
    );
    both_bool(
        "var C = function () {}; C.prototype = Array.prototype; Array.isArray(new C())",
        false,
    );
    both_bool("Array.isArray(Object.create(Array.prototype))", false);
    both_bool("Object.create(Array.prototype) instanceof Array", true);
    both_bool("Array.isArray(Object.setPrototypeOf([], null))", true);
    both_bool("[] instanceof Array", true);
    both_bool("Array.isArray([])", true);
    both_bool("Array.isArray(new Array(3))", true);
    both_bool("Array.isArray({})", false);
    both_bool(
        "Array.isArray('abc') || Array.isArray(1) || Array.isArray()",
        false,
    );
    both_bool("class B extends Array {} Array.isArray(new B())", true);
    both_bool(
        "const i = Array.isArray; var C = function () {}; C.prototype = []; i(new C())",
        false,
    );
}

#[test]
fn array_of_builds_from_arguments() {
    both_string("JSON.stringify(Array.of(7, 'x', null))", "[7,\"x\",null]");
    both_number("Array.of(7).length", 1.0);
    both_number("Array.of().length", 0.0);
    both_bool("Array.isArray(Array.of(1))", true);
    both_string(
        "JSON.stringify(Array.of(...[1, 2, 3, 4, 5]))",
        "[1,2,3,4,5]",
    );
}

#[test]
fn array_of_honours_this_constructor() {
    both_bool(&format!("{SUB}A.of(1, 2) instanceof A"), true);
    both_string(&format!("{SUB}A.of(1, 2).join()"), "1,2");
    both_number(&format!("{SUB}A.of(1, 2).length"), 2.0);
    // A non-constructor `this` makes a plain array.
    both_bool("Array.of.call(undefined, 1) instanceof Array", true);
    both_string("Array.of.call({}, 1, 2).join()", "1,2");
    // A plain constructor receives the length and gets indexed data properties.
    both_string(
        "function C(n) { this.n = n; } const c = Array.of.call(C, 'x', 'y'); c.n + c[0] + c[1] + c.length",
        "2xy2",
    );
}

#[test]
fn array_from_array_likes() {
    both_string(
        &format!("{LIKE}JSON.stringify(Array.from(o))"),
        "[\"a\",\"b\",null,\"d\"]",
    );
    both_number("Array.from({ length: 3 }).length", 3.0);
    both_bool(
        "Array.from({ length: 1 })[0] === undefined && Array.from({ length: 1 }).hasOwnProperty(0)",
        true,
    );
    both_string(
        "Array.from({ length: 2, 0: 1, 1: 2 }, function (x, i) { return x * 10 + i + this.d; }, { d: 100 }).join()",
        "110,121",
    );
    both_number("Array.from(5).length", 0.0);
}

#[test]
fn array_from_iterables() {
    both_string("Array.from('abc').join('-')", "a-b-c");
    both_string("Array.from([1, , 3]).join('-')", "1--3");
    both_bool("Array.from([1, , 3]).hasOwnProperty(1)", true);
    both_string("Array.from([1, 2], (x, i) => x + i).join()", "1,3");
    both_string("Array.from(new Set([1, 2, 2, 3])).join()", "1,2,3");
    both_string(
        "JSON.stringify(Array.from(new Map([[1, 'a'], [2, 'b']])))",
        "[[1,\"a\"],[2,\"b\"]]",
    );
    both_string(
        "function* g() { yield 1; yield 2; } Array.from(g(), (x) => x * 2).join()",
        "2,4",
    );
    // A copy, not the same array.
    both_bool("const a = [1]; Array.from(a) !== a", true);
}

#[test]
fn array_from_checks_map_fn_before_iterating() {
    throws_type_error("Array.from([1], 5)");
    throws_type_error("Array.from({ get length() { throw new Error('read'); } }, {})");
    both_string(
        "let s = ''; try { Array.from({ get length() { s += 'len'; return 0; } }, 1); } catch (e) { s += e.constructor.name; } s",
        "TypeError",
    );
    throws_type_error("Array.from(null)");
    throws_type_error("Array.from(undefined)");
}

#[test]
fn array_from_honours_this_constructor() {
    both_bool(&format!("{SUB}A.from([1, 2]) instanceof A"), true);
    both_string(&format!("{SUB}A.from('xy').join()"), "x,y");
    both_bool(
        &format!("{SUB}A.from({{ length: 1, 0: 9 }}) instanceof A"),
        true,
    );
    both_string(
        "function C(n) { this.args = arguments.length + ':' + n; } const c = Array.from.call(C, { length: 2, 0: 'p', 1: 'q' }); c.args + c[0] + c[1] + c.length",
        "1:2pq2",
    );
    both_string(
        "function C() { this.args = arguments.length; } const c = Array.from.call(C, ['p']); c.args + c[0] + c.length",
        "0p1",
    );
    both_bool("Array.from.call({}, [1]) instanceof Array", true);
}

// --- at ------------------------------------------------------------------------

#[test]
fn at_reads_relative_indices() {
    both_number("[1, 2, 3].at(0)", 1.0);
    both_number("[1, 2, 3].at(-1)", 3.0);
    both_bool(
        "[1, 2, 3].at(3) === undefined && [1, 2, 3].at(-4) === undefined",
        true,
    );
    both_number("[1, 2, 3].at(1.7)", 2.0);
    both_number("[1, 2, 3].at('2')", 3.0);
    both_number("[1, 2, 3].at()", 1.0);
    both_string(&format!("{LIKE}Array.prototype.at.call(o, -1)"), "d");
    both_bool(
        &format!("{LIKE}Array.prototype.at.call(o, 2) === undefined"),
        true,
    );
    both_string("Array.prototype.at.call('xyz', -2)", "y");
    both_number(&format!("{SUB}A.from([4, 5]).at(-1)"), 5.0);
    throws_type_error("Array.prototype.at.call(null, 0)");
}

// --- find / findIndex / findLast / findLastIndex ------------------------------

#[test]
fn find_family_visits_every_index() {
    both_number("[1, 2, 3, 4].find((x) => x > 2)", 3.0);
    both_number("[1, 2, 3, 4].findIndex((x) => x > 2)", 2.0);
    both_number("[1, 2, 3, 4].findLast((x) => x < 3)", 2.0);
    both_number("[1, 2, 3, 4].findLastIndex((x) => x < 3)", 1.0);
    both_bool("[1, 2].find((x) => x > 5) === undefined", true);
    both_number("[1, 2].findIndex((x) => x > 5)", -1.0);
    both_number("[1, 2].findLastIndex((x) => x > 5)", -1.0);
    // Holes are visited as undefined.
    both_string(
        "let s = ''; [1, , 3].find((x, i) => { s += i + ':' + x + ' '; }); s",
        "0:1 1:undefined 2:3 ",
    );
    both_string(
        "let s = ''; [1, , 3].findLast((x, i) => { s += i; }); s",
        "210",
    );
    // Array-like receivers, `this` and the third argument.
    both_string(
        &format!(
            "{LIKE}Array.prototype.find.call(o, function (x, i, obj) {{ return obj === o && x === this.v; }}, {{ v: 'b' }})"
        ),
        "b",
    );
    both_number(
        &format!("{LIKE}Array.prototype.findLastIndex.call(o, (x) => x === undefined)"),
        2.0,
    );
    // The length is read once.
    both_number(
        "const a = [1, 2]; a.findIndex((x) => { a.push(x); return x === 9; })",
        -1.0,
    );
    throws_type_error("[].find(1)");
    throws_type_error("[].findLast()");
    throws_type_error("Array.prototype.findIndex.call(undefined, (x) => x)");
}

// --- some / every ----------------------------------------------------------------

#[test]
fn some_and_every_skip_holes() {
    both_bool("[1, 2, 3].some((x) => x > 2)", true);
    both_bool("[1, 2, 3].some((x) => x > 3)", false);
    both_bool("[1, 2, 3].every((x) => x > 0)", true);
    both_bool("[1, 2, 3].every((x) => x > 1)", false);
    both_bool("[].some((x) => true)", false);
    both_bool("[].every((x) => false)", true);
    both_string("let s = ''; [1, , 3].some((x, i) => { s += i; }); s", "02");
    both_string(
        "let s = ''; [1, , 3].every((x, i) => { s += i; return true; }); s",
        "02",
    );
    both_bool(
        &format!("{LIKE}Array.prototype.some.call(o, (x) => x === 'd')"),
        true,
    );
    both_bool(
        &format!("{LIKE}Array.prototype.every.call(o, (x) => typeof x === 'string')"),
        true,
    );
    both_bool(
        "[1].some(function () { return this.ok; }, { ok: true })",
        true,
    );
    throws_type_error("[1].some()");
    throws_type_error("[1].every({})");
}

// --- reduceRight -----------------------------------------------------------------

#[test]
fn reduce_right_folds_from_the_end() {
    both_string("['a', 'b', 'c'].reduceRight((acc, x) => acc + x)", "cba");
    both_string(
        "['a', 'b', 'c'].reduceRight((acc, x) => acc + x, '>')",
        ">cba",
    );
    both_string("[1, , 3].reduceRight((acc, x, i) => acc + i, '')", "20");
    both_string(
        &format!(
            "{LIKE}Array.prototype.reduceRight.call(o, (acc, x, i, obj) => acc + x + (obj === o), '')"
        ),
        "dtruebtrueatrue",
    );
    throws_type_error("[].reduceRight((a, b) => a)");
    throws_type_error("[, ,].reduceRight((a, b) => a)");
    throws_type_error("[1].reduceRight(1)");
    both_number("[5].reduceRight((a, b) => a + b)", 5.0);
}

// --- lastIndexOf ------------------------------------------------------------------

#[test]
fn last_index_of_searches_backwards() {
    both_number("[1, 2, 1, 2].lastIndexOf(2)", 3.0);
    both_number("[1, 2, 1, 2].lastIndexOf(2, 2)", 1.0);
    both_number("[1, 2, 1, 2].lastIndexOf(2, -2)", 1.0);
    both_number("[1, 2, 1, 2].lastIndexOf(2, -5)", -1.0);
    both_number("[1, 2, 1, 2].lastIndexOf(3)", -1.0);
    both_number("[NaN].lastIndexOf(NaN)", -1.0);
    both_number("[, undefined].lastIndexOf(undefined)", 1.0);
    both_number("[undefined, ,].lastIndexOf(undefined, 1)", 0.0);
    both_number("[1, 2].lastIndexOf(1, undefined)", 0.0);
    both_number("[1, 2].lastIndexOf(2, 100)", 1.0);
    both_number(
        &format!("{LIKE}Array.prototype.lastIndexOf.call(o, 'a')"),
        0.0,
    );
    both_number("[].lastIndexOf(1)", -1.0);
    throws_type_error("Array.prototype.lastIndexOf.call(null, 1)");
}

// --- concat -------------------------------------------------------------------------

#[test]
fn concat_spreads_arrays_only() {
    both_string(
        "JSON.stringify([1].concat([2, 3], 4, [[5]]))",
        "[1,2,3,4,[5]]",
    );
    both_number("[1, ,].concat([, 2]).length", 4.0);
    both_bool("[1, ,].concat([, 2]).hasOwnProperty(1)", false);
    both_bool("const a = [1]; a.concat() !== a", true);
    // An array-like argument is one element; an array-like receiver too.
    both_number("[].concat({ length: 2, 0: 1 }).length", 1.0);
    both_string(
        &format!(
            "{LIKE}const r = Array.prototype.concat.call(o, [1]); (r[0] === o) + ':' + r.length"
        ),
        "true:2",
    );
    both_string(
        "const r = Array.prototype.concat.call('ab', 'c'); typeof r[0] + r.length",
        "object2",
    );
}

// --- flat / flatMap -------------------------------------------------------------------

#[test]
fn flat_flattens_to_depth() {
    both_string("JSON.stringify([1, [2, [3, [4]]]].flat())", "[1,2,[3,[4]]]");
    both_string("JSON.stringify([1, [2, [3, [4]]]].flat(2))", "[1,2,3,[4]]");
    both_string(
        "JSON.stringify([1, [2, [3, [4]]]].flat(Infinity))",
        "[1,2,3,4]",
    );
    both_string("JSON.stringify([1, [2]].flat(0))", "[1,[2]]");
    both_string("JSON.stringify([1, [2]].flat(-1))", "[1,[2]]");
    both_string("JSON.stringify([1, [2]].flat('1'))", "[1,2]");
    both_string("JSON.stringify([1, , [2, , 3]].flat())", "[1,2,3]");
    both_string(
        "JSON.stringify([{ length: 1, 0: 'x' }].flat())",
        "[{\"0\":\"x\",\"length\":1}]",
    );
    both_string(
        "JSON.stringify(Array.prototype.flat.call({ length: 2, 0: [1, 2], 1: 3 }))",
        "[1,2,3]",
    );
}

#[test]
fn flat_map_maps_then_flattens_one_level() {
    both_string(
        "JSON.stringify([1, 2].flatMap((x) => [x, x * 10]))",
        "[1,10,2,20]",
    );
    both_string("JSON.stringify([1, 2].flatMap((x) => [[x]]))", "[[1],[2]]");
    both_string("JSON.stringify([1, , 2].flatMap((x, i) => i))", "[0,2]");
    both_string(
        "JSON.stringify([1].flatMap(function (x, i, a) { return [this.k, i, a.length]; }, { k: 'k' }))",
        "[\"k\",0,1]",
    );
    throws_type_error("[1].flatMap()");
    throws_type_error("[].flatMap({})");
}

// --- ArraySpeciesCreate by constructor ---------------------------------------------

#[test]
fn species_create_uses_the_constructor() {
    for call in [
        "map((x) => x)",
        "filter((x) => true)",
        "slice(0)",
        "concat([3])",
        "flat()",
        "flatMap((x) => [x])",
    ] {
        both_bool(
            &format!("{SUB}const a = A.from([1, 2]); a.{call} instanceof A"),
            true,
        );
        // A plain array and an array-like make plain arrays.
        both_bool(&format!("[1, 2].{call}.constructor === Array"), true);
        both_bool(
            &format!(
                "Array.prototype.{}.constructor === Array",
                call.replacen('(', ".call({ length: 2, 0: 1, 1: 2 }, ", 1)
            ),
            true,
        );
        // `constructor` undefined makes a plain array; a primitive throws.
        both_bool(
            &format!("const a = [1, 2]; a.constructor = undefined; Array.isArray(a.{call})"),
            true,
        );
        throws_type_error(&format!("const a = [1, 2]; a.constructor = 0; a.{call}"));
        // A constructor without an Array species makes a plain array.
        both_bool(
            &format!(
                "const a = [1, 2]; const F = function () {{}}; a.constructor = F; const r = a.{call}; Array.isArray(r) && !(r instanceof F)"
            ),
            true,
        );
    }
    both_string(
        &format!("{SUB}A.from([1, 2, 3]).map((x) => x * 2).join()"),
        "2,4,6",
    );
    both_number(
        &format!("{SUB}A.from([1, 2, 3]).filter((x) => x > 1).length"),
        2.0,
    );
    both_string(&format!("{SUB}A.from([1, 2, 3]).slice(1).join()"), "2,3");
    both_string(
        &format!("{SUB}JSON.stringify(A.from([1, [2]]).flat())"),
        "[1,2]",
    );
    // The species constructor receives the length.
    both_string(
        "let seen = ''; class B extends Array { constructor(...args) { seen += args.join('/') + ';'; super(...args); } } const b = B.from([1, 2, 3]); seen = ''; b.map((x) => x); b.filter((x) => x); b.slice(1); b.concat(); seen",
        "3;0;2;0;",
    );
}

// --- audit: existing HOFs on array-likes --------------------------------------------

#[test]
fn existing_methods_on_array_likes_and_holes() {
    both_string(
        &format!("{LIKE}Array.prototype.map.call(o, (x) => x + x).length + ''"),
        "4",
    );
    both_bool(
        &format!("{LIKE}Array.prototype.map.call(o, (x) => x).hasOwnProperty(2)"),
        false,
    );
    both_string(
        &format!("{LIKE}Array.prototype.slice.call(o, 1).length + ''"),
        "3",
    );
    both_string(
        &format!("{LIKE}Array.prototype.join.call(o, '-')"),
        "a-b--d",
    );
    both_number(&format!("{LIKE}Array.prototype.indexOf.call(o, 'd')"), 3.0);
    both_bool(
        &format!("{LIKE}Array.prototype.includes.call(o, undefined)"),
        true,
    );
    // The callback check precedes iteration but follows the length read.
    both_string(
        "let s = ''; try { Array.prototype.map.call({ get length() { s += 'len,'; return 1; }, get 0() { s += 'get,'; return 1; } }, 1); } catch (e) { s += e.constructor.name; } s",
        "len,TypeError",
    );
    // The length is captured once.
    both_number(
        "const a = [1, 2]; let n = 0; a.forEach((x) => { n++; a.push(x); }); n",
        2.0,
    );
    both_number(
        "const a = [1, 2]; a.map((x) => { a.push(x); return x; }).length",
        2.0,
    );
}

// --- mutating methods: splice / sort / reverse / shift / unshift / fill / copyWithin ---

#[test]
fn mutating_methods_have_name_and_length() {
    for (name, length) in [
        ("splice", 2.0),
        ("sort", 1.0),
        ("reverse", 0.0),
        ("shift", 0.0),
        ("unshift", 1.0),
        ("fill", 1.0),
        ("copyWithin", 2.0),
    ] {
        both_string(&format!("typeof [].{name}"), "function");
        both_string(&format!("Array.prototype.{name}.name"), name);
        both_number(&format!("Array.prototype.{name}.length"), length);
        both_bool(
            &format!("Object.keys(Array.prototype).indexOf('{name}') === -1"),
            true,
        );
        throws_type_error(&format!("Array.prototype.{name}.call(null)"));
        throws_type_error(&format!("Array.prototype.{name}.call(undefined)"));
    }
}

#[test]
fn splice_removes_and_inserts() {
    both_string(
        "const a = [1, 2, 3, 4, 5]; const r = a.splice(1, 2); a.join() + '|' + r.join()",
        "1,4,5|2,3",
    );
    both_string(
        "const a = [1, 2, 3]; const r = a.splice(1, 0, 'x', 'y'); a.join() + '|' + r.length",
        "1,x,y,2,3|0",
    );
    both_string(
        "const a = [1, 2, 3, 4]; const r = a.splice(-2, 1, 'x', 'y', 'z'); a.join() + '|' + r.join()",
        "1,2,x,y,z,4|3",
    );
    // deleteCount omitted removes to the end; undefined removes none.
    both_string(
        "const a = [1, 2, 3]; const r = a.splice(1); a.join() + '|' + r.join()",
        "1|2,3",
    );
    both_string(
        "const a = [1, 2, 3]; const r = a.splice(1, undefined); a.join() + '|' + r.length",
        "1,2,3|0",
    );
    both_string(
        "const a = [1, 2, 3]; const r = a.splice(); a.join() + '|' + r.length",
        "1,2,3|0",
    );
    both_string(
        "const a = [1, 2, 3]; a.splice(10, 5, 'z'); a.join()",
        "1,2,3,z",
    );
    both_string("const a = [1, 2, 3]; a.splice(-10, 1.9); a.join()", "2,3");
    both_string(
        "const a = [1, 2, 3, 4, 5, 6, 7]; a.splice(1, 2, ...[8, 9, 10, 11, 12, 13]); a.join()",
        "1,8,9,10,11,12,13,4,5,6,7",
    );
}

#[test]
fn splice_on_array_likes_and_holes() {
    both_string(
        &format!(
            "{LIKE}const r = Array.prototype.splice.call(o, 1, 2, 'x'); o.length + ':' + o[0] + o[1] + o[2] + ('3' in o) + '|' + r.length + ('1' in r)"
        ),
        "3:axdfalse|2false",
    );
    // Holes move as holes.
    both_string(
        "const a = [1, , 3, , 5]; a.splice(0, 1); a.length + ':' + (0 in a) + (1 in a) + (2 in a) + (3 in a)",
        "4:falsetruefalsetrue",
    );
    both_string(
        "const a = [1, , 3]; a.splice(0, 0, 'x'); a.length + ':' + (2 in a) + a[3]",
        "4:false3",
    );
    // The result length is set even past the deleted elements.
    both_number(
        "Array.prototype.splice.call({ length: 2 }, 0, 2).length",
        2.0,
    );
    both_number(
        "const o = { length: 2 }; Array.prototype.splice.call(o, 0, 0, 'a'); o.length",
        3.0,
    );
    // Past 2^53 - 1 the new length throws before any change.
    throws_type_error("Array.prototype.splice.call({ length: 2 ** 53 - 1 }, 0, 0, 'a')");
    both_string(
        "const o = { length: 2 ** 53 - 1 }; try { Array.prototype.splice.call(o, 0, 0, 'a'); } catch (e) {} o.length + ':' + ('0' in o)",
        "9007199254740991:false",
    );
}

#[test]
fn splice_uses_species_constructor() {
    both_bool(
        &format!("{SUB}A.from([1, 2, 3]).splice(0, 1) instanceof A"),
        true,
    );
    both_string(
        "let seen = ''; class B extends Array { constructor(...args) { seen += args.join('/') + ';'; super(...args); } } const b = B.from([1, 2, 3]); seen = ''; b.splice(1, 2); b.splice(0, 0); seen",
        "2;0;",
    );
    both_bool(
        "const a = [1, 2]; a.constructor = undefined; Array.isArray(a.splice(0, 1))",
        true,
    );
    throws_type_error("const a = [1, 2]; a.constructor = 1; a.splice(0, 1)");
}

#[test]
fn sort_default_order() {
    both_string("[3, 1, 2].sort().join()", "1,2,3");
    // Code-unit order of ToString, not numeric order.
    both_string("[10, 9, 1, 100].sort().join()", "1,10,100,9");
    both_string("['b', 'a', 'B', 'aa'].sort().join()", "B,a,aa,b");
    both_bool("const a = [2, 1]; a.sort() === a", true);
    // undefined sorts last, holes after it.
    both_string(
        "const a = [undefined, 'c', , 'b', undefined, , 'a']; a.sort(); a.length + ':' + a.slice(0, 3).join() + ':' + a[3] + a[4] + ':' + (5 in a) + (6 in a) + (4 in a)",
        "7:a,b,c:undefinedundefined:falsefalsetrue",
    );
    both_string(
        "const a = [3, , 1]; a.sort(); a.length + ':' + a[0] + a[1] + (2 in a)",
        "3:13false",
    );
    both_string(
        "const a = [undefined, 2, 1]; a.sort((x, y) => { if (x === undefined || y === undefined) throw 0; return x - y; }); a[0] + ',' + a[1] + ',' + a[2]",
        "1,2,undefined",
    );
}

#[test]
fn sort_with_comparator() {
    both_string("[10, 9, 1, 100].sort((a, b) => a - b).join()", "1,9,10,100");
    both_string("[1, 2, 3].sort((a, b) => b - a).join()", "3,2,1");
    // The result is ToNumber'd.
    both_string(
        "[1, 2, 3].sort((a, b) => ({ valueOf() { return b - a; } })).join()",
        "3,2,1",
    );
    both_string("[1, 2, 3].sort((a, b) => String(b - a)).join()", "3,2,1");
    both_string("[3, 1, 2].sort(() => NaN).join()", "3,1,2");
    both_string("[3, 1, 2].sort(undefined).join()", "1,2,3");
    // The comparator gets (x, y) with this = undefined.
    both_string(
        "let s = ''; [1, 2].sort(function (x, y) { 'use strict'; s += typeof this + arguments.length; return 0; }); s",
        "undefined2",
    );
}

#[test]
fn sort_is_stable() {
    both_string(
        "const a = []; for (let i = 0; i < 40; i++) a.push({ k: i % 3, i }); a.sort((x, y) => x.k - y.k); a.map((x) => x.i).join()",
        "0,3,6,9,12,15,18,21,24,27,30,33,36,39,1,4,7,10,13,16,19,22,25,28,31,34,37,2,5,8,11,14,17,20,23,26,29,32,35,38",
    );
    both_string(
        "const a = ['b1', 'a1', 'b2', 'a2', 'b3']; a.sort((x, y) => x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0); a.join()",
        "a1,a2,b1,b2,b3",
    );
}

#[test]
fn sort_large_array_is_not_quadratic() {
    both_number(
        "const a = []; for (let i = 0; i < 20000; i++) a.push((i * 7919) % 20000); a.sort((x, y) => x - y); let ok = 1; for (let i = 0; i < 20000; i++) if (a[i] !== i) ok = 0; ok",
        1.0,
    );
    both_number(
        "const a = []; for (let i = 0; i < 5000; i++) a.push(5000 - i); let n = 0; a.sort((x, y) => { n++; return x - y; }); n < 5000 * 14 ? a[0] : -1",
        1.0,
    );
}

#[test]
fn sort_comparator_errors() {
    for bad in ["1", "'x'", "{}", "null", "true"] {
        throws_type_error(&format!("[2, 1].sort({bad})"));
    }
    // The comparator check precedes ToObject and any read.
    both_string(
        "let s = ''; try { Array.prototype.sort.call({ get length() { s += 'len'; return 2; } }, 1); } catch (e) { s += e.constructor.name; } s",
        "TypeError",
    );
    throws_type_error("Array.prototype.sort.call(null, 1)");
    // A throwing comparator propagates.
    both_string(
        "let r = ''; try { [1, 2, 3].sort(() => { throw new RangeError('cmp'); }); } catch (e) { r = e.name + ':' + e.message; } r",
        "RangeError:cmp",
    );
    both_string(
        "let r = ''; try { [{ toString() { throw new Error('ts'); } }, 1].sort(); } catch (e) { r = e.message; } r",
        "ts",
    );
}

#[test]
fn sort_on_array_likes_and_subclasses() {
    both_string(
        "const o = { length: 4, 0: 'c', 1: 'a', 3: 'b' }; Array.prototype.sort.call(o); o[0] + o[1] + o[2] + ('3' in o) + o.length",
        "abcfalse4",
    );
    both_string(
        "const o = { length: 3, 0: undefined, 2: 1 }; Array.prototype.sort.call(o); o[0] + ',' + o[1] + ',' + ('2' in o) + ',' + o.hasOwnProperty(1)",
        "1,undefined,false,true",
    );
    both_string(
        &format!("{SUB}const a = A.from([3, 1, 2]); a.sort(); (a instanceof A) + a.join()"),
        "true1,2,3",
    );
    both_string("Array.prototype.sort.call('').length + ''", "0");
}

#[test]
fn reverse_in_place() {
    both_string(
        "const a = [1, 2, 3]; a.reverse() === a && a.join()",
        "3,2,1",
    );
    both_string("[1, 2, 3, 4].reverse().join()", "4,3,2,1");
    both_string("[].reverse().length + ''", "0");
    both_string(
        "const a = [1, , 3, , ]; a.reverse(); a.length + ':' + (0 in a) + (1 in a) + (2 in a) + (3 in a) + a[1] + a[3]",
        "4:falsetruefalsetrue31",
    );
    both_string(
        &format!(
            "{LIKE}Array.prototype.reverse.call(o); o[0] + ('1' in o) + o[2] + o[3] + o.length"
        ),
        "dfalseba4",
    );
    both_string(
        &format!("{LIKE}Array.prototype.reverse.call(o); ('1' in o) + o[0]"),
        "falsed",
    );
    both_string(
        &format!("{SUB}const a = A.from([1, 2]); a.reverse(); (a instanceof A) + a.join()"),
        "true2,1",
    );
}

#[test]
fn shift_and_unshift() {
    both_string(
        "const a = [1, 2, 3]; const x = a.shift(); x + '|' + a.join() + '|' + a.length",
        "1|2,3|2",
    );
    both_bool("[].shift() === undefined", true);
    both_string(
        "const a = [1, , 3]; a.shift(); a.length + ':' + (0 in a) + a[1]",
        "2:false3",
    );
    both_string(
        &format!(
            "{LIKE}const x = Array.prototype.shift.call(o); x + o[0] + ('1' in o) + o[2] + ('3' in o) + o.length"
        ),
        "abfalsedfalse3",
    );
    both_number(
        "const o = { length: 0 }; Array.prototype.shift.call(o); o.length",
        0.0,
    );
    both_number("const o = {}; Array.prototype.shift.call(o); o.length", 0.0);
    both_string(
        "const a = [3, 4]; const n = a.unshift(1, 2); n + '|' + a.join()",
        "4|1,2,3,4",
    );
    both_number("[1].unshift()", 1.0);
    both_string(
        "const a = [1, , 3]; a.unshift('x'); a.length + ':' + a[0] + a[1] + (2 in a) + a[3]",
        "4:x1false3",
    );
    both_string(
        &format!(
            "{LIKE}const n = Array.prototype.unshift.call(o, 'z'); n + o[0] + o[1] + o[2] + ('3' in o) + o[4]"
        ),
        "5zabfalsed",
    );
    both_number(
        "const a = []; for (let i = 0; i < 9; i++) a.unshift(i); a.shift() + a.length",
        16.0,
    );
    throws_type_error("Array.prototype.unshift.call({ length: 2 ** 53 - 1 }, 1)");
    both_number(
        "Array.prototype.unshift.call({ length: 2 ** 53 - 1 })",
        9_007_199_254_740_991.0,
    );
    both_string(
        &format!(
            "{SUB}const a = A.from([2]); a.unshift(1); a.shift(); (a instanceof A) + a.join()"
        ),
        "true2",
    );
}

#[test]
fn fill_with_relative_bounds() {
    both_string("[1, 2, 3, 4].fill(0).join()", "0,0,0,0");
    both_string("[1, 2, 3, 4].fill(0, 1, 3).join()", "1,0,0,4");
    both_string("[1, 2, 3, 4].fill(0, -2).join()", "1,2,0,0");
    both_string("[1, 2, 3, 4].fill(0, -10, -3).join()", "0,2,3,4");
    both_string("[1, 2, 3, 4].fill(0, 2, undefined).join()", "1,2,0,0");
    both_string("[1, 2, 3, 4].fill(0, NaN, 2.5).join()", "0,0,3,4");
    both_bool("const a = [1]; a.fill(2) === a", true);
    both_string(
        "const a = [, , ]; a.fill(7); a.length + ':' + (0 in a) + a[1]",
        "2:true7",
    );
    both_string(
        "const o = { length: 3 }; Array.prototype.fill.call(o, 'x', 1); ('0' in o) + o[1] + o[2]",
        "falsexx",
    );
    both_string(
        &format!("{SUB}const a = A.from([1, 2]); (a.fill(9) instanceof A) + a.join()"),
        "true9,9",
    );
}

#[test]
fn copy_within_moves_ranges() {
    both_string("[1, 2, 3, 4, 5].copyWithin(0, 3).join()", "4,5,3,4,5");
    both_string("[1, 2, 3, 4, 5].copyWithin(1, 0).join()", "1,1,2,3,4");
    both_string("[1, 2, 3, 4, 5].copyWithin(0, 3, 4).join()", "4,2,3,4,5");
    both_string("[1, 2, 3, 4, 5].copyWithin(-2, -3, -1).join()", "1,2,3,3,4");
    both_string(
        "[1, 2, 3, 4, 5].copyWithin(2, 0, undefined).join()",
        "1,2,1,2,3",
    );
    both_bool("const a = [1]; a.copyWithin(0, 0) === a", true);
    // Holes copy as deletions.
    both_string(
        "const a = [1, , 3, 4]; a.copyWithin(2, 0, 2); a.length + ':' + a[2] + (3 in a)",
        "4:1false",
    );
    both_string(
        &format!("{LIKE}Array.prototype.copyWithin.call(o, 0, 2); ('0' in o) + o[1] + o[2] + o[3]"),
        "falsedundefinedd",
    );
    both_string(
        &format!("{SUB}const a = A.from([1, 2, 3]); (a.copyWithin(0, 1) instanceof A) + a.join()"),
        "true2,3,3",
    );
}

#[test]
fn mutating_methods_on_primitive_receivers() {
    both_string("typeof Array.prototype.reverse.call(1)", "object");
    both_string("typeof Array.prototype.fill.call(true, 0)", "object");
    both_bool("Array.prototype.shift.call(5) === undefined", true);
    throws_type_error("Array.prototype.reverse.call('ab')");
}

// --- change-by-copy methods ----------------------------------------------------

/// Assert `src` throws a `RangeError` (caught and tested in the program).
fn throws_range_error(src: &str) {
    both_bool(
        &format!(
            "let ok = false; try {{ {src}; }} catch (e) {{ ok = e instanceof RangeError; }} ok"
        ),
        true,
    );
}

#[test]
fn change_by_copy_methods_have_name_and_length() {
    for (name, length) in [
        ("with", 2.0),
        ("toReversed", 0.0),
        ("toSorted", 1.0),
        ("toSpliced", 2.0),
    ] {
        both_string(&format!("Array.prototype.{name}.name"), name);
        both_number(&format!("Array.prototype.{name}.length"), length);
    }
}

#[test]
fn with_replaces_one_index_in_a_copy() {
    both_string(
        "const a = [1, 2, 3]; a.with(1, 9).join() + ':' + a.join()",
        "1,9,3:1,2,3",
    );
    both_string("[1, 2, 3].with(-1, 9).join()", "1,2,9");
    both_string("[1, 2, 3].with(1.7, 9).join()", "1,9,3");
    both_bool("const a = [1]; a.with(0, 1) !== a", true);
    // Holes read as `undefined`.
    both_string(
        "const b = [, 2].with(1, 3); (0 in b) + ':' + b.length",
        "true:2",
    );
    throws_range_error("[1, 2].with(2, 0)");
    throws_range_error("[1, 2].with(-3, 0)");
    throws_range_error("[].with(0, 0)");
    both_string(
        &format!(
            "{LIKE}const b = Array.prototype.with.call(o, 1, 'x'); Array.isArray(b) + b.join()"
        ),
        "truea,x,,d",
    );
    both_string(
        &format!(
            "{SUB}const b = A.from([1, 2]).with(0, 3); (b instanceof A) + ':' + Array.isArray(b)"
        ),
        "false:true",
    );
    throws_type_error("Array.prototype.with.call(null, 0, 0)");
}

#[test]
fn to_reversed_copies_in_reverse() {
    both_string(
        "const a = [1, 2, 3]; a.toReversed().join() + ':' + a.join()",
        "3,2,1:1,2,3",
    );
    both_string("[].toReversed().length + ''", "0");
    both_string(
        "const b = [1, , 3].toReversed(); (1 in b) + ':' + b[1]",
        "true:undefined",
    );
    both_string(
        &format!("{LIKE}Array.prototype.toReversed.call(o).join()"),
        "d,,b,a",
    );
    both_string(
        &format!("{SUB}const b = A.from([1, 2]).toReversed(); (b instanceof A) + b.join()"),
        "false2,1",
    );
    throws_type_error("Array.prototype.toReversed.call(undefined)");
}

#[test]
fn to_sorted_sorts_a_copy() {
    both_string(
        "const a = [3, 1, 2]; a.toSorted().join() + ':' + a.join()",
        "1,2,3:3,1,2",
    );
    both_string("[10, 9, 1].toSorted().join()", "1,10,9");
    both_string("[1, 3, 2].toSorted((x, y) => y - x).join()", "3,2,1");
    // Holes become `undefined`, sorted last.
    both_string(
        "const b = [3, , 1, undefined].toSorted(); b.length + ':' + (1 in b) + (3 in b) + b[0] + b[1]",
        "4:truetrue13",
    );
    both_string(
        &format!("{LIKE}Array.prototype.toSorted.call(o).join()"),
        "a,b,d,",
    );
    both_string(
        &format!("{SUB}const b = A.from([2, 1]).toSorted(); (b instanceof A) + b.join()"),
        "false1,2",
    );
    // The comparator check comes before `ToObject(this)`.
    throws_type_error("Array.prototype.toSorted.call(null, 1)");
    throws_type_error("[1, 2].toSorted(null)");
    // Stable.
    both_string(
        "[{k: 1, v: 'a'}, {k: 0, v: 'b'}, {k: 1, v: 'c'}, {k: 0, v: 'd'}].toSorted((x, y) => x.k - y.k).map(e => e.v).join('')",
        "bdac",
    );
}

#[test]
fn to_spliced_splices_a_copy() {
    both_string(
        "const a = [1, 2, 3, 4]; a.toSpliced(1, 2, 'x').join() + ':' + a.join()",
        "1,x,4:1,2,3,4",
    );
    both_string("[1, 2, 3].toSpliced().join()", "1,2,3");
    both_string("[1, 2, 3].toSpliced(1).join()", "1");
    both_string("[1, 2, 3].toSpliced(-1, 0, 'a', 'b').join()", "1,2,a,b,3");
    both_string("[1, 2, 3].toSpliced(1, -5, 'a').join()", "1,a,2,3");
    both_string("[1, 2, 3].toSpliced(1, 99).join()", "1");
    both_string(
        "[1, 2, 3].toSpliced(0, 0, 4, 5, 6, 7, 8, 9).join()",
        "4,5,6,7,8,9,1,2,3",
    );
    both_string(
        "const b = [1, , 3].toSpliced(0, 0); (1 in b) + ':' + b.length",
        "true:3",
    );
    both_string(
        &format!("{LIKE}Array.prototype.toSpliced.call(o, 1, 1).join()"),
        "a,,d",
    );
    both_string(
        &format!("{SUB}const b = A.from([1, 2]).toSpliced(0, 1); (b instanceof A) + b.join()"),
        "false2",
    );
    throws_type_error("Array.prototype.toSpliced.call({ length: 2 ** 53 - 1 }, 0, 0, 1)");
    throws_range_error("Array.prototype.toSpliced.call({ length: 2 ** 32 }, 0, 0)");
    throws_range_error("Array.prototype.toReversed.call({ length: 2 ** 32 })");
    throws_range_error("Array.prototype.toSorted.call({ length: 2 ** 32 })");
    throws_range_error("Array.prototype.with.call({ length: 2 ** 32 }, 0, 0)");
}

#[test]
fn generic_methods_use_the_full_length_range() {
    const BIG: &str = "const n = 2 ** 53 - 1; ";
    both_string(
        &format!(
            "{BIG}const o = {{ length: Infinity }}; o[n - 1] = 'x'; Array.prototype.pop.call(o) + ':' + (o.length === n - 1)"
        ),
        "x:true",
    );
    both_bool(
        &format!("{BIG}const o = {{ length: n }}; o[n - 2] = 'c'; [].includes.call(o, 'c', n - 2)"),
        true,
    );
    both_bool(
        &format!(
            "{BIG}const o = {{ length: n }}; o[n - 2] = 'c'; [].indexOf.call(o, 'c', n - 3) === n - 2"
        ),
        true,
    );
    both_bool(
        &format!(
            "{BIG}const o = {{ length: n }}; o[n - 3] = 'c'; [].lastIndexOf.call(o, 'c', n - 2) === n - 3"
        ),
        true,
    );
    both_bool(
        &format!(
            "{BIG}let seen = -1; [].findLast.call({{ length: Infinity }}, (v, i) => {{ seen = i; return true; }}); seen === n - 1"
        ),
        true,
    );
    both_bool(
        &format!("{BIG}[].findLastIndex.call({{ length: Infinity }}, () => true) === n - 1"),
        true,
    );
    both_string(
        &format!(
            "{BIG}const o = {{ length: n }}; o[n - 1] = 1; o[n - 3] = 3; const r = []; try {{ [].reduceRight.call(o, (acc, v) => {{ r.push(v); if (v === 3) throw 0; return acc; }}, 0); }} catch (e) {{}} r.join()"
        ),
        "1,3",
    );
    both_string(
        &format!(
            "{BIG}const o = {{ length: n, [n - 2]: 'a', [n - 3]: 'b' }}; [].slice.call(o, n - 3, n - 1).join()"
        ),
        "b,a",
    );
    throws_range_error("[].slice.call({ length: 2 ** 32 + 1 }, 0)");
    throws_range_error("[].map.call({ length: 2 ** 32 }, () => 0)");
}

#[test]
fn push_past_the_array_length_limit_throws() {
    throws_range_error("const y = []; y.length = 2 ** 32 - 1; Array.prototype.push.call(y, 'x')");
    both_string(
        "const y = []; y.length = 2 ** 32 - 1; try { Array.prototype.push.call(y, 'x'); } catch (e) {} y[2 ** 32 - 1] + ':' + (y.length === 2 ** 32 - 1)",
        "x:true",
    );
    // The `arr.push(x)` call-site fast path throws the same RangeError.
    throws_range_error("const y = []; y.length = 2 ** 32 - 1; y.push('x')");
    both_string(
        "const y = []; y.length = 2 ** 32 - 1; try { y.push('x'); } catch (e) {} y[2 ** 32 - 1] + ':' + (y.length === 2 ** 32 - 1) + ':' + y.push()",
        "x:true:4294967295",
    );
}

#[test]
fn to_locale_string_invokes_each_element() {
    both_string(
        "const p = Array.prototype.toLocaleString; p.name + ':' + p.length",
        "toLocaleString:0",
    );
    both_string(
        "[1, null, undefined, 2.5, , 3].toLocaleString()",
        "1,,,2.5,,3",
    );
    both_string("[].toLocaleString()", "");
    // Each element's own `toLocaleString`, called with it as `this`.
    both_string(
        "const e = { toLocaleString() { return 'E' + (this === e); } }; [e, 1, e].toLocaleString()",
        "Etrue,1,Etrue",
    );
    // Generic over array-likes; the result is ToString'd.
    both_string(
        "const o = { length: 2, 0: { toLocaleString: () => 7 }, 1: 4 }; Array.prototype.toLocaleString.call(o)",
        "7,4",
    );
    both_string(
        "class A extends Array {} const a = new A(); a.push(1, 2); a.toLocaleString()",
        "1,2",
    );
    throws_type_error("Array.prototype.toLocaleString.call(null)");
    throws_type_error("[{ toLocaleString: 1 }].toLocaleString()");
    // A throwing element stops the walk.
    both_string(
        "let n = 0; const t = { toLocaleString() { n++; throw 'x'; } }; try { [t, t].toLocaleString(); } catch (e) {} '' + n",
        "1",
    );
}
