//! Integration tests: property descriptors and integrity levels (DESIGN.md
//! §6.3) — `Object.defineProperty` / `defineProperties` /
//! `getOwnPropertyDescriptor(s)` / `preventExtensions` / `seal` / `freeze` /
//! `isExtensible` / `isSealed` / `isFrozen`. Expected texts are Node's output
//! (strict mode), except where jsc's own error wording is noted.

mod common;
use common::{assert_output, assert_uncaught};

/// Shared helpers: `show(o, k)` renders a descriptor, `t(f)` / `tn(f)` run
/// `f` and render its error (name and message / name only).
const PRELUDE: &str = "function show(o, k) { let d = Object.getOwnPropertyDescriptor(o, k); if (d === undefined) return 'none'; \
    return Object.keys(d).map(function (f) { let v = d[f]; return f + '=' + (typeof v === 'function' ? 'fn' : String(v)) }).join(',') }
function t(f) { try { f(); return 'ok' } catch (e) { return e.name + ': ' + e.message } }
function tn(f) { try { f(); return 'ok' } catch (e) { return e.name } }
";

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        assert_output(&format!("{PRELUDE}{body}"), expected);
    }
}

/// `ValidateAndApplyPropertyDescriptor` on a new property: absent fields
/// default to false / undefined.
#[test]
fn validate_new_properties() {
    check(&[
        (
            r"let o = {}; Object.defineProperty(o, 'x', {}); console.log(show(o, 'x')); Object.defineProperty(o, 'y', {value: 1}); console.log(show(o, 'y'), Object.keys(o).length); Object.defineProperty(o, 'z', {value: 2, writable: true, enumerable: true, configurable: true}); console.log(show(o, 'z'), Object.keys(o).join())",
            "value=undefined,writable=false,enumerable=false,configurable=false\nvalue=1,writable=false,enumerable=false,configurable=false 0\nvalue=2,writable=true,enumerable=true,configurable=true z\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {get: function () { return 7 }}); console.log(show(o, 'x'), o.x, t(function () { o.x = 1 })); Object.defineProperty(o, 'y', {set: undefined, enumerable: true}); console.log(show(o, 'y'), o.y)",
            "get=fn,set=undefined,enumerable=false,configurable=false 7 TypeError: Cannot set property x of object which has only a getter\nget=undefined,set=undefined,enumerable=true,configurable=false undefined\n",
        ),
    ]);
}

/// Updates of a configurable property: partial descriptors keep absent
/// fields; a kind change keeps `enumerable` / `configurable`.
#[test]
fn validate_configurable_updates() {
    check(&[
        (
            r"let o = {x: 1}; Object.defineProperty(o, 'x', {enumerable: false}); console.log(show(o, 'x'), Object.keys(o).length, o.x); Object.defineProperty(o, 'x', {writable: false}); console.log(show(o, 'x'), t(function () { o.x = 2 }).slice(0, 9), o.x); Object.defineProperty(o, 'x', {value: 5}); console.log(o.x)",
            "value=1,writable=true,enumerable=false,configurable=true 0 1\nvalue=1,writable=false,enumerable=false,configurable=true TypeError 1\n5\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1}); Object.defineProperty(o, 'x', {}); console.log(show(o, 'x'))",
            "value=1,writable=false,enumerable=false,configurable=false\n",
        ),
        (
            r"let o = {x: 1}; Object.defineProperty(o, 'x', {get: function () { return 9 }}); console.log(show(o, 'x'), o.x); Object.defineProperty(o, 'x', {value: 2}); console.log(show(o, 'x')); let p = {}; Object.defineProperty(p, 'y', {get: function () { return 1 }, configurable: true}); Object.defineProperty(p, 'y', {writable: true}); console.log(show(p, 'y'))",
            "get=fn,set=undefined,enumerable=true,configurable=true 9\nvalue=2,writable=false,enumerable=true,configurable=true\nvalue=undefined,writable=true,enumerable=false,configurable=true\n",
        ),
        (
            r"let o = {}; let log = []; Object.defineProperty(o, 'x', {get: function () { return 4 }, configurable: true}); Object.defineProperty(o, 'x', {set: function (v) { log.push(v) }}); o.x = 5; console.log(o.x, log.join(), show(o, 'x')); Object.defineProperty(o, 'x', {get: undefined}); console.log(o.x, show(o, 'x'))",
            "4 5 get=fn,set=fn,enumerable=false,configurable=true\nundefined get=undefined,set=fn,enumerable=false,configurable=true\n",
        ),
        (
            r"class A { get v() { return 1 } } let a = new A(); Object.defineProperty(a, 'v', {value: 2, enumerable: true}); console.log(a.v, show(a, 'v'), show(A.prototype, 'v'))",
            "2 value=2,writable=false,enumerable=true,configurable=false get=fn,set=undefined,enumerable=false,configurable=true\n",
        ),
    ]);
}

