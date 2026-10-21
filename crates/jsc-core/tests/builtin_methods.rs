//! Integration tests: `%Array.prototype%` methods as real builtin function
//! values (design 4.2, S5), their guarded call-site fast paths (design 3.2)
//! and their generic behaviour over array-likes.

mod common;
use common::{assert_bool, assert_number, assert_string, assert_uncaught};

/// A prefix that makes a program not intrinsics-sealed without writing any
/// prototype, so the call-site fast paths run behind their run-time guards.
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

// --- builtin function values ---------------------------------------------------

#[test]
fn array_methods_are_function_values() {
    for name in [
        "push", "pop", "map", "filter", "reduce", "forEach", "indexOf", "includes", "join",
        "slice", "toString",
    ] {
        both_string(&format!("typeof [].{name}"), "function");
        both_bool(&format!("[].{name} === Array.prototype.{name}"), true);
        both_string(&format!("Array.prototype.{name}.name"), name);
    }
    for (name, length) in [
        ("push", 1.0),
        ("pop", 0.0),
        ("map", 1.0),
        ("filter", 1.0),
        ("reduce", 1.0),
        ("forEach", 1.0),
        ("indexOf", 1.0),
        ("includes", 1.0),
        ("join", 1.0),
        ("slice", 2.0),
    ] {
        both_number(&format!("Array.prototype.{name}.length"), length);
    }
    both_bool("Array.prototype.hasOwnProperty('push')", true);
    both_bool("[].hasOwnProperty('push')", false);
}

// --- DF12: user methods named like builtins ----------------------------------------

#[test]
fn user_methods_named_like_array_builtins_are_called() {
    both_number("const o = { push(x) { return 42; } }; o.push(1)", 42.0);
    both_string(
        "class C { map(f) { return 'mine'; } }; new C().map((x) => x)",
        "mine",
    );
    both_string(
        "const o = { indexOf(x) { return 'user ' + x; } }; o.indexOf(3)",
        "user 3",
    );
    both_string(
        "const o = { pop() { return 'p'; }, join() { return 'j'; }, slice() { return 's'; } }; o.pop() + o.join() + o.slice()",
        "pjs",
    );
    both_string(
        "const o = { filter() { return 'f'; }, reduce() { return 'r'; }, forEach() { return 'e'; }, includes() { return 'i'; } }; o.filter() + o.reduce() + o.forEach() + o.includes()",
        "frei",
    );
}

#[test]
fn own_methods_on_arrays_are_called() {
    both_string(
        "const a = [1]; a.push = function () { return 'own'; }; a.push(2)",
        "own",
    );
    both_string(
        "const a = [1]; a.map = function () { return 'own map'; }; a.map((x) => x)",
        "own map",
    );
    both_number(
        "const a = [1]; a.push = function () { return 'own'; }; a.push(2); a.length",
        1.0,
    );
}

#[test]
fn prototype_overrides_are_called() {
    assert_string(
        "Array.prototype.push = function () { return 'proto'; }; [].push(1)",
        "proto",
    );
    assert_string(
        "Array.prototype.map = function () { return 'pm'; }; [1].map((x) => x)",
        "pm",
    );
    assert_string(
        "Array.prototype.indexOf = function () { return 'pi'; }; [1].indexOf(1)",
        "pi",
    );
    assert_string(
        "class A extends Array { push(x) { return 'sub'; } }; new A().push(1)",
        "sub",
    );
}

// --- arity ----------------------------------------------------------------------

#[test]
fn push_takes_any_number_of_arguments() {
    both_number("const a = [0]; a.push(1, 2, 3)", 4.0);
    both_string("const a = [0]; a.push(1, 2, 3); a.join()", "0,1,2,3");
    both_number("const a = [5]; a.push()", 1.0);
    both_string(
        "const a = [0]; const xs = [1, 2]; a.push(...xs); a.join()",
        "0,1,2",
    );
    both_number("const a = []; a.push(...[1, 2, 3, 4, 5, 6])", 6.0);
    both_string(
        "const a = []; a.push(...[1, 2, 3, 4, 5, 6]); a.join('-')",
        "1-2-3-4-5-6",
    );
}

