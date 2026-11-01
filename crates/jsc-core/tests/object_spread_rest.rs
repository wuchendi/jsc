//! Integration tests: object spread in literals (`{...o}`) and object rest in
//! binding, assignment, and parameter patterns — both `CopyDataProperties`.

mod common;
use common::{assert_bool, assert_number, assert_output, assert_string, assert_uncaught};

// ---------------------------------------------------------------------------
// Object spread
// ---------------------------------------------------------------------------

#[test]
fn spread_copies_in_source_order_and_overwrites() {
    assert_string(
        "let o = {b: 2, c: 3}; JSON.stringify({a: 1, ...o, d: 4})",
        r#"{"a":1,"b":2,"c":3,"d":4}"#,
    );
    // A later property overwrites a spread key; a spread overwrites an
    // earlier one, keeping the earlier key's position.
    assert_string(
        "let o = {a: 2, b: 3}; JSON.stringify({a: 1, b: 0, ...o, b: 9})",
        r#"{"a":2,"b":9}"#,
    );
    assert_string(
        "let p = {x: 1}; let q = {x: 2, y: 3}; JSON.stringify({...p, ...q})",
        r#"{"x":2,"y":3}"#,
    );
    // Integer keys first (ascending), then strings in insertion order.
    assert_string(
        "let o = {b: 1, 2: 'two', a: 2, 1: 'one'}; Object.keys({...o}).join()",
        "1,2,b,a",
    );
    // The spread object is a fresh copy.
    assert_bool(
        "let o = {a: 1}; let c = {...o}; c.a = 5; c !== o && o.a === 1",
        true,
    );
}

#[test]
fn spread_interleaves_evaluation_order() {
    assert_output(
        "let log = []; let f = (n) => { log.push(n); return n }; \
         let o = {a: f(1), ...f({b: 2}), [f('c')]: f(3), ...(f(4), {d: 5})}; \
         console.log(JSON.stringify(log), JSON.stringify(o))",
        "[1,{\"b\":2},\"c\",3,4] {\"a\":1,\"b\":2,\"c\":3,\"d\":5}\n",
    );
}

#[test]
fn spread_runs_getters_once_and_copies_values() {
    assert_output(
        "let n = 0; let src = { get g() { n++; return 7 } }; let o = {...src}; \
         let d = Object.getOwnPropertyDescriptor(o, 'g'); \
         console.log(n, o.g, n, 'value' in d, d.get === undefined, d.writable, d.enumerable, d.configurable)",
        "1 7 1 true true true true true\n",
    );
}

#[test]
fn spread_copies_only_own_enumerable_properties() {
    assert_string(
        "let p = {inherited: 1}; let o = Object.create(p); o.own = 2; \
         Object.defineProperty(o, 'hidden', {value: 3, enumerable: false}); \
         Object.keys({...o}).join() + ':' + ('inherited' in {...o}) + ':' + ('hidden' in {...o})",
        "own:false:false",
    );
}

#[test]
fn spread_copies_symbol_keys_after_strings() {
    assert_output(
        "let s = Symbol('s'); let o = {[s]: 1, a: 2}; let c = {...o}; \
         console.log(c[s], c.a, Object.getOwnPropertySymbols(c).length === 1, Object.getOwnPropertySymbols(c)[0] === s)",
        "1 2 true true\n",
    );
}