/// Every non-configurable rejection branch, and the `SameValue` no-ops.
#[test]
fn validate_non_configurable_rules() {
    check(&[
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1}); console.log(t(function () { Object.defineProperty(o, 'x', {configurable: true}) })); console.log(t(function () { Object.defineProperty(o, 'x', {configurable: false}) }))",
            "TypeError: Cannot redefine property: x\nok\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1, enumerable: true}); console.log(t(function () { Object.defineProperty(o, 'x', {enumerable: false}) }), t(function () { Object.defineProperty(o, 'x', {enumerable: true}) }))",
            "TypeError: Cannot redefine property: x ok\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'd', {value: 1, writable: true}); Object.defineProperty(o, 'a', {get: function () { return 1 }}); console.log(t(function () { Object.defineProperty(o, 'd', {get: function () { return 2 }}) })); console.log(t(function () { Object.defineProperty(o, 'a', {value: 2}) })); console.log(t(function () { Object.defineProperty(o, 'a', {writable: false}) }))",
            "TypeError: Cannot redefine property: d\nTypeError: Cannot redefine property: a\nTypeError: Cannot redefine property: a\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1}); Object.defineProperty(o, 'n', {value: NaN}); Object.defineProperty(o, 'z', {value: 0}); Object.defineProperty(o, 's', {value: 'ab'}); console.log(t(function () { Object.defineProperty(o, 'x', {value: 2}) })); console.log(t(function () { Object.defineProperty(o, 'x', {value: 1}) }), t(function () { Object.defineProperty(o, 'n', {value: NaN}) }), t(function () { Object.defineProperty(o, 's', {value: 'a' + 'b'}) })); console.log(t(function () { Object.defineProperty(o, 'z', {value: -0}) })); console.log(t(function () { Object.defineProperty(o, 'x', {writable: true}) }), t(function () { Object.defineProperty(o, 'x', {writable: false}) }))",
            "TypeError: Cannot redefine property: x\nok ok ok\nTypeError: Cannot redefine property: z\nTypeError: Cannot redefine property: x ok\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1, writable: true}); Object.defineProperty(o, 'x', {value: 2}); console.log(o.x); o.x = 3; console.log(o.x); Object.defineProperty(o, 'x', {writable: false}); console.log(show(o, 'x')); console.log(t(function () { Object.defineProperty(o, 'x', {writable: true}) }))",
            "2\n3\nvalue=3,writable=false,enumerable=false,configurable=false\nTypeError: Cannot redefine property: x\n",
        ),
        (
            r"let g = function () { return 1 }; let s = function (v) {}; let o = {}; Object.defineProperty(o, 'x', {get: g, set: s}); console.log(t(function () { Object.defineProperty(o, 'x', {get: g, set: s}) })); console.log(t(function () { Object.defineProperty(o, 'x', {get: function () { return 2 }}) })); console.log(t(function () { Object.defineProperty(o, 'x', {set: undefined}) }))",
            "ok\nTypeError: Cannot redefine property: x\nTypeError: Cannot redefine property: x\n",
        ),
    ]);
}

/// `ToPropertyDescriptor`: object check, spec field order through getters,
/// inherited fields, callable `get` / `set`, no mixing.
#[test]
fn to_property_descriptor() {
    check(&[
        (
            r"let o = {}; console.log(t(function () { Object.defineProperty(o, 'x', 1) })); console.log(t(function () { Object.defineProperty(o, 'x', 'abc') })); console.log(t(function () { Object.defineProperty(o, 'x', undefined) })); console.log(t(function () { Object.defineProperties(o, {x: true}) }))",
            "TypeError: Property description must be an object: 1\nTypeError: Property description must be an object: abc\nTypeError: Property description must be an object: undefined\nTypeError: Property description must be an object: true\n",
        ),
        (
            r"let log = []; let d = {get set() { log.push('set'); return undefined }, get get() { log.push('get'); return undefined }, get writable() { log.push('writable'); return true }, get value() { log.push('value'); return 1 }, get configurable() { log.push('configurable'); return true }, get enumerable() { log.push('enumerable'); return true }}; let o = {}; console.log(tn(function () { Object.defineProperty(o, 'x', d) }), log.join())",
            "TypeError enumerable,configurable,value,writable,get,set\n",
        ),
        (
            r"class P { get enumerable() { return true } get value() { return 3 } } let d = new P(); d.writable = true; let o = {}; Object.defineProperty(o, 'x', d); console.log(show(o, 'x'))",
            "value=3,writable=true,enumerable=true,configurable=false\n",
        ),
        (
            r"let o = {}; console.log(t(function () { Object.defineProperty(o, 'x', {get: 1}) })); console.log(t(function () { Object.defineProperty(o, 'x', {set: 'f'}) })); console.log(t(function () { Object.defineProperty(o, 'x', {get: {}}) }).slice(0, 26)); console.log(tn(function () { Object.defineProperty(o, 'x', {get: function () {}, value: 1}) }), tn(function () { Object.defineProperty(o, 'x', {set: function () {}, writable: false}) }), 'x' in o)",
            "TypeError: Getter must be a function: 1\nTypeError: Setter must be a function: f\nTypeError: Getter must be \nTypeError TypeError false\n",
        ),
    ]);
}

