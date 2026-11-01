//! Integration tests: computed and numeric keys in object patterns (binding,
//! assignment, parameter, `for-of` and `catch` heads), numeric class member
//! keys, numeric object-literal keys, and object-literal accessor halves
//! split by a spread or a computed entry.

mod common;
use common::{assert_bool, assert_number, assert_rejected, assert_string};

// ---------------------------------------------------------------------------
// Object patterns
// ---------------------------------------------------------------------------

#[test]
fn binding_computed_keys_run_in_source_order() {
    // Each key is evaluated before its read; the default runs after the read.
    assert_string(
        "let log = []; \
         let src = { get a() { log.push('get a'); return 1 }, \
                     get b() { log.push('get b'); return undefined } }; \
         function k(n) { log.push('key ' + n); return n } \
         let { [k('a')]: x, [k('b')]: y = (log.push('default'), 9) } = src; \
         log.join() + '|' + x + y",
        "key a,get a,key b,get b,default|19",
    );
}

#[test]
fn assignment_computed_key_then_target_then_read_then_default() {
    // KeyedDestructuringAssignmentEvaluation: the PropertyName first, then the
    // target reference, then GetV, then the default, then the write.
    assert_string(
        "let log = []; \
         let src = { get a() { log.push('get'); return undefined } }; \
         let t = { set v(x) { log.push('set ' + x) } }; \
         function key() { log.push('key'); return 'a' } \
         function tgt() { log.push('target'); return t } \
         ({ [key()]: tgt().v = (log.push('default'), 5) } = src); \
         log.join()",
        "key,target,get,default,set 5",
    );
}

#[test]
fn symbol_key_stays_a_symbol() {
    assert_number(
        "let s = Symbol('s'); let { [s]: v } = { [s]: 42, s: 1 }; v",
        42.0,
    );
    assert_number(
        "let s = Symbol('s'); let v; ({ [s]: v } = { [s]: 7 }); v",
        7.0,
    );
}

#[test]
fn object_key_is_converted_exactly_once() {
    assert_string(
        "let n = 0; let key = { toString() { n++; return 'p' } }; \
         let { [key]: v, ...rest } = { p: 1, q: 2 }; \
         n + ':' + v + ':' + JSON.stringify(rest)",
        r#"1:1:{"q":2}"#,
    );
    assert_string(
        "let n = 0; let key = { toString() { n++; return 'p' } }; let v, rest; \
         ({ [key]: v, ...rest } = { p: 1, q: 2 }); \
         n + ':' + v + ':' + JSON.stringify(rest)",
        r#"1:1:{"q":2}"#,
    );
}

#[test]
fn numeric_keys_use_the_canonical_numeric_string() {
    assert_string(
        "let { 0: a, 1.0: b, 0x10: c, 1e21: d, 1.5: e, .5: f } = \
           { '0': 'a', '1': 'b', '16': 'c', '1e+21': 'd', '1.5': 'e', '0.5': 'f' }; \
         a + b + c + d + e + f",
        "abcdef",
    );
    assert_string(
        "let a, b; ({ 0x10: a, 1.0: b } = ['x', 'y']); typeof a + b",
        "undefinedy",
    );
    // Numeric keys index an array source.
    assert_string("let { 0: a, 2: c } = ['p', 'q', 'r']; a + c", "pr");
}

#[test]
fn rest_excludes_computed_and_symbol_keys() {
    assert_string(
        "let s = Symbol('s'); let k = 'b'; \
         let { [k]: b, [s]: sv, ...rest } = { a: 1, b: 2, [s]: 3, c: 4 }; \
         JSON.stringify(rest) + Object.getOwnPropertySymbols(rest).length + b + sv",
        r#"{"a":1,"c":4}023"#,
    );
    // A symbol not destructured is copied by the rest.
    assert_bool(
        "let s = Symbol('s'); let t = Symbol('t'); \
         let { [s]: x, ...rest } = { [s]: 1, [t]: 2 }; \
         rest[t] === 2 && !(s in rest)",
        true,
    );
    // Numeric keys are excluded by their canonical string.
    assert_string(
        "let { 1.0: a, ...rest } = { 1: 'one', 2: 'two' }; a + JSON.stringify(rest)",
        r#"one{"2":"two"}"#,
    );
    assert_string(
        "let k = 'b'; let b, rest; ({ [k]: b, ...rest } = { a: 1, b: 2 }); \
         b + JSON.stringify(rest)",
        r#"2{"a":1}"#,
    );
}