#[test]
fn extra_arguments_are_ignored_and_missing_are_undefined() {
    both_number("const a = [1, 2]; a.pop(9, 9)", 2.0);
    both_number("[1, 2, 3].indexOf(2, 0, 9)", 1.0);
    both_bool("[1, undefined].includes()", true);
    both_number("[1, undefined].indexOf()", 1.0);
    both_string("String([1, 2, 3].slice())", "1,2,3");
    both_string("String([1, 2, 3].slice(1, 2, 3))", "2");
}

// --- callbacks ------------------------------------------------------------------

#[test]
fn callbacks_receive_index_and_array_and_this_arg() {
    both_string(
        "const out = []; [7, 8].forEach((x, i, arr) => out.push(x + ':' + i + ':' + arr.length)); out.join()",
        "7:0:2,8:1:2",
    );
    both_number(
        "const o = { k: 10 }; [1, 2].map(function (x) { return x * this.k; }, o)[1]",
        20.0,
    );
    both_number(
        "[1, 2, 3].reduce((acc, x, i, arr) => acc + x * arr.length, 0)",
        18.0,
    );
    both_number("[1, 2, 3].reduce((acc, x) => acc + x)", 6.0);
    both_string(
        "String([1, 2, 3, 4].filter(function (x) { return x > this.min; }, { min: 2 }))",
        "3,4",
    );
}

#[test]
fn non_callable_callbacks_throw_type_error() {
    for name in ["map", "filter", "forEach", "reduce"] {
        both_bool(
            &format!(
                "let ok = false; try {{ [1].{name}(5); }} catch (e) {{ ok = e instanceof TypeError; }} ok"
            ),
            true,
        );
        both_bool(
            &format!(
                "let ok = false; try {{ Array.prototype.{name}.call({{ length: 0 }}, null); }} catch (e) {{ ok = e instanceof TypeError; }} ok"
            ),
            true,
        );
    }
}