/// `FromPropertyDescriptor` key order and `getOwnPropertyDescriptor(s)`
/// over ordinary objects, primitives, builtins, functions, and arrays.
#[test]
fn from_property_descriptor() {
    check(&[
        (
            r"let o = {a: 1}; Object.defineProperty(o, 'g', {get: function () { return 1 }, enumerable: true}); console.log(Object.keys(Object.getOwnPropertyDescriptor(o, 'a')).join(), Object.keys(Object.getOwnPropertyDescriptor(o, 'g')).join()); let d = Object.getOwnPropertyDescriptor(o, 'a'); console.log(d.value, d.writable, d.enumerable, d.configurable, JSON.stringify(d)); console.log(Object.getOwnPropertyDescriptor(o, 'g') === Object.getOwnPropertyDescriptor(o, 'g'))",
            "value,writable,enumerable,configurable get,set,enumerable,configurable\n1 true true true {\"value\":1,\"writable\":true,\"enumerable\":true,\"configurable\":true}\nfalse\n",
        ),
        (
            r"let o = {a: 1}; console.log(Object.getOwnPropertyDescriptor(o, 'b'), Object.getOwnPropertyDescriptor(o, 'toString')); console.log(show('abc', 'length'), show('abc', 1), show('abc', 5), show(5, 'x')); console.log(t(function () { Object.getOwnPropertyDescriptor(null, 'x') })); console.log(show({1: 'x'}, 1), show({}, undefined), show({undefined: 2}, undefined)); let k = {toString: function () { return 'a' }}; console.log(show(o, k))",
            "undefined undefined\nvalue=3,writable=false,enumerable=false,configurable=false value=b,writable=false,enumerable=true,configurable=false none none\nTypeError: Cannot convert undefined or null to object\nvalue=x,writable=true,enumerable=true,configurable=true none value=2,writable=true,enumerable=true,configurable=true\nvalue=1,writable=true,enumerable=true,configurable=true\n",
        ),
        (
            r"function f(a, b) {} console.log(show(f, 'length')); console.log(show(f, 'name')); console.log(show(f, 'prototype')); console.log(show(Math, 'PI')); console.log(show(Object, 'keys')); console.log(show(Object.prototype, 'toString')); console.log(show(globalThis, 'NaN'))",
            "value=2,writable=false,enumerable=false,configurable=true\nvalue=f,writable=false,enumerable=false,configurable=true\nvalue=[object Object],writable=true,enumerable=false,configurable=false\nvalue=3.141592653589793,writable=false,enumerable=false,configurable=false\nvalue=fn,writable=true,enumerable=false,configurable=true\nvalue=fn,writable=true,enumerable=false,configurable=true\nvalue=NaN,writable=false,enumerable=false,configurable=false\n",
        ),
        (
            r"let a = [1, , 3]; a.foo = 'f'; console.log(show(a, 0), show(a, 1), show(a, '2'), show(a, 3)); console.log(show(a, 'length')); console.log(show(a, 'foo'))",
            "value=1,writable=true,enumerable=true,configurable=true none value=3,writable=true,enumerable=true,configurable=true none\nvalue=3,writable=true,enumerable=false,configurable=false\nvalue=f,writable=true,enumerable=true,configurable=true\n",
        ),
        (
            r"let o = {b: 1, a: 2, 1: 3}; Object.defineProperty(o, 'h', {value: 4}); let ds = Object.getOwnPropertyDescriptors(o); console.log(Object.keys(ds).join(), show(ds, 'h'), ds.h.value, ds.b.writable); console.log(Object.keys(Object.getOwnPropertyDescriptors('ab')).join()); console.log(Object.keys(Object.getOwnPropertyDescriptors(function g(x) {})).join())",
            "1,b,a,h value=[object Object],writable=true,enumerable=true,configurable=true 4 true\n0,1,length\nlength,name,prototype\n",
        ),
    ]);
}

/// `defineProperty` / `defineProperties` arguments, return value, and the
/// read-all-then-apply order.
#[test]
fn define_properties_two_phase() {
    check(&[
        (
            r"console.log(t(function () { Object.defineProperty(1, 'x', {}) })); console.log(t(function () { Object.defineProperty(undefined, 'x', {}) })); console.log(Object.defineProperty.length, Object.defineProperty.name, Object.defineProperties.length, Object.getOwnPropertyDescriptor.length, Object.getOwnPropertyDescriptors.length, Object.freeze.length, Object.isFrozen.name); let o = {}; Object.defineProperty(o, 7, {value: 'n', enumerable: true}); console.log(Object.keys(o).join(), o[7])",
            "TypeError: Object.defineProperty called on non-object\nTypeError: Object.defineProperty called on non-object\n3 defineProperty 2 2 1 1 isFrozen\n7 n\n",
        ),
        (
            r"let o = {}; let r = Object.defineProperties(o, {a: {value: 1, enumerable: true}, b: {get: function () { return 2 }}}); console.log(r === o, show(o, 'a'), show(o, 'b'), o.b)",
            "true value=1,writable=false,enumerable=true,configurable=false get=fn,set=undefined,enumerable=false,configurable=false 2\n",
        ),
        (
            r"let o = {}; let props = {a: {value: 1}}; Object.defineProperty(props, 'b', {get: function () { throw new RangeError('boom') }, enumerable: true}); console.log(t(function () { Object.defineProperties(o, props) }), 'a' in o); let q = {}; Object.defineProperty(q, 'b', {value: 0}); console.log(tn(function () { Object.defineProperties(q, {a: {value: 1}, b: {value: 2}, c: {value: 3}}) }), 'a' in q, 'c' in q); let s = {}; console.log(tn(function () { Object.defineProperties(s, {a: {value: 1}, b: 5}) }), 'a' in s)",
            "RangeError: boom false\nTypeError true false\nTypeError false\n",
        ),
        (
            r"let props = {a: {value: 1}}; Object.defineProperty(props, 'hidden', {value: {value: 2}, enumerable: false}); let o = Object.defineProperties({}, props); console.log('a' in o, 'hidden' in o); console.log(t(function () { Object.defineProperties({}, null) })); console.log(Object.keys(Object.defineProperties({}, 5)).length, t(function () { Object.defineProperties({}, 'ab') }))",
            "true false\nTypeError: Cannot convert undefined or null to object\n0 TypeError: Property description must be an object: a\n",
        ),
    ]);
}