#[test]
fn nullish_source_throws_before_a_computed_key_runs() {
    assert_string(
        "let log = []; \
         try { let { [(log.push('key'), 'a')]: x } = null } \
         catch (e) { log.push(e instanceof TypeError) } \
         log.join()",
        "true",
    );
    assert_string(
        "let log = []; let x; \
         try { ({ [(log.push('key'), 'a')]: x } = undefined) } \
         catch (e) { log.push(e instanceof TypeError) } \
         log.join()",
        "true",
    );
}

#[test]
fn computed_keys_in_params_loop_heads_and_catch() {
    assert_number(
        "function f({ ['a' + 'b']: v, 2: w }) { return v + w } f({ ab: 1, 2: 2 })",
        3.0,
    );
    assert_number(
        "let k = 'x'; let out = 0; \
         for (const { [k]: v } of [{ x: 1 }, { x: 2 }]) out += v; out",
        3.0,
    );
    assert_number(
        "let k = 'x'; let v; for ({ [k]: v } of [{ x: 5 }]) ; v",
        5.0,
    );
    assert_string(
        "let k = 'x'; let out = ''; for (const { [k]: n, 0: c } in { ab: 1 }) out += c + n; out",
        "aundefined",
    );
    assert_number(
        "let k = 'x'; let r; try { throw { x: 7 } } catch ({ [k]: v }) { r = v } r",
        7.0,
    );
}

#[test]
fn a_property_with_a_default_is_read_once() {
    assert_number(
        "let n = 0; let { a = 1 } = { get a() { n++; return undefined } }; n * 10 + a",
        11.0,
    );
    assert_number(
        "let n = 0; let { b: c = 1 } = { get b() { n++; return 2 } }; n * 10 + c",
        12.0,
    );
    assert_number(
        "let n = 0; let k = 'b'; let { [k]: c = 1 } = { get b() { n++ } }; n * 10 + c",
        11.0,
    );
}

#[test]
fn computed_key_with_yield_in_a_generator() {
    assert_number(
        "function* g() { const { [yield 'k']: v } = { x: 9 }; return v } \
         let it = g(); it.next(); it.next('x').value",
        9.0,
    );
    assert_number(
        "function* g() { let v; ({ [yield 'k']: v } = { y: 4 }); return v } \
         let it = g(); it.next(); it.next('y').value",
        4.0,
    );
    // Nested patterns: the inner source survives the suspend too.
    assert_number(
        "function* g() { const { a: { [yield 'k']: v } } = { a: { x: 9 } }; return v } \
         let it = g(); it.next(); it.next('x').value",
        9.0,
    );
    assert_number(
        "function* g() { let v; ({ a: { [yield 'k']: v } } = { a: { y: 4 } }); return v } \
         let it = g(); it.next(); it.next('y').value",
        4.0,
    );
    assert_number(
        "function* g() { const [{ [yield 'k']: v }] = [{ x: 9 }]; return v } \
         let it = g(); it.next(); it.next('x').value",
        9.0,
    );
    // The key runs after the source; later keys read the same source.
    assert_string(
        "function* g() { let log = ''; \
           const { [(log += 'k', yield 'k')]: a, b } = (log += 's', { x: 1, b: 2 }); \
           return log + a + b } \
         let it = g(); it.next(); it.next('x').value",
        "sk12",
    );
}

#[test]
fn bigint_pattern_key_is_rejected() {
    assert_rejected("let { 1n: a } = {}; a");
}

