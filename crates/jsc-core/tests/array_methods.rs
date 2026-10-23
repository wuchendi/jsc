//! Integration tests: `Array.of` / `Array.from`, the non-mutating
//! `%Array.prototype%` methods (`at`, `find*`, `some`, `every`,
//! `reduceRight`, `lastIndexOf`, `concat`, `flat`, `flatMap`) and
//! `ArraySpeciesCreate` by `constructor` (DESIGN.md §6.4, §13): generic over
//! array-likes, holes, subclass receivers.

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