/// `preventExtensions` / `seal` / `freeze` and the `is*` tests on objects
/// and primitives, with the strict-mode throws on write / delete / add.
#[test]
fn integrity_levels() {
    check(&[
        (
            r"let o = {a: 1}; console.log(Object.isExtensible(o), Object.preventExtensions(o) === o, Object.isExtensible(o)); console.log(t(function () { Object.defineProperty(o, 'y', {value: 1}) })); console.log(t(function () { o.b = 2 }).slice(0, 9), 'b' in o); o.a = 5; Object.defineProperty(o, 'a', {enumerable: false}); console.log(o.a, show(o, 'a')); delete o.a; console.log('a' in o, t(function () { o.a = 1 }).slice(0, 9))",
            "true true false\nTypeError: Cannot define property y, object is not extensible\nTypeError false\n5 value=5,writable=true,enumerable=false,configurable=true\nfalse TypeError\n",
        ),
        (
            r"let o = {a: 1}; console.log(Object.isSealed(o), Object.seal(o) === o, Object.isSealed(o), Object.isFrozen(o), Object.isExtensible(o)); o.a = 2; console.log(o.a, show(o, 'a')); console.log(tn(function () { delete o.a }), tn(function () { o.b = 1 }), tn(function () { Object.defineProperty(o, 'a', {get: function () { return 1 }}) }))",
            "false true true false false\n2 value=2,writable=true,enumerable=true,configurable=false\nTypeError TypeError TypeError\n",
        ),
        (
            r"let o = {a: 1, get g() { return 2 }}; console.log(Object.isFrozen(o), Object.freeze(o) === o, Object.isFrozen(o), Object.isSealed(o)); console.log(show(o, 'a'), show(o, 'g')); console.log(tn(function () { o.a = 2 }), tn(function () { delete o.a }), tn(function () { o.c = 1 }), o.a, o.g); Object.defineProperty(o, 'a', {value: 1})",
            "false true true true\nvalue=1,writable=false,enumerable=true,configurable=false get=fn,set=undefined,enumerable=true,configurable=false\nTypeError TypeError TypeError 1 2\n",
        ),
        (
            r"let e = {}; Object.preventExtensions(e); console.log(Object.isSealed(e), Object.isFrozen(e)); let w = {}; Object.defineProperty(w, 'x', {value: 1, writable: true}); Object.preventExtensions(w); console.log(Object.isSealed(w), Object.isFrozen(w)); let a = {}; Object.defineProperty(a, 'x', {get: function () { return 1 }}); Object.preventExtensions(a); console.log(Object.isSealed(a), Object.isFrozen(a)); let c = {x: 1}; Object.preventExtensions(c); console.log(Object.isSealed(c), Object.isFrozen(c))",
            "true true\ntrue false\ntrue true\nfalse false\n",
        ),
        (
            r"console.log(Object.preventExtensions(1), Object.seal('s'), Object.freeze(true), Object.freeze(undefined), Object.seal(null)); console.log(Object.isExtensible(1), Object.isExtensible('s'), Object.isSealed(2), Object.isFrozen('x'), Object.isFrozen(undefined), Object.isSealed(null))",
            "1 s true undefined null\nfalse false true true true true\n",
        ),
    ]);
}

/// Functions (lazy properties), classes, Map/Set/Date/Promise/generator
/// handles, `arguments`, and class instances.
#[test]
fn object_kinds() {
    check(&[
        (
            r"function f(a) {} Object.freeze(f); console.log(Object.isFrozen(f), show(f, 'length'), show(f, 'prototype')); console.log(tn(function () { f.x = 1 })); class C { m() {} } Object.seal(C); console.log(Object.isSealed(C), Object.isFrozen(C), show(C, 'prototype')); let g = function () {}; Object.preventExtensions(g); console.log(Object.isExtensible(g), show(g, 'name'), tn(function () { g.y = 1 }))",
            "true value=1,writable=false,enumerable=false,configurable=false value=[object Object],writable=false,enumerable=false,configurable=false\nTypeError\ntrue true value=[object Object],writable=false,enumerable=false,configurable=false\nfalse value=g,writable=false,enumerable=false,configurable=true TypeError\n",
        ),
        (
            r"let m = new Map(); Object.freeze(m); m.set(1, 2); console.log(m.size, Object.isFrozen(m), tn(function () { m.x = 1 })); let s = new Set([1]); Object.seal(s); s.add(2); console.log(s.size, Object.isSealed(s)); let d = new Date(0); Object.preventExtensions(d); console.log(d.getTime(), Object.isExtensible(d)); let p = Promise.resolve(1); Object.freeze(p); console.log(Object.isFrozen(p)); function* gen() { yield 1 } let it = gen(); Object.freeze(it); console.log(it.next().value, Object.isFrozen(it))",
            "1 true TypeError\n2 true\n0 false\ntrue\n1 true\n",
        ),
        (
            r"function f() { let args = arguments; Object.freeze(args); return [Object.isFrozen(args), show(args, 0), show(args, 'length'), tn(function () { args[0] = 1 })] } console.log(f(5).join(' '))",
            "true value=5,writable=false,enumerable=true,configurable=false value=1,writable=false,enumerable=false,configurable=false TypeError\n",
        ),
        (
            r"class A { constructor() { this.x = 1 } } let a = new A(); Object.defineProperty(a, 'y', {value: 2, enumerable: true}); console.log(JSON.stringify(a), show(a, 'y')); Object.freeze(a); console.log(tn(function () { a.x = 2 }), Object.isFrozen(a))",
            "{\"x\":1,\"y\":2} value=2,writable=false,enumerable=true,configurable=false\nTypeError true\n",
        ),
    ]);
}