// ---------------------------------------------------------------------------
// Class members and object literals
// ---------------------------------------------------------------------------

#[test]
fn class_numeric_member_keys() {
    assert_string(
        "class C { 1() { return 'one' } static 2() { return 'two' } \
                   get 3() { return 'three' } 0x10 = 'sixteen'; static 1.5 = 'st'; } \
         let c = new C(); c[1]() + c[3] + c[16] + C[2]() + C['1.5']",
        "onethreesixteentwost",
    );
    assert_string(
        "class C { 1() {} get 3() { return 0 } static 0x10() {} } \
         C.prototype[1].name + '|' + \
         Object.getOwnPropertyDescriptor(C.prototype, '3').get.name + '|' + C[16].name",
        "1|get 3|16",
    );
    // Methods are non-enumerable prototype properties; a field is an own
    // enumerable property of the instance.
    assert_string(
        "class C { 2() {} 1e21() {} 0.5 = 1 } \
         Object.getOwnPropertyNames(C.prototype).join() + '|' + Object.keys(new C()).join()",
        "2,constructor,1e+21|0.5",
    );
}

#[test]
fn class_numeric_getter_and_setter_share_one_property() {
    assert_number(
        "class D { get 1() { return this.v } set 1(x) { this.v = x * 2 } } \
         let d = new D(); d[1] = 5; d[1]",
        10.0,
    );
    assert_string(
        "class D { static get 0x1() { return 1 } static set 1(x) {} } \
         let p = Object.getOwnPropertyDescriptor(D, '1'); typeof p.get + typeof p.set",
        "functionfunction",
    );
}

#[test]
fn bigint_class_member_key_is_rejected() {
    assert_rejected("class C { 1n() {} } 0");
}

#[test]
fn object_literal_numeric_keys_are_canonical() {
    assert_string(
        "Object.keys({ 1.0: 'x', 0x10: 'y', 1e21: 'z', .5: 'w' }).join()",
        "1,16,1e+21,0.5",
    );
    assert_string("let o = { 1.0: 'x' }; o['1']", "x");
    assert_string(
        "let o = { get 1.0() { return 'g' }, set 1(v) {} }; \
         let p = Object.getOwnPropertyDescriptor(o, '1'); typeof p.get + typeof p.set + o[1]",
        "functionfunctiong",
    );
}

#[test]
fn accessor_halves_split_by_a_spread_or_computed_entry() {
    // The setter is a partial descriptor: it keeps the getter defined before.
    assert_string(
        "let o = { get a() { return 'g' }, ...{ b: 1 }, set a(v) { this.s = v } }; \
         let d = Object.getOwnPropertyDescriptor(o, 'a'); o.a = 3; \
         typeof d.get + typeof d.set + o.a + o.s",
        "functionfunctiong3",
    );
    assert_string(
        "let k = 'z'; let o = { get a() { return 'g' }, [k]: 1, set a(v) {} }; \
         let d = Object.getOwnPropertyDescriptor(o, 'a'); typeof d.get + typeof d.set + o.a",
        "functionfunctiong",
    );
    // A spread or computed entry that made `a` a data property in between is
    // replaced by the setter half alone.
    assert_string(
        "let o = { get a() { return 1 }, ...{ a: 2 }, set a(v) {} }; \
         let d = Object.getOwnPropertyDescriptor(o, 'a'); typeof d.get + typeof d.set",
        "undefinedfunction",
    );
    assert_string(
        "let o = { get a() { return 1 }, ['a']: 2, set a(v) {} }; \
         let d = Object.getOwnPropertyDescriptor(o, 'a'); typeof d.get + typeof d.set",
        "undefinedfunction",
    );
    // The getter half after a spread keeps an earlier setter.
    assert_string(
        "let o = { set a(v) {}, ...{}, get a() { return 1 } }; \
         let d = Object.getOwnPropertyDescriptor(o, 'a'); typeof d.get + typeof d.set + d.enumerable",
        "functionfunctiontrue",
    );
}