#[test]
fn reduce_of_empty_without_initial_value_throws() {
    both_bool(
        "let ok = false; try { [].reduce((a, b) => a + b); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
    both_bool(
        "let ok = false; try { Array.prototype.reduce.call({ length: 0 }, (a, b) => a); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
    both_number("[, 4, , 5].reduce((a, b) => a + b)", 9.0);
    both_number("[].reduce((a, b) => a + b, 7)", 7.0);
}

#[test]
fn callbacks_skip_holes_and_removed_elements() {
    both_string(
        "const seen = []; [1, , 3].forEach((x, i) => seen.push(i)); seen.join()",
        "0,2",
    );
    // Elements popped by the callback are not visited.
    both_string(
        "const a = [1, 2, 3, 4]; const seen = []; a.forEach((x) => { seen.push(x); a.pop(); }); seen.join()",
        "1,2",
    );
}

// --- array-likes ----------------------------------------------------------------

const LIKE: &str = "const o = { length: 3, 0: 'a', 1: 'b', 2: 'c' }; ";

#[test]
fn generic_methods_over_array_likes() {
    both_number(
        &format!("{LIKE}Array.prototype.push.call(o, 'd', 'e')"),
        5.0,
    );
    both_string(
        &format!("{LIKE}Array.prototype.push.call(o, 'd'); o[3] + o.length"),
        "d4",
    );
    both_string(
        &format!("{LIKE}Array.prototype.pop.call(o) + o.length"),
        "c2",
    );
    both_number(
        "const o = { length: 0 }; Array.prototype.pop.call(o); o.length",
        0.0,
    );
    both_string(
        &format!("{LIKE}Array.prototype.map.call(o, (x) => x + x).join()"),
        "aa,bb,cc",
    );
    both_string(
        &format!("{LIKE}Array.prototype.filter.call(o, (x) => x !== 'b').join()"),
        "a,c",
    );
    both_string(
        &format!("{LIKE}Array.prototype.reduce.call(o, (acc, x) => acc + x, '>')"),
        ">abc",
    );
    both_string(
        &format!(
            "{LIKE}let s = ''; Array.prototype.forEach.call(o, (x, i, obj) => {{ s += x + i + (obj === o); }}); s"
        ),
        "a0trueb1truec2true",
    );
    both_number(&format!("{LIKE}Array.prototype.indexOf.call(o, 'c')"), 2.0);
    both_number(&format!("{LIKE}Array.prototype.indexOf.call(o, 'z')"), -1.0);
    both_bool(
        &format!("{LIKE}Array.prototype.includes.call(o, 'b')"),
        true,
    );
    both_bool(
        &format!("{LIKE}Array.prototype.includes.call(o, 'z')"),
        false,
    );
    both_string(&format!("{LIKE}Array.prototype.join.call(o)"), "a,b,c");
    both_string(&format!("{LIKE}Array.prototype.join.call(o, '-')"), "a-b-c");
    both_string(
        &format!("{LIKE}Array.prototype.slice.call(o, 1).join()"),
        "b,c",
    );
    both_string(
        &format!("{LIKE}Array.prototype.slice.call(o, -2, -1).join()"),
        "b",
    );
}

#[test]
fn generic_methods_over_strings() {
    both_string(
        "Array.prototype.map.call('ab', (c) => c + c).join()",
        "aa,bb",
    );
    both_number("Array.prototype.indexOf.call('abc', 'c')", 2.0);
    both_string("Array.prototype.join.call('abc', '.')", "a.b.c");
    both_string("Array.prototype.slice.call('abc', 1).join()", "b,c");
}

#[test]
fn array_likes_skip_absent_indices() {
    both_string(
        "const o = { length: 3, 0: 'x', 2: 'z' }; Array.prototype.map.call(o, (v) => v + '!').join()",
        "x!,,z!",
    );
    both_number(
        "const o = { length: 3, 0: 'x', 2: 'z' }; Array.prototype.map.call(o, (v) => v).length",
        3.0,
    );
    both_bool(
        "const o = { length: 2, 1: 'y' }; Array.prototype.includes.call(o, undefined)",
        true,
    );
    both_number(
        "const o = { length: 2, 1: 'y' }; Array.prototype.indexOf.call(o, undefined)",
        -1.0,
    );
}

#[test]
fn nullish_receivers_throw_type_error() {
    for name in ["push", "pop", "map", "indexOf", "join", "slice"] {
        both_bool(
            &format!(
                "let ok = false; try {{ Array.prototype.{name}.call(undefined, 1); }} catch (e) {{ ok = e instanceof TypeError; }} ok"
            ),
            true,
        );
    }
    assert_uncaught(
        "let a; a.push(1)",
        "TypeError: Cannot read properties of undefined (reading 'push')",
    );
}

// --- strings keep their methods ------------------------------------------------------

#[test]
fn string_receivers_keep_shared_method_names() {
    both_number("'hello'.indexOf('l')", 2.0);
    both_bool("'hello'.includes('ell')", true);
    both_string("'hello'.slice(1, 3)", "el");
    both_string("const s = 'hey'; s.slice(-2)", "ey");
    both_bool(
        "let ok = false; try { 'abc'.push(1); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
}

#[test]
fn push_past_the_maximum_length_throws() {
    both_bool(
        "const o = { length: 2 ** 53 - 1 }; let ok = false; try { Array.prototype.push.call(o, 1); } catch (e) { ok = e instanceof TypeError; } ok",
        true,
    );
    both_number(
        "const o = { length: 2 ** 32 + 1 }; Array.prototype.push.call(o, 'x')",
        4_294_967_298.0,
    );
}