/// The existing writers honour the attributes: inherited read-only, delete
/// of a non-configurable, non-enumerable keys, fields on a non-extensible
/// object, builtin redefinition, and the `JSON.parse` reviver.
#[test]
fn writers_honour_attributes() {
    check(&[
        (
            r"class A {} Object.defineProperty(A.prototype, 'x', {value: 1}); let a = new A(); console.log(a.x, tn(function () { a.x = 2 }), Object.prototype.hasOwnProperty.call(a, 'x')); Object.defineProperty(a, 'x', {value: 3, writable: true, enumerable: true, configurable: true}); console.log(a.x, A.prototype.x)",
            "1 TypeError false\n3 1\n",
        ),
        (
            r"let o = {}; Object.defineProperty(o, 'x', {value: 1}); console.log(t(function () { delete o.x }), o.x); Object.defineProperty(o, 'y', {value: 1, configurable: true}); delete o.y; console.log('y' in o)",
            "TypeError: Cannot delete property 'x' of object 1\nfalse\n",
        ),
        (
            r"let o = {a: 1}; Object.defineProperty(o, 'h', {value: 2}); let ks = []; for (let k in o) ks.push(k); console.log(Object.keys(o).join(), ks.join(), JSON.stringify(o), Object.values(o).join()); let {...r} = o; console.log(Object.keys(r).join())",
            "a a {\"a\":1} 1\na\n",
        ),
        (
            r"class A { constructor() { Object.preventExtensions(this) } } class B extends A { x = 1 } console.log(tn(function () { new B() }))",
            "TypeError\n",
        ),
        (
            r"Object.defineProperty(Array.prototype, 'push', {value: function () { return 'mine' }}); console.log([].push(1)); let M = Math; Object.freeze(M); console.log(M.floor(1.5), Object.isFrozen(Math), tn(function () { M.floor = null })); Object.defineProperty(String.prototype, 'shout', {value: function () { return 'hi' }}); console.log('x'.shout())",
            "mine\n1 true TypeError\nhi\n",
        ),
        (
            r#"let obj = JSON.parse('{"a": 1, "b": 2}', function (key, value) { if (key === 'a') { Object.defineProperty(this, 'b', {configurable: false}) } if (key === 'b') return; return value }); console.log(obj.a, obj.hasOwnProperty('b'), obj.b); let o2 = JSON.parse('{"a": 1, "b": 2}', function (key, value) { if (key === 'a') { Object.defineProperty(this, 'b', {configurable: false}) } if (key === 'b') return 22; return value }); console.log(o2.a, o2.b)"#,
            "1 true 2\n1 2\n",
        ),
    ]);
}
/// Arrays: an index with default attributes on a dense array is an element
/// store, a named key an ordinary property.
#[test]
fn array_default_indices() {
    check(&[(
        "let a = [1, 2]; Object.defineProperty(a, '0', {value: 5}); \
         Object.defineProperty(a, 3, {value: 7, writable: true, enumerable: true, configurable: true}); \
         Object.defineProperty(a, 'foo', {value: 1}); \
         console.log(a[0], a.length, a[3], 2 in a, show(a, 'foo'), Object.keys(a).join())",
        "5 4 7 false value=1,writable=false,enumerable=false,configurable=false 0,1,3\n",
    )]);
}

