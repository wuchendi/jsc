//! Integration tests: destructuring `catch` parameters (`catch ({message})`,
//! `catch ([a, b = 2, ...r])`) — bound like a `let` from the thrown value in
//! the catch clause's scope, a destructuring throw escaping the clause.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string};

#[test]
fn an_object_pattern_binds_the_thrown_value_properties() {
    assert_string(
        "let r; try { throw new TypeError('boom') } catch ({message, name}) { r = name + ': ' + message } r",
        "TypeError: boom",
    );
    assert_number(
        "let r; try { throw {a: 1, b: {c: 2}} } catch ({a, b: {c: d}}) { r = a + d } r",
        3.0,
    );
    // Shorthand and renamed defaults fill an `undefined` read.
    assert_number(
        "let r; try { throw {a: undefined} } catch ({a = 5, b: x = 7}) { r = a * x } r",
        35.0,
    );
    // An object rest collects the remaining own keys.
    assert_string(
        "let r; try { throw {a: 1, b: 2, c: 3} } catch ({a, ...rest}) { r = JSON.stringify(rest) } r",
        r#"{"b":2,"c":3}"#,
    );
}

#[test]
fn an_array_pattern_binds_through_the_iterator() {
    assert_string(
        "let r; try { throw [1, undefined, 3, 4] } catch ([a, b = 2, ...r2]) { r = [a, b, r2.join('|')].join() } r",
        "1,2,3|4",
    );
    assert_string(
        "let r; try { throw [[1, {x: 2}]] } catch ([[a, {x, y = 9}]]) { r = [a, x, y].join() } r",
        "1,2,9",
    );
    // A hole skips an element.
    assert_number(
        "let r; try { throw [1, 2, 3] } catch ([, , c]) { r = c } r",
        3.0,
    );
}

#[test]
fn the_bindings_are_mutable_and_scoped_to_the_clause() {
    assert_number(
        "let r; try { throw {a: 1} } catch ({a}) { a += 10; r = a } r",
        11.0,
    );
    assert_string(
        "let a = 'outer'; try { throw {a: 'inner'} } catch ({a}) {} a",
        "outer",
    );
    assert_string(
        "let a = 'outer'; let r; try { throw {} } catch ({b = a}) { r = b } r",
        "outer",
    );
}

#[test]
fn a_closure_captures_a_destructured_catch_binding() {
    assert_number(
        "let f; try { throw {x: 1} } catch ({x}) { f = () => x; x = 2 } f()",
        2.0,
    );
    assert_string(
        "let fs = []; for (const v of ['a', 'b']) { try { throw [v] } catch ([x]) { fs.push(() => x) } } fs.map(f => f()).join()",
        "a,b",
    );
    // A default closure sees the outer binding.
    assert_number(
        "let k = 4; let f; try { throw {} } catch ({g = () => k}) { f = g } f()",
        4.0,
    );
}

#[test]
fn a_destructuring_throw_escapes_the_clause_and_runs_finally() {
    // `null` into an object pattern: a TypeError caught by the outer `try`.
    assert_bool(
        "let r = false; try { try { throw null } catch ({a}) { r = 'body' } } catch (e) { r = e instanceof TypeError } r",
        true,
    );
    assert_bool(
        "let r = false; try { try { throw undefined } catch ({}) {} } catch (e) { r = e instanceof TypeError } r",
        true,
    );
    // A non-iterable into an array pattern.
    assert_bool(
        "let r = false; try { try { throw 5 } catch ([a]) {} } catch (e) { r = e instanceof TypeError } r",
        true,
    );
    // A throwing default replaces the exception.
    assert_string(
        "let r; try { try { throw {} } catch ({a = (() => { throw 'dflt' })()}) {} } catch (e) { r = e } r",
        "dflt",
    );
    assert_output(
        "try { try { throw null } catch ({a}) { console.log('body') } finally { console.log('finally') } } catch (e) { console.log(e instanceof TypeError) }",
        "finally\ntrue\n",
    );
}

#[test]
fn an_array_pattern_closes_the_iterator() {
    assert_output(
        "let it = { [Symbol.iterator]() { return this }, i: 0, \
           next() { return {value: ++this.i, done: false} }, \
           return() { console.log('closed'); return {} } }; \
         try { throw it } catch ([a, b]) { console.log(a, b) }",
        "closed\n1 2\n",
    );
    // A throwing default closes the iterator before the throw escapes.
    assert_output(
        "let it = { [Symbol.iterator]() { return this }, \
           next() { return {value: undefined, done: false} }, \
           return() { console.log('closed'); return {} } }; \
         try { try { throw it } catch ([a = (() => { throw 'x' })()]) {} } catch (e) { console.log(e) }",
        "closed\nx\n",
    );
}

#[test]
fn a_catch_pattern_works_in_a_generator() {
    assert_string(
        "function* g() { try { throw {a: 1, b: [2, 3]} } catch ({a, b: [c, ...d]}) { yield a; yield c; yield d.length } } \
         [...g()].join()",
        "1,2,1",
    );
    // A `yield` in a default suspends mid-destructuring.
    assert_number(
        "function* g() { try { throw {} } catch ({a = yield 'need'}) { return a * 2 } } \
         let it = g(); it.next(); it.next(21).value",
        42.0,
    );
    // A throw from a pattern inside a generator reaches the outer handler.
    assert_bool(
        "function* g() { try { try { throw null } catch ({a}) {} } catch (e) { yield e instanceof TypeError } } \
         g().next().value",
        true,
    );
    // A captured destructured binding across a suspend.
    assert_number(
        "function* g() { try { throw {x: 1} } catch ({x}) { const f = () => x; yield 0; x = 5; yield f() } } \
         let it = g(); it.next(); it.next().value",
        5.0,
    );
}

#[test]
fn a_catch_pattern_works_in_an_async_function() {
    assert_output(
        "async function f() { try { await Promise.reject({code: 7, list: [1, 2]}) } \
           catch ({code, list: [, second]}) { console.log(code, second) } } \
         f()",
        "7 2\n",
    );
    assert_output(
        "async function f() { try { throw {} } catch ({v = await Promise.resolve('awaited')}) { console.log(v) } } \
         f()",
        "awaited\n",
    );
}