#[test]
fn spread_skips_nullish_and_wraps_primitives() {
    assert_string(
        "JSON.stringify({a: 1, ...null, ...undefined})",
        r#"{"a":1}"#,
    );
    assert_string("JSON.stringify({...'hi'})", r#"{"0":"h","1":"i"}"#);
    assert_string("JSON.stringify({...5, ...true, x: 1})", r#"{"x":1}"#);
}

#[test]
fn spread_of_arrays_functions_and_instances() {
    assert_string(
        "let a = [1, 2]; a.extra = 'e'; JSON.stringify({...a})",
        r#"{"0":1,"1":2,"extra":"e"}"#,
    );
    assert_string(
        "function f() {} f.p = 1; JSON.stringify({...f})",
        r#"{"p":1}"#,
    );
    assert_string(
        "class C { constructor() { this.x = 1 } m() {} } JSON.stringify({...new C()})",
        r#"{"x":1}"#,
    );
}

#[test]
fn spread_defines_without_invoking_setters() {
    // Defining never runs a setter on the target or Object.prototype.
    assert_output(
        "let hits = 0; Object.defineProperty(Object.prototype, 'z', \
         { set(v) { hits++ }, configurable: true }); \
         let o = {...{z: 1}}; console.log(hits, Object.keys(o).join(), o.z)",
        "0 z 1\n",
    );
    // A spread key replaces an earlier accessor with a data property.
    assert_output(
        "let o = { get a() { return 0 }, ...{a: 1} }; \
         console.log(o.a, Object.getOwnPropertyDescriptor(o, 'a').get === undefined)",
        "1 true\n",
    );
}

#[test]
fn spread_proto_key_is_an_own_data_property() {
    assert_output(
        "let src = JSON.parse('{\"__proto__\": 5}'); let o = {...src}; \
         console.log(Object.getPrototypeOf(o) === Object.prototype, Object.keys(o).join(), o.hasOwnProperty('__proto__'))",
        "true __proto__ true\n",
    );
    // A literal `__proto__:` entry around a spread still sets the prototype.
    assert_output(
        "let p = {inherited: 1}; let o = {...{a: 1}, __proto__: p, b: 2}; \
         console.log(Object.getPrototypeOf(o) === p, o.inherited, Object.keys(o).join())",
        "true 1 a,b\n",
    );
    assert_output(
        "let p = {inherited: 1}; let o = {__proto__: p, ...{a: 1}}; \
         console.log(Object.getPrototypeOf(o) === p, Object.keys(o).join())",
        "true a\n",
    );
}

#[test]
fn spread_with_methods_and_accessors_after_it() {
    assert_number(
        "let o = {...{x: 1}, get y() { return this.x + 1 }, m() { return this.y * 10 }}; o.m()",
        20.0,
    );
}

#[test]
fn spread_in_generator_and_async_bodies() {
    assert_string(
        "function* g() { const o = {a: 1, ...(yield 0), c: 3}; return JSON.stringify(o) } \
         let it = g(); it.next(); it.next({b: 2}).value",
        r#"{"a":1,"b":2,"c":3}"#,
    );
    assert_output(
        "async function f() { return {...(await Promise.resolve({k: 1})), j: 2} } \
         f().then(o => console.log(JSON.stringify(o)))",
        "{\"k\":1,\"j\":2}\n",
    );
}

// ---------------------------------------------------------------------------
// Object rest
// ---------------------------------------------------------------------------

#[test]
fn rest_excludes_destructured_keys() {
    assert_string(
        "let {a, b: x, ...r} = {a: 1, b: 2, c: 3, d: 4}; JSON.stringify(r) + a + x",
        r#"{"c":3,"d":4}12"#,
    );
    // An excluded integer key compares as a property key.
    assert_string(
        "let {1: one, ...r} = {1: 'x', 2: 'y'}; JSON.stringify(r) + one",
        r#"{"2":"y"}x"#,
    );
}

#[test]
fn rest_is_a_fresh_ordinary_object() {
    assert_bool(
        "let src = Object.create({inherited: 1}); src.a = 1; let {...r} = src; \
         Object.getPrototypeOf(r) === Object.prototype && r !== src && !('inherited' in r)",
        true,
    );
}

#[test]
fn rest_runs_getters_and_copies_symbols() {
    assert_output(
        "let n = 0; let s = Symbol(); let src = { get g() { n++; return 3 }, [s]: 4 }; \
         let {...r} = src; console.log(n, r.g, n, r[s], Object.getOwnPropertyDescriptor(r, 'g').get === undefined)",
        "1 3 1 4 true\n",
    );
    // A getter of a destructured key runs exactly once (the read), never for rest.
    assert_number(
        "let n = 0; let src = { get a() { return ++n }, b: 1 }; let {a, ...r} = src; n * 10 + a",
        11.0,
    );
}

#[test]
fn rest_of_nullish_throws_type_error_before_reads() {
    assert_uncaught(
        "let {...r} = null;",
        "TypeError: Cannot read properties of null (reading '')",
    );
    assert_string(
        "let msg = ''; try { let {...r} = undefined } catch (e) { msg = e.constructor.name } msg",
        "TypeError",
    );
    // RequireObjectCoercible runs before the rest target reference.
    assert_output(
        "let log = []; let o = { get t() { log.push('t'); return {} } }; \
         try { ({...o.t.r} = null) } catch (e) { log.push(e instanceof TypeError) } \
         console.log(log.join())",
        "true\n",
    );
}

#[test]
fn rest_of_primitive_source() {
    assert_string(
        "let {...r} = 'ab'; JSON.stringify(r)",
        r#"{"0":"a","1":"b"}"#,
    );
    assert_string("let {...r} = 5; JSON.stringify(r)", "{}");
}

#[test]
fn nested_rest_patterns() {
    assert_string(
        "let {a: {x, ...inner}, ...outer} = {a: {x: 1, y: 2}, b: 3}; \
         JSON.stringify([x, inner, outer])",
        r#"[1,{"y":2},{"b":3}]"#,
    );
    assert_string("let [{...r}] = [{p: 1}]; JSON.stringify(r)", r#"{"p":1}"#);
}

#[test]
fn assignment_pattern_rest_into_member() {
    assert_string(
        "let o = {}; let a; ({a, ...o.r} = {a: 1, b: 2, c: 3}); JSON.stringify(o.r) + a",
        r#"{"b":2,"c":3}1"#,
    );
    assert_string(
        "let o = {}; ({...o['k']} = {z: 1}); JSON.stringify(o)",
        r#"{"k":{"z":1}}"#,
    );
}

#[test]
fn rest_in_generator_body() {
    assert_string(
        "function* g() { let {a, ...r} = yield 1; return JSON.stringify(r) + a } \
         let it = g(); it.next(); it.next({a: 1, b: 2}).value",
        r#"{"b":2}1"#,
    );
}

// ---------------------------------------------------------------------------
// Rest parameters with a pattern (BindingRestElement)
// ---------------------------------------------------------------------------

#[test]
fn rest_parameter_patterns() {
    assert_number(
        "function f(...[a, b]) { return a * 10 + b } f(1, 2, 3)",
        12.0,
    );
    assert_number(
        "function f(x, ...{length}) { return length } f(1, 2, 3)",
        2.0,
    );
    assert_string(
        "let f = (...[a, ...more]) => JSON.stringify([a, more]); f(1, 2, 3)",
        "[1,[2,3]]",
    );
    assert_number("function f(...[a = 5]) { return a } f()", 5.0);
    // The rest pattern does not count toward `length`.
    assert_number("function f(a, ...[b]) {} f.length", 1.0);
    // Captured bindings from a rest pattern.
    assert_number(
        "function f(...[a, b]) { return () => a + b } f(2, 3)()",
        5.0,
    );
}