/// `defineProperty(a, "length", desc)` is `ArraySetLength`: value
/// validation, generic descriptors, the `configurable` / `enumerable`
/// rejections, shrinking, and growth.
#[test]
fn array_set_length_descriptor() {
    check(&[
        (
            "let a = [1, 2, 3]; Object.defineProperty(a, 'length', {value: 1}); \
             console.log(a.length, a[1], 1 in a, show(a, 'length')); \
             Object.defineProperty(a, 'length', {value: {valueOf: function () { return 4 }}}); \
             console.log(a.length, 3 in a, a[0])",
            "1 undefined false value=1,writable=true,enumerable=false,configurable=false\n4 false 1\n",
        ),
        (
            "let a = [1, 2]; \
             console.log(tn(function () { Object.defineProperty(a, 'length', {value: -1}) }), \
             tn(function () { Object.defineProperty(a, 'length', {value: 1.5}) }), \
             tn(function () { Object.defineProperty(a, 'length', {configurable: true}) }), \
             tn(function () { Object.defineProperty(a, 'length', {enumerable: true}) }), \
             tn(function () { Object.defineProperty(a, 'length', {get: function () { return 1 }}) }), a.length); \
             Object.defineProperty(a, 'length', {}); Object.defineProperty(a, 'length', {writable: true, configurable: false, enumerable: false}); \
             console.log(a.length, show(a, 'length'))",
            "RangeError RangeError TypeError TypeError TypeError 2\n2 value=2,writable=true,enumerable=false,configurable=false\n",
        ),
        (
            "let a = [1, 2, 3, 4]; Object.defineProperty(a, 1, {value: 'x', configurable: false}); \
             console.log(show(a, 1)); console.log(tn(function () { a.length = 0 }), a.length, a[0], a[1], 2 in a); \
             let b = [1, 2, 3]; Object.defineProperty(b, 0, {configurable: false}); \
             console.log(tn(function () { Object.defineProperty(b, 'length', {value: 0, writable: false}) }), b.length, b[0], 1 in b, show(b, 'length'))",
            "value=x,writable=true,enumerable=true,configurable=false\nTypeError 2 1 x false\nTypeError 1 1 false value=1,writable=false,enumerable=false,configurable=false\n",
        ),
    ]);
}

/// A non-writable `length` makes every growth and every `length` write
/// throw; in-range element writes and named keys still work.
#[test]
fn array_read_only_length() {
    check(&[
        (
            "let a = [1, 2, 3]; Object.defineProperty(a, 'length', {value: 2, writable: false}); \
             console.log(a.length, show(a, 'length'), 2 in a, Object.isExtensible(a)); \
             console.log(tn(function () { a.length = 5 }), tn(function () { a.length = 2 }), tn(function () { a.push(1) }), \
             tn(function () { a[2] = 1 }), tn(function () { a[7] = 1 }), a.length); \
             a[0] = 9; a.foo = 1; console.log(a[0], Object.keys(a).join()); \
             console.log(tn(function () { Object.defineProperty(a, 'length', {value: 2}) }), \
             tn(function () { Object.defineProperty(a, 'length', {value: 3}) }), \
             tn(function () { Object.defineProperty(a, 'length', {writable: true}) }), \
             tn(function () { Object.defineProperty(a, 5, {value: 1}) }), tn(function () { a.pop() }), a.length)",
            "2 value=2,writable=false,enumerable=false,configurable=false false true\nTypeError TypeError TypeError TypeError TypeError 2\n9 0,1,foo\nok TypeError TypeError TypeError TypeError 2\n",
        ),
        (
            "let e = []; Object.defineProperty(e, 'length', {writable: false}); \
             console.log(tn(function () { e.push(1) }), tn(function () { e[0] = 1 }), tn(function () { e.length = 0 }), e.length, Object.isFrozen(e))",
            "TypeError TypeError TypeError 0 false\n",
        ),
    ]);
}

/// Array indices with arbitrary attributes: read-only, non-enumerable,
/// non-configurable, and accessor elements seen by reads, writes, delete,
/// `in`, `hasOwnProperty`, key order, `for-in`, JSON, and the builtins.
#[test]
fn array_index_descriptors() {
    check(&[
        (
            "let log = []; let a = [1, 2, 3]; Object.defineProperty(a, 0, {writable: false}); \
             Object.defineProperty(a, 1, {enumerable: false}); \
             Object.defineProperty(a, 2, {get: function () { return 'g' }, set: function (v) { log.push(v) }, configurable: true}); \
             console.log(show(a, 0)); console.log(show(a, 1)); console.log(show(a, 2)); \
             console.log(a[0], a[1], a[2], a.length, Object.keys(a).join(), JSON.stringify(a), 1 in a, a.hasOwnProperty(1)); \
             console.log(tn(function () { a[0] = 5 }), a[0]); a[2] = 'w'; console.log(log.join()); \
             let ks = []; for (let k in a) ks.push(k); \
             console.log(ks.join(), a.join('-'), a.indexOf(2), a.includes('g'), a.map(function (x) { return x + '!' }).join())",
            "value=1,writable=false,enumerable=true,configurable=true\nvalue=2,writable=true,enumerable=false,configurable=true\nget=fn,set=fn,enumerable=true,configurable=true\n1 2 g 3 0,2 [1,2,\"g\"] true true\nTypeError 1\nw\n0,2 1-2-g 1 true 1!,2!,g!\n",
        ),
        (
            "let b = [1, 2]; Object.defineProperty(b, 0, {configurable: false}); \
             console.log(tn(function () { delete b[0] }), tn(function () { delete b[1] }), b[0], 1 in b, b.length); console.log(b); \
             let c = []; Object.defineProperty(c, 3, {value: 'v', enumerable: true}); \
             console.log(c.length, c[3], show(c, 3), Object.keys(c).join(), tn(function () { c[3] = 1 }))",
            "TypeError ok 1 false 2\n[ 1, <1 empty item> ]\n4 v value=v,writable=false,enumerable=true,configurable=false 3 TypeError\n",
        ),
    ]);
}

/// `preventExtensions` / `seal` / `freeze` on arrays (empty or not), with
/// `isExtensible` / `isSealed` / `isFrozen` and every mutation path.
#[test]
fn array_integrity_levels() {
    check(&[
        (
            "let a = [1, 2]; console.log(Object.freeze(a) === a, Object.isFrozen(a), Object.isSealed(a), Object.isExtensible(a), show(a, 0), show(a, 'length')); \
             console.log(tn(function () { a[0] = 5 }), tn(function () { a.push(3) }), tn(function () { a.pop() }), tn(function () { a.length = 0 }), \
             tn(function () { delete a[0] }), tn(function () { a.x = 1 }), a.join(), a.length)",
            "true true true false value=1,writable=false,enumerable=true,configurable=false value=2,writable=false,enumerable=false,configurable=false\nTypeError TypeError TypeError TypeError TypeError TypeError 1,2 2\n",
        ),
        (
            "let s = [1]; Object.seal(s); s[0] = 7; \
             console.log(s[0], Object.isSealed(s), Object.isFrozen(s), show(s, 0), tn(function () { s[1] = 2 }), tn(function () { s.push(2) }), tn(function () { delete s[0] }), s.length); \
             s.length = 1; console.log(tn(function () { s.length = 0 }), s.length)",
            "7 true false value=7,writable=true,enumerable=true,configurable=false TypeError TypeError TypeError 1\nTypeError 1\n",
        ),
        (
            "let p = [1, 2]; Object.preventExtensions(p); p[0] = 3; \
             console.log(Object.isExtensible(p), Object.isSealed(p), p[0], tn(function () { p[2] = 1 }), tn(function () { p.push(1) }), tn(function () { p.y = 1 })); \
             p.length = 1; console.log(p.length, 1 in p); delete p[0]; console.log(p.length, 0 in p, tn(function () { p[0] = 1 }))",
            "false false 3 TypeError TypeError TypeError\n1 false\n1 false TypeError\n",
        ),
        (
            "let e = []; Object.freeze(e); console.log(Object.isFrozen(e), tn(function () { e.push(1) }), tn(function () { e[0] = 1 }), e.length); \
             let f = Object.preventExtensions([]); console.log(Object.isFrozen(f), Object.isSealed(f), tn(function () { f.length = 3 }), f.length); \
             console.log(Object.isExtensible([]), Object.isFrozen([]), Object.isSealed([1]))",
            "true TypeError TypeError 0\nfalse true ok 3\ntrue false false\n",
        ),
    ]);
}

/// The attribute table of functions, classes, builtin constructors, and
/// builtin methods, verified through descriptors, assignment, and delete.
#[test]
fn function_attributes() {
    check(&[
        (
            "function attrs(o, k) { let d = Object.getOwnPropertyDescriptor(o, k); return [d.writable, d.enumerable, d.configurable].join() } \
             function f(a, b) {} class C { m() {} } \
             console.log(show(f, 'length')); console.log(show(f, 'name')); console.log(show(f, 'prototype')); console.log(show(C, 'prototype')); \
             console.log(show(C, 'length'), show(C, 'name')); \
             console.log(attrs(Array, 'prototype'), attrs(Map, 'prototype'), attrs(Array.prototype, 'push'), attrs(Math, 'max'), attrs(Array, 'isArray'))",
            "value=2,writable=false,enumerable=false,configurable=true\nvalue=f,writable=false,enumerable=false,configurable=true\nvalue=[object Object],writable=true,enumerable=false,configurable=false\nvalue=[object Object],writable=false,enumerable=false,configurable=false\nvalue=0,writable=false,enumerable=false,configurable=true value=C,writable=false,enumerable=false,configurable=true\nfalse,false,false false,false,false true,false,true true,false,true true,false,true\n",
        ),
        (
            "function f(a) {} class C {} \
             console.log(tn(function () { f.length = 3 }), tn(function () { f.name = 'g' }), tn(function () { C.prototype = {} }), \
             tn(function () { Array.prototype = {} }), tn(function () { delete f.prototype }), tn(function () { delete C.prototype }), tn(function () { delete Array.prototype })); \
             f.prototype = 5; delete f.length; console.log(f.prototype, f.hasOwnProperty('length'), f.length); \
             Object.defineProperty(f, 'name', {value: 'renamed'}); console.log(f.name); \
             let o = {m() {}}; console.log((() => 1).hasOwnProperty('prototype'), o.m.hasOwnProperty('prototype'), C.m === undefined)",
            "TypeError TypeError TypeError TypeError TypeError TypeError TypeError\n5 false 0\nrenamed\nfalse false true\n",
        ),
    ]);
}

/// String wrappers and primitives (read-only enumerable indices, a read-only
/// non-enumerable `length`) and `arguments` (default indices, a writable
/// non-enumerable `length`).
#[test]
fn wrapper_and_arguments_descriptors() {
    check(&[
        (
            "let s = new String('ab'); console.log(show(s, 0)); console.log(show(s, 'length')); console.log(show('ab', 1), show(s, 2)); \
             console.log(tn(function () { s[0] = 'x' }), tn(function () { s.length = 1 }), tn(function () { delete s[0] }), \
             tn(function () { Object.defineProperty(s, 0, {value: 'z'}) }), tn(function () { Object.defineProperty(s, 0, {value: 'a'}) }), s[0]); \
             s.foo = 1; Object.defineProperty(s, 5, {value: 'q', enumerable: true}); console.log(Object.keys(s).join(), s[5])",
            "value=a,writable=false,enumerable=true,configurable=false\nvalue=2,writable=false,enumerable=false,configurable=false\nvalue=b,writable=false,enumerable=true,configurable=false none\nTypeError TypeError TypeError TypeError ok a\n0,1,5,foo q\n",
        ),
        (
            "function g() { return [show(arguments, 0), show(arguments, 'length'), Object.keys(arguments).join()] } console.log(g(7, 8).join(' | '))",
            "value=7,writable=true,enumerable=true,configurable=true | value=2,writable=true,enumerable=false,configurable=true | 0,1\n",
        ),
    ]);
}

/// The global object's own properties are reflectable, and a property
/// defined on it is honoured by later global reads (naming `globalThis` /
/// top-level `this` disables the `sealed(N)` fold).
#[test]
fn global_object_descriptors() {
    check(&[
        (
            "function attrs(o, k) { let d = Object.getOwnPropertyDescriptor(o, k); return [d.writable, d.enumerable, d.configurable].join() } \
             console.log(attrs(globalThis, 'NaN'), attrs(globalThis, 'Infinity'), attrs(globalThis, 'undefined')); \
             console.log(attrs(globalThis, 'Math'), attrs(globalThis, 'Array'), attrs(globalThis, 'globalThis'), attrs(globalThis, 'isNaN')); \
             Object.defineProperty(globalThis, 'answer', {value: 42}); console.log(answer, typeof answer, attrs(globalThis, 'answer')); \
             console.log(tn(function () { Object.defineProperty(globalThis, 'NaN', {value: 1}) }), tn(function () { Object.defineProperty(globalThis, 'NaN', {value: NaN}) })); \
             Object.defineProperty(globalThis, 'Math', {value: 5}); console.log(Math)",
            "false,false,false false,false,false false,false,false\ntrue,false,true true,false,true true,false,true true,false,true\n42 number false,false,false\nTypeError ok\n5\n",
        ),
        (
            "Object.defineProperty(this, 'viaThis', {value: 'v', writable: true}); console.log(viaThis); viaThis = 'w'; console.log(viaThis); \
             Object.defineProperty(this, 'JSON', {value: 'j'}); console.log(JSON)",
            "v\nw\nj\n",
        ),
    ]);
}

/// Redefining or freezing a watched intrinsic sets its dirty bit, so the
/// guarded fast paths fall back to the generic path and honour it.
#[test]
fn intrinsic_redefinition_beats_fast_paths() {
    check(&[
        (
            "Object.defineProperty(Array.prototype, 'push', {value: function (x) { return 'mine:' + x }}); let a = [1]; console.log(a.push(2), a.length)",
            "mine:2 1\n",
        ),
        (
            "Object.defineProperty(Array.prototype, 'pop', {value: function () { return 'p' }}); console.log([1, 2].pop())",
            "p\n",
        ),
        (
            "Object.defineProperty(Math, 'max', {value: function () { return 'm' }}); console.log(Math.max(1, 2))",
            "m\n",
        ),
        (
            "Object.defineProperty(JSON, 'stringify', {value: function () { return 's' }}); console.log(JSON.stringify(1))",
            "s\n",
        ),
        (
            "Object.freeze(Array.prototype); let b = [1]; b.push(2); console.log(b.length, b.join(), Object.isFrozen(Array.prototype), tn(function () { Array.prototype.foo = 1 }))",
            "2 1,2 true TypeError\n",
        ),
    ]);
}

/// `JSON.parse` revivers on arrays: `CreateDataProperty` and `[[Delete]]` of
/// a non-configurable element fail silently.
#[test]
fn json_reviver_array_non_configurable() {
    check(&[(
        r#"let a = JSON.parse('[1, 2]', function (key, value) { if (key === '0') { Object.defineProperty(this, '1', {configurable: false}) } if (key === '1') return 22; return value }); console.log(a[0], a[1]); let b = JSON.parse('[1, 2]', function (key, value) { if (key === '0') { Object.defineProperty(this, '1', {configurable: false}) } if (key === '1') return; return value }); console.log(b.hasOwnProperty('1'), b[1])"#,
        "1 2\ntrue 2\n",
    )]);
}

/// Strict-mode writes to frozen / sealed / non-extensible objects throw
/// uncaught with jsc's messages.
#[test]
fn strict_mode_throws_are_uncaught_type_errors() {
    assert_uncaught(
        "let o = Object.freeze({a: 1}); o.a = 2",
        "TypeError: Cannot assign to read only property 'a' of object",
    );
    assert_uncaught(
        "let o = Object.seal({a: 1}); delete o.a",
        "TypeError: Cannot delete property 'a' of object",
    );
    assert_uncaught(
        "let o = Object.preventExtensions({}); o.b = 1",
        "TypeError: Cannot add property b, object is not extensible",
    );
    assert_uncaught(
        "class A { constructor() { Object.freeze(this) } } class B extends A { x = 1 } new B()",
        "TypeError: Cannot define property x, object is not extensible",
    );
    assert_uncaught(
        "Object.defineProperty({}, 'x', {get: 1})",
        "TypeError: Getter must be a function: 1",
    );
    assert_uncaught(
        "let a = Object.freeze([1]); a.push(2)",
        "TypeError: Cannot add property 1, object is not extensible",
    );
    assert_uncaught(
        "let a = [1]; Object.defineProperty(a, 'length', {writable: false}); a.length = 0",
        "TypeError: Cannot assign to read only property 'length' of object",
    );
}
